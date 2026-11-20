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
    /// the most files one dataset may list, when the test sets fewer than a run's least.
    most: Option<usize>,
    /// the errors the network gives in place of an answer, as for one too large to read.
    failures: HashMap<String, Error>,
    /// the status a link nothing was recorded for is answered with, where a test allows one.
    otherwise: Option<u16>,
    /// whether a link nothing was recorded for gets the answer of a recorded link of its path,
    /// as a server that reads no page number sends its first page for every page.
    echo: bool,
    /// the most requests a test lets an adapter make, past which it counts as looping.
    most_asked: Option<usize>,
    /// how many folders a walk may ask for at once, one when unset.
    window: Option<usize>,
    /// how many numbered pages a listing may ask for at once, one when unset.
    pages: Option<usize>,
    /// how long each answer takes on tokio's clock, by link, and for any other link.
    holds: HashMap<String, std::time::Duration>,
    hold: Option<std::time::Duration>,
}

/// what a recorded network panics with when an adapter asks more than its test allows.
const LOOPING: &str = "asked more than its test allows";

impl Clone for Recorded {
    /// the same answers, with nothing asked yet.
    fn clone(&self) -> Recorded {
        Recorded {
            answers: self.answers.clone(),
            asked: Mutex::default(),
            private: self.private,
            most: self.most,
            failures: self.failures.clone(),
            otherwise: self.otherwise,
            echo: self.echo,
            most_asked: self.most_asked,
            window: self.window,
            pages: self.pages,
            holds: self.holds.clone(),
            hold: self.hold,
        }
    }
}

