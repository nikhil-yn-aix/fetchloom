//! Contract tests over what a volume and the processor can actually do.
//!
//! Every probe is checked against what the filesystem itself does, never
//! against a value hardcoded for one machine, so these assertions are the same
//! on all three platforms. A capability this machine has no filesystem for
//! carries an ignored test naming the filesystem, so the gap is listed rather
//! than silently passing.

#![expect(
    clippy::unwrap_used,
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
#[ignore = "needs a ReFS or Dev Drive volume, which this machine does not have"]
fn clone_is_reported_on_a_volume_that_supports_block_cloning() {}

#[test]
#[ignore = "needs a Btrfs or reflink-enabled XFS volume, reachable only on a Linux runner"]
fn clone_is_reported_on_a_reflink_volume() {}

#[test]
#[ignore = "needs an APFS volume, reachable only on a macOS runner"]
fn clone_is_reported_on_apfs() {}

#[test]
#[ignore = "needs a case-insensitive APFS volume, reachable only on a macOS runner"]
fn case_folding_is_reported_on_case_insensitive_apfs() {}

#[test]
#[ignore = "needs an HFS+ volume, which normalizes rather than preserves, reachable only on a macOS runner"]
fn normalization_is_reported_as_normalizing_on_hfs_plus() {}

#[test]
#[ignore = "needs an ext4 directory carrying the case-folding flag, reachable only on a Linux runner"]
fn case_folding_is_reported_on_a_casefolded_ext4_directory() {}

#[test]
#[ignore = "needs a mounted NFS or SMB share, which this machine does not have"]
fn a_network_share_is_reported_as_network_backed() {}

#[test]
#[ignore = "needs a FUSE mount, reachable only on a Linux or macOS runner"]
fn a_fuse_mount_is_reported_as_unknown_backing_rather_than_network() {}

#[test]
#[ignore = "needs a volume without sparse file support, which this machine does not have"]
fn sparse_support_is_reported_where_it_is_absent() {}
