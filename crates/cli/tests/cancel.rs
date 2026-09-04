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
    clippy::expect_used,
    reason = "test assertions, where the run that failed is the message"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_cli as _;
use fetchloom_engine as _;

use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
use serde as _;
use serde_json as _;
use toml as _;

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

mod support;

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
    let mut command = support::fetchloom();
    command
        .arg("get")
        .arg(&scene.source)
        .arg("--output")
        .arg(&scene.destination)
        .arg("--cache-dir")
        .arg(&scene.cache)
        .arg("--events")
        .arg(STREAMED)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    interruptible(&mut command);
    command.spawn().unwrap()
}

/// What `--events` is given so the stream arrives on standard output, where a
/// blocking read is the wait and no test has to poll a file.
const STREAMED: &str = "-";

/// Reads the event stream a child is writing until it says it reached this
/// event, which is a wait on the pipe and not on a clock.
fn wait_until(child: &mut Child, event: &str) {
    let stream = child.stdout.as_mut().expect("the events pipe");
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).unwrap();
        assert!(read > 0, "the run ended without ever saying {event}");
        if line.contains(event) {
            return;
        }
    }
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

/// Returns whether the cache holds nothing that fails its own verification,
/// asked of the cache rather than of a second reader that would have to know
/// the pack layout to answer.
fn every_object_is_its_own_name(cache: &Path) -> bool {
    let output = support::fetchloom()
        .args(["cache", "verify", "--cache-dir"])
        .arg(cache)
        .arg("--json")
        .output()
        .unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("a report from cache verify");
    report["quarantined"].as_array().is_some_and(Vec::is_empty)
}

#[test]
fn one_interrupt_stops_the_run_and_exits_one_hundred_and_thirty() {
    let scene = scene();
    let mut child = start(&scene);
    wait_until(&mut child, "plan.ready");

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
    wait_until(&mut child, "plan.ready");

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

/// How long each request to a delayed server waits before it is answered, so
/// that an interrupt lands while transfers are still in flight.
const CHARGED: Duration = Duration::from_millis(400);

/// How many objects the interrupted concurrent run transfers.
const IN_FLIGHT: usize = 12;

#[test]
fn an_interrupt_with_several_transfers_in_flight_stops_within_two_seconds() {
    let scratch = TempDir::new().unwrap();
    let mut servers = Vec::with_capacity(IN_FLIGHT);
    let mut artifacts = String::new();
    for index in 0..IN_FLIGHT {
        let bytes: Vec<u8> = (0..1 << 16_usize)
            .map(|offset| u8::try_from((offset + index * 41) % 251).unwrap_or(0))
            .collect();
        let server = fetchloom_faults::TestServer::start(
            fetchloom_faults::Script::serving(bytes)
                .delayed(fetchloom_faults::Latency::default().every_request(CHARGED)),
        )
        .unwrap();
        std::fmt::Write::write_fmt(
            &mut artifacts,
            format_args!(
                "  - id: object-{index}\n    sources: [\"{}/object-{index}\"]\n",
                server.origin()
            ),
        )
        .unwrap();
        servers.push(server);
    }
    let manifest = scratch.path().join("dataset.yaml");
    std::fs::write(&manifest, format!("name: delayed\nartifacts:\n{artifacts}")).unwrap();

    let mut command = support::fetchloom();
    command
        .arg("get")
        .arg(&manifest)
        .arg("--output")
        .arg(scratch.path().join("out"))
        .arg("--cache-dir")
        .arg(scratch.path().join("cache"))
        .arg("--events")
        .arg(STREAMED)
        .arg("--concurrency")
        .arg("8")
        .arg("--per-host")
        .arg("8")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    interruptible(&mut command);
    let mut child = command.spawn().unwrap();
    wait_until(&mut child, "transfer.start");

    let asked = Instant::now();
    interrupt(&child);
    let status = child.wait().unwrap();
    let took = asked.elapsed();

    assert_eq!(
        status.code(),
        Some(130),
        "a run interrupted with several transfers in flight did not exit as cancelled"
    );
    assert!(
        took < Duration::from_secs(2),
        "a run interrupted with several transfers in flight took {took:?} to stop, where the \
         contract allows two seconds"
    );
    assert!(
        every_object_is_its_own_name(&scratch.path().join("cache")),
        "an interrupted concurrent run left an object that does not hash to its name"
    );
}
