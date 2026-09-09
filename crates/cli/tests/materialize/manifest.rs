//! A reference that names a manifest resolves to every artifact the manifest
//! names.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions, where the run that failed is the message"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::support;
use fetchloom_faults::{TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter};
use flate2::Compression;
use flate2::write::GzEncoder;

use tempfile::TempDir;

fn scratch() -> &'static Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

fn archive(directory: &str, name: &str, body: &[u8]) -> Vec<u8> {
    let mut writer = TarWriter::new();
    writer.push(
        &TarHeader::ustar(format!("{directory}/").as_bytes(), TYPEFLAG_DIRECTORY),
        b"",
    );
    let mut entry = TarHeader::ustar(format!("{directory}/{name}").as_bytes(), TYPEFLAG_REGULAR);
    entry.set_size(body.len() as u64).set_mode(0o644);
    writer.push(&entry, body);
    let tar = writer.finish();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

struct Scene {
    _temporary: TempDir,
    root: PathBuf,
    manifest: PathBuf,
    destination: PathBuf,
    cache: PathBuf,
    lock: PathBuf,
}

fn two_artifacts(body: &str) -> Scene {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path().to_path_buf();
    std::fs::write(
        root.join("tools.tar.gz"),
        archive("tools", "run.sh", b"one\n"),
    )
    .unwrap();
    std::fs::write(
        root.join("data.tar.gz"),
        archive("data", "rows.csv", body.as_bytes()),
    )
    .unwrap();
    let manifest = root.join("dataset.yaml");
    std::fs::write(
        &manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: data\n    sources: [data.tar.gz]\n",
    )
    .unwrap();
    Scene {
        destination: root.join("out"),
        cache: root.join("cache"),
        lock: root.join("fetchloom.lock"),
        manifest,
        root,
        _temporary: temporary,
    }
}

fn get(scene: &Scene, extra: &[&str]) -> Output {
    support::fetchloom()
        .current_dir(scratch())
        .arg("get")
        .arg(&scene.manifest)
        .arg("--output")
        .arg(&scene.destination)
        .arg("--lock")
        .arg(&scene.lock)
        .arg("--json")
        .args(extra)
        .env("FETCHLOOM_CACHE_DIR", &scene.cache)
        .output()
        .unwrap()
}

fn body(output: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).unwrap_or_else(|error| {
        panic!(
            "the result was not JSON ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_manifest_materializes_every_artifact_it_names() {
    let scene = two_artifacts("a,b\n");
    let run = get(&scene, &[]);
    assert!(run.status.success(), "the run failed: {}", stderr(&run));
    assert_eq!(
        std::fs::read_to_string(scene.destination.join("tools").join("run.sh")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(scene.destination.join("data").join("rows.csv")).unwrap(),
        "a,b\n"
    );
    assert_eq!(body(&run)["dataset"], "pair");
}

#[test]
fn a_manifest_pins_every_artifact_it_names() {
    let scene = two_artifacts("a,b\n");
    assert!(get(&scene, &[]).status.success());
    let text = std::fs::read_to_string(&scene.lock).unwrap();
    assert!(text.contains("pair:"), "the lock named no dataset: {text}");
    assert!(text.contains("tools:"), "the lock pinned no tools: {text}");
    assert!(text.contains("data:"), "the lock pinned no data: {text}");
    assert_eq!(
        text.matches("interop:").count(),
        2,
        "the lock did not record an interop digest for each artifact: {text}"
    );
}

#[test]
fn a_second_run_of_a_manifest_writes_nothing() {
    let scene = two_artifacts("a,b\n");
    assert!(get(&scene, &[]).status.success());
    let again = get(&scene, &[]);
    assert!(again.status.success(), "{}", stderr(&again));
    let again = body(&again);
    assert_eq!(again["status"], "unchanged");
    assert_eq!(again["work"]["bytes_written"], 0);
}

#[test]
fn an_artifact_that_cannot_be_resolved_publishes_nothing_and_pins_what_verified() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: missing\n    sources: [absent.tar.gz]\n",
    )
    .unwrap();

    let run = get(&scene, &[]);
    assert!(
        !run.status.success(),
        "a run whose artifact could not be resolved succeeded"
    );
    assert!(
        !scene.destination.exists(),
        "a run that failed published a destination"
    );
    let text = std::fs::read_to_string(&scene.lock).unwrap();
    assert!(
        text.contains("tools:"),
        "the lock did not pin the artifact that did verify: {text}"
    );
    assert!(
        !text.contains("tree:"),
        "a lock recorded a tree for a run that materialized nothing: {text}"
    );
    let receipts = scene.cache.join("receipts");
    let held = std::fs::read_dir(&receipts).map_or(0, |entries| entries.flatten().count());
    assert_eq!(held, 0, "a run that materialized nothing wrote a receipt");
}

#[test]
fn a_declared_digest_the_bytes_do_not_have_fails_on_integrity() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n    digest:\n      blake3: \"blake3:0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(
        run.status.code(),
        Some(30),
        "a declared digest the bytes do not have was accepted: {}",
        stderr(&run)
    );
    assert!(!scene.destination.exists());
}

#[test]
fn a_manifest_that_does_not_parse_says_so() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    whatever: 1\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(run.status.code(), Some(10), "{}", stderr(&run));
    let said = String::from_utf8_lossy(&run.stdout).into_owned() + &stderr(&run);
    assert!(
        said.contains("manifest.invalid"),
        "a manifest that does not parse did not say so: {said}"
    );
}

