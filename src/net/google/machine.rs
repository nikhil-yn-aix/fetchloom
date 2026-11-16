//! this machine's service account: its email and tokens from google's metadata server at
//! 169.254.169.254, asked through a client of its own once no application default credentials
//! are on the machine and a listed bucket or drive item was refused without a token, on a machine
//! that may be one of google cloud's. no other request is let through to that private address.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::HeaderValue;
use url::Url;

use super::super::failure::{self, Failure, Retry};
use super::super::metadata::{self, Unheard};
use super::super::{Net, host};
use super::Bearer;
use crate::auth::oauth;
use crate::error::{Error, Kind};

/// where a google cloud machine's metadata server answers, by its address as google's go client
/// asks it, so no name lookup can send the request elsewhere.
pub(super) const SERVER: &str = "http://169.254.169.254";
/// the machine's default service account on that server.
const ACCOUNT: &str = "/computeMetadata/v1/instance/service-accounts/default";
/// where linux reads this machine's product name from its firmware, as google's own go client
/// does: google's virtual machines name themselves "Google Compute Engine".
const PRODUCT: &str = "/sys/class/dmi/id/product_name";
const LARGEST_ANSWER: usize = 64 << 10;
/// how long a token lives when the answer does not say: google's live an hour.
const LIFETIME: Duration = Duration::from_hours(1);
/// how to give fetchloom google credentials on a machine without a service account.
const NO_ACCOUNT: &str = "give the virtual machine a service account, run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file";

/// what the metadata server's address answered.
enum Answer {
    /// nothing, or a service that does not say it is google's, as off google cloud.
    Elsewhere(Elsewhere),
    /// google's metadata server, with this status and body.
    Said(u16, Vec<u8>),
}

/// why no google metadata server answered at its address, as off google cloud.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Elsewhere {
    /// nothing answered there.
    Unheard(Unheard),
    /// a service answered without saying it is google's.
    Foreign,
}

/// google's metadata server, when this machine may be one of google cloud's: never a mac, and on
/// linux one whose firmware names google, or that linux cannot tell.
pub(super) fn server() -> Option<Url> {
    let may_be = if cfg!(target_os = "linux") {
        may_be_googles(&std::fs::read_to_string(PRODUCT))
    } else {
        !cfg!(target_os = "macos")
    };
    if may_be {
        Url::parse(SERVER).ok()
    } else {
        None
    }
}

/// whether a linux machine whose product name read as `product` may be one of google's: it is
/// when that names google, and may be when linux cannot tell.
pub(super) fn may_be_googles(product: &std::io::Result<String>) -> bool {
    match product {
        Ok(name) => matches!(name.trim(), "Google" | "Google Compute Engine" | ""),
        Err(_) => true,
    }
}

impl Net {
    /// the email of this machine's service account, as the metadata server at `server` names it,
    /// or why no google metadata server answered there within the first ask's wait.
    ///
    /// # Errors
    ///
    /// `offline.missing` offline, `network.refused` in mirror-only mode, `auth.required` for a
    /// machine without a service account, `source.invalid` for a name that is no email, and
    /// `interrupted` once the run is stopped.
    pub(super) async fn machine_account(
        &self,
        server: &Url,
    ) -> Result<Result<Arc<str>, Elsewhere>, Error> {
        let url = at(server, "email")?;
        let mut retry = Retry::new(self, &url);
        loop {
            match self.ask_machine(&url, metadata::FIRST).await {
                Ok(Answer::Elsewhere(elsewhere)) => return Ok(Err(elsewhere)),
                Ok(Answer::Said(status, body)) => return account(status, &body).map(Ok),
                Err(failure) => retry.after(failure, false).await?,
            }
        }
    }

