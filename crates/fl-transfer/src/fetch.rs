//! Small documents read whole into memory: API records, listing pages, index pages. Each is
//! one request, retried like a download, with its status, headers and final location kept.

use reqwest::header::{self, HeaderMap};

use crate::client::{Answer, Ask, Client, Failure};
use crate::error::TransferError;
use crate::retry;
use crate::url::SafeUrl;

/// The largest document read into memory.
pub const LIMIT: usize = 64 << 20;

/// One document to fetch.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// Where it is.
    pub url: &'a SafeUrl,
    /// Follow redirects. When false, a redirect is answered as it came, with its `Location`.
    pub follow: bool,
    /// The user wrote this URL, so its host may be private.
    pub typed: bool,
}

/// A fetched document.
#[derive(Debug)]
pub struct Page {
    /// The status the last hop answered.
    pub status: u16,
    /// Where the document was found, after redirects.
    pub url: SafeUrl,
    /// A credential went with the request that answered.
    pub credentialed: bool,
    headers: HeaderMap,
    /// The body, at most [`LIMIT`] bytes.
    pub body: Vec<u8>,
}

impl Page {
    /// The first value of the header `name`, when it is text.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// Every value of the header `name` that is text, in order.
    pub fn headers<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .get_all(name)
            .into_iter()
            .filter_map(|value| value.to_str().ok())
    }
}

impl Client {
    /// Fetches the document `request` names into memory.
    ///
    /// A success status, or a redirect when `follow` is false, is a page. Statuses that may
    /// pass are retried with backoff like a download.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError::Status`] for any other status, the auth errors for 401 and
    /// 403, [`TransferError::Protocol`] for a body over [`LIMIT`], and whatever the connection
    /// reports.
    pub async fn get(&self, request: &Request<'_>) -> Result<Page, TransferError> {
        let mut failures = 0;
        loop {
            let ask = Ask {
                url: request.url,
                from: 0,
                to: None,
                if_range: None,
                attempt: failures + 1,
                typed: request.typed,
                follow: request.follow,
            };
            let attempt = match self.send(&ask).await {
                Ok(answer) => self.read(answer, request.follow).await,
                Err(failure) => Err(failure),
            };
            match attempt {
                Ok(page) => return Ok(page),
                Err(failure) => {
                    failures += 1;
                    self.pause(failures, failure, request.url).await?;
                }
            }
        }
    }

    async fn read(&self, answer: Answer, follow: bool) -> Result<Page, Failure> {
        let Answer {
            mut response,
            url,
            credentialed,
            ..
        } = answer;
        let status = response.status().as_u16();
        let accepted =
            response.status().is_success() || (!follow && response.status().is_redirection());
        if !accepted {
            return Err(match status {
                401 | 403 if credentialed => {
                    Failure::last(TransferError::AuthRejected { url, status })
                }
                401 | 403 => Failure::last(TransferError::AuthRequired { url, status }),
                _ if retry::transient_status(status) => Failure {
                    retry_after: response
                        .headers()
                        .get(header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| retry::retry_after(value, self.timer.now())),
                    error: TransferError::Status { url, status },
                    transient: true,
                },
                _ => Failure::last(TransferError::Status { url, status }),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > LIMIT as u64)
        {
            return Err(Failure::last(too_large(&url)));
        }
        let mut body = Vec::new();
        loop {
            let idle = self.config.idle;
            let Ok(chunk) = tokio::time::timeout(idle, response.chunk()).await else {
                return Err(Failure::again(TransferError::Timeout {
                    host: url.host().unwrap_or_default().to_owned(),
                    during: "receiving",
                    limit: idle,
                }));
            };
            match chunk {
                Ok(Some(bytes)) => {
                    self.counters.received(bytes.len() as u64);
                    if body.len() + bytes.len() > LIMIT {
                        return Err(Failure::last(too_large(&url)));
                    }
                    body.extend_from_slice(&bytes);
                }
                Ok(None) => break,
                Err(error) => return Err(self.failure(&error, &url, "receiving")),
            }
        }
        Ok(Page {
            status,
            headers: std::mem::take(response.headers_mut()),
            url,
            credentialed,
            body,
        })
    }
}

fn too_large(url: &SafeUrl) -> TransferError {
    TransferError::Protocol {
        url: url.clone(),
        reason: "sent a document larger than 64 MiB".to_owned(),
    }
}