impl Recorded {
    /// lets each dataset list at most `most` files.
    pub(crate) fn most(mut self, most: usize) -> Recorded {
        self.most = Some(most);
        self
    }

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
            signed: None,
            endpoint: None,
            remaining: None,
            body: std::sync::Arc::new(body.into()),
        };
        self.answers.insert(parsed.to_string(), response);
        self
    }

    /// the answer recorded for `url`, which a setter of `what` changes; a test that never
    /// recorded it is wrong.
    fn answer_to(&mut self, url: &str, what: &str) -> &mut Response {
        let key = Url::parse(url).unwrap().to_string();
        self.answers
            .get_mut(&key)
            .unwrap_or_else(|| panic!("record {url} before {what}"))
    }

    /// says the answer for `url` named the region its S3 bucket lives in.
    pub(crate) fn region(mut self, url: &str, region: &str) -> Recorded {
        self.answer_to(url, "its region").region = Some(region.to_owned());
        self
    }

    /// says the answer for `url` left `remaining` requests in its host's rate limit.
    pub(crate) fn remaining(mut self, url: &str, remaining: u64) -> Recorded {
        self.answer_to(url, "its rate limit").remaining = Some(remaining);
        self
    }

    /// says how the request for `url` went out to its S3 bucket, signed or not.
    pub(crate) fn keyed(mut self, url: &str, keyed: crate::net::s3::Keyed) -> Recorded {
        self.answer_to(url, "how it went out").keyed = Some(keyed);
        self
    }

    /// says how the request for `url` went out to its cloud storage bucket, with a token or not.
    pub(crate) fn authorized(mut self, url: &str, authorized: crate::net::Authorized) -> Recorded {
        self.answer_to(url, "how it went out").authorized = Some(authorized);
        self
    }

    /// says how the request for `url` went out to drive's api, with a token or not.
    pub(crate) fn drive(mut self, url: &str, answered: crate::net::DriveAnswered) -> Recorded {
        self.answer_to(url, "how it went out").drive = Some(answered);
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
        self.answer_to(url, "its next page").next = Some(Ok(Url::parse(next).unwrap()));
        self
    }

    /// says the answer for `url` counts `total` items in all, as a `Content-Range` header would.
    pub(crate) fn total(mut self, url: &str, total: u64) -> Recorded {
        self.answer_to(url, "its total").total = Some(total);
        self
    }

    /// says where the answer came from after redirects.
    pub(crate) fn redirected(mut self, url: &str, final_url: &str) -> Recorded {
        self.answer_to(url, "its redirect").url = Url::parse(final_url).unwrap();
        self
    }

    /// every URL asked for, in order.
    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    /// every link an answer was recorded for, in order.
    pub(crate) fn links(&self) -> Vec<String> {
        let mut links: Vec<String> = self.answers.keys().cloned().collect();
        links.sort_unstable();
        links
    }

    /// the body recorded for `url`.
    pub(crate) fn body(&self, url: &str) -> &[u8] {
        &self.answers[url].body
    }

    /// answers `url`, recorded before, with `status` and `body` instead, its headers kept.
    pub(crate) fn answer(mut self, url: &str, status: u16, body: Vec<u8>) -> Recorded {
        let answer = self.answer_to(url, "changing it");
        answer.status = status;
        answer.body = std::sync::Arc::new(body);
        self
    }

    /// gives `error` for `url`, as the network does for an answer it would not read.
    pub(crate) fn fail(mut self, url: &str, error: Error) -> Recorded {
        let key = Url::parse(url).unwrap().to_string();
        self.failures.insert(key, error);
        self
    }

    /// answers a link nothing was recorded for with `status` and no body.
    pub(crate) fn otherwise(mut self, status: u16) -> Recorded {
        self.otherwise = Some(status);
        self
    }

    /// answers a link nothing was recorded for as the first recorded link of its path.
    pub(crate) fn echo(mut self) -> Recorded {
        self.echo = true;
        self
    }

    /// panics with [`LOOPING`] at the request after the `most`th.
    pub(crate) fn at_most(mut self, most: usize) -> Recorded {
        self.most_asked = Some(most);
        self
    }

    /// lets a walk of folders ask for `window` at once.
    pub(crate) fn window(mut self, window: usize) -> Recorded {
        self.window = Some(window);
        self
    }

    /// lets a listing ask for `pages` of its numbered pages at once.
    pub(crate) fn pages(mut self, pages: usize) -> Recorded {
        self.pages = Some(pages);
        self
    }

    /// answers every link `hold` after it is asked, on tokio's clock, which a paused test runtime
    /// moves on at once, so a test counts the requests that waited on each other.
    pub(crate) fn holding(mut self, hold: std::time::Duration) -> Recorded {
        self.hold = Some(hold);
        self
    }

    /// answers `url` `hold` after it is asked, whatever the others take.
    pub(crate) fn holding_at(mut self, url: &str, hold: std::time::Duration) -> Recorded {
        self.holds
            .insert(Url::parse(url).unwrap().to_string(), hold);
        self
    }

    /// how long the answer for `url` takes.
    fn hold_for(&self, url: &Url) -> Option<std::time::Duration> {
        self.holds.get(url.as_str()).copied().or(self.hold)
    }

    /// what the network answers for `url`, asked as `asked`, which is remembered.
    fn reply(&self, url: &Url, asked: String) -> Result<Response, Error> {
        let count = {
            let mut log = self.asked.lock().unwrap();
            log.push(asked);
            log.len()
        };
        assert!(
            self.most_asked.is_none_or(|most| count <= most),
            "{LOOPING}: {count} requests, the last for {url}"
        );
        if let Some(error) = self.failures.get(url.as_str()) {
            return Err(error.clone());
        }
        if let Some(answer) = self.answers.get(url.as_str()) {
            return Ok(answer.clone());
        }
        let path = |link: &str| {
            Url::parse(link)
                .ok()
                .map(|link| link[..url::Position::AfterPath].to_owned())
        };
        let echoed = self.echo.then(|| {
            self.links()
                .into_iter()
                .find(|link| path(link).as_deref() == Some(&url[..url::Position::AfterPath]))
        });
        if let Some(Some(link)) = echoed {
            return Ok(self.answers[&link].clone());
        }
        match self.otherwise {
            Some(status) => {
                let empty = Recorded::default().on(url.as_str(), status, Vec::new());
                Ok(empty.answers[url.as_str()].clone())
            }
            None => panic!("nothing recorded for {url}"),
        }
    }
}

impl Api for Recorded {
    fn most_files(&self) -> usize {
        self.most.unwrap_or(crate::net::MOST_FILES)
    }

    fn walk_window(&self, _url: &Url) -> usize {
        self.window.unwrap_or(1)
    }

    fn pages_window(&self, _url: &Url) -> usize {
        self.pages.unwrap_or(1)
    }

    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send {
        held(self.hold_for(url), self.reply(url, url.to_string()))
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
        held(
            self.hold_for(url),
            self.reply(url, format!("{url} {}", carried.join(" "))),
        )
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

/// `answer`, given once `hold` has passed on tokio's clock, or at once.
async fn held(
    hold: Option<std::time::Duration>,
    answer: Result<Response, Error>,
) -> Result<Response, Error> {
    if let Some(hold) = hold {
        tokio::time::sleep(hold).await;
    }
    answer
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
    credentials: Vec<crate::auth::Credential>,
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
    )
    .with_rules(super::rules());
    (net, work)
}

/// a live service may stall and the network retries, so a test pins the listing's own requests.
pub(crate) fn asked(work: &crate::work::Work) -> u64 {
    let counts = work.counts();
    counts.requests - counts.retries
}
