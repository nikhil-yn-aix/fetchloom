use std::fs;

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn parallel_scans_do_not_retain_one_descriptor_per_folder() {
    const CHILD: &str = "FETCHLOOM_TEST_SCAN_DESCRIPTOR_LIMIT";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::sync::disk::tests::parallel_scans_do_not_retain_one_descriptor_per_folder",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let old = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    rustix::process::setrlimit(
        rustix::process::Resource::Nofile,
        rustix::process::Rlimit {
            current: Some(64),
            maximum: old.maximum,
        },
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    for index in 0..512 {
        let folder = dir.path().join(format!("data/folder-{index}"));
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("x"), b"data").unwrap();
    }
    let found = scan(
        dir.path(),
        "data",
        None,
        Threads::default(),
        &Stop::default(),
    )
    .unwrap()
    .found;
    assert_eq!(found.len(), 512);
}

#[test]
fn scan_thread_limits_preserve_files_and_staging_names() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for index in 0..512 {
        let path = root.join(format!("data/folder-{}/file-{index}", index / 64));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, index.to_string()).unwrap();
    }
    fs::write(
        root.join("data/.fetchloom-0123456789abcdef.copy"),
        b"partial",
    )
    .unwrap();
    fs::create_dir(root.join("outside")).unwrap();
    fs::write(root.join("outside/foreign"), b"outside").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("outside"), root.join("data/linked")).unwrap();
        std::os::unix::fs::symlink(root.join("outside/foreign"), root.join("data/linked-file"))
            .unwrap();
    }
    let serial = Threads::read(&|_| Some("1".into())).unwrap();
    let expected = scan(root, "data", None, serial, &Stop::default()).unwrap();
    assert_eq!(expected.found.len(), 513);
    assert_eq!(
        expected.temporaries,
        ["data/.fetchloom-0123456789abcdef.copy"]
    );
    for count in ["1", "8"] {
        let threads = Threads::read(&|_| Some(count.into())).unwrap();
        let actual = scan(root, "data", None, threads, &Stop::default()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            scan(root, "absent", None, threads, &Stop::default()).unwrap(),
            Scanned::default()
        );
    }
}

#[test]
fn two_spellings_both_listed_are_two_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let composed = "caf\u{e9}";
    let decomposed = "cafe\u{301}";
    for name in [composed, decomposed] {
        fs::create_dir_all(root.join("data").join(name)).unwrap();
        fs::write(root.join("data").join(name).join("a.txt"), name).unwrap();
    }
    // apfs and hfs+ see both spellings as one name, which leaves one folder
    if fs::read_dir(root.join("data")).unwrap().count() < 2 {
        return crate::test_skips::skip(
            "distinct-names",
            "this disk takes one unicode form of `café` for the other",
        );
    }
    let mut listings = Listings::new();
    for name in [composed, decomposed] {
        let path = format!("data/{name}/a.txt");
        assert!(!spelled_otherwise(root, &path, &mut listings), "{name}");
    }
    assert!(spelled_otherwise(
        root,
        "Data/caf\u{e9}/a.txt",
        &mut listings
    ));
}

/// a project of four recorded files and one new one, its state written.
fn recorded_project(dir: &Path) -> Project {
    let project = Project {
        root: dir.to_path_buf(),
    };
    let root = &project.root;
    let mut state = State::default();
    for path in ["data/a/x", "data/a/y", "data/b/z", "data/c"] {
        fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        fs::write(root.join(path), path).unwrap();
        let placed = record(root, path, path.as_bytes(), Method::Copy);
        state.files.insert(path.to_owned(), placed);
    }
    fs::write(root.join("data/new"), b"new").unwrap();
    project.write_state(&state).unwrap();
    project
}

/// a state read on a thread of its own while the data folder is walked gives the survey a read
/// before the walk gives, and so does one whose thread the system refused.
#[test]
fn a_state_read_beside_the_walk_is_the_same_survey() {
    let dir = tempfile::tempdir().unwrap();
    let project = recorded_project(dir.path());
    let survey = |beside_from| {
        let stop = Stop::default();
        let threads = Threads::default();
        Survey::surveyed_from(
            &project,
            "data",
            None,
            threads,
            &stop,
            beside_from,
            Level::Default,
        )
        .unwrap()
    };
    let before = survey(u64::MAX);
    assert_eq!(before.state.files.len(), 4);
    assert_eq!(before.disk.len(), 5);
    let counting = crate::threads::refuse_after(usize::MAX);
    let beside = survey(0);
    assert_eq!(crate::threads::spawns_left(), Some(usize::MAX - 1));
    drop(counting);
    let _refusing = crate::threads::refuse_after(0);
    let refused = survey(0);
    for survey in [beside, refused] {
        assert_eq!(survey.state.files, before.state.files);
        assert_eq!(survey.disk, before.disk);
        assert_eq!(survey.entries, before.entries);
    }
}

/// the run's cap on workers, as `FETCHLOOM_THREADS` sets it, or none.
fn capped(cap: &str) -> Threads {
    Threads::read(&|_| (!cap.is_empty()).then(|| cap.into())).unwrap()
}

/// a run kept to one worker, and a lean run, which spends memory before time, read the state on
/// their own thread, however large.
#[test]
fn one_worker_or_lean_reads_the_state_itself() {
    let dir = tempfile::tempdir().unwrap();
    let project = recorded_project(dir.path());
    for (cap, level) in [("1", Level::Default), ("", Level::Lean), ("2", Level::Lean)] {
        let _counting = crate::threads::refuse_after(usize::MAX);
        let stop = Stop::default();
        let survey =
            Survey::surveyed_from(&project, "data", None, capped(cap), &stop, 0, level).unwrap();
        assert_eq!(survey.state.files.len(), 4);
        assert_eq!(
            crate::threads::spawns_left(),
            Some(usize::MAX),
            "{cap:?} {level}"
        );
    }
}

