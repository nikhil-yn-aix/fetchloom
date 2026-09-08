//! Contract tests over the record a run leaves beside a destination: `status`,
//! `diff`, `revert`, `promote`, and the three way compare `get` makes when
//! upstream has moved under a destination that was edited.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::support;

use tempfile::TempDir;

struct Bench {
    _temporary: TempDir,
    cache: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

fn bench() -> Bench {
    let temporary = TempDir::new().unwrap();
    let cache = temporary.path().join("cache");
    let source = temporary.path().join("source");
    std::fs::create_dir_all(source.join("nested")).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    std::fs::write(source.join("nested").join("b.txt"), b"world").unwrap();
    Bench {
        destination: temporary.path().join("destination"),
        _temporary: temporary,
        cache,
        source,
    }
}

impl Bench {
    fn run(&self, arguments: &[&str]) -> Output {
        support::fetchloom()
            .current_dir(scratch())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", &self.cache)
            .output()
            .unwrap()
    }

    fn get(&self) -> Output {
        self.run(&[
            "get",
            self.source.to_str().unwrap(),
            "--output",
            self.destination.to_str().unwrap(),
        ])
    }

    fn status(&self) -> Output {
        self.run(&["status", self.destination.to_str().unwrap()])
    }

    fn at(&self, path: &str) -> PathBuf {
        self.destination.join(path)
    }
}

fn scratch() -> &'static Path {
    static SCRATCH: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    SCRATCH.get_or_init(|| TempDir::new().unwrap()).path()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_destination_nothing_touched_reports_no_entry_at_all() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    let status = bench.status();
    assert_eq!(status.status.code(), Some(0), "stderr was {}", err(&status));
    assert_eq!(out(&status), "", "a clean destination named an entry");
}

#[test]
fn the_four_states_are_reported_one_line_each_and_nothing_else() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::remove_file(bench.at("nested/b.txt")).unwrap();
    std::fs::write(bench.at("added.txt"), b"new").unwrap();

    let status = bench.status();
    assert_eq!(status.status.code(), Some(0), "stderr was {}", err(&status));
    let lines: Vec<String> = out(&status).lines().map(str::to_owned).collect();
    assert!(
        lines.iter().any(|line| line.starts_with("modified")
            && line.contains("a.txt")
            && !line.contains("added")),
        "modified was not reported: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("deleted") && line.contains("nested/b.txt")),
        "deleted was not reported: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("added") && line.contains("added.txt")),
        "added was not reported: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("unchanged")),
        "an unchanged entry was printed: {lines:?}"
    );
}

#[test]
fn a_destination_with_no_record_is_refused_rather_than_compared_against_nothing() {
    let bench = bench();
    std::fs::create_dir_all(&bench.destination).unwrap();
    std::fs::write(bench.at("a.txt"), b"hello").unwrap();
    let status = bench.status();
    assert_eq!(status.status.code(), Some(10));
    assert!(
        err(&status).contains("records what was written"),
        "stderr was {}",
        err(&status)
    );
}

#[test]
fn diff_states_what_each_changed_entry_was_and_what_it_is_now() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"a longer thing").unwrap();

    let differences = bench.run(&["diff", bench.destination.to_str().unwrap()]);
    assert_eq!(differences.status.code(), Some(0));
    let line = out(&differences);
    assert!(line.contains("a.txt"), "diff named no entry: {line}");
    assert!(line.contains("->"), "diff stated no difference: {line}");
    assert!(
        line.contains(" 5 ") && line.contains(" 14"),
        "diff stated neither length: {line}"
    );
}

#[test]
fn revert_puts_back_every_changed_entry_when_none_is_named() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::remove_file(bench.at("nested/b.txt")).unwrap();
    std::fs::write(bench.at("added.txt"), b"new").unwrap();

    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap()]);
    assert_eq!(
        reverted.status.code(),
        Some(0),
        "stderr was {}",
        err(&reverted)
    );
    assert_eq!(std::fs::read(bench.at("a.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::read(bench.at("nested/b.txt")).unwrap(), b"world");
    assert!(!bench.at("added.txt").exists(), "an added entry survived");
    assert_eq!(out(&bench.status()), "");
}

#[test]
fn revert_of_one_entry_leaves_every_other_edit_alone() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::write(bench.at("nested/b.txt"), b"also mine").unwrap();

    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap(), "a.txt"]);
    assert_eq!(
        reverted.status.code(),
        Some(0),
        "stderr was {}",
        err(&reverted)
    );
    assert_eq!(std::fs::read(bench.at("a.txt")).unwrap(), b"hello");
    assert_eq!(
        std::fs::read(bench.at("nested/b.txt")).unwrap(),
        b"also mine"
    );
}

