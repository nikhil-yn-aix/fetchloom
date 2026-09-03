//! What a run is allowed to do, decided by settings, streams, and terms.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use fetchloom_engine::credential::{
    Credential, CredentialOrigin, Necessity, ProviderHelp, Secrets, SigningKeys, host_variable,
    token_variable,
};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::license::{Acceptance, License};
use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::redact::Secret;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::{IoMode, Policy};
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::verification::VerificationPolicy;

use crate::settings::{Environment, Settings};
use crate::surface::{DurabilityChoice, IoChoice, TransferFlags, VerifyChoice};
use crate::terminal::Streams;

/// Asks a yes or no question on the terminal a run is attached to.
pub trait Prompter: Send + Sync {
    /// Asks the question and reports whether the answer was yes.
    fn confirm(&self, question: &str) -> bool;
}

/// A prompter that reads the answer from the process's own standard input.
#[derive(Debug, Default)]
pub struct StdinPrompter;

impl Prompter for StdinPrompter {
    fn confirm(&self, question: &str) -> bool {
        eprint!("{question} ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && answer.trim().eq_ignore_ascii_case("y")
    }
}

/// Writes a provider's fixed help record to standard error, exactly as it is
/// stated, never improvised.
fn print_help(help: &ProviderHelp) {
    eprintln!("{}: {}", help.provider, help.unlocks);
    for (index, step) in help.steps.iter().enumerate() {
        eprintln!("  {}. {step}", index + 1);
    }
    eprintln!("  put it in: {}", help.placement);
    eprintln!("  verify with: {}", help.verification);
    eprintln!("  scope: {}", help.scope);
}

/// Renders a duration the way a person reads it, in whole seconds or minutes.
fn human_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds} seconds")
    } else {
        format!("{} minutes", seconds / 60)
    }
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
    store: &'a dyn CredentialStore,
    observer: &'a dyn Observer,
    sequence: &'a Sequence,
    prompter: &'a dyn Prompter,
    declined: Mutex<HashSet<String>>,
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
    #[expect(
        clippy::too_many_arguments,
        reason = "a policy is decided by its settings, its streams, whether terms were asserted, each place a credential is looked for, and what asks the user a question"
    )]
    pub fn new(
        settings: Settings,
        transfer: &TransferFlags,
        streams: Streams,
        accepted_terms: bool,
        environment: &'a dyn Environment,
        store: &'a dyn CredentialStore,
        observer: &'a dyn Observer,
        sequence: &'a Sequence,
        prompter: &'a dyn Prompter,
    ) -> Self {
        let limits = crate::settings::limits_for(&settings);
        Self {
            settings,
            verification: verification_of(transfer),
            durability: durability_of(transfer),
            limits,
            streams,
            accepted_terms,
            environment,
            store,
            observer,
            sequence,
            prompter,
            declined: Mutex::new(HashSet::new()),
        }
    }

    fn emit(&self, payload: EventPayload) {
        self.observer.emit(&Event::new(self.sequence, payload));
    }

    fn found(host: &Host, origin: CredentialOrigin, secrets: Secrets) -> Credential {
        Credential {
            host: host.clone(),
            origin,
            secrets,
        }
    }

    /// Reads the signing keys the host-scoped variables hold, which is the
    /// first tier and wins over every other.
    ///
    /// # Errors
    ///
    /// Fails with `policy.credential_invalid` when an access key is set without
    /// the region a signature is computed over.
    fn host_scoped_keys(&self, host: &Host) -> Result<Option<SigningKeys>, Error> {
        let Some(access_key) = self
            .environment
            .get(&host_variable("FETCHLOOM_ACCESS_KEY_", host))
        else {
            return Ok(None);
        };
        let Some(secret_key) = self
            .environment
            .get(&host_variable("FETCHLOOM_SECRET_KEY_", host))
        else {
            return Err(missing_half(
                host,
                &host_variable("FETCHLOOM_SECRET_KEY_", host),
                "a secret key",
            ));
        };
        let Some(region) = self
            .environment
            .get(&host_variable("FETCHLOOM_REGION_", host))
        else {
            return Err(missing_region(
                host,
                &host_variable("FETCHLOOM_REGION_", host),
            ));
        };
        Ok(Some(SigningKeys {
            access_key,
            secret_key: Secret::new(secret_key),
            session_token: self
                .environment
                .get(&host_variable("FETCHLOOM_SESSION_TOKEN_", host))
                .map(Secret::new),
            region,
        }))
    }

    /// Reads the signing keys a provider's own convention holds, which is the
    /// third tier and is asked only after the two above answered nothing.
    ///
    /// # Errors
    ///
    /// Fails with `policy.credential_invalid` when the provider's own variables
    /// name a key without the region a signature is computed over.
    fn helper_keys(&self, host: &Host) -> Result<Option<(SigningKeys, String)>, Error> {
        if !fetchloom_sources::signs_requests(host.as_str()) {
            return Ok(None);
        }
        let Some(access_key) = self.environment.get("AWS_ACCESS_KEY_ID") else {
            return Ok(None);
        };
        let Some(secret_key) = self.environment.get("AWS_SECRET_ACCESS_KEY") else {
            return Err(missing_half(host, "AWS_SECRET_ACCESS_KEY", "a secret key"));
        };
        let Some(region) = self
            .environment
            .get("AWS_REGION")
            .or_else(|| self.environment.get("AWS_DEFAULT_REGION"))
        else {
            return Err(missing_region(host, "AWS_REGION"));
        };
        Ok(Some((
            SigningKeys {
                access_key,
                secret_key: Secret::new(secret_key),
                session_token: self.environment.get("AWS_SESSION_TOKEN").map(Secret::new),
                region,
            },
            "the AWS environment variables".to_owned(),
        )))
    }
}