/// a whole state is read beside the walk from its size on, with two workers or more, at the
/// default and max levels, never at lean.
#[test]
fn a_state_is_read_beside_the_walk_unless_lean_or_one_worker() {
    let from = 16 << 20;
    for (size, cap, level, read_beside) in [
        (0, "", Level::Max, false),
        (from - 1, "", Level::Default, false),
        (from, "", Level::Default, true),
        (from, "", Level::Max, true),
        (u64::MAX, "8", Level::Max, true),
        (from, "2", Level::Default, true),
        (from, "1", Level::Default, false),
        (from, "1", Level::Max, false),
        (from, "", Level::Lean, false),
        (from, "2", Level::Lean, false),
        (u64::MAX, "8", Level::Lean, false),
    ] {
        assert_eq!(
            beside(size, from, capped(cap), level),
            read_beside,
            "{size} bytes, cap {cap:?}, {level}"
        );
    }
}

fn record(root: &Path, path: &str, bytes: &[u8], method: Method) -> crate::project::state::Placed {
    crate::project::state::Placed {
        dataset: "set".to_owned(),
        sha256: crate::digest::sha256(bytes),
        method,
        look: Look::of(&fs::metadata(root.join(path)).unwrap()),
        writable: method == Method::Copy,
        checked: 0,
        adopted: false,
        racy_intact: std::sync::OnceLock::new(),
    }
}

#[test]
fn folder_order_matches_a_terminal_separator() {
    let paths = [
        "", "a", "a-", "a.", "a/", "a//", "a/b", "a0", "é", "é-", "é/", "/", "/a",
    ];
    for left in paths {
        for right in paths {
            assert_eq!(
                folder_order(left, right),
                format!("{left}/").cmp(&format!("{right}/")),
                "{left:?}, {right:?}"
            );
        }
    }
}

#[test]
fn selected_folders_keep_separator_boundaries_and_shared_paths() {
    let folders = [
        "data/a/inner",
        "data/a-",
        "data/a",
        "data/a",
        "data/z/inner",
        "data/café/child",
        "data/café-",
        "data/café",
    ]
    .map(String::from);
    let roots = outermost("data", &folders);
    assert_eq!(
        roots.iter().map(String::as_str).collect::<HashSet<_>>(),
        HashSet::from([
            "data/a",
            "data/a-",
            "data/z/inner",
            "data/café",
            "data/café-"
        ])
    );
    for (path, expected) in [
        ("data/a", true),
        ("data/a/x", true),
        ("data/a/inner/x", true),
        ("data/a-", true),
        ("data/a-/x", true),
        ("data/a--/x", false),
        ("data/ab/x", false),
        ("data/a.b/x", false),
        ("data", false),
        ("data/z", false),
        ("data/z/inner", true),
        ("data/z/inner/x", true),
        ("data/z/inner-/x", false),
        ("data/café/x", true),
        ("data/café-/x", true),
        ("data/café--/x", false),
        ("other/data/a/x", false),
        ("", false),
    ] {
        assert_eq!(inside_any(path, &roots), expected, "{path}");
    }
    let roots = outermost("data", &["data/a".into(), "data".into(), "data/a-".into()]);
    assert_eq!(roots, ["data"]);
    for (path, expected) in [
        ("data", true),
        ("data/a/x", true),
        ("data-", false),
        ("database/x", false),
    ] {
        assert_eq!(inside_any(path, &roots), expected, "{path}");
    }
    assert!(!inside_any("data/a/x", &outermost("data", &[])));
}

/// a first ctrl-c need not wait for the whole listing of a large or slow data folder.
#[test]
fn a_survey_ends_at_a_stop_asked_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir_all(project.root.join("data/a")).unwrap();
    fs::write(project.root.join("data/a/x"), b"x").unwrap();
    let stop = Stop::default();
    stop.ask();
    let Err(error) = Survey::read(&project, "data", Threads::default(), &stop) else {
        panic!("the survey listed the data folder after a stop");
    };
    assert_eq!(error.kind, crate::error::Kind::Interrupted);
}

