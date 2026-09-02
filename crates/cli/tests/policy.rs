//! Contract tests over what a run is allowed to do.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use tempfile as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_cli::config::Discovered;
use fetchloom_cli::policy::{CommandLinePolicy, CredentialStore, Prompter};
use fetchloom_cli::settings::{self, Environment};
use fetchloom_cli::surface::{GlobalFlags, TransferFlags};
use fetchloom_cli::terminal::Streams;
use fetchloom_engine::credential::token_variable;
use fetchloom_engine::credential::{Credential, CredentialOrigin};
use fetchloom_engine::credential::{Necessity, ProviderHelp};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::license::{Acceptance, License};
use fetchloom_engine::limits::{Bandwidth, Limits};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::policy::{IoMode, Policy};
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_faults::RecordingObserver;
use std::num::NonZeroU32;
use std::path::Path;

mod support;

use support::FakeEnvironment;

fn settings_for(flags: &GlobalFlags, environment: &dyn Environment) -> settings::Settings {
    settings::resolve_all(
        flags,
        &fetchloom_cli::surface::TransferFlags::default(),
        &Discovered::default(),
        environment,
    )
    .expect("no level supplied a value this build cannot read")
}

const NON_INTERACTIVE: Streams = Streams {
    stdout: false,
    stderr: false,
    stdin: false,
};

const INTERACTIVE: Streams = Streams {
    stdout: true,
    stderr: true,
    stdin: true,
};

/// A prompter that fails the test if it is ever asked anything, for the runs
/// that must never block on a question.
struct PanicPrompter;

impl Prompter for PanicPrompter {
    #[expect(
        clippy::panic,
        reason = "the assertion this test makes is that this is never reached"
    )]
    fn confirm(&self, question: &str) -> bool {
        panic!("a prompt was shown when none should have been: {question}");
    }
}

/// A prompter that always answers the same way.
struct FixedPrompter(bool);

impl Prompter for FixedPrompter {
    fn confirm(&self, _question: &str) -> bool {
        self.0
    }
}

fn accepting_license() -> License {
    License {
        spdx: Some("CC-BY-4.0".to_owned()),
        url: None,
        requires_acceptance: true,
    }
}

#[test]
fn a_required_prompt_in_a_non_interactive_run_is_a_policy_failure() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let error = policy.terms(&accepting_license()).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PolicyTermsRequired);
    assert_eq!(ExitCode::from(error.layer()).code(), 40);
}

#[test]
fn asserting_acceptance_on_the_command_line_needs_no_prompt() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        true,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    assert_eq!(
        policy.terms(&accepting_license()).unwrap(),
        Acceptance::Asserted
    );
}

#[test]
fn a_license_needing_no_acceptance_never_prompts() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    assert_eq!(
        policy.terms(&License::default()).unwrap(),
        Acceptance::Asserted
    );
}

#[test]
fn a_missing_required_credential_is_a_policy_failure() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let host = Host::new("example.invalid");
    let error = policy.credential(&host, Necessity::Required).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::PolicyCredentialMissing);
    assert_eq!(ExitCode::from(error.layer()).code(), 40);
}