/// Returns the failure a half-set signing credential states.
fn missing_half(host: &Host, variable: &str, what: &str) -> Error {
    Error::new(
        ErrorKind::PolicyCredentialInvalid,
        format!(
            "set {variable} as well, because {host} is reached with a signing credential and an access key without {what} signs nothing"
        ),
    )
    .with_source(host.as_str())
}

/// Returns the failure a signing credential with no region states, which is
/// never guessed because a signature is computed over one.
fn missing_region(host: &Host, variable: &str) -> Error {
    Error::new(
        ErrorKind::PolicyCredentialInvalid,
        format!(
            "set {variable} to the region {host} serves from, because a signature is computed over a region and a guessed one is refused by the source with an error you cannot act on"
        ),
    )
    .with_source(host.as_str())
}

/// This platform's own credential store, which is the second place a run looks.
pub trait CredentialStore: Send + Sync {
    /// Reads the token the store holds for a host.
    ///
    /// # Errors
    ///
    /// Fails with `policy.credential_invalid` when the store is present and
    /// cannot be read.
    fn token(&self, host: &Host) -> Result<Option<String>, Error>;

    /// Names where the store keeps a credential, without the secret.
    fn describe(&self) -> String;
}

/// The credential store this platform ships.
#[derive(Debug, Default)]
pub struct NativeCredentialStore;

impl CredentialStore for NativeCredentialStore {
    fn token(&self, host: &Host) -> Result<Option<String>, Error> {
        let configuration = crate::config::user_config_directory().unwrap_or_default();
        fetchloom_platform::stored_token(host.as_str(), &configuration)
    }

    fn describe(&self) -> String {
        if cfg!(windows) {
            "the Windows Credential Manager".to_owned()
        } else {
            "the credentials file beside the user configuration".to_owned()
        }
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
        self.settings.concurrency.value
    }

    fn per_host(&self) -> Option<NonZeroU32> {
        self.settings.per_host.value
    }

    fn bandwidth(&self) -> Option<Bandwidth> {
        self.settings.bandwidth.value
    }

