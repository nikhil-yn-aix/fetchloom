//! What an interrupted run does, and what it leaves behind.

#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "sending a console control event to another process group is a Windows call"
    )
)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the run that failed is the message"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use toml as _;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// How many files the corpus holds, chosen so a run takes long enough to be
/// interrupted while it is still working.
const FILES: usize = 1500;

struct Scene {
    _scratch: TempDir,
    source: PathBuf,
    destination: PathBuf,
    cache: PathBuf,
}

fn scene() -> Scene {
    let scratch = TempDir::new().unwrap();
    let source = scratch.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    for index in 0..FILES {
        std::fs::write(
            source.join(format!("file-{index:05}.bin")),
            format!("the only copy of the bytes for file {index}").as_bytes(),
        )
        .unwrap();
    }
    Scene {
        source,
        destination: scratch.path().join("out"),
        cache: scratch.path().join("cache"),
        _scratch: scratch,
    }
}

fn start(scene: &Scene) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_fetchloom"));
    command
        .arg("get")
        .arg(&scene.source)
        .arg("--output")
        .arg(&scene.destination)
        .arg("--cache-dir")
        .arg(&scene.cache)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    interruptible(&mut command);
    command.spawn().unwrap()
}

#[cfg(windows)]
fn interruptible(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

#[cfg(windows)]
fn interrupt(child: &Child) {
    // SAFETY: the child was started in its own process group, and its identifier names that group for as long as the handle is held.
    let sent = unsafe {
        windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
            windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
            child.id(),
        )
    };
    assert!(
        sent != 0,
        "the console refused to send the interrupt: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn interruptible(_command: &mut Command) {}

#[cfg(unix)]
fn interrupt(child: &Child) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::INT);
}

/// Waits until the run has written something, so the interrupt lands on a run
/// that is working rather than on one that has not started.
fn wait_until_working(scene: &Scene) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if entries_under(&scene.cache) > 4 {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the run wrote nothing into the cache within twenty seconds");
}

fn entries_under(directory: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.path().is_dir() {
                entries_under(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

/// Returns whether every object in the cache hashes to the name it is under.
fn every_object_is_its_own_name(cache: &Path) -> bool {
    let objects = cache.join("objects");
    let Ok(shards) = std::fs::read_dir(&objects) else {
        return true;
    };
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let named = entry.file_name().to_string_lossy().into_owned();
            let hashed = fetchloom_engine::hashing::hash_bytes(&bytes).to_string();
            if !hashed.ends_with(&named) && !named.ends_with(&hashed) {
                return false;
            }
        }
    }
    true
}

#[test]
fn one_interrupt_stops_the_run_and_exits_one_hundred_and_thirty() {
    let scene = scene();
    let mut child = start(&scene);
    wait_until_working(&scene);

    let asked = Instant::now();
    interrupt(&child);
    let status = child.wait().unwrap();
    let took = asked.elapsed();

    assert_eq!(
        status.code(),
        Some(130),
        "an interrupted run did not exit as cancelled"
    );
    assert!(
        took < Duration::from_secs(2),
        "an interrupted run took {took:?} to stop, where the contract allows two seconds"
    );
    assert!(
        every_object_is_its_own_name(&scene.cache),
        "an interrupted run left an object that does not hash to its name"
    );
}

#[test]
fn a_second_interrupt_aborts_and_the_cache_still_holds_only_what_it_verified() {
    let scene = scene();
    let mut child = start(&scene);
    wait_until_working(&scene);

    interrupt(&child);
    interrupt(&child);
    let status = child.wait().unwrap();

    assert!(
        !status.success(),
        "a run interrupted twice reported success"
    );
    assert!(
        every_object_is_its_own_name(&scene.cache),
        "a run aborted mid-write left an object that does not hash to its name"
    );
}
