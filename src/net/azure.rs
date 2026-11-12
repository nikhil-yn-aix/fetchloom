//! which requests to azure's blob hosts carry a credential: those for a container the
//! `azure_containers` setting lists, once azure refused one without a credential this run, and
//! only to the container's own blob host over https, before any redirect. the credential is the
//! SAS the container's entry gives, else what the environment gives as the az cli reads it: the
//! SAS in `AZURE_STORAGE_SAS_TOKEN`, or the key in `AZURE_STORAGE_KEY`, kept to the account
//! `AZURE_STORAGE_ACCOUNT` names, else a token of the az cli's login, as [`crate::auth::azure`]
//! finds them. a key signs the request, as [`shared_key`] does, and is never sent; a token is
//! asked for when first needed and again shortly before it expires, by one request while the
//! others wait. every first request to a blob host asks for the version of azure's API that
//! listings are read in.

mod shared_key;

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::failure::Failure;
use super::google::Environment;
use super::{Authorized, Net, Shown, host};
use crate::auth::azure::{AccountKey, SignIn, Storage};
use crate::auth::lease::Lease;
use crate::error::{Error, Kind};
use crate::source::{AZURE_VERSION, AzureContainer, Sas, azure_account, is_azure_container};
use crate::stop::Stop;

/// what one run knows about azure access: the containers listed, what the environment gives,
/// read when first needed, and which containers refused a request without a credential.
pub(crate) struct Azure {
    listed: Vec<AzureContainer>,
    environment: Environment,
    storage: OnceLock<Result<Storage, Error>>,
    /// containers azure refused without a credential this run, as `account/container`, asked
    /// with one from then on.
    refused: Mutex<HashSet<String>>,
    /// the sign-in's token, which reads every listed container its identity may read.
    token: Lease<Bearer>,
}

/// who a token signs in as, as messages name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignedIn {
    /// the az cli's login.
    Cli,
}

impl SignedIn {
    fn phrase(self) -> &'static str {
        match self {
            SignedIn::Cli => "the az cli's login",
        }
    }
}

/// a token as a request carries it, in `authorization`, which logs never show.
#[derive(Clone, PartialEq)]
struct Bearer {
    value: HeaderValue,
    by: SignedIn,
}

/// where a SAS was given, as messages name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Given {
    /// the container's own entry in `azure_containers`.
    Setting,
    /// `AZURE_STORAGE_SAS_TOKEN`.
    Environment,
}

impl Given {
    fn phrase(self) -> &'static str {
        match self {
            Given::Setting => "from azure_containers",
            Given::Environment => "in AZURE_STORAGE_SAS_TOKEN",
        }
    }
}

/// what a request for a listed container carries.
#[derive(Clone)]
enum Credential {
    /// a SAS after the link's query, from where `Given` says.
    Sas(Sas, Given),
    /// a key of the container's account, which signs the request.
    Key(AccountKey),
    /// a token of the sign-in.
    Token(Bearer),
}

/// the azure container a request is for, as its link names it.
pub(crate) struct Located {
    account: String,
    container: String,
    listed: bool,
    /// the link carries a secret of its own, such as a SAS pasted for one `get`, which no
    /// credential of the settings joins.
    signed: bool,
    /// the link as written, whose path and query a key signs.
    url: Url,
    /// whether the request went again with a new token already, after one was turned away.
    renewed: AtomicBool,
}

impl Located {
    fn key(&self) -> String {
        format!("{}/{}", self.account, self.container)
    }

    fn named(&self) -> String {
        format!("azure:{}/{}", self.account, self.container)
    }
}

/// how a request for an azure container went out, kept on its answer so a refusal can say why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Sent {
    /// without a credential: `azure_containers` does not list the container, or lists it and
    /// none was found for it.
    Anonymous { container: String, listed: bool },
    /// with the SAS `given` there, which says it works until `expiry`.
    Sas {
        container: String,
        given: Given,
        expiry: Option<String>,
    },
    /// signed with the key in `AZURE_STORAGE_KEY` of the container's account, `account`.
    Key { container: String, account: String },
    /// with a token of `by`.
    Token { container: String, by: SignedIn },
}

impl Default for Azure {
    fn default() -> Azure {
        Azure::new(Vec::new(), Arc::new(crate::auth::env))
    }
}

impl Azure {
    /// sends a credential for the `listed` containers: their own SAS, or what the settings
    /// `environment` gives.
    pub(crate) fn new(listed: Vec<AzureContainer>, environment: Environment) -> Azure {
        Azure {
            listed,
            environment,
            storage: OnceLock::new(),
            refused: Mutex::default(),
            token: Lease::default(),
        }
    }

