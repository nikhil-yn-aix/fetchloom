//! Large files split into parallel ranges: when it happens, what is asked for, and that every
//! byte is fetched once.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fl_model::error::ErrorKind;
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;
use tracing_subscriber::fmt::format::FmtSpan;

use crate::support::memory::{Broken, Memory, Waits, client_with, patient};
use crate::support::{Body, Captured, Recorded, Reply, Server, bounded, chunked};

type TestResult = Result<(), Box<dyn Error>>;

const MIB: usize = 1 << 20;
const BUFFER: usize = 256 * 1024;
const BIG: usize = 65 * MIB;
const ETAG: &str = "\"v1\"";
const CALM: Duration = Duration::from_secs(5);
const SHORT: Duration = Duration::from_millis(600);
const CUT: usize = 1_000_000;

fn body(length: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let block: Vec<u8> = (0..1_048_573)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect();
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        let take = block.len().min(length - bytes.len());
        bytes.extend_from_slice(&block[..take]);
    }
    bytes
}

fn range(request: &Recorded) -> Option<(usize, usize)> {
    let (from, to) = request
        .header("range")?
        .strip_prefix("bytes=")?
        .split_once('-')?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

fn slice(bytes: &[u8], from: usize, to: usize, etag: &str) -> Reply {
    Reply {
        status: 206,
        ..Reply::ok(bytes[from..=to].to_vec())
    }
    .header(
        "Content-Range",
        &format!("bytes {from}-{to}/{}", bytes.len()),
    )
    .header("ETag", etag)
}

fn whole(bytes: &[u8], ranges: bool, etag: &str) -> Reply {
    let reply = Reply::ok(bytes.to_vec()).header("ETag", etag);
    if ranges {
        reply.header("Accept-Ranges", "bytes")
    } else {
        reply
    }
}

async fn serve(bytes: Arc<Vec<u8>>, etag: &'static str, ranges: bool) -> std::io::Result<Server> {
    Server::plain(move |request| match range(request) {
        Some((from, to)) => slice(&bytes, from, to, etag),
        None => whole(&bytes, ranges, etag),
    })
    .await
}

fn client(idle: Duration, waits: &Waits) -> Result<Client, Box<dyn Error>> {
    client_with(patient(idle), Chain::new(Vec::new()), waits)
}

fn download(server: &Server) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        url: SafeUrl::parse(&server.url("/big.bin"))?,
        length: None,
        stated: Vec::new(),
        stable: false,
        typed: true,
    })
}

#[tokio::test]
async fn a_large_file_with_a_strong_etag_is_fetched_in_parts_once() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let server = serve(Arc::clone(&bytes), ETAG, true).await?;
        let waits = Waits::default();
        let client = client(CALM, &waits)?;
        let done = client
            .download(&download(&server)?, Memory::default())
            .await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        assert_eq!(done.hashes.blake3, *blake3::hash(&bytes).as_bytes());
        assert!(done.sink.writes.iter().all(|write| *write <= BUFFER));
        let counted = client.counters().read();
        assert_eq!(counted.requests, 3);
        assert_eq!(counted.retries, 0);
        assert_eq!(counted.bytes_down, bytes.len() as u64);
        assert!(waits.taken().is_empty());
        let mut ranges: Vec<(usize, usize)> = server.requests().iter().filter_map(range).collect();
        ranges.sort_unstable();
        assert_eq!(ranges.len(), 2);
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
    })
    .await?
}

