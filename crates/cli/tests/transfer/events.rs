//! That every event name the contract lists is emitted by a run that did the
//! thing the name is about.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use fetchloom_engine::event::EVENT_NAMES;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_faults::{
    IndexFormat, Latency, Reply, Script, TYPEFLAG_REGULAR, TarHeader, TarWriter, TestServer,
};
use tempfile::TempDir;

use crate::support;

struct Emitted {
    names: BTreeSet<String>,
    code: i32,
    said: String,
}

fn run_in(directory: &Path, arguments: &[&str]) -> Emitted {
    let stream = directory.join(format!("events-{}.ndjson", arguments.join("-").len()));
    let output = support::fetchloom()
        .current_dir(directory)
        .args(arguments)
        .arg("--events")
        .arg(&stream)
        .env("FETCHLOOM_CACHE_DIR", directory.join("cache"))
        .output()
        .unwrap();
    Emitted {
        names: names_in(&stream),
        code: output.status.code().unwrap_or(-1),
        said: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn names_in(stream: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(stream)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|event| event["event"].as_str().map(str::to_owned))
        .collect()
}

fn object(length: usize, seed: usize) -> Vec<u8> {
    (0..length)
        .map(|offset| u8::try_from((offset + seed * 37) % 251).unwrap_or(0))
        .collect()
}

fn tar_holding(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut writer = TarWriter::new();
    for (name, body) in entries {
        let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
        header.set_size(body.len() as u64);
        writer.push(&header, body);
    }
    writer.finish()
}

fn gzip(content: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

fn write(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let target = root.join(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&target, bytes).unwrap();
    target
}

fn a_local_directory_fetched_twice(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let source = scratch.path().join("source");
    write(&source, "a.txt", b"hello");
    write(&source, "nested/b.txt", b"world");

    for out in ["first", "second"] {
        let run = run_in(
            scratch.path(),
            &["get", source.to_str().unwrap(), "--output", out],
        );
        assert_eq!(run.code, 0, "a local directory run failed: {}", run.said);
        seen.extend(run.names);
    }
    let again = run_in(
        scratch.path(),
        &["get", source.to_str().unwrap(), "--output", "first"],
    );
    seen.extend(again.names);
}

fn an_archive_is_extracted(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let archive = write(
        scratch.path(),
        "sample.tar.gz",
        &gzip(&tar_holding(&[(b"hello.txt", b"hello\n")])),
    );
    let run = run_in(
        scratch.path(),
        &["get", archive.to_str().unwrap(), "--output", "out"],
    );
    assert_eq!(run.code, 0, "an archive run failed: {}", run.said);
    seen.extend(run.names);
}

fn an_archive_entry_is_rejected(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let archive = write(
        scratch.path(),
        "hostile.tar.gz",
        &gzip(&tar_holding(&[
            (b"safe.txt".as_slice(), b"safe\n".as_slice()),
            (b"../escaped.txt".as_slice(), b"escaped\n".as_slice()),
        ])),
    );
    let run = run_in(
        scratch.path(),
        &["get", archive.to_str().unwrap(), "--output", "out"],
    );
    assert_ne!(run.code, 0, "an archive escaping the destination succeeded");
    seen.extend(run.names);
}

fn an_object_is_transferred_and_verified(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(512 * 1024, 1);
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let manifest = format!(
        "name: served\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        server.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    assert_eq!(run.code, 0, "a served run failed: {}", run.said);
    seen.extend(run.names);
}

fn a_mismatch_is_retried(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(64 * 1024, 2);
    let server = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::Flipped { offset: 7 }]),
    )
    .unwrap();
    let manifest = format!(
        "name: flipped\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        server.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    seen.extend(run.names);
}

fn a_cut_transfer_is_resumed(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(512 * 1024, 3);
    let server = TestServer::start(
        Script::serving(bytes.clone()).replying(vec![Reply::ClosedMidBody { after: 8192 }]),
    )
    .unwrap();
    let manifest = format!(
        "name: cut\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        server.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    seen.extend(run.names);
}

fn a_refusing_source_fails_over(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(32 * 1024, 4);
    let refusing =
        TestServer::start(Script::serving(bytes.clone()).replying(vec![Reply::Status {
            code: 503,
            retry_after: None,
        }]))
        .unwrap();
    let serving = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let manifest = format!(
        "name: two\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\", \"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        refusing.origin(),
        serving.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    seen.extend(run.names);
}

fn a_listing_is_read(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let server =
        TestServer::start(
            Script::serving(object(1024, 5)).replying(vec![Reply::Listing {
                format: IndexFormat::GeneratedHtml,
            }]),
        )
        .unwrap();
    let run = run_in(
        scratch.path(),
        &[
            "get",
            &format!("{}/data/", server.origin()),
            "--output",
            "out",
        ],
    );
    seen.extend(run.names);
}

fn a_credential_is_required(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let server =
        TestServer::start(
            Script::serving(object(1024, 6)).replying(vec![Reply::Status {
                code: 401,
                retry_after: None,
            }]),
        )
        .unwrap();
    let run = run_in(
        scratch.path(),
        &[
            "get",
            &format!("{}/object.bin", server.origin()),
            "--output",
            "out",
        ],
    );
    assert_ne!(run.code, 0, "a source behind a credential served bytes");
    seen.extend(run.names);
}

fn a_second_wanter_waits_for_the_first(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(256 * 1024, 7);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .delayed(Latency::default().every_request(std::time::Duration::from_millis(400))),
    )
    .unwrap();
    let mut artifacts = String::new();
    for index in 0..4 {
        writeln!(
            artifacts,
            "  - id: object-{index}\n    sources: [\"{}/object-{index}.bin\"]\n    digest:\n      blake3: \"{}\"",
            server.origin(),
            hash_bytes(&bytes)
        )
        .unwrap();
    }
    write(
        scratch.path(),
        "dataset.yaml",
        format!("name: shared\nartifacts:\n{artifacts}").as_bytes(),
    );
    let run = run_in(
        scratch.path(),
        &[
            "get",
            "dataset.yaml",
            "--output",
            "out",
            "--concurrency",
            "4",
            "--per-host",
            "4",
        ],
    );
    seen.extend(run.names);
}

fn an_edited_directory_meets_a_moved_upstream(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let source = scratch.path().join("source");
    write(&source, "a.txt", b"hello");
    write(&source, "nested/b.txt", b"world");

    let first = run_in(
        scratch.path(),
        &["get", source.to_str().unwrap(), "--output", "out"],
    );
    assert_eq!(first.code, 0, "the first run failed: {}", first.said);
    seen.extend(first.names);

    write(&scratch.path().join("out"), "a.txt", b"mine");
    write(&source, "nested/b.txt", b"moved");

    let merged = run_in(
        scratch.path(),
        &["get", source.to_str().unwrap(), "--output", "out"],
    );
    assert_eq!(merged.code, 0, "the merging run failed: {}", merged.said);
    assert!(
        merged.names.contains("merge.resolution"),
        "a three way run emitted no resolution"
    );
    seen.extend(merged.names);
}
#[test]
fn every_event_name_the_contract_lists_is_emitted_by_a_run() {
    let mut seen = BTreeSet::new();
    a_local_directory_fetched_twice(&mut seen);
    an_archive_is_extracted(&mut seen);
    an_archive_entry_is_rejected(&mut seen);
    an_object_is_transferred_and_verified(&mut seen);
    a_mismatch_is_retried(&mut seen);
    a_cut_transfer_is_resumed(&mut seen);
    a_refusing_source_fails_over(&mut seen);
    a_listing_is_read(&mut seen);
    a_credential_is_required(&mut seen);
    a_second_wanter_waits_for_the_first(&mut seen);
    a_dead_source_fails_over(&mut seen);
    a_second_run_resumes_a_partial(&mut seen);
    a_credential_is_offered_and_declined(&mut seen);
    a_bare_name_resolves_to_a_location(&mut seen);
    one_run_waits_for_another(&mut seen);
    a_damaged_object_is_repaired_by_range(&mut seen);
    an_edited_directory_meets_a_moved_upstream(&mut seen);

    let unnamed: Vec<&str> = seen
        .iter()
        .map(String::as_str)
        .filter(|name| !EVENT_NAMES.contains(name))
        .collect();
    assert!(
        unnamed.is_empty(),
        "runs emitted events the contract does not list: {unnamed:?}"
    );

    let missing: Vec<&str> = EVENT_NAMES
        .iter()
        .filter(|name| !seen.contains(**name))
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "the contract lists these event names and no run in this file emits one, so nothing proves a reader will ever see them: {missing:?}"
    );
}

fn seed_measurement(cache: &Path, host: &str, throughput: u64) {
    let work = std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new());
    let processor = std::sync::Arc::new(
        fetchloom_engine::pool::Processor::new(fetchloom_engine::threads::ThreadBudget::resolve(
            std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
            None,
        ))
        .unwrap(),
    );
    let held: fetchloom_cache::Cache<fetchloom_platform::NativePlatform> =
        fetchloom_cli::cache::require(
            cache,
            fetchloom_engine::compression::CompressionChoice::Auto,
            work,
            processor,
        )
        .unwrap();
    held.record_measurement(
        host,
        &fetchloom_engine::tuning::HostMeasurement {
            concurrency: 1,
            throughput,
            time_to_first_byte_ms: 1,
            observed_at: fetchloom_engine::timestamp::Timestamp::now(),
        },
    )
    .unwrap();
}

