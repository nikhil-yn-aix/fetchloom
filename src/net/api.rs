//! small answers read whole, such as the listings and metadata that sources ask for.

use std::borrow::Cow;
use std::future::Future;

use bytes::BytesMut;
use reqwest::header;
use serde::de::DeserializeOwned;
use url::Url;

use super::failure::{self, Failure, Retry};
use super::{IDLE, Net, host};
use crate::error::{Error, Kind};
use crate::work::Work;

/// the largest answer read whole, so a hostile server cannot fill the memory.
const LARGEST: usize = 64 * 1024 * 1024;

/// the network as a source sees it: one GET at a time. the network layer retries while a server
/// says to try again, and every other answer comes back for the source to read.
pub(crate) trait Api: Sync {
    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send;
}

/// what a server answered, with the few headers sources read.
#[derive(Clone, Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    /// where the answer came from, after redirects.
    pub(crate) url: Url,
    /// the `rel="next"` target of a `Link` header, for listings sent in pages.
    pub(crate) next: Option<Url>,
    pub(crate) body: Vec<u8>,
}

impl Response {
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, Error> {
        serde_json::from_slice(&self.body).map_err(|err| {
            Error::new(Kind::SourceInvalid, format!("{} sent an answer fetchloom could not read: {err}", host(&self.url)))
                .detail(format!("from {}", super::Shown(&self.url)))
                .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
        })
    }

    pub(crate) fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

impl Api for Net {
    async fn get(&self, url: &Url) -> Result<Response, Error> {
        if self.offline.enabled() {
            return Err(Error::new(
                Kind::OfflineMissing,
                format!("offline, and this needs an answer from {}", host(url)),
            )
            .help(self.offline.retry()));
        }
        self.typed
            .check(url)
            .map_err(|reason| failure::refused(url, &reason))?;
        let _permits = self.permit(url).await?;
        let mut retry = Retry::new(self, url);
        loop {
            match self.answer(url).await {
                Ok(response) => return Ok(response),
                Err(Failure::Final(err)) => return Err(err),
                Err(Failure::Limited { reason }) => retry.limited(reason, false).await?,
                Err(Failure::Transient {
                    reason,
                    retry_after,
                }) => retry.wait(reason, retry_after, false).await?,
            }
        }
    }
}

impl Net {
    async fn answer(&self, url: &Url) -> Result<Response, Failure> {
        Work::add(&self.work.requests, 1);
        let mut response = self
            .send(url, |request| {
                request.header(
                    header::ACCEPT,
                    "application/json, text/html;q=0.9, */*;q=0.8",
                )
            })
            .await?;
        let status = response.status();
        tracing::debug!(url = %super::Shown(url), status = status.as_u16(), "answer");
        if failure::is_transient(status) {
            return Err(failure::refusal(url, status, response.headers()));
        }
        let text = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value: &header::HeaderValue| value.to_str().ok())
                .map(str::to_owned)
        };
        let next = text(header::LINK)
            .and_then(|value| next_link(&value))
            .and_then(|link| response.url().join(&link).ok());
        let final_url = response.url().clone();
        let mut body = BytesMut::new();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(IDLE, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
            };
            match chunk {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => break,
                Ok(Ok(Some(chunk))) => {
                    Work::add(&self.work.bytes_down, chunk.len() as u64);
                    if body.len() + chunk.len() > LARGEST {
                        return Err(Failure::Final(Error::new(
                            Kind::SourceInvalid,
                            format!(
                                "{} sent more than 64 MiB where a short answer was expected",
                                host(url)
                            ),
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
        Ok(Response {
            status: status.as_u16(),
            url: final_url,
            next,
            body: body.to_vec(),
        })
    }
}

/// the target of the `rel="next"` link in a `Link` header.
fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|link| {
        let (target, params) = link.trim().split_once(';')?;
        let is_next = params.split(';').any(|param| {
            let param = param.trim().to_ascii_lowercase();
            param == "rel=\"next\"" || param == "rel=next"
        });
        is_next.then(|| {
            target
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::next_link;

    #[test]
    fn the_next_page_comes_from_the_link_header() {
        let value = r#"<https://huggingface.co/api/datasets/x/tree/main?cursor=abc>; rel="next", <https://x/first>; rel="first""#;
        assert_eq!(
            next_link(value).as_deref(),
            Some("https://huggingface.co/api/datasets/x/tree/main?cursor=abc")
        );
        assert_eq!(next_link(r#"<https://x/first>; rel="first""#), None);
        assert_eq!(
            next_link("<https://x/2>; rel=next").as_deref(),
            Some("https://x/2")
        );
    }
}
