//! a server on this machine for network tests: it answers each connection with the next scripted
//! response, byte for byte, then hangs up, so a test can send what real servers send and polite
//! fakes cannot, such as a body cut short.

use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use url::Url;

/// serves `responses`, one connection each, and keeps the head of every request it took. a head
/// is kept before its response is sent, so every request a finished client made is there.
pub(super) fn serve(responses: Vec<Vec<u8>>) -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        for response in responses {
            // a test that connects fewer times than scripted leaves the server to end on its own
            let Ok(Ok((stream, _))) = runtime.block_on(tokio::time::timeout(
                Duration::from_secs(10),
                listener.accept(),
            )) else {
                return;
            };
            let mut stream = stream.into_std().unwrap();
            // windows hands out accepted sockets non-blocking like their listener
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&head).into_owned());
            let _ = stream.write_all(&response);
        }
    });
    (url, asked)
}

/// serves every connection with what `answer` makes of its request target, the path and query,
/// until none comes for 10 s, and keeps the head of every request it took, as [`serve`] does.
pub(super) fn route(
    answer: impl Fn(&str) -> Vec<u8> + Send + 'static,
) -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        while let Ok(Ok((stream, _))) = runtime.block_on(tokio::time::timeout(
            Duration::from_secs(10),
            listener.accept(),
        )) {
            let mut stream = stream.into_std().unwrap();
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
            seen.lock().unwrap().push(head);
            let _ = stream.write_all(&answer(&target));
        }
    });
    (url, asked)
}

/// the first line of each head [`serve`] kept, such as `GET /start HTTP/1.1`.
pub(super) fn lines(asked: &Mutex<Vec<String>>) -> Vec<String> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|head| head.lines().next().unwrap_or_default().to_owned())
        .collect()
}

/// a whole response that closes its connection: the status, `headers` and the length of `body`,
/// then `body`.
pub(super) fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    head.push_str("\r\n");
    let mut response = head.into_bytes();
    response.extend_from_slice(body);
    response
}
