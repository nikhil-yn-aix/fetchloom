use super::*;
use crate::digest;
use crate::project::lock::{Locked, LockedFile};

fn manifest(text: &str) -> Manifest {
    Manifest::parse(text).unwrap()
}

fn lock(datasets: &[(&str, &[&str])]) -> Lock {
    Lock {
        datasets: datasets
            .iter()
            .map(|(name, paths)| Locked {
                name: (*name).to_owned(),
                reference: "https://example.org/x".to_owned(),
                spec: String::new(),
                extract: true,
                resolved: String::new(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: String::new(),
                files: paths
                    .iter()
                    .map(|path| LockedFile {
                        path: (*path).to_owned(),
                        size: 1,
                        sha256: digest::sha256(path.as_bytes()),
                        md5: None,
                        url: String::new(),
                        unpacks_to: None,
                    })
                    .collect(),
            })
            .collect(),
        ..Lock::default()
    }
}

const TWO: &str =
    "[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"elsewhere/b\"\ncopy = true\n";
const ONE: &str = "[datasets.a]\nref = \"x\"\n";

/// the paths a lock places when no archive was unpacked yet.
fn targets<'a>(manifest: &Manifest, lock: &'a Lock) -> Result<Vec<Target<'a>>, Error> {
    plan(manifest, lock, &|_| None).map(|plan| plan.targets)
}

fn member(path: &str) -> Member {
    Member {
        path: path.to_owned(),
        size: 1,
        sha256: digest::sha256(path.as_bytes()),
    }
}

/// where dataset `a` places its files when `archive` unpacks to `members`, beside `others`.
fn unpacked(archive: &str, members: &[&str], others: &[&str]) -> Vec<String> {
    let mut locked = lock(&[("a", others)]);
    let mut file = locked.datasets[0]
        .files
        .first()
        .cloned()
        .unwrap_or(LockedFile {
            path: String::new(),
            size: 1,
            sha256: digest::sha256(b"archive"),
            md5: None,
            url: String::new(),
            unpacks_to: None,
        });
    file.path = archive.to_owned();
    file.sha256 = digest::sha256(archive.as_bytes());
    file.unpacks_to = Some(digest::sha256(b"tree"));
    locked.datasets[0].files.push(file);
    let members: Vec<Member> = members.iter().map(|path| member(path)).collect();
    let planned = plan(&manifest(ONE), &locked, &|unpacking| {
        (unpacking.file.path == archive).then(|| members.clone())
    })
    .unwrap();
    let mut paths: Vec<String> = planned
        .targets
        .into_iter()
        .filter(|target| target.file().is_some_and(|file| file.path == archive))
        .map(|target| target.path)
        .collect();
    paths.sort();
    paths
}

/// data.toml `text` with `datasets` pinned as it asks, each archive among them one that
/// unpacks.
fn pinned(text: &str, datasets: &[(&str, &[&str])]) -> (Manifest, Lock) {
    let manifest = manifest(text);
    let mut locked = lock(datasets);
    for dataset in &mut locked.datasets {
        if let Some(source) = manifest.datasets[&dataset.name].source() {
            dataset.spec = crate::project::lock::spec(source);
        }
        for file in &mut dataset.files {
            if archive::by_name(&file.path) == Some(archive::Packed::Archive) {
                file.unpacks_to = Some(digest::sha256(file.path.as_bytes()));
            }
        }
    }
    (manifest, locked)
}

/// where `lock` places its files when each archive unpacks to the members `archives` names
/// for its path, in the order planned.
fn placed_from(
    manifest: &Manifest,
    lock: &Lock,
    archives: &[(&str, &[&str])],
) -> Result<Vec<String>, Error> {
    let members = |unpacking: &Archive<'_>| {
        archives
            .iter()
            .find(|(archive, _)| *archive == unpacking.file.path)
            .map(|(_, paths)| paths.iter().map(|path| member(path)).collect())
    };
    Ok(plan(manifest, lock, &members)?
        .targets
        .into_iter()
        .map(|target| target.path)
        .collect())
}