/// what stands on the way down to the named folders is listed as a whole survey lists it, and
/// a link there is never followed.
#[test]
fn a_survey_within_folders_lists_and_keeps_only_them() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    let root = &project.root;
    for path in [
        "data/a/x",
        "data/a/inner/y",
        "data/a-/v",
        "data/b/z",
        "data/ab/w",
        "data/f",
    ] {
        fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
        fs::write(root.join(path), path).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("data/a"), root.join("data/link")).unwrap();
    let mut state = State::default();
    for path in [
        "data/a/x",
        "data/a/inner/y",
        "data/a-/v",
        "data/b/z",
        "data/ab/w",
    ] {
        let placed = record(root, path, path.as_bytes(), Method::Copy);
        state.files.insert(path.to_owned(), placed);
    }
    project.write_state(&state).unwrap();
    let sorted = |paths: Vec<&String>| {
        let mut paths: Vec<String> = paths.into_iter().cloned().collect();
        paths.sort();
        paths
    };
    let whole = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    assert_eq!(whole.disk.len(), 6);
    assert_eq!(whole.state.files.len(), 5);
    let within = [
        "data/a",
        "data/a-",
        "data/a",
        "data/a/inner",
        "data/link",
        "data/f/g",
        "data/missing",
    ]
    .map(String::from);
    let survey = Survey::read_within(
        &project,
        "data",
        &within,
        Threads::default(),
        &Stop::default(),
    )
    .unwrap();
    assert_eq!(
        sorted(survey.disk.keys().collect()),
        ["data/a-/v", "data/a/inner/y", "data/a/x", "data/f"]
    );
    assert_eq!(
        sorted(survey.state.files.keys().collect()),
        ["data/a-/v", "data/a/inner/y", "data/a/x"]
    );
    assert_eq!(survey.entries.get("data/a"), Some(&Entry::Folder));
    #[cfg(unix)]
    assert_eq!(survey.entries.get("data/link"), Some(&Entry::Link));
    assert!(survey.edited.is_empty());
    let all = Survey::read_within(
        &project,
        "data",
        &["data/a".to_owned(), "data".to_owned()],
        Threads::default(),
        &Stop::default(),
    )
    .unwrap();
    assert_eq!(all.disk, whole.disk);
    assert_eq!(all.state.files, whole.state.files);
    let none =
        Survey::read_within(&project, "data", &[], Threads::default(), &Stop::default()).unwrap();
    assert_eq!(none.disk.len(), 0);
    assert_eq!(none.state.files.len(), 0);
}

#[test]
fn racy_edits_are_hashed_and_smudged_without_distrusting_unchanged_files() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    for path in ["data/changed", "data/intact"] {
        fs::write(project.root.join(path), b"original").unwrap();
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    for path in ["data/changed", "data/intact"] {
        survey.state.files.insert(
            path.to_owned(),
            record(&project.root, path, b"original", Method::Copy),
        );
    }
    let path = project.root.join("data/changed");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, b"modified").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        Look::of(&fs::metadata(&path).unwrap()),
        survey.disk["data/changed"]
    );
    survey.hash_racy(&project.root, 0, false);
    assert_eq!(survey.edited, HashSet::from(["data/changed".to_owned()]));
    assert!(survey.needs_writing);
    survey.smudge_edited();
    assert_eq!(survey.state.files["data/changed"].look.size, u64::MAX);
    assert_eq!(survey.state.files["data/intact"].look.size, 8);
    assert_eq!(fs::read(path).unwrap(), b"modified");
}

/// a changed link that still shares its bytes is the cache's copy and any other changed file
/// is damaged; files already found edited, or of a dataset the run leaves alone, are skipped.
#[test]
fn verifying_finds_changes_that_kept_their_size_and_time() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let placed = [
        ("copy", Method::Copy),
        ("link", Method::Hardlink),
        ("lone", Method::Hardlink),
        ("intact", Method::Copy),
        ("edited", Method::Copy),
        ("other", Method::Copy),
    ];
    for (name, _) in placed {
        fs::write(project.root.join("data").join(name), b"original").unwrap();
    }
    // the cache's own name for the link's bytes; "lone" was a link once but has only its own
    let linked = fs::hard_link(
        project.root.join("data").join("link"),
        project.root.join("object"),
    )
    .is_ok();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    for (name, method) in placed {
        let path = format!("data/{name}");
        let mut record = record(&project.root, &path, b"original", method);
        if name == "other" {
            "another set".clone_into(&mut record.dataset);
        }
        survey.state.files.insert(path, record);
    }
    for name in ["copy", "link", "lone", "edited", "other"] {
        let path = project.root.join("data").join(name);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, b"Original").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }
    survey.edited.insert("data/edited".to_owned());

    let named = Scope::Only(std::collections::BTreeSet::from(["set".to_owned()]));
    survey.verify(&project.root, &named, None).unwrap();

    let (shared, alone): (Vec<_>, Vec<_>) = ["data/copy", "data/link", "data/lone"]
        .into_iter()
        .map(str::to_owned)
        .partition(|path| linked && path == "data/link");
    assert_eq!(survey.damaged, HashSet::from_iter(alone));
    assert_eq!(survey.elsewhere, HashSet::from_iter(shared));
    assert_eq!(survey.edited, HashSet::from(["data/edited".to_owned()]));
    fs::remove_file(project.root.join("data/intact")).unwrap();
    survey.disk.insert(
        "data/intact".to_owned(),
        survey.state.files["data/intact"].look,
    );
    assert!(
        survey.verify(&project.root, &Scope::All, None).is_err(),
        "an unreadable file stops it"
    );
}

