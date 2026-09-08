//! What a credential carries, and what it never lets out.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the value that was absent is the message"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::credential::{Credential, CredentialOrigin, Secrets, SigningKeys};
use fetchloom_engine::redact::Secret;
use fetchloom_engine::reference::Host;

fn signing() -> Credential {
    Credential {
        host: Host::new("bucket.example"),
        origin: CredentialOrigin::Environment,
        secrets: Secrets::Signing {
            keys: SigningKeys {
                access_key: "AKIAIOSFODNN7EXAMPLE".to_owned(),
                secret_key: Secret::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned()),
                session_token: None,
                region: "us-east-1".to_owned(),
            },
        },
    }
}

#[test]
fn a_signing_credential_never_renders_its_secret_key() {
    let credential = signing();
    let debug = format!("{credential:?}");
    assert!(
        !debug.contains("wJalrXUtnFEMI"),
        "the secret key reached a debug rendering: {debug}"
    );
    assert!(
        debug.contains("[redacted]"),
        "the secret key was dropped rather than redacted: {debug}"
    );
}

#[test]
fn a_signing_credential_never_serializes_its_secret_key() {
    let credential = signing();
    let written = serde_json::to_string(&credential).unwrap();
    assert!(
        !written.contains("wJalrXUtnFEMI"),
        "the secret key reached a serialized credential: {written}"
    );
    assert!(
        written.contains("bucket.example"),
        "the host it is bound to was not recorded: {written}"
    );
}

#[test]
fn an_access_key_is_not_a_secret_and_the_session_token_is() {
    let mut credential = signing();
    if let Secrets::Signing { keys } = &mut credential.secrets {
        keys.session_token = Some(Secret::new("a-session-token-value".to_owned()));
    }
    let written = format!(
        "{credential:?}{}",
        serde_json::to_string(&credential).unwrap()
    );
    assert!(
        !written.contains("a-session-token-value"),
        "the session token reached an output: {written}"
    );
}

#[test]
fn a_bearer_credential_never_renders_its_value() {
    let credential = Credential {
        host: Host::new("host.example"),
        origin: CredentialOrigin::PlatformStore,
        secrets: Secrets::Bearer {
            value: Secret::new("super-secret-bearer".to_owned()),
        },
    };
    let written = format!(
        "{credential:?}{}",
        serde_json::to_string(&credential).unwrap()
    );
    assert!(
        !written.contains("super-secret-bearer"),
        "the bearer value reached an output: {written}"
    );
}
