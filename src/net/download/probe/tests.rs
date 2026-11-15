use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::net::{Offline, Options, Splitting};

/// a server that answers every request with the first byte of a 10-byte file, the byte a moment
/// after the head as a far server's may come, keeping each connection open for the next request,
/// and counts the connections it took.
fn keeping() -> (Url, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&connections);
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            counted.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                loop {
                    let mut head = Vec::new();
                    let mut byte = [0; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let head = b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/10\r\nContent-Length: 1\r\n\r\n";
                    if stream
                        .write_all(head)
                        .and_then(|()| stream.flush())
                        .is_err()
                    {
                        return;
                    }
                    // a client that leaves the byte unread by now has dropped the connection
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    if stream.write_all(b"x").is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, connections)
}

/// many files of one host are asked a few at a time over a few connections, each kept for the
/// next file once its one byte is read, so no host sees a connection a file.
#[tokio::test(flavor = "multi_thread")]
async fn the_files_of_one_host_share_a_few_connections() {
    let (url, connections) = keeping();
    let options = Options {
        offline: Offline::No,
        jobs: 8,
        credentials: Vec::new(),
        split: Splitting::Never,
    };
    let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {})).with_host_jobs(2);
    net.typed(&url);
    let net = Arc::new(net);
    let mut asking = tokio::task::JoinSet::new();
    for file in 0..24 {
        let (net, link) = (Arc::clone(&net), url.join(&format!("{file}.bin")).unwrap());
        asking.spawn(async move { net.probe(&Fetch::new(link)).await });
    }
    while let Some(answer) = asking.join_next().await {
        let probed = answer.unwrap().unwrap();
        assert!(
            matches!(
                probed,
                Probed::File {
                    total: Some(10),
                    page: false
                }
            ),
            "{probed:?}"
        );
    }
    let opened = connections.load(Ordering::SeqCst);
    assert!(opened <= 4, "{opened} connections for 24 files");
}