    /// a token of `scope` for `account`, this machine's service account, from the metadata
    /// server at `server`, as a request carries it, and how long it lives.
    ///
    /// # Errors
    ///
    /// what [`Net::machine_account`] fails with, and `auth.required` when the server gives no
    /// token or stops answering.
    pub(super) async fn machine_bearer(
        &self,
        server: &Url,
        account: &Arc<str>,
        scope: &str,
    ) -> Result<(Bearer, Option<Duration>), Error> {
        let mut url = at(server, "token")?;
        url.query_pairs_mut().append_pair("scopes", scope);
        let mut retry = Retry::new(self, &url);
        let (token, lifetime) = loop {
            match self.ask_machine(&url, metadata::CONNECT + self.idle).await {
                Ok(Answer::Elsewhere(_)) => {
                    return Err(Error::new(
                        Kind::AuthRequired,
                        "google's metadata server stopped answering for this machine's service account",
                    )
                    .help("try again; if it goes on, check that nothing blocks 169.254.169.254"));
                }
                Ok(Answer::Said(status, body)) => break token(status, &body, account)?,
                Err(failure) => retry.after(failure, false).await?,
            }
        };
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            Error::new(
                Kind::SourceInvalid,
                "google's metadata server sent a token no request header can carry",
            )
        })?;
        value.set_sensitive(true);
        let bearer = Bearer {
            value,
            identity: format!("this machine's service account {account}").into(),
            account: Some(account.clone()),
        };
        Ok((bearer, Some(lifetime)))
    }

    /// one GET of `url` with `Metadata-Flavor: Google`, given up after `wait`. no answer, or one
    /// that does not say it is google's, is the answer of a machine elsewhere: nothing would ever
    /// answer there otherwise.
    async fn ask_machine(&self, url: &Url, wait: Duration) -> Result<Answer, Failure> {
        if self.offline.enabled() {
            return Err(Failure::Final(
                Error::new(
                    Kind::OfflineMissing,
                    "offline, and this needs a token of this machine's google service account",
                )
                .help(self.offline.retry()),
            ));
        }
        self.may_ask(url, false, None)?;
        tracing::debug!(
            host = host(url),
            "asking google's metadata server for this machine's service account"
        );
        let mut response = match self
            .ask_metadata(url, Some(("metadata-flavor", "Google")), wait)
            .await?
        {
            Ok(response) => response,
            Err(unheard) => return Ok(Answer::Elsewhere(Elsewhere::Unheard(unheard))),
        };
        // google's server says it is google's, as google's own clients check
        if response
            .headers()
            .get("metadata-flavor")
            .is_none_or(|flavor| flavor != "Google")
        {
            return Ok(Answer::Elsewhere(Elsewhere::Foreign));
        }
        let status = response.status();
        if failure::is_transient(status) {
            return Err(failure::transient(format!(
                "google's metadata server answered {}",
                status.as_u16()
            )));
        }
        let body = self
            .read_body(
                url,
                &mut response,
                "the answer",
                LARGEST_ANSWER,
                || {
                    Error::new(
                        Kind::SourceInvalid,
                        "google's metadata server sent more than 64 KiB where a token was expected",
                    )
                },
                |_| async { Ok(()) },
            )
            .await
            .map_err(|failure| match failure {
                Failure::Transient { .. } => {
                    failure::transient("google's metadata server broke off its answer".to_owned())
                }
                failure => failure,
            })?;
        Ok(Answer::Said(status.as_u16(), body))
    }
}

/// the link of `leaf` in the default service account's folder on the server at `server`.
fn at(server: &Url, leaf: &str) -> Result<Url, Error> {
    server.join(&format!("{ACCOUNT}/{leaf}")).map_err(|err| {
        Error::new(
            Kind::AuthRequired,
            format!("could not make the link of google's metadata server: {err}"),
        )
    })
}

/// the service account's email in the server's answer of `status` and `body`.
///
/// # Errors
///
/// `auth.required` for a machine without a service account, and `source.invalid` for a name
/// that is no email.
fn account(status: u16, body: &[u8]) -> Result<Arc<str>, Error> {
    if status != 200 {
        let message = if status == 404 {
            "this machine has no service account: google's metadata server named none".to_owned()
        } else {
            format!("google's metadata server answered {status} for this machine's service account")
        };
        return Err(Error::new(Kind::AuthRequired, message).help(NO_ACCOUNT));
    }
    std::str::from_utf8(body)
        .ok()
        .map(str::trim)
        .filter(|email| {
            email.len() <= 254 && email.contains('@') && email.bytes().all(|b| b.is_ascii_graphic())
        })
        .map(Arc::from)
        .ok_or_else(|| {
            Error::new(
                Kind::SourceInvalid,
                "google's metadata server named a service account that is no email",
            )
        })
}

/// the token in the server's answer of `status` and `body` for `account`, and how long it lives.
///
/// # Errors
///
/// `auth.required` with what the server said when it gave no token, and `source.invalid` for an
/// answer without a bearer token.
fn token(status: u16, body: &[u8], account: &str) -> Result<(String, Duration), Error> {
    if status != 200 {
        return Err(Error::new(
            Kind::AuthRequired,
            format!(
                "google's metadata server gave no token for this machine's service account {account}"
            ),
        )
        .detail(oauth::said("google's metadata server", "google", status, body))
        .help("check the access scopes the virtual machine gives its service account, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file"));
    }
    let (value, seconds) = oauth::bearer("google's metadata server", body)?;
    Ok((value, seconds.map_or(LIFETIME, Duration::from_secs)))
}