#[test]
fn revert_of_an_entry_the_record_does_not_name_is_refused() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap(), "absent.txt"]);
    assert_eq!(reverted.status.code(), Some(10));
    assert!(
        err(&reverted).contains("absent.txt"),
        "stderr was {}",
        err(&reverted)
    );
}

#[test]
fn revert_with_the_object_pruned_names_what_is_missing_and_fetches_nothing() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    let objects = bench.cache.join("objects");
    remove_every_object(&objects);
    let packs = bench.cache.join("packs");
    remove_every_object(&packs);

    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap()]);
    assert_eq!(
        reverted.status.code(),
        Some(80),
        "stdout was {} and stderr was {}",
        out(&reverted),
        err(&reverted)
    );
    let said = err(&reverted);
    assert!(
        said.contains("a.txt"),
        "the failure did not name the entry it could not restore: {said}"
    );
    assert!(
        said.contains("cache.corrupt"),
        "the failure did not name the kind a missing object is: {said}"
    );
    assert!(
        said.contains("blake3:"),
        "the failure did not name the object it could not restore: {said}"
    );
    assert_eq!(
        std::fs::read(bench.at("a.txt")).unwrap(),
        b"mine",
        "a failed revert changed the destination"
    );
}

fn remove_every_object(root: &Path) {
    let Ok(listing) = std::fs::read_dir(root) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_every_object(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[test]
fn promote_writes_a_manifest_that_states_what_it_was_derived_from() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine now").unwrap();

    let manifest = bench.destination.with_file_name("promoted.yaml");
    let lock = bench.destination.with_file_name("promoted.lock");
    let promoted = bench.run(&[
        "promote",
        bench.destination.to_str().unwrap(),
        "--output",
        manifest.to_str().unwrap(),
        "--lock",
        lock.to_str().unwrap(),
    ]);
    assert_eq!(
        promoted.status.code(),
        Some(0),
        "stderr was {}",
        err(&promoted)
    );
    let written = std::fs::read_to_string(&manifest).unwrap();
    assert!(
        written.contains("derived_from"),
        "the manifest states no provenance: {written}"
    );
    assert!(
        written.contains("a.txt"),
        "the manifest names no artifact: {written}"
    );
    let pinned = std::fs::read_to_string(&lock).unwrap();
    assert!(
        pinned.contains("blake3:"),
        "the lock pins no content digest: {pinned}"
    );
}

#[test]
fn promote_keeps_the_edited_bytes_so_a_locked_run_can_fetch_them_again() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine now").unwrap();

    let manifest = bench.destination.with_file_name("promoted.yaml");
    let lock = bench.destination.with_file_name("promoted.lock");
    assert_eq!(
        bench
            .run(&[
                "promote",
                bench.destination.to_str().unwrap(),
                "--output",
                manifest.to_str().unwrap(),
                "--lock",
                lock.to_str().unwrap(),
            ])
            .status
            .code(),
        Some(0)
    );
    let elsewhere = bench.destination.with_file_name("elsewhere");
    let fetched = bench.run(&[
        "get",
        manifest.to_str().unwrap(),
        "--output",
        elsewhere.to_str().unwrap(),
        "--offline",
    ]);
    assert_eq!(
        fetched.status.code(),
        Some(0),
        "stderr was {}",
        err(&fetched)
    );
    assert_eq!(
        std::fs::read(elsewhere.join("a.txt")).unwrap(),
        b"mine now",
        "the promoted dataset did not carry the edited bytes"
    );
}

#[test]
fn an_entry_only_upstream_changed_is_taken_and_an_entry_only_you_changed_is_kept() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::write(bench.source.join("nested").join("b.txt"), b"moved").unwrap();

    let second = bench.get();
    assert_eq!(second.status.code(), Some(0), "stderr was {}", err(&second));
    assert_eq!(std::fs::read(bench.at("a.txt")).unwrap(), b"mine");
    assert_eq!(std::fs::read(bench.at("nested/b.txt")).unwrap(), b"moved");
}

