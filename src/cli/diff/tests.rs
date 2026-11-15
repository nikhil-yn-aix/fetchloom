use super::*;
use crate::digest;
use crate::project::lock::derived::{Code, Input};
use crate::source::Access;

fn file(path: &str, bytes: &[u8]) -> LockedFile {
    LockedFile {
        path: path.to_owned(),
        size: bytes.len() as u64,
        sha256: digest::sha256(bytes),
        md5: None,
        url: format!("https://example.org/{path}"),
        unpacks_to: None,
    }
}

fn fetched(name: &str, version: u8, files: Vec<LockedFile>) -> Locked {
    Locked {
        name: name.to_owned(),
        reference: format!("zenodo:{name}"),
        spec: format!("sha256:{}", digest::sha256(name.as_bytes())),
        extract: true,
        web_page: false,
        resolved: format!("zenodo:{name}@{version}"),
        title: None,
        license: Some("cc-by-4.0".to_owned()),
        doi: None,
        access: Access::Public,
        timestamp: "2026-11-10T00:00:00Z".to_owned(),
        files,
    }
}

fn made(name: &str, seed: &str, files: &[(&str, &[u8])]) -> Derived {
    let files: Vec<Member> = files
        .iter()
        .map(|(path, bytes)| Member {
            path: (*path).to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
        })
        .collect();
    Derived {
        name: name.to_owned(),
        from: vec![Input {
            name: "iris".to_owned(),
            content: digest::sha256(b"iris"),
        }],
        run: vec![
            "python3".to_owned(),
            "sim.py".to_owned(),
            "{out}".to_owned(),
        ],
        code: vec![Code {
            path: "sim.py".to_owned(),
            sha256: digest::sha256(b"sim"),
            folder: false,
        }],
        env: [("SEED".to_owned(), seed.to_owned())].into(),
        key: digest::sha256(seed.as_bytes()),
        timestamp: "2026-11-10T00:00:00Z".to_owned(),
        tree: crate::cache::tree_hash(&files),
        files,
    }
}

/// the datasets that differ from `before` to `after`, each lock whole, with every list.
fn diffed(before: &Lock, after: &Lock) -> Vec<Dataset> {
    let whole = |lock: &Lock| Read {
        lock: lock.clone(),
        found: true,
        unlisted: Vec::new(),
    };
    compared(&whole(before), &whole(after))
}

/// what a person reads for each dataset: its summary, then a line for each file.
fn told(datasets: &[Dataset]) -> Vec<String> {
    datasets
        .iter()
        .flat_map(|dataset| {
            std::iter::once(summary(dataset, "A", "B"))
                .chain(dataset.files.iter().map(|file| line(&dataset.name, file)))
        })
        .collect()
}

/// a file changes with its bytes, never its link; a dataset whose pin is the same stays out, and
/// one added or removed lists every file it pins.
#[test]
fn each_dataset_and_file_that_differs_has_a_line() {
    let before = Lock {
        datasets: vec![
            fetched(
                "iris",
                1,
                vec![
                    file("a.csv", b"same"),
                    file("b.csv", b"short"),
                    file("c.csv", b"four"),
                    file("gone.csv", b"x"),
                ],
            ),
            fetched("old", 1, vec![file("old.csv", b"old bytes")]),
            fetched("still", 1, vec![file("s.csv", b"s")]),
        ],
        derived: vec![made("sim", "7", &[("draws.csv", b"1\n2\n")])],
        stated: Vec::new(),
    };
    let mut moved = file("a.csv", b"same");
    moved.url = "https://mirror.example.org/a.csv".to_owned();
    let after = Lock {
        datasets: vec![
            fetched(
                "iris",
                2,
                vec![
                    moved,
                    file("b.csv", b"longer"),
                    file("c.csv", b"FOUR"),
                    file("d.csv", b"new"),
                ],
            ),
            fetched("new", 3, vec![file("new.csv", b"fresh")]),
            fetched("still", 1, vec![file("s.csv", b"s")]),
        ],
        derived: vec![made("sim", "8", &[("draws.csv", b"3\n4\n")])],
        stated: Vec::new(),
    };
    let datasets = diffed(&before, &after);
    assert_eq!(
        told(&datasets),
        [
            "iris changed: version zenodo:iris@1 \u{2192} zenodo:iris@2; 1 file added, 1 removed, 2 changed",
            "M  iris/b.csv  5 B \u{2192} 6 B",
            "M  iris/c.csv  4 B, other bytes",
            "A  iris/d.csv  3 B",
            "D  iris/gone.csv  1 B",
            "new added: zenodo:new@3; 1 file added, 5 B",
            "A  new/new.csv  5 B",
            "old removed: zenodo:old@1; 1 file removed, 9 B",
            "D  old/old.csv  9 B",
            "sim changed: env SEED changed; 1 file changed",
            "M  sim/draws.csv  4 B, other bytes",
        ]
    );
    let iris = &datasets[0];
    assert_eq!((iris.added, iris.removed, iris.changed), (1, 1, 2));
    assert_eq!(
        iris.fields,
        [Field {
            name: "resolved",
            before: Some("zenodo:iris@1".to_owned()),
            after: Some("zenodo:iris@2".to_owned()),
        }]
    );
    assert_eq!(datasets[1].kind, "fetched");
    assert_eq!(datasets[3].kind, "recipe");
    // a recipe's env values may hold a key, so --json names the variables alone
    let env = datasets[3].fields.iter().find(|field| field.name == "env");
    assert_eq!(
        env.map(|field| (field.before.as_deref(), field.after.as_deref())),
        Some((Some("SEED=***"), Some("SEED=***")))
    );
    assert!(diffed(&after, &after).is_empty());
}

