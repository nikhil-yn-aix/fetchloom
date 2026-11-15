//! rate-limit headers, admission decisions at fixed instants, and downloads asked together.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::*;
use crate::cache::Cache;
use crate::net::download::Fetch;
use crate::net::scripted::{reply, route};
use crate::net::{Net, Offline, Options, Splitting};
use crate::source::recorded::block_on;

/// downloads that ask at once share what the host says is left, requests still on their way
/// included: 12 asked four at a time of a host that takes 5 a second go 5, 5 and 2, a window's
/// rest held to its reset rather than sent to be refused.
#[test]
fn requests_asked_at_once_never_take_more_than_the_hosts_quota() {
    let paces = Paces::default();
    let mut at = Instant::now();
    let (mut left, mut windows) = (12, Vec::new());
    while left > 0 {
        let mut taken = 0;
        loop {
            // four ask together, and the host answers each once all four went
            let asked: Vec<Ticket> = (0..left.min(4))
                .map_while(|_| paces.admit("zenodo.org", at).ok())
                .collect();
            if asked.is_empty() {
                break;
            }
            for ticket in asked {
                (taken, left) = (taken + 1, left - 1);
                let limit = Limit {
                    name: String::new(),
                    quota: Some(5),
                    window: None,
                    remaining: 5_u64.saturating_sub(taken),
                    reset: Duration::from_secs(1),
                    stamp: None,
                };
                Paces::heard(ticket, vec![limit], at);
            }
        }
        windows.push(taken);
        at += Duration::from_secs(1);
    }
    assert_eq!(windows, [5, 5, 2]);
}

/// downloads asked together send no more than the host said is left, each counted from its
/// ticket until its answer: with 3 of 4 left, of four asked at once three go and one waits for
/// the host's next window, so the host refuses none and nothing is tried again.
#[test]
fn downloads_asked_together_send_no_more_than_the_host_has_left() {
    const QUOTA: u64 = 4;
    // a window opens with the first request after the last one ended and lasts a second
    let window: Arc<Mutex<Option<(Instant, u64)>>> = Arc::default();
    let refused = Arc::new(AtomicUsize::new(0));
    let (counted, refusals) = (window, refused.clone());
    let (url, asked) = route(move |_| {
        let now = Instant::now();
        let taken = {
            let mut window = counted.lock().unwrap();
            let open = window.filter(|(start, _)| now < *start + Duration::from_secs(1));
            let (start, taken) = open.unwrap_or((now, 0));
            *window = Some((start, taken + 1));
            taken + 1
        };
        let left = QUOTA.saturating_sub(taken).to_string();
        let quota = QUOTA.to_string();
        let headers = [
            ("X-RateLimit-Limit", quota.as_str()),
            ("X-RateLimit-Remaining", left.as_str()),
            ("X-RateLimit-Reset", "1"),
        ];
        if taken > QUOTA {
            refusals.fetch_add(1, Ordering::SeqCst);
            reply("429 Too Many Requests", &headers, b"")
        } else {
            reply("200 OK", &headers, b"a file")
        }
    });
    let options = Options {
        offline: Offline::No,
        jobs: 8,
        credentials: Vec::new(),
        split: Splitting::Never,
    };
    let net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
    net.typed(&url);
    let folder = tempfile::tempdir().unwrap();
    let cache = Cache::open(folder.path().join("cache")).unwrap();
    let download = |n: u32| {
        let fetch = Fetch::new(url.join(&n.to_string()).unwrap());
        let (net, cache) = (&net, &cache);
        async move { net.download(&fetch, cache, &|_| Ok(())).await.map(|_| ()) }
    };
    // the first answer tells what the host has left
    block_on(download(0)).unwrap();
    let together =
        block_on(async { tokio::join!(download(1), download(2), download(3), download(4)) });
    assert!(together.0.is_ok() && together.1.is_ok() && together.2.is_ok() && together.3.is_ok());
    assert_eq!(
        refused.load(Ordering::SeqCst),
        0,
        "a request went past the limit"
    );
    assert_eq!(net.work.counts().retries, 0);
    assert_eq!(asked.lock().unwrap().len(), 5);
}

