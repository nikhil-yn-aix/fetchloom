use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use super::*;
use crate::net::{Offline, Options, Splitting};
use crate::source::recorded::block_on;

/// the link of the server at `address`.
fn at(address: SocketAddr) -> Url {
    Url::parse(&format!("http://{address}/")).unwrap()
}

/// a listener that takes each connection and never answers it, as an address held by a firewall
/// looks to a client, which waits for its own timeout.
pub(in crate::net) struct Silent {
    pub(in crate::net) url: Url,
    taken: Arc<Mutex<Vec<TcpStream>>>,
}

impl Silent {
    pub(in crate::net) fn new() -> Silent {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = at(listener.local_addr().unwrap());
        let taken = Arc::new(Mutex::new(Vec::new()));
        let held = taken.clone();
        std::thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                held.lock().unwrap().push(stream);
            }
        });
        Silent { url, taken }
    }

    /// how many connections it took.
    pub(in crate::net) fn taken(&self) -> usize {
        self.taken.lock().unwrap().len()
    }
}

/// a listener whose queue of connections is full, so the kernel drops each new one unanswered,
/// as a firewall that drops packets does: a client waits out its own timeout.
struct Dropping {
    url: Url,
    _held: (TcpListener, Vec<TcpStream>),
}

impl Dropping {
    fn new() -> Dropping {
        // only a socket made by hand listens with a queue this short
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let listener = runtime.block_on(async {
            let socket = tokio::net::TcpSocket::new_v4().unwrap();
            socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
            socket.listen(0).unwrap().into_std().unwrap()
        });
        let address = listener.local_addr().unwrap();
        let filled = (0..3)
            .filter_map(|_| TcpStream::connect_timeout(&address, Duration::from_millis(200)).ok())
            .collect();
        Dropping {
            url: at(address),
            _held: (listener, filled),
        }
    }
}

fn net() -> Net {
    Net::new(
        Options {
            offline: Offline::No,
            jobs: 8,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
}

/// where nothing answers a metadata service's address, an ask is unheard: refused at once, or
/// given up after the wait it was given when the connection is held unanswered or dropped.
#[test]
fn an_ask_nothing_answers_is_unheard_after_its_wait() {
    let net = net();
    let wait = Duration::from_millis(300);
    let ask = |url: &Url| {
        let Ok(heard) = block_on(net.ask_metadata(url, None, wait)) else {
            panic!("the ask of {url} failed");
        };
        heard.map(|response| response.status())
    };
    let refusing = crate::test_ports::Refusing::new().unwrap();
    // windows refuses a connection only after trying it again, about 2 s later
    let refused = if cfg!(windows) {
        Unheard::Waited(wait)
    } else {
        Unheard::Refused
    };
    assert_eq!(ask(&at(refusing.address)), Err(refused));
    let silent = Silent::new();
    assert_eq!(ask(&silent.url), Err(Unheard::Waited(wait)));
    assert_eq!(silent.taken(), 1);
    let dropping = Dropping::new();
    assert_eq!(ask(&dropping.url), Err(Unheard::Waited(wait)));
}