fn a_dead_source_fails_over(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(32 * 1024, 8);
    let dead = TestServer::start_on(
        "::1",
        Script::serving(bytes.clone()).replying(vec![
            Reply::ClosedMidBody { after: 1024 },
            Reply::ClosedMidBody { after: 1024 },
            Reply::ClosedMidBody { after: 1024 },
            Reply::ClosedMidBody { after: 1024 },
        ]),
    )
    .unwrap();
    let serving = TestServer::start_on("127.0.0.1", Script::serving(bytes.clone())).unwrap();
    let cache = scratch.path().join("cache");
    seed_measurement(&cache, "::1", 1_000_000_000);
    seed_measurement(&cache, "127.0.0.1", 1);
    let manifest = format!(
        "name: failover\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\", \"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        dead.origin(),
        serving.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(
        scratch.path(),
        &["get", "dataset.yaml", "--output", "out", "--retries", "1"],
    );
    assert_eq!(run.code, 0, "a failover run failed: {}", run.said);
    seen.extend(run.names);
}

fn a_second_run_resumes_a_partial(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(512 * 1024, 9);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![
                Reply::ClosedMidBody { after: 16_384 },
                Reply::ClosedMidBody { after: 16_384 },
            ]),
    )
    .unwrap();
    let manifest = format!(
        "name: partial\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        server.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());

    let cut = run_in(
        scratch.path(),
        &["get", "dataset.yaml", "--output", "out", "--retries", "1"],
    );
    seen.extend(cut.names);
    let resumed = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    assert_eq!(
        resumed.code, 0,
        "the run that should have resumed failed: {}",
        resumed.said
    );
    seen.extend(resumed.names);
}

