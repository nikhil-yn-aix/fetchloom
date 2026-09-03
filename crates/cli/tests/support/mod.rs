//! Shared helpers for the command line contract tests.

#![expect(
    dead_code,
    reason = "test helpers, where each test binary uses a different part"
)]

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::Path;

use fetchloom_cli::settings::Environment;
use fetchloom_engine::credential::{Credential, Necessity, ProviderHelp};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::Error;
use fetchloom_engine::license::{Acceptance, License};
use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::policy::{IoMode, Policy};
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::verification::VerificationPolicy;

/// An environment a test writes, so no test reads the real process environment.
#[derive(Default)]
pub struct FakeEnvironment(HashMap<String, String>);

impl FakeEnvironment {
    /// Builds an environment holding exactly these names.
    pub fn with(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }
}

impl Environment for FakeEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

/// A policy that resolves no credential, requires no acceptance, and refuses
/// nothing, for tests exercising a transfer rather than the policy seam.
#[derive(Debug, Default)]
pub struct NoCredentialPolicy {
    /// The bounds this policy states.
    pub limits: Limits,
}

impl Policy for NoCredentialPolicy {
    fn offline(&self) -> bool {
        false
    }

    fn limits(&self) -> &Limits {
        &self.limits
    }

    fn verification(&self) -> VerificationPolicy {
        VerificationPolicy::Fingerprint
    }

    fn durability(&self) -> DurabilityTier {
        DurabilityTier::Normal
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

    fn aggressive(&self) -> bool {
        false
    }

    fn adapts(&self) -> bool {
        true
    }

    fn io(&self) -> IoMode {
        IoMode::Auto
    }

    fn accepts(&self, _class: TrustClass) -> bool {
        true
    }

    fn credential(&self, _host: &Host, _necessity: Necessity) -> Result<Option<Credential>, Error> {
        Ok(None)
    }

    fn offer_credential(
        &self,
        _help: &ProviderHelp,
        _projected_gain: std::time::Duration,
    ) -> Result<Option<Credential>, Error> {
        Ok(None)
    }

    fn terms(&self, _license: &License) -> Result<Acceptance, Error> {
        Ok(Acceptance::Asserted)
    }
}

/// The prefix every environment name Fetchloom reads begins with.
const READS: &str = "FETCHLOOM_";

/// Returns the binary under test carrying nothing this machine happened to
/// export, so what a run reads is exactly what its own test wrote.
fn controlled() -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_fetchloom"));
    command.stdin(std::process::Stdio::null());
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with(READS) {
            command.env_remove(&name);
        }
    }
    command
}

/// Returns the binary under test with no inherited setting and no configuration
/// file, which is how every test that is not about configuration runs it.
pub fn fetchloom() -> std::process::Command {
    let mut command = controlled();
    command.arg("--no-config");
    command
}

/// Returns the binary under test with no inherited setting and configuration
/// discovery left on, for the tests whose subject is configuration.
pub fn fetchloom_reading_configuration() -> std::process::Command {
    controlled()
}