/// a split archive lands by its name before the counter, its parts landing nowhere; kept
/// packed, it lands as its parts.
#[test]
fn a_split_archive_lands_as_the_whole_would() {
    let parts: &[&str] = &["scans.zip.001", "scans.zip.002"];
    let members: &[&str] = &["top/a.csv", "top/b.csv", "c.csv"];
    let with_readme = [&["README.md"][..], parts].concat();
    let (manifest, mut beside) = pinned(ONE, &[("a", &with_readme)]);
    beside.datasets[0].files[1].unpacks_to = Some(digest::sha256(b"tree"));
    assert_eq!(
        placed_from(&manifest, &beside, &[("scans.zip.001", members)]).unwrap(),
        [
            "data/a/README.md",
            "data/a/scans/top/a.csv",
            "data/a/scans/top/b.csv",
            "data/a/scans/c.csv"
        ]
    );
    let unknown = plan(&manifest, &beside, &|_| None).unwrap();
    assert_eq!(unknown.targets.len(), 1, "README.md alone");
    assert_eq!(
        unknown.unknown,
        [("a", &beside.datasets[0].files[1])],
        "never unpacked here, by its first part"
    );
    let (manifest, mut alone) = pinned(ONE, &[("a", parts)]);
    alone.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
    assert_eq!(
        placed_from(&manifest, &alone, &[("scans.zip.001", &members[..2])]).unwrap(),
        ["data/a/a.csv", "data/a/b.csv"],
        "alone, without the folder its files share"
    );
    let (manifest, packed) = pinned(ONE, &[("a", parts)]);
    assert_eq!(
        placed_from(&manifest, &packed, &[]).unwrap(),
        ["data/a/scans.zip.001", "data/a/scans.zip.002"]
    );
}

/// a kept file lands where it would without the filter, though those left share no top folder.
#[test]
fn member_filters_leave_other_members_where_they_were() {
    let members: &[&str] = &["top/a.csv", "top/b.csv", "other.txt"];
    let (plain, locked) = pinned(ONE, &[("a", &["x.zip"])]);
    let everything = placed_from(&plain, &locked, &[("x.zip", members)]).unwrap();
    assert_eq!(
        everything,
        ["data/a/top/a.csv", "data/a/top/b.csv", "data/a/other.txt"]
    );
    let (filtered, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"x.zip\"]\nexclude = \"other.txt\"\n"),
        &[("a", &["x.zip"])],
    );
    assert_eq!(
        placed_from(&filtered, &locked, &[("x.zip", members)]).unwrap(),
        ["data/a/top/a.csv", "data/a/top/b.csv"]
    );
    let (selected, locked) = pinned(
        &format!(
            "{ONE}[datasets.a.members.\"x.zip\"]\nselect = \"top/*.csv\"\nexclude = \"top/a.csv\"\n"
        ),
        &[("a", &["x.zip"])],
    );
    assert_eq!(
        placed_from(&selected, &locked, &[("x.zip", members)]).unwrap(),
        ["data/a/top/b.csv"]
    );
}

/// names are checked where they land below the dataset's folder, skipping members left out.
#[test]
fn names_windows_cannot_store_are_found_in_files_and_archives() {
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"x.zip\"]\nexclude = \"top/left*\"\n"),
        &[("a", &["ok.csv", "what?.csv", "x.zip"])],
    );
    let inside: &[&str] = &[
        "top/con.txt",
        "top/dot.",
        "top/fine.txt",
        "top/left|out.txt",
    ];
    let members = |unpacking: &Archive<'_>| {
        (unpacking.file.path == "x.zip").then(|| inside.iter().map(|path| member(path)).collect())
    };
    assert_eq!(
        unportable(&manifest, &locked.datasets[0], &members),
        ["top/con.txt", "top/dot.", "what?.csv"],
        "a lone top folder comes out as it is"
    );
    let (manifest, locked) = pinned(ONE, &[("a", &["ok.csv", "con-data.txt"])]);
    assert_eq!(
        unportable(&manifest, &locked.datasets[0], &|_| None),
        Vec::<String>::new()
    );
}

/// two archives of one dataset may hold the same path; a table names one archive.
#[test]
fn a_member_exclude_reaches_only_its_own_archive() {
    let both: &[&str] = &["images/x.png", "labels.csv"];
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"train.zip\"]\nexclude = [\"images/x.png\"]\n"),
        &[("a", &["test.zip", "train.zip"])],
    );
    assert_eq!(
        placed_from(
            &manifest,
            &locked,
            &[("train.zip", both), ("test.zip", both)]
        )
        .unwrap(),
        [
            "data/a/test/images/x.png",
            "data/a/test/labels.csv",
            "data/a/train/labels.csv"
        ]
    );
}

