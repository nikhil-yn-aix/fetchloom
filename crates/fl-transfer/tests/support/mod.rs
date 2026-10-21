//! Scripted HTTP/1.1 server on loopback, plain or TLS, for tests that need
//! behaviour a mock server cannot produce: stalls in the middle of a body,
//! byte-by-byte trickles and TLS.

pub mod disk;
pub mod memory;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

const MAX_HEAD: usize = 64 * 1024;

/// One request as the server received it.
#[derive(Debug, Clone)]
pub struct Recorded {
    /// The request target as sent: path and query.
    pub target: String,
    /// The request headers in arrival order with lowercased names.
    pub headers: Vec<(String, String)>,
}

impl Recorded {
    /// Returns the value of the first header with the given name, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// How a reply body is written to the connection.
#[derive(Debug, Clone)]
pub enum Body {
    /// All bytes are written at once.
    Bytes(Vec<u8>),
    /// One byte is written, then the server sleeps for `every`, repeatedly.
    Trickle {
        /// The full body.
        bytes: Vec<u8>,
        /// The pause after each byte.
        every: Duration,
    },
    /// The first `after` bytes are written, then the connection stays open and
    /// silent until the client goes away. The headers state the full length.
    Stall {
        /// The full body; its length is advertised.
        bytes: Vec<u8>,
        /// How many bytes are sent before going silent.
        after: usize,
    },
}

impl Body {
    fn len(&self) -> usize {
        match self {
            Body::Bytes(bytes) | Body::Trickle { bytes, .. } | Body::Stall { bytes, .. } => {
                bytes.len()
            }
        }
    }
}

/// A scripted response.
#[derive(Debug, Clone)]
pub struct Reply {
    /// The status code.
    pub status: u16,
    /// Extra response headers.
    pub headers: Vec<(String, String)>,
    /// The body and how it is delivered.
    pub body: Body,
    /// The pause before the status line is written.
    pub delay: Duration,
}

impl Reply {
    /// A `200` reply carrying `bytes` at once.
    pub fn ok(bytes: Vec<u8>) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: Body::Bytes(bytes),
            delay: Duration::ZERO,
        }
    }

    /// A reply with the given status and an empty body.
    pub fn status(code: u16) -> Self {
        Self {
            status: code,
            ..Self::ok(Vec::new())
        }
    }

    /// Adds a response header.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Delays the status line by `delay`.
    #[must_use]
    pub fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn head(&self) -> Vec<u8> {
        let reason = if self.status == 200 { "OK" } else { "Status" };
        let mut lines = vec![format!("HTTP/1.1 {} {reason}", self.status)];
        let sets_length = self
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
        if !sets_length {
            lines.push(format!("Content-Length: {}", self.body.len()));
        }
        for (name, value) in &self.headers {
            lines.push(format!("{name}: {value}"));
        }
        lines.push("Connection: close".to_owned());
        format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes()
    }
}

type Handler = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;
type Log = Arc<Mutex<Vec<Recorded>>>;

/// A running scripted server. Dropping it stops accepting connections.
pub struct Server {
    addr: SocketAddr,
    ca_der: Option<Vec<u8>>,
    log: Log,
    task: JoinHandle<()>,
}

impl Server {
    /// Starts a plain HTTP server answering every request with `handler`.
    pub async fn plain(
        handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::start(Arc::new(handler), None, None).await
    }

    /// Starts an HTTPS server whose certificate covers `127.0.0.1` and
    /// `localhost` and is signed by a freshly generated test CA.
    pub async fn tls(
        handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let (config, ca_der) = tls_material().map_err(io::Error::other)?;
        let acceptor = TlsAcceptor::from(Arc::new(config));
        Self::start(Arc::new(handler), Some(acceptor), Some(ca_der)).await
    }

    async fn start(
        handler: Handler,
        acceptor: Option<TlsAcceptor>,
        ca_der: Option<Vec<u8>>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let log: Log = Arc::default();
        let task = tokio::spawn(accept_loop(listener, handler, acceptor, log.clone()));
        Ok(Self {
            addr,
            ca_der,
            log,
            task,
        })
    }

    /// The absolute URL of `path` on this server.
    pub fn url(&self, path: &str) -> String {
        let scheme = if self.ca_der.is_some() {
            "https"
        } else {
            "http"
        };
        let path = path.trim_start_matches('/');
        format!("{scheme}://{}/{path}", self.authority())
    }

    /// The `host:port` the server listens on.
    pub fn authority(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    /// Every request received so far, in arrival order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The test CA certificate in DER form, or `None` for a plain server.
    pub fn ca_der(&self) -> Option<Vec<u8>> {
        self.ca_der.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn tls_material() -> Result<(ServerConfig, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate()?)?;

    let mut leaf_params = CertificateParams::new(vec!["localhost".to_owned()])?;
    leaf_params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_params.signed_by(&leaf_key, &ca)?;

    let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], key)?;
    Ok((config, ca.as_ref().der().to_vec()))
}

async fn accept_loop(
    listener: TcpListener,
    handler: Handler,
    acceptor: Option<TlsAcceptor>,
    log: Log,
) {
    while let Ok((tcp, _)) = listener.accept().await {
        tokio::spawn(serve_connection(
            tcp,
            handler.clone(),
            acceptor.clone(),
            log.clone(),
        ));
    }
}

async fn serve_connection(
    tcp: TcpStream,
    handler: Handler,
    acceptor: Option<TlsAcceptor>,
    log: Log,
) {
    let _ = match acceptor {
        Some(acceptor) => match acceptor.accept(tcp).await {
            Ok(stream) => respond(stream, &handler, &log).await,
            Err(error) => Err(error),
        },
        None => respond(tcp, &handler, &log).await,
    };
}

async fn respond<S>(mut stream: S, handler: &Handler, log: &Log) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = read_request(&mut stream).await?;
    log.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(request.clone());
    let reply = handler(&request);
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    stream.write_all(&reply.head()).await?;
    write_body(&mut stream, &reply.body).await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn write_body<S>(stream: &mut S, body: &Body) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match body {
        Body::Bytes(bytes) => stream.write_all(bytes).await,
        Body::Trickle { bytes, every } => {
            for byte in bytes {
                stream.write_all(&[*byte]).await?;
                stream.flush().await?;
                tokio::time::sleep(*every).await;
            }
            Ok(())
        }
        Body::Stall { bytes, after } => {
            stream
                .write_all(&bytes[..(*after).min(bytes.len())])
                .await?;
            stream.flush().await?;
            let mut sink = [0u8; 256];
            while stream.read(&mut sink).await? > 0 {}
            Ok(())
        }
    }
}

async fn read_request<S>(stream: &mut S) -> io::Result<Recorded>
where
    S: AsyncRead + Unpin,
{
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        if head.len() > MAX_HEAD {
            return Err(io::Error::other("request head too large"));
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        head.extend_from_slice(&chunk[..read]);
    }
    parse_head(&String::from_utf8_lossy(&head))
}

fn parse_head(head: &str) -> io::Result<Recorded> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(_), Some(target)) = (parts.next(), parts.next()) else {
        return Err(io::Error::other("malformed request line"));
    };
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(Recorded {
        target: target.to_owned(),
        headers,
    })
}
