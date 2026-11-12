//! which requests carry a google access token: those for a cloud storage bucket the
//! `gcs_buckets` setting lists, once cloud storage refused one without a token this run, and only
//! to storage.googleapis.com over https, before any redirect, and those to drive's api that
//! [`drive`] lets through. the token comes from the application default credentials, asked of
//! google's token endpoint when first needed and again shortly before it expires, by one request
//! while the others wait.

mod drive;

use std::collections::HashSet;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap, HeaderValue};
use url::Url;

use super::failure::{self, Failure, Retry};
use super::{Lane, Net, Shown, host};
use crate::auth::google::{self, Credentials, DRIVE_READ, STORAGE_READ, TOKEN_ENDPOINT};
use crate::auth::lease::Lease;
use crate::error::{Error, Kind};
use crate::source::{DriveId, GcsBucket};

use crate::work::Work;
pub(crate) use drive::{Answered as DriveAnswered, Private};

/// the host every cloud storage request with a token goes to.
const STORAGE: &str = "storage.googleapis.com";
/// how to get a token with the scope that reads cloud storage.
const SCOPE_HELP: &str = "a login needs the cloud-platform scope: sign in again with `gcloud auth application-default login`, with no --scopes, or with https://www.googleapis.com/auth/cloud-platform among them";
/// the largest answer the token endpoint may send.
const LARGEST_ANSWER: usize = 64 * 1024;

/// reads the environment for the google settings, as [`crate::auth::env`] does.
pub(crate) type Environment = Arc<dyn Fn(&str) -> Option<OsString> + Send + Sync>;

/// what one run knows about google access: the buckets listed, the credentials, read when first
/// needed, the token, and which buckets refused a request without one.
pub(crate) struct Google {
    listed: Vec<GcsBucket>,
    environment: Environment,
    credentials: OnceLock<Result<Option<Arc<Credentials>>, Error>>,
    /// buckets cloud storage refused without a token this run, asked with one from then on.
    refused: Mutex<HashSet<String>>,
    storage: Lease<Bearer>,
    /// the drive folders and files listed, and what this run learned of them.
    drive: drive::Drive,
    /// in debug builds, a server on this machine that stands in for google's token endpoint.
    #[cfg(debug_assertions)]
    token_endpoint: Option<Url>,
}

/// the cloud storage bucket or drive item a request is for, as its link names it.
pub(crate) struct Located {
    asked: Asked,
    /// whether the request went again with a new token already, after one was refused.
    renewed: AtomicBool,
}

enum Asked {
    Bucket { bucket: String, listed: bool },
    Drive(drive::Asked),
}

/// an access token as a request carries it, and whose it is.
#[derive(Clone, PartialEq)]
pub(crate) struct Bearer {
    value: HeaderValue,
    /// who the token signs in as, as messages name it.
    identity: Arc<str>,
    /// the service account's email, which an owner shares with. none for a person's login.
    account: Option<Arc<str>>,
}

/// how a request for a cloud storage bucket went out, kept on its answer so a refusal can say
/// why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Authorized {
    /// without a token: `gcs_buckets` does not list the bucket, or lists it and no credentials
    /// were found.
    Anonymous { bucket: String, listed: bool },
    /// with a token of `identity`, which cloud storage may have refused for lacking the scope
    /// that reads it.
    Token {
        bucket: String,
        identity: String,
        lacks_scope: bool,
    },
    /// a request for an azure container, as [`super::azure::Sent`] says it went.
    Azure(super::azure::Sent),
}

impl Default for Google {
    fn default() -> Google {
        Google::new(Vec::new(), Arc::new(crate::auth::env))
    }
}

impl Google {
    /// sends tokens for the `listed` buckets, with credentials the settings `environment` names.
    pub(crate) fn new(listed: Vec<GcsBucket>, environment: Environment) -> Google {
        Google {
            listed,
            environment,
            credentials: OnceLock::new(),
            refused: Mutex::default(),
            storage: Lease::default(),
            drive: drive::Drive::default(),
            #[cfg(debug_assertions)]
            token_endpoint: None,
        }
    }