/// a table orphaned when an upgrade renames its archive is refused, so the files it left out
/// stay out. a pin data.toml moved past is planned as it stands until sync pins again.
#[test]
fn a_members_table_for_an_archive_the_lock_does_not_pin_is_refused() {
    let text = format!("{ONE}\n[datasets.a.members.\"pets.zip\"]\nexclude = \"x\"\n");
    let (manifest, locked) = pinned(&text, &[("a", &["README", "pets-v2.zip"])]);
    let error = placed_from(&manifest, &locked, &[("pets-v2.zip", &["x", "y"])]).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "members.\"pets.zip\" in datasets.a names no archive data.lock pins for it"
    );
    assert_eq!(
        error.details,
        ["data.toml:4   [datasets.a.members.\"pets.zip\"]"]
    );
    assert_eq!(
        error.help.as_deref(),
        Some("name an archive by its path in data.lock, such as \"pets-v2.zip\"")
    );
    let (manifest, mut stale) = pinned(&text, &[("a", &["README", "pets-v2.zip"])]);
    stale.datasets[0].spec = "sha256:older".to_owned();
    assert_eq!(
        placed_from(&manifest, &stale, &[("pets-v2.zip", &["x", "y"])]).unwrap(),
        ["data/a/README", "data/a/pets-v2/x", "data/a/pets-v2/y"]
    );
    let glob = format!("{ONE}\n[datasets.a.members.\"pets-v1*.zip\"]\nexclude = \"x\"\n");
    let (manifest, locked) = pinned(&glob, &[("a", &["README", "pets-v2.zip"])]);
    let error = placed_from(&manifest, &locked, &[("pets-v2.zip", &["x", "y"])]).unwrap_err();
    assert_eq!(
        error.message,
        "members.\"pets-v1*.zip\" in datasets.a names no archive data.lock pins for it",
        "a glob that matches no archive is refused as such a name is"
    );
}

/// three webdataset shards beside a readme, and the files of one sample each holds.
const SHARDS: [&str; 4] = [
    "README",
    "shard-000000.tar",
    "shard-000001.tar",
    "shard-000002.tar",
];
const SAMPLE: [&str; 3] = ["s.json", "s.txt", "s.wav"];

/// each shard of [`SHARDS`] unpacking to [`SAMPLE`], and the archives of `more` to theirs.
fn shards<'a>(more: &[(&'a str, &'a [&'a str])]) -> Vec<(&'a str, &'a [&'a str])> {
    SHARDS[1..]
        .iter()
        .map(|shard| (*shard, &SAMPLE[..]))
        .chain(more.iter().copied())
        .collect()
}

/// a glob key chooses as a table under each archive's path would, and the archive's own table
/// wins. `*` stays in one folder, so a shard in a folder of the dataset is not matched.
#[test]
fn a_glob_key_chooses_the_files_of_every_archive_it_matches() {
    let paths = [&SHARDS[..], &["train/shard-000003.tar"]].concat();
    let (manifest, locked) = pinned(
        &format!(
            "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = \"*.wav\"\n[datasets.a.members.\"shard-000001.tar\"]\nselect = \"*.txt\"\n"
        ),
        &[("a", &paths)],
    );
    assert_eq!(
        placed_from(
            &manifest,
            &locked,
            &shards(&[("train/shard-000003.tar", &SAMPLE[..])])
        )
        .unwrap(),
        [
            "data/a/README",
            "data/a/shard-000000/s.wav",
            "data/a/shard-000001/s.txt",
            "data/a/shard-000002/s.wav",
            "data/a/train/shard-000003/s.json",
            "data/a/train/shard-000003/s.txt",
            "data/a/train/shard-000003/s.wav",
        ]
    );
}

/// planning stops naming both and the archive, unless it has a table of its own, which wins;
/// gc and status, which cannot stop, take no table and keep every file.
#[test]
fn two_glob_keys_matching_one_archive_are_refused() {
    let text = format!(
        "{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = \"*.wav\"\n[datasets.a.members.\"*-000001.tar\"]\nselect = \"*.txt\"\n"
    );
    let (manifest, locked) = pinned(&text, &[("a", &SHARDS)]);
    let error = placed_from(&manifest, &locked, &shards(&[])).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "members.\"shard-*.tar\" and members.\"*-000001.tar\" in datasets.a both match shard-000001.tar"
    );
    assert_eq!(
        error.details,
        [
            "data.toml:3   [datasets.a.members.\"shard-*.tar\"]",
            "data.toml:5   [datasets.a.members.\"*-000001.tar\"]"
        ]
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "change one key so only one matches it, or give it a table of its own, as [datasets.a.members.\"shard-000001.tar\"], which wins over both"
        )
    );
    let members = &manifest.datasets["a"].source().unwrap().members;
    assert!(members.get("shard-000001.tar").is_none());
    let (manifest, locked) = pinned(
        &format!("{text}[datasets.a.members.\"shard-000001.tar\"]\nexclude = \"s.json\"\n"),
        &[("a", &SHARDS)],
    );
    assert_eq!(
        placed_from(&manifest, &locked, &shards(&[])).unwrap(),
        [
            "data/a/README",
            "data/a/shard-000000/s.wav",
            "data/a/shard-000001/s.txt",
            "data/a/shard-000001/s.wav",
            "data/a/shard-000002/s.wav",
        ]
    );
}

