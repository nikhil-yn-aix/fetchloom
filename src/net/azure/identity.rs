//! this machine's managed identity: a token for azure storage from the instance metadata service
//! at 169.254.169.254, asked through a client of its own and only once `AZURE_TOKEN_CREDENTIALS`
//! names the identity. no other request is let through to that private address.

use std::time::Duration;

use reqwest::StatusCode;
use url::Url;

use super::super::failure::{self, Failure, Retry};
use super::super::metadata::{self, Unheard};
use super::super::{Net, host};
use crate::auth::azure::STORAGE;
use crate::auth::oauth;
use crate::error::{Error, Kind};

/// where every virtual machine's instance metadata service gives its identity's tokens.
const ENDPOINT: &str = "http://169.254.169.254/metadata/identity/oauth2/token";
/// the version of the service's API that answers for a token.
const VERSION: &str = "2018-02-01";
const LARGEST_ANSWER: usize = 64 << 10;
/// how long a token lives when the answer does not say: azure's tokens live an hour or more.
const LIFETIME: Duration = Duration::from_hours(1);

impl Net {
    /// a token that reads azure storage, of this machine's managed identity or the user-assigned
    /// one `client_id` names, and how long it lives. `endpoint` names a test's stand-in service.
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
        if self.offline.enabled() {
            return Err(Error::new(
                Kind::OfflineMissing,
                "offline, and this needs a token of this machine's managed identity",
            )
            .help(self.offline.retry()));
        }
        self.may_ask(&url, false, None)
            .map_err(|failure| failure.into_error(Kind::NetworkRefused))?;
        // azure's own sdk asks first without the `metadata` header, which the service refuses at
        // once, so a machine off azure waits no longer than that first ask
        let first = self
            .ask_metadata(&url, None, metadata::FIRST)
            .await
            .map_err(|failure| failure.into_error(Kind::AuthRequired))?;
        if let Err(unheard) = first {
            return Err(unanswered(unheard));
        }
        let mut retry = Retry::new(self, &url);
        loop {
            match self.ask_identity(&url).await {
                Ok((status, body)) => return token(status, &body, client_id),
                Err(failure) => retry.after(failure, false).await?,
            }
        }
    }

    /// one GET of the service at `url`, with `metadata: true`: the status and body of its answer,
    /// unless another try may get past it, as for the 404 and 410 it sends while it starts. a
    /// service that does not answer fails at once: off azure, nothing ever will.
    async fn ask_identity(&self, url: &Url) -> Result<(u16, Vec<u8>), Failure> {
        tracing::debug!(
            host = host(url),
            "asking this machine's managed identity for an azure storage token"
        );
        let mut response = match self
            .ask_metadata(
                url,
                Some(("metadata", "true")),
                metadata::CONNECT + self.idle,
            )
            .await?
        {
            Ok(response) => response,
            Err(unheard) => return Err(Failure::Final(unanswered(unheard))),
        };
        let status = response.status();
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
            || failure::is_transient(status)
        {
            return Err(failure::transient(format!(
                "azure's metadata service answered {}",
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
                        "azure's metadata service sent more than 64 KiB where a token was expected",
                    )
                },
                |_| async { Ok(()) },
            )
            .await
            .map_err(|failure| match failure {
                Failure::Transient { .. } => {
                    failure::transient("azure's metadata service broke off its answer".to_owned())
                }
                failure => failure,
            })?;
        Ok((status.as_u16(), body))
    }
}

/// the error for a metadata service that does not answer, as off azure, for the reason
/// `unheard`.
fn unanswered(unheard: Unheard) -> Error {
    let unheard = match unheard {
        Unheard::Refused => "169.254.169.254 refused the connection".to_owned(),
        Unheard::Waited(wait) => format!(
            "no metadata service answered at 169.254.169.254 within {} s",
            wait.as_secs()
        ),
    };
    Error::new(
        Kind::AuthRequired,
        format!(
            "this machine's managed identity could not be asked: {unheard}, so this is no azure virtual machine, or something blocks its service"
        ),
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
    if status != 200 {
        let said = oauth::said("azure's metadata service", "azure", status, body);
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
    let (value, seconds) = oauth::bearer("azure's metadata service", body)?;
    Ok((value, seconds.map_or(LIFETIME, Duration::from_secs)))
}
