//! large downloads over several streams, against a server on this machine that serves ranges at
//! a set rate.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::cache::{Cache, Validators};
use crate::digest::sha256;
use crate::net::download::{Expect, Fetch, Fetched};
use crate::net::{Offline, Options, PER_HOST, Split};
use crate::source::recorded::block_on;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

#[derive(Clone, Copy)]
struct Serving {
    etag: Option<&'static str>,
    /// says `Accept-Ranges: bytes` and answers a range with 206.
    ranges: bool,
    /// answers a request for a closed range with the whole file.
    whole_for_parts: bool,
    /// bytes a second for each connection.
    rate: u64,
    /// one rate for every connection together.
    shared: bool,
}

const SERVING: Serving = Serving {
    etag: Some("\"v1\""),
    ranges: true,
    whole_for_parts: false,
    rate: 2 * MIB,
    shared: false,
};

struct Server {
    url: Url,
    site: Arc<Site>,
}

/// what every connection to the server shares.
struct Site {
    body: Arc<Vec<u8>>,
    serving: Serving,
    heads: Mutex<Vec<String>>,
    /// when the next chunk may go, for a rate shared by every connection.
    clock: Mutex<Instant>,
    active: AtomicUsize,
    most: AtomicUsize,
}

impl Server {
    /// every request so far, lowercased.
    fn heads(&self) -> Vec<String> {
        self.site.heads.lock().unwrap().clone()
    }

    /// the most requests the server answered at once.
    fn most(&self) -> usize {
        self.site.most.load(Ordering::SeqCst)
    }
}

/// a file whose every 4 bytes count up, so a byte in the wrong place changes its hash.
fn body(size: u64) -> Arc<Vec<u8>> {
    Arc::new(
        (0..u32::try_from(size / 4).unwrap())
            .flat_map(u32::to_le_bytes)
            .collect(),
    )
}

fn serve(body: Arc<Vec<u8>>, serving: Serving) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!(
        "http://{}/big.bin",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let site = Arc::new(Site {
        body,
        serving,
        heads: Mutex::default(),
        clock: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let shared = site.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let site = shared.clone();
            std::thread::spawn(move || answer(stream, &site));
        }
    });
    Server { url, site }
}

/// answers one request. a connection counts as active from its request until its answer ends or
/// the client goes. the count waits a moment first, since a client drops a connection whose
/// range it no longer needs just before it asks for the next one, and this server notices that
/// only between chunks.
fn answer(mut stream: TcpStream, site: &Site) {
    let head = read_head(&mut stream).to_ascii_lowercase();
    site.heads.lock().unwrap().push(head.clone());
    std::thread::sleep(Duration::from_millis(50));
    let now = site.active.fetch_add(1, Ordering::SeqCst) + 1;
    site.most.fetch_max(now, Ordering::SeqCst);
    respond(&mut stream, &head, site);
    site.active.fetch_sub(1, Ordering::SeqCst);
}

fn respond(stream: &mut TcpStream, head: &str, site: &Site) {
    let (body, serving) = (site.body.as_slice(), site.serving);
    let asked = head
        .lines()
        .find_map(|line| line.strip_prefix("range: bytes="))
        .and_then(|range| {
            let (start, end) = range.split_once('-')?;
            let start: usize = start.trim().parse().ok()?;
            let end = end.trim();
            let end: Option<usize> = if end.is_empty() {
                None
            } else {
                Some(end.parse().ok()?)
            };
            Some((start, end))
        });
    let current = head
        .lines()
        .find_map(|line| line.strip_prefix("if-range: "))
        .is_none_or(|tag| {
            serving
                .etag
                .is_some_and(|etag| etag.eq_ignore_ascii_case(tag.trim()))
        });
    let part = asked.filter(|(_, end)| {
        serving.ranges && current && !(serving.whole_for_parts && end.is_some())
    });
    let (start, end) = part.map_or((0, body.len()), |(start, end)| {
        (
            start,
            end.map_or(body.len(), |end| (end + 1).min(body.len())),
        )
    });
    let status = if part.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut text = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n",
        end - start
    );
    if part.is_some() {
        write!(
            text,
            "Content-Range: bytes {start}-{}/{}\r\n",
            end - 1,
            body.len()
        )
        .unwrap();
    }
    if serving.ranges {
        text.push_str("Accept-Ranges: bytes\r\n");
    }
    if let Some(etag) = serving.etag {
        write!(text, "ETag: {etag}\r\n").unwrap();
    }
    text.push_str("\r\n");
    if stream.write_all(text.as_bytes()).is_err() {
        return;
    }
    let begun = Instant::now();
    let mut sent = 0;
    for chunk in body[start..end].chunks(16 * 1024) {
        if !present(stream) {
            return;
        }
        let length = u64::try_from(chunk.len()).unwrap();
        // a connection that wakes late keeps its pace, so one connection alone reaches the rate
        let until = if serving.shared {
            let mut next = site.clock.lock().unwrap();
            let now = Instant::now();
            let late = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            *next = (*next).max(late) + Duration::from_nanos(length * 1_000_000_000 / serving.rate);
            *next
        } else {
            sent += length;
            begun + Duration::from_nanos(sent * 1_000_000_000 / serving.rate)
        };
        std::thread::sleep(until.saturating_duration_since(Instant::now()));
        if stream.write_all(chunk).is_err() {
            return;
        }
    }
}

