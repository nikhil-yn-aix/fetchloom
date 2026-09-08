//! Contract tests over the engine's public surface.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use sha2 as _;
use toml as _;

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use fetchloom_engine::digest::{Algorithm, ContentDigest, Digest, InteropDigest, ParseDigestError};
use fetchloom_engine::error::{Error, ErrorKind, Layer};
use fetchloom_engine::event::{EVENT_NAMES, Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::redact::{REDACTED, SafeUrl, Secret};
use fetchloom_engine::threads::{BudgetOrigin, ThreadBudget};
use fetchloom_engine::timestamp::Timestamp;
use fetchloom_engine::tree::{EntryPath, EntryPathError, Mode};

fn nonzero(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

#[test]
fn exit_codes_are_the_numbers_the_contract_lists() {
    assert_eq!(ExitCode::Success.code(), 0);
    assert_eq!(ExitCode::Usage.code(), 2);
    assert_eq!(ExitCode::Resolution.code(), 10);
    assert_eq!(ExitCode::Network.code(), 20);
    assert_eq!(ExitCode::Integrity.code(), 30);
    assert_eq!(ExitCode::Policy.code(), 40);
    assert_eq!(ExitCode::Resource.code(), 50);
    assert_eq!(ExitCode::Destination.code(), 60);
    assert_eq!(ExitCode::Archive.code(), 70);
    assert_eq!(ExitCode::Cache.code(), 80);
    assert_eq!(ExitCode::Cancelled.code(), 130);
}

#[test]
fn every_layer_maps_to_the_exit_code_that_reports_it() {
    assert_eq!(ExitCode::from(Layer::Resolve), ExitCode::Resolution);
    assert_eq!(ExitCode::from(Layer::Transfer), ExitCode::Network);
    assert_eq!(ExitCode::from(Layer::Verify), ExitCode::Integrity);
    assert_eq!(ExitCode::from(Layer::Extract), ExitCode::Archive);
    assert_eq!(ExitCode::from(Layer::Materialize), ExitCode::Destination);
    assert_eq!(ExitCode::from(Layer::Cache), ExitCode::Cache);
    assert_eq!(ExitCode::from(Layer::Policy), ExitCode::Policy);
    assert_eq!(ExitCode::from(Layer::Resource), ExitCode::Resource);
}

#[test]
fn every_error_kind_the_contract_lists_exists_with_its_layer() {
    let expected: Vec<(&str, Layer)> = vec![
        ("reference.unresolved", Layer::Resolve),
        ("manifest.invalid", Layer::Resolve),
        ("alias.unstable", Layer::Resolve),
        ("network.timeout", Layer::Transfer),
        ("network.refused", Layer::Transfer),
        ("network.status", Layer::Transfer),
        ("network.tls", Layer::Transfer),
        ("source.unsupported_range", Layer::Transfer),
        ("source.identity_changed", Layer::Transfer),
        ("integrity.mismatch", Layer::Verify),
        ("integrity.truncated", Layer::Verify),
        ("integrity.range_mismatch", Layer::Verify),
        ("archive.unsafe_path", Layer::Extract),
        ("archive.link_escape", Layer::Extract),
        ("archive.collision", Layer::Extract),
        ("archive.bomb", Layer::Extract),
        ("archive.unsupported", Layer::Extract),
        ("destination.conflict", Layer::Materialize),
        ("destination.modified", Layer::Materialize),
        ("destination.foreign", Layer::Materialize),
        ("destination.unrepresentable", Layer::Materialize),
        ("destination.cross_volume", Layer::Materialize),
        ("cache.locked", Layer::Cache),
        ("cache.corrupt", Layer::Cache),
        ("cache.format_mismatch", Layer::Cache),
        ("cache.cross_volume", Layer::Cache),
        ("cache.locking_unsupported", Layer::Cache),
        ("policy.offline", Layer::Policy),
        ("policy.credential_missing", Layer::Policy),
        ("policy.credential_invalid", Layer::Policy),
        ("policy.terms_required", Layer::Policy),
        ("policy.trust_refused", Layer::Policy),
        ("resource.disk", Layer::Resource),
        ("resource.limit", Layer::Resource),
    ];
    let found: Vec<(&str, Layer)> = ErrorKind::ALL
        .iter()
        .map(|kind| (kind.label(), kind.layer()))
        .collect();
    assert_eq!(found, expected);
}

#[test]
fn every_error_kind_carries_the_fields_a_report_needs() {
    for kind in ErrorKind::ALL {
        let error = Error::new(kind, "do the thing")
            .with_dataset("silesia")
            .with_artifact("corpus")
            .with_source("https://host/x.tar.zst")
            .with_attempts(3)
            .with_retryable(true);
        assert_eq!(error.kind(), kind);
        assert_eq!(error.layer(), kind.layer());
        assert_eq!(error.dataset(), Some("silesia"));
        assert_eq!(error.artifact(), Some("corpus"));
        assert_eq!(
            error.source_location().map(SafeUrl::as_str),
            Some("https://host/x.tar.zst")
        );
        assert_eq!(error.attempts(), 3);
        assert!(error.retryable());
        assert_eq!(error.next_action(), "do the thing");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one payload per contracted event name, and the contract lists thirty-four"
)]
fn every_event_name_the_contract_lists_is_produced_by_a_payload() {
    let listed: BTreeSet<&str> = EVENT_NAMES.iter().copied().collect();
    assert_eq!(listed.len(), EVENT_NAMES.len());

    let error = Error::new(ErrorKind::IntegrityMismatch, "refetch the object");
    let source = SafeUrl::new("https://host/x");
    let digest = ContentDigest::from_bytes([0u8; 32]);
    let produced: BTreeSet<&str> = vec![
        EventPayload::RunStart,
        EventPayload::RunEnd { duration_ms: 1 },
        EventPayload::ResolveStart,
        EventPayload::ResolveAlias {
            from: "a".to_owned(),
            to: "b".to_owned(),
        },
        EventPayload::ResolveEnd { duration_ms: 1 },
        EventPayload::PlanReady,
        EventPayload::CacheHit { digest },
        EventPayload::CacheMiss { digest },
        EventPayload::CacheWait { digest },
        EventPayload::CredentialRequired {
            provider: "p".to_owned(),
        },
        EventPayload::CredentialOffer {
            provider: "p".to_owned(),
        },
        EventPayload::CredentialDeclined {
            provider: "p".to_owned(),
        },
        EventPayload::ListingStart {
            source: source.clone(),
        },
        EventPayload::ListingSkipped { count: 1 },
        EventPayload::ListingEnd {
            entries: 1,
            duration_ms: 1,
        },
        EventPayload::SourceProbe {
            source: source.clone(),
        },
        EventPayload::SourceSelected {
            source: source.clone(),
            reason: "fastest".to_owned(),
        },
        EventPayload::SourceFailover {
            from: source.clone(),
            to: source.clone(),
            reason: "stalled".to_owned(),
        },
        EventPayload::TransferStart {
            host: fetchloom_engine::reference::Host::new("host.example"),
            source: source.clone(),
            expected_bytes: None,
        },
        EventPayload::TransferProgress { bytes: 1 },
        EventPayload::TransferRetry {
            host: fetchloom_engine::reference::Host::new("host.example"),
            attempt: 1,
            reason: "timeout".to_owned(),
        },
        EventPayload::TransferResume {
            rung: fetchloom_engine::resume::ResumeRung::Outboard,
            bytes_kept: 1,
        },
        EventPayload::TransferEnd {
            host: fetchloom_engine::reference::Host::new("host.example"),
            bytes: 1,
            duration_ms: 1,
        },
        EventPayload::VerifyStart,
        EventPayload::VerifyRange { start: 0, end: 1 },
        EventPayload::VerifyMismatch {
            error: error.clone(),
        },
        EventPayload::VerifyEnd {
            bytes: 1,
            duration_ms: 1,
        },
        EventPayload::ExtractStart,
        EventPayload::ExtractReject {
            path: "a".to_owned(),
            error: error.clone(),
        },
        EventPayload::ExtractEnd {
            entries: 1,
            bytes: 1,
            duration_ms: 1,
        },
        EventPayload::PublishCommit,
        EventPayload::ReconcileOutcomeReached {
            path: "a".to_owned(),
            outcome: fetchloom_engine::reconcile::ReconcileOutcome::Unchanged,
        },
        EventPayload::MergeResolutionReached {
            path: "a".to_owned(),
            resolution: fetchloom_engine::merge::Resolution::Conflict,
        },
        EventPayload::Degrade {
            requested: "clone".to_owned(),
            used: "copy".to_owned(),
            reason: "the volume has no block cloning".to_owned(),
        },
        EventPayload::Failure { error },
    ]
    .iter()
    .map(EventPayload::name)
    .collect();

    assert_eq!(produced, listed);
}

#[test]
fn an_event_is_written_with_its_name_and_a_monotonic_sequence_number() {
    let sequence = Sequence::new();
    let first = Event::new(&sequence, EventPayload::RunStart).with_dataset("silesia");
    let second = Event::new(&sequence, EventPayload::RunEnd { duration_ms: 7 });
    assert_eq!(first.seq(), 0);
    assert_eq!(second.seq(), 1);

    let written = serde_json::to_value(&first).unwrap();
    assert_eq!(written["event"], "run.start");
    assert_eq!(written["seq"], 0);
    assert_eq!(written["dataset"], "silesia");
    assert!(written["artifact"].is_null());
}

#[test]
fn a_userinfo_component_never_reaches_an_event() {
    let url = SafeUrl::new("https://alice:hunter2@host/data.tar?X-Amz-Signature=deadbeef");
    let written = url.as_str();
    assert!(!written.contains("hunter2"), "{written}");
    assert!(!written.contains("alice"), "{written}");
    assert!(!written.contains("deadbeef"), "{written}");
    assert_eq!(
        written,
        "https://[redacted]@host/data.tar?X-Amz-Signature=[redacted]"
    );
}

#[test]
fn redaction_is_the_fixed_text_and_survives_a_round_trip() {
    assert_eq!(REDACTED, "[redacted]");
    let once = SafeUrl::new("https://user:pass@host/x?sig=abc");
    let twice = SafeUrl::new(once.as_str());
    assert_eq!(once, twice);

    let written = serde_json::to_string(&once).unwrap();
    let read: SafeUrl = serde_json::from_str(&written).unwrap();
    assert_eq!(read, once);
}

#[test]
fn a_secret_is_written_as_the_fixed_text_and_never_as_itself() {
    let secret = Secret::new("hunter2".to_owned());
    assert_eq!(format!("{secret}"), REDACTED);
    assert_eq!(format!("{secret:?}"), REDACTED);
    assert_eq!(serde_json::to_string(&secret).unwrap(), "\"[redacted]\"");
    assert_eq!(secret.expose(), "hunter2");
}

#[test]
fn a_digest_is_written_as_its_algorithm_and_lowercase_hex() {
    let digest = Digest::new(Algorithm::Blake3, [0xabu8; 32]);
    let text = format!("blake3:{}", "ab".repeat(32));
    assert_eq!(digest.to_string(), text);
    assert_eq!(text.parse::<Digest>().unwrap(), digest);
}

#[test]
fn a_digest_string_that_breaks_a_rule_is_rejected() {
    assert_eq!(
        "deadbeef".parse::<Digest>(),
        Err(ParseDigestError::MissingAlgorithm)
    );
    assert_eq!(
        format!("md5:{}", "ab".repeat(16)).parse::<Digest>(),
        Err(ParseDigestError::UnknownAlgorithm)
    );
    assert_eq!(
        "blake3:abcd".parse::<Digest>(),
        Err(ParseDigestError::MalformedHex)
    );
    assert_eq!(
        format!("blake3:{}", "AB".repeat(32)).parse::<Digest>(),
        Err(ParseDigestError::MalformedHex)
    );
}

#[test]
fn a_digest_domain_refuses_the_wrong_algorithm() {
    let blake3 = Digest::new(Algorithm::Blake3, [0u8; 32]);
    let sha256 = Digest::new(Algorithm::Sha256, [0u8; 32]);
    assert!(ContentDigest::try_from(blake3).is_ok());
    assert!(ContentDigest::try_from(sha256).is_err());
    assert!(InteropDigest::try_from(sha256).is_ok());
    assert!(InteropDigest::try_from(blake3).is_err());
}

#[test]
fn an_entry_path_carries_no_form_a_tree_digest_cannot_reproduce() {
    assert!(EntryPath::new("a/b/c.txt").is_ok());
    assert_eq!(EntryPath::new(""), Err(EntryPathError::Empty));
    assert_eq!(EntryPath::new("/a"), Err(EntryPathError::Absolute));
    assert_eq!(EntryPath::new("a/"), Err(EntryPathError::TrailingSeparator));
    assert_eq!(EntryPath::new("a//b"), Err(EntryPathError::EmptyComponent));
    assert_eq!(
        EntryPath::new("a/../b"),
        Err(EntryPathError::RelativeComponent)
    );
    assert_eq!(
        EntryPath::new("a/./b"),
        Err(EntryPathError::RelativeComponent)
    );
    assert_eq!(EntryPath::new("a\\b"), Err(EntryPathError::Backslash));
    assert_eq!(
        EntryPath::new("a\nb"),
        Err(EntryPathError::ControlCharacter)
    );
}

#[test]
fn a_mode_reduces_to_the_only_two_a_tree_digest_records() {
    assert_eq!(Mode::reduce(0o644).bits(), 0o644);
    assert_eq!(Mode::reduce(0o600).bits(), 0o644);
    assert_eq!(Mode::reduce(0o755).bits(), 0o755);
    assert_eq!(Mode::reduce(0o700).bits(), 0o755);
    assert_eq!(Mode::reduce(0o111).bits(), 0o755);
    assert_eq!(Mode::reduce(0o4755).bits(), 0o755);
}

#[test]
fn a_thread_ceiling_above_the_detected_budget_is_clamped_and_reported() {
    let detected = nonzero(4);
    let clamped = ThreadBudget::resolve(detected, Some(nonzero(16)));
    assert_eq!(clamped.threads(), detected);
    assert_eq!(clamped.origin(), BudgetOrigin::Clamped);

    let honored = ThreadBudget::resolve(detected, Some(nonzero(2)));
    assert_eq!(honored.threads(), nonzero(2));
    assert_eq!(honored.origin(), BudgetOrigin::Requested);

    let none = ThreadBudget::resolve(detected, None);
    assert_eq!(none.threads(), detected);
    assert_eq!(none.origin(), BudgetOrigin::Detected);
}

#[test]
fn a_timestamp_is_written_and_read_as_one_form() {
    let text = "2026-08-29T04:11:02Z";
    let timestamp: Timestamp = text.parse().unwrap();
    assert_eq!(timestamp.to_string(), text);
    assert_eq!(
        "1970-01-01T00:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .to_string(),
        "1970-01-01T00:00:00Z"
    );
    assert!("2026-08-29 04:11:02".parse::<Timestamp>().is_err());
}

const DEFINITION: &str = "error.rs";

const TESTS_BEGIN: &str = "\nmod tests {";

fn production_source() -> String {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    let mut collected = String::new();
    let mut pending = vec![workspace.join("crates")];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "tests") {
                    continue;
                }
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|kind| kind != "rs")
                || path.file_name().is_some_and(|name| name == DEFINITION)
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            collected.push_str(text.split(TESTS_BEGIN).next().unwrap_or_default());
        }
    }
    collected
}

#[test]
fn every_error_kind_appears_in_production_source() {
    let source = production_source();
    assert!(
        source.len() > 100_000,
        "the walk read {} bytes of source, so it proves nothing",
        source.len()
    );
    let absent: Vec<&str> = ErrorKind::ALL
        .iter()
        .filter(|kind| !source.contains(&format!("ErrorKind::{kind:?}")))
        .map(|kind| kind.label())
        .collect();
    assert!(
        absent.is_empty(),
        "these kinds are defined and their name is written nowhere outside the tests, which is a \
         grep over the source and not a proof that any run reaches them: {absent:?}"
    );
}
