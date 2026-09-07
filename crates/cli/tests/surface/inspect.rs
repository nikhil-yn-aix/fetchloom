//! `probe` and `list`: what a source states, and what each format costs to
//! enumerate.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::Output;

use fetchloom_faults::{Reply, Script, TestServer, ZipLocalHeader, ZipMember, ZipWriter};
use tempfile::TempDir;

use crate::support;

fn run(cache: &Path, arguments: &[&str]) -> Output {
    support::fetchloom()
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .output()
        .unwrap()
}

fn zip_of(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut writer = ZipWriter::new();
    for (name, data) in members {
        let offset = writer.offset();
        let local = ZipLocalHeader::store(name.as_bytes(), data);
        let central = fetchloom_faults::ZipCentralHeader::from_local(&local, offset);
        writer.push(ZipMember {
            local,
            central,
            data: data.clone(),
        });
    }
    writer.finish()
}

fn tar_gz_of(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    use std::io::Write as _;

    let mut writer = fetchloom_faults::TarWriter::new();
    for (name, data) in members {
        let mut header =
            fetchloom_faults::TarHeader::ustar(name.as_bytes(), fetchloom_faults::TYPEFLAG_REGULAR);
        header.set_size(data.len() as u64);
        writer.push(&header, data);
    }
    let plain = writer.finish();
    let mut packed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    packed.write_all(&plain).unwrap();
    packed.finish().unwrap()
}

fn ranged_bytes(server: &TestServer) -> u64 {
    server
        .received()
        .iter()
        .filter_map(|request| request.header("range").map(str::to_owned))
        .filter_map(|value| {
            let span = value.strip_prefix("bytes=")?.to_owned();
            let (start, end) = span.split_once('-')?;
            let start: u64 = start.parse().ok()?;
            let end: u64 = end.parse().ok()?;
            Some(end.saturating_sub(start) + 1)
        })
        .sum()
}

