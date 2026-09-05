//! Contract tests over many processes on one cache, and over being killed.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test setup, where a failure to build the input is the assertion"
)]
#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "killing this process outright is a Win32 call, and it is what the test is for"
    )
)]

use blake3 as _;
use fetchloom_faults as _;
use serde as _;
use serde_json as _;
use zstd as _;

mod support;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use fetchloom_cache::layout::Layout;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::store::Store;

use support::{bytes_of, cache_in};

const CACHE_PATH: &str = "FETCHLOOM_TEST_CACHE_PATH";

const REPORT_PATH: &str = "FETCHLOOM_TEST_REPORT_PATH";

const ABORT_AFTER: &str = "FETCHLOOM_TEST_ABORT_AFTER";

const FIRST_OBJECT: &str = "FETCHLOOM_TEST_FIRST_OBJECT";

const RACED_LENGTH: usize = 1 << 18;

const RACERS: usize = 8;

const KILLS: u32 = 1000;

fn child(name: &str, cache: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env(CACHE_PATH, cache);
    command
}

#[test]
fn many_processes_wanting_one_digest_perform_one_transfer() {
    let scratch = tempfile::TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    drop(cache_in(scratch.path()));

    let mut running = Vec::new();
    for index in 0..RACERS {
        let report = scratch.path().join(format!("report-{index}"));
        let handle = child("race_for_one_digest", &cache)
            .env(REPORT_PATH, &report)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        running.push((handle, report));
    }

    let mut transferred = 0;
    let mut reused = 0;
    for (mut handle, report) in running {
        let mut said = String::new();
        if let Some(mut stream) = handle.stderr.take() {
            use std::io::Read as _;
            let _ = stream.read_to_string(&mut said);
        }
        let status = handle.wait().unwrap();
        assert!(
            status.success(),
            "a racing process failed: {status}: {said}"
        );
        match std::fs::read_to_string(&report).unwrap().trim() {
            "transferred" => transferred += 1,
            "reused" => reused += 1,
            other => panic!("a racing process reported {other}"),
        }
    }

    assert_eq!(
        transferred, 1,
        "{transferred} processes transferred one digest, and one of them was a second transfer"
    );
    assert_eq!(
        reused,
        RACERS - 1,
        "a process neither transferred nor reused"
    );

    let held = cache_in(scratch.path());
    let digest = hash_bytes(&bytes_of(RACED_LENGTH, 23));
    assert!(held.contains(digest).unwrap());
    assert!(
        support::digests_are_their_bytes(held.layout().root()),
        "an object does not hash to the name it is stored under"
    );
}

#[test]
#[ignore = "run only as a racing child of the concurrency test"]
fn race_for_one_digest() {
    let (Ok(cache), Ok(report)) = (std::env::var(CACHE_PATH), std::env::var(REPORT_PATH)) else {
        return;
    };
    let held = support::open_cache_at(Path::new(&cache)).unwrap();
    let bytes = bytes_of(RACED_LENGTH, 23);
    let digest = hash_bytes(&bytes);

    let lease = held.lease(PartialKey::of_content(digest)).unwrap();
    let outcome = if held.contains(digest).unwrap() {
        "reused"
    } else {
        let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
        writer.write_all(&bytes).unwrap();
        held.commit(lease, writer).unwrap();
        "transferred"
    };
    std::fs::write(&report, outcome).unwrap();
}

