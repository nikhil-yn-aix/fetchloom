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

/// What a run is allowed to do.
pub trait Policy: Send + Sync {
    /// Reports whether every network activity is forbidden.
    fn offline(&self) -> bool;

    /// Returns the bounds no part of the run may exceed.
    fn limits(&self) -> &Limits;

    /// Returns what a cache hit is checked against.
    fn verification(&self) -> VerificationPolicy;

    /// Returns how far a write is pushed before publication.
    fn durability(&self) -> DurabilityTier;

    /// Returns where the cache directory is, when a cache is in use.
    fn cache_directory(&self) -> Option<&Path>;

    /// Returns the ceiling on transfers in flight across every host.
    fn concurrency(&self) -> Option<NonZeroU32>;

    /// Returns the ceiling on transfers in flight for one host.
    fn per_host(&self) -> Option<NonZeroU32>;

    /// Returns the ceiling on how fast the run may transfer.
    fn bandwidth(&self) -> Option<Bandwidth>;

    /// Reports whether a trust class is weak enough to refuse.
    fn accepts(&self, class: TrustClass) -> bool;

    /// Finds the credential for a host, without reporting the secret.
    ///
    /// # Errors
    ///
    /// Fails when a credential is required and none was found, and when a
    /// found credential is expired, revoked, or too narrowly scoped.
    fn credential(&self, host: &Host, necessity: Necessity) -> Result<Option<Credential>, Error>;

    /// Decides whether an optional credential is worth interrupting for, and
    /// asks when it is.
    ///
    /// Takes the provider's fixed help record and the projected difference the
    /// credential would make. Returns the credential when the user supplied
    /// one and nothing when the user declined or was not asked.
    ///
    /// # Errors
    ///
    /// Fails when a prompt is required and no terminal is attached.
    fn offer_credential(
        &self,
        help: &ProviderHelp,
        projected_gain: std::time::Duration,
    ) -> Result<Option<Credential>, Error>;

    /// Decides whether the recorded terms have been accepted.
    ///
    /// # Errors
    ///
    /// Fails when acceptance is required and no terminal is attached.
    fn terms(&self, license: &License) -> Result<Acceptance, Error>;
}