    /// sends tokens to drive's api for the `listed` drive folders and files, and for what google
    /// finds inside the folders.
    pub(crate) fn with_drive(mut self, listed: Vec<DriveId>) -> Google {
        self.drive = drive::Drive::new(listed);
        self
    }

    /// asks the server at `endpoint` for tokens instead of google's token endpoint. only tests
    /// need it.
    #[cfg(debug_assertions)]
    pub(crate) fn with_token_endpoint(mut self, endpoint: Option<Url>) -> Google {
        self.token_endpoint = endpoint;
        self
    }

    /// the bucket `url` is for, when it is a cloud storage request over https to
    /// storage.googleapis.com that carries no secret of its own: an object by its path, or the
    /// JSON API's listing or download of one. else the drive item it is for, when it is a request
    /// to drive's api that [`drive::locate`] takes.
    pub(crate) fn locate(&self, url: &Url) -> Option<Located> {
        if let Some(asked) = drive::locate(url) {
            return Some(Located {
                asked: Asked::Drive(asked),
                renewed: AtomicBool::new(false),
            });
        }
        if url.scheme() != "https"
            || url.host_str() != Some(STORAGE)
            || url.port().is_some()
            || super::secret(url).is_some()
        {
            return None;
        }
        let mut segments = url.path_segments()?;
        let name = match segments.next()? {
            "storage" => api_bucket(segments)?,
            "download" => {
                (segments.next()? == "storage").then_some(())?;
                api_bucket(segments)?
            }
            "upload" | "batch" => return None,
            name => name,
        };
        let bucket = GcsBucket::parse(name).filter(|bucket| bucket.name() == name)?;
        Some(Located {
            asked: Asked::Bucket {
                listed: self.listed.contains(&bucket),
                bucket: name.to_owned(),
            },
            renewed: AtomicBool::new(false),
        })
    }

    /// the token a request for `located` carries: none until cloud storage refused its bucket
    /// without one this run, for a drive item none unless [`drive`] lets it have one, and none
    /// when no credentials were found.
    ///
    /// # Errors
    ///
    /// what reading the credentials or asking for a token found wrong, such as a revoked login.
    pub(crate) async fn bearer(
        &self,
        net: &Net,
        located: &Located,
    ) -> Result<Option<Bearer>, Error> {
        let (lease, scope) = match &located.asked {
            Asked::Bucket { bucket, listed } => {
                if !*listed || !self.refused_buckets().contains(bucket) {
                    return Ok(None);
                }
                (&self.storage, STORAGE_READ)
            }
            Asked::Drive(asked) => {
                if !self.drive.may_carry(asked.id()) {
                    return Ok(None);
                }
                (&self.drive.token, DRIVE_READ)
            }
        };
        let Some(credentials) = self.credentials()? else {
            return Ok(None);
        };
        #[cfg(debug_assertions)]
        let endpoint = self.token_endpoint.as_ref();
        #[cfg(not(debug_assertions))]
        let endpoint = None;
        // boxed, so the future of every request does not carry the token request's retries
        lease
            .get(Instant::now, || {
                Box::pin(net.google_token(&credentials, scope, endpoint))
            })
            .await
            .map(Some)
    }

    /// learns from cloud storage refusing a request for `located` without a token: a listed
    /// bucket is asked with one from now on. says whether credentials were found to ask with.
    ///
    /// # Errors
    ///
    /// what reading the credentials found wrong, such as a file of another kind.
    pub(crate) fn refused(&self, located: &Located) -> Result<bool, Error> {
        let Asked::Bucket {
            bucket,
            listed: true,
        } = &located.asked
        else {
            return Ok(false);
        };
        self.refused_buckets().insert(bucket.clone());
        Ok(self.credentials()?.is_some())
    }

    /// forgets `bearer`, which google refused for `located` as no longer valid, so the next
    /// request asks for a new token.
    pub(crate) async fn forget(&self, located: &Located, bearer: &Bearer) {
        match located.asked {
            Asked::Bucket { .. } => self.storage.forget(bearer).await,
            Asked::Drive(_) => self.drive.token.forget(bearer).await,
        }
    }