#[test]
fn a_missing_optional_credential_does_not_stop_the_run() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let host = Host::new("example.invalid");
    assert!(
        policy
            .credential(&host, Necessity::Optional)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_credential_is_read_from_the_host_scoped_variable_and_never_reported() {
    let environment = FakeEnvironment::with(&[("FETCHLOOM_TOKEN_EXAMPLE_INVALID", "secret-value")]);
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let host = Host::new("example.invalid");
    let found = policy
        .credential(&host, Necessity::Required)
        .unwrap()
        .unwrap();
    assert_eq!(found.host, host);
    assert_eq!(found.value.expose(), "secret-value");

    let rendered = format!("{:?}", found.value);
    assert!(
        !rendered.contains("secret-value"),
        "debug leaked the secret"
    );
    let serialized = serde_json::to_string(&found).unwrap();
    assert!(
        !serialized.contains("secret-value"),
        "serialization leaked the secret"
    );
}

#[test]
fn the_token_variable_is_named_for_the_host() {
    assert_eq!(
        token_variable(&Host::new("example.invalid")),
        "FETCHLOOM_TOKEN_EXAMPLE_INVALID"
    );
    assert_eq!(
        token_variable(&Host::new("data.lab.edu")),
        "FETCHLOOM_TOKEN_DATA_LAB_EDU"
    );
}

#[test]
fn an_optional_credential_is_never_offered_without_a_terminal() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let help = ProviderHelp {
        provider: "Example".to_owned(),
        unlocks: "faster transfers".to_owned(),
        necessity: Necessity::Optional,
        steps: vec!["open the page".to_owned()],
        placement: "FETCHLOOM_TOKEN_EXAMPLE".to_owned(),
        verification: "fetchloom doctor".to_owned(),
        scope: "read".to_owned(),
    };
    let offered = policy
        .offer_credential(&help, std::time::Duration::from_secs(600))
        .unwrap();
    assert!(offered.is_none());
    assert!(observer.names().contains(&"credential.declined"));
    assert!(!observer.names().contains(&"credential.offer"));
}

#[test]
fn an_interactive_run_may_be_offered_a_credential() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &FixedPrompter(false),
    );

    let help = ProviderHelp {
        provider: "Example".to_owned(),
        unlocks: "faster transfers".to_owned(),
        necessity: Necessity::Optional,
        steps: vec!["open the page".to_owned()],
        placement: "FETCHLOOM_TOKEN_EXAMPLE".to_owned(),
        verification: "fetchloom doctor".to_owned(),
        scope: "read".to_owned(),
    };
    policy
        .offer_credential(&help, std::time::Duration::from_secs(600))
        .unwrap();
    assert!(observer.names().contains(&"credential.offer"));
    assert!(
        observer.names().contains(&"credential.declined"),
        "declining the prompt did not report the decline"
    );
}

#[test]
fn a_declined_optional_credential_is_not_offered_a_second_time_in_the_same_run() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &FixedPrompter(false),
    );

    let help = ProviderHelp {
        provider: "Example".to_owned(),
        unlocks: "faster transfers".to_owned(),
        necessity: Necessity::Optional,
        steps: vec!["open the page".to_owned()],
        placement: "FETCHLOOM_TOKEN_EXAMPLE".to_owned(),
        verification: "fetchloom doctor".to_owned(),
        scope: "read".to_owned(),
    };
    policy
        .offer_credential(&help, std::time::Duration::from_secs(600))
        .unwrap();
    assert_eq!(
        observer
            .names()
            .iter()
            .filter(|name| **name == "credential.offer")
            .count(),
        1,
        "the first offer was not recorded once"
    );

    policy
        .offer_credential(&help, std::time::Duration::from_secs(600))
        .unwrap();
    assert_eq!(
        observer
            .names()
            .iter()
            .filter(|name| **name == "credential.offer")
            .count(),
        1,
        "a declined credential was offered a second time in the same run"
    );
}

#[test]
fn settings_reach_the_policy() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let flags = GlobalFlags {
        offline: true,
        ..GlobalFlags::default()
    };
    let policy = CommandLinePolicy::new(
        settings_for(&flags, &environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    assert!(policy.offline());
    assert_eq!(policy.limits().retry_attempts, 5);
}

#[test]
fn a_gain_below_the_offer_threshold_produces_no_prompt_and_no_message() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), &environment),
        &TransferFlags::default(),
        INTERACTIVE,
        false,
        &environment,
        &EmptyStore,
        &observer,
        &sequence,
        &PanicPrompter,
    );

    let help = ProviderHelp {
        provider: "Example".to_owned(),
        unlocks: "faster transfers".to_owned(),
        necessity: Necessity::Optional,
        steps: vec!["open the page".to_owned()],
        placement: "FETCHLOOM_TOKEN_EXAMPLE".to_owned(),
        verification: "fetchloom doctor".to_owned(),
        scope: "read".to_owned(),
    };
    let offered = policy
        .offer_credential(&help, std::time::Duration::from_secs(30))
        .unwrap();

    assert!(offered.is_none());
    assert!(
        observer.names().is_empty(),
        "a gain below the threshold produced {:?}",
        observer.names()
    );
}