#[test]
fn probe_states_the_size_and_the_ranges_without_moving_the_object() {
    let object = vec![7_u8; 4096];
    let server = TestServer::start(Script::serving(object.clone())).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(
        cache.path(),
        &[
            "probe",
            &format!("{}/object.bin", server.origin()),
            "--json",
        ],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answered: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("probe printed one JSON object");
    let artifact = &answered["artifacts"][0];
    assert_eq!(artifact["size"].as_u64(), Some(object.len() as u64));
    assert_eq!(artifact["ranges"].as_bool(), Some(true));
    assert_eq!(artifact["cached"].as_bool(), Some(false));
    assert_eq!(artifact["trust"].as_str(), Some("tofu"));
    assert!(
        server
            .received()
            .iter()
            .all(|request| request.method.eq_ignore_ascii_case("head")),
        "probe asked for the bytes rather than for what the source states"
    );
}

#[test]
fn probe_answers_unknown_and_still_exits_zero() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(cache.path(), &["probe", source.to_str().unwrap(), "--json"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answered: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(answered["artifacts"][0]["digests"].is_array());
}

#[test]
fn probe_offline_answers_from_the_cache_or_says_the_network_was_forbidden() {
    let object = vec![3_u8; 2048];
    let server = TestServer::start(Script::serving(object.clone())).unwrap();
    let cache = TempDir::new().unwrap();
    let location = format!("{}/object.bin", server.origin());

    let refused = run(cache.path(), &["probe", &location, "--offline"]);
    assert_eq!(refused.status.code(), Some(40));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("policy.offline"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let destination = TempDir::new().unwrap();
    let fetched = run(
        cache.path(),
        &[
            "get",
            &location,
            "--output",
            destination.path().join("here").to_str().unwrap(),
            "--lock",
            destination.path().join("fetchloom.lock").to_str().unwrap(),
        ],
    );
    assert_eq!(fetched.status.code(), Some(0));

    let answered = run(cache.path(), &["probe", &location, "--offline", "--json"]);
    assert_eq!(
        answered.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&answered.stderr)
    );
    let held: serde_json::Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(held["artifacts"][0]["cached"].as_bool(), Some(true));
    assert_eq!(
        held["artifacts"][0]["size"].as_u64(),
        Some(object.len() as u64)
    );
}

#[test]
fn a_zip_over_a_source_that_serves_ranges_costs_its_index_and_not_the_archive() {
    let members = vec![
        ("train/a.bin", vec![1_u8; 1 << 20]),
        ("train/b.bin", vec![2_u8; 1 << 20]),
    ];
    let archive = zip_of(&members);
    let server = TestServer::start(Script::serving(archive.clone())).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(
        cache.path(),
        &["list", &format!("{}/corpus.zip", server.origin()), "--json"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths: Vec<String> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(paths.contains(&"train/a.bin".to_owned()), "{paths:?}");
    assert!(paths.contains(&"train/b.bin".to_owned()), "{paths:?}");

    let asked = ranged_bytes(&server);
    assert!(
        asked < archive.len() as u64 / 4,
        "listing asked for {asked} of {} bytes, which is not the index alone",
        archive.len()
    );
    let requests = server.received().len();
    assert!(
        requests <= 6,
        "listing took {requests} requests where the index and one local header per member is five"
    );
}

#[test]
fn a_zip_over_a_source_that_refuses_ranges_says_what_it_cost_instead() {
    let archive = zip_of(&[("a.txt", b"hello".to_vec())]);
    let server = TestServer::start(Script::serving(archive).ranges(false)).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(
        cache.path(),
        &["list", &format!("{}/corpus.zip", server.origin())],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not serve ranges"), "{stderr}");
}

#[test]
fn a_tar_states_no_index_so_listing_it_reads_all_of_it_and_says_so_first() {
    let archive = tar_gz_of(&[("a.txt", b"hello".to_vec()), ("b.txt", b"world".to_vec())]);
    let server = TestServer::start(Script::serving(archive)).unwrap();
    let cache = TempDir::new().unwrap();
    let location = format!("{}/corpus.tar.gz", server.origin());

    let output = run(cache.path(), &["list", &location]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("states no index"), "{stderr}");

    let after_first = server.received().len();
    let again = run(cache.path(), &["list", &location]);
    assert_eq!(again.status.code(), Some(0));
    assert_eq!(
        server.received().len(),
        after_first,
        "the second listing asked the source again for bytes the cache already held"
    );
}

#[test]
fn a_truncated_central_directory_fails_rather_than_guessing() {
    let mut archive = zip_of(&[("a.txt", b"hello".to_vec())]);
    let cut = archive.len() - 8;
    archive.truncate(cut);
    let temporary = TempDir::new().unwrap();
    let path = temporary.path().join("corpus.zip");
    std::fs::write(&path, &archive).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(cache.path(), &["list", path.to_str().unwrap()]);

    assert_ne!(
        output.status.code(),
        Some(0),
        "a truncated archive was listed as though it were whole"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("archive"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_source_that_does_not_honor_the_length_it_states_still_probes_as_it_stated_it() {
    let object = vec![9_u8; 4096];
    let server = TestServer::start(Script::serving(object.clone()).replying(vec![
            Reply::Truncated {
                after: 512
            };
            8
        ]))
    .unwrap();
    let cache = TempDir::new().unwrap();
    let location = format!("{}/object.bin", server.origin());

    let answered = run(cache.path(), &["probe", &location, "--json"]);
    assert_eq!(
        answered.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&answered.stderr)
    );
    let held: serde_json::Value = serde_json::from_slice(&answered.stdout).unwrap();
    assert_eq!(
        held["artifacts"][0]["size"].as_u64(),
        Some(object.len() as u64),
        "probe reported something other than what the source stated"
    );

    let destination = TempDir::new().unwrap();
    let fetched = run(
        cache.path(),
        &[
            "get",
            &location,
            "--output",
            destination.path().join("here").to_str().unwrap(),
            "--lock",
            destination.path().join("fetchloom.lock").to_str().unwrap(),
        ],
    );
    assert_ne!(
        fetched.status.code(),
        Some(0),
        "a source that served fewer bytes than it stated was accepted"
    );
}

#[test]
fn a_directory_is_listed_by_the_same_walk_a_run_would_materialize() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("corpus");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"world").unwrap();
    let cache = TempDir::new().unwrap();

    let output = run(cache.path(), &["list", source.to_str().unwrap(), "--json"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths: Vec<String> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(paths.iter().any(|path| path == "a.txt"), "{paths:?}");
    assert!(paths.iter().any(|path| path == "nested/b.txt"), "{paths:?}");
}