    /// the credentials, read once a run.
    fn credentials(&self) -> Result<Option<Arc<Credentials>>, Error> {
        self.credentials
            .get_or_init(|| google::find(&*self.environment).map(|found| found.map(Arc::new)))
            .clone()
    }

    fn refused_buckets(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// the bucket a JSON API path names after `storage/`: `v1/b/<bucket>/...`.
fn api_bucket<'u>(mut segments: impl Iterator<Item = &'u str>) -> Option<&'u str> {
    (segments.next()? == "v1" && segments.next()? == "b").then_some(())?;
    segments.next()
}

impl Located {
    /// tells `response` how its request went out, with `bearer` or not, so a refusal can say
    /// why and a drive listing can teach what its folder holds.
    fn tell(&self, response: &mut reqwest::Response, bearer: Option<&Bearer>) {
        let status = response.status();
        match &self.asked {
            Asked::Bucket { bucket, listed } => {
                let authorized = match bearer {
                    Some(bearer) => Authorized::Token {
                        bucket: bucket.clone(),
                        identity: bearer.identity.to_string(),
                        lacks_scope: status == StatusCode::FORBIDDEN
                            && insufficient_scope(response.headers()),
                    },
                    None => Authorized::Anonymous {
                        bucket: bucket.clone(),
                        listed: *listed,
                    },
                };
                response.extensions_mut().insert(authorized);
            }
            Asked::Drive(asked) => {
                let answered = drive::Answered::new(asked, bearer, response.headers());
                response.extensions_mut().insert(answered);
            }
        }
    }

    /// the bucket a request is for and whether it is listed, when it is for one.
    #[cfg(test)]
    fn bucket(&self) -> Option<(&str, bool)> {
        match &self.asked {
            Asked::Bucket { bucket, listed } => Some((bucket, *listed)),
            Asked::Drive(_) => None,
        }
    }
}

/// one hop of a request for a cloud storage bucket, and the token it carries.
pub(super) struct Tokened<'r> {
    stored: Option<&'r Located>,
    bearer: Option<Bearer>,
}

impl Tokened<'_> {
    /// the headers that carry the token: its `authorization`, which logs never show, or none.
    pub(super) fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(bearer) = &self.bearer {
            headers.insert(header::AUTHORIZATION, bearer.value.clone());
        }
        headers
    }
}

impl Authorized {
    /// the error for a download cloud storage refused, as this says the request went.
    pub(crate) fn refusal(&self, url: &Url) -> Error {
        let shown = Shown(url);
        match self {
            Authorized::Azure(sent) => sent.refusal(url),
            Authorized::Token {
                identity,
                lacks_scope: true,
                ..
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "google cloud storage refused {identity} for {shown}: its token lacks the scope that reads cloud storage"
                ),
            )
            .help(SCOPE_HELP),
            Authorized::Token { identity, .. } => Error::new(
                Kind::AuthRequired,
                format!("google cloud storage refused {identity} for {shown}"),
            )
            .help("ask the bucket's owner to let that account read it"),
            Authorized::Anonymous {
                listed: true, ..
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "google cloud storage refused {shown} without credentials, and no google application default credentials were found"
                ),
            )
            .help(google::SIGN_IN),
            Authorized::Anonymous { bucket, .. } => Error::new(
                Kind::AuthRequired,
                format!("google cloud storage refused {shown} without credentials"),
            )
            .help(trust(bucket)),
        }
    }

    /// the error for a listing of `prefix` cloud storage refused, as this says it went and
    /// cloud storage's `message` says why.
    pub(crate) fn listing_refusal(&self, prefix: &str, message: Option<&str>) -> Error {
        // google words a token without the scope so, besides the header a download reads
        let unscoped = message.is_some_and(|message| {
            message
                .to_ascii_lowercase()
                .contains("insufficient authentication scopes")
        });
        match self {
            Authorized::Azure(sent) => sent.listing_refusal(prefix),
            Authorized::Token {
                identity,
                lacks_scope,
                ..
            } if *lacks_scope || unscoped => Error::new(
                Kind::AuthRequired,
                format!(
                    "google cloud storage refused {identity} for {prefix}: its token lacks the scope that reads cloud storage"
                ),
            )
            .help(SCOPE_HELP),
            Authorized::Token { identity, .. } => Error::new(
                Kind::AuthRequired,
                format!("google cloud storage refused {identity} for {prefix}"),
            )
            .help("ask the bucket's owner to let that account list and read it"),
            Authorized::Anonymous {
                listed: true, ..
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private: google cloud storage refused to list it without credentials, and no google application default credentials were found"
                ),
            )
            .help(google::SIGN_IN),
            Authorized::Anonymous { bucket, .. } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private: google cloud storage refused to list it without credentials"
                ),
            )
            .help(trust(bucket)),
        }
    }
}