#[test]
fn verification_reuses_intact_racy_hashes_without_hiding_unchecked_files() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let recent = old + std::time::Duration::from_secs(4);
    let files = [
        ("racy-intact", recent),
        ("racy-edited", recent),
        ("unchecked", old),
        ("outside", old),
    ];
    let write = |name: &str, time: SystemTime, bytes: &[u8]| {
        let path = project.root.join("data").join(name);
        fs::write(&path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    };
    for (name, time) in files {
        write(name, time, b"original");
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    for (name, _) in files {
        let path = format!("data/{name}");
        let mut placed = record(&project.root, &path, b"original", Method::Copy);
        if name == "outside" {
            "other".clone_into(&mut placed.dataset);
        }
        survey.state.files.insert(path, placed);
    }
    for (name, time) in files.into_iter().filter(|(name, _)| *name != "racy-intact") {
        write(name, time, b"modified");
        assert_eq!(
            Look::of(&fs::metadata(project.root.join("data").join(name)).unwrap()),
            survey.disk[&format!("data/{name}")]
        );
    }
    survey.hash_racy(&project.root, nanos(old) + 2_000_000_000, false);
    let checked = project.root.join("already-read");
    fs::rename(project.root.join("data/racy-intact"), &checked).unwrap();
    let scope = Scope::Only(std::collections::BTreeSet::from(["set".to_owned()]));
    let found = survey.verify(&project.root, &scope, None).unwrap();
    assert_eq!(found, HashSet::from(["data/unchecked".to_owned()]));
    assert_eq!(
        survey.edited,
        HashSet::from(["data/racy-edited".to_owned()])
    );
    assert_eq!(survey.damaged, HashSet::from(["data/unchecked".to_owned()]));
    assert!(survey.vouched.is_empty());
    fs::rename(checked, project.root.join("data/racy-intact")).unwrap();
    write("racy-intact", recent, b"modified");
    survey.damaged.insert("data/racy-intact".to_owned());
    assert_eq!(
        survey.verify(&project.root, &scope, None).unwrap(),
        HashSet::from(["data/racy-intact".to_owned(), "data/unchecked".to_owned()])
    );
}

#[test]
fn a_data_folder_on_another_device_allows_for_a_coarser_clock() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let path = project.root.join("data/table.csv");
    let stamp = |bytes: &[u8], time: SystemTime| {
        fs::write(&path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    };
    // fat cuts times down to two seconds: a file placed at 10.3 s and edited at 11 s keeps
    // 10 s, while a state on a finer clock was written at 10.5 s
    let placed = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_010);
    stamp(b"original", placed);
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    survey.state.files.insert(
        "data/table.csv".to_owned(),
        record(&project.root, "data/table.csv", b"original", Method::Copy),
    );
    stamp(b"modified", placed);
    let written = nanos(placed) + 500_000_000;

    survey.hash_racy(&project.root, written, false);
    assert!(survey.edited.is_empty(), "one clock dates both");
    survey.hash_racy(&project.root, written, true);
    assert_eq!(survey.edited, HashSet::from(["data/table.csv".to_owned()]));
}

#[test]
fn only_folders_on_different_devices_are_apart() {
    let dir = tempfile::tempdir().unwrap();
    let (data, own) = (dir.path().join("data"), dir.path().join(".fetchloom"));
    fs::create_dir(&data).unwrap();
    fs::create_dir(&own).unwrap();
    assert!(!apart(&data, &own));
    assert!(apart(&dir.path().join("missing"), &own));
    #[cfg(target_os = "linux")]
    assert!(apart(Path::new("/proc"), &own));
}

#[test]
fn change_marks_hash_old_links_and_refresh_their_listing_metadata() {
    use crate::cache::tests::{hardlinks, store};

    let dir = tempfile::tempdir().unwrap();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let good = store(&cache, b"intact");
    let bad = store(&cache, b"before");
    for (name, stored) in [("intact", &good), ("changed", &bad)] {
        let object = cache.object_path(&stored.digest);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&object).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "the test edits a cached object as a writable hardlink would"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(&object, permissions).unwrap();
        }
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
            .unwrap();
        fs::hard_link(object, project.root.join("data").join(name)).unwrap();
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    survey.state.files.insert(
        "data/intact".to_owned(),
        record(&project.root, "data/intact", b"intact", Method::Hardlink),
    );
    survey.state.files.insert(
        "data/changed".to_owned(),
        record(&project.root, "data/changed", b"before", Method::Hardlink),
    );
    fs::write(
        cache.object_path(&bad.digest),
        b"changed through another link",
    )
    .unwrap();
    cache.mark_changed(&good.digest).unwrap();
    cache.mark_changed(&bad.digest).unwrap();
    assert_eq!(survey.doubted(&project.root, &cache).unwrap().len(), 2);
    survey.check_doubted(&project.root, &cache).unwrap();
    assert_eq!(survey.elsewhere, HashSet::from(["data/changed".to_owned()]));
    assert_eq!(survey.vouched, HashSet::from(["data/intact".to_owned()]));
    assert_eq!(
        fs::read(project.root.join("data/changed")).unwrap(),
        b"changed through another link"
    );
}

/// a hardlink replaced by a file of its own that kept the recorded size and time shares
/// nothing with the cache, so hashing after a change mark finds it damaged.
#[test]
fn a_changed_file_that_is_no_longer_a_link_is_damaged_not_changed_elsewhere() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/file"), b"other").unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
    cache.mark_changed(&placed.sha256).unwrap();
    survey.state.files.insert("data/file".to_owned(), placed);

    survey.check_doubted(&project.root, &cache).unwrap();

    assert_eq!(survey.damaged, HashSet::from(["data/file".to_owned()]));
    assert!(survey.elsewhere.is_empty());
}

/// a mark records the changed link's own copy, so other projects linking that copy check it.
#[test]
fn a_changed_shared_link_the_cache_lost_is_marked_for_the_other_projects() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let file = project.root.join("data/file");
    fs::write(&file, b"other").unwrap();
    if fs::hard_link(&file, project.root.join("another project's link")).is_err() {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
    let digest = placed.sha256;
    survey.state.files.insert("data/file".to_owned(), placed);
    survey.elsewhere.insert("data/file".to_owned());

    audit(&project, &survey, &cache).unwrap();

    let copy = crate::cache::Identity::of(&file).unwrap();
    assert!(cache.marks().unwrap()[&digest].copies.contains(&copy));
}

