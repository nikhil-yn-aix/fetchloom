//! Documents read whole: status, headers, redirects kept or followed, retries, and the trust
//! of a configured host held to its port.

use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fl_model::error::ErrorKind;
use fl_transfer::credentials::Chain;
use fl_transfer::fetch::{LIMIT, Request};
use fl_transfer::url::SafeUrl;
use tracing_subscriber::fmt::format::FmtSpan;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::memory::{Waits, client, client_with, patient, tokens, trusting};
use crate::support::{Captured, Reply, Server, bounded, chunked};

type TestResult = Result<(), Box<dyn Error>>;

fn request(url: &SafeUrl, follow: bool) -> Request<'_> {
    Request {
        url,
        follow,
        typed: true,
    }
}

#[tokio::test]
async fn a_document_keeps_its_headers_and_final_location() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", "/new"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(
            ResponseTemplate::new(200)
                .append_header("link", "<a>; rel=\"prev\"")
                .append_header("link", "<b>; rel=\"next\"")
                .set_body_string("{}"),
        )
        .mount(&server)
        .await;
    let client = client(&Waits::default())?;
    let url = SafeUrl::parse(&format!("{}/old", server.uri()))?;
    let page = client.get(&request(&url, true)).await?;
    assert_eq!(page.status, 200);
    assert_eq!(page.url.to_string(), format!("{}/new", server.uri()));
    assert_eq!(page.headers("link").collect::<Vec<_>>().len(), 2);
    assert_eq!(page.body, b"{}");
    let counted = client.counters().read();
    assert_eq!((counted.requests, counted.bytes_down), (2, 2));
    Ok(())
}

#[tokio::test]
async fn a_redirect_not_followed_is_answered_with_its_location() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/10.1/x"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://landing.example/r/1"),
        )
        .mount(&server)
        .await;
    let client = client(&Waits::default())?;
    let url = SafeUrl::parse(&format!("{}/10.1/x", server.uri()))?;
    let page = client.get(&request(&url, false)).await?;
    assert_eq!(page.status, 302);
    assert_eq!(page.header("location"), Some("https://landing.example/r/1"));
    assert_eq!(client.counters().read().requests, 1);
    Ok(())
}

#[tokio::test]
async fn a_rate_limited_document_is_asked_for_again_and_a_missing_one_is_a_status() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/busy"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/busy"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let waits = Waits::default();
    let client = client(&waits)?;
    let busy = SafeUrl::parse(&format!("{}/busy", server.uri()))?;
    assert_eq!(client.get(&request(&busy, true)).await?.body, b"ok");
    assert_eq!(waits.taken().len(), 1);
    assert!(waits.taken()[0].as_secs() >= 2);
    let missing = SafeUrl::parse(&format!("{}/gone", server.uri()))?;
    let error = client
        .get(&request(&missing, true))
        .await
        .err()
        .ok_or("fetched")?;
    assert_eq!(error.kind(), ErrorKind::NetworkStatus);
    assert_eq!(client.counters().read().requests, 3);
    Ok(())
}