#[tokio::test]
async fn as_many_parts_as_streams_the_host_allows_are_asked_for() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let server = serve(Arc::clone(&bytes), ETAG, true).await?;
        let config = Config {
            per_host: 2,
            ..patient(CALM)
        };
        let client = client_with(config, Chain::new(Vec::new()), &Waits::default())?;
        let done = client
            .download(&download(&server)?, Memory::default())
            .await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        let asked: Vec<_> = server.requests().iter().filter_map(range).collect();
        assert_eq!(asked.len(), 1);
        assert_eq!(client.counters().read().requests, 2);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn a_part_is_written_in_full_buffers_whatever_size_its_pieces_arrive_in() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let source = Arc::clone(&bytes);
        let server = Server::plain(move |request| match range(request) {
            Some((from, to)) => Reply {
                body: Body::Bytes(chunked(source[from..=to].chunks(1000))),
                ..slice(&source, from, to, ETAG)
            }
            .header("Transfer-Encoding", "chunked"),
            None => Reply {
                body: Body::Bytes(chunked(source.chunks(1000))),
                ..whole(&source, true, ETAG)
            }
            .header("Transfer-Encoding", "chunked"),
        })
        .await?;
        let job = Download {
            length: Some(BIG as u64),
            ..download(&server)?
        };
        let client = client(CALM, &Waits::default())?;
        let done = client.download(&job, Memory::default()).await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        assert_eq!(server.requests().len(), 3);
        let short = done.sink.writes.iter().filter(|write| **write != BUFFER);
        assert!(short.count() <= 3, "{:?}", done.sink.writes.len());
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn no_split_without_a_strong_etag_ranges_or_room_on_the_host() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let client = client(CALM, &Waits::default())?;
        let mut requests = 0;
        for (etag, ranges) in [("W/\"v1\"", true), (ETAG, false)] {
            let server = serve(Arc::clone(&bytes), etag, ranges).await?;
            let done = client
                .download(&download(&server)?, Memory::default())
                .await?;
            requests += 1;
            assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
            assert_eq!(done.sink.reads, 0);
            assert_eq!(client.counters().read().requests, requests);
        }
        let strong = serve(Arc::clone(&bytes), ETAG, true).await?;
        let hosts = client.hosts();
        let held: Vec<_> = (0..4).filter_map(|_| hosts.try_take("127.0.0.1")).collect();
        assert_eq!(held.len(), 4);
        let done = client
            .download(&download(&strong)?, Memory::default())
            .await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        assert_eq!(done.sink.reads, 0);
        assert_eq!(client.counters().read().requests, requests + 1);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn a_small_file_is_never_split_though_it_could_be() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(2 * MIB));
        let server = serve(Arc::clone(&bytes), ETAG, true).await?;
        let client = client(CALM, &Waits::default())?;
        let done = client
            .download(&download(&server)?, Memory::default())
            .await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        assert_eq!(done.sink.reads, 0);
        assert_eq!(server.requests().len(), 1);
        Ok(())
    })
    .await?
}

#[derive(Clone, Default)]
struct Cut(Arc<Mutex<Option<(usize, usize)>>>);

