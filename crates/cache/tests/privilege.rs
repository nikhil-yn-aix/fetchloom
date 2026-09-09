//! What a cache directory lets another user do, and what a write inside one
//! does when it finds a symlink at the name it is about to use.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use fetchloom_engine as _;
use fetchloom_platform as _;
use serde as _;
use serde_json as _;
#[cfg(windows)]
use windows_sys as _;
use zstd as _;

mod support;

use support::{bytes_of, cache_in, publish};

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;

    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

#[test]
fn a_cache_under_a_private_directory_is_not_writable_by_another_user() {
    #[cfg(not(unix))]
    {
        fetchloom_faults::decline!("a platform whose directories carry mode bits");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = scratch.path().join("cache");
        let held = cache_in(scratch.path());
        drop(held);

        for directory in [root.clone(), root.join("objects"), root.join("meta")] {
            let mode = mode_of(&directory);
            assert_eq!(
                mode & 0o022,
                0,
                "{} is mode {mode:o}, which lets another user write a name inside it",
                directory.display()
            );
        }
    }
}

#[test]
fn a_cache_under_a_directory_every_user_can_write_stays_shared() {
    #[cfg(not(unix))]
    {
        fetchloom_faults::decline!("a platform whose directories carry mode bits");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        let root = scratch.path().join("cache");
        let held = cache_in(scratch.path());
        drop(held);

        let mode = mode_of(&root);
        assert_eq!(
            mode,
            0o1777,
            "{} is mode {mode:o}, so the users the person made the directory for cannot use it",
            root.display()
        );
    }
}

#[test]
fn a_record_write_refuses_a_symlink_left_at_the_name_rather_than_writing_through_it() {
    #[cfg(not(unix))]
    {
        fetchloom_faults::decline!("a platform whose links a write follows");
    }
    #[cfg(unix)]
    {
        let scratch = tempfile::TempDir::new().unwrap();
        let elsewhere = scratch.path().join("not-the-cache");
        std::fs::write(&elsewhere, b"the victim's file").unwrap();
        let target = scratch.path().join("recovered");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();

        let outcome = fetchloom_engine::atomic::replace(&target, b"the attacker's bytes");
        assert!(outcome.is_ok(), "the write beside the name failed outright");
        assert_eq!(
            std::fs::read(&elsewhere).unwrap(),
            b"the victim's file",
            "the write followed the symlink and replaced a file outside the cache"
        );
        assert!(
            std::fs::symlink_metadata(&target).unwrap().is_file(),
            "the name did not become a plain file"
        );
    }
}

#[test]
fn a_pin_refuses_a_symlink_left_at_its_name() {
    #[cfg(not(unix))]
    {
        fetchloom_faults::decline!("a platform whose links a write follows");
    }
    #[cfg(unix)]
    {
        let scratch = tempfile::TempDir::new().unwrap();
        let elsewhere = scratch.path().join("not-the-cache");
        std::fs::write(&elsewhere, b"the victim's file").unwrap();
        let target = scratch.path().join("pin");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();

        let refused = fetchloom_engine::atomic::touch(&target).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&elsewhere).unwrap(),
            b"the victim's file",
            "the pin followed the symlink and truncated a file outside the cache"
        );
    }
}

#[test]
fn a_cache_hit_on_a_shared_cache_is_not_taken_on_a_fingerprint_alone() {
    let scratch = tempfile::TempDir::new().unwrap();
    let held = cache_in(scratch.path());
    let bytes = bytes_of(4096, 7);
    let digest = publish(&held, &bytes);
    held.check_hit(digest)
        .expect("an object this run published did not pass its own check");
}

#[test]
fn an_object_another_user_wrote_is_hashed_rather_than_trusted_on_its_fingerprint() {
    let Some(other) = support::another_owner() else {
        fetchloom_faults::decline!("a second user this machine may give a file to");
        return;
    };
    #[cfg(not(unix))]
    {
        let _ = other;
        fetchloom_faults::decline!("a platform whose directories carry mode bits");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::TempDir::new().unwrap();
        std::fs::set_permissions(scratch.path(), std::fs::Permissions::from_mode(0o1777)).unwrap();
        let held = support::cache_in(scratch.path());
        let bytes = bytes_of(4096, 13);
        let digest = publish(&held, &bytes);
        let object = held.placement(digest).unwrap().container().to_path_buf();

        let mut wrong = bytes.clone();
        wrong[0] ^= 0xff;
        std::fs::write(&object, &wrong).unwrap();
        support::give_away(&object, &other);

        let refused = held.check_hit(digest).expect_err(
            "a cache hit on an object another user wrote was taken on its fingerprint alone",
        );
        assert!(
            !format!("{refused}").contains("changed since it was published"),
            "the object was refused on its fingerprint rather than on its bytes, so a rewrite that kept the fingerprint would have been served: {refused}"
        );
    }
}
