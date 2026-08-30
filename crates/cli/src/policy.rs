//! What a run is allowed to do, decided by settings, streams, and terms.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
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
use crate::terminal::Streams;

/// Builds the environment variable name a host's credential is read from.
///
/// Takes a host name. Returns the variable name with the host uppercased and
/// every character that is not a letter or a digit replaced by an underscore.
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
    ///
    /// Takes the resolved settings, what the streams are attached to, whether
    /// terms were asserted on the command line, and where to read the
    /// environment and emit events.
    #[must_use]
    pub fn new(
        settings: Settings,
        streams: Streams,
        accepted_terms: bool,
        environment: &'a dyn Environment,
        observer: &'a dyn Observer,
        sequence: &'a Sequence,
    ) -> Self {
        Self {
            settings,
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
        VerificationPolicy::default()
    }

    fn durability(&self) -> DurabilityTier {
        DurabilityTier::default()
    }

    fn cache_directory(&self) -> Option<&Path> {
        None
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
            TrustClass::Unverified => false,
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
        _projected_gain: Duration,
    ) -> Result<Option<Credential>, Error> {
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

/// Returns the cache directory a run uses when no level supplied one.
///
/// This is the cache location and never the configuration location; the two are
/// separate directories. Returns nothing when the platform's own variable is
/// unset and no home directory is known.
#[must_use]
pub fn default_cache_directory(environment: &dyn Environment) -> Option<PathBuf> {
    if cfg!(windows) {
        environment
            .get("LOCALAPPDATA")
            .map(|base| Path::new(&base).join("Fetchloom").join("Cache"))
    } else {
        environment
            .get("XDG_CACHE_HOME")
            .map(|base| Path::new(&base).join("fetchloom"))
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| Path::new(&home).join(".cache").join("fetchloom"))
            })
    }
}
