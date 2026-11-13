//! large downloads over several streams, against a server on this machine that serves ranges at
//! a set rate.

use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::cache::{Cache, Validators};
use crate::digest::sha256;
use crate::net::download::{Expect, Fetch, Fetched};
use crate::net::{Offline, Options, Rate, Split, Splitting};
use crate::source::recorded::block_on;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

#[derive(Clone, Copy)]
struct Serving {
    etag: Option<&'static str>,
    /// the `ETag` every answer after the first gives, as another server behind the same name
    /// may label the same bytes.
    drift: Option<&'static str>,
    /// says `Accept-Ranges: bytes` and answers a range with 206.
    ranges: bool,
    /// answers a request for a closed range with the whole file.
    whole_for_parts: bool,
    /// bytes a second for each connection.
    rate: u64,
    /// the `x-amz-version-id` every answer gives, as an S3 bucket that keeps versions does.
    version: Option<&'static str>,
}

const SERVING: Serving = Serving {
    etag: Some("\"v1\""),
    drift: None,
    ranges: true,
    whole_for_parts: false,
    rate: 2 * MIB,
    version: None,
};

/// how far the first answer goes, when ranges get the whole file, before it waits for one such
/// answer to end. the client then knows ranges fail before its first stream needs another.
const HELD: usize = 256 * 1024;

struct Server {
    url: Url,
    site: Arc<Site>,
    serving: Option<std::thread::JoinHandle<()>>,
}

