//! Contract tests over what a volume and the processor can actually do.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use rustix as _;

mod support;

use std::num::NonZeroUsize;

use fetchloom_engine::capability::{Backing, CaseFolding, CopyMechanism, Normalization, Scanner};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::threads::BudgetOrigin;
use fetchloom_platform::NativePlatform;

#[test]
fn a_probe_reports_every_capability_the_contract_names() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let found = platform.volume_capabilities(scratch.path()).unwrap();

    assert!(
        found.max_component_length > 0,
        "a name has some length limit"
    );
    assert!(found.max_path_length > 0, "a path has some length limit");
    assert!(
        found.max_path_length >= found.max_component_length,
        "a path holds at least one component"
    );
    match found.scanner {
        Scanner::Absent => {}
        Scanner::Present { cost_ratio, .. } | Scanner::Unknown { cost_ratio } => {
            assert!(
                cost_ratio.is_finite() && cost_ratio > 0.0,
                "a measured cost is a real number"
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn a_platform_that_cannot_enumerate_never_claims_a_scanner_is_present() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let found = platform.volume_capabilities(scratch.path()).unwrap();

    assert!(
        !matches!(found.scanner, Scanner::Present { .. }),
        "a ratio cannot tell a scanner from a slow filesystem, so presence may not be claimed"
    );
}

#[test]
fn only_an_unknown_scanner_answer_emits_a_degrade_and_it_names_the_ratio() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let found = platform.volume_capabilities(scratch.path()).unwrap();
    let degradations = platform.take_degradations();
    let named = degradations
        .iter()
        .find(|degradation| degradation.requested == SCANNER_REQUEST);

    match found.scanner {
        Scanner::Unknown { cost_ratio } => {
            assert!(
                named.is_some(),
                "an unknown scanner answer degrades: {degradations:?}"
            );
            let named = named.unwrap();
            assert!(
                named.used.contains(&format!("{cost_ratio:.2}")),
                "the degrade names the measured ratio: {named:?}"
            );
            assert!(
                named.reason.contains("unknown"),
                "the degrade says the cause is unknown: {named:?}"
            );
        }
        Scanner::Present { .. } | Scanner::Absent => {
            assert!(
                named.is_none(),
                "an enumerated answer is not a degradation: {named:?}"
            );
        }
    }
}

/// What a scanner degrade names as the thing that was wanted.
const SCANNER_REQUEST: &str = "whether an on-access scanner inspects writes on this volume";

/// What a normalization degrade names as the thing that was wanted.
const NORMALIZATION_REQUEST: &str = "how this volume treats two spellings of one name";

#[test]
fn a_volume_that_refuses_the_probe_name_reports_normalization_as_unknown() {
    let scratch = support::scratch();
    let mut directories = vec![scratch.path().to_path_buf()];
    let held = support::scratch_on(support::Property::NoSparse);
    directories.extend(held.iter().map(|scratch| scratch.path().to_path_buf()));

    for directory in directories {
        let platform = NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        ));
        let found = platform.volume_capabilities(&directory).unwrap();
        let degradations = platform.take_degradations();
        let named = degradations
            .iter()
            .any(|degradation| degradation.requested == NORMALIZATION_REQUEST);

        let composed = String::from_utf8(vec![b'f', b'l', 0xc3, 0xa9]).unwrap();
        let path = directory.join(&composed);
        let refused = std::fs::File::create_new(&path).is_err();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            found.normalization == Normalization::Unknown,
            refused,
            "{} reported {:?} and creating the probe name was refused: {refused}",
            directory.display(),
            found.normalization
        );
        assert_eq!(
            named,
            refused,
            "{} degraded {named} and creating the probe name was refused: {refused}",
            directory.display()
        );
    }
}

#[test]
fn a_probe_leaves_nothing_behind() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    platform.volume_capabilities(scratch.path()).unwrap();
    assert!(
        support::is_empty(scratch.path()),
        "a probe left files behind: {:?}",
        support::names(scratch.path())
    );
}

#[test]
fn case_folding_is_reported_as_the_filesystem_behaves() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let reported = platform.volume_capabilities(scratch.path()).unwrap();

    let upper = scratch.path().join("FetchloomCaseCheck");
    let lower = scratch.path().join("fetchloomcasecheck");
    platform.create_file_exclusive(&upper).unwrap();
    let folds = platform.create_file_exclusive(&lower).is_err();

    let expected = if folds {
        CaseFolding::Folding
    } else {
        CaseFolding::Sensitive
    };
    assert_eq!(
        reported.case_folding, expected,
        "the probe disagreed with the filesystem"
    );
}

