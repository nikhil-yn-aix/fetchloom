//! this machine's managed identity: a token for azure storage from the instance metadata service
//! azure gives every virtual machine. the service answers plain http at 169.254.169.254, a
//! link-local address the network refuses for every link, so its token is asked through a client
//! of its own, with no proxy, no redirect, no cookie and no other credential, and only once
//! `AZURE_TOKEN_CREDENTIALS` names the identity. no other request is let through to the address.

use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use url::Url;

use super::super::failure::{self, Failure, Retry};
use super::super::{Net, host};
use crate::auth::azure::STORAGE;
use crate::error::{Error, Kind, report_answer};
use crate::work::Work;

/// where every virtual machine's instance metadata service gives its identity's tokens.
const ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
/// the version of the service's API that answers for a token.
const VERSION: &str = "2018-02-01";
/// how long the service may take to open a connection: it is on this machine's own network.
const CONNECT: Duration = Duration::from_secs(5);
/// the largest answer the service may send.
const LARGEST_ANSWER: usize = 64 << 10;
/// how long a token lives when the answer does not say: azure's tokens live an hour or more.
const LIFETIME: Duration = Duration::from_hours(1);

/// what the service answers: a token, or why it gave none.
#[derive(Deserialize)]
struct Answer {
    access_token: Option<String>,
    expires_in: Option<serde_json::Value>,
    token_type: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

impl Net {
    /// a token that reads azure storage, of this machine's managed identity, or of the
    /// user-assigned one `client_id` names, from the service at `endpoint` when it names one, as
    /// a test's stand-in does, and how long it lives. a failure another try may get past is tried
    /// again, as any request's is.
    ///
    /// # Errors
    ///
    /// `offline.missing` offline, `network.refused` in mirror-only mode, `auth.required` when the
    /// machine has no such identity or no service answers, as off azure, and `source.invalid`
    /// for an answer without a token.
    pub(super) async fn identity_token(
        &self,
        client_id: Option<&str>,
        endpoint: Option<&Url>,
    ) -> Result<(String, Duration), Error> {
        let mut url = match endpoint {
            Some(endpoint) => endpoint.clone(),
            None => Url::parse(ENDPOINT).map_err(|err| {
                Error::new(
                    Kind::AuthRequired,
                    format!("could not make the link of azure's metadata service: {err}"),
                )
            })?,
        };
        url.query_pairs_mut()
            .append_pair("api-version", VERSION)
            .append_pair("resource", STORAGE);
        if let Some(client_id) = client_id {
            url.query_pairs_mut().append_pair("client_id", client_id);
        }
        let mut retry = Retry::new(self, &url);
        loop {
            let failure = match self.ask_identity(&url).await {
                Ok((status, body)) => return token(status, &body, client_id),
                Err(failure) => failure,
            };
            match failure {
                Failure::Final(error) => return Err(error),
                Failure::Limited { reason, wait } => retry.limited(reason, wait, false).await?,
                Failure::Transient {
                    reason,
                    retry_after,
                } => retry.wait(reason, retry_after, false).await?,
            }
        }
    }