#[test]
fn an_unreadable_doubted_link_is_not_silently_vouched_for() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/file"), b"bytes").unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let mut placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
    placed.look.modified = 0;
    survey.disk.insert("data/file".to_owned(), placed.look);
    cache.mark_changed(&placed.sha256).unwrap();
    survey.state.files.insert("data/file".to_owned(), placed);
    fs::remove_file(project.root.join("data/file")).unwrap();
    let error = survey.check_doubted(&project.root, &cache).unwrap_err();
    assert_eq!(error.kind, crate::error::Kind::LocalIo);
    assert!(error.message.contains("data/file"));
    assert_eq!(survey.vouched.len(), 0);
    assert_eq!(survey.elsewhere.len(), 0);
}

#[test]
fn a_mark_whose_time_a_coarse_clock_cut_down_still_doubts_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/file"), b"bytes").unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let mut placed = record(&project.root, "data/file", b"bytes", Method::Hardlink);
    placed.look.modified = 0;
    placed.checked = 1_700_000_000_500_000_000;
    survey.disk.insert("data/file".to_owned(), placed.look);
    cache.mark_changed(&placed.sha256).unwrap();
    let mark = project
        .root
        .join("cache")
        .join("changed")
        .join(placed.sha256.hex());
    survey.state.files.insert("data/file".to_owned(), placed);
    for (seconds, doubted) in [(1_700_000_000, true), (1_699_999_997, false)] {
        fs::File::options()
            .write(true)
            .open(&mark)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .unwrap();
        assert_eq!(
            survey.doubted(&project.root, &cache).unwrap().len(),
            usize::from(doubted),
            "{seconds}"
        );
    }
}

/// a changed link is damage while it is the cache's copy, or the old copy a mark records after
/// a repair; a file the project put at the link's name is its own edit, which needs no hash.
#[test]
fn a_changed_link_that_is_the_cached_copy_is_doubted_and_found_damaged() {
    use crate::cache::tests::{hardlinks, rewrite, seal, store};

    let dir = tempfile::tempdir().unwrap();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let stored = store(&cache, b"good");
    let object = cache.object_path(&stored.digest);
    for name in ["mine", "theirs", "replaced"] {
        fs::hard_link(&object, project.root.join("data").join(name)).unwrap();
    }
    let records: Vec<_> = ["mine", "theirs", "replaced"]
        .iter()
        .map(|name| {
            let path = format!("data/{name}");
            let placed = record(&project.root, &path, b"good", Method::Hardlink);
            (path, placed)
        })
        .collect();
    let replaced = project.root.join("data/replaced");
    fs::remove_file(&replaced).unwrap();
    fs::write(&replaced, b"a file of its own").unwrap();
    rewrite(
        &project.root.join("data/mine"),
        b"good, edited through a link",
    );
    seal(&object);
    let surveyed = || {
        let mut survey =
            Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
        survey.state.files.extend(records.iter().cloned());
        // as reading a state that records them does, so windows reads the links as they are
        survey.look_through_links(&project.root);
        survey
    };

    let mut survey = surveyed();
    assert_eq!(
        survey.doubted(&project.root, &cache).unwrap(),
        ["data/mine", "data/theirs"]
    );
    survey.check_doubted(&project.root, &cache).unwrap();
    assert_eq!(
        survey.elsewhere,
        HashSet::from(["data/mine".to_owned(), "data/theirs".to_owned()])
    );

    assert!(!cache.verify(&stored.digest).unwrap());
    assert!(store(&cache, b"good").new);
    fs::remove_file(project.root.join("data/mine")).unwrap();
    fs::hard_link(&object, project.root.join("data/mine")).unwrap();
    let mut survey = surveyed();
    survey.state.files.get_mut("data/mine").unwrap().look =
        Look::of(&fs::metadata(project.root.join("data/mine")).unwrap());
    assert_eq!(
        survey.doubted(&project.root, &cache).unwrap(),
        ["data/theirs"]
    );
    survey.check_doubted(&project.root, &cache).unwrap();
    assert_eq!(survey.elsewhere, HashSet::from(["data/theirs".to_owned()]));
}

/// a repair replaces a changed object. its mark must still reach a link to the old copy that
/// was checked before the change was found, while a link to the new copy stays trusted.
#[test]
fn a_repair_doubts_links_to_the_old_copy_alone() {
    use crate::cache::tests::{hardlinks, seal, store};

    let dir = tempfile::tempdir().unwrap();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    let cache = Cache::open(project.root.join("cache")).unwrap();
    let stored = store(&cache, b"good");
    let object = cache.object_path(&stored.digest);
    let long_ago = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
    let mut permissions = fs::metadata(&object).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test edits a cached object as a writable hardlink would"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&object, permissions).unwrap();
    let set_time = || {
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
    };
    set_time();
    fs::hard_link(&object, project.root.join("data/old")).unwrap();
    let mut old = record(&project.root, "data/old", b"good", Method::Hardlink);
    // the scan of the sync that repairs, and the last check of the old link, come first
    let checked = nanos(SystemTime::now());
    old.checked = checked;
    // the old copy changes keeping its size and time, then a fresh download replaces it
    fs::write(&object, b"evil").unwrap();
    set_time();
    seal(&object);
    assert!(store(&cache, b"good").new);
    fs::hard_link(&object, project.root.join("data/new")).unwrap();
    let mut new = record(&project.root, "data/new", b"good", Method::Hardlink);
    new.checked = checked;
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    survey.state.files.insert("data/old".to_owned(), old);
    survey.state.files.insert("data/new".to_owned(), new);
    assert_eq!(survey.doubted(&project.root, &cache).unwrap(), ["data/old"]);

    // a coarse clock can give the old copy the very time of the new one; only the file
    // itself says which copy a link names
    let old_path = project.root.join("data/old");
    let replaced = fs::metadata(&object).unwrap().modified().unwrap();
    let mut permissions = fs::metadata(&old_path).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test retimes the old copy, which its seal made read-only"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&old_path, permissions.clone()).unwrap();
    fs::File::options()
        .write(true)
        .open(&old_path)
        .unwrap()
        .set_modified(replaced)
        .unwrap();
    permissions.set_readonly(true);
    fs::set_permissions(&old_path, permissions).unwrap();
    let mut old = record(&project.root, "data/old", b"good", Method::Hardlink);
    old.checked = checked;
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let new = record(&project.root, "data/new", b"good", Method::Hardlink);
    assert_eq!(old.look.modified, new.look.modified);
    survey.state.files.insert("data/old".to_owned(), old);
    survey.state.files.insert(
        "data/new".to_owned(),
        crate::project::state::Placed { checked, ..new },
    );
    assert_eq!(survey.doubted(&project.root, &cache).unwrap(), ["data/old"]);
}

