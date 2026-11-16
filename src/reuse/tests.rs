use std::ffi::OsString;
use std::fs;

use super::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// no candidate at all.
const NONE: [(PathBuf, PathBuf); 0] = [];

fn digest(bytes: &[u8]) -> Sha256 {
    crate::digest::sha256(bytes)
}

/// a variable reader holding `pairs` alone.
fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name: &str| {
        pairs
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| OsString::from(value))
    }
}

fn hugging_face_file(path: &[&str]) -> Known {
    Known::HuggingFace {
        folder: "datasets--lab--rows".to_owned(),
        commit: COMMIT.to_owned(),
        path: path.iter().map(|name| (*name).to_owned()).collect(),
    }
}

#[test]
fn caches_are_found_where_their_own_tools_look() {
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let at = |pairs: &[(&str, &str)]| {
        let var = vars(pairs);
        located(&var)
    };
    let base = Path::new("/h");
    assert_eq!(
        at(&[(home, "/h")]),
        [
            (
                Tool::HuggingFace,
                base.join(".cache").join("huggingface").join("hub")
            ),
            (Tool::Kagglehub, base.join(".cache").join("kagglehub")),
        ]
    );
    let first = at(&[
        (home, "/h"),
        ("HF_HUB_CACHE", "/a"),
        ("HF_HOME", "/b"),
        ("XDG_CACHE_HOME", "/c"),
        ("KAGGLEHUB_CACHE", "/k"),
    ]);
    assert_eq!(
        first,
        [
            (Tool::HuggingFace, PathBuf::from("/a")),
            (Tool::Kagglehub, PathBuf::from("/k")),
        ]
    );
    let home_hub = at(&[(home, "/h"), ("HF_HOME", "~/b"), ("XDG_CACHE_HOME", "/c")]);
    assert_eq!(home_hub[0], (Tool::HuggingFace, base.join("b").join("hub")));
    let xdg = at(&[(home, "/h"), ("XDG_CACHE_HOME", "/c")]);
    assert_eq!(
        xdg[0],
        (
            Tool::HuggingFace,
            Path::new("/c").join("huggingface").join("hub")
        )
    );
    // an empty variable names no folder, so the next one counts
    let empty = at(&[(home, "/h"), ("HF_HUB_CACHE", ""), ("HF_HOME", "/b")]);
    assert_eq!(empty[0], (Tool::HuggingFace, Path::new("/b").join("hub")));
    assert_eq!(at(&[]), []);
}

#[test]
fn a_hugging_face_link_names_its_repository_folder_commit_and_path() {
    let link = format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/data/a%20b.csv");
    assert_eq!(
        known(&link),
        Some(Known::HuggingFace {
            folder: "datasets--lab--rows".to_owned(),
            commit: COMMIT.to_owned(),
            path: vec!["data".to_owned(), "a b.csv".to_owned()],
        })
    );
    let model = format!("https://huggingface.co/gpt2/resolve/{COMMIT}/config.json");
    assert!(matches!(
        known(&model),
        Some(Known::HuggingFace { folder, .. }) if folder == "models--gpt2"
    ));
    let space = format!("https://huggingface.co/spaces/a/b/resolve/{COMMIT}/app.py");
    assert!(matches!(
        known(&space),
        Some(Known::HuggingFace { folder, .. }) if folder == "spaces--a--b"
    ));
}

/// data.lock comes with a clone, so no part of its link may lead a look out of the cache.
#[test]
fn a_link_that_could_leave_the_cache_names_nothing() {
    for link in [
        format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/%2e%2e/%2e%2e/x"),
        format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/a%2F..%2F..%2Fx"),
        format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/a%5Cb"),
        format!("https://huggingface.co/datasets/l%2E%2E/rows/resolve/{COMMIT}/x"),
        format!("https://huggingface.co/datasets/a--b/rows/resolve/{COMMIT}/x"),
        "https://huggingface.co/datasets/lab/rows/resolve/main/x".to_owned(),
        format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/"),
        format!("https://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/x?download=1"),
        format!("http://huggingface.co/datasets/lab/rows/resolve/{COMMIT}/x"),
        format!("https://huggingface.co:444/datasets/lab/rows/resolve/{COMMIT}/x"),
        format!("https://hf-mirror.com/datasets/lab/rows/resolve/{COMMIT}/x"),
        "https://api.kaggle.com/v1/datasets/download/a/b/..%2Fx?datasetVersionNumber=2".to_owned(),
        "https://api.kaggle.com/v1/datasets/download/a/b/x?datasetVersionNumber=two".to_owned(),
        "https://api.kaggle.com/v1/datasets/download/a/b/x".to_owned(),
        "https://api.kaggle.com/v1/datasets/download/a%2F../b/x?datasetVersionNumber=2".to_owned(),
    ] {
        assert_eq!(known(&link), None, "{link}");
    }
}