#[test]
fn an_entry_both_sides_changed_conflicts_leaves_yours_and_writes_upstream_beside_it() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::write(bench.source.join("a.txt"), b"theirs").unwrap();

    let second = bench.get();
    assert_eq!(
        second.status.code(),
        Some(60),
        "stdout was {} and stderr was {}",
        out(&second),
        err(&second)
    );
    assert!(
        err(&second).contains("a.txt"),
        "the conflict was not named: {}",
        err(&second)
    );
    assert_eq!(
        std::fs::read(bench.at("a.txt")).unwrap(),
        b"mine",
        "a conflict overwrote work with no other copy"
    );
    assert_eq!(
        std::fs::read(bench.at("a.txt.upstream")).unwrap(),
        b"theirs",
        "upstream's version was not written beside yours"
    );
}

#[test]
fn an_entry_you_deleted_that_upstream_left_alone_stays_deleted() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::remove_file(bench.at("a.txt")).unwrap();
    std::fs::write(bench.source.join("nested").join("b.txt"), b"moved").unwrap();

    let second = bench.get();
    assert_eq!(second.status.code(), Some(0), "stderr was {}", err(&second));
    assert!(
        !bench.at("a.txt").exists(),
        "an entry deleted on purpose came back"
    );
    assert_eq!(std::fs::read(bench.at("nested/b.txt")).unwrap(), b"moved");
}

#[test]
fn an_entry_you_deleted_that_upstream_changed_conflicts() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::remove_file(bench.at("a.txt")).unwrap();
    std::fs::write(bench.source.join("a.txt"), b"theirs").unwrap();

    let second = bench.get();
    assert_eq!(
        second.status.code(),
        Some(60),
        "stdout was {} and stderr was {}",
        out(&second),
        err(&second)
    );
    assert_eq!(
        std::fs::read(bench.at("a.txt.upstream")).unwrap(),
        b"theirs"
    );
    assert!(
        !bench.at("a.txt").exists(),
        "a deletion was undone by a conflict"
    );
}

#[test]
fn an_entry_you_added_survives_a_run_that_takes_what_upstream_changed() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("mine.txt"), b"only here").unwrap();
    std::fs::write(bench.source.join("a.txt"), b"theirs").unwrap();

    let second = bench.get();
    assert_eq!(second.status.code(), Some(0), "stderr was {}", err(&second));
    assert_eq!(std::fs::read(bench.at("mine.txt")).unwrap(), b"only here");
    assert_eq!(std::fs::read(bench.at("a.txt")).unwrap(), b"theirs");
}

#[test]
fn a_conflict_onto_a_name_something_already_holds_is_refused_before_anything_is_written() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::write(bench.source.join("a.txt"), b"theirs").unwrap();
    std::fs::write(bench.source.join("a.txt.upstream"), b"already here").unwrap();

    let second = bench.get();
    assert_eq!(
        second.status.code(),
        Some(60),
        "stdout was {} and stderr was {}",
        out(&second),
        err(&second)
    );
    assert_eq!(std::fs::read(bench.at("a.txt")).unwrap(), b"mine");
}

#[test]
fn a_merged_run_records_upstream_so_your_edit_is_not_taken_for_upstreams_next_time() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::write(bench.source.join("nested").join("b.txt"), b"moved").unwrap();
    assert_eq!(bench.get().status.code(), Some(0));

    assert_eq!(
        out(&bench.status()),
        "modified  a.txt\n",
        "the record forgot that a.txt is your own version"
    );
    std::fs::write(bench.source.join("nested").join("b.txt"), b"moved again").unwrap();
    let third = bench.get();
    assert_eq!(third.status.code(), Some(0), "stderr was {}", err(&third));
    assert_eq!(
        std::fs::read(bench.at("a.txt")).unwrap(),
        b"mine",
        "a later run took upstream's version of an entry upstream never changed"
    );
    assert_eq!(
        std::fs::read(bench.at("nested/b.txt")).unwrap(),
        b"moved again"
    );
}

fn links_are_permitted(directory: &Path) -> bool {
    let platform = fetchloom_platform::NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let _ = std::fs::create_dir_all(directory);
    let probe = directory.join("fetchloom-link-probe");
    let created =
        fetchloom_engine::seam::platform::Platform::create_symlink(&platform, b"target", &probe)
            .is_ok();
    let _ = std::fs::remove_file(&probe);
    created
}