// runs where the disk ignores case, as windows and macos disks do by default
#[test]
fn alias_retargeting_carries_edits_and_the_original_listed_spelling() {
    use crate::project::lock::LockedFile;

    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/A.txt"), b"a").unwrap();
    if !project.root.join("data/a.txt").is_file() {
        return crate::test_skips::skip("aliases", "this disk keeps `a.txt` apart from `A.txt`");
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let placed = record(&project.root, "data/a.txt", b"a", Method::Copy);
    survey
        .state
        .files
        .insert("data/a.txt".to_owned(), placed.clone());
    survey.hash_racy(&project.root, 0, false);
    placed.racy_intact.set(()).unwrap();
    assert_eq!(survey.disk.len(), 1);
    assert_eq!(survey.disk["data/a.txt"], placed.look);
    assert_eq!(survey.listed["data/a.txt"], "data/A.txt");
    for paths in [
        &mut survey.edited,
        &mut survey.elsewhere,
        &mut survey.vouched,
    ] {
        paths.insert("data/a.txt".to_owned());
    }
    let locked = LockedFile {
        path: "A.txt".to_owned(),
        size: 1,
        sha256: placed.sha256,
        md5: None,
        url: "https://example.org/a".to_owned(),
        unpacks_to: None,
    };
    let target = Target {
        dataset: "set",
        path: "data/A.txt".to_owned(),
        sha256: placed.sha256,
        size: 1,
        copy: true,
        holder: crate::plan::Holder::File(&locked),
    };
    survey.retarget(&project.root, &[target]);
    assert_eq!(survey.state.files.len(), 1);
    assert_eq!(survey.state.files["data/A.txt"], placed);
    assert_eq!(survey.listed["data/A.txt"], "data/A.txt");
    for paths in [&survey.edited, &survey.elsewhere, &survey.vouched] {
        assert_eq!(paths, &HashSet::from(["data/A.txt".to_owned()]));
    }
    assert!(survey.needs_writing);
}

fn target<'a>(locked: &'a crate::project::lock::LockedFile, path: &str) -> Target<'a> {
    Target {
        dataset: "set",
        path: path.to_owned(),
        sha256: locked.sha256,
        size: locked.size,
        copy: false,
        holder: crate::plan::Holder::File(locked),
    }
}

fn locked(path: &str, bytes: &[u8]) -> crate::project::lock::LockedFile {
    crate::project::lock::LockedFile {
        path: path.to_owned(),
        size: bytes.len() as u64,
        sha256: crate::digest::sha256(bytes),
        md5: None,
        url: "https://example.org/a".to_owned(),
        unpacks_to: None,
    }
}

// runs where the disk sees composed and decomposed unicode as one name, as macos disks do
#[test]
fn a_name_listed_in_another_composition_is_the_targets_own_file() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/cafe\u{301}.txt"), b"a").unwrap();
    if !project.root.join("data/caf\u{e9}.txt").is_file() {
        return crate::test_skips::skip(
            "aliases",
            "this disk keeps two unicode forms of a name apart",
        );
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let listed = survey.disk.keys().next().unwrap().clone();
    let locked = locked("caf\u{e9}.txt", b"a");
    survey.retarget(&project.root, &[target(&locked, "data/caf\u{e9}.txt")]);
    assert_eq!(
        survey.disk.keys().collect::<Vec<_>>(),
        ["data/caf\u{e9}.txt"]
    );
    assert_eq!(survey.listed["data/caf\u{e9}.txt"], listed);
    assert!(survey.state.files.is_empty());
    assert!(!survey.needs_writing);
}

// runs where the disk sees composed and decomposed unicode as one name, as macos disks do
#[test]
fn a_writable_file_listed_in_another_composition_stays_writable_under_the_targets_name() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/cafe\u{301}.txt"), b"a").unwrap();
    if !project.root.join("data/caf\u{e9}.txt").is_file() {
        return crate::test_skips::skip(
            "aliases",
            "this disk keeps two unicode forms of a name apart",
        );
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    assert_eq!(survey.writable.len(), 1);
    let locked = locked("caf\u{e9}.txt", b"a");
    survey.retarget(&project.root, &[target(&locked, "data/caf\u{e9}.txt")]);
    assert!(
        survey.writable.contains("data/caf\u{e9}.txt"),
        "{:?}",
        survey.writable
    );
}

// runs where the disk ignores case, as windows and macos disks do by default
#[test]
fn a_name_listed_in_another_case_stays_apart_until_a_record_joins_them() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    fs::create_dir(project.root.join("data")).unwrap();
    fs::write(project.root.join("data/A.txt"), b"a").unwrap();
    if !project.root.join("data/a.txt").is_file() {
        return crate::test_skips::skip("aliases", "this disk keeps `a.txt` apart from `A.txt`");
    }
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let locked = locked("a.txt", b"a");
    survey.retarget(&project.root, &[target(&locked, "data/a.txt")]);
    assert_eq!(survey.disk.keys().collect::<Vec<_>>(), ["data/A.txt"]);
    assert!(survey.listed.is_empty());
}

