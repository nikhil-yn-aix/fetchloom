//! Documents read whole: status, headers, redirects kept or followed, retries, and the trust
//! of a configured host held to its port.

use std::error::Error;

use fl_model::error::ErrorKind;
use fl_transfer::fetch::Request;
use fl_transfer::url::SafeUrl;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::memory::{Waits, client};

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
