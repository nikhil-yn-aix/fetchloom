//! a local server for network tests that answers each connection with the next scripted bytes,
//! then hangs up, so a test can send what polite fakes cannot, such as a body cut short.

use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use url::Url;

/// serves `responses`, one connection each, keeping each request's head before its response
/// goes, so every request a finished client made is there.
pub(super) fn serve(responses: Vec<Vec<u8>>) -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        // no deadline for the next connection: a busy disk has held a test's client back for
        // 15 s. a test that connects fewer times than scripted leaves the server waiting
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let head = request(&mut stream);
            seen.lock().unwrap().push(head);
            let _ = stream.write_all(&response);
        }
    });
    (url, asked)
}

/// serves every connection with what `answer` makes of its request target, the path and query,
/// for as long as the tests run, and keeps the head of every request it took, as [`serve`] does.
pub(super) fn route(
    answer: impl Fn(&str) -> Vec<u8> + Send + 'static,
) -> (Url, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let head = request(&mut stream);
            let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
            seen.lock().unwrap().push(head);
            let _ = stream.write_all(&answer(&target));
        }
    });
    (url, asked)
}

/// reads one request from `stream`, its head and then its `Content-Length` body, and returns the
/// head. a connection closed with bytes unread is reset, and windows then drops the unread answer.
fn request(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok())
        .unwrap_or(0);
    let _ = std::io::copy(&mut stream.take(length), &mut std::io::sink());
    head
}

/// the first line of each head [`serve`] kept, such as `GET /start HTTP/1.1`.
pub(super) fn lines(asked: &Mutex<Vec<String>>) -> Vec<String> {
    // a test that failed holding the lock poisons it, and a guard reading it as the test
    // unwinds must not panic again
    asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
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

/// so the connection ends after the answer rather than with a reset.
#[test]
fn a_request_body_is_read_before_the_answer() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"token")]);
    let mut stream = TcpStream::connect(&*url.socket_addrs(|| None).unwrap()).unwrap();
    stream
        .write_all(b"POST /token HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\ngrant")
        .unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    assert!(answer.ends_with(b"\r\n\r\ntoken"));
    assert_eq!(lines(&asked), ["POST /token HTTP/1.1"]);
}