#[cfg(unix)]
#[test]
fn whatever_stands_in_a_files_way_is_found_from_the_scan_alone() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    let data = project.root.join("data");
    for folder in ["real", "empty.csv", "full.csv/inner"] {
        fs::create_dir_all(data.join(folder)).unwrap();
    }
    fs::write(data.join("file"), b"notes").unwrap();
    fs::write(data.join("full.csv/inner/kept"), b"work").unwrap();
    symlink(dir.path(), data.join("linked")).unwrap();
    symlink(dir.path().join("elsewhere"), data.join("real/link.csv")).unwrap();
    let survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let blocker = |path: &str| {
        survey
            .blocker(path)
            .map(|found| (found.path, found.found, found.folder, found.clearable))
    };

    assert_eq!(
        blocker("data/file/a.csv"),
        Some(("data/file".to_owned(), Entry::File, true, true))
    );
    assert_eq!(
        blocker("data/linked/deep/a.csv"),
        Some(("data/linked".to_owned(), Entry::Link, true, true))
    );
    assert_eq!(
        blocker("data/real/link.csv"),
        Some(("data/real/link.csv".to_owned(), Entry::Link, false, true))
    );
    assert_eq!(
        blocker("data/empty.csv"),
        Some(("data/empty.csv".to_owned(), Entry::Folder, false, true))
    );
    assert_eq!(
        blocker("data/full.csv"),
        Some(("data/full.csv".to_owned(), Entry::Folder, false, false))
    );
    assert_eq!(blocker("data/real/free.csv"), None);
    assert_eq!(blocker("data/absent/a.csv"), None);
    assert_eq!(blocker("elsewhere/a.csv"), None);
}

/// an empty folder inside a folder in the way keeps it there, as removing files never takes it.
#[test]
fn a_folder_in_the_way_is_freed_only_when_removing_files_empties_it() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    let data = project.root.join("data");
    for folder in ["alone", "mixed", "nested/inner", "beside/empty", "empty"] {
        fs::create_dir_all(data.join(folder)).unwrap();
    }
    for file in [
        "alone/a",
        "mixed/a",
        "mixed/kept",
        "nested/inner/a",
        "beside/a",
        "going",
        "staying",
    ] {
        fs::write(data.join(file), file).unwrap();
    }
    let survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    let going: HashSet<String> = [
        "data/alone/a",
        "data/mixed/a",
        "data/nested/inner/a",
        "data/beside/a",
        "data/going",
    ]
    .map(str::to_owned)
    .into();
    let freeing = survey.freeing(&going);
    let frees = |path: &str, found: Entry| {
        freeing.frees(&Blocker {
            path: path.to_owned(),
            found,
            folder: false,
            clearable: true,
            dataset: None,
        })
    };
    assert!(frees("data/alone", Entry::Folder));
    assert!(frees("data/nested", Entry::Folder));
    assert!(!frees("data/mixed", Entry::Folder), "a file that stays");
    assert!(
        !frees("data/beside", Entry::Folder),
        "an empty folder stays"
    );
    assert!(!frees("data/empty", Entry::Folder), "nothing to remove");
    assert!(frees("data/going", Entry::File));
    assert!(!frees("data/staying", Entry::File));
}

#[cfg(unix)]
#[test]
fn scanning_skips_links_even_when_their_targets_are_files_or_folders() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("data/nested")).unwrap();
    fs::create_dir(dir.path().join("outside")).unwrap();
    fs::write(dir.path().join("outside/private"), b"outside").unwrap();
    fs::write(dir.path().join("data/nested/kept"), b"inside").unwrap();
    symlink(
        dir.path().join("outside"),
        dir.path().join("data/folder-link"),
    )
    .unwrap();
    symlink(
        dir.path().join("outside/private"),
        dir.path().join("data/file-link"),
    )
    .unwrap();
    let Scanned {
        found: files,
        temporaries: leftovers,
        entries,
        ..
    } = scan(
        dir.path(),
        "data",
        None,
        Threads::default(),
        &Stop::default(),
    )
    .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files["data/nested/kept"].size, 6);
    assert_eq!(leftovers, Vec::<String>::new());
    assert_eq!(
        entries,
        HashMap::from([
            ("data/nested".to_owned(), Entry::Folder),
            ("data/folder-link".to_owned(), Entry::Link),
            ("data/file-link".to_owned(), Entry::Link),
        ])
    );
    assert_eq!(
        scan(
            dir.path(),
            "missing",
            None,
            Threads::default(),
            &Stop::default()
        )
        .unwrap()
        .found
        .len(),
        0
    );
}