#[test]
fn a_host_is_asked_no_faster_than_its_limit_allows() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    for window in 0..3 {
        let at = start + Duration::from_secs(window);
        for remaining in (0..5).rev() {
            let ticket = paces.admit("zenodo.org", at).unwrap();
            Paces::heard(
                ticket,
                vec![Limit {
                    name: String::new(),
                    quota: Some(5),
                    window: None,
                    remaining,
                    reset: Duration::from_secs(1),
                    stamp: Some(1_793_350_507 + window),
                }],
                at,
            );
        }
        for request in 0..4 {
            assert_eq!(
                paces.admit("zenodo.org", at).unwrap_err(),
                Hold {
                    until: at + Duration::from_secs(1),
                    quota: Some(5),
                    window: None,
                    announce: window == 0 && request == 0,
                }
            );
        }
        assert_eq!(
            paces
                .admit("zenodo.org", at + Duration::from_millis(999))
                .unwrap_err(),
            Hold {
                until: at + Duration::from_secs(1),
                quota: Some(5),
                window: None,
                announce: false,
            }
        );
    }
    assert!(
        paces
            .admit("zenodo.org", start + Duration::from_secs(3))
            .is_ok()
    );
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

/// a 503's `Retry-After` holds every request to its host until then, as a 429's does, not only the
/// one it answered: a host that asks its requests to start apart waits for it past its own pause.
#[test]
fn a_503s_retry_after_holds_every_request_to_its_host() {
    let now = SystemTime::now();
    let asked = headers(&[("retry-after", "30")]);
    assert_eq!(
        used_up(&limits(StatusCode::SERVICE_UNAVAILABLE, &asked, now)),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        used_up(&limits(
            StatusCode::SERVICE_UNAVAILABLE,
            &HeaderMap::new(),
            now
        )),
        None,
        "a 503 that says nothing of when holds nothing"
    );
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let arxiv =
        crate::source::rules().paced(&Url::parse("https://export.arxiv.org/api/query").unwrap());
    for host in ["zenodo.org", arxiv.as_str()] {
        let ticket = paces.admit(host, at(0)).unwrap();
        let refused = answered(StatusCode::SERVICE_UNAVAILABLE, ticket.sent, at(0), 0.0);
        paces.answered(host, &refused);
        Paces::heard(
            ticket,
            limits(StatusCode::SERVICE_UNAVAILABLE, &asked, now),
            at(0),
        );
        assert_eq!(
            paces.admit(host, at(4)).unwrap_err().until,
            at(30),
            "{host}"
        );
        assert!(paces.admit(host, at(30)).is_ok(), "{host}");
    }
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
    let paces = Paces::spaced_by(crate::source::rules().spacing);
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

/// NCBI asks no more than three a second of a program without a key, however many try at once. the
/// waits are too short to tell, and other hosts are not held.
#[test]
fn requests_to_ncbi_never_start_more_than_three_in_a_second() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let mut starts = Vec::new();
    let mut at = start;
    while starts.len() < 10 {
        match paces.admit("ftp.ncbi.nlm.nih.gov", at) {
            Ok(ticket) => {
                starts.push(at);
                drop(ticket);
            }
            Err(hold) => {
                assert_eq!(hold.told(at), None, "a wait this short is not told");
                assert!(
                    paces.admit("ftp.sra.ebi.ac.uk", at).is_ok(),
                    "another host is not held"
                );
                at = hold.until;
            }
        }
    }
    for (index, first) in starts.iter().enumerate() {
        let within = starts[index..]
            .iter()
            .filter(|later| **later < *first + Duration::from_secs(1))
            .count();
        assert!(within <= 3, "{within} starts within a second of {index}");
    }
    assert!(
        starts[9] - starts[0] < Duration::from_secs(4),
        "they wait no longer than they must"
    );
}

/// arXiv's terms limit its hosts together, so a request to one waits three seconds after a request
/// to another, while the mirror on google's storage is not held.
#[test]
fn requests_to_arxivs_hosts_start_three_seconds_apart_whichever_they_ask() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let name = |link: &str| crate::source::rules().paced(&Url::parse(link).unwrap());
    let first = paces.admit(&name("https://export.arxiv.org/api/query"), start);
    drop(first.unwrap());
    let later = start + Duration::from_secs(1);
    let hold = paces
        .admit(&name("https://oaipmh.arxiv.org/oai"), later)
        .unwrap_err();
    assert_eq!(hold.until, start + Duration::from_secs(3));
    // a name with a closing dot is the same host
    for dotted in [
        "https://export.arxiv.org./api/query",
        "https://ARXIV.org../pdf/1706.03762v7",
    ] {
        let same = paces.admit(&name(dotted), later).unwrap_err();
        assert_eq!(same.until, hold.until, "{dotted}");
    }
    assert!(
        paces
            .admit(
                &name("https://storage.googleapis.com/arxiv-dataset/x"),
                later
            )
            .is_ok()
    );
    assert!(
        paces
            .admit(&name("https://arxiv.org/pdf/1706.03762v7"), hold.until)
            .is_ok()
    );
}

