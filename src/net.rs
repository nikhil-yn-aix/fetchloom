//! the network: one HTTP client, polite retries, resumable downloads, and rules about where a
//! request may go.

mod address;
mod disposition;
mod download;

use std::sync::Arc;
use std::time::Duration;

use url::Url;

pub(crate) use download::{ATTEMPTS, Event, Fetched, Origin};

use crate::error::{Error, Kind};
use crate::stop::Stop;
use crate::work::Work;

const CONNECT: Duration = Duration::from_secs(45);
const RESPONSE: Duration = Duration::from_secs(30);
const IDLE: Duration = Duration::from_secs(30);
const REDIRECTS: usize = 10;

pub(crate) struct Net {
    client: reqwest::Client,
    offline: bool,
    typed: address::Typed,
    work: Arc<Work>,
    stop: Arc<Stop>,
}

impl Net {
    pub(crate) fn new(offline: bool, work: Arc<Work>, stop: Arc<Stop>) -> Result<Net, Error> {
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let typed = address::Typed::default();
        let for_redirects = typed.clone();
        let client = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT)
            .dns_resolver(Arc::new(address::Resolver::new(typed.clone(), offline)))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= REDIRECTS {
                    attempt.error(address::Refused(format!("more than {REDIRECTS} redirects")))
                } else if let Err(refused) = for_redirects.check(attempt.url()) {
                    attempt.error(refused)
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|err| {
                Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not start the network client: {err}"),
                )
            })?;
        Ok(Net {
            client,
            offline,
            typed,
            work,
            stop,
        })
    }

    /// marks the host of a URL the user typed, so it may be a private address such as a lab server.
    pub(crate) fn typed(&self, url: &Url) {
        self.typed.add(url);
    }
}

/// the host of a URL as people say it, for messages.
pub(crate) fn host(url: &Url) -> &str {
    url.host_str().unwrap_or("the server")
}