/// a dataset whose table data.toml renamed keeps its pin, so diff tells one rename, not a
/// dataset removed and another added; one that changed besides stays both.
#[test]
fn a_renamed_dataset_is_told_as_one() {
    let files = vec![file("penguins.csv", b"species\n")];
    let before = Lock {
        datasets: vec![fetched("penguins", 1, files.clone())],
        ..Lock::default()
    };
    let mut birds = fetched("penguins", 1, files.clone());
    birds.name = "birds".to_owned();
    let after = Lock {
        datasets: vec![birds],
        ..Lock::default()
    };
    let datasets = diffed(&before, &after);
    assert_eq!(
        told(&datasets),
        ["penguins renamed to birds; the same files"]
    );
    assert_eq!(datasets[0].renamed_from.as_deref(), Some("penguins"));
    let mut other = fetched("penguins", 2, files);
    other.name = "birds".to_owned();
    let changed = Lock {
        datasets: vec![other],
        ..Lock::default()
    };
    let datasets = diffed(&before, &changed);
    assert_eq!(datasets.len(), 2, "{:?}", told(&datasets));
}

/// a terminal shows a dataset's first file lines and counts the rest, naming the command that
/// lists them all; a pipe has every line.
#[test]
fn a_terminal_shows_the_first_files_and_counts_the_rest() {
    let before = Lock {
        datasets: vec![fetched("iris", 1, Vec::new())],
        ..Lock::default()
    };
    let files: Vec<LockedFile> = ["a.csv", "b.csv", "c.csv", "d.csv"]
        .into_iter()
        .map(|path| file(path, path.as_bytes()))
        .collect();
    let after = Lock {
        datasets: vec![fetched("iris", 1, files)],
        ..Lock::default()
    };
    let datasets = diffed(&before, &after);
    let again = "fetchloom diff HEAD~1 -v";
    assert_eq!(
        file_lines(&datasets[0], 2, again),
        "\nA  iris/a.csv  5 B\nA  iris/b.csv  5 B\nand 2 more \u{b7} `fetchloom diff HEAD~1 -v` names each"
    );
    assert_eq!(
        file_lines(&datasets[0], usize::MAX, again).lines().count(),
        5
    );
}

/// a spec is a hash of what data.toml asks; with `ref` and `web_page` the same, select or
/// exclude changed, while a dataset now made where it was fetched changes its kind alone.
#[test]
fn a_spec_tells_select_and_a_kind_is_said_alone() {
    let before = Lock {
        datasets: vec![
            fetched("iris", 1, vec![file("a.csv", b"a")]),
            fetched("swap", 1, vec![file("a.csv", b"a")]),
        ],
        ..Lock::default()
    };
    let mut selected = fetched("iris", 1, vec![file("a.csv", b"a")]);
    selected.spec = format!("sha256:{}", digest::sha256(b"select *.csv"));
    selected.extract = false;
    let mut swap = made("swap", "1", &[("a.csv", b"a")]);
    swap.name = "swap".to_owned();
    let after = Lock {
        datasets: vec![selected],
        derived: vec![swap],
        stated: Vec::new(),
    };
    assert_eq!(
        told(&diffed(&before, &after)),
        [
            "iris changed: select or exclude changed in data.toml; extract true \u{2192} false; the same files",
            "swap changed: kind fetched \u{2192} recipe; the same files",
        ]
    );
}

/// a name from a lock, which a commit anyone made can hold, reaches the terminal escaped.
#[test]
fn a_control_character_in_a_path_is_shown_escaped() {
    let before = Lock::default();
    let after = Lock {
        datasets: vec![fetched("iris", 1, vec![file("a\u{1b}[2J.csv", b"a")])],
        ..Lock::default()
    };
    let told = told(&diffed(&before, &after));
    assert!(!told.iter().any(|line| line.contains('\u{1b}')), "{told:?}");
}