/// three shards of one dataset that differ: only the second holds a `.flac`, and the third
/// has no `.txt`.
fn uneven() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("shard-000000.tar", &["s.json", "s.txt", "s.wav"]),
        ("shard-000001.tar", &["s.flac", "s.json", "s.txt", "s.wav"]),
        ("shard-000002.tar", &["s.json", "s.wav"]),
    ]
}

/// each select needs a file in one covered archive, not all; a shard left none places none.
#[test]
fn a_glob_tables_select_places_each_kind_where_it_is() {
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = [\"*.flac\", \"*.txt\"]\n"),
        &[("a", &SHARDS)],
    );
    assert_eq!(
        placed_from(&manifest, &locked, &uneven()).unwrap(),
        [
            "data/a/README",
            "data/a/shard-000000/s.txt",
            "data/a/shard-000001/s.flac",
            "data/a/shard-000001/s.txt",
        ]
    );
}

/// the refusal names key and pattern, as for a typo, once every archive covered is known.
#[test]
fn a_glob_tables_select_that_matches_in_no_archive_is_refused() {
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"shard-*.tar\"]\nselect = [\"*.flca\", \"*.txt\"]\n"),
        &[("a", &SHARDS)],
    );
    let error = placed_from(&manifest, &locked, &uneven()).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "the select pattern \"*.flca\" in members.\"shard-*.tar\" of datasets.a matches no file inside the 3 archives it covers"
    );
    assert_eq!(
        error.details,
        ["data.toml:4   select = [\"*.flca\", \"*.txt\"]"]
    );
    assert_eq!(
        error.help.as_deref(),
        Some("paths inside shard-000000.tar look like \"s.json\"")
    );
    let unknown: Vec<_> = uneven()
        .into_iter()
        .filter(|(archive, _)| *archive != "shard-000001.tar")
        .collect();
    assert!(
        placed_from(&manifest, &locked, &unknown).is_ok(),
        "a shard not unpacked here yet may hold it"
    );
}

/// the refusal names the key, as an own table's does, once every covered archive is known.
#[test]
fn a_glob_table_that_leaves_every_archive_no_file_is_refused() {
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"shard-*.tar\"]\nexclude = \"s.*\"\n"),
        &[("a", &SHARDS)],
    );
    let error = placed_from(&manifest, &locked, &uneven()).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "select and exclude in members.\"shard-*.tar\" of datasets.a leave no file of the 3 archives it covers"
    );
    assert_eq!(error.details, ["data.toml:4   exclude = \"s.*\""]);
    assert_eq!(
        error.help.as_deref(),
        Some(
            "loosen the patterns, or exclude \"shard-*.tar\" in datasets.a to leave every archive it covers out"
        )
    );
    let shards = uneven();
    assert!(
        placed_from(&manifest, &locked, &shards[..2]).is_ok(),
        "a shard not unpacked here yet may keep a file"
    );
}

#[test]
fn an_unmatched_member_select_suggests_the_top_folder() {
    let members: &[&str] = &["pets-v1/images/cat.png", "pets-v1/labels.csv"];
    for (select, help) in [
        (
            "images/cat.png",
            "try \"pets-v1/images/cat.png\": paths inside an archive start with its top folder",
        ),
        (
            "pictures/*.png",
            "paths inside pets.zip look like \"pets-v1/images/cat.png\"",
        ),
    ] {
        let (manifest, locked) = pinned(
            &format!("{ONE}[datasets.a.members.\"pets.zip\"]\nselect = [\"{select}\"]\n"),
            &[("a", &["pets.zip"])],
        );
        let error = placed_from(&manifest, &locked, &[("pets.zip", members)]).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            format!(
                "the select pattern \"{select}\" in members.\"pets.zip\" of datasets.a matches no file inside pets.zip"
            )
        );
        assert_eq!(
            error.details,
            [format!("data.toml:4   select = [\"{select}\"]")]
        );
        assert_eq!(error.help.as_deref(), Some(help));
    }
}

#[test]
fn member_filters_that_leave_an_archive_no_file_are_refused() {
    let (manifest, locked) = pinned(
        &format!("{ONE}[datasets.a.members.\"pets.zip\"]\nexclude = [\"**\"]\n"),
        &[("a", &["README", "pets.zip"])],
    );
    let error = placed_from(&manifest, &locked, &[("pets.zip", &["a/x", "a/y"])]).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "select and exclude in members.\"pets.zip\" of datasets.a leave no file of pets.zip"
    );
    assert_eq!(error.details, ["data.toml:4   exclude = [\"**\"]"]);
    assert_eq!(
        error.help.as_deref(),
        Some(
            "loosen the patterns, or exclude \"pets.zip\" in datasets.a to leave the whole archive out"
        )
    );
}

