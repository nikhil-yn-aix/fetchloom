use proptest::prelude::*;

use super::*;
use crate::tree::{Tree, TreeEntry};

fn name(text: &str) -> Name {
    Name::new(text).unwrap()
}

fn path(text: &str) -> DataPath {
    DataPath::new(text).unwrap()
}

fn file(text: &str, size: u64, byte: u8) -> LockedFile {
    LockedFile {
        path: path(text),
        size,
        blake3: [byte; 32],
        sha256: None,
        sha1: None,
        md5: None,
        at: vec![format!("https://host/{text}")],
        from: None,
        role: None,
    }
}

fn tree_of(files: &[LockedFile]) -> Digest {
    let entries = files
        .iter()
        .filter(|f| f.role.is_none())
        .map(|f| TreeEntry {
            path: f.path.clone(),
            size: f.size,
            blake3: f.blake3,
        })
        .collect();
    Tree::new(entries).unwrap().hash()
}

fn dataset(dataset_name: &str, files: Vec<LockedFile>) -> LockedDataset {
    LockedDataset {
        name: name(dataset_name),
        reference: "zenodo:3242074".parse().unwrap(),
        spec: Digest::Blake3([1; 32]),
        resolved: "zenodo:3242074".parse().unwrap(),
        title: Some("EEG \"Motor\" Imagery".to_owned()),
        license: Some("CC0-1.0".to_owned()),
        doi: Some("10.5281/zenodo.3242074".to_owned()),
        retrieved: "2026-09-28T10:00:00Z".parse().unwrap(),
        tree: tree_of(&files),
        files: Some(files),
    }
}

fn step(step_name: &str) -> LockedStep {
    LockedStep {
        name: name(step_name),
        key: Digest::Blake3([7; 32]),
        items: Some(3),
        tree: Digest::Blake3([8; 32]),
    }
}

fn sample_lock() -> Lock {
    let mut first = file("participants.tsv", 1834, 0xaa);
    first.md5 = Some([0xbb; 16]);
    first.sha256 = Some([0xcc; 32]);
    first.sha1 = Some([0xdd; 20]);
    first.at.push("https://mirror/participants.tsv".to_owned());
    let archive = LockedFile {
        role: Some(Role::Archive),
        ..file("raw.tar.gz", 100, 0x11)
    };
    let member = LockedFile {
        at: Vec::new(),
        from: Some(Digest::Blake3([0x11; 32])),
        ..file("raw/a.edf", 40, 0x22)
    };
    let eeg = dataset("eeg", vec![member, first, archive]);
    let mut bare = dataset("bare", Vec::new());
    bare.title = None;
    bare.license = None;
    bare.doi = None;
    bare.files = None;
    let mut plain = step("plain");
    plain.items = None;
    Lock::new(vec![eeg, bare], vec![step("filter"), plain]).unwrap()
}

const SAMPLE_TEXT: &str = r#"version = 1

[[dataset]]
name = "bare"
ref = "zenodo:3242074"
spec = "blake3:0101010101010101010101010101010101010101010101010101010101010101"
resolved = "zenodo:3242074"
retrieved = "2026-09-28T10:00:00Z"
tree = "blake3:BARE"

