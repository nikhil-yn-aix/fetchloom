//! a host on this machine that limits how often it may be asked, the way zenodo does: a quota for
//! each second, stated on every answer, and a 429 for a request past it.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::*;
use crate::cache::Cache;
use crate::net::download::Fetch;
use crate::net::{Net, Notice, Offline, Options};
use crate::source::recorded::block_on;

/// requests the host takes in each second.
const QUOTA: u64 = 5;

struct Host {
    url: Url,
    refused: Arc<AtomicUsize>,
}

fn serve() -> Host {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    // the second the count is for, and the requests in it
    let window = Arc::new(Mutex::new((0, 0)));
    let refused = Arc::new(AtomicUsize::new(0));
    let (shared, count) = (window.clone(), refused.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (window, refused) = (shared.clone(), count.clone());
            std::thread::spawn(move || answer(stream, &window, &refused));
        }
    });
    Host { url, refused }
}

fn answer(mut stream: TcpStream, window: &Mutex<(u64, u64)>, refused: &AtomicUsize) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
        head.push(byte[0]);
    }
    let now = SystemTime::now();
    let second = now.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let taken = {
        let mut window = window.lock().unwrap();
        if window.0 != second {
            *window = (second, 0);
        }
        window.1 += 1;
        window.1
    };
    // zenodo sends retry-after on every answer. on one that worked it must slow nothing, so it
    // says far longer here than the limit takes to reset
    let (status, body, retry_after) = if taken > QUOTA {
        refused.fetch_add(1, Ordering::SeqCst);
        ("429 Too Many Requests", "", 1)
    } else {
        ("200 OK", "a file", 30)
    };
    let text = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\nDate: {}\r\n\
         X-RateLimit-Limit: {QUOTA}\r\nX-RateLimit-Remaining: {}\r\nX-RateLimit-Reset: {}\r\n\
         Retry-After: {retry_after}\r\n\r\n{body}",
        body.len(),
        httpdate::fmt_http_date(now),
        QUOTA.saturating_sub(taken),
        second + 1,
    );
    let _ = stream.write_all(text.as_bytes());
}

#[test]
fn a_host_is_asked_no_faster_than_its_limit_allows() {
    let host = serve();
    let options = Options {
        offline: Offline::No,
        jobs: 8,
        credentials: Vec::new(),
        split: crate::net::Splitting::WhenSlow,
    };
    let waits = Arc::new(Mutex::new(Vec::new()));
    let heard = waits.clone();
    let notify: crate::net::Notify = Arc::new(move |notice| {
        if let Notice::Waiting { quota, .. } = notice {
            heard.lock().unwrap().push(quota);
        }
    });
    let net = Net::new(options, Arc::default(), Arc::default(), notify);
    net.typed(&host.url);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let started = Instant::now();
    // four at a time, as many as one host gets
    let batch = |first: usize| {
        let (net, cache, url) = (&net, &cache, &host.url);
        async move {
            for n in (first..12).step_by(4) {
                let fetch = Fetch::new(url.join(&n.to_string()).unwrap());
                net.download(&fetch, cache, &|_| Ok(())).await.unwrap();
            }
        }
    };
    block_on(async { tokio::join!(batch(0), batch(1), batch(2), batch(3)) });
    assert_eq!(
        host.refused.load(Ordering::SeqCst),
        0,
        "a request went past the limit"
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(1),
        "12 requests at 5 a second took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "retry-after on answers that worked slowed the run to {elapsed:?}"
    );
    assert_eq!(net.work.counts().retries, 0);
    let waits = waits.lock().unwrap();
    assert!(
        (1..=3).contains(&waits.len()),
        "one notice for each reset waited for, not one for each request: {waits:?}"
    );
    assert!(waits.iter().all(|quota| *quota == Some(QUOTA)), "{waits:?}");
}

fn date(text: &str) -> SystemTime {
    httpdate::parse_http_date(text).unwrap()
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    pairs
        .iter()
        .map(|(name, value)| {
            (
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            )
        })
        .collect()
}

#[test]
fn zenodo_states_its_limit_on_every_answer_by_its_own_clock() {
    // the reset is 58 s after the host's date, while this machine's clock runs 100 s ahead
    let answer = headers(&[
        ("date", "Fri, 30 Oct 2026 08:55:06 GMT"),
        ("x-ratelimit-limit", "133"),
        ("x-ratelimit-remaining", "130"),
        ("x-ratelimit-reset", "1793350564"),
        ("retry-after", "57"),
    ]);
    let ahead = date("Fri, 30 Oct 2026 08:56:46 GMT");
    let stated = limits(StatusCode::OK, &answer, ahead);
    assert_eq!(
        stated,
        [Limit {
            name: String::new(),
            quota: Some(133),
            window: None,
            remaining: 130,
            reset: Duration::from_secs(58),
            stamp: Some(1_793_350_564),
        }]
    );
    assert_eq!(
        used_up(&stated),
        None,
        "retry-after on a 200 asks for nothing"
    );
    let plain = headers(&[("retry-after", "57")]);
    assert_eq!(limits(StatusCode::OK, &plain, ahead), []);
}