#[test]
fn two_artifacts_landing_on_one_path_is_a_collision() {
    let scene = two_artifacts("a,b\n");
    std::fs::write(
        scene.root.join("same.tar.gz"),
        archive("tools", "run.sh", b"two\n"),
    )
    .unwrap();
    std::fs::write(
        &scene.manifest,
        "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n  - id: other\n    sources: [same.tar.gz]\n",
    )
    .unwrap();
    let run = get(&scene, &[]);
    assert_eq!(
        run.status.code(),
        Some(70),
        "two artifacts writing one path were not refused: {}",
        stderr(&run)
    );
    assert!(!scene.destination.exists());
}

#[test]
fn a_local_manifest_past_the_bound_is_refused_rather_than_read_whole() {
    let scene = two_artifacts("a,b\n");
    let padded = format!(
        "name: {}\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n",
        "x".repeat(17 * 1024 * 1024)
    );
    std::fs::write(&scene.manifest, padded).unwrap();
    let run = get(&scene, &[]);
    assert_ne!(
        run.status.code(),
        Some(0),
        "a manifest larger than the bound was accepted"
    );
    let said = stderr(&run);
    assert!(
        said.contains("resource.limit"),
        "a local manifest larger than the bound failed as {said} rather than a resource limit, so \
         a document is read whole before its size is judged"
    );
}

fn interop_of(bytes: &[u8]) -> fetchloom_engine::digest::InteropDigest {
    let processor =
        fetchloom_engine::pool::Processor::new(fetchloom_engine::threads::ThreadBudget::resolve(
            std::num::NonZeroUsize::MIN,
            Some(std::num::NonZeroUsize::MIN),
        ))
        .unwrap();
    let mut pair = fetchloom_engine::hashing::Pair::new();
    pair.update(&processor, bytes);
    pair.finish().interop
}

fn one_artifact_claiming(root: &Path, claim: &str) -> PathBuf {
    let manifest = root.join("interop.yaml");
    std::fs::write(
        &manifest,
        format!("name: pair\nartifacts:\n  - id: data\n    sources: [data.tar.gz]\n    digest:\n      sha256: \"{claim}\"\n"),
    )
    .unwrap();
    manifest
}

#[test]
fn a_sha256_the_manifest_states_is_checked_against_the_bytes() {
    let mut scene = two_artifacts("a,b\n");
    let stated = interop_of(&std::fs::read(scene.root.join("data.tar.gz")).unwrap());
    let wrong: fetchloom_engine::digest::InteropDigest =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap();
    assert_ne!(stated, wrong);
    scene.manifest = one_artifact_claiming(&scene.root, &wrong.to_string());

    let run = get(&scene, &[]);
    assert!(
        !run.status.success(),
        "a run whose only digest claim disagreed with the bytes succeeded"
    );
    assert!(
        stderr(&run).contains("integrity.mismatch"),
        "the failure was not an integrity mismatch: {}",
        stderr(&run)
    );
    assert!(
        !scene.destination.exists(),
        "a run that failed its digest claim published a destination"
    );
}

#[test]
fn a_sha256_that_matches_the_bytes_is_verified_rather_than_trusted_on_first_use() {
    let mut scene = two_artifacts("a,b\n");
    let stated = interop_of(&std::fs::read(scene.root.join("data.tar.gz")).unwrap());
    scene.manifest = one_artifact_claiming(&scene.root, &stated.to_string());

    let run = get(&scene, &[]);
    assert!(run.status.success(), "the run failed: {}", stderr(&run));
    assert_eq!(
        body(&run)["trust"],
        "verified",
        "a digest the publisher stated and the bytes matched was not evidence: {}",
        body(&run)
    );
}

#[test]
fn a_blake3_that_matches_the_bytes_is_verified_rather_than_trusted_on_first_use() {
    let scene = two_artifacts("a,b\n");
    let bytes = std::fs::read(scene.root.join("data.tar.gz")).unwrap();
    let stated = fetchloom_engine::hashing::hash_bytes(&bytes);
    std::fs::write(
        &scene.manifest,
        format!("name: pair\nartifacts:\n  - id: data\n    sources: [data.tar.gz]\n    digest:\n      blake3: \"{stated}\"\n"),
    )
    .unwrap();

    let run = get(&scene, &[]);
    assert!(run.status.success(), "the run failed: {}", stderr(&run));
    assert_eq!(
        body(&run)["trust"],
        "verified",
        "a blake3 the publisher stated and the bytes matched was not evidence: {}",
        body(&run)
    );
}

#[test]
fn a_run_that_cannot_fit_on_the_volume_is_refused_before_it_fetches_anything() {
    let scene = two_artifacts("a,b\n");
    let exabyte = 1_u64 << 60;
    std::fs::write(
        &scene.manifest,
        format!(
            "name: pair\nartifacts:\n  - id: tools\n    sources: [tools.tar.gz]\n    size: {exabyte}\n"
        ),
    )
    .unwrap();

    let run = get(&scene, &[]);

    assert_eq!(
        run.status.code(),
        Some(50),
        "a run needing an exabyte was not refused for room: {}",
        stderr(&run)
    );
    let said = body(&run);
    assert_eq!(said["kind"], "resource.disk");
    assert!(
        !scene.destination.exists(),
        "the run wrote a destination it could never have finished"
    );
    let objects = scene.cache.join("objects");
    let partials = scene.cache.join("partial");
    for held in [&objects, &partials] {
        let count = std::fs::read_dir(held).map_or(0, std::iter::Iterator::count);
        assert_eq!(
            count,
            0,
            "the run moved bytes into {} before deciding it had nowhere to put them",
            held.display()
        );
    }
}