    /// the container `url` is for, when it is a request over https to an azure blob host.
    pub(crate) fn locate(&self, url: &Url) -> Option<Located> {
        if url.scheme() != "https" || url.port().is_some() {
            return None;
        }
        let account = azure_account(url.host_str()?)?;
        let container = url.path_segments()?.next()?;
        if !is_azure_container(container) {
            return None;
        }
        Some(Located {
            listed: self.entry(account, container).is_some(),
            account: account.to_owned(),
            container: container.to_owned(),
            signed: super::secret(url).is_some(),
            url: url.clone(),
            renewed: AtomicBool::new(false),
        })
    }

    fn entry(&self, account: &str, container: &str) -> Option<&AzureContainer> {
        self.listed
            .iter()
            .find(|entry| entry.account() == account && entry.name() == container)
    }

    /// the credential for `located`: its entry's own SAS, else the environment's SAS, unless
    /// that is kept to another account, else the key of its account, else a token of the
    /// sign-in, asked for when none is held or the one held is due for renewal. none for a
    /// container nobody listed, or a link with a secret of its own.
    ///
    /// # Errors
    ///
    /// what [`crate::auth::azure::find`] finds wrong with the environment, or what getting a
    /// token found wrong, the same for every later request, or `interrupted` once `stop` asks.
    async fn credential(
        &self,
        located: &Located,
        stop: &Stop,
    ) -> Result<Option<Credential>, Error> {
        if located.signed {
            return Ok(None);
        }
        let Some(entry) = self.entry(&located.account, &located.container) else {
            return Ok(None);
        };
        if let Some(own) = entry.sas() {
            return Ok(Some(Credential::Sas(own.clone(), Given::Setting)));
        }
        let storage = self
            .storage
            .get_or_init(|| crate::auth::azure::find(&*self.environment, &self.listed))
            .as_ref()
            .map_err(Clone::clone)?;
        let account = storage.account.as_deref();
        if let Some(sas) = &storage.sas
            && account.is_none_or(|account| account == located.account)
        {
            return Ok(Some(Credential::Sas(sas.clone(), Given::Environment)));
        }
        if let Some(key) = storage
            .key
            .clone()
            .filter(|_| account == Some(located.account.as_str()))
        {
            return Ok(Some(Credential::Key(key)));
        }
        let SignIn::Cli(path) = storage.sign_in.as_ref().map_err(Clone::clone)? else {
            return Ok(None);
        };
        // boxed, so the future of every request does not carry the az cli's run
        let bearer = self
            .token
            .get(Instant::now, || {
                Box::pin(async {
                    let token =
                        crate::auth::azure::cli_token(path, SystemTime::now(), stop).await?;
                    Ok((bearer(&token.value, SignedIn::Cli)?, Some(token.lifetime)))
                })
            })
            .await?;
        Ok(Some(Credential::Token(bearer)))
    }

    fn refused_containers(&self) -> MutexGuard<'_, HashSet<String>> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `token` of `by` as a request carries it.
///
/// # Errors
///
/// a token no request header can carry.
fn bearer(token: &str, by: SignedIn) -> Result<Bearer, Error> {
    let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
        Error::new(
            Kind::AuthRequired,
            format!("{} gave a token no request header can carry", by.phrase()),
        )
    })?;
    value.set_sensitive(true);
    Ok(Bearer { value, by })
}

impl Sent {
    /// the bytes of text this holds, which a kept answer counts.
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Sent::Anonymous { container, .. } | Sent::Token { container, .. } => container.len(),
            Sent::Sas {
                container, expiry, ..
            } => container.len() + expiry.as_ref().map_or(0, String::len),
            Sent::Key { container, account } => container.len() + account.len(),
        }
    }

    /// the error for a download azure refused, as this says the request went.
    pub(crate) fn refusal(&self, url: &Url) -> Error {
        let shown = Shown(url).to_string();
        match self {
            Sent::Sas {
                container,
                given,
                expiry,
            } => sas_refused(container, *given, expiry.as_deref(), &shown),
            Sent::Key { container, account } => key_refused(container, account, &shown),
            Sent::Token { container, by } => token_refused(container, *by, &shown),
            Sent::Anonymous {
                container,
                listed: true,
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "azure refused {shown} without credentials, and none were found for {container}"
                ),
            )
            .help(missing(container)),
            Sent::Anonymous { container, .. } => Error::new(
                Kind::AuthRequired,
                format!("azure refused {shown} without credentials"),
            )
            .help(trust(container)),
        }
    }

    /// the error for a listing of `prefix` azure refused, as this says it went. azure refuses a
    /// container that does not exist as it refuses a private one.
    pub(crate) fn listing_refusal(&self, prefix: &str) -> Error {
        match self {
            Sent::Sas {
                container,
                given,
                expiry,
            } => sas_refused(container, *given, expiry.as_deref(), prefix),
            Sent::Key { container, account } => key_refused(container, account, prefix),
            Sent::Token { container, by } => token_refused(container, *by, prefix),
            Sent::Anonymous {
                container,
                listed: true,
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private, or does not exist: azure refused to list it without credentials, and none were found for {container}"
                ),
            )
            .help(missing(container)),
            Sent::Anonymous { container, .. } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private, or does not exist: azure refused to list it without credentials"
                ),
            )
            .help(trust(container)),
        }
    }
}