#[test]
fn a_thousand_kills_leave_no_invalid_object_and_no_orphan_after_recovery() {
    let scratch = tempfile::TempDir::new().unwrap();
    let cache = scratch.path().join("cache");
    let layout = Layout::new(&cache);
    drop(cache_in(scratch.path()));

    let mut checked = support::Checked::default();
    let mut reached_publication = false;
    let mut killed = 0;
    while killed < KILLS {
        let mut running = Vec::new();
        for index in 0..RACERS {
            let round = killed + u32::try_from(index).unwrap_or(0);
            running.push(
                child("publish_until_killed", &cache)
                    .env(ABORT_AFTER, (round % 5).to_string())
                    .env(FIRST_OBJECT, (round / 2).to_string())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut handle in running {
            let status = handle.wait().unwrap();
            assert!(
                !status.success(),
                "a writer that was told to abort exited normally after {killed} kills"
            );
        }
        killed += u32::try_from(RACERS).unwrap_or(1);

        assert!(
            checked.newly_published_are_their_bytes(layout.root()),
            "a killed writer left an object that does not hash to its name after {killed} kills"
        );
        let published = layout.objects().read_dir().unwrap().next().is_some()
            || layout
                .packs()
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_some());
        if published {
            reached_publication = true;
        }
    }

    assert!(
        reached_publication,
        "no round ever published anything, so nothing about publication was tested"
    );

    support::pretend_a_previous_boot(&layout);
    let recovered = cache_in(scratch.path());

    assert!(
        support::digests_are_their_bytes(recovered.layout().root()),
        "recovery left an object that does not hash to its name"
    );
    assert_eq!(
        support::entries_in(&layout.partial()),
        Vec::<PathBuf>::new(),
        "recovery left a partial from a previous boot"
    );
    assert_eq!(
        support::entries_in(&layout.staging()),
        Vec::<PathBuf>::new(),
        "recovery left a staging tree from a previous boot"
    );
}

#[test]
#[ignore = "run only as the killed child of the concurrency test"]
fn publish_until_killed() {
    let (Ok(cache), Ok(after), Ok(first)) = (
        std::env::var(CACHE_PATH),
        std::env::var(ABORT_AFTER),
        std::env::var(FIRST_OBJECT),
    ) else {
        return;
    };
    let after: u32 = after.parse().unwrap();
    let first: u32 = first.parse().unwrap();
    let held = support::open_cache_at(Path::new(&cache)).unwrap();

    let mut done = 0;
    loop {
        if done == after {
            killed_now();
        }
        let bytes = bytes_of(1 << 16, u8::try_from((first + done) % 251).unwrap_or(1));
        let digest = hash_bytes(&bytes);
        let lease = held.lease(PartialKey::of_content(digest)).unwrap();
        let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
        writer.write_all(&bytes).unwrap();
        if done + 1 == after {
            killed_now();
        }
        held.commit(lease, writer).unwrap();
        done += 1;
    }
}

#[test]
fn recovery_keeps_what_this_boot_wrote_and_removes_what_a_previous_boot_left() {
    let scratch = tempfile::TempDir::new().unwrap();
    let layout = Layout::new(scratch.path().join("cache"));
    let held = cache_in(scratch.path());

    let bytes = bytes_of(4096, 41);
    let digest = hash_bytes(&bytes);
    let lease = held.lease(PartialKey::of_content(digest)).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(&bytes).unwrap();
    drop(writer);
    drop(lease);
    drop(held);

    let reopened = cache_in(scratch.path());
    assert!(
        layout.partial_of(digest).is_file(),
        "a partial written in this boot was removed, and it is what resume continues"
    );
    drop(reopened);

    support::pretend_a_previous_boot(&layout);
    let after = cache_in(scratch.path());
    assert!(
        !layout.partial_of(digest).exists(),
        "a partial from a previous boot survived recovery"
    );
    assert!(
        support::is_empty(&layout.partial()),
        "recovery left something beside the partial it removed: {:?}",
        support::names(&layout.partial())
    );
    drop(after);
}

#[test]
fn recovery_removes_the_source_record_beside_a_partial_it_removes() {
    let scratch = tempfile::TempDir::new().unwrap();
    let layout = Layout::new(scratch.path().join("cache"));
    let held = cache_in(scratch.path());

    let bytes = bytes_of(4096, 43);
    let digest = hash_bytes(&bytes);
    let lease = held.lease(PartialKey::of_content(digest)).unwrap();
    let mut writer = held.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(&bytes).unwrap();
    drop(writer);
    held.record_source(PartialKey::of_content(digest), &support::a_source_record())
        .unwrap();
    drop(lease);
    drop(held);

    support::pretend_a_previous_boot(&layout);
    let after = cache_in(scratch.path());
    assert!(
        support::is_empty(&layout.partial()),
        "recovery left {:?} behind",
        support::names(&layout.partial())
    );
    drop(after);
}

#[test]
fn recovery_leaves_an_entry_another_machine_wrote() {
    let scratch = tempfile::TempDir::new().unwrap();
    let layout = Layout::new(scratch.path().join("cache"));
    drop(cache_in(scratch.path()));

    let theirs = layout.partial().join("another-machine");
    std::fs::write(&theirs, b"bytes another machine is writing").unwrap();
    support::write_foreign_owner(&theirs);

    drop(cache_in(scratch.path()));
    assert!(
        theirs.is_file(),
        "recovery removed an entry another machine wrote, which is not this machine's to recover"
    );
}

const PACKED: usize = 64;

const PACKED_LENGTH: usize = 4096;

#[test]
fn objects_published_at_once_by_one_process_each_read_back_as_themselves() {
    let scratch = tempfile::TempDir::new().unwrap();
    let cache = cache_in(scratch.path());
    let wanted: Vec<Vec<u8>> = (0..PACKED)
        .map(|seed| bytes_of(PACKED_LENGTH, u8::try_from(seed + 1).unwrap_or(1)))
        .collect();

    std::thread::scope(|scope| {
        for bytes in &wanted {
            scope.spawn(|| {
                let digest = hash_bytes(bytes);
                let lease = cache.lease(PartialKey::of_content(digest)).unwrap();
                let mut writer = cache.begin(&lease, bytes.len() as u64).unwrap();
                writer.write_all(bytes).unwrap();
                cache.commit(lease, writer).unwrap();
            });
        }
    });

    for bytes in &wanted {
        let digest = hash_bytes(bytes);
        let mut reader = cache.open(digest).unwrap();
        let mut found = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut found).unwrap();
        assert_eq!(
            hash_bytes(&found),
            digest,
            "an object published beside others read back as different bytes, so two writers \
             recorded one place in the pack"
        );
    }
}

#[cfg(windows)]
fn killed_now() -> ! {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

    // SAFETY: the pseudo handle GetCurrentProcess returns needs no closing, and terminating this process cannot outlive it.
    unsafe {
        TerminateProcess(GetCurrentProcess(), 137);
    }
    unreachable!("the process was told to end and did not")
}

#[cfg(not(windows))]
fn killed_now() -> ! {
    std::process::abort()
}
