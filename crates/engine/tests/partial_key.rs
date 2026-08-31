//! Contract tests for the key a partial, its lease, and its records are named
//! by.

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use std::time::Duration;

use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{SourceIdentity, SourceMetadata};

fn metadata(identity: SourceIdentity) -> SourceMetadata {
    SourceMetadata {
        location: SafeUrl::new("https://example.invalid/object"),
        host: Host::new("example.invalid"),
        size: Some(4096),
        content: None,
        interop: None,
        identity,
        last_modified: None,
        supports_ranges: true,
        time_to_first_byte: Duration::from_millis(1),
        retry_after: None,
    }
}

#[test]
fn of_content_names_the_partial_by_the_digest_and_expects_it() {
    let digest = hash_bytes(b"an object the run already knows the digest of");
    let key = PartialKey::of_content(digest);

    assert_eq!(key.name(), digest);
    assert_eq!(key.expected(), Some(digest));
}

#[test]
fn of_source_expects_nothing() {
    let key = PartialKey::of_source(&metadata(SourceIdentity::StrongValidator(
        "\"one\"".to_owned(),
    )));

    assert_eq!(key.expected(), None);
}

#[test]
fn of_source_names_the_same_location_host_and_identity_the_same() {
    let identity = SourceIdentity::StrongValidator("\"one\"".to_owned());
    let first = PartialKey::of_source(&metadata(identity.clone()));
    let second = PartialKey::of_source(&metadata(identity));

    assert_eq!(
        first.name(),
        second.name(),
        "the same location, host, and identity named two different partials"
    );
}

#[test]
fn of_source_names_a_changed_identity_differently() {
    let one = PartialKey::of_source(&metadata(SourceIdentity::StrongValidator(
        "\"one\"".to_owned(),
    )));
    let two = PartialKey::of_source(&metadata(SourceIdentity::StrongValidator(
        "\"two\"".to_owned(),
    )));

    assert_ne!(
        one.name(),
        two.name(),
        "a changed identity named the same partial as before"
    );
}

#[test]
fn of_source_names_a_changed_host_differently() {
    let mut changed = metadata(SourceIdentity::None);
    changed.host = Host::new("other.invalid");
    let base = PartialKey::of_source(&metadata(SourceIdentity::None));
    let other = PartialKey::of_source(&changed);

    assert_ne!(
        base.name(),
        other.name(),
        "a changed host named the same partial as before"
    );
}

#[test]
fn of_source_names_a_changed_location_differently() {
    let mut changed = metadata(SourceIdentity::None);
    changed.location = SafeUrl::new("https://example.invalid/other");
    let base = PartialKey::of_source(&metadata(SourceIdentity::None));
    let other = PartialKey::of_source(&changed);

    assert_ne!(
        base.name(),
        other.name(),
        "a changed location named the same partial as before"
    );
}