#[test]
fn normalization_is_reported_as_the_filesystem_behaves() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let reported = platform.volume_capabilities(scratch.path()).unwrap();

    let composed = String::from_utf8(vec![b'e', 0xc3, 0xa9]).unwrap();
    let decomposed = String::from_utf8(vec![b'e', 0x65, 0xcc, 0x81]).unwrap();
    let first = scratch.path().join(&composed);
    let second = scratch.path().join(&decomposed);
    platform.create_file_exclusive(&first).unwrap();
    let folds = platform.create_file_exclusive(&second).is_err();

    if folds {
        assert_ne!(
            reported.normalization,
            Normalization::Sensitive,
            "the filesystem folded two spellings and the probe called it sensitive"
        );
    } else {
        assert_eq!(
            reported.normalization,
            Normalization::Sensitive,
            "the filesystem kept two spellings apart and the probe did not"
        );
    }
}

#[test]
fn symlink_support_is_reported_as_the_platform_behaves() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let reported = platform.volume_capabilities(scratch.path()).unwrap();
    assert_eq!(
        reported.symlink,
        support::symlink_works(scratch.path()),
        "the probe disagreed with what creating a symbolic link actually does"
    );
}

#[test]
fn clone_support_is_reported_as_the_volume_behaves() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let reported = platform.volume_capabilities(scratch.path()).unwrap();

    let from = scratch.path().join("source");
    let to = scratch.path().join("target");
    support::write_file(&from, b"bytes to place somewhere else");
    let mechanism = platform.clone_or_copy(&from, &to).unwrap();

    if reported.clone {
        assert_eq!(
            mechanism,
            CopyMechanism::Clone,
            "the probe reported cloning and the copy did not clone"
        );
    } else {
        assert_eq!(
            mechanism,
            CopyMechanism::Copy,
            "the probe reported no cloning and the copy cloned anyway"
        );
    }
}

#[test]
fn a_local_volume_is_not_reported_as_network_backed() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let reported = platform.volume_capabilities(scratch.path()).unwrap();
    assert_ne!(
        reported.backing,
        Backing::Network,
        "a local temporary directory was called network backed"
    );
}

#[test]
fn a_capability_answer_is_the_same_every_time_it_is_asked() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let first = platform.volume_capabilities(scratch.path()).unwrap();
    let second = platform.volume_capabilities(scratch.path()).unwrap();

    assert_eq!(first.case_folding, second.case_folding);
    assert_eq!(first.normalization, second.normalization);
    assert_eq!(first.clone, second.clone);
    assert_eq!(first.sparse, second.sparse);
    assert_eq!(first.symlink, second.symlink);
    assert_eq!(first.hard_link, second.hard_link);
    assert_eq!(first.max_component_length, second.max_component_length);
    assert_eq!(first.max_path_length, second.max_path_length);
    assert_eq!(first.backing, second.backing);
    assert_eq!(
        std::mem::discriminant(&first.scanner),
        std::mem::discriminant(&second.scanner),
        "a volume was given one scanner answer once and another the next time"
    );
}

#[test]
fn the_thread_budget_is_the_detected_count_when_nothing_is_requested() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let found = platform.processor_capabilities(None);
    assert_eq!(found.budget.origin(), BudgetOrigin::Detected);
    assert_eq!(found.budget.threads(), found.budget.detected());
}

#[test]
fn a_thread_ceiling_below_the_detected_count_is_honored() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let detected = platform.processor_capabilities(None).budget.detected();
    if detected.get() < 2 {
        return;
    }
    let found = platform.processor_capabilities(NonZeroUsize::new(1));
    assert_eq!(found.budget.threads().get(), 1);
    assert_eq!(found.budget.origin(), BudgetOrigin::Requested);
}

#[test]
fn a_thread_ceiling_above_the_detected_count_is_clamped_and_reported() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let detected = platform.processor_capabilities(None).budget.detected();
    let asking = NonZeroUsize::new(detected.get() * 4 + 1).unwrap();
    let found = platform.processor_capabilities(Some(asking));
    assert_eq!(
        found.budget.threads(),
        detected,
        "a ceiling above the detected count was honored rather than clamped"
    );
    assert_eq!(found.budget.origin(), BudgetOrigin::Clamped);
}

