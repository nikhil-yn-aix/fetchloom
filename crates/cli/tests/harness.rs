//! That every test in this suite starts the binary the one controlled way.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the file that could not be read is the message"
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
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use tempfile as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

const NAMES_THE_BINARY: &str = concat!("CARGO_BIN", "_EXE_fetchloom");

const HARNESS: &str = "mod.rs";

#[test]
fn no_test_starts_the_binary_around_the_harness() {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut walked = 0_usize;
    let mut pending = vec![tests.clone()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|kind| kind != "rs") {
                continue;
            }
            walked += 1;
            if path.file_name().is_some_and(|name| name == HARNESS) {
                continue;
            }
            if std::fs::read_to_string(&path)
                .unwrap()
                .contains(NAMES_THE_BINARY)
            {
                offenders.push(path.display().to_string());
            }
        }
    }
    assert!(
        walked > 1,
        "the walk read {walked} files, so it proves nothing"
    );
    assert!(
        offenders.is_empty(),
        "these tests start the binary themselves rather than through support::fetchloom, so \
         nothing clears the environment they inherit or keeps a stray configuration file out of \
         them: {offenders:?}"
    );
}
