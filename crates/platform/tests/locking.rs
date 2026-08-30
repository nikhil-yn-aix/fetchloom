//! Contract tests over advisory locking and the liveness ladder.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

#[cfg(unix)]
use rustix as _;

mod support;

use std::process::Command;

use fetchloom_engine::identity::{BootId, MachineId};
use fetchloom_engine::seam::platform::{Liveness, OwnerToken, Platform};
use fetchloom_platform::NativePlatform;

/// The variable the child holding a lock reads.
const LOCK_PATH: &str = "FETCHLOOM_TEST_LOCK_PATH";

/// The variable the child writes to say it has the lock.
const READY_PATH: &str = "FETCHLOOM_TEST_READY_PATH";

fn ours() -> OwnerToken {
    NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ))
    .owner_token()
    .unwrap()
}

#[test]
fn this_process_can_describe_itself_as_a_lock_holder() {
    let token = ours();
    assert!(
        !token.machine.as_str().is_empty(),
        "machine identity is empty"
    );
    assert!(!token.boot.as_str().is_empty(), "boot identity is empty");
    assert_eq!(token.pid, std::process::id());
    assert!(token.start > 0, "a process start time was not read");
}

#[test]
fn the_owner_token_is_the_same_every_time_it_is_read() {
    assert_eq!(ours(), ours(), "this process described itself two ways");
}

#[test]
fn a_holder_that_is_this_process_is_live() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    assert_eq!(platform.liveness(&ours()), Liveness::Live);
}

#[test]
fn a_holder_on_another_machine_is_never_stale() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let token = OwnerToken {
        machine: MachineId::new("a-machine-that-is-not-this-one"),
        ..ours()
    };
    assert_eq!(
        platform.liveness(&token),
        Liveness::OtherMachine,
        "a holder on another machine must never be treated as stale"
    );
}

#[test]
fn a_holder_from_another_machine_is_other_machine_even_when_its_process_is_gone() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let token = OwnerToken {
        machine: MachineId::new("a-machine-that-is-not-this-one"),
        boot: BootId::new("a-boot-that-is-not-this-one"),
        pid: 999_999_999,
        start: 1,
    };
    assert_eq!(
        platform.liveness(&token),
        Liveness::OtherMachine,
        "machine identity is decided before anything else in the ladder"
    );
}

#[test]
fn a_holder_from_a_previous_boot_is_stale() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let token = OwnerToken {
        boot: BootId::new("a-boot-that-is-not-this-one"),
        ..ours()
    };
    assert_eq!(platform.liveness(&token), Liveness::Stale);
}

#[test]
fn a_holder_whose_process_does_not_exist_is_stale() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let token = OwnerToken {
        pid: 999_999_999,
        ..ours()
    };
    assert_eq!(platform.liveness(&token), Liveness::Stale);
}

#[test]
fn a_holder_whose_process_identifier_was_recycled_is_stale() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let token = OwnerToken {
        start: ours().start.wrapping_add(1_000_000),
        ..ours()
    };
    assert_eq!(
        platform.liveness(&token),
        Liveness::Stale,
        "a recycled process identifier was treated as the original holder"
    );
}

#[test]
fn a_holder_with_an_unreadable_field_is_never_stolen_from() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for token in [
        OwnerToken {
            machine: MachineId::new(""),
            ..ours()
        },
        OwnerToken {
            boot: BootId::new(""),
            ..ours()
        },
    ] {
        match platform.liveness(&token) {
            Liveness::Undecidable { missing } => {
                assert!(!missing.is_empty(), "the missing field was not named");
            }
            other => panic!("a lock was decided on missing evidence: {other:?}"),
        }
    }
}

#[test]
fn a_lock_is_taken_once_and_refused_to_a_second_holder() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let held = platform.try_lock(&path).unwrap();
    assert!(held.is_some(), "the first holder did not get the lock");

    let second = platform.try_lock(&path).unwrap();
    assert!(
        second.is_none(),
        "two holders took the same lock in one process"
    );
}

#[test]
fn a_lock_is_released_when_it_is_dropped() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let held = platform.try_lock(&path).unwrap().unwrap();
    drop(held);

    let again = platform.try_lock(&path).unwrap();
    assert!(again.is_some(), "the lock was not released when dropped");
}

#[test]
fn a_lock_held_by_another_process_is_observed_as_held() {
    let scratch = support::scratch();
    let path = scratch.path().join("digest.lock");
    let ready = scratch.path().join("ready");

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hold_lock_until_removed",
            "--ignored",
            "--nocapture",
        ])
        .env(LOCK_PATH, &path)
        .env(READY_PATH, &ready)
        .spawn()
        .unwrap();

    while !ready.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child exited before it took the lock"
        );
        std::hint::spin_loop();
    }

    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let taken = platform.try_lock(&path).unwrap();
    assert!(
        taken.is_none(),
        "a lock held by another process was taken by this one"
    );

    std::fs::remove_file(&ready).unwrap();
    child.wait().unwrap();

    let after = platform.try_lock(&path).unwrap();
    assert!(
        after.is_some(),
        "the lock was not released when the holder exited"
    );
}