fn sas_refused(container: &str, given: Given, expiry: Option<&str>, what: &str) -> Error {
    let error = Error::new(
        Kind::AuthRequired,
        format!("azure refused the SAS {} for {what}", given.phrase()),
    )
    .help(format!(
        "a SAS must be for {container}, let requests read and list (sp=rl) and not have expired. ask the container's owner for a new one"
    ));
    match expiry {
        Some(expiry) => error.detail(format!("the SAS says it works until {expiry}")),
        None => error,
    }
}

fn key_refused(container: &str, account: &str, what: &str) -> Error {
    Error::new(
        Kind::AuthRequired,
        format!("azure refused the key in AZURE_STORAGE_KEY for {what}"),
    )
    .help(format!(
        "the key must be one of the access keys of the account {account}, as the azure portal shows them, and the account must let keys in. otherwise ask the owner of {container} for a SAS"
    ))
}

fn token_refused(container: &str, by: SignedIn, what: &str) -> Error {
    Error::new(
        Kind::AuthRequired,
        format!("azure refused {} for {what}", by.phrase()),
    )
    .help(format!(
        "it needs a role that reads blobs, such as Storage Blob Data Reader, on {container}: ask the container's owner for one, or sign in as another account with `az login`"
    ))
}

/// how to let fetchloom send credentials for `container`.
fn trust(container: &str) -> String {
    format!(
        "if you trust {container} with your azure credentials, list it in azure_containers in the settings file or FETCHLOOM_AZURE_CONTAINERS, and sign in with `az login` or set AZURE_STORAGE_SAS_TOKEN"
    )
}

/// how to give fetchloom credentials for `container`, which the settings list, when no az cli
/// is on PATH to sign in with.
fn missing(container: &str) -> String {
    format!(
        "install the azure cli and sign in with `az login`, or set AZURE_STORAGE_SAS_TOKEN to a SAS for {container}, or AZURE_STORAGE_KEY and AZURE_STORAGE_ACCOUNT to a key of its account"
    )
}

/// one hop of a request for an azure container, and the credential it carries.
pub(super) struct Blobbed<'r> {
    located: Option<&'r Located>,
    credential: Option<Credential>,
}

impl Blobbed<'_> {
    pub(super) fn authorized(&self) -> Option<Authorized> {
        let located = self.located.filter(|located| !located.signed)?;
        let container = located.named();
        let sent = match &self.credential {
            Some(Credential::Sas(sas, given)) => Sent::Sas {
                container,
                given: *given,
                expiry: sas.expiry(),
            },
            Some(Credential::Key(_)) => Sent::Key {
                container,
                account: located.account.clone(),
            },
            Some(Credential::Token(bearer)) => Sent::Token {
                container,
                by: bearer.by,
            },
            None => Sent::Anonymous {
                container,
                listed: located.listed,
            },
        };
        Some(Authorized::Azure(sent))
    }

    /// the link the hop asks: `hop`, with the SAS after its query when it carries one.
    pub(super) fn url<'h>(&self, hop: &'h Url) -> Cow<'h, Url> {
        let Some(Credential::Sas(sas, _)) = &self.credential else {
            return Cow::Borrowed(hop);
        };
        let query = match hop.query().filter(|query| !query.is_empty()) {
            Some(query) => format!("{query}&{}", sas.query()),
            None => sas.query().to_owned(),
        };
        let mut url = hop.clone();
        url.set_query(Some(&query));
        Cow::Owned(url)
    }

    /// the version of azure's API a request to a blob host asks for, and the token it carries,
    /// or nothing.
    pub(super) fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if self.located.is_some() {
            headers.insert(
                HeaderName::from_static("x-ms-version"),
                HeaderValue::from_static(AZURE_VERSION),
            );
        }
        if let Some(Credential::Token(bearer)) = &self.credential {
            headers.insert(header::AUTHORIZATION, bearer.value.clone());
        }
        headers
    }

    /// `request`, made last, signed with the key the hop carries at `now`: the time in
    /// `x-ms-date`, then the signature of all it holds by then, the headers its shape added
    /// among them. any other request goes as it is.
    ///
    /// # Errors
    ///
    /// a request that cannot be made, which no signature can then hold.
    pub(super) fn signed(
        &self,
        request: reqwest::RequestBuilder,
        now: SystemTime,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let (Some(located), Some(Credential::Key(key))) = (self.located, &self.credential) else {
            return Ok(request);
        };
        let request = request.header(
            HeaderName::from_static("x-ms-date"),
            httpdate::fmt_http_date(now),
        );
        let (client, built) = request.build_split();
        let unsigned = |err: &dyn std::fmt::Display| {
            Error::new(
                Kind::AuthRequired,
                format!("could not sign a request for {}: {err}", located.named()),
            )
        };
        let mut built = built.map_err(|err| unsigned(&err))?;
        let authorization = shared_key::authorization(
            &located.account,
            key,
            built.method().as_str(),
            built.headers(),
            &located.url,
        );
        let mut value = HeaderValue::from_str(&authorization).map_err(|err| unsigned(&err))?;
        value.set_sensitive(true);
        built.headers_mut().insert(header::AUTHORIZATION, value);
        Ok(reqwest::RequestBuilder::from_parts(client, built))
    }
}

