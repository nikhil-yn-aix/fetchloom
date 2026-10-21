//! No secret reaches a log line: every event at trace level, from fetchloom and from the HTTP
//! stack beneath it, is captured and searched for the secrets the downloads carried.

mod support;

use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use fl_transfer::credentials::{Chain, Environment};
use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;
use support::memory::{Memory, Waits, client_with, quick};
use support::{Reply, Server};

type TestResult = Result<(), Box<dyn Error>>;

const SECRET: &str = "SECRET-7f3a";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

fn plain(url: &str) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        url: SafeUrl::parse(url)?,
        length: None,
        stated: Vec::new(),
        stable: false,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn no_secret_reaches_a_log_line_at_trace_level() -> TestResult {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .map_err(|error| error.to_string())?;
    let other = Arc::new(Server::tls(|_| Reply::ok(b"elsewhere".to_vec())).await?);
    let away = other.url(&format!("/signed?X-Amz-Signature={SECRET}&id=1"));
    let home = Server::tls(move |request| {
        if request.target.starts_with("/away") {
            Reply::status(302).header("Location", &away)
        } else if request.target.starts_with("/gone") {
            Reply::status(404)
        } else {
            Reply::ok(b"here".to_vec())
        }
    })
    .await?;
    let variable = format!(
        "FETCHLOOM_TOKEN_{}",
        home.authority().replace(['.', ':'], "_")
    );
    let config = fl_transfer::client::Config {
        trust_only: [&home, other.as_ref()]
            .iter()
            .filter_map(|server| server.ca_der())
            .collect(),
        ..quick()
    };
    let client = client_with(
        config,
        Chain::new(vec![Box::new(Environment::new(move |name| {
            (name == variable).then(|| SECRET.to_owned())
        }))]),
        &Waits::default(),
    )?;
    let with_password = home
        .url("/file")
        .replace("https://", &format!("https://me:{SECRET}@"));
    for url in [
        home.url("/file"),
        with_password,
        home.url(&format!("/file?token={SECRET}")),
        home.url("/away"),
    ] {
        client.download(&plain(&url)?, Memory::default()).await?;
    }
    let error = client
        .download(
            &plain(&home.url(&format!("/gone?token={SECRET}")))?,
            Memory::default(),
        )
        .await
        .err()
        .ok_or("a missing file downloaded")?;
    assert!(!error.to_string().contains(SECRET), "{error}");
    assert_eq!(
        home.requests()[0].header("authorization"),
        Some(format!("Bearer {SECRET}").as_str())
    );
    let logs = captured.text();
    assert!(logs.contains("request"), "nothing was captured");
    assert!(logs.contains("TRACE"), "trace events were not captured");
    assert!(logs.contains("attempt=1"), "the attempt was not recorded");
    assert!(!logs.contains(SECRET), "a secret was logged");
    Ok(())
}