#[test]
fn a_kaggle_link_names_its_dataset_version_and_path() {
    let link = "https://api.kaggle.com/v1/datasets/download/uciml/iris/data%2FIris.csv?datasetVersionNumber=2&raw=true";
    assert_eq!(
        known(link),
        Some(Known::Kaggle {
            owner: "uciml".to_owned(),
            slug: "iris".to_owned(),
            version: 2,
            path: vec!["data".to_owned(), "Iris.csv".to_owned()],
        })
    );
}

#[test]
fn hf_cache_rm_takes_the_repository_of_a_cache_folder() {
    assert_eq!(
        removable(OsStr::new("datasets--scikit-learn--iris")),
        Some("dataset/scikit-learn/iris".to_owned())
    );
    assert_eq!(
        removable(OsStr::new("models--gpt2")),
        Some("model/gpt2".to_owned())
    );
    assert_eq!(removable(OsStr::new("blobs")), None);
}

/// a hugging face cache with one repository folder, `datasets--lab--rows`.
fn hub() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("hub");
    let repository = root.join("datasets--lab--rows");
    fs::create_dir_all(repository.join("blobs")).unwrap();
    fs::create_dir_all(repository.join("snapshots").join(COMMIT).join("data")).unwrap();
    (folder, root, repository)
}

#[test]
fn a_blob_named_by_the_pin_is_found_in_any_repository() {
    let (_folder, root, repository) = hub();
    let bytes = b"rows\n";
    let blob = repository.join("blobs").join(digest(bytes).hex());
    fs::write(&blob, bytes).unwrap();
    let repositories = repositories(&root);
    assert_eq!(repositories, std::slice::from_ref(&repository));
    let found = in_hugging_face(&root, &repositories, &digest(bytes), 5, None);
    assert_eq!(found, [(repository, blob)]);
    // another size is no candidate, whatever its name
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), 6, None),
        NONE
    );
}

/// where hugging face makes no links, as on windows without developer mode, the snapshot entry is
/// the only copy, and a plain git file's blob is named by its git hash, never its SHA-256.
#[test]
fn a_plain_snapshot_entry_is_found_for_its_commit_and_path() {
    let (_folder, root, repository) = hub();
    let bytes = b"a,b\n1,2\n";
    let entry = repository
        .join("snapshots")
        .join(COMMIT)
        .join("data")
        .join("a.csv");
    fs::write(&entry, bytes).unwrap();
    let repositories = repositories(&root);
    let known = hugging_face_file(&["data", "a.csv"]);
    let size = bytes.len() as u64;
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), size, Some(&known)),
        [(repository, entry)]
    );
    let other = hugging_face_file(&["data", "b.csv"]);
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), size, Some(&other)),
        NONE
    );
}

/// hugging face's own relative links are read as names in its layout, never followed, so one that
/// leads anywhere else finds nothing.
#[cfg(unix)]
#[test]
fn links_are_read_only_in_the_layout_hugging_face_writes() {
    use std::os::unix::fs::symlink;

    let (folder, root, repository) = hub();
    let bytes = b"a,b\n1,2\n";
    let size = bytes.len() as u64;
    let git = "0a1b2c3d4e5f60718293a4b5c6d7e8f901234567";
    let blob = repository.join("blobs").join(git);
    fs::write(&blob, bytes).unwrap();
    let snapshot = repository.join("snapshots").join(COMMIT);
    symlink(
        format!("../../../blobs/{git}"),
        snapshot.join("data").join("a.csv"),
    )
    .unwrap();
    let repositories = repositories(&root);
    let known = hugging_face_file(&["data", "a.csv"]);
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), size, Some(&known)),
        [(repository.clone(), blob.clone())]
    );
    // a link that climbs past the repository, or to a file by its full path, is no blob
    let outside = folder.path().join("outside.csv");
    fs::write(&outside, bytes).unwrap();
    symlink("../../../../outside.csv", snapshot.join("b.csv")).unwrap();
    symlink(&outside, snapshot.join("c.csv")).unwrap();
    symlink(
        format!("../../blobs/{git}"),
        snapshot.join("data").join("d.csv"),
    )
    .unwrap();
    for name in [&["b.csv"][..], &["c.csv"], &["data", "d.csv"]] {
        let known = hugging_face_file(name);
        assert_eq!(
            in_hugging_face(&root, &repositories, &digest(bytes), size, Some(&known)),
            NONE,
            "{name:?}"
        );
    }
}