/// a retired folder lists links and a person's files but no staged copy, and `pets-extra` is
/// no part of `pets`.
#[cfg(unix)]
#[test]
fn a_retired_folder_is_the_highest_holding_no_wanted_file() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    let placed = [
        "data/pets/pets.csv",
        "data/pets-extra/keep.csv",
        "data/a/b/old.csv",
        "data/a/c/new.csv",
        "data/old/deep/only.csv",
        "data/gone/only.csv",
    ];
    let own = [
        "data/pets/notes.txt",
        "data/pets/sub/x.txt",
        "data/pets-extra/mine.txt",
        "data/a/b/own.txt",
        "data/a/b/.fetchloom-0123456789abcdef.copy",
        "data/old/mine.txt",
    ];
    for path in placed.iter().chain(&own) {
        let file = project.root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, path).unwrap();
    }
    std::os::unix::fs::symlink(dir.path(), project.root.join("data/pets/link")).unwrap();
    let mut survey = Survey::read(&project, "data", Threads::default(), &Stop::default()).unwrap();
    for path in placed {
        let record = record(&project.root, path, path.as_bytes(), Method::Copy);
        survey.state.files.insert(path.to_owned(), record);
    }
    let wanted = HashSet::from(["data/pets-extra/keep.csv", "data/a/c/new.csv"]);
    let going = HashSet::from([
        "data/pets/pets.csv",
        "data/a/b/old.csv",
        "data/old/deep/only.csv",
        "data/gone/only.csv",
    ]);

    assert_eq!(
        survey.retired(&wanted, &going),
        [
            Retired {
                folder: "data/a/b".to_owned(),
                unplaced: vec!["data/a/b/own.txt".to_owned()],
            },
            Retired {
                folder: "data/old".to_owned(),
                unplaced: vec!["data/old/mine.txt".to_owned()],
            },
            Retired {
                folder: "data/pets".to_owned(),
                unplaced: vec![
                    "data/pets/link".to_owned(),
                    "data/pets/notes.txt".to_owned(),
                    "data/pets/sub/x.txt".to_owned(),
                ],
            },
        ]
    );
}

/// a file reached through a link is not left behind, nor is a data folder file that a disk
/// ignoring case reads under another spelling.
#[cfg(unix)]
#[test]
fn only_real_files_outside_the_data_folder_are_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for path in [
        "data/iris/iris.csv",
        "elsewhere/x.csv",
        "inputs/raw/new.csv",
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"bytes").unwrap();
    }
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("linked")).unwrap();
    fs::create_dir(root.join("special")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere/x.csv"), root.join("special/x.csv")).unwrap();
    let mut state = State::default();
    for path in [
        "data/iris/iris.csv",
        "data/gone.csv",
        "linked/x.csv",
        "special/x.csv",
        "../outside.csv",
        "inputs/raw/new.csv",
    ] {
        state.files.insert(
            path.to_owned(),
            record(root, "elsewhere/x.csv", b"bytes", Method::Copy),
        );
    }

    assert_eq!(
        outside(root, "inputs/raw", &state, &HashMap::new()),
        ["data/iris/iris.csv"]
    );
    let names_alias = root.join("INPUTS").try_exists().unwrap();
    assert_eq!(
        outside(root, "INPUTS/raw", &state, &HashMap::new())
            .contains(&"inputs/raw/new.csv".to_owned()),
        !names_alias
    );
}

/// the old data folder's links are named, but not its folders or a copy a placement staged.
#[cfg(unix)]
#[test]
fn a_folder_left_behind_names_what_fetchloom_did_not_place() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let project = Project {
        root: root.to_path_buf(),
    };
    for path in [
        "data/iris/iris.csv",
        "data/iris/notes.txt",
        "data/iris/.fetchloom-0123456789abcdef.copy",
        "data/scratch/plot.py",
        "inputs/iris/iris.csv",
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"bytes").unwrap();
    }
    std::os::unix::fs::symlink(root.join("inputs"), root.join("data/shortcut")).unwrap();
    let mut survey =
        Survey::read(&project, "inputs", Threads::default(), &Stop::default()).unwrap();
    survey.state.files.insert(
        "data/iris/iris.csv".to_owned(),
        record(root, "data/iris/iris.csv", b"bytes", Method::Copy),
    );
    survey.outside = vec!["data/iris/iris.csv".to_owned()];
    let going = HashSet::from(["data/iris/iris.csv"]);
    let wanted = HashSet::from(["inputs/iris/iris.csv"]);

    let left = survey.left_behind(root, &wanted, &going).unwrap();
    assert_eq!(left.folder.as_deref(), Some("data"));
    assert_eq!(
        left.others,
        [
            "data/iris/notes.txt",
            "data/scratch/plot.py",
            "data/shortcut"
        ]
    );
}

/// the folder named for files left behind is the old data folder, found from where the same
/// files sit now, else the folder they share.
#[test]
fn the_folder_left_behind_never_holds_the_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    let project = Project {
        root: dir.path().to_path_buf(),
    };
    let left = |data_dir: &str, outside: &[&str], wanted: &[&str]| {
        let mut survey =
            Survey::read(&project, data_dir, Threads::default(), &Stop::default()).unwrap();
        survey.outside = outside.iter().map(|path| (*path).to_owned()).collect();
        let going: HashSet<&str> = outside.iter().copied().collect();
        let wanted: HashSet<&str> = wanted.iter().copied().collect();
        survey
            .left_behind(&project.root, &wanted, &going)
            .map(|left| left.folder)
    };

    let moved = ["data/iris/iris.csv", "data/pets/pets.csv"];
    assert_eq!(
        left("inputs", &moved[..1], &["inputs/iris/iris.csv"]),
        Some(Some("data".to_owned())),
        "the data folder, though one dataset alone was in it"
    );
    assert_eq!(
        left("inputs", &moved, &["inputs/iris/iris.csv"]),
        Some(Some("data".to_owned())),
        "the folder they share"
    );
    assert_eq!(
        left("data/new", &moved, &["data/new/iris/iris.csv"]),
        Some(None)
    );
    assert_eq!(
        left("Data", &moved, &[]),
        Some(None),
        "a disk that ignores case"
    );
    assert_eq!(left("inputs", &[], &["inputs/iris/iris.csv"]), None);
}
