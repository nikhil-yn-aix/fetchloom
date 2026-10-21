//! Resuming a download: which requests are sent for bytes already kept, and what is done with
//! each answer.

use fl_model::error::ErrorKind;
use fl_model::partial::{PartialRecord, Validator};
use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;

use crate::support::memory::{Memory, Waits, client};
use crate::support::{Recorded, Reply, Server};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[allow(
    clippy::cast_possible_truncation,
    reason = "the index is reduced to one byte on purpose"
)]
fn pattern(length: usize) -> Vec<u8> {
    (0..length).map(|i| (i * 7 + i / 13) as u8).collect()
}

fn job(server: &Server) -> Result<Download, Box<dyn std::error::Error>> {
    Ok(Download {
        url: SafeUrl::parse(&server.url("/file.bin"))?,
        length: None,
        stated: Vec::new(),
        stable: false,
        typed: true,
    })
}

fn kept(job: &Download, body: &[u8], count: usize, total: u64, validator: Validator) -> Memory {
    Memory::kept(
        &body[..count],
        PartialRecord {
            location: job.url.to_string(),
            length: Some(total),
            validator,
            received: count as u64,
        },
    )
}

fn partial(body: &[u8], from: usize) -> Reply {
    Reply {
        status: 206,
        ..Reply::ok(body[from..].to_vec())
    }
    .header(
        "content-range",
        &format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
    )
}

fn ranged(request: &Recorded) -> bool {
    request.header("range").is_some()
}

#[tokio::test]
async fn a_strong_etag_resumes_with_if_range() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            partial(&payload, 400).header("etag", "\"v1\"")
        } else {
            Reply::ok(payload.clone())
        }
    })
    .await?;
    let job = job(&server)?;
    let sink = kept(
        &job,
        &body,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[0].header("if-range"), Some("\"v1\""));
    let counted = client.counters().read();
    assert_eq!(counted.requests, 1);
    assert_eq!(counted.bytes_down, 600);
    assert_eq!(counted.resumes, 1);
    assert!(downloaded.resumed);
    assert!(!downloaded.restarted);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn a_weak_etag_that_changed_starts_again_without_if_range() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            partial(&payload, 400).header("etag", "W/\"v2\"")
        } else {
            Reply::ok(payload.clone()).header("etag", "W/\"v2\"")
        }
    })
    .await?;
    let job = job(&server)?;
    let stale: Vec<u8> = body[..400].iter().map(|byte| byte ^ 0xff).collect();
    let sink = kept(
        &job,
        &stale,
        400,
        1000,
        Validator::WeakEtag("W/\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[0].header("if-range"), None);
    assert_eq!(requests[1].header("range"), None);
    assert_eq!(client.counters().read().requests, 2);
    assert!(downloaded.restarted);
    assert!(!downloaded.resumed);
    assert!(downloaded.sink.resets >= 1);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn a_stable_download_resumes_without_any_validator() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            partial(&payload, 400)
        } else {
            Reply::ok(payload.clone())
        }
    })
    .await?;
    let mut job = job(&server)?;
    job.stable = true;
    let sink = kept(&job, &body, 400, 1000, Validator::None);
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[0].header("if-range"), None);
    assert!(downloaded.resumed);
    assert_eq!(client.counters().read().resumes, 1);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn a_full_answer_to_a_range_replaces_the_kept_bytes() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |_| Reply::ok(payload.clone())).await?;
    let job = job(&server)?;
    let stale: Vec<u8> = body[..400].iter().map(|byte| byte ^ 0xff).collect();
    let sink = kept(
        &job,
        &stale,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    let counted = client.counters().read();
    assert_eq!(counted.requests, 1);
    assert_eq!(counted.bytes_down, 1000);
    assert_eq!(counted.resumes, 0);
    assert!(downloaded.restarted);
    assert!(!downloaded.resumed);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn an_unsatisfiable_range_asks_again_from_zero() -> TestResult {
    let new = pattern(1200);
    let payload = new.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            Reply::status(416).header("content-range", "bytes */1200")
        } else {
            Reply::ok(payload.clone())
        }
    })
    .await?;
    let job = job(&server)?;
    let old = pattern(1000);
    let sink = kept(
        &job,
        &old,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[1].header("range"), None);
    assert!(downloaded.restarted);
    assert_eq!(downloaded.bytes, 1200);
    assert_eq!(downloaded.sink.bytes, new);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&new).as_bytes());
    Ok(())
}

