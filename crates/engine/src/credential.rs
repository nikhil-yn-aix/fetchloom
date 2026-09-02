//! Credentials, where they were found, and what a provider tells the user.

use serde::Serialize;

use crate::redact::Secret;
use crate::reference::Host;

/// Where a credential was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialOrigin {
    /// A host-scoped environment variable.
    Environment,
    /// This platform's credential store.
    PlatformStore,
    /// The provider's own helper.
    ProviderHelper,
}

/// The keys a request is signed with, where the secret is signed with rather
/// than sent.
#[derive(Clone, Debug, Serialize)]
pub struct SigningKeys {
    /// The key that names which credential signed, which travels in the clear.
    pub access_key: String,
    /// The key a signature is derived from, which never crosses the wire.
    pub secret_key: Secret<String>,
    /// The token a temporary credential carries, when it has one.
    pub session_token: Option<Secret<String>>,
    /// The region a signature is computed over.
    pub region: String,
}

/// What a credential proves with, which is one of exactly two shapes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "shape")]
pub enum Secrets {
    /// One opaque value, sent as it is.
    Bearer {
        /// The value sent, which is never written anywhere.
        value: Secret<String>,
    },
    /// Keys a request is signed with.
    Signing {
        /// The keys signed with, whose secret never crosses the wire.
        keys: SigningKeys,
    },
}

impl Secrets {
    /// Returns the name this shape is reported under.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bearer { .. } => "a bearer token",
            Self::Signing { .. } => "a signing key pair",
        }
    }
}

/// A credential, bound to the host it was resolved for.
#[derive(Clone, Debug, Serialize)]
pub struct Credential {
    /// The host this credential may be sent to and no other.
    pub host: Host,
    /// Where it was found, reported without the secret.
    pub origin: CredentialOrigin,
    /// What it proves with, which is never written anywhere.
    pub secrets: Secrets,
}

impl Credential {
    /// Returns the one opaque value a bearer credential is sent as, and nothing
    /// for a credential of the other shape.
    #[must_use]
    pub fn bearer(&self) -> Option<&str> {
        match &self.secrets {
            Secrets::Bearer { value } => Some(value.expose()),
            Secrets::Signing { .. } => None,
        }
    }

    /// Returns the keys a signing credential signs with, and nothing for a
    /// credential of the other shape.
    #[must_use]
    pub fn signing(&self) -> Option<&SigningKeys> {
        match &self.secrets {
            Secrets::Signing { keys } => Some(keys),
            Secrets::Bearer { .. } => None,
        }
    }
}

/// Whether a run can proceed without a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Necessity {
    /// No reachable source can serve the request without it.
    Required,
    /// A source needing it scored better than every reachable alternative.
    Optional,
}

/// The fixed record a provider ships, which Fetchloom never improvises.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderHelp {
    /// The name the user recognizes.
    pub provider: String,
    /// What becomes possible or faster, stated concretely.
    pub unlocks: String,
    /// Whether the credential is required or optional.
    pub necessity: Necessity,
    /// Numbered steps naming the page to open and the control to use.
    pub steps: Vec<String>,
    /// The environment variable or credential store entry to create.
    pub placement: String,
    /// The command that confirms the credential works.
    pub verification: String,
    /// The narrowest permissions that work.
    pub scope: String,
}

/// Returns the environment variable a host's credential is read from.
#[must_use]
pub fn token_variable(host: &Host) -> String {
    host_variable("FETCHLOOM_TOKEN_", host)
}

/// Returns the host-scoped environment variable one field of a credential is
/// read from.
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
