//! Garbage collection against registered locks, on the real filesystem.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_model::lock::{Lock, LockedDataset, LockedFile, LockedStep};
use fl_model::name::Name;
use fl_model::partial::{PartialRecord, Validator};
use fl_model::path::DataPath;
use fl_model::tree::{Tree, TreeEntry};
use fl_store::error::StoreError;
use fl_store::gc::{Collect, Collected};
use fl_store::tier::{Sharing, Tier};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

const HOUR: Duration = Duration::from_secs(3600);

struct Store {
    temp: TempDir,
    tier: Tier,
}

impl Store {
    fn new() -> Result<Self, Box<dyn Error>> {
        let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
        Ok(Self { temp, tier })
    }

    fn object(&self, bytes: &[u8]) -> Result<[u8; 32], Box<dyn Error>> {
        let mut staged = self.tier.stage(None)?;
        staged.write_all(bytes)?;
        match self.tier.ingest(staged)?.digest {
            Digest::Blake3(blake3) => Ok(blake3),
            other => Err(format!("not blake3: {other}").into()),
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.temp.path().join("tier").join("v1").join(relative)
    }

    fn object_path(&self, blake3: [u8; 32]) -> Result<PathBuf, Box<dyn Error>> {
        Ok(self
            .tier
            .locate(&Digest::Blake3(blake3))
            .ok_or("object gone")?
            .path)
    }

    fn lock(&self, project: &str, lock: &Lock) -> Result<PathBuf, Box<dyn Error>> {
        let path = self.temp.path().join(project).join("data.lock");
        fs::create_dir_all(path.parent().ok_or("no parent")?)?;
        fs::write(&path, lock.to_toml())?;
        self.tier.register_root(&path)?;
        Ok(path)
    }

    fn collect(&self, after: Duration, dry_run: bool) -> Result<Collected, Box<dyn Error>> {
        Ok(self.tier.collect(&Collect {
            now: SystemTime::now() + after,
            grace: HOUR,
            dry_run,
        })?)
    }
}

fn entry(path: &str, blake3: [u8; 32]) -> Result<TreeEntry, Box<dyn Error>> {
    Ok(TreeEntry {
        path: DataPath::new(path)?,
        size: 1,
        blake3,
    })
}

fn dataset(
    name: &str,
    entries: &[TreeEntry],
    inline: bool,
) -> Result<LockedDataset, Box<dyn Error>> {
    let tree = Tree::new(entries.to_vec())?.hash();
    let files = entries
        .iter()
        .map(|entry| LockedFile {
            path: entry.path.clone(),
            size: entry.size,
            blake3: entry.blake3,
            sha256: None,
            sha1: None,
            md5: None,
            at: Vec::new(),
            from: None,
            role: None,
        })
        .collect();
    Ok(LockedDataset {
        name: Name::new(name)?,
        reference: "zenodo:1".parse()?,
        spec: Digest::Blake3([1; 32]),
        resolved: "zenodo:1".parse()?,
        title: None,
        license: None,
        doi: None,
        retrieved: "2026-10-19T10:00:00Z".parse()?,
        tree,
        files: inline.then_some(files),
    })
}

fn write_tree(store: &Store, entries: &[TreeEntry]) -> Result<Digest, Box<dyn Error>> {
    let tree = Tree::new(entries.to_vec())?;
    let digest = tree.hash();
    let name = digest.to_string();
    let hex = name.split_once(':').ok_or("no colon")?.1;
    fs::write(store.path("trees").join(hex), tree.encode())?;
    Ok(digest)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

#[test]
fn old_unmarked_files_go_and_needed_or_young_ones_stay() -> TestResult {
    let store = Store::new()?;
    let needed = store.object(b"needed")?;
    let unneeded = store.object_path(store.object(b"unneeded")?)?;
    let lock = Lock::new(
        vec![dataset("a", &[entry("x", needed)?], true)?],
        Vec::new(),
    )?;
    store.lock("project", &lock)?;
    let leftover = store.path("tmp").join("dead-writer");
    fs::write(&leftover, b"half")?;
    assert_eq!(store.collect(Duration::ZERO, false)?, Collected::default());
    let collected = store.collect(2 * HOUR, false)?;
    let mut expected = vec![unneeded, leftover.clone()];
    expected.sort_unstable();
    assert_eq!(collected.removed, expected);
    assert_eq!(collected.bytes, 8 + 4);
    assert!(exists(&store.object_path(needed)?));
    assert!(!exists(&leftover));
    Ok(())
}

#[test]
fn a_dry_run_lists_without_removing() -> TestResult {
    let store = Store::new()?;
    let unneeded = store.object(b"unneeded")?;
    let path = store.object_path(unneeded)?;
    let gone = store.lock("gone", &Lock::new(Vec::new(), Vec::new())?)?;
    fs::remove_file(&gone)?;
    let collected = store.collect(2 * HOUR, true)?;
    assert_eq!(collected.removed, std::slice::from_ref(&path));
    assert_eq!(collected.dropped_roots, [std::path::absolute(&gone)?]);
    assert!(exists(&path));
    assert_eq!(store.tier.roots()?.len(), 1);
    let collected = store.collect(2 * HOUR, false)?;
    assert_eq!(collected.dropped_roots.len(), 1);
    assert!(!exists(&path));
    assert!(store.tier.roots()?.is_empty());
    Ok(())
}

#[test]
fn tree_objects_and_step_results_keep_what_they_list() -> TestResult {
    let store = Store::new()?;
    let in_tree = store.object(b"in a tree")?;
    let in_step = store.object(b"step output")?;
    let dataset_entries = [entry("big", in_tree)?];
    let dataset_tree = write_tree(&store, &dataset_entries)?;
    let step_tree = write_tree(&store, &[entry("out", in_step)?])?;
    let key = Digest::Blake3([9; 32]);
    let record = store.path("steps").join("09".repeat(32));
    fs::write(&record, b"record")?;
    let step = LockedStep {
        name: Name::new("s")?,
        key,
        items: None,
        tree: step_tree,
    };
    let lock = Lock::new(vec![dataset("a", &dataset_entries, false)?], vec![step])?;
    store.lock("project", &lock)?;
    let collected = store.collect(2 * HOUR, false)?;
    assert!(collected.removed.is_empty(), "{:?}", collected.removed);
    assert!(exists(&record));
    assert_ne!(dataset_tree, step_tree);
    Ok(())
}

#[test]
fn what_cannot_be_read_stops_gc_before_anything_goes() -> TestResult {
    let store = Store::new()?;
    let unneeded = store.object(b"unneeded")?;
    let missing_tree = Lock::new(
        vec![dataset("a", &[entry("x", [5; 32])?], false)?],
        Vec::new(),
    )?;
    let lock = store.lock("tree", &missing_tree)?;
    let error = store.collect(2 * HOUR, false).err().ok_or("gc ran")?;
    assert!(error.to_string().contains("is missing, and"), "{error}");
    fs::write(&lock, "version = 2\n")?;
    let error = store.tier.collect(&Collect {
        now: SystemTime::now() + 2 * HOUR,
        grace: HOUR,
        dry_run: false,
    });
    assert_eq!(
        error.err().ok_or("gc ran")?.kind(),
        ErrorKind::ProjectInvalidLock
    );
    assert!(exists(&store.object_path(unneeded)?));
    Ok(())
}

#[test]
fn a_file_exactly_as_old_as_the_grace_period_stays() -> TestResult {
    let store = Store::new()?;
    let leftover = store.path("tmp").join("dead-writer");
    fs::write(&leftover, b"half")?;
    let modified = fs::metadata(&leftover)?.modified()?;
    let at_grace = store.tier.collect(&Collect {
        now: modified + HOUR,
        grace: HOUR,
        dry_run: false,
    })?;
    assert!(at_grace.removed.is_empty(), "{:?}", at_grace.removed);
    assert!(exists(&leftover));
    let past_grace = store.tier.collect(&Collect {
        now: modified + HOUR + Duration::from_secs(1),
        grace: HOUR,
        dry_run: false,
    })?;
    assert_eq!(past_grace.removed, [leftover]);
    Ok(())
}

#[test]
fn a_tree_object_path_that_is_a_directory_stops_gc() -> TestResult {
    let store = Store::new()?;
    let entries = [entry("x", [5; 32])?];
    let digest = Tree::new(entries.to_vec())?.hash();
    let name = digest.to_string();
    let hex = name.split_once(':').ok_or("no colon")?.1;
    fs::create_dir(store.path("trees").join(hex))?;
    let lock = Lock::new(vec![dataset("a", &entries, false)?], Vec::new())?;
    store.lock("project", &lock)?;
    let error = collect_error(&store)?;
    assert!(
        matches!(error, StoreError::Io { op: "read", .. }),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_directory_in_tmp_older_than_the_grace_period_stops_gc() -> TestResult {
    let store = Store::new()?;
    let stray = store.path("tmp").join("stray");
    fs::create_dir(&stray)?;
    let error = collect_error(&store)?;
    assert!(
        matches!(error, StoreError::Io { op: "remove", .. }),
        "{error}"
    );
    assert!(stray.is_dir());
    Ok(())
}

fn collect_error(store: &Store) -> Result<StoreError, Box<dyn Error>> {
    store
        .tier
        .collect(&Collect {
            now: SystemTime::now() + 2 * HOUR,
            grace: HOUR,
            dry_run: false,
        })
        .err()
        .ok_or_else(|| "gc ran".into())
}

fn set_age(path: &Path, age: Duration) -> TestResult {
    #[cfg(unix)]
    let file = fs::File::open(path)?;
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new().access_mode(0x100).open(path)?
    };
    file.set_modified(SystemTime::now() - age)?;
    Ok(())
}

#[test]
fn an_object_ingested_again_is_kept_through_the_grace_period() -> TestResult {
    let store = Store::new()?;
    let blake3 = store.object(b"found again")?;
    let path = store.object_path(blake3)?;
    set_age(&path, 3 * HOUR)?;
    store.object(b"found again")?;
    assert!(store.collect(Duration::ZERO, false)?.removed.is_empty());
    set_age(&path, 3 * HOUR)?;
    assert_eq!(store.collect(Duration::ZERO, false)?.removed, [path]);
    Ok(())
}

#[test]
fn hidden_files_left_by_nfs_are_passed_over() -> TestResult {
    let store = Store::new()?;
    let blake3 = store.object(b"kept company")?;
    let fan = store.object_path(blake3)?;
    let fan = fan.parent().ok_or("no fan out directory")?;
    let hidden = [fan.join(".nfs0001"), store.path("objects").join(".nfs0002")];
    for path in &hidden {
        fs::write(path, b"")?;
    }
    let collected = store.collect(2 * HOUR, false)?;
    assert_eq!(collected.removed.len(), 1);
    assert_eq!(store.tier.verify(|_| true)?.checked, 0);
    assert!(hidden.iter().all(|path| exists(path)));
    Ok(())
}

#[test]
fn a_stale_partial_goes_and_a_fresh_one_stays() -> TestResult {
    let store = Store::new()?;
    let mut stale = store
        .tier
        .partial(&Digest::Blake3([1; 32]), SystemTime::now())?;
    stale.write_at(0, b"old")?;
    let stale_path = stale.path().to_owned();
    drop(stale);
    let mut fresh = store
        .tier
        .partial(&Digest::Blake3([2; 32]), SystemTime::now())?;
    fresh.write_at(0, b"new")?;
    let fresh_path = fresh.path().to_owned();
    drop(fresh);
    set_age(&stale_path, 2 * HOUR)?;
    let collected = store.tier.collect(&Collect {
        now: SystemTime::now(),
        grace: HOUR,
        dry_run: false,
    })?;
    assert_eq!(collected.removed, std::slice::from_ref(&stale_path));
    assert!(!exists(&stale_path));
    assert!(exists(&fresh_path));
    Ok(())
}

fn record() -> PartialRecord {
    PartialRecord {
        location: "https://host.org/a".to_owned(),
        length: None,
        validator: Validator::None,
        received: 3,
    }
}

#[test]
fn a_download_record_goes_with_its_bytes_and_alone_by_its_own_age() -> TestResult {
    let store = Store::new()?;
    let mut writing = store
        .tier
        .partial(&Digest::Blake3([1; 32]), SystemTime::now())?;
    writing.save(&record())?;
    let mut done = store
        .tier
        .partial(&Digest::Blake3([2; 32]), SystemTime::now())?;
    done.save(&record())?;
    let orphan = store.path("partial").join("orphan.json");
    fs::write(&orphan, b"{}")?;
    let writing_record = writing.path().with_extension("json");
    let done_record = done.path().with_extension("json");
    let done_part = done.path().to_owned();
    drop(done);
    for path in [&writing_record, &done_record, &done_part, &orphan] {
        set_age(path, 2 * HOUR)?;
    }
    let collected = store.tier.collect(&Collect {
        now: SystemTime::now(),
        grace: HOUR,
        dry_run: false,
    })?;
    let mut expected = vec![done_part, done_record, orphan];
    expected.sort_unstable();
    assert_eq!(collected.removed, expected);
    assert!(exists(&writing_record));
    assert!(exists(writing.path()));
    Ok(())
}

#[test]
fn an_object_a_download_finds_again_is_kept_through_the_grace_period() -> TestResult {
    let store = Store::new()?;
    let blake3 = store.object(b"old")?;
    let path = store.object_path(blake3)?;
    set_age(&path, 3 * HOUR)?;
    let mut partial = store
        .tier
        .partial(&Digest::Blake3([3; 32]), SystemTime::now())?;
    partial.write_at(0, b"old")?;
    let ingested = store.tier.ingest_partial(partial, blake3, 3)?;
    assert!(!ingested.new);
    assert!(store.collect(Duration::ZERO, false)?.removed.is_empty());
    Ok(())
}

#[test]
fn the_index_entry_of_a_removed_object_goes_and_that_of_a_kept_one_stays() -> TestResult {
    let store = Store::new()?;
    let needed = store.object(b"needed")?;
    let unneeded = store.object(b"unneeded")?;
    let lock = Lock::new(
        vec![dataset("a", &[entry("x", needed)?], true)?],
        Vec::new(),
    )?;
    store.lock("project", &lock)?;
    let (kept, gone) = (Digest::Md5([1; 16]), Digest::Md5([2; 16]));
    store.tier.remember(&[kept], needed)?;
    store.tier.remember(&[gone], unneeded)?;
    assert!(store.collect(Duration::ZERO, false)?.removed.is_empty());
    let collected = store.collect(2 * HOUR, false)?;
    let entry_path = store
        .path("index")
        .join("md5")
        .join("02")
        .join("02".repeat(15));
    assert!(
        collected.removed.contains(&entry_path),
        "{:?}",
        collected.removed
    );
    assert!(!exists(&entry_path));
    assert_eq!(store.tier.recall(&[kept], None), Some(needed));
    assert_eq!(store.tier.recall(&[gone], None), None);
    Ok(())
}
