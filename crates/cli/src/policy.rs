//! What a run is allowed to do, decided by settings, streams, and terms.

use std::num::NonZeroU32;
use std::path::Path;
use std::time::Duration;

use fetchloom_engine::credential::{Credential, CredentialOrigin, Necessity, ProviderHelp};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::license::{Acceptance, License};
use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::redact::Secret;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::verification::VerificationPolicy;

use crate::settings::{Environment, Settings};
use crate::surface::{DurabilityChoice, TransferFlags, VerifyChoice};
use crate::terminal::Streams;

/// Builds the environment variable name a host's credential is read from.
#[must_use]
pub fn token_variable(host: &Host) -> String {
    let mut name = String::from("FETCHLOOM_TOKEN_");
    for character in host.as_str().chars() {
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}

/// The Policy the command line resolves to.
pub struct CommandLinePolicy<'a> {
    settings: Settings,
    verification: VerificationPolicy,
    durability: DurabilityTier,
    limits: Limits,
    streams: Streams,
    accepted_terms: bool,
    environment: &'a dyn Environment,
    observer: &'a dyn Observer,
    sequence: &'a Sequence,
}

impl std::fmt::Debug for CommandLinePolicy<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandLinePolicy")
            .field("settings", &self.settings)
            .field("streams", &self.streams)
            .field("accepted_terms", &self.accepted_terms)
            .finish_non_exhaustive()
    }
}

impl<'a> CommandLinePolicy<'a> {
    /// Builds the policy for a run.
    #[must_use]
    pub fn new(
        settings: Settings,
        transfer: &TransferFlags,
        streams: Streams,
        accepted_terms: bool,
        environment: &'a dyn Environment,
        observer: &'a dyn Observer,
        sequence: &'a Sequence,
    ) -> Self {
        Self {
            settings,
            verification: verification_of(transfer),
            durability: durability_of(transfer),
            limits: Limits::default(),
            streams,
            accepted_terms,
            environment,
            observer,
            sequence,
        }
    }

    fn emit(&self, payload: EventPayload) {
        self.observer.emit(&Event::new(self.sequence, payload));
    }
}

impl Policy for CommandLinePolicy<'_> {
    fn offline(&self) -> bool {
        self.settings.offline.value
    }

    fn limits(&self) -> &Limits {
        &self.limits
    }

    fn verification(&self) -> VerificationPolicy {
        self.verification
    }

    fn durability(&self) -> DurabilityTier {
        self.durability
    }

    fn cache_directory(&self) -> Option<&Path> {
        Some(self.settings.cache_dir.value.as_path())
    }

    fn concurrency(&self) -> Option<NonZeroU32> {
        None
    }

    fn per_host(&self) -> Option<NonZeroU32> {
        None
    }

    fn bandwidth(&self) -> Option<Bandwidth> {
        None
    }

    fn accepts(&self, class: TrustClass) -> bool {
        match class {
            TrustClass::Verified | TrustClass::Corroborated | TrustClass::Tofu => true,
            TrustClass::Unverified => self.verification == VerificationPolicy::Never,
        }
    }

    fn credential(&self, host: &Host, necessity: Necessity) -> Result<Option<Credential>, Error> {
        if let Some(value) = self.environment.get(&token_variable(host)) {
            return Ok(Some(Credential {
                host: host.clone(),
                origin: CredentialOrigin::Environment,
                value: Secret::new(value),
            }));
        }
        self.emit(EventPayload::Degrade {
            requested: "a credential from the platform credential store".to_owned(),
            used: "the host scoped environment variable only".to_owned(),
            reason: "this build reads no platform credential store".to_owned(),
        });
        match necessity {
            Necessity::Optional => Ok(None),
            Necessity::Required => {
                self.emit(EventPayload::CredentialRequired {
                    provider: host.to_string(),
                });
                Err(Error::new(
                    ErrorKind::PolicyCredentialMissing,
                    format!(
                        "set {} to a token for {host}, then run the command again",
                        token_variable(host)
                    ),
                )
                .with_source(host.as_str()))
            }
        }
    }

    fn offer_credential(
        &self,
        help: &ProviderHelp,
        projected_gain: Duration,
    ) -> Result<Option<Credential>, Error> {
        if projected_gain <= self.limits.credential_offer_threshold {
            return Ok(None);
        }
        if !self.streams.can_prompt() {
            self.emit(EventPayload::CredentialDeclined {
                provider: help.provider.clone(),
            });
            return Ok(None);
        }
        self.emit(EventPayload::CredentialOffer {
            provider: help.provider.clone(),
        });
        self.emit(EventPayload::CredentialDeclined {
            provider: help.provider.clone(),
        });
        Ok(None)
    }

    fn terms(&self, license: &License) -> Result<Acceptance, Error> {
        if !license.requires_acceptance {
            return Ok(Acceptance::Asserted);
        }
        if self.accepted_terms {
            return Ok(Acceptance::Asserted);
        }
        if self.streams.can_prompt() {
            return Ok(Acceptance::Withheld);
        }
        Err(Error::new(
            ErrorKind::PolicyTermsRequired,
            "run the command again with --yes to assert that you accept the recorded terms",
        ))
    }
}

/// Returns the check a run applies to a cache hit and to a destination entry.
fn verification_of(transfer: &TransferFlags) -> VerificationPolicy {
    match transfer.verify {
        Some(VerifyChoice::Always) => VerificationPolicy::Always,
        Some(VerifyChoice::Fingerprint) | None => VerificationPolicy::Fingerprint,
        Some(VerifyChoice::Never) => VerificationPolicy::Never,
    }
}

/// Returns how far a write is pushed before publication.
fn durability_of(transfer: &TransferFlags) -> DurabilityTier {
    match transfer.durability {
        Some(DurabilityChoice::Strict) => DurabilityTier::Strict,
        Some(DurabilityChoice::Normal) | None => DurabilityTier::Normal,
        Some(DurabilityChoice::Fast) => DurabilityTier::Fast,
    }
}