/// whether the client still holds the connection: one it closed reads as an end or an error,
/// while one it holds has nothing to read yet.
fn present(stream: &TcpStream) -> bool {
    let mut byte = [0u8; 1];
    stream.set_nonblocking(true).unwrap();
    let peeked = stream.peek(&mut byte);
    stream.set_nonblocking(false).unwrap();
    match peeked {
        Ok(read) => read > 0,
        Err(err) => err.kind() == std::io::ErrorKind::WouldBlock,
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
        head.push(byte[0]);
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// the network with `jobs` downloads at once, and splitting scaled down to small files and short
/// windows.
fn net(url: &Url, jobs: usize) -> Net {
    let options = Options {
        offline: Offline::No,
        jobs,
        credentials: Vec::new(),
        split: true,
    };
    let mut net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
    net.split = Split {
        enabled: true,
        above: MIB,
        piece: 64 * KIB,
        window: Duration::from_millis(100),
    };
    net.typed(url);
    net
}

fn download(net: &Net, cache: &Cache, url: &Url, expect: Expect) -> Fetched {
    let fetch = Fetch {
        expect,
        ..Fetch::new(url.clone())
    };
    block_on(net.download(&fetch, cache, &|_| Ok(()))).unwrap()
}

fn cache() -> (tempfile::TempDir, Cache) {
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    (folder, cache)
}

/// the requests that asked for a closed range.
fn parts(server: &Server) -> Vec<String> {
    server
        .heads()
        .into_iter()
        .filter(|head| {
            head.lines()
                .any(|line| line.starts_with("range: bytes=") && !line.ends_with('-'))
        })
        .collect()
}

#[test]
fn a_large_file_arrives_over_several_streams_and_hashes_as_one() {
    let body = body(8 * MIB);
    let server = serve(body.clone(), SERVING);
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(fs::read(cache.object_path(&fetched.digest)).unwrap(), *body);
    assert!(
        (2..=PER_HOST).contains(&server.most()),
        "{} streams at once",
        server.most()
    );
    let parts = parts(&server);
    assert_ne!(parts, Vec::<String>::new());
    for head in &parts {
        assert!(head.contains("if-range: \"v1\""), "{head}");
    }
    assert_eq!(
        net.work.counts().requests,
        server.heads().len() as u64,
        "every range is a counted request"
    );
    assert_eq!(net.work.counts().resumes, 0);
}

#[test]
fn a_file_is_split_only_when_it_is_large_and_its_ranges_are_safe_to_join() {
    let fast = Serving {
        rate: 8 * MIB,
        ..SERVING
    };
    for (serving, size, enabled, why) in [
        (
            Serving {
                ranges: false,
                ..fast
            },
            4 * MIB,
            true,
            "no ranges",
        ),
        (
            Serving { etag: None, ..fast },
            4 * MIB,
            true,
            "no validator",
        ),
        (
            Serving {
                etag: Some("W/\"weak\""),
                ..fast
            },
            4 * MIB,
            true,
            "a weak validator",
        ),
        (fast, MIB / 2, true, "small"),
        (fast, 4 * MIB, false, "turned off"),
    ] {
        let body = body(size);
        let server = serve(body.clone(), serving);
        let mut net = net(&server.url, 8);
        net.split.enabled = enabled;
        let (_folder, cache) = cache();
        let fetched = download(&net, &cache, &server.url, Expect::default());
        assert_eq!(fetched.digest, sha256(&body), "{why}");
        assert_eq!(server.heads().len(), 1, "{why}");
    }
}

#[test]
fn a_pinned_file_splits_without_validators() {
    let body = body(4 * MIB);
    let server = serve(
        body.clone(),
        Serving {
            etag: None,
            ..SERVING
        },
    );
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let expect = Expect {
        sha256: Some(sha256(&body)),
        ..Expect::default()
    };
    let fetched = download(&net, &cache, &server.url, expect);
    assert_eq!(fetched.digest, sha256(&body));
    assert_ne!(parts(&server), Vec::<String>::new());
    assert!(server.heads().iter().all(|head| !head.contains("if-range")));
}

#[test]
fn a_whole_answer_to_a_range_stops_splitting() {
    let body = body(4 * MIB);
    let server = serve(
        body.clone(),
        Serving {
            whole_for_parts: true,
            ..SERVING
        },
    );
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(parts(&server).len(), 1, "no range is asked for after a 200");
    assert_eq!(
        net.work.counts().retries,
        0,
        "the first answer covers the rest"
    );
}

#[test]
fn streams_are_added_only_while_the_download_gets_faster() {
    let body = body(4 * MIB);
    let server = serve(
        body.clone(),
        Serving {
            shared: true,
            rate: 4 * MIB,
            ..SERVING
        },
    );
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(
        server.most(),
        2,
        "a second stream that adds nothing is the last"
    );
}

#[test]
fn a_stream_is_added_only_when_a_slot_is_free() {
    let body = body(4 * MIB);
    let server = serve(body.clone(), SERVING);
    let net = net(&server.url, 1);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(server.heads().len(), 1);
}

#[test]
fn a_resume_fetches_only_the_ranges_not_kept() {
    let body = body(4 * MIB);
    let server = serve(body.clone(), SERVING);
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let validators = Validators {
        etag: Some("\"v1\"".to_owned()),
        last_modified: None,
    };
    let crate::cache::Claim::Owned(partial) = cache.claim(server.url.as_str()).unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, Some(4 * MIB), validators, false, true)
        .unwrap();
    let mib = usize::try_from(MIB).unwrap();
    writer.write_at(0, &body[..mib]).unwrap();
    writer.write_at(3 * MIB, &body[3 * mib..]).unwrap();
    writer.save().unwrap();
    drop(writer);
    drop(partial);
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    let first = &server.heads()[0];
    assert!(first.contains(&format!("range: bytes={MIB}-")), "{first}");
    assert!(first.contains("if-range: \"v1\""), "{first}");
    let counts = net.work.counts();
    assert_eq!(counts.resumes, 1);
    assert!(
        counts.bytes_down < 3 * MIB,
        "the kept megabyte at the end came again: {} bytes",
        counts.bytes_down
    );
}

#[test]
fn a_finished_stream_takes_half_of_the_largest_range_left() {
    let mut board = Board::new(vec![(0, 100)]);
    assert_eq!(board.assign(1, 10), Some((50, 100)));
    assert_eq!(board.share(0).map(|share| share.end), Some(50));
    assert_eq!(board.deliver(1, 30), (50, 30));
    assert_eq!(
        board.assign(2, 10),
        Some((25, 50)),
        "stream 0 had the most left"
    );
    assert_eq!(board.assign(3, 15), None, "no half left is 15 bytes");
}

#[test]
fn a_range_given_up_goes_back_to_an_answer_that_runs_on() {
    let mut board = Board::new(vec![(0, 100)]);
    assert_eq!(board.assign(1, 10), Some((50, 100)));
    board.give_back(1);
    assert_eq!(board.share(0).map(|share| share.end), Some(100));
    assert_eq!(board.free, []);
    // a stream whose answer ends with its range cannot grow, so the rest waits for another
    assert_eq!(board.assign(2, 10), Some((50, 100)));
    assert_eq!(board.deliver(2, 10), (50, 10));
    board.give_back(2);
    assert_eq!(board.free, [(60, 100)]);
    assert_eq!(board.assign(3, 10), Some((60, 100)));
}

#[test]
fn kept_ranges_leave_gaps_for_the_streams() {
    let mut board = Board::new(vec![(3, 6), (9, 12)]);
    assert_eq!(
        board.share(0).map(|share| (share.next, share.end)),
        Some((3, 6))
    );
    assert_eq!(
        board.deliver(0, 5),
        (3, 3),
        "a stream takes only its own share"
    );
    assert!(!board.finished());
    assert_eq!(board.assign(0, 1), Some((9, 12)));
    assert_eq!(board.deliver(0, 3), (9, 3));
    assert!(board.finished());
}
