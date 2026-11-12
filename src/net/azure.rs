//! which requests to azure's blob hosts carry a SAS: those for a container the `azure_containers`
//! setting lists, once azure refused one without a SAS this run, and only to the container's own
//! blob host over https, before any redirect. the SAS is the one the container's entry gives,
//! else `AZURE_STORAGE_SAS_TOKEN`, kept to the account `AZURE_STORAGE_ACCOUNT` names when it
//! names one, as the az cli reads them. every first request to a blob host asks for the version
//! of azure's API that listings are read in.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::failure::Failure;
use super::google::Environment;
use super::{Authorized, Net, Shown, host};
use crate::error::{Error, Kind};
use crate::source::{
    AZURE_VERSION, AzureContainer, Sas, azure_account, is_azure_account, is_azure_container,
};

/// what one run knows about azure access: the containers listed, the environment's SAS, read
/// when first needed, and which containers refused a request without one.
pub(crate) struct Azure {
    listed: Vec<AzureContainer>,
    environment: Environment,
    shared: OnceLock<Result<Option<Shared>, Error>>,
    /// containers azure refused without a SAS this run, as `account/container`, asked with one
    /// from then on.
    refused: Mutex<HashSet<String>>,
}

/// the SAS `AZURE_STORAGE_SAS_TOKEN` gives, and the one account `AZURE_STORAGE_ACCOUNT` keeps it
/// to, when it names one.
#[derive(Clone)]
struct Shared {
    sas: Sas,
    account: Option<String>,
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

/// the azure container a request is for, as its link names it.
pub(crate) struct Located {
    account: String,
    container: String,
    listed: bool,
    /// the link carries a secret of its own, such as a SAS pasted for one `get`, which no SAS of
    /// the settings joins.
    signed: bool,
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
    /// without a SAS: `azure_containers` does not list the container, or lists it and no SAS
    /// was found for it.
    Anonymous { container: String, listed: bool },
    /// with the SAS `given` there, which says it works until `expiry`.
    Sas {
        container: String,
        given: Given,
        expiry: Option<String>,
    },
}

impl Default for Azure {
    fn default() -> Azure {
        Azure::new(Vec::new(), Arc::new(crate::auth::env))
    }
}

impl Azure {
    /// sends a SAS for the `listed` containers, their own or the one the settings `environment`
    /// gives.
    pub(crate) fn new(listed: Vec<AzureContainer>, environment: Environment) -> Azure {
        Azure {
            listed,
            environment,
            shared: OnceLock::new(),
            refused: Mutex::default(),
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
        })
    }

    fn entry(&self, account: &str, container: &str) -> Option<&AzureContainer> {
        self.listed
            .iter()
            .find(|entry| entry.account() == account && entry.name() == container)
    }

    /// the SAS for `located`: its entry's own, else the environment's, unless that is kept to
    /// another account. none for a container nobody listed, or a link with a secret of its own.
    ///
    /// # Errors
    ///
    /// an `AZURE_STORAGE_SAS_TOKEN` or `AZURE_STORAGE_ACCOUNT` that holds something else.
    fn sas(&self, located: &Located) -> Result<Option<(Sas, Given)>, Error> {
        if located.signed {
            return Ok(None);
        }
        let Some(entry) = self.entry(&located.account, &located.container) else {
            return Ok(None);
        };
        if let Some(own) = entry.sas() {
            return Ok(Some((own.clone(), Given::Setting)));
        }
        let shared = self
            .shared
            .get_or_init(|| shared(&*self.environment))
            .clone()?;
        Ok(shared
            .filter(|shared| {
                shared
                    .account
                    .as_deref()
                    .is_none_or(|account| account == located.account)
            })
            .map(|shared| (shared.sas, Given::Environment)))
    }