/// a dataset's exclude leaves an archive's files where they are, so one naming them is found by
/// the path they land at; one that left out a listed file, as `images/x.png`, did its job.
#[test]
fn an_exclude_naming_files_inside_an_archive_is_found() {
    let (manifest, locked) = pinned(
        &format!("{ONE}exclude = [\"images/x.png\", \"labels.txt\", \"images/*.jpg\"]\n"),
        &[("a", &["pets.zip"])],
    );
    let members: &[&str] = &[
        "pets-v1/images/x.png",
        "pets-v1/images/y.jpg",
        "pets-v1/images/z.jpg",
        "pets-v1/labels.csv",
    ];
    let paths = |archive: &str| -> Option<Vec<Member>> {
        (archive == "pets.zip").then(|| members.iter().map(|path| member(path)).collect())
    };
    let planned = plan(&manifest, &locked, &|archive| paths(&archive.file.path)).unwrap();
    let missed = vec!["labels.txt".to_owned(), "images/*.jpg".to_owned()];
    let unlisted = Unlisted::from([("a".to_owned(), missed)]);
    assert_eq!(
        unreached(&manifest, &planned.targets, &unlisted),
        [Unreached {
            dataset: "a".to_owned(),
            pattern: "images/*.jpg".to_owned(),
            archives: vec!["pets.zip".to_owned()],
            paths: vec![
                "data/a/images/y.jpg".to_owned(),
                "data/a/images/z.jpg".to_owned()
            ],
        },]
    );
    let (plain, locked) = pinned(ONE, &[("a", &["pets.zip"])]);
    let planned = plan(&plain, &locked, &|archive| paths(&archive.file.path)).unwrap();
    assert_eq!(unreached(&plain, &planned.targets, &unlisted), []);
}

/// data.lock keeps no file an exclude left out, so a plan of it alone cannot tell an exclude
/// that left out a listed `train.csv` beside `train.csv.zip` from one that missed.
#[test]
fn a_lock_alone_never_calls_an_exclude_unreached() {
    let (manifest, locked) = pinned(
        &format!("{ONE}exclude = [\"train.csv\"]\n"),
        &[("a", &["test.csv", "train.csv.zip"])],
    );
    let planned = plan(&manifest, &locked, &|archive| {
        (archive.file.path == "train.csv.zip").then(|| vec![member("train.csv")])
    })
    .unwrap();
    assert_eq!(unreached(&manifest, &planned.targets, &Unlisted::new()), []);
}

#[test]
fn files_land_in_their_dataset_folder() {
    let locked = lock(&[("a", &["x.csv", "sub/y.csv"]), ("b", &["z.csv"])]);
    let targets = targets(&manifest(TWO), &locked).unwrap();
    let paths: Vec<&str> = targets.iter().map(|target| target.path.as_str()).collect();
    assert_eq!(
        paths,
        ["data/a/x.csv", "data/a/sub/y.csv", "data/elsewhere/b/z.csv"]
    );
    assert!(targets[2].copy && !targets[0].copy);
}

#[test]
fn a_dataset_at_the_root_lands_in_the_data_folder() {
    let root = manifest("[datasets.a]\nref = \"x\"\npath = \".\"\n");
    assert_eq!(folder(&root, "a"), "data");
    let locked = lock(&[("a", &["train.csv", "raw/x.csv"])]);
    let paths: Vec<String> = targets(&root, &locked)
        .unwrap()
        .into_iter()
        .map(|target| target.path)
        .collect();
    assert_eq!(paths, ["data/train.csv", "data/raw/x.csv"]);
}

#[test]
fn datasets_share_a_folder_when_no_file_clashes() {
    let shared = manifest(
        "[datasets.a]\nref = \"w\"\npath = \"splits\"\n[datasets.b]\nref = \"x\"\npath = \"splits\"\n[datasets.c]\nref = \"y\"\npath = \"splits/raw\"\n[datasets.d]\nref = \"z\"\npath = \".\"\n",
    );
    let locked = lock(&[
        ("a", &["train.csv"]),
        ("b", &["test.csv", "raw/labels.csv"]),
        ("c", &["one.png"]),
        ("d", &["README"]),
    ]);
    let paths: Vec<String> = targets(&shared, &locked)
        .unwrap()
        .into_iter()
        .map(|target| target.path)
        .collect();
    assert_eq!(
        paths,
        [
            "data/splits/train.csv",
            "data/splits/test.csv",
            "data/splits/raw/labels.csv",
            "data/splits/raw/one.png",
            "data/README"
        ]
    );
}