/// a xet file shared across repositories sits once in the cache's store, and each repository's
/// blob is a link to it there.
#[cfg(unix)]
#[test]
fn a_blob_linked_to_the_shared_store_is_found_there() {
    use std::os::unix::fs::symlink;

    let (_folder, root, repository) = hub();
    let bytes = b"shared rows\n";
    let xet = format!("ab{}", "c".repeat(62));
    let store = root.join("blobs").join("ab");
    fs::create_dir_all(&store).unwrap();
    fs::write(store.join(&xet), bytes).unwrap();
    let blob = repository.join("blobs").join(digest(bytes).hex());
    symlink(format!("../../blobs/ab/{xet}"), &blob).unwrap();
    let repositories = repositories(&root);
    let size = bytes.len() as u64;
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), size, None),
        [(repository, store.join(&xet))]
    );
    fs::remove_file(&blob).unwrap();
    symlink(format!("../../blobs/cd/{xet}"), &blob).unwrap();
    assert_eq!(
        in_hugging_face(&root, &repositories, &digest(bytes), size, None),
        NONE
    );
}

#[test]
fn a_kagglehub_version_counts_only_once_marked_complete() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    let dataset = root.join("datasets").join("uciml").join("iris");
    let file = dataset.join("versions").join("2").join("Iris.csv");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"iris\n").unwrap();
    let known = Known::Kaggle {
        owner: "uciml".to_owned(),
        slug: "iris".to_owned(),
        version: 2,
        path: vec!["Iris.csv".to_owned()],
    };
    let sizes = HashSet::from([5]);
    let kagglehub = [Folder {
        path: root.to_path_buf(),
        tool: Some(Tool::Kagglehub),
    }];
    let stop = Stop::default();
    let scanned = || scan(&kagglehub, &sizes, Path::new("/nowhere"), &stop).unwrap();
    assert_eq!(in_kagglehub(root, &known, 5), None);
    assert!(scanned().is_empty());
    // one file of the version, marked as kagglehub marks a file downloaded alone
    let one = dataset
        .join(".complete")
        .join("2")
        .join("Iris.csv.complete");
    fs::create_dir_all(one.parent().unwrap()).unwrap();
    fs::write(&one, b"").unwrap();
    assert_eq!(in_kagglehub(root, &known, 5), Some(file.clone()));
    fs::remove_file(&one).unwrap();
    assert_eq!(in_kagglehub(root, &known, 5), None);
    fs::write(dataset.join("2.complete"), b"").unwrap();
    assert_eq!(in_kagglehub(root, &known, 5), Some(file.clone()));
    assert_eq!(scanned()[&5], [(0, file)]);
}

/// a scan reads sizes alone, so only a plain file of a pinned size is ever a candidate: no link,
/// no file of another size, nothing in fetchloom's own cache.
#[test]
fn a_scan_finds_plain_files_of_the_pinned_sizes_alone() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().join("usb");
    let deep = root.join("a").join("b");
    let cache = root.join("fetchloom-cache");
    fs::create_dir_all(&deep).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::write(root.join("three.txt"), b"abc").unwrap();
    fs::write(root.join("five.txt"), b"abcde").unwrap();
    fs::write(deep.join("also-five.bin"), b"12345").unwrap();
    fs::write(cache.join("cached-five"), b"vwxyz").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("five.txt"), root.join("link-to-five")).unwrap();
    let folders = [Folder {
        path: root.clone(),
        tool: None,
    }];
    let found = scan(&folders, &HashSet::from([5]), &cache, &Stop::default()).unwrap();
    let mut paths: Vec<&PathBuf> = found[&5].iter().map(|(_, path)| path).collect();
    paths.sort();
    assert_eq!(paths, [&deep.join("also-five.bin"), &root.join("five.txt")]);
    assert_eq!(found.len(), 1);
    let none = scan(&folders, &HashSet::new(), &cache, &Stop::default()).unwrap();
    assert!(none.is_empty());
}

/// hugging face's locks and the downloads it has not finished are not its files.
#[test]
fn a_scan_of_the_hugging_face_cache_passes_over_unfinished_downloads() {
    let (_folder, root, repository) = hub();
    fs::write(repository.join("blobs").join("x.incomplete"), b"12345").unwrap();
    fs::create_dir_all(root.join(".locks")).unwrap();
    fs::write(root.join(".locks").join("x.lock"), b"12345").unwrap();
    let kept = repository.join("blobs").join("y");
    fs::write(&kept, b"12345").unwrap();
    let folders = [Folder {
        path: root,
        tool: Some(Tool::HuggingFace),
    }];
    let found = scan(
        &folders,
        &HashSet::from([5]),
        Path::new("/nowhere"),
        &Stop::default(),
    )
    .unwrap();
    assert_eq!(found[&5], [(0, kept)]);
}