#[test]
#[ignore = "run only as the lock-holding child of the locking test"]
fn hold_lock_until_removed() {
    let (Ok(path), Ok(ready)) = (std::env::var(LOCK_PATH), std::env::var(READY_PATH)) else {
        return;
    };
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let held = platform.try_lock(std::path::Path::new(&path)).unwrap();
    assert!(held.is_some(), "the child could not take the lock");
    std::fs::write(&ready, b"held").unwrap();
    while std::path::Path::new(&ready).exists() {
        std::hint::spin_loop();
    }
}

#[test]
#[cfg(unix)]
fn a_lock_is_honored_across_users() {
    let Some(user) = support::another_user() else {
        return;
    };
    let shared = support::scratch();
    shared_by_everyone(shared.path());
    let path = shared.path().join("digest.lock");
    let ready = shared.path().join("ready");

    let mut child = Command::new("setpriv")
        .args(["--reuid", &user, "--regid", &user, "--clear-groups", "--"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hold_lock_until_removed",
            "--ignored",
            "--nocapture",
        ])
        .env(LOCK_PATH, &path)
        .env(READY_PATH, &ready)
        .spawn()
        .unwrap();

    while !ready.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child exited before it took the lock as {user}"
        );
        std::hint::spin_loop();
    }

    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let taken = platform.try_lock(&path).unwrap();
    assert!(
        taken.is_none(),
        "a lock held by {user} was taken by this user"
    );

    std::fs::remove_file(&ready).unwrap();
    child.wait().unwrap();

    let after = platform.try_lock(&path).unwrap();
    assert!(
        after.is_some(),
        "the lock was not released when the holder exited"
    );
}

#[cfg(unix)]
fn shared_by_everyone(directory: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o777)).unwrap();
}

#[test]
#[ignore = "no filesystem reachable inside the verification container refuses a lock: a bindfs mount in user space, tmpfs through /dev/shm and a procfs file each granted both an fcntl write lock and a flock, and the one filesystem known to answer ENOLCK is an NFS mount without a lock daemon, which needs a server the container lane does not run"]
fn a_volume_that_cannot_lock_is_refused_with_locking_unsupported() {}

#[test]
fn two_shared_holders_take_one_lock_at_once() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let first = platform.try_lock_shared(&path).unwrap();
    let second = platform.try_lock_shared(&path).unwrap();

    assert!(first.is_some(), "the first shared holder was refused");
    assert!(
        second.is_some(),
        "a second shared holder was refused a lock a reader must be able to share"
    );
}

#[test]
fn a_shared_holder_refuses_a_writer() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let reader = platform.try_lock_shared(&path).unwrap();
    assert!(reader.is_some());
    assert!(
        platform.try_lock(&path).unwrap().is_none(),
        "a writer took a lock a reader was holding"
    );
}

#[test]
fn a_writer_refuses_a_shared_holder() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let writer = platform.try_lock(&path).unwrap();
    assert!(writer.is_some());
    assert!(
        platform.try_lock_shared(&path).unwrap().is_none(),
        "a reader took a lock a writer was holding"
    );
}

#[test]
fn a_shared_lock_is_released_when_it_is_dropped() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("digest.lock");

    let held = platform.try_lock_shared(&path).unwrap();
    drop(held);

    assert!(
        platform.try_lock(&path).unwrap().is_some(),
        "a shared lock was not released when it was dropped"
    );
}

#[test]
fn this_process_owns_what_it_creates() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("object");
    support::write_file(&path, b"bytes");

    assert!(
        platform.owns(&path).unwrap(),
        "a file this process created is not reported as its own"
    );
}

#[test]
fn a_volume_without_ownership_does_not_report_this_process_as_the_owner() {
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    for scratch in support::scratch_on(support::Property::NoOwnership) {
        let path = scratch.path().join("object");
        support::write_file(&path, b"bytes");
        assert!(
            !platform.owns(&path).unwrap(),
            "{} records no owner and reported this process as one",
            scratch.path().display()
        );
    }
}

#[test]
fn an_identity_read_from_a_handle_is_the_identity_of_its_path() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("object");
    let file = platform.create_file_exclusive(&path).unwrap();

    assert_eq!(
        platform.file_id_of(&file).unwrap(),
        platform.file_id(&path).unwrap(),
        "a handle and its own path reported two identities"
    );
}

#[test]
fn an_identity_read_from_a_handle_survives_the_name_being_replaced() {
    let scratch = support::scratch();
    let platform = NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let path = scratch.path().join("object");
    let file = platform.create_file_exclusive(&path).unwrap();
    let before = platform.file_id_of(&file).unwrap();

    std::fs::remove_file(&path).unwrap();
    support::write_file(&path, b"a different file wearing the same name");

    assert_eq!(
        platform.file_id_of(&file).unwrap(),
        before,
        "an open handle changed identity when its name was reused"
    );
    assert_ne!(
        platform.file_id(&path).unwrap(),
        before,
        "a replaced name reported the identity of the file it replaced"
    );
}
