//! Contract tests over the platform credential store.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use fetchloom_engine as _;
use fetchloom_faults as _;
#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use rustix as _;

#[cfg(unix)]
use std::path::Path;

use fetchloom_platform::stored_token;
use tempfile::TempDir;

#[test]
fn a_host_the_store_holds_nothing_for_answers_nothing() {
    let empty = TempDir::new().unwrap();
    let found = stored_token("nothing.invalid", empty.path())
        .expect("reading a store that holds nothing is not a failure");
    assert!(
        found.is_none(),
        "the store answered with a credential for a host it was never given one for"
    );
}

#[cfg(unix)]
fn written(configuration: &Path, contents: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;

    let file = configuration.join("credentials");
    std::fs::write(&file, contents).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_token_the_file_holds_is_read_for_the_host_it_names() {
    let configuration = TempDir::new().unwrap();
    written(
        configuration.path(),
        "example.invalid = \"the-token\"\nother.invalid = \"another\"\n",
        0o600,
    );

    assert_eq!(
        stored_token("example.invalid", configuration.path())
            .expect("the store could not be read")
            .as_deref(),
        Some("the-token")
    );
    assert_eq!(
        stored_token("other.invalid", configuration.path())
            .expect("the store could not be read")
            .as_deref(),
        Some("another")
    );
    assert_eq!(
        stored_token("absent.invalid", configuration.path()).expect("the store could not be read"),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_credential_file_another_user_can_read_is_refused_rather_than_used() {
    use fetchloom_engine::error::ErrorKind;

    let configuration = TempDir::new().unwrap();
    written(
        configuration.path(),
        "example.invalid = \"the-token\"\n",
        0o644,
    );

    let refused = stored_token("example.invalid", configuration.path())
        .expect_err("a credential file every user can read was used");

    assert_eq!(refused.kind(), ErrorKind::PolicyCredentialInvalid);
    assert!(
        refused.next_action().contains("600"),
        "the refusal does not say what mode to set: {}",
        refused.next_action()
    );
}

#[cfg(windows)]
#[test]
fn a_credential_the_platform_store_holds_is_read_for_the_host_it_names() {
    let host = "fetchloom-test.invalid";
    let target = format!("fetchloom:{host}");
    let empty = TempDir::new().unwrap();

    let written = std::process::Command::new("cmdkey")
        .arg(format!("/generic:{target}"))
        .arg("/user:fetchloom")
        .arg("/pass:the-token")
        .output()
        .expect("cmdkey could not be run");
    assert!(
        written.status.success(),
        "cmdkey refused to write the credential: {}",
        String::from_utf8_lossy(&written.stdout)
    );

    let found = stored_token(host, empty.path());

    let _ = std::process::Command::new("cmdkey")
        .arg(format!("/delete:{target}"))
        .output();

    assert_eq!(
        found.expect("the store could not be read").as_deref(),
        Some("the-token")
    );
}
