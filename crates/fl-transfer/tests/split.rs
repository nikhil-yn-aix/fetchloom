//! Large files split into parallel ranges: when it happens, what is asked for, and that every
//! byte is fetched once.

use std::error::Error;
use std::sync::Arc;

use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;

use crate::support::memory::{Memory, Waits, client};
use crate::support::{Recorded, Reply, Server};

type TestResult = Result<(), Box<dyn Error>>;

const MIB: usize = 1 << 20;

fn body(length: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn range(request: &Recorded) -> Option<(usize, usize)> {
    let (from, to) = request
        .header("range")?
        .strip_prefix("bytes=")?
        .split_once('-')?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

async fn serve(bytes: Arc<Vec<u8>>, etag: &'static str) -> std::io::Result<Server> {
    Server::plain(move |request| match range(request) {
        Some((from, to)) => Reply {
            status: 206,
            ..Reply::ok(bytes[from..=to].to_vec())
        }
        .header(
            "Content-Range",
            &format!("bytes {from}-{to}/{}", bytes.len()),
        )
        .header("ETag", etag),
        None => Reply::ok(bytes.to_vec())
            .header("Accept-Ranges", "bytes")
            .header("ETag", etag),
    })
    .await
}

fn download(server: &Server) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        url: SafeUrl::parse(&server.url("/big.bin"))?,
        length: None,
        stated: Vec::new(),
        stable: false,
    })
}

#[tokio::test]
async fn a_large_file_with_a_strong_etag_is_fetched_in_parts_once() -> TestResult {
    let bytes = Arc::new(body(100 * MIB));
    let server = serve(Arc::clone(&bytes), "\"v1\"").await?;
    let client = client(&Waits::default())?;
    let done = client
        .download(&download(&server)?, Memory::default())
        .await?;
    assert_eq!(done.sink.bytes.len(), bytes.len());
    assert_eq!(done.sink.bytes, *bytes);
    assert_eq!(done.hashes.blake3, *blake3::hash(&bytes).as_bytes());
    let counted = client.counters().read();
    assert_eq!(counted.requests, 4);
    assert_eq!(counted.bytes_down, bytes.len() as u64);
    let mut ranges: Vec<(usize, usize)> = server.requests().iter().filter_map(range).collect();
    ranges.sort_unstable();
    assert_eq!(ranges.len(), 3);
    assert_eq!(ranges.last().map(|last| last.1), Some(bytes.len() - 1));
    assert!(ranges.windows(2).all(|pair| pair[0].1 + 1 == pair[1].0));
    assert!(
        server
            .requests()
            .iter()
            .skip(1)
            .all(|request| request.header("if-range") == Some("\"v1\""))
    );
    Ok(())
}

#[tokio::test]
async fn no_split_without_a_strong_etag_or_without_room_on_the_host() -> TestResult {
    let bytes = Arc::new(body(70 * MIB));
    let weak = serve(Arc::clone(&bytes), "W/\"v1\"").await?;
    let client = client(&Waits::default())?;
    let done = client
        .download(&download(&weak)?, Memory::default())
        .await?;
    assert_eq!(done.sink.bytes, *bytes);
    assert_eq!(client.counters().read().requests, 1);
    let strong = serve(Arc::clone(&bytes), "\"v1\"").await?;
    let hosts = client.hosts();
    let held: Vec<_> = (0..4).filter_map(|_| hosts.try_take("127.0.0.1")).collect();
    assert_eq!(held.len(), 4);
    let done = client
        .download(&download(&strong)?, Memory::default())
        .await?;
    assert_eq!(done.sink.bytes, *bytes);
    assert_eq!(client.counters().read().requests, 2);
    Ok(())
}