/// two datasets' files on one path clash even with the same bytes, since each placed file
/// has one dataset that owns it.
#[test]
fn a_clash_between_datasets_names_both_and_the_path() {
    let flat = manifest(
        "[datasets.a]\nref = \"x\"\npath = \".\"\n[datasets.b]\nref = \"y\"\npath = \".\"\n",
    );
    for (a, b, said) in [
        (
            "README",
            "README",
            "datasets a and b would both place data/README",
        ),
        (
            "README",
            "readme",
            "datasets a and b would place data/README and data/readme, the same file on a disk that ignores case",
        ),
        (
            "raw",
            "raw/x.csv",
            "dataset a places a file at data/raw, but dataset b needs a folder there for data/raw/x.csv",
        ),
    ] {
        let error = targets(&flat, &lock(&[("a", &[a]), ("b", &[b])])).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.message, said);
        assert_eq!(
            error.help.as_deref(),
            Some("give datasets.a or datasets.b another path in data.toml")
        );
    }
}

#[test]
fn a_dataset_that_is_one_archive_gets_its_files_without_their_shared_top_folder() {
    assert_eq!(
        unpacked("MNIST.zip", &["mnist/train.csv", "mnist/test/a.csv"], &[]),
        ["data/a/test/a.csv", "data/a/train.csv"]
    );
    assert_eq!(
        unpacked("v2/data.tar.gz", &["a.csv", "b/c.csv"], &[]),
        ["data/a/v2/a.csv", "data/a/v2/b/c.csv"]
    );
    assert_eq!(
        unpacked("big.csv.zip", &["big.csv"], &[]),
        ["data/a/big.csv"]
    );
}

#[test]
fn an_archive_beside_other_files_unpacks_where_it_sits() {
    assert_eq!(
        unpacked("images.zip", &["imgs/1.png", "imgs/2.png"], &["labels.csv"]),
        ["data/a/imgs/1.png", "data/a/imgs/2.png"],
        "a lone top folder comes out as it is"
    );
    assert_eq!(
        unpacked("big.csv.zip", &["big.csv"], &["README.md"]),
        ["data/a/big.csv"],
        "so does a lone file"
    );
    assert_eq!(
        unpacked("raw/train.tar.zst", &["1.png", "2.png"], &["README.md"]),
        ["data/a/raw/train/1.png", "data/a/raw/train/2.png"],
        "loose files get a folder named after the archive"
    );
}

#[test]
fn an_archive_not_unpacked_here_is_reported_not_planned() {
    let mut locked = lock(&[("a", &["x.zip", "y.csv"])]);
    locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
    let planned = plan(&manifest(ONE), &locked, &|_| None).unwrap();
    assert_eq!(planned.targets.len(), 1);
    assert_eq!(planned.unknown.len(), 1);
    assert_eq!(planned.unknown[0].1.path, "x.zip");
}

/// for a file beside an archive holding one of the same name, either can be left out, the
/// member only while its table keeps another file, or the archive kept packed. two archives
/// with one path are both named, with the same remedies beside a dataset for each.
#[test]
fn a_clash_between_two_archives_names_both() {
    let (manifest, locked) = pinned(ONE, &[("a", &["fold-000000.tar", "fold-000001.tar"])]);
    let same: &[&str] = &["train/0000.txt"];
    let shards = [("fold-000000.tar", same), ("fold-000001.tar", same)];
    let error = placed_from(&manifest, &locked, &shards).unwrap_err();
    assert_eq!(
        error.message,
        "two files would land on data/a/train/0000.txt: one from fold-000000.tar and one from fold-000001.tar"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "leave one of them out with exclude in [datasets.a.members.\"fold-000000.tar\"] in data.toml, or set extract = false on datasets.a to keep its archives packed, or give each archive a dataset of its own, whose folder keeps its files apart"
        )
    );
}

#[test]
fn a_clash_with_an_archives_file_suggests_leaving_either_out_or_keeping_it_packed() {
    for (listed, members, help) in [
        (
            "notes.txt",
            vec![member("notes.txt")],
            "leave out the notes.txt beside x.zip with exclude = [\"notes.txt\"] in datasets.a, or set extract = false on datasets.a to keep its archives packed",
        ),
        (
            "t/notes.txt",
            vec![member("t/notes.txt"), member("t/more.txt")],
            "leave out the t/notes.txt beside x.zip with exclude = [\"t/notes.txt\"] in datasets.a, or the one inside it with exclude in [datasets.a.members.\"x.zip\"], or set extract = false on datasets.a to keep its archives packed",
        ),
    ] {
        let mut locked = lock(&[("a", &[listed, "x.zip"])]);
        locked.datasets[0].files[1].unpacks_to = Some(digest::sha256(b"tree"));
        let err = plan(&manifest(ONE), &locked, &|archive| {
            (archive.file.path == "x.zip").then(|| members.clone())
        })
        .unwrap_err();
        let clash = format!("two files would land on data/a/{listed}");
        assert!(err.message.contains(&clash), "{}", err.message);
        assert_eq!(err.help.as_deref(), Some(help));
    }
}