    /// one GET of the service at `url`, with `metadata: true` as it asks, and the status and
    /// body of its answer, unless it is one another try may get past: 404 and 410, which it
    /// sends while it starts, and the statuses any request retries. a service that does not
    /// answer at all fails at once: off azure, nothing ever will.
    async fn ask_identity(&self, url: &Url) -> Result<(u16, Vec<u8>), Failure> {
        if self.offline.enabled() {
            return Err(Failure::Final(
                Error::new(
                    Kind::OfflineMissing,
                    "offline, and this needs a token of this machine's managed identity",
                )
                .help(self.offline.retry()),
            ));
        }
        self.may_ask(url, false, None)?;
        // the process may already have a provider, which is fine
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .user_agent(concat!("fetchloom/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT)
            .build()
            .map_err(|err| {
                Failure::Final(Error::new(
                    Kind::NetworkUnreachable,
                    format!("could not start the network client: {err}"),
                ))
            })?;
        let ticket = self.pace(url).await.map_err(Failure::Final)?;
        Work::add(&self.work.requests, 1);
        tracing::debug!(
            host = host(url),
            "asking this machine's managed identity for an azure storage token"
        );
        let request = client
            .get(url.clone())
            .header("metadata", HeaderValue::from_static("true"));
        let mut response = tokio::select! {
            sent = tokio::time::timeout(CONNECT + self.idle, request.send()) => match sent {
                Ok(Ok(response)) => response,
                _ => return Err(Failure::Final(unanswered())),
            },
            () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
        };
        let limits =
            super::super::pace::limits(response.status(), response.headers(), SystemTime::now());
        super::super::pace::Paces::heard(ticket, limits, std::time::Instant::now());
        let status = response.status();
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
            || failure::is_transient(status)
        {
            return Err(failure::transient(format!(
                "azure's metadata service answered {}",
                status.as_u16()
            )));
        }
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(self.idle, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
            };
            match chunk {
                Err(_) | Ok(Err(_)) => {
                    return Err(failure::transient(
                        "azure's metadata service broke off its answer".to_owned(),
                    ));
                }
                Ok(Ok(None)) => return Ok((status.as_u16(), body)),
                Ok(Ok(Some(chunk))) => {
                    if body.len() + chunk.len() > LARGEST_ANSWER {
                        return Err(Failure::Final(Error::new(
                            Kind::SourceInvalid,
                            "azure's metadata service sent more than 64 KiB where a token was expected",
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
    }
}

/// the error for a metadata service that does not answer, as off azure.
fn unanswered() -> Error {
    Error::new(
        Kind::AuthRequired,
        "this machine's managed identity could not be asked: no metadata service answered at 169.254.169.254, so this is no azure virtual machine, or something blocks its service",
    )
    .help("on azure, check that nothing blocks 169.254.169.254; elsewhere, unset AZURE_TOKEN_CREDENTIALS and sign in with `az login`")
}

/// the token in the service's answer, of `status` and `body`, and how long it lives.
///
/// # Errors
///
/// `auth.required` with the service's words when it gave no token, as for a machine with no
/// identity or none with `client_id`, and `source.invalid` for an answer without a bearer token.
fn token(status: u16, body: &[u8], client_id: Option<&str>) -> Result<(String, Duration), Error> {
    let answer: Option<Answer> = serde_json::from_slice(body).ok();
    if status != 200 {
        let said = answer
            .and_then(|answer| {
                let code = answer.error?;
                Some(match answer.error_description {
                    Some(description) => format!("{code}: {description}"),
                    None => code,
                })
            })
            .map_or_else(
                || format!("azure's metadata service answered {status}"),
                |said| format!("azure said {}", crate::auth::google::plain(&said)),
            );
        let (message, help) = match client_id {
            Some(client_id) => (
                format!(
                    "this machine has no managed identity with the client id {client_id}, or azure refused it"
                ),
                "set AZURE_CLIENT_ID to the client id of a user-assigned identity this machine has, or unset it for the machine's own",
            ),
            None => (
                "this machine has no managed identity, or azure refused it".to_owned(),
                "give the virtual machine a managed identity, or unset AZURE_TOKEN_CREDENTIALS and sign in with `az login`",
            ),
        };
        return Err(Error::new(Kind::AuthRequired, message)
            .detail(said)
            .help(help));
    }
    let unusable = |what: &str| {
        Error::new(
            Kind::SourceInvalid,
            format!("azure's metadata service sent {what}"),
        )
        .help(report_answer())
    };
    let answer = answer.ok_or_else(|| unusable("an answer fetchloom could not read"))?;
    if answer
        .token_type
        .as_deref()
        .is_some_and(|kind| !kind.eq_ignore_ascii_case("bearer"))
    {
        return Err(unusable("a token of another kind than bearer"));
    }
    // a token goes in a header, so one a header cannot carry is no token
    let value = answer
        .access_token
        .filter(|token| !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_graphic()))
        .ok_or_else(|| unusable("no token a request can carry"))?;
    let seconds = match answer.expires_in {
        Some(serde_json::Value::Number(number)) => number.as_u64(),
        Some(serde_json::Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    };
    Ok((value, seconds.map_or(LIFETIME, Duration::from_secs)))
}