/// a dataset `add --no-sync` pinned by the digests its source states is a fetched dataset not
/// downloaded yet: its files compare by their MD5 where no SHA-256 is stated, so a sync that
/// downloads the same bytes changes nothing of them.
#[test]
fn a_dataset_not_downloaded_yet_is_said_and_compared_by_its_stated_digests() {
    let md5 = |bytes: &[u8]| {
        let mut hasher = digest::Hasher::with_md5();
        hasher.update(bytes);
        hasher.finish().1
    };
    let stated_file = |path: &str, bytes: &[u8]| StatedFile {
        path: path.to_owned(),
        size: bytes.len() as u64,
        sha256: None,
        md5: md5(bytes),
        url: format!("https://example.org/{path}"),
    };
    let mut pin = fetched("zen", 1, Vec::new());
    pin.title = None;
    let stated = Stated {
        written: crate::project::lock::Words::of(&pin),
        pin,
        files: vec![stated_file("a.csv", b"aa"), stated_file("b.csv", b"bbb")],
    };
    let not_yet = Lock {
        stated: vec![stated],
        ..Lock::default()
    };
    assert_eq!(
        told(&diffed(&Lock::default(), &not_yet)),
        [
            "zen added: zenodo:zen@1, not downloaded yet; 2 files added, 5 B",
            "A  zen/a.csv  2 B",
            "A  zen/b.csv  3 B",
        ]
    );
    let with_md5 = |path: &str, bytes: &[u8]| LockedFile {
        md5: md5(bytes),
        ..file(path, bytes)
    };
    let downloaded = Lock {
        datasets: vec![fetched(
            "zen",
            1,
            vec![with_md5("a.csv", b"aa"), with_md5("b.csv", b"BBB")],
        )],
        ..Lock::default()
    };
    assert_eq!(
        told(&diffed(&not_yet, &downloaded)),
        [
            "zen changed: now downloaded; 1 file changed",
            "M  zen/b.csv  3 B, other bytes",
        ]
    );
}

/// a recipe's change is said by what changed in it: an input whose bytes changed by its name,
/// "from" only when the inputs it names do, each file of its code, and each variable with its
/// values; a version that is the link itself is not said twice.
#[test]
fn a_recipe_change_names_what_changed_in_it() {
    let was = made("sim", "7", &[("draws.csv", b"1\n")]);
    let mut now = made("sim", "7", &[("draws.csv", b"1\n")]);
    now.from[0].content = digest::sha256(b"iris, other bytes");
    now.code[0].sha256 = digest::sha256(b"sim, edited");
    now.env.insert("RUNS".to_owned(), "3".to_owned());
    now.key = digest::sha256(b"other");
    let before = Lock {
        derived: vec![was.clone()],
        ..Lock::default()
    };
    let after = Lock {
        derived: vec![now.clone()],
        ..Lock::default()
    };
    assert_eq!(
        told(&diffed(&before, &after)),
        ["sim changed: iris changed; sim.py changed; env RUNS added; the same files"]
    );
    now.from.push(Input {
        name: "penguins".to_owned(),
        content: digest::sha256(b"penguins"),
    });
    now.from[0].content = was.from[0].content;
    now.code = was.code.clone();
    now.env = was.env.clone();
    let after = Lock {
        derived: vec![now],
        ..Lock::default()
    };
    assert_eq!(
        told(&diffed(&before, &after)),
        ["sim changed: from iris \u{2192} iris, penguins; the same files"]
    );
    let mut link = fetched("raw", 1, vec![file("a.csv", b"a")]);
    link.reference = "https://lab.example/a.csv".to_owned();
    link.resolved = link.reference.clone();
    let mut moved = link.clone();
    moved.reference = "https://lab.example/b.csv".to_owned();
    moved.resolved = moved.reference.clone();
    let (before, after) = (
        Lock {
            datasets: vec![link],
            ..Lock::default()
        },
        Lock {
            datasets: vec![moved],
            ..Lock::default()
        },
    );
    assert_eq!(
        told(&diffed(&before, &after)),
        [
            "raw changed: ref https://lab.example/a.csv \u{2192} https://lab.example/b.csv; the same files"
        ]
    );
}

/// a dataset whose file list a side's commit lacks is told with what else changed, its files
/// left uncompared rather than all counted as added or removed.
#[test]
fn a_dataset_whose_list_a_commit_lacks_is_said_so() {
    let before = Read {
        lock: Lock {
            derived: vec![made("sim", "7", &[("draws.csv", b"1\n")])],
            ..Lock::default()
        },
        found: true,
        unlisted: Vec::new(),
    };
    let mut kept = made("sim", "8", &[]);
    kept.files.clear();
    let after = Read {
        lock: Lock {
            derived: vec![kept],
            ..Lock::default()
        },
        found: true,
        unlisted: vec!["sim".to_owned()],
    };
    let datasets = compared(&before, &after);
    assert_eq!(
        told(&datasets),
        [
            "sim changed: env SEED changed; its file list is not in B, so its files were not compared"
        ]
    );
    assert_eq!(datasets[0].unlisted, ["to"]);
    assert_eq!(datasets[0].files, []);
}
