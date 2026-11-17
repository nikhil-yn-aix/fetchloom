use std::net::SocketAddr;
#[cfg(not(target_os = "linux"))]
use std::net::{TcpListener, TcpStream};

/// an address on this machine that refuses each connection while it lives, and whose port no
/// other process can take meanwhile, as one could once a dropped listener freed it.
///
/// on linux it is a socket bound and never listening: linux refuses a connection to it at once and
/// never gives a bound port to a connect as its source port. the port of a connection is no use
/// there: linux gives it to another connect when the pair of addresses differs, so a connect to it
/// can reach itself and read its own request back.
///
/// elsewhere it is the local end of a connection kept open: macos drops connections to a bound,
/// idle socket unanswered, and never gives a connect a source port another socket uses. windows
/// refuses a connection only after trying it again, about 2 s later, so each one costs that long
/// there.
pub(crate) struct Refusing {
    pub(crate) address: SocketAddr,
    #[cfg(target_os = "linux")]
    _held: rustix::fd::OwnedFd,
    #[cfg(not(target_os = "linux"))]
    _held: (TcpListener, TcpStream),
}

impl Refusing {
    #[cfg(target_os = "linux")]
    pub(crate) fn new() -> std::io::Result<Refusing> {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, getsockname, socket_with};
        let held = socket_with(
            AddressFamily::INET,
            SocketType::STREAM,
            SocketFlags::CLOEXEC,
            None,
        )?;
        bind(&held, &SocketAddr::from(([127, 0, 0, 1], 0)))?;
        Ok(Refusing {
            address: SocketAddr::try_from(getsockname(&held)?)?,
            _held: held,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn new() -> std::io::Result<Refusing> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let held = TcpStream::connect(listener.local_addr()?)?;
        Ok(Refusing {
            address: held.local_addr()?,
            _held: (listener, held),
        })
    }
}
