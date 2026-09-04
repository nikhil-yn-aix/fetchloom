//! Credentials, where they were found, and what a provider tells the user.

use serde::Serialize;

use crate::redact::Secret;
use crate::reference::Host;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialOrigin {
    Environment,
    PlatformStore,
    ProviderHelper,
}

#[derive(Clone, Debug, Serialize)]
pub struct SigningKeys {
    pub access_key: String,
    pub secret_key: Secret<String>,
    pub session_token: Option<Secret<String>>,
    pub region: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "shape")]
pub enum Secrets {
    Bearer { value: Secret<String> },
    Signing { keys: SigningKeys },
}

impl Secrets {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bearer { .. } => "a bearer token",
            Self::Signing { .. } => "a signing key pair",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Credential {
    pub host: Host,
    pub origin: CredentialOrigin,
    pub secrets: Secrets,
}

impl Credential {
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        match &self.secrets {
            Secrets::Bearer { value } => Some(value.expose()),
            Secrets::Signing { .. } => None,
        }
    }

    #[must_use]
    pub fn signing(&self) -> Option<&SigningKeys> {
        match &self.secrets {
            Secrets::Signing { keys } => Some(keys),
            Secrets::Bearer { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Necessity {
    Required,
    Optional,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderHelp {
    pub provider: String,
    pub unlocks: String,
    pub necessity: Necessity,
    pub steps: Vec<String>,
    pub placement: String,
    pub verification: String,
    pub scope: String,
}

#[must_use]
pub fn token_variable(host: &Host) -> String {
    host_variable("FETCHLOOM_TOKEN_", host)
}

#[must_use]
pub fn host_variable(prefix: &str, host: &Host) -> String {
    let mut name = String::from(prefix);
    for character in host.as_str().chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}
