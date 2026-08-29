//! Contract tests over what a run is allowed to do.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::collections::HashMap;

use clap as _;
use clap_complete as _;
use fetchloom_cache as _;
use fetchloom_platform as _;
use serde as _;
use serde_json as _;
use tempfile as _;
use toml as _;

use fetchloom_cli::config::Discovered;
use fetchloom_cli::policy::{CommandLinePolicy, token_variable};
use fetchloom_cli::settings::{self, Environment};
use fetchloom_cli::surface::GlobalFlags;
use fetchloom_cli::terminal::Streams;
use fetchloom_engine::credential::{Necessity, ProviderHelp};
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::event::Sequence;
use fetchloom_engine::license::{Acceptance, License};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_faults::RecordingObserver;

#[derive(Default)]
struct FakeEnvironment(HashMap<String, String>);

impl FakeEnvironment {
    fn with(pairs: &[(&str, &str)]) -> Self {
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

fn settings_for(flags: &GlobalFlags, environment: &dyn Environment) -> settings::Settings {
    settings::resolve_all(flags, &Discovered::default(), environment)
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        true,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
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
        NON_INTERACTIVE,
        false,
        &environment,
        &observer,
        &sequence,
    );

    assert!(policy.offline());
    assert_eq!(policy.limits().retry_attempts, 5);
}