/// only a build makes a recipe's files, whatever source pin data.lock keeps under its name.
#[test]
fn a_recipe_places_nothing_from_an_old_source_pin() {
    let made = manifest(
        "[datasets.a]\nref = \"x\"\n\n[datasets.b]\nfrom = [\"a\"]\nrun = [\"t\", \"{a}\", \"{out}\"]\n",
    );
    let locked = lock(&[("a", &["x.csv"]), ("b", &["y.csv"])]);
    let paths: Vec<String> = targets(&made, &locked)
        .unwrap()
        .into_iter()
        .map(|target| target.path)
        .collect();
    assert_eq!(paths, ["data/a/x.csv"]);
}

#[test]
fn a_locked_dataset_missing_from_data_toml_places_nothing() {
    let only_a = manifest(ONE);
    assert_eq!(
        targets(&only_a, &lock(&[("a", &["x"]), ("gone", &["y"])]))
            .unwrap()
            .len(),
        1
    );
}

/// a dataset with `place = false`, fetched or made, has nothing downloaded, unpacked or placed,
/// and nothing of it clashes.
#[test]
fn an_unplaced_dataset_has_no_targets() {
    let (unplaced, mut locked) = pinned(
        "[datasets.raw]\nref = \"x\"\npath = \".\"\nplace = false\n\n[datasets.other]\nref = \"y\"\npath = \".\"\n\n[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\nplace = false\n",
        &[("raw", &["a.csv", "x.zip"]), ("other", &["a.csv"])],
    );
    let files = vec![member("c.csv")];
    locked.derived.push(Derived {
        name: "clean".to_owned(),
        from: Vec::new(),
        run: Vec::new(),
        code: Vec::new(),
        env: std::collections::BTreeMap::new(),
        key: digest::sha256(b"key"),
        timestamp: String::new(),
        tree: digest::sha256(b"tree"),
        files,
    });
    let planned = plan(&unplaced, &locked, &|_| None).unwrap();
    let paths: Vec<&str> = planned
        .targets
        .iter()
        .map(|target| target.path.as_str())
        .collect();
    assert_eq!(paths, ["data/a.csv"]);
    assert_eq!(planned.targets[0].dataset, "other");
    assert!(planned.unknown.is_empty(), "{:?}", planned.unknown);
}

#[test]
fn a_refused_archive_member_suggests_keeping_the_archive_packed() {
    let mut locked = lock(&[("a", &["data.zip"])]);
    locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
    let members = vec![member("safe.csv"), member(".git/config")];
    let mut planned = Plan::default();
    expand(
        &mut planned,
        "a",
        "",
        false,
        &locked.datasets[0].files,
        &Members::new(),
        &|_| Some(members.clone()),
    )
    .unwrap();
    for (patterns, expected) in [
        (Patterns::InDataToml, "extract = false"),
        (Patterns::OnCommandLine, "--no-extract"),
    ] {
        let error = check(&planned.targets, patterns).unwrap_err();
        assert!(error.help.unwrap().contains(expected));
    }
}

#[test]
fn metadata_targets_are_refused() {
    for path in [
        ".git/hooks/x",
        ".hg/store/x",
        ".svn/entries",
        ".fetchloom/state",
        "data/a/.GIT/hooks/x",
        "nested/.git. /hooks/x",
        "GIT~1/hooks/x",
        "nested/.git:stream/hooks/x",
    ] {
        let locked = lock(&[("a", &[path])]);
        let mut planned = Plan::default();
        expand(
            &mut planned,
            "a",
            "",
            false,
            &locked.datasets[0].files,
            &Members::new(),
            &|_| None,
        )
        .unwrap();
        for patterns in [Patterns::InDataToml, Patterns::OnCommandLine] {
            let result = check(&planned.targets, patterns);
            assert!(result.is_err(), "accepted metadata target: {path}");
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(error.message.contains(path), "{}", error.message);
            assert!(error.message.contains("metadata"), "{}", error.message);
            assert!(error.help.is_some());
        }
    }
}

/// the refusal comes before any write, with help to leave the file out; 255 bytes still fit.
#[test]
fn a_name_longer_than_the_255_bytes_a_disk_allows_is_refused() {
    for (name, fits) in [("f".repeat(255), true), ("f".repeat(256), false)] {
        let locked = lock(&[("a", &[name.as_str()])]);
        let mut planned = Plan::default();
        let filters = Members::new();
        expand(
            &mut planned,
            "a",
            "",
            false,
            &locked.datasets[0].files,
            &filters,
            &|_| None,
        )
        .unwrap();
        let result = check(&planned.targets, Patterns::InDataToml);
        if fits {
            assert!(result.is_ok(), "{} bytes: {result:?}", name.len());
            continue;
        }
        let Err(error) = result else {
            panic!("a name of {} bytes was taken", name.len());
        };
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            format!("{name} cannot be stored on this machine: its name is longer than disks allow")
        );
        assert_eq!(
            error.help.as_deref(),
            Some("leave it out with an exclude pattern in datasets.a in data.toml")
        );
    }
}

