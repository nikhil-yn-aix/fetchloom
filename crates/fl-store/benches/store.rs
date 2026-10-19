//! Benches for ingest against a plain copy and hash, and for linking 10,000 objects into a project.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use fl_model::digest::Digest;
use fl_model::path::DataPath;
use fl_store::data_dir::{DataDir, Destination};
use fl_store::link::{Ladder, Linker, Mode};
use fl_store::tier::{Sharing, Tier};
use tempfile::TempDir;

const SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const BUFFER: usize = 1024 * 1024;
const OBJECTS: usize = 10_000;
const DIRECTORIES: usize = 20;
const FILES: usize = 25;

struct Source {
    dir: TempDir,
    path: PathBuf,
}

struct Objects {
    dir: TempDir,
    tier: Tier,
    digests: Vec<Digest>,
}

static SOURCE: OnceLock<Source> = OnceLock::new();
static STORED: OnceLock<Objects> = OnceLock::new();

fn main() {
    divan::main();
    if let Some(source) = SOURCE.get() {
        let _ = fs::remove_dir_all(source.dir.path());
    }
    if let Some(objects) = STORED.get() {
        let _ = fs::remove_dir_all(objects.dir.path());
    }
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn source() -> &'static Source {
    SOURCE.get_or_init(|| {
        let dir = TempDir::new().expect("a temporary directory");
        let path = dir.path().join("source.bin");
        let mut file = File::create(&path).expect("the source file");
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut buffer = vec![0_u8; BUFFER];
        for _ in 0..SOURCE_BYTES / BUFFER as u64 {
            for chunk in buffer.as_chunks_mut::<8>().0 {
                *chunk = xorshift(&mut state).to_le_bytes();
            }
            file.write_all(&buffer).expect("the source bytes");
        }
        file.sync_all().expect("the source flushed");
        Source { dir, path }
    })
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn objects() -> &'static Objects {
    STORED.get_or_init(|| {
        let dir = TempDir::new().expect("a temporary directory");
        let tier = Tier::open(&dir.path().join("tier"), Sharing::Private).expect("the tier");
        let staging = dir.path().join("sources");
        fs::create_dir(&staging).expect("the source directory");
        let digests = (0..OBJECTS)
            .map(|i| {
                let path = staging.join(format!("{i}.bin"));
                let text = format!("object {i:08} of the link bench, distinct content, 64 bytes");
                fs::write(&path, text).expect("an object source");
                tier.ingest_file(&path).expect("an ingested object").digest
            })
            .collect();
        Objects { dir, tier, digests }
    })
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn fresh_tier() -> (TempDir, Tier) {
    let dir = TempDir::new().expect("a temporary directory");
    let tier = Tier::open(&dir.path().join("tier"), Sharing::Private).expect("the tier");
    (dir, tier)
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn data_path(text: &str) -> DataPath {
    DataPath::new(text).expect("generated bench paths are valid")
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn destinations(root: &Path, digests: &[Digest]) -> Vec<(Digest, Destination)> {
    let data = DataDir::new(root.join("data"));
    digests
        .iter()
        .enumerate()
        .map(|(i, digest)| {
            let dataset = i / (DIRECTORIES * FILES);
            let directory = i / FILES % DIRECTORIES;
            let file = i % FILES;
            let dir = data_path(&format!("ds{dataset:02}/d{directory:02}"));
            let name = data_path(&format!("f{file:02}.bin"));
            let destination = data
                .destination(&dir, &name)
                .expect("a destination below the data directory");
            (*digest, destination)
        })
        .collect()
}

/// Ingests a 256 MiB file of non repeating bytes into a fresh tier.
#[divan::bench(sample_count = 10, sample_size = 1)]
#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn ingest_file(bencher: divan::Bencher) {
    let source = source();
    bencher.with_inputs(fresh_tier).bench_values(|(dir, tier)| {
        let ingested = tier.ingest_file(&source.path).expect("an ingested file");
        assert!(ingested.new);
        (ingested, dir, tier)
    });
}

/// Copies the same file and hashes the copy with a 1 MiB buffer, the baseline for ingest.
#[divan::bench(sample_count = 10, sample_size = 1)]
#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn copy_then_hash(bencher: divan::Bencher) {
    let source = source();
    bencher
        .with_inputs(|| TempDir::new().expect("a temporary directory"))
        .bench_values(|dir| {
            let copy = dir.path().join("copy.bin");
            fs::copy(&source.path, &copy).expect("the copy");
            let mut file = File::open(&copy).expect("the copy opened");
            let mut hasher = blake3::Hasher::new();
            let mut buffer = vec![0_u8; BUFFER];
            loop {
                let read = file.read(&mut buffer).expect("the copy read");
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            (hasher.finalize(), dir)
        });
}

/// Hardlinks 10,000 stored objects into 20 datasets of 20 directories of 25 files.
#[divan::bench(sample_count = 10, sample_size = 1)]
fn link_10000(bencher: divan::Bencher) {
    let objects = objects();
    bencher
        .with_inputs(|| {
            let (dir, _) = fresh_tier();
            let pairs = destinations(dir.path(), &objects.digests);
            (dir, pairs)
        })
        .bench_values(|(dir, pairs)| {
            let linker = Linker::new(&objects.tier, Ladder::only(Mode::Hardlink));
            let results = linker.link_all(&pairs);
            assert!(results.iter().all(Result::is_ok));
            (results, pairs, dir)
        });
}
