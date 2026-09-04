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

    fn accepts(&self, class: TrustClass) -> bool;

    fn credential(&self, host: &Host, necessity: Necessity) -> Result<Option<Credential>, Error>;

    fn offer_credential(
        &self,
        help: &ProviderHelp,
        projected_gain: std::time::Duration,
    ) -> Result<Option<Credential>, Error>;

    fn terms(&self, license: &License) -> Result<Acceptance, Error>;
}