#[test]
fn metadata_archive_targets_are_refused() {
    let manifest = manifest(ONE);
    let mut locked = lock(&[("a", &["x.zip"])]);
    locked.datasets[0].files[0].unpacks_to = Some(digest::sha256(b"tree"));
    let members = vec![member(".git/hooks/x"), member("readme.txt")];
    let result = plan(&manifest, &locked, &|_| Some(members.clone()));
    assert!(result.is_err(), "accepted metadata from archive members");
    let error = result.unwrap_err();
    assert!(error.message.contains(".git/hooks/x"), "{}", error.message);
    assert!(error.message.contains("metadata"), "{}", error.message);
}

#[test]
fn clashes_are_refused_before_anything_is_written() {
    let one = manifest(ONE);
    assert!(
        targets(&one, &lock(&[("a", &["Readme.md", "README.md"])])).is_err(),
        "case-insensitive disks see one file"
    );
    assert!(
        targets(&one, &lock(&[("a", &["\u{3c2}.txt", "\u{3c3}.txt"])])).is_err(),
        "windows upcases both sigmas to one letter"
    );
    assert!(
        targets(&one, &lock(&[("a", &["caf\u{e9}.csv", "cafe\u{301}.csv"])])).is_err(),
        "two unicode spellings of one name count as one"
    );
    assert!(
        targets(&one, &lock(&[("a", &["x", "x/y"])])).is_err(),
        "a file where a folder must be"
    );
    assert_eq!(
        targets(&one, &lock(&[("a", &["2020-01-01T10:00:00.csv"])])).is_err(),
        cfg!(windows)
    );
}

/// `path` names a folder, so one ending in its file's name likely meant the file's place.
#[test]
fn a_path_named_like_its_file_is_found() {
    let flagged = |path: &str, files: &[&str]| {
        let text = format!("[datasets.train]\nref = \"x\"\npath = \"{path}\"\n");
        let manifest = manifest(&text);
        let locked = lock(&[("train", files)]);
        file_shaped_paths(&manifest, &plan(&manifest, &locked, &|_| None).unwrap())
    };
    assert_eq!(
        flagged("train.csv", &["train.csv", "README"]),
        [(
            "path = \"train.csv\" makes a folder named train.csv, so the file is data/train.csv/train.csv".to_owned(),
            "path names a folder; to put the file at data/train.csv, set path = \".\" on datasets.train in data.toml".to_owned()
        )]
    );
    assert_eq!(
        flagged("raw/Train.CSV", &["train.csv"]),
        [(
            "path = \"raw/Train.CSV\" makes a folder named Train.CSV, so the file is data/raw/Train.CSV/train.csv".to_owned(),
            "path names a folder; to put the file at data/raw/train.csv, set path = \"raw\" on datasets.train in data.toml".to_owned()
        )],
        "a disk that ignores case"
    );
    for (path, files) in [
        ("train.csv", &["sub/train.csv"][..]),
        ("splits", &["train.csv"][..]),
        (".", &["train.csv"][..]),
    ] {
        assert_eq!(flagged(path, files), [], "{path} with {files:?}");
    }
    let unnamed = manifest("[datasets.iris]\nref = \"x\"\n");
    let locked = lock(&[("iris", &["iris"])]);
    let planned = plan(&unnamed, &locked, &|_| None).unwrap();
    assert_eq!(file_shaped_paths(&unnamed, &planned), []);
}

#[test]
fn a_top_folder_is_shared_only_by_every_member() {
    let members = |paths: &[&str]| paths.iter().map(|path| member(path)).collect::<Vec<_>>();
    assert_eq!(
        common_top(&members(&["data/a.csv", "data/sub/b.csv"])),
        Some("data")
    );
    assert_eq!(common_top(&members(&["data/a.csv", "b.csv"])), None);
    assert_eq!(common_top(&members(&["data/a.csv", "data2/b.csv"])), None);
    assert_eq!(common_top(&members(&["a.csv"])), None);
    assert_eq!(common_top(&[]), None);
}

#[test]
fn caseless_sharp_s_paths_are_refused() {
    let locked = lock(&[("a", &["ß.csv", "ẞ.csv"])]);
    assert!(targets(&manifest(ONE), &locked).is_err());
}
