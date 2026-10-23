//! A DOI resolver served over TLS, which a mock server cannot do.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use fl_model::error::ErrorKind;
use fl_sources::Registry;
use fl_transfer::url::SafeUrl;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use url::Url;

use crate::support::{self, Failure};

struct Redirector {
    port: u16,
    ca: Vec<u8>,
    task: JoinHandle<()>,
}

impl Drop for Redirector {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn material() -> Result<(ServerConfig, Vec<u8>), Failure> {
    let mut authority = CertificateParams::new(Vec::<String>::new())?;
    authority.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    authority.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(authority, KeyPair::generate()?)?;
    let mut leaf = CertificateParams::new(Vec::<String>::new())?;
    leaf.subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate()?;
    let signed = leaf.signed_by(&key, &ca)?;
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![signed.der().clone()],
            PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )?;
    Ok((config, ca.as_ref().der().to_vec()))
}

async fn redirecting_to(location: &'static str) -> Result<Redirector, Failure> {
    let (config, ca) = material()?;
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut head = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(count) => head.extend_from_slice(&chunk[..count]),
                    }
                }
                let reply = format!(
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok(Redirector { port, ca, task })
}

async fn resolve(redirector: &Redirector) -> Result<ErrorKind, Failure> {
    let client = support::client_trusting(redirector.ca.clone())?;
    let resolver = Url::parse(&format!("https://127.0.0.1:{}/", redirector.port))?;
    client.trust(
        &SafeUrl::parse(resolver.as_str())?
            .origin()
            .ok_or("no origin")?,
    );
    let registry = Registry::builtin(&client)?.with_resolver(resolver);
    let error = registry
        .resolve(&"doi:10.1/x".parse()?, &support::no_filters()?)
        .await
        .err()
        .ok_or("resolved")?;
    Ok(error.kind())
}

#[tokio::test]
async fn a_redirect_from_https_to_http_is_refused() -> Result<(), Failure> {
    let redirector = redirecting_to("http://landing.example.org/record").await?;
    assert_eq!(resolve(&redirector).await?, ErrorKind::NetworkInsecure);
    Ok(())
}

#[tokio::test]
async fn a_redirect_from_https_to_https_is_followed_until_no_source_serves_it()
-> Result<(), Failure> {
    let redirector = redirecting_to("/next").await?;
    assert_eq!(resolve(&redirector).await?, ErrorKind::ReferenceUnresolved);
    Ok(())
}