#[test]
fn cloning_shares_blocks_on_a_volume_that_supports_it() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::Clone) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert!(
            reported.clone,
            "{} was built for block sharing and the probe reported none",
            scratch.path().display()
        );

        let from = scratch.path().join("source");
        let to = scratch.path().join("target");
        let bytes = vec![0x5au8; 1 << 20];
        support::write_file(&from, &bytes);
        let mechanism = platform.clone_or_copy(&from, &to).unwrap();

        assert_eq!(
            mechanism,
            CopyMechanism::Clone,
            "{} supports block sharing and the copy fell back",
            scratch.path().display()
        );
        assert_eq!(
            std::fs::read(&to).unwrap(),
            bytes,
            "a clone did not reproduce the bytes"
        );
    }
}

#[test]
fn case_folding_is_reported_on_a_case_sensitive_volume() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for directory in support::volume_directories(support::Property::CaseSensitive) {
        let reported = platform.volume_capabilities(&directory).unwrap();
        let upper = directory.join("FetchloomCaseCheck");
        let lower = directory.join("fetchloomcasecheck");
        support::remove_all(&[upper.clone(), lower.clone()]);

        platform.create_file_exclusive(&upper).unwrap();
        let both = platform.create_file_exclusive(&lower).is_ok();
        support::remove_all(&[upper, lower]);

        assert!(
            both,
            "{} was built case sensitive and refused the other case",
            directory.display()
        );
        assert_eq!(reported.case_folding, CaseFolding::Sensitive);
    }
}

#[test]
fn case_folding_is_reported_on_a_case_insensitive_volume() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for directory in support::volume_directories(support::Property::CaseInsensitive) {
        let scratch = tempfile::TempDir::new_in(&directory).unwrap();
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        platform
            .create_file_exclusive(&scratch.path().join("FetchloomCaseCheck"))
            .unwrap();
        assert!(
            platform
                .create_file_exclusive(&scratch.path().join("fetchloomcasecheck"))
                .is_err(),
            "{} was built case insensitive and accepted both cases",
            scratch.path().display()
        );
        assert_eq!(reported.case_folding, CaseFolding::Folding);
    }
}
#[test]
fn normalization_is_reported_on_a_volume_that_normalizes() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::Normalizing) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert_eq!(
            reported.normalization,
            Normalization::Normalizing,
            "{} stores a normalized form and the probe said otherwise",
            scratch.path().display()
        );
    }
}

#[test]
fn a_network_volume_is_reported_as_network_backed() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::Network) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert_eq!(
            reported.backing,
            Backing::Network,
            "{} is a network mount and the probe called it something else",
            scratch.path().display()
        );
    }
}

#[test]
fn a_memory_volume_is_reported_as_local() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::Memory) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert_eq!(
            reported.backing,
            Backing::Local,
            "{} is memory and the probe called it network backed",
            scratch.path().display()
        );
    }
}

#[test]
fn sparse_support_is_reported_where_it_is_absent() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::NoSparse) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert!(
            !reported.sparse,
            "{} stores no holes and the probe reported sparse support",
            scratch.path().display()
        );
    }
}

#[test]
fn a_fuse_mount_is_reported_as_unknown_backing_rather_than_network() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::Fuse) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        assert_eq!(
            reported.backing,
            Backing::Unknown,
            "{} is a filesystem in user space, which may be either, and was not reported as unknown",
            scratch.path().display()
        );
    }
}

#[test]
fn two_directories_on_one_volume_are_reported_separately() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for directory in support::volume_directories(support::Property::CaseSensitive) {
        let elsewhere = support::scratch();
        let first = platform.volume_capabilities(elsewhere.path()).unwrap();
        let second = platform.volume_capabilities(&directory).unwrap();

        let upper = directory.join("FetchloomOrderCheck");
        let lower = directory.join("fetchloomordercheck");
        support::remove_all(&[upper.clone(), lower.clone()]);
        platform.create_file_exclusive(&upper).unwrap();
        let both = platform.create_file_exclusive(&lower).is_ok();
        support::remove_all(&[upper, lower]);

        let expected = if both {
            CaseFolding::Sensitive
        } else {
            CaseFolding::Folding
        };
        assert_eq!(
            second.case_folding,
            expected,
            "{} was reported as {:?}, which is the answer for another directory on its volume",
            directory.display(),
            first.case_folding
        );
    }
}
