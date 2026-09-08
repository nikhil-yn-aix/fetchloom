//! Contract tests over identity, exclusive creation, and placing bytes.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use rustix as _;

mod support;

use fetchloom_engine::capability::CopyMechanism;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

#[test]
fn two_files_have_different_identities() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let one = scratch.path().join("one");
    let two = scratch.path().join("two");
    support::write_file(&one, b"one");
    support::write_file(&two, b"two");

    assert_ne!(
        platform.file_id(&one).unwrap(),
        platform.file_id(&two).unwrap()
    );
}

#[test]
fn one_file_has_the_same_identity_through_two_paths() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let nested = scratch.path().join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let direct = nested.join("file");
    support::write_file(&direct, b"content");
    let indirect = scratch.path().join("nested").join(".").join("file");

    assert_eq!(
        platform.file_id(&direct).unwrap(),
        platform.file_id(&indirect).unwrap()
    );
}

#[test]
fn a_file_identity_survives_being_reopened() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("file");
    support::write_file(&path, b"content");

    let first = platform.file_id(&path).unwrap();
    let second = platform.file_id(&path).unwrap();
    assert_eq!(first, second);
}

#[test]
fn a_fingerprint_changes_when_the_content_changes() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("file");
    support::write_file(&path, b"before");
    let before = platform.fingerprint(&path).unwrap();

    support::write_file(&path, b"after this is longer");
    let after = platform.fingerprint(&path).unwrap();

    assert_ne!(
        before, after,
        "a changed file kept the fingerprint that says it is unchanged"
    );
}

#[test]
fn a_fingerprint_carries_the_volume_and_the_file_it_describes() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("file");
    support::write_file(&path, b"content");

    let fingerprint = platform.fingerprint(&path).unwrap();
    assert_eq!(fingerprint.volume, platform.volume_id(&path).unwrap());
    assert_eq!(fingerprint.file, platform.file_id(&path).unwrap());
    assert_eq!(fingerprint.size, 7);
}

#[test]
fn creating_a_file_that_exists_fails_rather_than_truncating_it() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("file");
    support::write_file(&path, b"existing content");

    assert!(
        platform.create_file_exclusive(&path).is_err(),
        "exclusive creation overwrote an existing name"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"existing content",
        "the existing file was truncated"
    );
}

#[test]
fn creating_a_directory_that_exists_fails() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("directory");
    platform.create_directory_exclusive(&path).unwrap();
    assert!(
        platform.create_directory_exclusive(&path).is_err(),
        "exclusive creation accepted a name that already exists"
    );
}

#[test]
fn placing_bytes_produces_an_identical_file_whichever_mechanism_is_used() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let from = scratch.path().join("source");
    let to = scratch.path().join("target");
    let bytes: Vec<u8> = (0..64_u32).flat_map(u32::to_le_bytes).collect();
    support::write_file(&from, &bytes);

    let mechanism = platform.clone_or_copy(&from, &to).unwrap();

    assert_eq!(
        std::fs::read(&to).unwrap(),
        bytes,
        "{mechanism:?} lost bytes"
    );
    assert!(
        matches!(mechanism, CopyMechanism::Clone | CopyMechanism::Copy),
        "the mechanism used was not reported"
    );
}

#[test]
fn a_volume_that_cannot_clone_falls_back_to_copy_and_reports_the_fallback() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let capabilities = platform.volume_capabilities(scratch.path()).unwrap();
    if capabilities.clone {
        fetchloom_faults::decline!("a volume that refuses a block clone");
        return;
    }

    let from = scratch.path().join("source");
    let to = scratch.path().join("target");
    support::write_file(&from, b"bytes to copy");
    let mechanism = platform.clone_or_copy(&from, &to).unwrap();

    assert_eq!(mechanism, CopyMechanism::Copy);
    let reported = platform.take_degradations();
    assert!(
        reported
            .iter()
            .any(|entry| entry.used.contains("copy") || entry.used.contains("copied")),
        "a fallback to copying was not reported as a degradation: {reported:?}"
    );
}

#[test]
fn placing_bytes_onto_a_name_that_exists_fails() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let from = scratch.path().join("source");
    let to = scratch.path().join("target");
    support::write_file(&from, b"source");
    support::write_file(&to, b"already here");

    assert!(
        platform.clone_or_copy(&from, &to).is_err(),
        "placing bytes overwrote a name that already exists"
    );
    assert_eq!(std::fs::read(&to).unwrap(), b"already here");
}

#[test]
fn a_symlink_carries_the_target_bytes_it_was_given() {
    let scratch = support::scratch();
    if !support::symlink_works(scratch.path()) {
        fetchloom_faults::decline!("a volume that permits creating a symbolic link");
        return;
    }
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let link = scratch.path().join("link");
    platform.create_symlink(b"some/target/path", &link).unwrap();

    let read = std::fs::read_link(&link).unwrap();
    assert_eq!(
        read.to_string_lossy().replace('\\', "/"),
        "some/target/path"
    );
}

#[test]
fn a_degradation_names_what_was_requested_what_was_used_and_why() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let from = scratch.path().join("source");
    let to = scratch.path().join("target");
    support::write_file(&from, b"bytes");
    platform.clone_or_copy(&from, &to).unwrap();
    platform
        .preallocate(
            &platform
                .create_file_exclusive(&scratch.path().join("reserved"))
                .unwrap(),
            4096,
        )
        .unwrap();

    let reported = platform.take_degradations();
    let capabilities = platform.volume_capabilities(scratch.path()).unwrap();
    let expected = usize::from(!capabilities.clone) + usize::from(!capabilities.sparse);
    assert!(
        reported.len() >= expected,
        "this volume cannot clone or reserve, and {expected} degradations were expected rather than {}",
        reported.len()
    );

    for entry in reported {
        assert!(
            !entry.requested.is_empty(),
            "a degradation named no request"
        );
        assert!(!entry.used.is_empty(), "a degradation named nothing used");
        assert!(!entry.reason.is_empty(), "a degradation named no reason");
    }
}

#[test]
fn draining_degradations_empties_the_queue() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let from = scratch.path().join("source");
    support::write_file(&from, b"bytes");
    platform
        .clone_or_copy(&from, &scratch.path().join("target"))
        .unwrap();

    let _ = platform.take_degradations();
    assert!(
        platform.take_degradations().is_empty(),
        "degradations were reported twice"
    );
}

#[test]
fn reserving_nothing_is_not_a_failure() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("empty");
    let file = platform.create_file_exclusive(&path).unwrap();

    platform.preallocate(&file, 0).unwrap();

    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        0,
        "reserving nothing changed the length"
    );
}
