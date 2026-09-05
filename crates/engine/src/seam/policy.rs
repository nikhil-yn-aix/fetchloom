//! The Policy seam: trust, offline, limits, credentials, and terms.

use std::num::NonZeroU32;
use std::path::Path;

use crate::credential::{Credential, Necessity, ProviderHelp};
use crate::durability::DurabilityTier;
use crate::error::Error;
use crate::license::{Acceptance, License};
use crate::limits::{Bandwidth, Limits};
use crate::reference::Host;
use crate::trust::TrustClass;
use crate::verification::VerificationPolicy;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IoMode {
    Auto,
    Buffered,
    Uncached,
}

pub trait Policy: Send + Sync {
    fn offline(&self) -> bool;

    fn limits(&self) -> &Limits;

    fn verification(&self) -> VerificationPolicy;

    fn durability(&self) -> DurabilityTier;

    fn cache_directory(&self) -> Option<&Path>;

    fn concurrency(&self) -> Option<NonZeroU32>;

    fn per_host(&self) -> Option<NonZeroU32>;

    fn bandwidth(&self) -> Option<Bandwidth>;

    fn aggressive(&self) -> bool;

    fn adapts(&self) -> bool;

    fn io(&self) -> IoMode;

    fn compression(&self) -> crate::compression::CompressionChoice;

    fn accepts(&self, class: TrustClass) -> bool;

    /// # Errors
    /// `policy.credential_missing` when the host needs one and none is held,
    /// and `policy.credential_invalid` when the stored credential cannot be
    /// read. A host that needs none is `None` rather than an error.
    fn credential(&self, host: &Host, necessity: Necessity) -> Result<Option<Credential>, Error>;

    /// # Errors
    /// `policy.credential_invalid` when the credential offered cannot be
    /// read. A refused offer is `None` rather than an error.
    fn offer_credential(
        &self,
        help: &ProviderHelp,
        projected_gain: std::time::Duration,
    ) -> Result<Option<Credential>, Error>;

    /// # Errors
    /// `policy.terms_required` when the license must be accepted and this run
    /// cannot ask.
    fn terms(&self, license: &License) -> Result<Acceptance, Error>;
}
