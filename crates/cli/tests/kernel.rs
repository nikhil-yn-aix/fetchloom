//! The work counters, judged against what the operating system itself recorded
//! for the same run rather than against a number this project wrote down.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]
#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "the kernel's own counters are readable only through a Win32 call"
    )
)]

use std::path::Path;

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
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use toml as _;

use fetchloom_engine::work::Work;
mod support;

use tempfile::TempDir;

/// How large the object each run materializes is.
const OBJECT_BYTES: u64 = 8 * 1024 * 1024;

/// What the `--json` result carries that this suite reads.
#[derive(serde::Deserialize)]
struct Reported {
    work: Work,
}

fn object_at(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let mut bytes = vec![0_u8; usize::try_from(OBJECT_BYTES).unwrap()];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::try_from(index % 251).unwrap_or(0);
    }
    std::fs::write(root.join("obj.bin"), &bytes).unwrap();
}

/// What one run reported and what the operating system counted for it.
struct Observed {
    reported: Work,
    kernel_read: u64,
    kernel_written: u64,
}

#[cfg(windows)]
fn run_observed(source: &Path, destination: &Path, cache: &Path) -> Observed {
    use std::io::Read;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{GetProcessIoCounters, IO_COUNTERS};

    let mut child = support::fetchloom()
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let mut body = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut body).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "the run failed");

    let mut counters = IO_COUNTERS {
        ReadOperationCount: 0,
        WriteOperationCount: 0,
        OtherOperationCount: 0,
        ReadTransferCount: 0,
        WriteTransferCount: 0,
        OtherTransferCount: 0,
    };
    // SAFETY: the child handle is owned and still open, and the counter structure is a live local of exactly the size the call writes.
    let ok = unsafe { GetProcessIoCounters(child.as_raw_handle() as HANDLE, &raw mut counters) };
    assert!(ok != 0, "the kernel refused to report its counters");

    let reported: Reported = serde_json::from_slice(&body).expect("a json result");
    Observed {
        reported: reported.work,
        kernel_read: counters.ReadTransferCount,
        kernel_written: counters.WriteTransferCount,
    }
}

#[cfg(not(windows))]
fn run_observed(source: &Path, destination: &Path, cache: &Path) -> Observed {
    let output = support::fetchloom()
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reported: Reported = serde_json::from_slice(&output.stdout).expect("a json result");
    Observed {
        reported: reported.work,
        kernel_read: 0,
        kernel_written: 0,
    }
}

#[cfg(windows)]
#[test]
fn the_reported_bytes_are_the_bytes_the_kernel_moved() {
    for shape in ["file", "directory"] {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        object_at(&source);
        let target = if shape == "file" {
            source.join("obj.bin")
        } else {
            source.clone()
        };

        let observed = run_observed(
            &target,
            &temporary.path().join("out"),
            &temporary.path().join("cache"),
        );

        for (name, reported, kernel) in [
            ("read", observed.reported.bytes_read, observed.kernel_read),
            (
                "written",
                observed.reported.bytes_written,
                observed.kernel_written,
            ),
        ] {
            assert!(
                reported <= kernel,
                "a {shape} run reported {name} {reported} bytes where the kernel moved {kernel}, \
                 so it counted bytes that were never moved"
            );
            assert!(
                reported * 20 >= kernel * 19,
                "a {shape} run reported {name} {reported} bytes where the kernel moved {kernel}, \
                 so it counted less than the run performed"
            );
        }
    }
}

#[test]
fn the_reported_writes_are_the_bytes_left_on_disk() {
    for shape in ["file", "directory"] {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        object_at(&source);
        let destination = temporary.path().join("out");
        let cache = temporary.path().join("cache");
        let target = if shape == "file" {
            source.join("obj.bin")
        } else {
            source.clone()
        };

        let observed = run_observed(&target, &destination, &cache);
        let left = occupied(&destination)
            + occupied(&cache.join("objects"))
            + occupied(&cache.join("packs"))
            + occupied(&cache.join("outboard"));

        assert_eq!(
            observed.reported.bytes_written, left,
            "a {shape} run reported writing {} bytes of content and left {left} bytes of it on \n             disk",
            observed.reported.bytes_written
        );
    }
}

/// How many bytes of content a tree holds.
fn occupied(root: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            total += occupied(&entry.path());
        } else if let Ok(data) = entry.metadata() {
            total += data.len();
        }
    }
    total
}