#[test]
fn hugging_face_names_each_limit() {
    let answer = headers(&[
        ("ratelimit", "\"api\";r=0;t=9"),
        ("ratelimit-policy", "\"fixed window\";\"api\";q=500;w=300"),
    ]);
    let stated = limits(StatusCode::TOO_MANY_REQUESTS, &answer, SystemTime::now());
    assert_eq!(
        stated,
        [Limit {
            name: "api".to_owned(),
            quota: Some(500),
            window: Some(Duration::from_secs(300)),
            remaining: 0,
            reset: Duration::from_secs(9),
            stamp: None,
        }]
    );
    assert_eq!(used_up(&stated), Some(Duration::from_secs(9)));
}

#[test]
fn a_429_is_used_up_for_as_long_as_it_asks() {
    let now = SystemTime::now();
    let bare = headers(&[("retry-after", "30")]);
    assert_eq!(
        used_up(&limits(StatusCode::TOO_MANY_REQUESTS, &bare, now)),
        Some(Duration::from_secs(30))
    );
    let counted = headers(&[
        ("x-ratelimit-remaining", "0"),
        ("x-ratelimit-reset", "5"),
        ("retry-after", "20"),
    ]);
    assert_eq!(
        used_up(&limits(StatusCode::TOO_MANY_REQUESTS, &counted, now)),
        Some(Duration::from_secs(20)),
        "the longer of the two"
    );
    assert_eq!(
        used_up(&limits(
            StatusCode::TOO_MANY_REQUESTS,
            &HeaderMap::new(),
            now
        )),
        None,
        "a 429 that says nothing of when is retried as before"
    );
}

fn limit(remaining: u64, reset: u64, stamp: Option<u64>) -> Limit {
    Limit {
        name: String::new(),
        quota: Some(10),
        window: None,
        remaining,
        reset: Duration::from_secs(reset),
        stamp,
    }
}

#[test]
fn requests_stop_while_a_limit_is_used_up_and_start_at_its_reset() {
    let paces = Paces::default();
    let start = Instant::now();
    let first = paces.admit("zenodo.org", start).unwrap();
    Paces::heard(first, vec![limit(2, 10, None)], start);
    let (one, two) = (
        paces.admit("zenodo.org", start).unwrap(),
        paces.admit("zenodo.org", start).unwrap(),
    );
    let hold = paces.admit("zenodo.org", start).unwrap_err();
    assert_eq!(hold.until, start + Duration::from_secs(10));
    assert_eq!(hold.quota, Some(10));
    assert!(hold.announce);
    assert!(
        !paces.admit("zenodo.org", start).unwrap_err().announce,
        "the second request waiting for the same reset says nothing"
    );
    assert!(
        paces.admit("example.org", start).is_ok(),
        "another host is not held"
    );
    drop((one, two));
    assert!(
        paces
            .admit("zenodo.org", start + Duration::from_secs(10))
            .is_ok()
    );
}

#[test]
fn short_waits_just_after_a_reset_pass_quietly() {
    let paces = Paces::default();
    let start = Instant::now();
    let wait = |at: Instant, reset: u64| {
        let ticket = paces.admit("zenodo.org", at).unwrap();
        Paces::heard(ticket, vec![limit(0, reset, None)], at);
        paces.admit("zenodo.org", at).unwrap_err().announce
    };
    assert!(wait(start, 40));
    let reset = start + Duration::from_secs(40);
    assert!(
        !wait(reset, 3),
        "a 429 for a few seconds more right after the reset says nothing again"
    );
    assert!(wait(reset + Duration::from_secs(20), 40));
}

#[test]
fn a_limit_that_resets_too_late_to_wait_for_is_left_to_the_host() {
    let paces = Paces::default();
    let start = Instant::now();
    let first = paces.admit("huggingface.co", start).unwrap();
    Paces::heard(first, vec![limit(0, 300, None)], start);
    assert!(paces.admit("huggingface.co", start).is_ok());
}

#[test]
fn answers_from_one_window_keep_its_lowest_count() {
    let paces = Paces::default();
    let start = Instant::now();
    let tickets: Vec<_> = (0..3)
        .map(|_| paces.admit("zenodo.org", start).unwrap())
        .collect();
    let mut tickets = tickets.into_iter();
    // answers come back out of order: the second window's first, then the first window's last
    let next = 1_793_350_624;
    Paces::heard(
        tickets.next().unwrap(),
        vec![limit(9, 61, Some(next))],
        start,
    );
    Paces::heard(
        tickets.next().unwrap(),
        vec![limit(0, 1, Some(next - 60))],
        start,
    );
    let admitted = paces.admit("zenodo.org", start);
    assert!(
        admitted.is_ok(),
        "an answer from the window before changes nothing"
    );
    Paces::heard(
        tickets.next().unwrap(),
        vec![limit(1, 61, Some(next))],
        start,
    );
    assert!(
        paces.admit("zenodo.org", start).is_err(),
        "one was left, and the request admitted above takes it"
    );
    drop(admitted);
    assert!(paces.admit("zenodo.org", start).is_ok());
}
