//! Single downloads against real servers on loopback: requests, retries, redirects, timeouts
//! and credentials, checked by counters and by what the servers received.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_transfer::client::Config;
use fl_transfer::credentials::Chain;
use fl_transfer::download::{Download, Downloaded};
use fl_transfer::url::SafeUrl;
use md5::Digest as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::support::memory::{
    Broken, Memory, Waits, client, client_with, patient, quick, tokens, trusting,
};
use crate::support::{Body, Reply, Server, bounded};

type TestResult = Result<(), Box<dyn Error>>;

fn body(length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| u8::try_from((i * 7 + i / 251) % 256).unwrap_or(0))
        .collect()
}

fn plain(url: &str) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        url: SafeUrl::parse(url)?,
        length: None,
        stated: Vec::new(),
        stable: false,
        typed: true,
    })
}

struct Sequence(Vec<ResponseTemplate>, AtomicUsize);

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let index = self.1.fetch_add(1, Ordering::SeqCst).min(self.0.len() - 1);
        self.0[index].clone()
    }
}

async fn serve(responses: Vec<ResponseTemplate>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/file"))
        .respond_with(Sequence(responses, AtomicUsize::new(0)))
        .mount(&server)
        .await;
    server
}

async fn fetch(
    client: &fl_transfer::client::Client,
    download: &Download,
) -> Result<Downloaded<Memory>, fl_transfer::error::TransferError> {
    client.download(download, Memory::default()).await
}

#[tokio::test]
async fn a_cold_file_is_one_get_asking_for_no_encoding() -> TestResult {
    let bytes = body(300_000);
    let server = serve(vec![
        ResponseTemplate::new(200).set_body_bytes(bytes.clone()),
    ])
    .await;
    let waits = Waits::default();
    let client = client(&waits)?;
    let download = Download {
        stated: vec![Digest::Md5(md5::Md5::digest(&bytes).into())],
        length: Some(bytes.len() as u64),
        ..plain(&format!("{}/file", server.uri()))?
    };
    let done = fetch(&client, &download).await?;
    assert_eq!(done.sink.bytes, bytes);
    assert_eq!(done.hashes.blake3, *blake3::hash(&bytes).as_bytes());
    let counted = client.counters().read();
    assert_eq!(
        (counted.requests, counted.bytes_down),
        (1, bytes.len() as u64)
    );
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 1);
    let headers = &requests[0].headers;
    assert!(headers.get("accept-encoding").is_none());
    assert!(headers.get("range").is_none());
    let agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok());
    assert_eq!(
        agent,
        Some(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
    );
    Ok(())
}

#[tokio::test]
async fn a_gzip_encoded_body_is_kept_and_hashed_as_sent() -> TestResult {
    let sent = b"\x1f\x8b\x08\x00not really gzip".to_vec();
    let server = serve(vec![
        ResponseTemplate::new(200)
            .insert_header("content-encoding", "gzip")
            .set_body_bytes(sent.clone()),
    ])
    .await;
    let done = fetch(
        &client(&Waits::default())?,
        &plain(&format!("{}/file", server.uri()))?,
    )
    .await?;
    assert_eq!(done.sink.bytes, sent);
    assert_eq!(done.hashes.blake3, *blake3::hash(&sent).as_bytes());
    Ok(())
}

#[tokio::test]
async fn too_many_requests_then_success_retries_once_after_a_wait() -> TestResult {
    let server = serve(vec![
        ResponseTemplate::new(429),
        ResponseTemplate::new(200).set_body_bytes(body(10)),
    ])
    .await;
    let waits = Waits::default();
    let client = client(&waits)?;
    fetch(&client, &plain(&format!("{}/file", server.uri()))?).await?;
    let counted = client.counters().read();
    assert_eq!((counted.requests, counted.retries), (2, 1));
    assert_eq!(waits.taken().len(), 1);
    Ok(())
}