impl Cut {
    fn get(&self) -> Option<(usize, usize)> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn cut_server(bytes: Arc<Vec<u8>>, stall: bool) -> std::io::Result<(Server, Cut)> {
    let cut = Cut::default();
    let first = AtomicBool::new(true);
    let seen = cut.clone();
    let server = Server::plain(move |request| match range(request) {
        Some((from, to)) if first.swap(false, Ordering::SeqCst) => {
            *seen.0.lock().unwrap_or_else(PoisonError::into_inner) = Some((from, to));
            let reply = slice(&bytes, from, to, ETAG);
            if stall {
                Reply {
                    body: Body::Stall {
                        bytes: bytes[from..=to].to_vec(),
                        after: CUT,
                    },
                    ..reply
                }
            } else {
                Reply {
                    body: Body::Bytes(bytes[from..from + CUT].to_vec()),
                    ..reply
                }
            }
        }
        Some((from, to)) => slice(&bytes, from, to, ETAG),
        None => whole(&bytes, true, ETAG),
    })
    .await?;
    Ok((server, cut))
}

#[tokio::test]
async fn a_part_that_stalls_or_ends_early_is_asked_again_from_where_it_stopped() -> TestResult {
    bounded(async {
        let captured = Captured::default();
        let writer = captured.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_span_events(FmtSpan::CLOSE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .try_init()
            .map_err(|error| error.to_string())?;
        for stall in [true, false] {
            captured.clear();
            let bytes = Arc::new(body(BIG));
            let (server, cut) = cut_server(Arc::clone(&bytes), stall).await?;
            let waits = Waits::default();
            let client = client(SHORT, &waits)?;
            let done = client
                .download(&download(&server)?, Memory::default())
                .await?;
            assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
            assert_eq!(done.hashes.blake3, *blake3::hash(&bytes).as_bytes());
            let (from, to) = cut.get().ok_or("no part was cut")?;
            let asked: Vec<_> = server.requests().iter().filter_map(range).collect();
            assert_eq!(asked.len(), 3, "{stall}");
            assert!(asked.contains(&(from + CUT, to)), "{stall} {asked:?}");
            let counted = client.counters().read();
            assert_eq!(counted.retries, 1, "{stall}");
            assert_eq!(waits.taken().len(), 1, "{stall}");
            let logs = captured.text();
            assert!(logs.contains("attempt=2"), "{stall} {logs}");
            let again = format!("bytes={}", to + 1 - from - CUT);
            assert!(logs.contains(&again), "{stall} {again} {logs}");
        }
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn a_part_is_complete_at_the_end_of_its_range_and_takes_no_more() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let source = Arc::clone(&bytes);
        let server = Server::plain(move |request| match range(request) {
            Some((from, to)) => {
                let mut sent = source[from..=to].to_vec();
                sent.extend_from_slice(&[0xee; 101]);
                Reply {
                    body: Body::Stall {
                        bytes: sent,
                        after: to + 1 - from + 100,
                    },
                    ..slice(&source, from, to, ETAG)
                }
            }
            None => whole(&source, true, ETAG),
        })
        .await?;
        let waits = Waits::default();
        let client = client(SHORT, &waits)?;
        let done = client
            .download(&download(&server)?, Memory::default())
            .await?;
        assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
        let counted = client.counters().read();
        assert_eq!(counted.bytes_down, bytes.len() as u64);
        assert_eq!(counted.retries, 0);
        assert!(waits.taken().is_empty());
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn parts_that_keep_stalling_give_up_with_a_timeout() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let source = Arc::clone(&bytes);
        let server = Server::plain(move |request| match range(request) {
            Some((from, to)) => Reply {
                body: Body::Stall {
                    bytes: source[from..=to].to_vec(),
                    after: 0,
                },
                ..slice(&source, from, to, ETAG)
            },
            None => whole(&source, true, ETAG),
        })
        .await?;
        let client = client(SHORT, &Waits::default())?;
        let error = client
            .download(&download(&server)?, Memory::default())
            .await
            .err()
            .ok_or("a failing part was taken")?;
        assert_eq!(error.kind(), ErrorKind::NetworkTimeout);
        assert!(client.counters().read().retries >= 4);
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn a_split_that_cannot_finish_says_why() -> TestResult {
    bounded(async {
        let bytes = Arc::new(body(BIG));
        let server = serve(Arc::clone(&bytes), ETAG, true).await?;
        let client = client(CALM, &Waits::default())?;
        let error = client
            .download(&download(&server)?, Broken)
            .await
            .err()
            .ok_or("a broken sink took the bytes")?;
        assert_eq!(error.kind(), ErrorKind::LocalIo);
        assert!(error.to_string().ends_with("the disk is full"), "{error}");
        let job = Download {
            length: Some(BIG as u64 + 1000),
            ..download(&server)?
        };
        let error = client
            .download(&job, Memory::default())
            .await
            .err()
            .ok_or("a file of another length was taken")?;
        assert_eq!(error.kind(), ErrorKind::IntegrityMismatch);
        let source = Arc::clone(&bytes);
        let crooked = Server::plain(move |request| match range(request) {
            Some((from, to)) => Reply {
                status: 206,
                ..Reply::ok(source[from..=to].to_vec())
            }
            .header(
                "Content-Range",
                &format!("bytes {}-{to}/{}", from + 1, source.len()),
            )
            .header("ETag", ETAG),
            None => whole(&source, true, ETAG),
        })
        .await?;
        let error = client
            .download(&download(&crooked)?, Memory::default())
            .await
            .err()
            .ok_or("a range other than the one asked for was taken")?;
        assert_eq!(error.kind(), ErrorKind::NetworkProtocol);
        Ok(())
    })
    .await?
}
