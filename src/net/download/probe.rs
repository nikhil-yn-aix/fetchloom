//! whether a link still serves a pinned file: one request for its first byte through a download's
//! own path, its sign-in, signing, slots, pacing, retries, redirects and the pages a source sends
//! first, reading none of the file but that byte.

use reqwest::StatusCode;
use reqwest::header;
use url::Url;

use super::{Fetch, Instead, unreadable};
use crate::error::{Error, Kind};
use crate::net::failure::{self, Failure, Retry};
use crate::net::{Holding, Lane, Net, Permits, Relink, headers};
use crate::shown::Shown;
use crate::work::Work;

/// what a link answered to a request for a file's first byte.
#[derive(Debug)]
pub(crate) enum Probed {
    /// the file or its first byte: the whole file's size when the answer states it, whether the
    /// answer is a web page that names no file, and the name the server gives the file, which a
    /// download of the link takes.
    File {
        total: Option<u64>,
        page: bool,
        name: Option<String>,
    },
    /// an answer that is no file, with the error a download would end with: a refusal with its
    /// sign-in hint, nothing at the link, or another status.
    Answered { status: u16, error: Error },
    /// no answer after the retries, or a request refused before it went out.
    Unanswered(Error),
}

/// where one try got to.
enum Step {
    Probed(Probed),
    /// a page pointed to the file's link.
    Divert(Url),
    /// the link a source gives only for a while expired.
    Relink(Box<dyn Relink>),
}

impl Net {
    /// asks the link of `fetch` for the file's first byte, as a download asks for the file. it
    /// reads at most the byte it asked for, and an answer of the whole file not at all.
    ///
    /// # Errors
    ///
    /// only a stop, `Interrupted`; every other end is a [`Probed`].
    pub(crate) async fn probe(&self, fetch: &Fetch) -> Result<Probed, Error> {
        let span = tracing::debug_span!("probe", url = %Shown(&fetch.url));
        let probed = tracing::Instrument::instrument(self.first_byte(fetch), span).await?;
        let refused = matches!(&probed, Probed::Answered { error, .. } if unreadable(error));
        // a pinned S3 version may be gone, or kept from a reader without a key, while the object
        // there now is the pinned file, which a download then takes once its hash says so
        let current = fetch
            .expect
            .sha256
            .and_then(|_| crate::net::s3::unversioned(&fetch.from))
            .filter(|_| refused);
        let Some((from, _)) = current else {
            return Ok(probed);
        };
        let now = Fetch {
            from,
            ..fetch.clone()
        };
        match self.first_byte(&now).await? {
            served @ Probed::File { .. } => Ok(served),
            _ => Ok(probed),
        }
    }

    async fn first_byte(&self, fetch: &Fetch) -> Result<Probed, Error> {
        if self.offline.enabled() {
            return Ok(Probed::Unanswered(failure::offline(
                &fetch.url,
                self.offline,
                true,
            )));
        }
        let relinked;
        let fetch = match (self.rules.relink)(&fetch.from) {
            Some(relink) => match self.relinked(&*relink, None).await {
                Ok(from) => {
                    relinked = Fetch {
                        from,
                        ..fetch.clone()
                    };
                    &relinked
                }
                Err(error) => return unanswered(error),
            },
            None => fetch,
        };
        if let Err(reason) = self.typed.check(&fetch.from) {
            return Ok(Probed::Unanswered(failure::refused(&fetch.from, &reason)));
        }
        let mut from = fetch.from.clone();
        let (mut diversions, mut relinks) = (0, 0);
        let mut permits = Some(self.permit(&fetch.from).await?);
        let mut retry = Retry::named(self, fetch);
        loop {
            self.keep_permit(&fetch.from, &mut permits).await?;
            let failure = match self.ask_first_byte(&from, fetch, &mut permits).await {
                Ok(Step::Probed(probed)) => return Ok(probed),
                Ok(Step::Divert(next)) => match self.divert(fetch, next, &mut diversions) {
                    Ok(next) => {
                        from = next;
                        continue;
                    }
                    Err(error) => return unanswered(error),
                },
                Ok(Step::Relink(relink)) => {
                    Self::release_for_wait(&mut permits);
                    match self.refreshed(&*relink, &from, &mut relinks).await {
                        Ok(fresh) => {
                            from = fresh;
                            continue;
                        }
                        Err(error) => return unanswered(error),
                    }
                }
                Err(failure) => failure,
            };
            Self::release_for_wait(&mut permits);
            if let Err(error) = retry.after(failure, false).await {
                return unanswered(error);
            }
        }
    }

