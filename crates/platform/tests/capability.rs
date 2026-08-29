//! Contract tests over what a volume and the processor can actually do.
//!
//! Every probe is checked against what the filesystem itself does, never
//! against a value hardcoded for one machine, so these assertions are the same
//! on all three platforms. A capability that needs a filesystem this machine
//! does not have runs against the volumes the environment names, and a runner
//! that promised such a volume and did not build it fails rather than passing
//! quietly.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use libc as _;
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
    let platform = NativePlatform::new();
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
        Scanner::Present { cost_ratio, .. } => {
            assert!(
                cost_ratio.is_finite() && cost_ratio > 0.0,
                "a measured cost is a real number"
            );
        }
    }
}

#[test]
fn a_probe_leaves_nothing_behind() {
    let scratch = support::scratch();
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
    let first = platform.volume_capabilities(scratch.path()).unwrap();
    let second = platform.volume_capabilities(scratch.path()).unwrap();
    assert_eq!(first, second, "a capability answer is not stable");
}

#[test]
fn the_thread_budget_is_the_detected_count_when_nothing_is_requested() {
    let platform = NativePlatform::new();
    let found = platform.processor_capabilities(None);
    assert_eq!(found.budget.origin(), BudgetOrigin::Detected);
    assert_eq!(found.budget.threads(), found.budget.detected());
}

#[test]
fn a_thread_ceiling_below_the_detected_count_is_honored() {
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
    for scratch in support::scratch_on(support::Property::CaseSensitive) {
        let reported = platform.volume_capabilities(scratch.path()).unwrap();
        platform
            .create_file_exclusive(&scratch.path().join("FetchloomCaseCheck"))
            .unwrap();
        platform
            .create_file_exclusive(&scratch.path().join("fetchloomcasecheck"))
            .unwrap_or_else(|reason| {
                panic!(
                    "{} was built case sensitive and refused the other case: {reason}",
                    scratch.path().display()
                )
            });
        assert_eq!(reported.case_folding, CaseFolding::Sensitive);
    }
}

#[test]
fn case_folding_is_reported_on_a_case_insensitive_volume() {
    let platform = NativePlatform::new();
    for scratch in support::scratch_on(support::Property::CaseInsensitive) {
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
#[ignore = "needs a FUSE mount, which no runner builds yet"]
fn a_fuse_mount_is_reported_as_unknown_backing_rather_than_network() {}
