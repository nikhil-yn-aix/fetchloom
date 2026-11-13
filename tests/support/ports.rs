use std::net::{TcpListener, TcpStream};

/// an address on this machine nothing listens at while it lives, so each connection is refused,
/// and no other process can take its port meanwhile, as one could once a dropped listener freed
/// it. the port is the local end of a connection kept open: a socket bound without listening
/// holds a port too, but macos drops connections to it unanswered.
pub(crate) struct Refusing {
    pub(crate) address: std::net::SocketAddr,
    _held: (TcpListener, TcpStream),
}

impl Refusing {
    pub(crate) fn new() -> std::io::Result<Refusing> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let held = TcpStream::connect(listener.local_addr()?)?;
        Ok(Refusing {
            address: held.local_addr()?,
            _held: (listener, held),
        })
    }
}
