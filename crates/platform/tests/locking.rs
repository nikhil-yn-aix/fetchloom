//! Contract tests over advisory locking and the liveness ladder.
//!
//! The ladder is decided in a fixed order and never from a file modification
//! time. Two rules matter more than the rest and are asserted directly: a live
//! holder is never reported stale, and a holder on another machine is never
//! reported stale at all, because it cannot be inspected.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]

#[cfg(windows)]
use windows_sys as _;

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
    NativePlatform::new().owner_token().unwrap()
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
    let platform = NativePlatform::new();
    assert_eq!(platform.liveness(&ours()), Liveness::Live);
}

#[test]
fn a_holder_on_another_machine_is_never_stale() {
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
    let token = OwnerToken {
        boot: BootId::new("a-boot-that-is-not-this-one"),
        ..ours()
    };
    assert_eq!(platform.liveness(&token), Liveness::Stale);
}

#[test]
fn a_holder_whose_process_does_not_exist_is_stale() {
    let platform = NativePlatform::new();
    let token = OwnerToken {
        pid: 999_999_999,
        ..ours()
    };
    assert_eq!(platform.liveness(&token), Liveness::Stale);
}

#[test]
fn a_holder_whose_process_identifier_was_recycled_is_stale() {
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
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

    let platform = NativePlatform::new();
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
    let platform = NativePlatform::new();
    let held = platform.try_lock(std::path::Path::new(&path)).unwrap();
    assert!(held.is_some(), "the child could not take the lock");
    std::fs::write(&ready, b"held").unwrap();
    while std::path::Path::new(&ready).exists() {
        std::hint::spin_loop();
    }
}

#[test]
#[ignore = "needs a second user account and a shared cache directory, which this machine does not have"]
fn a_lock_is_honored_across_users() {}

#[test]
#[ignore = "needs a mounted NFS or SMB share, which this machine does not have"]
fn a_network_volume_is_refused_for_a_shared_cache() {}

#[test]
#[ignore = "needs a filesystem whose locking fails with ENOLCK or EOPNOTSUPP, reachable only on a Linux runner"]
fn a_volume_that_cannot_lock_is_refused_with_locking_unsupported() {}