/// A policy that answers `offline` from itself and carries no settings at all,
/// so a refusal under it can only have come from the seam.
#[derive(Debug)]
struct SeamOnly {
    offline: bool,
    limits: Limits,
}

impl Policy for SeamOnly {
    fn offline(&self) -> bool {
        self.offline
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
    fn credential(
        &self,
        _host: &Host,
        _necessity: Necessity,
    ) -> Result<Option<Credential>, fetchloom_engine::error::Error> {
        Ok(None)
    }
    fn offer_credential(
        &self,
        _help: &ProviderHelp,
        _projected_gain: std::time::Duration,
    ) -> Result<Option<Credential>, fetchloom_engine::error::Error> {
        Ok(None)
    }
    fn terms(&self, _license: &License) -> Result<Acceptance, fetchloom_engine::error::Error> {
        Ok(Acceptance::Asserted)
    }
}

#[test]
fn the_seam_is_what_refuses_a_network_reference_offline() {
    let refusing = SeamOnly {
        offline: true,
        limits: Limits::default(),
    };
    let failed = fetchloom_cli::run::allowed_offline("https://example.invalid/object", &refusing)
        .expect_err("an offline policy let a network reference through");
    assert_eq!(failed.kind(), ErrorKind::PolicyOffline);
    assert_eq!(ExitCode::from(failed.kind().layer()), ExitCode::Policy);

    let permitting = SeamOnly {
        offline: false,
        limits: Limits::default(),
    };
    assert!(
        fetchloom_cli::run::allowed_offline("https://example.invalid/object", &permitting).is_ok(),
        "a policy that permits the network still refused"
    );
    assert!(
        fetchloom_cli::run::allowed_offline("file:///tmp/object", &refusing).is_ok(),
        "an offline policy refused a reference that needs no network"
    );
}

/// A store holding nothing, which is every platform before a user writes one.
struct EmptyStore;

impl CredentialStore for EmptyStore {
    fn token(&self, _host: &Host) -> Result<Option<String>, Error> {
        Ok(None)
    }

    fn describe(&self) -> String {
        "a store holding nothing".to_owned()
    }
}

/// A store that is present and refuses to be read, which is what a locked
/// credential manager does.
struct LockedStore;

impl CredentialStore for LockedStore {
    fn token(&self, _host: &Host) -> Result<Option<String>, Error> {
        Err(Error::new(
            ErrorKind::PolicyCredentialInvalid,
            "unlock the credential store and run the command again",
        ))
    }

    fn describe(&self) -> String {
        "a store that is locked".to_owned()
    }
}

/// A store holding one token for every host asked about.
struct FilledStore(&'static str);

impl CredentialStore for FilledStore {
    fn token(&self, _host: &Host) -> Result<Option<String>, Error> {
        Ok(Some(self.0.to_owned()))
    }

    fn describe(&self) -> String {
        "a store holding one token".to_owned()
    }
}

fn policy_with<'a>(
    environment: &'a FakeEnvironment,
    store: &'a dyn CredentialStore,
    observer: &'a RecordingObserver,
    sequence: &'a Sequence,
) -> CommandLinePolicy<'a> {
    CommandLinePolicy::new(
        settings_for(&GlobalFlags::default(), environment),
        &TransferFlags::default(),
        NON_INTERACTIVE,
        false,
        environment,
        store,
        observer,
        sequence,
        &PanicPrompter,
    )
}

