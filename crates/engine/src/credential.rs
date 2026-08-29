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

/// A credential, bound to the host it was resolved for.
#[derive(Clone, Debug, Serialize)]
pub struct Credential {
    /// The host this credential may be sent to and no other.
    pub host: Host,
    /// Where it was found, reported without the secret.
    pub origin: CredentialOrigin,
    /// The secret itself, which is never written anywhere.
    pub value: Secret<String>,
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