fn linked(target: &[u8], at: &Path) {
    let platform = fetchloom_platform::NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    fetchloom_engine::seam::platform::Platform::create_symlink(&platform, target, at).unwrap();
}

fn every_receipt(root: &Path, seen: &mut Vec<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(root) else {
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        if path.is_dir() {
            every_receipt(&path, seen);
        } else if std::fs::read_to_string(&path)
            .is_ok_and(|text| text.contains("fingerprints") && text.contains("destination"))
        {
            seen.push(path);
        }
    }
}

#[test]
fn a_promote_of_a_tree_holding_a_link_fails_naming_what_cannot_be_kept() {
    let bench = bench();
    if !links_are_permitted(&bench.cache) {
        fetchloom_faults::decline!("a volume that permits creating a symbolic link");
        return;
    }
    assert_eq!(bench.get().status.code(), Some(0));
    linked(b"a.txt", &bench.at("link.txt"));

    let manifest = bench.destination.with_file_name("promoted.yaml");
    let promoted = bench.run(&[
        "promote",
        bench.destination.to_str().unwrap(),
        "--output",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(
        promoted.status.code(),
        Some(10),
        "stdout was {} and stderr was {}",
        out(&promoted),
        err(&promoted)
    );
    assert!(
        err(&promoted).contains("symbolic link"),
        "stderr was {}",
        err(&promoted)
    );
    assert!(
        !manifest.exists(),
        "a manifest was written for a tree promote could not keep"
    );
}

#[test]
fn a_conflict_on_a_symbolic_link_writes_upstreams_link_beside_yours() {
    let bench = bench();
    if !links_are_permitted(&bench.cache) {
        fetchloom_faults::decline!("a volume that permits creating a symbolic link");
        return;
    }
    linked(b"a.txt", &bench.source.join("link.txt"));
    assert_eq!(bench.get().status.code(), Some(0));

    std::fs::remove_file(bench.at("link.txt")).unwrap();
    linked(b"nested/b.txt", &bench.at("link.txt"));
    std::fs::remove_file(bench.source.join("link.txt")).unwrap();
    std::fs::write(bench.source.join("added.txt"), b"added").unwrap();
    linked(b"added.txt", &bench.source.join("link.txt"));

    let second = bench.get();
    assert_eq!(
        second.status.code(),
        Some(60),
        "stdout was {} and stderr was {}",
        out(&second),
        err(&second)
    );
    assert_eq!(
        std::fs::read_link(bench.at("link.txt"))
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
        "nested/b.txt",
        "a conflict replaced your link"
    );
    assert_eq!(
        std::fs::read_link(bench.at("link.txt.upstream"))
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/"),
        "added.txt",
        "upstream's link was not written beside yours"
    );
}

#[test]
fn a_record_whose_every_timestamp_lies_costs_time_and_never_correctness() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"MINE").unwrap();

    let mut receipts = Vec::new();
    every_receipt(&bench.cache, &mut receipts);
    assert_eq!(receipts.len(), 1, "found {} receipts", receipts.len());
    let text = std::fs::read_to_string(&receipts[0]).unwrap();
    let lying = text.replace("modified_nanos: \"", "modified_nanos: \"1");
    assert_ne!(lying, text, "no timestamp was rewritten");
    std::fs::write(&receipts[0], lying).unwrap();

    let status = bench.status();
    assert_eq!(status.status.code(), Some(0), "stderr was {}", err(&status));
    assert_eq!(
        out(&status),
        "modified  a.txt\n",
        "a record whose timestamps all lie changed what status reports"
    );
}

#[test]
fn a_rewrite_of_the_same_length_is_still_modified() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    std::fs::write(bench.at("a.txt"), b"HELLO").unwrap();
    assert_eq!(out(&bench.status()), "modified  a.txt\n");
}