/// each refusal for load doubles the pause's ceiling, of which the first half is always waited.
#[test]
fn a_pause_after_refusals_doubles_up_to_a_minute_with_a_random_second_half() {
    let seconds = Duration::from_secs_f64;
    for (spacing, refused, share, pause) in [
        (3.0, 0, 0.7, 3.0),
        (3.0, 1, 0.0, 3.0),
        (3.0, 1, 1.0, 6.0),
        (3.0, 2, 0.5, 9.0),
        (3.0, 3, 0.0, 12.0),
        (3.0, 5, 1.0, 60.0),
        (3.0, 40, 0.0, 30.0),
        (1.0, 1, 0.5, 1.5),
        (1.0, 1, 2.0, 2.0),
    ] {
        assert_eq!(
            paused(seconds(spacing), refused, share),
            seconds(pause),
            "{spacing} s after {refused} refusals with {share}"
        );
    }
}

/// the answer `status` to a request sent at `sent`, come at `at`, with `share` of a pause's second
/// half to wait.
fn answered(status: StatusCode, sent: Instant, at: Instant, share: f64) -> Answered {
    Answered {
        status,
        sent,
        at,
        share,
    }
}

/// arXiv asks for three seconds between requests; once it refuses one for load, the next waits
/// longer, until an answer that is no refusal.
#[test]
fn a_paced_host_that_refuses_for_load_is_asked_less_often_until_it_answers() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let host =
        crate::source::rules().paced(&Url::parse("https://export.arxiv.org/api/query").unwrap());
    let start = Instant::now();
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let sent = paces.admit(&host, at(0)).unwrap().sent;
    paces.answered(
        &host,
        &answered(StatusCode::SERVICE_UNAVAILABLE, sent, at(1), 1.0),
    );
    assert_eq!(paces.pause(&host, at(1)), Some(Duration::from_secs(6)));
    assert_eq!(paces.admit(&host, at(4)).unwrap_err().until, at(7));
    let sent = paces.admit(&host, at(7)).unwrap().sent;
    paces.answered(
        &host,
        &answered(StatusCode::TOO_MANY_REQUESTS, sent, at(8), 0.0),
    );
    assert_eq!(paces.admit(&host, at(10)).unwrap_err().until, at(14));
    let sent = paces.admit(&host, at(14)).unwrap().sent;
    paces.answered(&host, &answered(StatusCode::OK, sent, at(15), 0.0));
    assert_eq!(paces.pause(&host, at(15)), None);
    assert_eq!(paces.admit(&host, at(16)).unwrap_err().until, at(17));
    assert!(!paces.busy(&host));
}

/// NCBI takes four requests at once, and four refused in one burst are one refusal; four
/// refusals of requests each sent after the pause the last one set are four in a row, which stop
/// the host, unless an answer between them ended the run.
#[test]
fn a_paced_host_that_keeps_refusing_is_asked_nothing_more_while_others_are_never_paused() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let ncbi =
        crate::source::rules().paced(&Url::parse("https://eutils.ncbi.nlm.nih.gov/x").unwrap());
    let start = Instant::now();
    let later = |minutes: u64| start + Duration::from_secs(61 * minutes);
    let refused = |sent: Instant| answered(StatusCode::TOO_MANY_REQUESTS, sent, sent, 0.0);
    for _ in 0..4 {
        paces.answered(&ncbi, &refused(start));
    }
    assert!(!paces.busy(&ncbi), "a burst counts once");
    for minutes in 1..3 {
        paces.answered(&ncbi, &refused(later(minutes)));
    }
    assert!(!paces.busy(&ncbi));
    paces.answered(&ncbi, &answered(StatusCode::OK, later(3), later(3), 0.0));
    for minutes in 4..7 {
        paces.answered(&ncbi, &refused(later(minutes)));
    }
    assert!(!paces.busy(&ncbi), "an answer between them ended the run");
    paces.answered(&ncbi, &refused(later(7)));
    assert!(paces.busy(&ncbi));
    for minutes in 0..10 {
        paces.answered("zenodo.org", &refused(later(minutes)));
    }
    assert!(!paces.busy("zenodo.org"));
    assert_eq!(paces.pause("zenodo.org", start), None);
}

