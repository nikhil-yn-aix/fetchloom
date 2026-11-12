//! answers recorded from real services, replayed in tests in place of the network.

use std::collections::HashMap;
use std::future::{Future, ready};
use std::sync::Mutex;

use url::Url;

use crate::error::Error;
use crate::net::{Api, Response};

/// a network that answers only what a test recorded, and remembers every request.
#[derive(Default)]
pub(crate) struct Recorded {
    answers: HashMap<String, Response>,
    asked: Mutex<Vec<String>>,
}

impl Recorded {
    /// answers `url` with `status` and `body`.
    pub(crate) fn on(mut self, url: &str, status: u16, body: impl Into<Vec<u8>>) -> Recorded {
        let parsed = Url::parse(url).unwrap();
        let response = Response {
            status,
            url: parsed.clone(),
            next: None,
            total: None,
            region: None,
            keyed: None,
            authorized: None,
            body: body.into(),
        };
        self.answers.insert(parsed.to_string(), response);
        self
    }

    /// says the answer for `url` named the region its S3 bucket lives in.
    pub(crate) fn region(mut self, url: &str, region: &str) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before its region"));
        answer.region = Some(region.to_owned());
        self
    }

    /// says how the request for `url` went out to its S3 bucket, signed or not.
    pub(crate) fn keyed(mut self, url: &str, keyed: crate::source::Keyed) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before how it went out"));
        answer.keyed = Some(keyed);
        self
    }

    /// says how the request for `url` went out to its cloud storage bucket, with a token or not.
    pub(crate) fn authorized(mut self, url: &str, authorized: crate::net::Authorized) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before how it went out"));
        answer.authorized = Some(authorized);
        self
    }

    /// answers `url` with a file from `tests/fixtures/sources`.
    pub(crate) fn fixture(self, url: &str, fixture: &str) -> Recorded {
        self.on(url, 200, fixture_bytes(fixture))
    }

    /// says the answer for `url` has a next page at `next`, as a `Link` header would.
    pub(crate) fn next(mut self, url: &str, next: &str) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before its next page"));
        answer.next = Some(Url::parse(next).unwrap());
        self
    }

    /// says the answer for `url` counts `total` items in all, as a `Content-Range` header would.
    pub(crate) fn total(mut self, url: &str, total: u64) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before its total"));
        answer.total = Some(total);
        self
    }

    /// says where the answer came from after redirects.
    pub(crate) fn redirected(mut self, url: &str, final_url: &str) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before its redirect"));
        answer.url = Url::parse(final_url).unwrap();
        self
    }

    /// every URL asked for, in order.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl Api for Recorded {
    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send {
        self.asked.lock().unwrap().push(url.to_string());
        let answer = self
            .answers
            .get(url.as_str())
            .cloned()
            .unwrap_or_else(|| panic!("nothing recorded for {url}"));
        ready(Ok(answer))
    }
}

/// the bytes of a file under `tests/fixtures/sources`.
pub(crate) fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = crate::test_paths::workspace()
        .join("tests")
        .join("fixtures")
        .join("sources")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

pub(crate) fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
