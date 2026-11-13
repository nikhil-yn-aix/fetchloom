//! the networks source tests run against: answers recorded from real services, replayed in
//! place of the network, and the real one the ignored `live_` tests reach.

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
    /// what the network says of a drive item drive refused without a token.
    private: Option<crate::net::Private>,
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
            drive: None,
            body: std::sync::Arc::new(body.into()),
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

    /// says how the request for `url` went out to drive's api, with a token or not.
    pub(crate) fn drive(mut self, url: &str, answered: crate::net::DriveAnswered) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        let answer = self
            .answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before how it went out"));
        answer.drive = Some(answered);
        self
    }

    /// says what the network makes of every drive item drive refused without a token.
    pub(crate) fn private(mut self, private: crate::net::Private) -> Recorded {
        self.private = Some(private);
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

    /// answers as [`Api::get`] does, and remembers the request as its link, then each header
    /// it carried, as `name: value`.
    fn get_with(
        &self,
        url: &Url,
        headers: &'static [(&'static str, &'static str)],
    ) -> impl Future<Output = Result<Response, Error>> + Send {
        let carried: Vec<String> = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        self.asked
            .lock()
            .unwrap()
            .push(format!("{url} {}", carried.join(" ")));
        let answer = self
            .answers
            .get(url.as_str())
            .cloned()
            .unwrap_or_else(|| panic!("nothing recorded for {url}"));
        ready(Ok(answer))
    }

    fn drive_refused<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Result<crate::net::Private, Error>> + Send + 'a {
        self.asked
            .lock()
            .unwrap()
            .push(format!("drive refused {id}"));
        ready(Ok(self.private.unwrap_or(crate::net::Private::Unlisted)))
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

/// the real network for a `live_` test, with counters of its own and the `credentials` it
/// signs in with, none for a service anyone may read.
pub(crate) fn live(
    credentials: Vec<crate::net::Credential>,
) -> (crate::net::Net, std::sync::Arc<crate::work::Work>) {
    let work = std::sync::Arc::new(crate::work::Work::default());
    let options = crate::net::Options {
        offline: crate::net::Offline::No,
        jobs: 4,
        credentials,
        split: crate::net::Splitting::WhenSlow,
    };
    let net = crate::net::Net::new(
        options,
        work.clone(),
        std::sync::Arc::default(),
        std::sync::Arc::new(|_| {}),
    );
    (net, work)
}