#[tokio::test]
async fn a_trusted_server_is_trusted_on_its_own_port_only() -> TestResult {
    let trusted = MockServer::start().await;
    let other = MockServer::start().await;
    for server in [&trusted, &other] {
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x"))
            .mount(server)
            .await;
    }
    let client = client(&Waits::default())?;
    let home = SafeUrl::parse(&format!("{}/record", trusted.uri()))?;
    client.trust(&home.origin().ok_or("no origin")?);
    let untyped = |url| Request {
        url,
        follow: true,
        typed: false,
    };
    assert_eq!(client.get(&untyped(&home)).await?.body, b"x");
    let elsewhere = SafeUrl::parse(&format!("{}/file", other.uri()))?;
    let error = client
        .get(&untyped(&elsewhere))
        .await
        .err()
        .ok_or("reached another port")?;
    assert_eq!(error.kind(), ErrorKind::NetworkRefused);
    assert!(
        other
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_status_that_is_not_a_success_is_refused_with_or_without_following() -> TestResult {
    for follow in [true, false] {
        let server = Server::plain(|_| Reply::status(404)).await?;
        let client = client(&Waits::default())?;
        let url = SafeUrl::parse(&server.url("/gone"))?;
        let error = bounded(client.get(&request(&url, follow)))
            .await?
            .err()
            .ok_or("fetched")?;
        assert_eq!(error.kind(), ErrorKind::NetworkStatus, "{follow}");
        assert_eq!(server.requests().len(), 1, "{follow}");
    }
    Ok(())
}

#[tokio::test]
async fn a_refused_credential_is_told_from_a_missing_one() -> TestResult {
    for status in [401, 403] {
        let server = Server::tls(move |_| Reply::status(status)).await?;
        let url = SafeUrl::parse(&server.url("/doc"))?;
        let without = client_with(
            trusting(&[&server]),
            Chain::new(Vec::new()),
            &Waits::default(),
        )?;
        let error = bounded(without.get(&request(&url, true)))
            .await?
            .err()
            .ok_or("fetched")?;
        assert_eq!(error.kind(), ErrorKind::AuthRequired, "{status}");
        let with = client_with(
            trusting(&[&server]),
            Chain::new(vec![Box::new(tokens(&[&server]))]),
            &Waits::default(),
        )?;
        let error = bounded(with.get(&request(&url, true)))
            .await?
            .err()
            .ok_or("fetched")?;
        assert_eq!(error.kind(), ErrorKind::AuthRejected, "{status}");
        assert_eq!(server.requests().len(), 2, "{status}");
    }
    Ok(())
}

#[tokio::test]
async fn a_busy_server_is_asked_five_times_and_then_the_status_is_the_answer() -> TestResult {
    let asked = Arc::new(AtomicUsize::new(0));
    let tally = Arc::clone(&asked);
    let server = Server::plain(move |_| {
        if tally.fetch_add(1, Ordering::SeqCst) < 5 {
            Reply::status(503)
        } else {
            Reply::ok(b"late".to_vec())
        }
    })
    .await?;
    let client = client(&Waits::default())?;
    let url = SafeUrl::parse(&server.url("/busy"))?;
    let error = bounded(client.get(&request(&url, true)))
        .await?
        .err()
        .ok_or("fetched")?;
    assert_eq!(error.kind(), ErrorKind::NetworkStatus);
    let counted = client.counters().read();
    assert_eq!((counted.requests, counted.retries), (5, 4));
    Ok(())
}

#[tokio::test]
async fn each_attempt_at_a_document_is_numbered_in_the_log() -> TestResult {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_span_events(FmtSpan::CLOSE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .map_err(|error| error.to_string())?;
    let asked = Arc::new(AtomicUsize::new(0));
    let tally = Arc::clone(&asked);
    let server = Server::plain(move |_| {
        if tally.fetch_add(1, Ordering::SeqCst) == 0 {
            Reply::status(503)
        } else {
            Reply::ok(b"ok".to_vec())
        }
    })
    .await?;
    let client = client(&Waits::default())?;
    let url = SafeUrl::parse(&server.url("/doc"))?;
    bounded(client.get(&request(&url, true))).await??;
    let logs = captured.text();
    assert!(logs.contains("attempt=1"), "{logs}");
    assert!(logs.contains("attempt=2"), "{logs}");
    Ok(())
}

fn big_client() -> Result<fl_transfer::client::Client, Box<dyn Error>> {
    client_with(
        patient(Duration::from_secs(5)),
        Chain::new(Vec::new()),
        &Waits::default(),
    )
}

#[tokio::test]
async fn a_document_declared_over_the_limit_is_refused_before_it_is_read() -> TestResult {
    let server = Server::plain(|_| {
        Reply::ok(b"short".to_vec()).header("content-length", &(LIMIT + 1).to_string())
    })
    .await?;
    let client = big_client()?;
    let url = SafeUrl::parse(&server.url("/big"))?;
    let error = bounded(client.get(&request(&url, true)))
        .await?
        .err()
        .ok_or("fetched")?;
    assert_eq!(error.kind(), ErrorKind::NetworkProtocol);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_document_of_exactly_the_limit_is_taken() -> TestResult {
    let bytes = Arc::new(vec![7_u8; LIMIT]);
    let server = Server::plain(move |_| Reply::ok(bytes.to_vec())).await?;
    let client = big_client()?;
    let url = SafeUrl::parse(&server.url("/big"))?;
    let page = bounded(client.get(&request(&url, true))).await??;
    assert_eq!(page.body.len(), LIMIT);
    Ok(())
}

#[tokio::test]
async fn a_document_that_outgrows_the_limit_while_arriving_is_refused() -> TestResult {
    let bytes = Arc::new(vec![7_u8; LIMIT + 1]);
    let server = Server::plain(move |_| {
        Reply::ok(chunked([&bytes[..LIMIT - 1], &bytes[LIMIT - 1..]]))
            .header("transfer-encoding", "chunked")
    })
    .await?;
    let client = big_client()?;
    let url = SafeUrl::parse(&server.url("/big"))?;
    let error = bounded(client.get(&request(&url, true)))
        .await?
        .err()
        .ok_or("fetched")?;
    assert_eq!(error.kind(), ErrorKind::NetworkProtocol);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}