    fn refused_containers(&self) -> MutexGuard<'_, HashSet<String>> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// the SAS `AZURE_STORAGE_SAS_TOKEN` gives, kept to the account `AZURE_STORAGE_ACCOUNT` names.
fn shared(
    environment: &(dyn Fn(&str) -> Option<std::ffi::OsString> + Send + Sync),
) -> Result<Option<Shared>, Error> {
    let set = |name| environment(name).filter(|value| !value.is_empty());
    let Some(text) = set("AZURE_STORAGE_SAS_TOKEN") else {
        return Ok(None);
    };
    let sas = text.to_str().and_then(Sas::parse).ok_or_else(|| {
        Error::new(
            Kind::AuthRequired,
            "AZURE_STORAGE_SAS_TOKEN holds no SAS fetchloom can read",
        )
        .help("set it to the SAS the azure portal or `az storage container generate-sas` gives, a query such as sv=...&sp=rl&se=...&sig=...")
    })?;
    let account = match set("AZURE_STORAGE_ACCOUNT") {
        None => None,
        Some(account) => Some(
            account
                .to_str()
                .map(str::to_ascii_lowercase)
                .filter(|account| is_azure_account(account))
                .ok_or_else(|| {
                    Error::new(
                        Kind::AuthRequired,
                        "AZURE_STORAGE_ACCOUNT holds no storage account's name",
                    )
                    .help("set it to the account the SAS is for, 3 to 24 lowercase letters and digits, or unset it")
                })?,
        ),
    };
    Ok(Some(Shared { sas, account }))
}

impl Sent {
    /// the error for a download azure refused, as this says the request went.
    pub(crate) fn refusal(&self, url: &Url) -> Error {
        let shown = Shown(url).to_string();
        match self {
            Sent::Sas {
                container,
                given,
                expiry,
            } => sas_refused(container, *given, expiry.as_deref(), &shown),
            Sent::Anonymous {
                container,
                listed: true,
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "azure refused {shown} without a SAS, and no SAS was found for {container}"
                ),
            )
            .help(missing(container)),
            Sent::Anonymous { container, .. } => Error::new(
                Kind::AuthRequired,
                format!("azure refused {shown} without a SAS"),
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
            Sent::Anonymous {
                container,
                listed: true,
            } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private, or does not exist: azure refused to list it without a SAS, and no SAS was found for {container}"
                ),
            )
            .help(missing(container)),
            Sent::Anonymous { container, .. } => Error::new(
                Kind::AuthRequired,
                format!(
                    "{prefix} is private, or does not exist: azure refused to list it without a SAS"
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

/// how to let fetchloom send a SAS for `container`.
fn trust(container: &str) -> String {
    format!(
        "if you trust {container} with a SAS, list it in azure_containers in the settings file or FETCHLOOM_AZURE_CONTAINERS, and set AZURE_STORAGE_SAS_TOKEN"
    )
}

/// how to give fetchloom a SAS for `container`, which the settings list.
fn missing(container: &str) -> String {
    format!(
        "set AZURE_STORAGE_SAS_TOKEN to a SAS for {container}, or give its SAS link as its entry in azure_containers in the settings file"
    )
}

/// one hop of a request for an azure container, and the SAS it carries.
pub(super) struct Blobbed<'r> {
    located: Option<&'r Located>,
    sas: Option<(Sas, Given)>,
}

impl Blobbed<'_> {
    /// the link the hop asks: `hop`, with the SAS after its query when it carries one.
    pub(super) fn url<'h>(&self, hop: &'h Url) -> Cow<'h, Url> {
        let Some((sas, _)) = &self.sas else {
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

    /// the version of azure's API a request to a blob host asks for, or nothing.
    pub(super) fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if self.located.is_some() {
            headers.insert(
                HeaderName::from_static("x-ms-version"),
                HeaderValue::from_static(AZURE_VERSION),
            );
        }
        headers
    }
}

impl Net {
    /// the hop after `redirects` redirects of a request for `located`: only the first asks for
    /// azure's version and, once its container refused a request without one, carries the SAS.
    pub(super) fn azure_hop<'r>(
        &self,
        located: Option<&'r Located>,
        redirects: usize,
    ) -> Result<Blobbed<'r>, Failure> {
        let located = located.filter(|_| redirects == 0);
        let sas = match located {
            Some(located) if self.azure.refused_containers().contains(&located.key()) => {
                self.azure.sas(located).map_err(Failure::Final)?
            }
            _ => None,
        };
        if let (Some(located), Some(_)) = (located, &sas) {
            tracing::debug!(
                host = format!("{}.blob.core.windows.net", located.account),
                "sending a SAS"
            );
        }
        Ok(Blobbed { located, sas })
    }

    /// whether the request goes again after `response`: once, with the SAS, when azure refused
    /// it without one for a listed container a SAS was found for. otherwise the response is told
    /// how the request went.
    ///
    /// # Errors
    ///
    /// an environment that holds no SAS fetchloom can read.
    pub(super) fn azure_again(
        &self,
        hop: &Blobbed<'_>,
        response: &mut reqwest::Response,
    ) -> Result<bool, Failure> {
        let Some(located) = hop.located.filter(|located| !located.signed) else {
            return Ok(false);
        };
        if hop.sas.is_none()
            && refused(response)
            && self.azure.sas(located).map_err(Failure::Final)?.is_some()
        {
            self.azure.refused_containers().insert(located.key());
            tracing::debug!(
                host = host(response.url()),
                "azure refused the request without a SAS, so it goes again with one"
            );
            return Ok(true);
        }
        let sent = match &hop.sas {
            Some((sas, given)) => Sent::Sas {
                container: located.named(),
                given: *given,
                expiry: sas.expiry(),
            },
            None => Sent::Anonymous {
                container: located.named(),
                listed: located.listed,
            },
        };
        response.extensions_mut().insert(Authorized::Azure(sent));
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
