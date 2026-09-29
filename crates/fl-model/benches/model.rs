//! Benches for lock parsing and writing at 10,000 files and tree hashing at 100,000 entries.

use std::sync::LazyLock;

use fl_model::digest::Digest;
use fl_model::lock::{Lock, LockedDataset, LockedFile};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::tree::{Tree, TreeEntry};

const LOCK_FILES: u64 = 10_000;
const TREE_ENTRIES: u64 = 100_000;

fn dataset_path(i: u64) -> String {
    let subject = i / 100;
    let run = i % 100;
    format!("sub-{subject:03}/eeg/sub-{subject:03}_run-{run:02}_eeg.edf")
}

fn blake3_bytes(seed: u64) -> [u8; 32] {
    *blake3::hash(&seed.to_le_bytes()).as_bytes()
}

fn md5_bytes(seed: u64) -> [u8; 16] {
    let hash = blake3::hash(&seed.rotate_left(17).to_le_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    bytes
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn locked_file(i: u64) -> LockedFile {
    let path = dataset_path(i);
    let at = format!("https://s3.amazonaws.com/openneuro.org/ds003061/{path}");
    LockedFile {
        path: DataPath::new(&path).expect("generated bench paths are valid"),
        size: 1_000_000 + i * 37,
        blake3: blake3_bytes(i),
        sha256: None,
        sha1: None,
        md5: Some(md5_bytes(i)),
        at: vec![at],
        from: None,
        role: None,
    }
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn locked_dataset() -> LockedDataset {
    let files: Vec<LockedFile> = (0..LOCK_FILES).map(locked_file).collect();
    let entries = files
        .iter()
        .map(|file| TreeEntry {
            path: file.path.clone(),
            size: file.size,
            blake3: file.blake3,
        })
        .collect();
    let tree = Tree::new(entries)
        .expect("generated bench paths are unique")
        .hash();
    LockedDataset {
        name: Name::new("eeg").expect("eeg is a valid dataset name"),
        reference: "openneuro:ds003061@1.1.0"
            .parse()
            .expect("a valid reference"),
        spec: Digest::Blake3([0x1c; 32]),
        resolved: "openneuro:ds003061@1.1.0"
            .parse()
            .expect("a valid reference"),
        title: Some("EEG Motor Movement/Imagery".to_owned()),
        license: Some("CC0-1.0".to_owned()),
        doi: Some("10.18112/openneuro.ds003061.v1.1.0".to_owned()),
        retrieved: "2026-09-28T10:00:00Z".parse().expect("a valid timestamp"),
        tree,
        files: Some(files),
    }
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn lock() -> Lock {
    Lock::new(vec![locked_dataset()], Vec::new()).expect("the generated dataset is valid")
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn tree() -> Tree {
    let entries = (0..TREE_ENTRIES)
        .map(|i| TreeEntry {
            path: DataPath::new(&dataset_path(i)).expect("generated bench paths are valid"),
            size: 1_000_000 + i * 37,
            blake3: blake3_bytes(i),
        })
        .collect();
    Tree::new(entries).expect("generated bench paths are unique")
}

static LOCK: LazyLock<Lock> = LazyLock::new(lock);
static LOCK_TEXT: LazyLock<String> = LazyLock::new(|| LOCK.to_toml());
static TREE: LazyLock<Tree> = LazyLock::new(tree);

#[divan::bench]
fn lock_parse(bencher: divan::Bencher) {
    bencher
        .with_inputs(|| LOCK_TEXT.as_str())
        .bench_values(Lock::from_toml);
}

#[divan::bench]
fn lock_write(bencher: divan::Bencher) {
    bencher.with_inputs(|| &*LOCK).bench_values(Lock::to_toml);
}

#[divan::bench]
fn tree_hash(bencher: divan::Bencher) {
    bencher.with_inputs(|| &*TREE).bench_values(Tree::hash);
}

fn main() {
    divan::main();
}