#[test]
fn the_environment_variable_wins_over_the_platform_store() {
    let environment = FakeEnvironment::with(&[("FETCHLOOM_TOKEN_EXAMPLE_INVALID", "from-the-env")]);
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let store = FilledStore("from-the-store");
    let policy = policy_with(&environment, &store, &observer, &sequence);

    let found = policy
        .credential(&Host::new("example.invalid"), Necessity::Required)
        .expect("the lookup failed")
        .expect("no credential was found");

    assert_eq!(found.origin, CredentialOrigin::Environment);
    assert_eq!(found.value.expose(), "from-the-env");
}

#[test]
fn the_platform_store_is_asked_when_the_environment_holds_nothing() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let store = FilledStore("from-the-store");
    let policy = policy_with(&environment, &store, &observer, &sequence);

    let found = policy
        .credential(&Host::new("example.invalid"), Necessity::Required)
        .expect("the lookup failed")
        .expect("no credential was found");

    assert_eq!(found.origin, CredentialOrigin::PlatformStore);
    assert_eq!(found.value.expose(), "from-the-store");
}

#[test]
fn a_credential_store_that_is_present_and_locked_fails_rather_than_answering_nothing() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = policy_with(&environment, &LockedStore, &observer, &sequence);

    let refused = policy
        .credential(&Host::new("example.invalid"), Necessity::Optional)
        .expect_err("a locked store was read as a store holding nothing");

    assert_eq!(refused.kind(), ErrorKind::PolicyCredentialInvalid);
    assert_eq!(ExitCode::from(refused.layer()).code(), 40);
}

#[test]
fn a_required_credential_no_tier_holds_stops_the_run_and_says_where_to_put_one() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = policy_with(&environment, &EmptyStore, &observer, &sequence);

    let refused = policy
        .credential(&Host::new("example.invalid"), Necessity::Required)
        .expect_err("a required credential no tier holds was allowed to be absent");

    assert_eq!(refused.kind(), ErrorKind::PolicyCredentialMissing);
    assert!(
        refused
            .next_action()
            .contains("FETCHLOOM_TOKEN_EXAMPLE_INVALID"),
        "the failure does not name where to put a token: {}",
        refused.next_action()
    );
    assert!(
        observer
            .events()
            .iter()
            .any(|event| serde_json::to_string(&event)
                .unwrap_or_default()
                .contains("credential.required")),
        "credential.required was not emitted"
    );
}

#[test]
fn an_optional_credential_no_tier_holds_leaves_the_run_to_proceed() {
    let environment = FakeEnvironment::default();
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let policy = policy_with(&environment, &EmptyStore, &observer, &sequence);

    let found = policy
        .credential(&Host::new("example.invalid"), Necessity::Optional)
        .expect("an optional credential no tier holds is not a failure");

    assert!(found.is_none(), "a credential was invented");
}

#[test]
fn no_secret_appears_in_anything_the_credential_lookup_writes() {
    let secret = "s3cr3t-value-do-not-print";
    let environment = FakeEnvironment::with(&[("FETCHLOOM_TOKEN_EXAMPLE_INVALID", secret)]);
    let observer = RecordingObserver::new();
    let sequence = Sequence::new();
    let store = FilledStore("also-secret");
    let policy = policy_with(&environment, &store, &observer, &sequence);

    let found = policy
        .credential(&Host::new("example.invalid"), Necessity::Required)
        .expect("the lookup failed")
        .expect("no credential was found");

    assert_eq!(
        format!("{:?}", found.value),
        "[redacted]",
        "the secret survives being formatted"
    );
    let serialized = serde_json::to_string(&found).expect("the credential could not be serialized");
    assert!(
        !serialized.contains(secret),
        "the secret survives being serialized: {serialized}"
    );
    for event in observer.events() {
        let rendered = serde_json::to_string(&event).unwrap_or_default();
        assert!(
            !rendered.contains(secret),
            "the secret reached the event stream: {rendered}"
        );
    }
}