    fn aggressive(&self) -> bool {
        self.settings.aggressive.value
    }

    fn adapts(&self) -> bool {
        !self.settings.deterministic_io.value
    }

    fn io(&self) -> IoMode {
        match self.settings.io.value {
            IoChoice::Auto => IoMode::Auto,
            IoChoice::Buffered => IoMode::Buffered,
            IoChoice::Uncached => IoMode::Uncached,
        }
    }

    fn accepts(&self, class: TrustClass) -> bool {
        match class {
            TrustClass::Verified | TrustClass::Corroborated | TrustClass::Tofu => true,
            TrustClass::Unverified => self.verification == VerificationPolicy::Never,
        }
    }

    fn credential(&self, host: &Host, necessity: Necessity) -> Result<Option<Credential>, Error> {
        if let Some(value) = self.environment.get(&token_variable(host)) {
            return Ok(Some(Self::found(
                host,
                CredentialOrigin::Environment,
                Secrets::Bearer {
                    value: Secret::new(value),
                },
            )));
        }
        if let Some(keys) = self.host_scoped_keys(host)? {
            return Ok(Some(Self::found(
                host,
                CredentialOrigin::Environment,
                Secrets::Signing { keys },
            )));
        }
        if let Some(value) = self.store.token(host)? {
            return Ok(Some(Self::found(
                host,
                CredentialOrigin::PlatformStore,
                Secrets::Bearer {
                    value: Secret::new(value),
                },
            )));
        }
        if let Some((keys, _from)) = self.helper_keys(host)? {
            return Ok(Some(Self::found(
                host,
                CredentialOrigin::ProviderHelper,
                Secrets::Signing { keys },
            )));
        }
        match necessity {
            Necessity::Optional => Ok(None),
            Necessity::Required => {
                self.emit(EventPayload::CredentialRequired {
                    provider: host.to_string(),
                });
                print_help(&fetchloom_sources::help_for(
                    host.as_str(),
                    Necessity::Required,
                ));
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
        if self.already_declined(&help.provider) {
            return Ok(None);
        }
        self.emit(EventPayload::CredentialOffer {
            provider: help.provider.clone(),
        });
        crate::hint::record(|observed| {
            observed.projected_gain = Some(projected_gain);
            observed.placement = Some(help.placement.clone());
        });
        eprintln!(
            "a credential for {} would save about {} on this transfer",
            help.provider,
            human_duration(projected_gain)
        );
        if !self.streams.can_prompt() {
            eprintln!(
                "this run cannot ask, so it is using the alternative source instead. To take the \
                 faster one next time:"
            );
            print_help(help);
            self.emit(EventPayload::CredentialDeclined {
                provider: help.provider.clone(),
            });
            self.remember_declined(&help.provider);
            return Ok(None);
        }
        print_help(help);
        if self.prompter.confirm("set this up now? [y/N]") {
            return Ok(None);
        }
        self.emit(EventPayload::CredentialDeclined {
            provider: help.provider.clone(),
        });
        self.remember_declined(&help.provider);
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
            if let Some(url) = license.url.as_deref() {
                eprintln!("the recorded terms are at {url}");
            }
            if self.prompter.confirm("accept the recorded terms? [y/N]") {
                return Ok(Acceptance::Asserted);
            }
            return Ok(Acceptance::Withheld);
        }
        Err(Error::new(
            ErrorKind::PolicyTermsRequired,
            "run the command again with --yes to assert that you accept the recorded terms",
        ))
    }
}

impl CommandLinePolicy<'_> {
    /// Reports whether this provider's optional credential was already
    /// declined earlier in this run.
    fn already_declined(&self, provider: &str) -> bool {
        self.declined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(provider)
    }

    /// Records that this provider's optional credential was declined, so it
    /// is never offered again in this run.
    fn remember_declined(&self, provider: &str) {
        crate::hint::record(|observed| {
            observed.declined_provider = Some(provider.to_owned());
        });
        self.declined
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(provider.to_owned());
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
