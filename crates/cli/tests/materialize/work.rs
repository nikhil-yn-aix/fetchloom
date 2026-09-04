//! The deterministic work counters, asserted through the real binary.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;

use crate::support;
use fetchloom_engine::work::Work;
use fetchloom_faults::{TYPEFLAG_REGULAR, TarHeader, TarWriter};

use tempfile::TempDir;

fn corpus(root: &Path) {
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("zero.txt"), b"").unwrap();
    std::fs::write(root.join("nested").join("b.txt"), b"world").unwrap();
}

#[derive(serde::Deserialize)]
struct Reported {
    work: Work,
}

fn get_reporting_work(source: &Path, destination: &Path, cache: &Path) -> Work {
    let output = support::fetchloom()
        .current_dir(scratch())
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
    reported.work
}

fn get_reporting_work_without_cache(source: &Path, destination: &Path, cache: &Path) -> Work {
    let output = support::fetchloom()
        .current_dir(scratch())
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--no-cache")
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
    reported.work
}

#[test]
fn a_run_reports_the_work_it_did() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    corpus(&source);
    let work = get_reporting_work(
        &source,
        &temporary.path().join("out"),
        &temporary.path().join("cache"),
    );
    assert!(work.bytes_read > 0, "a run that copied files read bytes");
    assert!(
        work.bytes_written > 0,
        "a run that copied files wrote bytes"
    );
    assert_eq!(
        work.requests, 0,
        "a local run issues no request to any source"
    );
}

#[test]
fn identical_inputs_count_identically() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    corpus(&first.path().join("source"));
    corpus(&second.path().join("source"));

    let one = get_reporting_work(
        &first.path().join("source"),
        &first.path().join("out"),
        &first.path().join("cache"),
    );
    let two = get_reporting_work(
        &second.path().join("source"),
        &second.path().join("out"),
        &second.path().join("cache"),
    );
    assert_eq!(
        one, two,
        "the same inputs must produce the same counts, or the metric cannot gate"
    );
}

#[test]
fn a_local_ingest_writes_nothing_into_a_cache_that_already_holds_the_bytes() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    one_object(&source);
    let cache = temporary.path().join("cache");

    let cold = get_reporting_work(&source, &temporary.path().join("cold"), &cache);
    let warm = get_reporting_work(&source, &temporary.path().join("warm"), &cache);

    assert!(
        warm.bytes_written < cold.bytes_written,
        "a run against a cache that already holds every object wrote as much as the run that \
         filled it: cold {} warm {}",
        cold.bytes_written,
        warm.bytes_written
    );
    assert!(
        warm.bytes_read < cold.bytes_read,
        "a run against a cache that already holds every object read as much as the run that \
         filled it: cold {} warm {}",
        cold.bytes_read,
        warm.bytes_read
    );
}

fn scratch() -> &'static std::path::Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

#[test]
fn every_file_a_run_creates_is_counted_on_whichever_path_created_it() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    let files = 64;
    for index in 0..files {
        std::fs::write(
            source.join(format!("file-{index}.bin")),
            format!("distinct content for file {index}").as_bytes(),
        )
        .unwrap();
    }

    let cached = get_reporting_work(
        &source,
        &temporary.path().join("cached"),
        &temporary.path().join("cache"),
    );
    let bypassed = get_reporting_work_without_cache(
        &source,
        &temporary.path().join("bypassed"),
        &temporary.path().join("other"),
    );

    assert!(
        cached.file_operations >= files,
        "a run that created {files} files counted {} operations",
        cached.file_operations
    );
    assert!(
        bypassed.file_operations >= files,
        "a run that created {files} files without a cache counted {} operations",
        bypassed.file_operations
    );
}

#[test]
fn every_byte_a_run_writes_to_a_file_is_counted() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    corpus(&source);
    let cache = temporary.path().join("cache");

    get_reporting_work(&source, &temporary.path().join("first"), &cache);
    let placed = get_reporting_work(&source, &temporary.path().join("second"), &cache);
    assert!(
        placed.bytes_written > 0,
        "a run that placed every entry out of the cache reported writing nothing"
    );

    let archive = temporary.path().join("sample.tar.gz");
    std::fs::write(&archive, gzip(&greeting_tar())).unwrap();
    get_reporting_work(&archive, &temporary.path().join("cold"), &cache);
    let warm = get_reporting_work(&archive, &temporary.path().join("warm"), &cache);
    assert_eq!(
        warm.bytes_written,
        GREETING.len() as u64,
        "an extraction that wrote the greeting reported writing {} bytes",
        warm.bytes_written
    );
}

const GREETING: &[u8] = b"hello\n";

fn greeting_tar() -> Vec<u8> {
    let mut header = TarHeader::ustar(b"hello.txt", TYPEFLAG_REGULAR);
    header.set_size(GREETING.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, GREETING);
    writer.finish()
}

fn gzip(content: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

const OBJECT_BYTES: u64 = 8 * 1024 * 1024;

fn object_of(root: &Path, length: u64) -> u64 {
    std::fs::create_dir_all(root).unwrap();
    let mut bytes = vec![0_u8; usize::try_from(length).unwrap()];
    for (index, slot) in bytes.iter_mut().enumerate() {
        *slot = u8::try_from(index % 251).unwrap_or(0);
    }
    std::fs::write(root.join("obj.bin"), &bytes).unwrap();
    length
}

fn one_object(root: &Path) -> u64 {
    object_of(root, OBJECT_BYTES)
}

#[test]
fn a_run_reads_and_writes_the_source_a_whole_number_of_times() {
    const SMALLER: u64 = 8 * 1024 * 1024;
    const LARGER: u64 = 12 * 1024 * 1024;
    const RECORDS: u64 = 4096;

    for (shape, reads, writes) in [("file", 3, 2), ("directory", 2, 2)] {
        let mut counted = Vec::new();
        for length in [SMALLER, LARGER] {
            let temporary = TempDir::new().unwrap();
            let source = temporary.path().join("source");
            object_of(&source, length);
            let target = if shape == "file" {
                source.join("obj.bin")
            } else {
                source.clone()
            };
            counted.push(get_reporting_work(
                &target,
                &temporary.path().join("out"),
                &temporary.path().join("cache"),
            ));
        }
        let grew = LARGER - SMALLER;
        let read = counted[1].bytes_read - counted[0].bytes_read;
        let written = counted[1].bytes_written - counted[0].bytes_written;

        assert!(
            read.abs_diff(grew * reads) < RECORDS,
            "a {shape} run reads the object {reads} times, so growing it by {grew} bytes must \
             grow the reported reads by {}, and it grew them by {read}",
            grew * reads
        );
        assert!(
            written.abs_diff(grew * writes) < RECORDS,
            "a {shape} run writes the object {writes} times, so growing it by {grew} bytes must \
             grow the reported writes by {}, and it grew them by {written}",
            grew * writes
        );
    }
}