/// zenodo's answers once its limit is used up: a 429 whose limit resets a second after its date,
/// then 429s sent after that reset, which spend no try. the host's clock decides, not this one.
#[test]
fn a_used_up_limit_is_waited_out_once_and_a_passed_reset_holds_nothing() {
    let sent = "Fri, 30 Oct 2026 08:55:06 GMT";
    let second = date(sent).duration_since(UNIX_EPOCH).unwrap().as_secs();
    let refusal = |reset: u64, retry_after: &str| {
        headers(&[
            ("date", sent),
            ("x-ratelimit-limit", "133"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &reset.to_string()),
            ("retry-after", retry_after),
        ])
    };
    for clock in [
        date(sent),
        date("Fri, 30 Oct 2026 08:36:46 GMT"),
        date("Fri, 30 Oct 2026 09:13:26 GMT"),
    ] {
        let paces = Paces::spaced_by(crate::source::rules().spacing);
        let start = Instant::now();
        let first = paces.admit("zenodo.org", start).unwrap();
        let stated = limits(
            StatusCode::TOO_MANY_REQUESTS,
            &refusal(second + 1, "1"),
            clock,
        );
        assert_eq!(used_up(&stated), Some(Duration::from_secs(1)));
        Paces::heard(first, stated, start);
        let hold = paces.admit("zenodo.org", start).unwrap_err();
        assert_eq!(hold.until, start + Duration::from_secs(1));
        assert_eq!(hold.told(start), Some(Duration::from_secs(1)));
        let mut at = hold.until;
        for _ in 0..5 {
            let ticket = paces.admit("zenodo.org", at).unwrap();
            let stated = limits(StatusCode::TOO_MANY_REQUESTS, &refusal(second, "0"), clock);
            assert_eq!(used_up(&stated), Some(Duration::ZERO));
            Paces::heard(ticket, stated, at);
            at += Duration::from_millis(1);
        }
        assert!(paces.admit("zenodo.org", at).is_ok());
    }
}

/// the first request to wait for the reset tells of the wait; a shorter one passes without a line.
#[test]
fn a_wait_is_told_once_while_half_a_second_is_left() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let ticket = paces.admit("zenodo.org", start).unwrap();
    Paces::heard(ticket, vec![limit(0, 2, None)], start);
    let first = paces.admit("zenodo.org", start).unwrap_err();
    let reset = start + Duration::from_secs(2);
    assert_eq!(first.told(start), Some(Duration::from_secs(2)));
    let late = start + Duration::from_millis(1500);
    assert_eq!(first.told(late), Some(Duration::from_millis(500)));
    assert_eq!(first.told(late + Duration::from_millis(1)), None);
    assert_eq!(first.told(reset + Duration::from_secs(1)), None);
    let second = paces.admit("zenodo.org", start).unwrap_err();
    assert_eq!(second.told(start), None, "the same reset is told once");
}

#[test]
fn short_waits_just_after_a_reset_pass_quietly() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
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
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let first = paces.admit("huggingface.co", start).unwrap();
    Paces::heard(first, vec![limit(0, 300, None)], start);
    assert!(paces.admit("huggingface.co", start).is_ok());
}

/// a server may state any number, and adding the largest to a clock would end the process.
#[test]
fn a_reset_too_far_to_add_to_a_clock_is_capped() {
    let answer = headers(&[("ratelimit", "\"api\";r=0;t=18446744073709551615")]);
    let stated = limits(StatusCode::TOO_MANY_REQUESTS, &answer, SystemTime::now());
    assert_eq!(stated[0].reset, FARTHEST);
    let paces = Paces::spaced_by(crate::source::rules().spacing);
    let start = Instant::now();
    let ticket = paces.admit("huggingface.co", start).unwrap();
    Paces::heard(ticket, stated, start);
    assert!(paces.admit("huggingface.co", start).is_ok());
}

#[test]
fn answers_from_one_window_keep_its_lowest_count() {
    let paces = Paces::spaced_by(crate::source::rules().spacing);
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

#[test]
fn missing_policy_headers_consume_remembered_allowances() {
    for another_policy in [false, true] {
        let paces = Paces::spaced_by(crate::source::rules().spacing);
        let start = Instant::now();
        let first = paces.admit("example.org", start).unwrap();
        let mut known = vec![limit(1, 10, Some(1_793_350_624))];
        if another_policy {
            let mut other = limit(5, 10, Some(1_793_350_624));
            other.name = "other".to_owned();
            known.push(other);
        }
        Paces::heard(first, known, start);
        let spent = paces.admit("example.org", start).unwrap();
        let mut stated = Vec::new();
        if another_policy {
            let mut other = limit(4, 10, Some(1_793_350_624));
            other.name = "other".to_owned();
            stated.push(other);
        }
        Paces::heard(spent, stated, start);
        let held = paces.admit("example.org", start).unwrap_err();
        assert_eq!(held.until, start + Duration::from_secs(10));
        assert!(
            paces
                .admit("example.org", start + Duration::from_secs(10))
                .is_ok()
        );
    }
}