    /// one try: the answer to `Range: bytes=0-0` at `from`, past the pages and refusals a
    /// download's own checks read. a failure is one the request ended with before any answer, or
    /// one worth another try.
    async fn ask_first_byte(
        &self,
        from: &Url,
        fetch: &Fetch,
        permits: &mut Option<Permits>,
    ) -> Result<Step, Failure> {
        let holding = Holding::new(&fetch.from, permits);
        // the shared client keeps its connections, so many files of one host take a few
        let response = self
            .send_on(
                Lane::Shared,
                from,
                fetch.mirror,
                |request| request.header(header::RANGE, "bytes=0-0"),
                Some(holding),
            )
            .await?;
        let status = response.status();
        let head = response.headers().clone();
        tracing::debug!(url = %Shown(from), status = status.as_u16(), "first byte");
        let response = match self.instead(from, fetch, response).await {
            Ok(Ok(response)) => response,
            Ok(Err(Instead::Divert(next))) => return Ok(Step::Divert(next)),
            Ok(Err(Instead::Relink(relink))) => return Ok(Step::Relink(relink)),
            Err(Failure::Final(error)) => {
                return Ok(Step::Probed(Probed::Answered {
                    status: status.as_u16(),
                    error,
                }));
            }
            Err(failure) => return Err(failure),
        };
        // an empty file has no first byte, and the answer still states its size
        let empty = (status == StatusCode::RANGE_NOT_SATISFIABLE)
            .then(|| headers::total_length(&head, StatusCode::PARTIAL_CONTENT))
            .flatten();
        let served = |response: &reqwest::Response| super::served_name(response, &fetch.url);
        match status {
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => {
                let total = headers::total_length(&head, status);
                let page = headers::is_html(&head) && headers::file_name(&head).is_none();
                let name = match served(&response) {
                    Ok(name) => name,
                    Err(error) => {
                        return Ok(Step::Probed(Probed::Answered {
                            status: status.as_u16(),
                            error,
                        }));
                    }
                };
                if status == StatusCode::PARTIAL_CONTENT {
                    self.read_byte(from, response).await?;
                }
                Ok(Step::Probed(Probed::File { total, page, name }))
            }
            _ if empty.is_some() => Ok(Step::Probed(Probed::File {
                total: empty,
                page: false,
                name: served(&response).ok().flatten(),
            })),
            status => match self.status_refusal(from, status, &head) {
                Failure::Final(error) => Ok(Step::Probed(Probed::Answered {
                    status: status.as_u16(),
                    error,
                })),
                failure => Err(failure),
            },
        }
    }

    /// reads the byte a part brings, so its connection can carry the next request, as one left
    /// with unread bytes cannot; a part that holds more is left unread.
    async fn read_byte(&self, from: &Url, mut response: reqwest::Response) -> Result<(), Failure> {
        if response.content_length().is_none_or(|length| length > 1) {
            return Ok(());
        }
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(self.idle, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(from))),
            };
            match chunk {
                Ok(Ok(Some(chunk))) => Work::add(&self.work.bytes_down, chunk.len() as u64),
                // the answer was heard whole, and a part that ends early changes none of it
                _ => return Ok(()),
            }
        }
    }
}

/// `error`, which ended a probe before any answer, unless it is a stop, which ends the run.
fn unanswered(error: Error) -> Result<Probed, Error> {
    if error.kind == Kind::Interrupted {
        Err(error)
    } else {
        Ok(Probed::Unanswered(error))
    }
}

#[cfg(test)]
mod tests;
