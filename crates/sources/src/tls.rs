//! The TLS a control connection and its data connections are secured with.

use std::net::TcpStream;
use std::sync::{Arc, OnceLock};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use rustls_platform_verifier::ConfigVerifierExt;

pub(crate) type Secured = StreamOwned<ClientConnection, TcpStream>;

static CONFIGURED: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();

fn configured() -> Option<Arc<ClientConfig>> {
    CONFIGURED
        .get_or_init(|| {
            let _ = rustls_graviola::default_provider().install_default();
            ClientConfig::with_platform_verifier().ok().map(Arc::new)
        })
        .clone()
}

pub(crate) fn secured(stream: TcpStream, host: &str) -> Result<Secured, String> {
    let config = configured().ok_or_else(|| {
        "no certificate store this platform offers could be read, so nothing could be verified"
            .to_owned()
    })?;
    let named = ServerName::try_from(host.to_owned()).map_err(|reason| {
        format!("{host} is not a name a certificate can be checked against: {reason}")
    })?;
    let connection = ClientConnection::new(config, named)
        .map_err(|reason| format!("the handshake could not be started: {reason}"))?;
    let mut wire = StreamOwned::new(connection, stream);
    wire.conn
        .complete_io(&mut wire.sock)
        .map_err(|reason| format!("the handshake did not complete: {reason}"))?;
    Ok(wire)
}