#[tokio::test]
async fn retry_after_up_to_a_minute_is_waited_at_least() -> TestResult {
    let server = serve(vec![
        ResponseTemplate::new(503).insert_header("retry-after", "45"),
        ResponseTemplate::new(200).set_body_bytes(body(10)),
    ])
    .await;
    let waits = Waits::default();
    fetch(&client(&waits)?, &plain(&format!("{}/file", server.uri()))?).await?;
    let taken = waits.taken();
    assert_eq!(taken.len(), 1);
    assert!(taken[0] >= Duration::from_secs(45), "{taken:?}");
    Ok(())
}

#[tokio::test]
async fn retry_after_over_a_minute_is_not_waited_out() -> TestResult {
    let server =
        serve(vec![ResponseTemplate::new(429).insert_header(
            "retry-after",
            "Fri, 15 Jan 2027 08:02:00 GMT",
        )])
        .await;
    let waits = Waits::default();
    let client = client(&waits)?;
    let error = fetch(&client, &plain(&format!("{}/file", server.uri()))?)
        .await
        .err()
        .ok_or("the download succeeded")?;
    assert_eq!(error.kind(), ErrorKind::NetworkRateLimited);
    assert!(waits.taken().is_empty());
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn a_busy_server_is_tried_five_times_then_given_up() -> TestResult {
    let server = serve(vec![ResponseTemplate::new(503)]).await;
    let client = client(&Waits::default())?;
    let error = bounded(fetch(&client, &plain(&format!("{}/file", server.uri()))?))
        .await?
        .err()
        .ok_or("the download succeeded")?;
    assert_eq!(error.kind(), ErrorKind::NetworkStatus);
    let counted = client.counters().read();
    assert_eq!((counted.requests, counted.retries), (5, 4));
    Ok(())
}

#[tokio::test]
async fn final_statuses_are_not_retried() -> TestResult {
    for (status, kind) in [
        (404, ErrorKind::NetworkStatus),
        (401, ErrorKind::AuthRequired),
        (403, ErrorKind::AuthRequired),
    ] {
        let server = serve(vec![ResponseTemplate::new(status)]).await;
        let client = client(&Waits::default())?;
        let error = bounded(fetch(&client, &plain(&format!("{}/file", server.uri()))?))
            .await?
            .err()
            .ok_or("the download succeeded")?;
        assert_eq!(error.kind(), kind, "{status}");
        assert_eq!(client.counters().read().requests, 1, "{status}");
    }
    Ok(())
}

#[tokio::test]
async fn a_stated_hash_that_fails_is_not_retried() -> TestResult {
    let server = serve(vec![ResponseTemplate::new(200).set_body_bytes(body(100))]).await;
    let client = client(&Waits::default())?;
    let download = Download {
        stated: vec![Digest::Md5([0; 16])],
        ..plain(&format!("{}/file", server.uri()))?
    };
    let error = bounded(fetch(&client, &download))
        .await?
        .err()
        .ok_or("the download succeeded")?;
    assert_eq!(error.kind(), ErrorKind::IntegrityMismatch);
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn offline_sends_nothing() -> TestResult {
    let server = serve(vec![ResponseTemplate::new(200)]).await;
    let config = Config {
        offline: true,
        ..quick()
    };
    let client = client_with(config, Chain::new(Vec::new()), &Waits::default())?;
    let error = fetch(&client, &plain(&format!("{}/file", server.uri()))?)
        .await
        .err()
        .ok_or("the download succeeded")?;
    assert_eq!(error.kind(), ErrorKind::OfflineMissing);
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn a_location_nobody_typed_never_reaches_a_private_address() -> TestResult {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(path("/file"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body(5)))
        .mount(&server)
        .await;
    let config = Config {
        connect: Config::default().connect,
        ..quick()
    };
    let client = client_with(config, Chain::new(Vec::new()), &Waits::default())?;
    for url in [
        format!("http://127.0.0.1:{port}/file"),
        format!("http://localhost:{port}/file"),
        "http://169.254.169.254/latest/meta-data".to_owned(),
    ] {
        let download = Download {
            typed: false,
            ..plain(&url)?
        };
        let error = fetch(&client, &download)
            .await
            .err()
            .ok_or("a location nobody typed reached a private address")?;
        assert_eq!(error.kind(), ErrorKind::NetworkRefused, "{url}");
    }
    assert_eq!(client.counters().read().bytes_down, 0);
    assert!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    let done = fetch(&client, &plain(&format!("{}/file", server.uri()))?).await?;
    assert_eq!(done.sink.bytes, body(5));
    Ok(())
}

#[tokio::test]
async fn redirects_stay_on_the_typed_host_and_never_reach_private_addresses() -> TestResult {
    let server = MockServer::start().await;
    let port = server.address().port();
    for (from, to) in [
        ("/same", format!("{}/file", server.uri())),
        (
            "/metadata",
            "http://169.254.169.254/latest/meta-data".to_owned(),
        ),
        ("/mapped", "http://[::ffff:a9fe:a9fe]/latest".to_owned()),
        ("/named", format!("http://localhost:{port}/file")),
    ] {
        Mock::given(path(from))
            .respond_with(ResponseTemplate::new(302).insert_header("location", to.as_str()))
            .mount(&server)
            .await;
    }
    Mock::given(path("/file"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body(5)))
        .mount(&server)
        .await;
    let config = Config {
        connect: Config::default().connect,
        ..quick()
    };
    let client = client_with(config, Chain::new(Vec::new()), &Waits::default())?;
    let done = fetch(&client, &plain(&format!("{}/same", server.uri()))?).await?;
    assert_eq!(done.sink.bytes, body(5));
    assert_eq!(client.counters().read().requests, 2);
    for refused in ["/metadata", "/mapped", "/named"] {
        let error = fetch(&client, &plain(&format!("{}{refused}", server.uri()))?)
            .await
            .err()
            .ok_or("a private redirect was followed")?;
        assert_eq!(error.kind(), ErrorKind::NetworkRefused, "{refused}");
    }
    let reached: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(
        reached,
        ["/same", "/file", "/metadata", "/mapped", "/named"]
    );
    Ok(())
}

#[tokio::test]
async fn silence_mid_body_times_out_and_a_trickle_does_not() -> TestResult {
    let bytes = body(10);
    let stall = bytes.clone();
    let stalled = Server::plain(move |_| Reply {
        body: Body::Stall {
            bytes: stall.clone(),
            after: 5,
        },
        ..Reply::ok(Vec::new())
    })
    .await?;
    let client = client(&Waits::default())?;
    let error = bounded(fetch(&client, &plain(&stalled.url("/"))?))
        .await?
        .err()
        .ok_or("a stalled body finished")?;
    assert_eq!(error.kind(), ErrorKind::NetworkTimeout);
    assert!(error.to_string().contains("while receiving"), "{error}");
    let trickle = bytes.clone();
    let trickling = Server::plain(move |_| Reply {
        body: Body::Trickle {
            bytes: trickle.clone(),
            every: Duration::from_millis(50),
        },
        ..Reply::ok(Vec::new())
    })
    .await?;
    let done = bounded(fetch(&client, &plain(&trickling.url("/"))?)).await??;
    assert_eq!(done.sink.bytes, bytes);
    Ok(())
}

#[tokio::test]
async fn slow_headers_time_out_and_are_asked_for_again() -> TestResult {
    let asked = AtomicUsize::new(0);
    let slow = Server::plain(move |_| {
        let delay = if asked.fetch_add(1, Ordering::SeqCst) == 0 {
            3
        } else {
            0
        };
        Reply::ok(body(5)).delayed(Duration::from_secs(delay))
    })
    .await?;
    let waits = Waits::default();
    let client = client(&waits)?;
    let done = fetch(&client, &plain(&slow.url("/"))?).await?;
    assert_eq!(done.sink.bytes, body(5));
    assert_eq!(client.counters().read().requests, 2);
    assert_eq!(waits.taken().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_credential_reaches_its_origin_and_no_other() -> TestResult {
    let other = Arc::new(Server::tls(|_| Reply::ok(body(5))).await?);
    let target = other.url("/file");
    let home = Server::tls(move |request| {
        if request.target == "/away" {
            Reply::status(302).header("Location", &target)
        } else if request.target == "/near" {
            Reply::status(302).header("Location", "/file")
        } else {
            Reply::ok(body(5))
        }
    })
    .await?;
    let client = client_with(
        trusting(&[&home, &other]),
        Chain::new(vec![Box::new(tokens(&[&home]))]),
        &Waits::default(),
    )?;
    client.trust(&SafeUrl::parse(&other.url("/"))?.origin().ok_or("origin")?);
    fetch(&client, &plain(&home.url("/file"))?).await?;
    fetch(&client, &plain(&home.url("/away"))?).await?;
    fetch(&client, &plain(&home.url("/near"))?).await?;
    let sent: Vec<Option<String>> = home
        .requests()
        .iter()
        .chain(other.requests().iter())
        .map(|request| request.header("authorization").map(str::to_owned))
        .collect();
    let bearer = Some("Bearer s3cret".to_owned());
    assert_eq!(
        sent,
        [bearer.clone(), bearer.clone(), bearer.clone(), bearer, None]
    );
    Ok(())
}

#[tokio::test]
async fn plain_http_never_carries_a_credential_and_https_never_falls_to_it() -> TestResult {
    let insecure = Arc::new(Server::plain(|_| Reply::ok(body(5))).await?);
    let target = insecure.url("/file");
    let secure = Server::tls(move |_| Reply::status(301).header("Location", &target)).await?;
    let client = client_with(
        trusting(&[&secure]),
        Chain::new(vec![Box::new(tokens(&[&insecure, &secure]))]),
        &Waits::default(),
    )?;
    fetch(&client, &plain(&insecure.url("/file"))?).await?;
    assert_eq!(insecure.requests()[0].header("authorization"), None);
    let error = fetch(&client, &plain(&secure.url("/file"))?)
        .await
        .err()
        .ok_or("a redirect to http was followed")?;
    assert_eq!(error.kind(), ErrorKind::NetworkInsecure);
    assert_eq!(insecure.requests().len(), 1);
    let user_info = insecure.url("/file").replace("http://", "http://me:pw@");
    let error = fetch(&client, &plain(&user_info)?)
        .await
        .err()
        .ok_or("a password went over http")?;
    assert_eq!(error.kind(), ErrorKind::NetworkInsecure);
    assert_eq!(insecure.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_download_has_saved_its_progress_when_it_returns() -> TestResult {
    let bytes = body(1000);
    let stall = bytes.clone();
    let stalled = Server::plain(move |_| Reply {
        body: Body::Stall {
            bytes: stall.clone(),
            after: 600,
        },
        ..Reply::ok(Vec::new())
    })
    .await?;
    let temp = tempfile::tempdir()?;
    let download = Download {
        stable: true,
        length: Some(1000),
        ..plain(&stalled.url("/"))?
    };
    let client = client(&Waits::default())?;
    let sink = crate::support::disk::Disk::open(temp.path())?;
    let error = bounded(client.download(&download, sink))
        .await?
        .err()
        .ok_or("finished")?;
    assert_eq!(error.kind(), ErrorKind::NetworkTimeout);
    let record = crate::support::disk::read_record(&temp.path().join("record.json"))
        .ok_or("no record was saved")?;
    assert_eq!(record.received, 600);
    assert_eq!(client.counters().read().requests, 5);
    Ok(())
}

#[tokio::test]
async fn a_sink_that_fails_is_reported_as_itself_and_not_retried() -> TestResult {
    let server = serve(vec![
        ResponseTemplate::new(200).set_body_bytes(body(600_000)),
    ])
    .await;
    let client = client(&Waits::default())?;
    let download = Download {
        stable: true,
        ..plain(&format!("{}/file", server.uri()))?
    };
    let error = bounded(client.download(&download, Broken))
        .await?
        .err()
        .ok_or("a broken sink took the bytes")?;
    assert_eq!(error.kind(), ErrorKind::LocalIo);
    assert!(error.to_string().ends_with("the disk is full"), "{error}");
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn a_credential_place_used_alone_still_never_goes_over_http() -> TestResult {
    let insecure = Server::plain(|_| Reply::ok(body(5))).await?;
    let client = client_with(quick(), tokens(&[&insecure]), &Waits::default())?;
    fetch(&client, &plain(&insecure.url("/file"))?).await?;
    assert_eq!(insecure.requests()[0].header("authorization"), None);
    Ok(())
}

#[tokio::test]
async fn an_untrusted_certificate_is_refused_and_says_why() -> TestResult {
    let server = Server::tls(|_| Reply::ok(body(5))).await?;
    let client = client_with(quick(), Chain::new(Vec::new()), &Waits::default())?;
    let error = fetch(&client, &plain(&server.url("/file"))?)
        .await
        .err()
        .ok_or("an untrusted server was downloaded from")?;
    assert!(error.to_string().contains("certificate"), "{error}");
    Ok(())
}

#[tokio::test]
async fn a_cold_download_is_not_a_restart_and_saves_progress_every_64_mib_only() -> TestResult {
    const MIB: usize = 1 << 20;
    let mut bytes = vec![0xab; 64 * MIB + 300_000];
    bytes[..251].copy_from_slice(&body(251));
    let payload = bytes.clone();
    let server = Server::plain(move |_| Reply::ok(payload.clone())).await?;
    let client = client_with(
        patient(Duration::from_secs(5)),
        Chain::new(Vec::new()),
        &Waits::default(),
    )?;
    let done = bounded(fetch(&client, &plain(&server.url("/file"))?)).await??;
    assert_eq!(blake3::hash(&done.sink.bytes), blake3::hash(&bytes));
    assert!(!done.restarted);
    assert!(!done.resumed);
    assert_eq!(done.sink.resets, 1);
    assert_eq!(done.sink.reads, 0);
    let (last, full) = done.sink.writes.split_last().ok_or("nothing was written")?;
    assert!(full.iter().all(|write| *write == 256 * 1024));
    assert_eq!(full.len() * 256 * 1024 + last, bytes.len());
    let record = done.sink.record.ok_or("no record was saved")?;
    assert_eq!(record.received, 64 * MIB as u64);
    Ok(())
}

#[tokio::test]
async fn a_failure_before_any_byte_is_kept_does_not_restart_or_save() -> TestResult {
    for stable in [false, true] {
        let server = serve(vec![
            ResponseTemplate::new(503),
            ResponseTemplate::new(200).set_body_bytes(body(100)),
        ])
        .await;
        let download = Download {
            stable,
            ..plain(&format!("{}/file", server.uri()))?
        };
        let waits = Waits::default();
        let done = bounded(fetch(&client(&waits)?, &download)).await??;
        assert_eq!(done.sink.bytes, body(100));
        assert!(!done.restarted, "{stable}");
        assert_eq!(done.sink.resets, 1, "{stable}");
        assert!(done.sink.record.is_none(), "{stable}");
        assert_eq!(waits.taken().len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn a_partial_answer_nobody_asked_for_is_a_status() -> TestResult {
    let server = Server::plain(|_| {
        Reply {
            status: 206,
            ..Reply::ok(body(10))
        }
        .header("content-range", "bytes 0-9/10")
    })
    .await?;
    let client = client(&Waits::default())?;
    let error = bounded(fetch(&client, &plain(&server.url("/file"))?))
        .await?
        .err()
        .ok_or("a partial answer was taken")?;
    assert_eq!(error.kind(), ErrorKind::NetworkStatus);
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn a_refused_credential_is_told_from_a_missing_one() -> TestResult {
    let server = Server::tls(|_| Reply::status(401)).await?;
    let client = client_with(
        trusting(&[&server]),
        Chain::new(vec![Box::new(tokens(&[&server]))]),
        &Waits::default(),
    )?;
    let error = bounded(fetch(&client, &plain(&server.url("/file"))?))
        .await?
        .err()
        .ok_or("a refused credential downloaded")?;
    assert_eq!(error.kind(), ErrorKind::AuthRejected);
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}