fn archive_bytes(one: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut writer = fetchloom_faults::TarWriter::new();
    writer.push(
        &fetchloom_faults::TarHeader::ustar(b"docs/", fetchloom_faults::TYPEFLAG_DIRECTORY),
        b"",
    );
    for (name, body) in [
        (&b"docs/one.txt"[..], one),
        (&b"docs/two.txt"[..], &b"two\n"[..]),
    ] {
        let mut header =
            fetchloom_faults::TarHeader::ustar(name, fetchloom_faults::TYPEFLAG_REGULAR);
        header.set_size(body.len() as u64).set_mode(0o644);
        writer.push(&header, body);
    }
    let tar = writer.finish();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn an_entry_inside_an_archive_is_reverted_out_of_the_object_the_cache_holds() {
    let bench = bench();
    let archive = bench.source.with_file_name("corpus.tar.gz");
    std::fs::write(&archive, archive_bytes(b"one\n")).unwrap();
    let fetched = bench.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        bench.destination.to_str().unwrap(),
    ]);
    assert_eq!(
        fetched.status.code(),
        Some(0),
        "stderr was {}",
        err(&fetched)
    );

    std::fs::write(bench.at("docs/one.txt"), b"mine\n").unwrap();
    assert_eq!(out(&bench.status()), "modified  docs/one.txt\n");

    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap()]);
    assert_eq!(
        reverted.status.code(),
        Some(0),
        "stderr was {}",
        err(&reverted)
    );
    assert_eq!(std::fs::read(bench.at("docs/one.txt")).unwrap(), b"one\n");
    assert_eq!(std::fs::read(bench.at("docs/two.txt")).unwrap(), b"two\n");
    assert_eq!(out(&bench.status()), "");
}

#[test]
fn an_archive_upstream_changed_merges_against_the_member_you_edited() {
    let bench = bench();
    let archive = bench.source.with_file_name("corpus.tar.gz");
    std::fs::write(&archive, archive_bytes(b"one\n")).unwrap();
    assert_eq!(
        bench
            .run(&[
                "get",
                archive.to_str().unwrap(),
                "--output",
                bench.destination.to_str().unwrap(),
            ])
            .status
            .code(),
        Some(0)
    );

    std::fs::write(bench.at("docs/two.txt"), b"mine\n").unwrap();
    std::fs::write(&archive, archive_bytes(b"one, moved\n")).unwrap();

    let second = bench.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        bench.destination.to_str().unwrap(),
    ]);
    assert_eq!(second.status.code(), Some(0), "stderr was {}", err(&second));
    assert_eq!(
        std::fs::read(bench.at("docs/one.txt")).unwrap(),
        b"one, moved\n"
    );
    assert_eq!(std::fs::read(bench.at("docs/two.txt")).unwrap(), b"mine\n");
}

#[test]
fn revert_writes_no_record_and_leaves_the_one_the_run_wrote_byte_for_byte() {
    let bench = bench();
    assert_eq!(bench.get().status.code(), Some(0));
    let mut before = Vec::new();
    every_receipt(&bench.cache, &mut before);
    assert_eq!(before.len(), 1, "found {} receipts", before.len());
    let written = std::fs::read(&before[0]).unwrap();

    std::fs::write(bench.at("a.txt"), b"mine").unwrap();
    std::fs::remove_file(bench.at("nested/b.txt")).unwrap();
    let reverted = bench.run(&["revert", bench.destination.to_str().unwrap()]);
    assert_eq!(
        reverted.status.code(),
        Some(0),
        "stderr was {}",
        err(&reverted)
    );

    let mut after = Vec::new();
    every_receipt(&bench.cache, &mut after);
    assert_eq!(after, before, "revert wrote a record of its own");
    assert_eq!(
        std::fs::read(&before[0]).unwrap(),
        written,
        "revert rewrote the record the run left"
    );
}

#[test]
fn promote_of_a_destination_with_no_record_is_refused_and_writes_no_manifest() {
    let bench = bench();
    std::fs::create_dir_all(&bench.destination).unwrap();
    std::fs::write(bench.at("a.txt"), b"hello").unwrap();
    let manifest = bench.destination.with_file_name("promoted.yaml");

    let promoted = bench.run(&[
        "promote",
        bench.destination.to_str().unwrap(),
        "--output",
        manifest.to_str().unwrap(),
    ]);
    assert_eq!(
        promoted.status.code(),
        Some(10),
        "stdout was {}",
        out(&promoted)
    );
    assert!(
        err(&promoted).contains("records what was written"),
        "stderr was {}",
        err(&promoted)
    );
    assert!(
        !manifest.exists(),
        "a promote of a directory nothing wrote left a manifest behind"
    );
}