impl Net {
    /// the hop after `redirects` redirects of a request for `located`: only the first asks for
    /// azure's version and, once its container refused a request without one, carries a
    /// credential.
    pub(super) async fn azure_hop<'r>(
        &self,
        located: Option<&'r Located>,
        redirects: usize,
    ) -> Result<Blobbed<'r>, Failure> {
        let located = located.filter(|_| redirects == 0);
        let refused =
            located.is_some_and(|located| self.azure.refused_containers().contains(&located.key()));
        let credential = match located {
            Some(located) if refused => self
                .azure
                .credential(located, &self.stop)
                .await
                .map_err(Failure::Final)?,
            _ => None,
        };
        if let Some(located) = located {
            let host = format!("{}.blob.core.windows.net", located.account);
            match &credential {
                Some(Credential::Sas(..)) => tracing::debug!(host, "sending a SAS"),
                Some(Credential::Key(_)) => {
                    tracing::debug!(host, "signing the request with an azure account key");
                }
                Some(Credential::Token(_)) => tracing::debug!(host, "sending an azure token"),
                None => {}
            }
        }
        Ok(Blobbed {
            located,
            credential,
        })
    }

    /// whether the request goes again after `response`: once, with a credential, when azure
    /// refused it without one for a listed container one was found for, and once with a new
    /// token when azure turned one away with 401, as an expired or revoked one is, which is then
    /// forgotten. otherwise the response is told how the request went.
    ///
    /// # Errors
    ///
    /// an environment that holds no credential fetchloom can read, or a token that could not be
    /// got.
    pub(super) async fn azure_again(
        &self,
        hop: &Blobbed<'_>,
        response: &mut reqwest::Response,
    ) -> Result<bool, Failure> {
        let Some(located) = hop.located.filter(|located| !located.signed) else {
            return Ok(false);
        };
        if let Some(Credential::Token(bearer)) = &hop.credential
            && response.status() == StatusCode::UNAUTHORIZED
            && !located.renewed.swap(true, Ordering::Relaxed)
        {
            tracing::debug!(
                host = host(response.url()),
                "azure turned the token away, so the request goes again with a new one"
            );
            self.azure.token.forget(bearer).await;
            return Ok(true);
        }
        if hop.credential.is_none()
            && refused(response)
            && self
                .azure
                .credential(located, &self.stop)
                .await
                .map_err(Failure::Final)?
                .is_some()
        {
            self.azure.refused_containers().insert(located.key());
            tracing::debug!(
                host = host(response.url()),
                "azure refused the request without credentials, so it goes again with them"
            );
            return Ok(true);
        }
        if let Some(sent) = hop.authorized() {
            response.extensions_mut().insert(sent);
        }
        Ok(false)
    }
}

/// whether azure turned a request away for want of access: 401 or 403, or 409 from an account
/// that lets nobody in without a key.
fn refused(response: &reqwest::Response) -> bool {
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => true,
        StatusCode::CONFLICT => response
            .headers()
            .get("x-ms-error-code")
            .is_some_and(|code| code == "PublicAccessNotPermitted"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
