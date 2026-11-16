//! one ask of a cloud's metadata service at 169.254.169.254, through a client of its own with no
//! proxy, redirect, cookie or other credential, given up after a wait it is given, so a machine
//! off that cloud, where nothing answers there, waits no longer.

use std::time::{Duration, Instant, SystemTime};

use reqwest::header::HeaderValue;
use url::Url;

use super::Net;
use super::failure::{self, Failure};
use crate::error::{Error, Kind};
use crate::work::Work;

/// how long a metadata service may take to take a connection: it is on this machine's own
/// network.
pub(super) const CONNECT: Duration = Duration::from_secs(1);

/// how long the first ask of a run may take, connection and answer: a cloud's service answers
/// at once, so a machine off that cloud gives up as soon.
pub(super) const FIRST: Duration = Duration::from_secs(1);

/// why a metadata service was not heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unheard {
    /// the connection was refused, or broke before an answer.
    Refused,
    /// nothing answered within this wait, connection and answer.
    Waited(Duration),
}

impl Net {
    /// one GET of `url`, with `header` when one is given: its answer, or why none came within
    /// `wait`.
    ///
    /// # Errors
    ///
    /// `network.unreachable` when no client starts, and `interrupted` once the run is stopped.
    pub(super) async fn ask_metadata(
        &self,
        url: &Url,
        header: Option<(&'static str, &'static str)>,
        wait: Duration,
    ) -> Result<Result<reqwest::Response, Unheard>, Failure> {
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT.min(wait))
            .build()
            .map_err(|err| {
                Failure::Final(Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not start the network client: {err}"),
                ))
            })?;
        let ticket = self.pace(url).await.map_err(Failure::Final)?;
        Work::add(&self.work.requests, 1);
        let mut request = client.get(url.clone());
        if let Some((name, value)) = header {
            request = request.header(name, HeaderValue::from_static(value));
        }
        let response = tokio::select! {
            sent = tokio::time::timeout(wait, request.send()) => match sent {
                Ok(Ok(response)) => response,
                Ok(Err(err)) if !err.is_timeout() => return Ok(Err(Unheard::Refused)),
                _ => return Ok(Err(Unheard::Waited(wait))),
            },
            () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
        };
        let limits = super::pace::limits(response.status(), response.headers(), SystemTime::now());
        super::pace::Paces::heard(ticket, limits, Instant::now());
        Ok(Ok(response))
    }
}

#[cfg(test)]
pub(super) mod tests;