#[tokio::test]
async fn an_unsatisfiable_range_over_all_kept_bytes_is_done() -> TestResult {
    let body = pattern(1000);
    let server =
        Server::plain(|_| Reply::status(416).header("content-range", "bytes */1000")).await?;
    let job = job(&server)?;
    let sink = kept(
        &job,
        &body,
        1000,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), Some("bytes=1000-"));
    let counted = client.counters().read();
    assert_eq!(counted.requests, 1);
    assert_eq!(counted.bytes_down, 0);
    assert_eq!(downloaded.bytes, 1000);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn a_second_unsatisfiable_answer_is_a_final_status() -> TestResult {
    let server = Server::plain(|request| {
        if ranged(request) {
            Reply::status(416).header("content-range", "bytes */1200")
        } else {
            Reply::status(416)
        }
    })
    .await?;
    let job = job(&server)?;
    let old = pattern(1000);
    let sink = kept(
        &job,
        &old,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let waits = Waits::default();
    let client = client(&waits)?;
    let error = client
        .download(&job, sink)
        .await
        .err()
        .ok_or("expected the download to fail")?;
    assert_eq!(error.kind(), ErrorKind::NetworkStatus);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[1].header("range"), None);
    assert_eq!(client.counters().read().retries, 0);
    Ok(())
}

#[tokio::test]
async fn bytes_kept_for_another_location_are_not_resumed() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |_| Reply::ok(payload.clone())).await?;
    let job = job(&server)?;
    let stale: Vec<u8> = body[..400].iter().map(|byte| byte ^ 0xff).collect();
    let mut sink = kept(
        &job,
        &stale,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    if let Some(record) = sink.record.as_mut() {
        record.location = "http://127.0.0.1:1/other.bin".to_owned();
    }
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), None);
    assert_eq!(requests[0].header("if-range"), None);
    assert!(downloaded.sink.resets >= 1);
    assert!(!downloaded.resumed);
    assert_eq!(downloaded.sink.bytes, body);
    assert_eq!(downloaded.hashes.blake3, *blake3::hash(&body).as_bytes());
    Ok(())
}

#[tokio::test]
async fn a_range_carrying_another_strong_etag_starts_again() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            partial(&payload, 400).header("etag", "\"v2\"")
        } else {
            Reply::ok(payload.clone()).header("etag", "\"v2\"")
        }
    })
    .await?;
    let job = job(&server)?;
    let stale: Vec<u8> = body[..400].iter().map(|byte| byte ^ 0xff).collect();
    let sink = kept(
        &job,
        &stale,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    let downloaded = client(&Waits::default())?.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("if-range"), Some("\"v1\""));
    assert_eq!(requests[1].header("range"), None);
    assert!(downloaded.restarted);
    assert_eq!(downloaded.sink.bytes, body);
    Ok(())
}

#[tokio::test]
async fn a_pinned_file_that_fails_after_resuming_starts_again_once() -> TestResult {
    use md5::Digest as _;
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |request| {
        if ranged(request) {
            partial(&payload, 400)
        } else {
            Reply::ok(payload.clone())
        }
    })
    .await?;
    let job = Download {
        stable: true,
        stated: vec![fl_model::digest::Digest::Md5(
            md5::Md5::digest(&body).into(),
        )],
        ..job(&server)?
    };
    let stale: Vec<u8> = body[..400].iter().map(|byte| byte ^ 0xff).collect();
    let sink = kept(&job, &stale, 400, 1000, Validator::None);
    let downloaded = client(&Waits::default())?.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header("range"), Some("bytes=400-"));
    assert_eq!(requests[1].header("range"), None);
    assert!(downloaded.restarted);
    assert_eq!(downloaded.sink.bytes, body);
    Ok(())
}

#[tokio::test]
async fn a_record_claiming_more_than_the_sink_holds_starts_again() -> TestResult {
    let body = pattern(1000);
    let payload = body.clone();
    let server = Server::plain(move |_| Reply::ok(payload.clone())).await?;
    let job = job(&server)?;
    let mut sink = kept(
        &job,
        &body,
        400,
        1000,
        Validator::StrongEtag("\"v1\"".to_owned()),
    );
    sink.bytes.truncate(200);
    let waits = Waits::default();
    let client = client(&waits)?;
    let downloaded = client.download(&job, sink).await?;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("range"), None);
    assert!(downloaded.sink.resets >= 1);
    assert!(!downloaded.resumed);
    assert_eq!(downloaded.sink.bytes, body);
    Ok(())
}