[[dataset]]
name = "eeg"
ref = "zenodo:3242074"
spec = "blake3:0101010101010101010101010101010101010101010101010101010101010101"
resolved = "zenodo:3242074"
title = "EEG \"Motor\" Imagery"
license = "CC0-1.0"
doi = "10.5281/zenodo.3242074"
retrieved = "2026-09-28T10:00:00Z"
tree = "blake3:TREE"
files = [
  { path = "participants.tsv", size = 1834, blake3 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", sha256 = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc", sha1 = "dddddddddddddddddddddddddddddddddddddddd", md5 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", at = ["https://host/participants.tsv", "https://mirror/participants.tsv"] },
  { path = "raw.tar.gz", size = 100, blake3 = "1111111111111111111111111111111111111111111111111111111111111111", at = ["https://host/raw.tar.gz"], role = "archive" },
  { path = "raw/a.edf", size = 40, blake3 = "2222222222222222222222222222222222222222222222222222222222222222", from = "blake3:1111111111111111111111111111111111111111111111111111111111111111" },
]

[[step]]
name = "filter"
key = "blake3:0707070707070707070707070707070707070707070707070707070707070707"
items = 3
tree = "blake3:0808080808080808080808080808080808080808080808080808080808080808"

[[step]]
name = "plain"
key = "blake3:0707070707070707070707070707070707070707070707070707070707070707"
tree = "blake3:0808080808080808080808080808080808080808080808080808080808080808"
"#;

fn sample_text() -> String {
    let lock = sample_lock();
    SAMPLE_TEXT
        .replace("blake3:BARE", &lock.datasets()[0].tree.to_string())
        .replace("blake3:TREE", &lock.datasets()[1].tree.to_string())
}

#[test]
fn accepts_a_byte_order_mark_crlf_and_any_order() {
    let lock = sample_lock();
    let text = lock.to_toml();
    let crlf = format!("\u{feff}{}", text.replace('\n', "\r\n"));
    assert_eq!(Lock::from_toml(&crlf).unwrap(), lock);
    let mut reversed = lock.clone();
    reversed.datasets.reverse();
    reversed.steps.reverse();
    if let Some(files) = &mut reversed.datasets[0].files {
        files.reverse();
    }
    let unsorted = crate::lock::write::to_toml(&reversed);
    assert_ne!(unsorted, text);
    assert_eq!(Lock::from_toml(&unsorted).unwrap(), lock);
}

#[test]
fn writes_an_empty_lock() {
    let lock = Lock::new(Vec::new(), Vec::new()).unwrap();
    assert_eq!(lock.to_toml(), "version = 1\n");
    assert_eq!(Lock::from_toml("version = 1\n").unwrap(), lock);
}

#[test]
fn writes_an_empty_file_list_inline() {
    let lock = Lock::new(vec![dataset("x", Vec::new())], Vec::new()).unwrap();
    let text = lock.to_toml();
    assert!(text.contains("\nfiles = []\n"), "{text}");
    assert_eq!(Lock::from_toml(&text).unwrap(), lock);
}

fn refused(text: &str) -> LockError {
    Lock::from_toml(text).unwrap_err()
}

#[test]
fn refuses_other_versions_and_unknown_keys() {
    assert_eq!(
        refused("version = 2\n").to_string(),
        "data.lock version 2 is not supported, this fetchloom reads version 1"
    );
    assert_eq!(refused("").to_string(), "missing key `version`");
    let err = refused("version = 1\n[[dataset]]\nnam = \"x\"\n");
    assert_eq!(err.to_string(), "unknown key `nam`");
    assert_eq!(err.help.as_deref(), Some("the closest valid key is `name`"));
    assert!(err.span.is_some());
}

#[test]
fn refuses_malformed_values() {
    let text = sample_text();
    let cases = [
        (
            "participants.tsv\"",
            "../x\"",
            "dataset path `../x` has a `..` component",
        ),
        (
            "blake3 = \"aaaa",
            "blake3 = \"AAAA",
            "blake3 digests are 64 lowercase hex characters",
        ),
        (
            "md5 = \"bbbb",
            "md5 = \"bbb",
            "md5 digests are 32 lowercase hex characters",
        ),
        (
            "2026-09-28T10:00:00Z",
            "2026-09-28",
            "`2026-09-28` is not a UTC time",
        ),
        (
            "role = \"archive\"",
            "role = \"member\"",
            "unknown variant `member`, expected `archive`",
        ),
        (
            "size = 1834",
            "size = -1",
            "invalid value: integer `-1`, expected u64",
        ),
    ];
    for (from, to, message) in cases {
        let bad = text.replacen(from, to, 1);
        let err = refused(&bad);
        assert!(err.to_string().starts_with(message), "{to}: {err}");
        assert!(err.span.is_some(), "{to}");
    }
}

#[test]
fn refuses_what_breaks_the_lock_rules() {
    let text = sample_text();
    let cases = [
        (
            text.replacen("name = \"bare\"", "name = \"eeg\"", 1),
            "dataset `eeg` appears twice",
        ),
        (
            text.replacen("name = \"plain\"", "name = \"filter\"", 1),
            "step `filter` appears twice",
        ),
        (
            text.replacen("name = \"plain\"", "name = \"eeg\"", 1),
            "`eeg` names both a dataset and a step",
        ),
        (
            text.replacen("path = \"raw.tar.gz\"", "path = \"participants.tsv\"", 1),
            "path `participants.tsv` appears twice in dataset `eeg`",
        ),
        (
            text.replacen("size = 40", "size = 41", 1),
            "dataset `eeg` records tree",
        ),
        (
            text.replacen("from = \"blake3:1111", "from = \"blake3:3333", 1),
            "file `raw/a.edf` of dataset `eeg` comes from",
        ),
    ];
    for (bad, message) in cases {
        let err = refused(&bad);
        assert!(err.to_string().starts_with(message), "{message}: {err}");
    }
}

#[test]
fn new_refuses_what_cannot_be_written() {
    let largest = LockedFile {
        size: i64::MAX.unsigned_abs(),
        ..file("a", 1, 1)
    };
    Lock::new(vec![dataset("x", vec![largest])], Vec::new()).unwrap();
    let too_big = LockedFile {
        size: i64::MAX.unsigned_abs() + 1,
        ..file("a", 1, 1)
    };
    let err = Lock::new(vec![dataset("x", vec![too_big])], Vec::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "file `a` of dataset `x` is larger than a lock can record"
    );
    let mut many = step("s");
    many.items = Some(i64::MAX.unsigned_abs());
    Lock::new(Vec::new(), vec![many.clone()]).unwrap();
    many.items = Some(i64::MAX.unsigned_abs() + 1);
    let err = Lock::new(Vec::new(), vec![many]).unwrap_err();
    assert_eq!(
        err.to_string(),
        "step `s` has more items than a lock can record"
    );
    let mut files: Vec<LockedFile> = (0..INLINE_FILES_MAX)
        .map(|i| file(&format!("f{i}"), 1, 1))
        .collect();
    Lock::new(vec![dataset("x", files.clone())], Vec::new()).unwrap();
    files.push(file("g", 1, 1));
    let err = Lock::new(vec![dataset("x", files)], Vec::new()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "dataset `x` lists 10,001 files inline, a lock holds at most 10,000 files"
    );
}

#[test]
fn reads_the_documented_example() {
    let text = format!(
        r#"version = 1

[[dataset]]
name = "eeg"
ref = "openneuro:ds003061@1.1.0"
spec = "blake3:1c9e04a7b2f1{zeros52}"
resolved = "openneuro:ds003061@1.1.0"
title = "EEG Motor Movement/Imagery"
license = "CC0-1.0"
doi = "10.18112/openneuro.ds003061.v1.1.0"
retrieved = "2026-09-28T10:00:00Z"
tree = "{tree}"
files = [
  {{ path = "participants.tsv", size = 1834, blake3 = "{b3}", md5 = "{md5}", at = ["https://s3.amazonaws.com/openneuro.org/ds003061/participants.tsv"] }},
]

[[step]]
name = "filter"
key = "blake3:77ab{zeros60}"
items = 3
tree = "blake3:4e10{zeros60}"
"#,
        zeros52 = "0".repeat(52),
        zeros60 = "0".repeat(60),
        b3 = "ab".repeat(32),
        md5 = "cd".repeat(16),
        tree = Tree::new(vec![TreeEntry {
            path: path("participants.tsv"),
            size: 1834,
            blake3: [0xab; 32],
        }])
        .unwrap()
        .hash(),
    );
    let lock = Lock::from_toml(&text).unwrap();
    let eeg = &lock.datasets()[0];
    assert_eq!(eeg.reference.as_str(), "openneuro:ds003061@1.1.0");
    assert_eq!(eeg.files.as_ref().unwrap()[0].md5, Some([0xcd; 16]));
    assert_eq!(lock.steps()[0].items, Some(3));
    assert_eq!(lock.to_toml(), text);
}

fn any_file() -> impl Strategy<Value = LockedFile> {
    (
        "[a-z\"# \u{e9}]{1,6}(/[a-z]{1,6}){0,2}",
        0..=i64::MAX as u64,
        any::<[u8; 32]>(),
        proptest::option::of(any::<[u8; 16]>()),
        prop::collection::vec("https://[a-z]{1,8}/[a-z\"\\\\]{0,8}", 0..3),
    )
        .prop_map(|(p, size, blake3, md5, at)| LockedFile {
            size,
            blake3,
            md5,
            at,
            ..file(&p, 0, 0)
        })
}

fn any_dataset() -> impl Strategy<Value = LockedDataset> {
    (
        "[a-z]{1,8}",
        prop::collection::btree_map("[a-z\"# \u{e9}]{1,6}(/[a-z]{1,6}){0,2}", any_file(), 0..6),
        proptest::option::of("\\PC{0,20}"),
        any::<bool>(),
    )
        .prop_map(|(n, files, title, inline)| {
            let files: Vec<LockedFile> = files
                .into_iter()
                .map(|(p, f)| LockedFile {
                    path: path(&p),
                    ..f
                })
                .collect();
            let mut locked = dataset(&n, files);
            locked.title = title;
            if !inline {
                locked.files = None;
            }
            locked
        })
}

proptest! {
    #[test]
    fn any_lock_round_trips_byte_for_byte(
        datasets in prop::collection::btree_map("[a-z]{1,8}", any_dataset(), 0..4),
        steps in prop::collection::btree_set("[0-9]{1,4}", 0..4),
    ) {
        let datasets: Vec<LockedDataset> = datasets
            .into_iter()
            .map(|(n, d)| LockedDataset { name: name(&n), ..d })
            .collect();
        let steps: Vec<LockedStep> = steps.iter().map(|s| step(s)).collect();
        let lock = Lock::new(datasets, steps).unwrap();
        let text = lock.to_toml();
        let read = Lock::from_toml(&text).unwrap();
        prop_assert_eq!(&read, &lock);
        prop_assert_eq!(read.to_toml(), text);
    }
}