fn a_credential_is_offered_and_declined(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(4096, 10);
    let gated = TestServer::start_on(
        "::1",
        Script::serving(Vec::new()).replying(vec![Reply::Status {
            code: 401,
            retry_after: None,
        }]),
    )
    .unwrap();
    let open = TestServer::start_on("127.0.0.1", Script::serving(bytes.clone())).unwrap();

    let cache = scratch.path().join("cache");
    seed_measurement(&cache, "::1", 1_000_000_000);
    seed_measurement(&cache, "127.0.0.1", 1);

    let manifest = format!(
        "name: mirrored\nartifacts:\n  - id: object\n    sources: [\"{}/object\", \"{}/object\"]\n    digest:\n      blake3: \"{}\"\n",
        gated.origin(),
        open.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());
    let run = run_in(scratch.path(), &["get", "dataset.yaml", "--output", "out"]);
    assert_eq!(
        run.code, 0,
        "the run did not finish with the alternative: {}",
        run.said
    );
    seen.extend(run.names);
}

fn a_bare_name_resolves_to_a_location(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let server = TestServer::start(Script::serving(object(2048, 11))).unwrap();
    write(
        scratch.path(),
        "fetchloom.toml",
        format!("sources = [\"{}/data/\"]\n", server.origin()).as_bytes(),
    );
    let stream = scratch.path().join("alias.ndjson");
    let output = support::fetchloom_reading_configuration()
        .current_dir(scratch.path())
        .args(["get", "silesia", "--output", "out", "--events"])
        .arg(&stream)
        .env("FETCHLOOM_CACHE_DIR", scratch.path().join("cache"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "a bare name did not resolve: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    seen.extend(names_in(&stream));
}

fn one_run_waits_for_another(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(1024 * 1024, 12);
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .delayed(Latency::default().every_request(std::time::Duration::from_millis(1200))),
    )
    .unwrap();
    let manifest = format!(
        "name: contended\nartifacts:\n  - id: object\n    sources: [\"{}/object.bin\"]\n    digest:\n      blake3: \"{}\"\n",
        server.origin(),
        hash_bytes(&bytes)
    );
    write(scratch.path(), "dataset.yaml", manifest.as_bytes());

    let cache = scratch.path().join("cache");
    let mut running = Vec::new();
    for index in 0..2 {
        let stream = scratch.path().join(format!("wait-{index}.ndjson"));
        let child = support::fetchloom()
            .current_dir(scratch.path())
            .args(["get", "dataset.yaml", "--output"])
            .arg(format!("out-{index}"))
            .arg("--events")
            .arg(&stream)
            .env("FETCHLOOM_CACHE_DIR", &cache)
            .spawn()
            .unwrap();
        running.push((child, stream));
    }
    for (mut child, stream) in running {
        let status = child.wait().unwrap();
        assert!(status.success(), "a contending run failed");
        seen.extend(names_in(&stream));
    }
}

fn a_damaged_object_is_repaired_by_range(seen: &mut BTreeSet<String>) {
    let scratch = TempDir::new().unwrap();
    let bytes = object(4 * 1024 * 1024, 13);
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let location = format!("{}/object.bin", server.origin());
    let cache = scratch.path().join("cache");

    let filled = run_in(scratch.path(), &["get", &location, "--output", "out"]);
    assert_eq!(filled.code, 0, "the filling run failed: {}", filled.said);
    seen.extend(filled.names);

    damage_one_object(&cache);

    let stream = scratch.path().join("repair.ndjson");
    let output = support::fetchloom()
        .current_dir(scratch.path())
        .args(["repair", &location, "--events"])
        .arg(&stream)
        .env("FETCHLOOM_CACHE_DIR", &cache)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "the repair failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    seen.extend(names_in(&stream));
}

fn damage_one_object(cache: &Path) {
    let mut pending = vec![cache.join("objects")];
    let mut damaged = 0_usize;
    while let Some(directory) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let mut held = std::fs::read(&path).unwrap();
            let at = held.len() / 2;
            held[at] ^= 0xff;
            std::fs::write(&path, &held).unwrap();
            damaged += 1;
        }
    }
    assert_eq!(damaged, 1, "the cache held {damaged} objects to damage");
}