/// what every connection to the server shares.
struct Site {
    body: Arc<Vec<u8>>,
    serving: Serving,
    heads: Mutex<Vec<String>>,
    /// a request for a closed range was answered with the whole file.
    whole_sent: (Mutex<bool>, Condvar),
    stopped: AtomicBool,
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
        whole_sent: (Mutex::new(false), Condvar::new()),
        stopped: AtomicBool::new(false),
        active: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let shared = site.clone();
    let serving = std::thread::spawn(move || {
        let mut workers = Vec::new();
        for stream in listener.incoming().flatten() {
            if shared.stopped.load(Ordering::SeqCst) {
                break;
            }
            let site = shared.clone();
            workers.push(std::thread::spawn(move || answer(stream, &site)));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    Server {
        url,
        site,
        serving: Some(serving),
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.site.stopped.store(true, Ordering::SeqCst);
        *self.site.whole_sent.0.lock().unwrap() = true;
        self.site.whole_sent.1.notify_all();
        let _ = TcpStream::connect(("127.0.0.1", self.url.port().unwrap()));
        if let Some(serving) = self.serving.take() {
            let result = serving.join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }
}

/// counts overlapping server responses; request permits are checked separately without sockets.
fn answer(mut stream: TcpStream, site: &Site) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let head = read_head(&mut stream).to_ascii_lowercase();
    let index = {
        let mut heads = site.heads.lock().unwrap();
        heads.push(head.clone());
        heads.len() - 1
    };
    let now = site.active.fetch_add(1, Ordering::SeqCst) + 1;
    site.most.fetch_max(now, Ordering::SeqCst);
    respond(&mut stream, &head, site, index);
    site.active.fetch_sub(1, Ordering::SeqCst);
    let closed = head
        .lines()
        .any(|line| line.starts_with("range: bytes=") && !line.ends_with('-'));
    if site.serving.whole_for_parts && closed {
        *site.whole_sent.0.lock().unwrap() = true;
        site.whole_sent.1.notify_all();
    }
}

fn respond(stream: &mut TcpStream, head: &str, site: &Site, index: usize) {
    let (body, serving) = (site.body.as_slice(), site.serving);
    let etag = if index > 0 {
        serving.drift.or(serving.etag)
    } else {
        serving.etag
    };
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
        .is_none_or(|tag| etag.is_some_and(|etag| etag.eq_ignore_ascii_case(tag.trim())));
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
    if let Some(etag) = etag {
        write!(text, "ETag: {etag}\r\n").unwrap();
    }
    if let Some(version) = serving.version {
        write!(text, "x-amz-version-id: {version}\r\n").unwrap();
    }
    text.push_str("\r\n");
    if stream.write_all(text.as_bytes()).is_err() {
        return;
    }
    let begun = Instant::now();
    let mut sent = 0;
    let waits = serving.whole_for_parts && index == 0;
    for chunk in body[start..end].chunks(16 * 1024) {
        if site.stopped.load(Ordering::SeqCst) || !present(stream) {
            return;
        }
        if waits && sent >= HELD as u64 {
            let sent = site.whole_sent.0.lock().unwrap();
            drop(site.whole_sent.1.wait_while(sent, |sent| !*sent).unwrap());
        }
        let length = u64::try_from(chunk.len()).unwrap();
        sent += length;
        let until = begun + Duration::from_nanos(sent * 1_000_000_000 / serving.rate);
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

/// the network with `jobs` downloads at once, and splitting from the first answer, scaled down
/// to small files and short windows.
fn net(url: &Url, jobs: usize) -> Net {
    let options = Options {
        offline: Offline::No,
        jobs,
        credentials: Vec::new(),
        split: Splitting::Always,
    };
    let mut net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
    net.split = Split {
        when: Splitting::Always,
        above: MIB,
        piece: 64 * KIB,
        window: Duration::from_millis(100),
        slow: 4 * MIB,
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
    let serving = Serving {
        version: Some("v1"),
        ..SERVING
    };
    let server = serve(body.clone(), serving);
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(fetched.version.as_deref(), Some("v1"));
    assert_eq!(fs::read(cache.object_path(&fetched.digest)).unwrap(), *body);
    assert!(server.most() >= 2, "{} streams at once", server.most());
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
            MIB,
            true,
            "no ranges",
        ),
        (Serving { etag: None, ..fast }, MIB, true, "no validator"),
        (
            Serving {
                etag: Some("W/\"weak\""),
                ..fast
            },
            MIB,
            true,
            "a weak validator",
        ),
        (fast, MIB / 2, true, "small"),
        (fast, MIB, false, "turned off"),
    ] {
        let body = body(size);
        let server = serve(body.clone(), serving);
        let mut net = net(&server.url, 8);
        net.split.when = if enabled {
            Splitting::Always
        } else {
            Splitting::Never
        };
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

/// the requests that asked for the file from its first byte, so a count above one is a start
/// over.
fn from_the_start(server: &Server) -> usize {
    server
        .heads()
        .iter()
        .filter(|head| !head.contains("range:"))
        .count()
}

/// a pinned file's hash decides, while an unpinned file starts over.
#[test]
fn a_pinned_file_keeps_its_parts_when_another_server_labels_them_differently() {
    let body = body(4 * MIB);
    let (sha256_pin, md5) = {
        let mut hasher = crate::digest::Hasher::with_md5();
        hasher.update(&body);
        hasher.finish()
    };
    let drifting = Serving {
        drift: Some("\"v2\""),
        ..SERVING
    };
    for (expect, why) in [
        (
            Expect {
                sha256: Some(sha256_pin),
                ..Expect::default()
            },
            "a pinned sha256",
        ),
        (
            Expect {
                md5,
                size: Some(4 * MIB),
                ..Expect::default()
            },
            "a stated md5 and size",
        ),
    ] {
        let server = serve(body.clone(), drifting);
        let net = net(&server.url, 8);
        let (_folder, cache) = cache();
        let fetched = download(&net, &cache, &server.url, expect);
        assert_eq!(fetched.digest, sha256_pin, "{why}");
        assert_ne!(parts(&server), Vec::<String>::new(), "{why}");
        assert_eq!(from_the_start(&server), 1, "{why}: started over");
    }
    let server = serve(body.clone(), drifting);
    let net = net(&server.url, 8);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256_pin);
    assert_eq!(
        from_the_start(&server),
        2,
        "nothing proves the unpinned parts"
    );
}

#[test]
fn a_whole_answer_to_a_range_stops_splitting() {
    for when in [Splitting::Always, Splitting::WhenSlow] {
        let body = body(2 * MIB);
        let server = serve(
            body.clone(),
            Serving {
                whole_for_parts: true,
                ..SERVING
            },
        );
        let mut net = net(&server.url, 8);
        net.split.when = when;
        let (_folder, cache) = cache();
        let fetched = download(&net, &cache, &server.url, Expect::default());
        assert_eq!(fetched.digest, sha256(&body), "{when:?}");
        assert_eq!(
            parts(&server).len(),
            1,
            "no range is asked for after a 200, {when:?}"
        );
        assert_eq!(
            net.work.counts().retries,
            0,
            "the first answer covers the rest, {when:?}"
        );
    }
}

#[test]
fn a_slow_stream_hands_its_answer_over_to_more_streams() {
    let body = body(8 * MIB);
    let server = serve(body.clone(), SERVING);
    let mut net = net(&server.url, 8);
    net.split.when = Splitting::WhenSlow;
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    let heads = server.heads();
    assert!(!heads[0].contains("range:"), "{}", heads[0]);
    assert_eq!(
        heads.len() - parts(&server).len(),
        1,
        "the first answer is read on from where the one stream stopped"
    );
    assert!(server.most() >= 2, "{} streams at once", server.most());
    for head in &parts(&server) {
        assert!(head.contains("if-range: \"v1\""), "{head}");
    }
    let counts = net.work.counts();
    assert_eq!(counts.requests, heads.len() as u64);
    assert_eq!(counts.resumes, 0);
}

#[test]
fn a_fast_stream_is_never_split_by_the_slow_rate_rule() {
    let split = Split {
        when: Splitting::WhenSlow,
        above: SPLIT_ABOVE,
        piece: SMALLEST_PIECE,
        window: WINDOW,
        slow: SLOW,
    };
    for (rate, left, expected) in [
        (0.0, 2 * SMALLEST_PIECE, true),
        (999_999.0, 2 * SMALLEST_PIECE, true),
        (1_000_000.0, 2 * SMALLEST_PIECE, false),
        (0.0, 2 * SMALLEST_PIECE - 1, false),
        (f64::NAN, SPLIT_ABOVE, false),
    ] {
        assert_eq!(split.hand_over(rate, left), expected);
    }
}

#[test]
fn a_stream_that_makes_the_download_no_faster_is_the_last() {
    let window = Duration::from_millis(100);
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    // 1 MiB a window before the stream is added, then the same with it
    let mut probe = Probe::Measure {
        since: start,
        delivered: 0,
    };
    assert_eq!(probe.step(at(50), MIB / 2, false, false, window), None);
    let before = probe.step(at(100), MIB, false, false, window).unwrap();
    probe = Probe::Wait { id: 1, before };
    assert_eq!(probe.step(at(110), MIB, false, false, window), None);
    assert!(matches!(probe, Probe::Wait { .. }), "waits for the stream");
    assert_eq!(probe.step(at(120), MIB, true, false, window), None);
    assert_eq!(probe.step(at(200), 2 * MIB, true, false, window), None);
    assert_eq!(probe.step(at(220), 2 * MIB, true, false, window), None);
    assert!(matches!(probe, Probe::Settled));
    assert_eq!(probe.step(at(900), 9 * MIB, true, false, window), None);
}

#[test]
fn a_stream_that_makes_the_download_faster_lets_another_in() {
    let window = Duration::from_millis(100);
    let start = Instant::now();
    let at = |millis| start + Duration::from_millis(millis);
    let mib = 1_048_576.0;
    let mut probe = Probe::Wait {
        id: 1,
        before: 10.0 * mib,
    };
    assert_eq!(probe.step(at(0), 0, true, false, window), None);
    // 2 MiB in a tenth of a second is twice the rate before
    let after = probe.step(at(100), 2 * MIB, true, false, window).unwrap();
    assert!((after - 20.0 * mib).abs() < mib, "{after}");
    // a stream that ended before it delivered anything measures again
    let mut probe = Probe::Wait { id: 1, before: 1.0 };
    assert_eq!(probe.step(at(10), 5, false, true, window), None);
    assert!(matches!(probe, Probe::Measure { delivered: 5, .. }));
}

#[test]
fn a_stream_is_added_only_when_a_slot_is_free() {
    let body = body(2 * MIB);
    let server = serve(body.clone(), SERVING);
    let net = net(&server.url, 1);
    let (_folder, cache) = cache();
    let fetched = download(&net, &cache, &server.url, Expect::default());
    assert_eq!(fetched.digest, sha256(&body));
    assert_eq!(server.heads().len(), 1);
}

#[test]
fn a_resume_fetches_only_the_ranges_not_kept() {
    for when in [Splitting::Always, Splitting::WhenSlow] {
        let body = body(4 * MIB);
        let server = serve(body.clone(), SERVING);
        let mut net = net(&server.url, 8);
        net.split.when = when;
        let (_folder, cache) = cache();
        let validators = Validators {
            etag: Some("\"v1\"".to_owned()),
            last_modified: None,
            ..Validators::default()
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
        assert_eq!(fetched.digest, sha256(&body), "{when:?}");
        let first = &server.heads()[0];
        assert!(first.contains(&format!("range: bytes={MIB}-")), "{first}");
        assert!(first.contains("if-range: \"v1\""), "{first}");
        let counts = net.work.counts();
        assert_eq!(counts.resumes, 1, "{when:?}");
        assert!(
            counts.bytes_down < 3 * MIB,
            "the kept megabyte at the end came again, {when:?}: {} bytes",
            counts.bytes_down
        );
    }
}

#[test]
fn a_rate_capped_resume_fetches_only_the_ranges_not_kept() {
    for when in [Splitting::Always, Splitting::WhenSlow] {
        let body = body(4 * MIB);
        let server = serve(body.clone(), SERVING);
        let rate = Rate::read(&|_| Some("1M".into())).unwrap();
        let mut net = net(&server.url, 8).with_rate(rate);
        net.split.when = when;
        let (_folder, cache) = cache();
        let validators = Validators {
            etag: Some("\"v1\"".to_owned()),
            last_modified: None,
            ..Validators::default()
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
        let fetch = Fetch::new(server.url.clone());
        // a hang guard only: a round takes about a second, and a busy disk has held the cache's
        // writes for 15 s
        let fetched = block_on(async {
            tokio::time::timeout(
                Duration::from_secs(120),
                net.download(&fetch, &cache, &|_| Ok(())),
            )
            .await
            .unwrap()
        })
        .unwrap();
        assert_eq!(fetched.size, 4 * MIB, "{when:?}");
        assert_eq!(fetched.digest, sha256(&body), "{when:?}");
        assert_eq!(fs::read(cache.object_path(&fetched.digest)).unwrap(), *body);
        let first = &server.heads()[0];
        assert!(first.contains(&format!("range: bytes={MIB}-")), "{first}");
        assert!(first.contains("if-range: \"v1\""), "{first}");
        let counts = net.work.counts();
        assert_eq!(counts.resumes, 1, "{when:?}");
        assert_eq!(counts.retries, 0, "{when:?}");
        assert!(
            counts.bytes_down < 3 * MIB,
            "the kept megabyte at the end came again, {when:?}: {} bytes",
            counts.bytes_down
        );
    }
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

#[test]
fn a_full_reply_replaces_saved_ranges_when_prefix_empty() {
    let body = body(4 * MIB);
    for pinned in [false, true] {
        let server = serve(
            body.clone(),
            Serving {
                etag: Some("\"v2\""),
                // one stream may run, so pacing it would only make the test slow
                rate: u64::MAX,
                ..SERVING
            },
        );
        let net = net(&server.url, 1);
        let (_folder, cache) = cache();
        let crate::cache::Claim::Owned(partial) = cache.claim(server.url.as_str()).unwrap() else {
            panic!("nothing else holds the download");
        };
        let validators = Validators {
            etag: Some("\"v1\"".to_owned()),
            last_modified: None,
            ..Validators::default()
        };
        let mut writer = partial
            .writer(0, Some(4 * MIB), validators, false, true)
            .unwrap();
        writer
            .write_at(3 * MIB, &vec![0xff; usize::try_from(MIB).unwrap()])
            .unwrap();
        writer.save().unwrap();
        drop(writer);
        assert_eq!(partial.resumable().unwrap().0, 0);
        drop(partial);
        let fetched = download(
            &net,
            &cache,
            &server.url,
            Expect {
                sha256: pinned.then(|| sha256(&body)),
                ..Expect::default()
            },
        );
        assert_eq!(fetched.digest, sha256(&body), "pinned={pinned}");
        assert_eq!(fs::read(cache.object_path(&fetched.digest)).unwrap(), *body);
        let heads = server.heads();
        assert!(heads[0].contains("range: bytes=0-"), "{}", heads[0]);
        assert!(heads[0].contains("if-range: \"v1\""), "{}", heads[0]);
        assert_eq!(
            heads.len(),
            1,
            "a full replacement needs no further request"
        );
    }
}

#[test]
fn terminal_stream_failures_win_in_either_arrival_order() {
    let url = Url::parse("https://example.org/file").unwrap();
    for terminal_first in [false, true] {
        let terminal = failure::refusal(&url, StatusCode::FORBIDDEN, &header::HeaderMap::new());
        let transient = failure::transient("a connection broke".to_owned());
        let failures = if terminal_first {
            [terminal, transient]
        } else {
            [transient, terminal]
        };
        let mut outcome = None;
        for failure in failures {
            keep_failure(&mut outcome, failure);
        }
        assert!(matches!(outcome, Some(Failure::Final(error)) if error.kind == Kind::AuthRequired));
    }
}