/// whether a refusal's `WWW-Authenticate` says the token lacks a scope, as RFC 6750 words it.
fn insufficient_scope(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value
                .to_ascii_lowercase()
                .replace(['"', ' '], "")
                .contains("error=insufficient_scope")
        })
}

/// how to let fetchloom send credentials for `bucket`.
fn trust(bucket: &str) -> String {
    format!(
        "if you trust gs://{bucket} with your google credentials, add it to gcs_buckets in the settings file or FETCHLOOM_GCS_BUCKETS"
    )
}

impl Net {
    /// the hop after `redirects` redirects of a request for `stored`, and the token it carries:
    /// one only before any redirect, as a signature goes.
    pub(super) async fn google_bearer<'r>(
        &self,
        stored: Option<&'r Located>,
        redirects: usize,
    ) -> Result<Tokened<'r>, Failure> {
        let stored = stored.filter(|_| redirects == 0);
        let bearer = match stored {
            Some(stored) => self
                .google
                .bearer(self, stored)
                .await
                .map_err(Failure::Final)?,
            None => None,
        };
        if let Some(stored) = stored.filter(|_| bearer.is_some()) {
            let host = match stored.asked {
                Asked::Bucket { .. } => STORAGE,
                Asked::Drive(_) => drive::API,
            };
            tracing::debug!(host, "sending a google access token");
        }
        Ok(Tokened { stored, bearer })
    }

    /// whether the request goes again after `response` answered `hop`: once refused without a
    /// token for a listed bucket with credentials, and once refused a token with 401, as an
    /// expired or revoked one is, which is then forgotten. otherwise the response is told how
    /// the request went.
    ///
    /// # Errors
    ///
    /// what reading the credentials found wrong.
    pub(super) async fn google_again(
        &self,
        hop: &Tokened<'_>,
        response: &mut reqwest::Response,
    ) -> Result<bool, Failure> {
        let (Some(stored), bearer) = (hop.stored, hop.bearer.as_ref()) else {
            return Ok(false);
        };
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            match bearer {
                None if self.google.refused(stored).map_err(Failure::Final)? => {
                    tracing::debug!(url = %Shown(response.url()), "cloud storage refused the request without a token, so it goes again with one");
                    return Ok(true);
                }
                Some(sent)
                    if status == StatusCode::UNAUTHORIZED
                        && !stored.renewed.swap(true, Ordering::Relaxed) =>
                {
                    tracing::debug!(url = %Shown(response.url()), "google refused the token, so it goes again with a new one");
                    self.google.forget(stored, sent).await;
                    return Ok(true);
                }
                _ => {}
            }
        }
        stored.tell(response, bearer);
        Ok(false)
    }

    /// a token of `scope` for `credentials` from google's token endpoint, and how long it lives:
    /// one POST of the grant, never redirected and carrying no other credential, tried again only
    /// for a failure another try may get past.
    async fn google_token(
        &self,
        credentials: &Credentials,
        scope: &str,
        endpoint: Option<&Url>,
    ) -> Result<(Bearer, Option<Duration>), Error> {
        let google = Url::parse(TOKEN_ENDPOINT).map_err(|err| {
            Error::new(
                Kind::AuthRequired,
                format!("could not make the link of google's token endpoint: {err}"),
            )
        })?;
        let url = match endpoint {
            Some(local) => {
                #[cfg(debug_assertions)]
                self.typed(local);
                local.clone()
            }
            None => google,
        };
        let mut retry = Retry::new(self, &url);
        loop {
            // each try is signed anew, so its time is now
            let grant = credentials.grant(scope, SystemTime::now())?;
            let failure = match self.post_grant(&url, grant).await {
                Ok((status, body)) => {
                    let token = credentials
                        .answer(status, &body)
                        .map_err(|error| drive::relogin(error, credentials, scope))?;
                    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.value))
                        .map_err(|_| {
                            Error::new(
                                Kind::SourceInvalid,
                                "google's token endpoint sent a token no request header can carry",
                            )
                        })?;
                    value.set_sensitive(true);
                    let bearer = Bearer {
                        value,
                        identity: credentials.shown().into(),
                        account: credentials.described().1.map(Arc::from),
                    };
                    return Ok((bearer, Some(token.lifetime)));
                }
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

    /// one POST of `grant` to the token endpoint at `url`, and the status and body of its answer,
    /// unless it is one another try may get past. it follows no redirect and carries no other
    /// credential; in mirror-only mode it is refused, as any request to a source is.
    async fn post_grant(&self, url: &Url, grant: String) -> Result<(u16, Vec<u8>), Failure> {
        if self.offline.enabled() {
            return Err(Failure::Final(
                Error::new(
                    Kind::OfflineMissing,
                    format!("offline, and this needs a google token from {}", host(url)),
                )
                .help(self.offline.retry()),
            ));
        }
        self.may_ask(url, false, None)?;
        self.typed
            .check(url)
            .map_err(|reason| Failure::Final(failure::refused(url, &reason)))?;
        let wire = self.wire().map_err(Failure::Final)?;
        wire.proxies
            .check(url, &self.typed)
            .await
            .map_err(|reason| Failure::Final(failure::refused(url, &reason)))?;
        let carrier = wire.proxies.carrier(url);
        let client = wire.lane(Lane::Shared, url).map_err(Failure::Final)?;
        let ticket = self.pace(url).await.map_err(Failure::Final)?;
        Work::add(&self.work.requests, 1);
        tracing::debug!(host = host(url), "asking for a google access token");
        let request = client
            .post(url.clone())
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::ACCEPT, "application/json")
            .body(grant);
        let mut response = self.sent(wire, url, carrier.as_ref(), request).await?;
        let limits = super::pace::limits(response.status(), response.headers(), SystemTime::now());
        super::pace::Paces::heard(ticket, limits, Instant::now());
        let status = response.status();
        if failure::is_transient(status) {
            return Err(failure::refusal(url, status, response.headers()));
        }
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(self.idle, response.chunk()) => chunk,
                () = self.stop.asked() => return Err(Failure::Final(failure::interrupted(url))),
            };
            match chunk {
                Err(_) => {
                    return Err(failure::transient(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        self.idle.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(failure::transient(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => return Ok((status.as_u16(), body)),
                Ok(Ok(Some(chunk))) => {
                    Work::add(&self.work.bytes_down, chunk.len() as u64);
                    if body.len() + chunk.len() > LARGEST_ANSWER {
                        return Err(Failure::Final(Error::new(
                            Kind::SourceInvalid,
                            format!(
                                "{} sent more than 64 KiB where a token was expected",
                                host(url)
                            ),
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
    }
}

/// the server on this machine that `FETCHLOOM_TEST_GOOGLE_TOKEN` names to stand in for google's
/// token endpoint, read once when the network is made. release builds never read it.
#[cfg(debug_assertions)]
pub(crate) fn test_token_endpoint() -> Option<Url> {
    super::api::local_test_server("FETCHLOOM_TEST_GOOGLE_TOKEN")
}

#[cfg(test)]
mod tests;
