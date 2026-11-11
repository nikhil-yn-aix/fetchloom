//! where every locked file lands in the project, checked for clashes before anything is written.
//! names are compared the way a case-insensitive disk compares them, so a lock that places
//! cleanly here places cleanly on windows too.

use std::collections::{HashMap, HashSet};

use crate::archive;
use crate::cache::{Cache, Member};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::project::lock::{Lock, Locked, LockedFile};
use crate::project::manifest::{self, Manifest};

/// one file a lock places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target<'a> {
    pub(crate) dataset: &'a str,
    /// the path in the project, with `/` between folders.
    pub(crate) path: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
    /// a writable copy, for tools that write into their inputs.
    pub(crate) copy: bool,
    /// the locked file whose download holds these bytes: the file itself, or the archive it
    /// unpacks from.
    pub(crate) locked: &'a LockedFile,
}

/// every file a lock places, and the archives whose files this machine does not know yet.
#[derive(Debug, Default)]
pub(crate) struct Plan<'a> {
    pub(crate) targets: Vec<Target<'a>>,
    /// archives never unpacked on this machine, with their dataset, so the files they hold
    /// cannot be planned until they are.
    pub(crate) unknown: Vec<(&'a str, &'a LockedFile)>,
}

/// the folder a dataset lands in, relative to the project: the data folder itself for
/// `path = "."`.
pub(crate) fn folder(manifest: &Manifest, name: &str) -> String {
    match manifest
        .datasets
        .get(name)
        .and_then(|entry| entry.path.as_deref())
    {
        Some(manifest::DATA_FOLDER_ITSELF) => manifest.data_dir.clone(),
        inner => format!("{}/{}", manifest.data_dir, inner.unwrap_or(name)),
    }
}

/// the plan for `lock`, with each archive standing for the files the cache records it unpacked
/// to.
pub(crate) fn from_cache<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    cache: &Cache,
) -> Result<Plan<'a>, Error> {
    plan(manifest, lock, &|file| {
        cache.unpacked(&file.sha256, file.unpacks_to.as_ref())
    })
}

/// refuses, before anything is downloaded, a clash among the files of `datasets` whose place is
/// already known, as the cache records what each archive unpacked to. an archive with no record
/// is planned once it is unpacked, and checked then, before anything is placed.
pub(crate) fn foresee<'a>(
    manifest: &Manifest,
    datasets: impl IntoIterator<Item = &'a Locked>,
    cache: &Cache,
) -> Result<(), Error> {
    // in data.lock's order, so a clash names its datasets as the later check does
    let mut datasets: Vec<&Locked> = datasets.into_iter().collect();
    datasets.sort_by(|a, b| a.name.cmp(&b.name));
    planned(manifest, datasets, &|file| {
        cache.unpacked(&file.sha256, file.unpacks_to.as_ref())
    })
    .map(drop)
}

/// every file the lock places for the datasets data.toml lists, or the first clash. an archive
/// is replaced by the files `members` says it unpacked to. datasets may share a folder, but no
/// two files may share a path.
pub(crate) fn plan<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    members: &dyn Fn(&LockedFile) -> Option<Vec<Member>>,
) -> Result<Plan<'a>, Error> {
    planned(manifest, &lock.datasets, members)
}

fn planned<'a>(
    manifest: &Manifest,
    datasets: impl IntoIterator<Item = &'a Locked>,
    members: &dyn Fn(&LockedFile) -> Option<Vec<Member>>,
) -> Result<Plan<'a>, Error> {
    let span = tracing::debug_span!("plan", files = tracing::field::Empty).entered();
    let mut plan = Plan::default();
    for dataset in datasets {
        let Some(entry) = manifest.datasets.get(&dataset.name) else {
            continue;
        };
        expand(
            &mut plan,
            &dataset.name,
            &folder(manifest, &dataset.name),
            entry.copy,
            &dataset.files,
            members,
        );
    }
    check(&plan.targets, Patterns::InDataToml)?;
    span.record("files", plan.targets.len());
    Ok(plan)
}

/// adds where one dataset's files land in `folder`, an archive standing for the files `members`
/// says it unpacked to.
pub(crate) fn expand<'a>(
    plan: &mut Plan<'a>,
    dataset: &'a str,
    folder: &str,
    copy: bool,
    files: &'a [LockedFile],
    members: &dyn Fn(&LockedFile) -> Option<Vec<Member>>,
) {
    let alone = files.len() == 1;
    for file in files {
        let target = |path: &str, sha256, size| Target {
            dataset,
            path: join(folder, path),
            sha256,
            size,
            copy,
            locked: file,
        };
        if file.unpacks_to.is_none() {
            plan.targets
                .push(target(&file.path, file.sha256, file.size));
            continue;
        }
        let Some(members) = members(file) else {
            plan.unknown.push((dataset, file));
            continue;
        };
        for (member, path) in members
            .iter()
            .zip(unpacked_paths(&file.path, alone, &members))
        {
            plan.targets.push(target(&path, member.sha256, member.size));
        }
    }
}

/// where the patterns that decide a dataset's files are set, so a clash says where to change
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Patterns {
    InDataToml,
    OnCommandLine,
}

/// where an archive's files land inside its dataset's folder, in the order of `members`. a
/// dataset that is one archive gets the archive's files straight in its folder, without the one
/// top folder they may all share, as nix's fetchzip does. an archive beside other files unpacks
/// where it sits: a lone top folder or file comes out as it is, as `unzip` would make it, and
/// anything else goes into a folder named after the archive.
pub(crate) fn unpacked_paths(archive: &str, alone: bool, members: &[Member]) -> Vec<String> {
    let (beside, name) = archive.rsplit_once('/').unwrap_or(("", archive));
    let top = common_top(members);
    if alone {
        return match top {
            Some(top) => place_under(beside, members, |path| &path[top.len() + 1..]),
            None => place_under(beside, members, |path| path),
        };
    }
    let lone = top.is_some() || matches!(members, [only] if !only.path.contains('/'));
    let inside = if lone {
        beside.to_owned()
    } else {
        join(beside, archive::stem(name))
    };
    place_under(&inside, members, |path| path)
}

fn place_under(folder: &str, members: &[Member], rest: impl Fn(&str) -> &str) -> Vec<String> {
    members
        .iter()
        .map(|member| join(folder, rest(&member.path)))
        .collect()
}

fn join(folder: &str, path: &str) -> String {
    if folder.is_empty() {
        path.to_owned()
    } else {
        format!("{folder}/{path}")
    }
}

/// the one top folder every member sits under, when there is one.
fn common_top(members: &[Member]) -> Option<&str> {
    let (top, _) = members.first()?.path.split_once('/')?;
    members
        .iter()
        .all(|member| {
            member
                .path
                .strip_prefix(top)
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .then_some(top)
}

/// how a case-insensitive disk sees a name.
fn key(path: &str) -> String {
    name::fold(path)
}

/// a warning and its help for each dataset whose `path` ends in the name of a file placed
/// straight inside it, as `path = "train.csv"` puts train.csv at data/train.csv/train.csv: a
/// person who wrote that most likely meant the file's own place, since `path` names a folder.
pub(crate) fn file_shaped_paths(manifest: &Manifest, plan: &Plan) -> Vec<(String, String)> {
    let mut warnings = Vec::new();
    for (dataset, entry) in &manifest.datasets {
        let Some(path) = entry
            .path
            .as_deref()
            .filter(|path| *path != manifest::DATA_FOLDER_ITSELF)
        else {
            continue;
        };
        let (parent, name) = path
            .rsplit_once('/')
            .unwrap_or((manifest::DATA_FOLDER_ITSELF, path));
        let inside = format!("{}/", folder(manifest, dataset));
        let named = key(name);
        let Some(file) = plan.targets.iter().find_map(|target| {
            let file = target.path.strip_prefix(&inside)?;
            (target.dataset == dataset && !file.contains('/') && key(file) == named).then_some(file)
        }) else {
            continue;
        };
        let meant = match parent {
            manifest::DATA_FOLDER_ITSELF => format!("{}/{file}", manifest.data_dir),
            parent => format!("{}/{parent}/{file}", manifest.data_dir),
        };
        warnings.push((
            format!("path = \"{path}\" makes a folder named {name}, so the file is {inside}{file}"),
            format!(
                "path names a folder; to put the file at {meant}, set path = \"{parent}\" on datasets.{dataset} in data.toml"
            ),
        ));
    }
    warnings
}

/// refuses targets that cannot all be stored: a name this machine cannot store, two files on one
/// path as a case-insensitive disk sees it, or a file where a folder must be. datasets may share
/// folders, so targets of two datasets clash only where their files do.
pub(crate) fn check(targets: &[Target], patterns: Patterns) -> Result<(), Error> {
    let mut files: HashMap<String, &Target> = HashMap::with_capacity(targets.len());
    let mut folders: HashMap<String, &Target> = HashMap::new();
    for target in targets {
        if let Some(reason) = name::metadata(&target.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("{} is refused because {reason}", target.path),
            )
            .help(leave_out("it", target, patterns)));
        }
        if let Some(reason) = name::unstorable(&target.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("{} cannot be stored on this machine: {reason}", target.path),
            )
            .help(leave_out("it", target, patterns)));
        }
        let path_key = key(&target.path);
        if let Some(other) = files.insert(path_key.clone(), target) {
            return Err(clash(other, target, patterns));
        }
        let mut parent = path_key.as_str();
        while let Some((folder, _)) = parent.rsplit_once('/') {
            // a folder seen before had its own parents recorded with it
            if folders.contains_key(folder) {
                break;
            }
            folders.insert(folder.to_owned(), target);
            parent = folder;
        }
    }
    let file_keys: HashSet<&String> = files.keys().collect();
    for (folder, inside) in &folders {
        if file_keys.contains(folder) {
            let file = files[folder];
            let message = if file.dataset == inside.dataset {
                format!(
                    "{} is a file, but {} needs a folder there",
                    file.path, inside.path
                )
            } else {
                format!(
                    "dataset {} places a file at {}, but dataset {} needs a folder there for {}",
                    file.dataset, file.path, inside.dataset, inside.path
                )
            };
            return Err(Error::new(Kind::ProjectInvalid, message).help(fix(file, inside, patterns)));
        }
    }
    Ok(())
}

fn clash(first: &Target, second: &Target, patterns: Patterns) -> Error {
    let datasets = format!("datasets {} and {}", first.dataset, second.dataset);
    let message = match (first.path == second.path, first.dataset == second.dataset) {
        (true, true) => format!("two files would land on {}", first.path),
        (true, false) => format!("{datasets} would both place {}", first.path),
        (false, true) => format!(
            "{} and {} would be the same file on a disk that ignores case",
            first.path, second.path
        ),
        (false, false) => format!(
            "{datasets} would place {} and {}, the same file on a disk that ignores case",
            first.path, second.path
        ),
    };
    Error::new(Kind::ProjectInvalid, message).help(fix(first, second, patterns))
}

/// how to resolve a clash between `first` and `second`: give one of two datasets a folder of its
/// own, or within one dataset keep an archive involved packed, or leave a file out.
fn fix(first: &Target, second: &Target, patterns: Patterns) -> String {
    if first.dataset != second.dataset {
        return format!(
            "give datasets.{} or datasets.{} another path in data.toml",
            first.dataset, second.dataset
        );
    }
    let unpacked = [first, second]
        .into_iter()
        .find(|target| target.locked.unpacks_to.is_some());
    leave_out("one of them", unpacked.unwrap_or(second), patterns)
}

fn leave_out(what: &str, target: &Target, patterns: Patterns) -> String {
    match (target.locked.unpacks_to.is_some(), patterns) {
        (true, Patterns::InDataToml) => format!(
            "set extract = false on datasets.{} in data.toml to keep its archives packed",
            target.dataset
        ),
        (true, Patterns::OnCommandLine) => "pass --no-extract to keep archives packed".to_owned(),
        (false, Patterns::InDataToml) => format!(
            "leave {what} out with an exclude pattern in datasets.{} in data.toml",
            target.dataset
        ),
        (false, Patterns::OnCommandLine) => format!("leave {what} out with --exclude"),
    }
}

#[cfg(test)]
mod tests {
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

    const TWO: &str = "[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"elsewhere/b\"\ncopy = true\n";
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
        let planned = plan(&manifest(ONE), &locked, &|file| {
            (file.path == archive).then(|| members.clone())
        })
        .unwrap();
        let mut paths: Vec<String> = planned
            .targets
            .into_iter()
            .filter(|target| target.locked.path == archive)
            .map(|target| target.path)
            .collect();
        paths.sort();
        paths
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

    #[test]
    fn a_clash_with_an_archives_file_suggests_keeping_it_packed() {
        let mut locked = lock(&[("a", &["notes.txt", "x.zip"])]);
        locked.datasets[0].files[1].unpacks_to = Some(digest::sha256(b"tree"));
        let members = vec![member("notes.txt")];
        let err = plan(&manifest(ONE), &locked, &|file| {
            (file.path == "x.zip").then(|| members.clone())
        })
        .unwrap_err();
        assert!(
            err.message
                .contains("two files would land on data/a/notes.txt"),
            "{}",
            err.message
        );
        assert!(err.help.unwrap_or_default().contains("extract = false"));
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
            &|_| Some(members.clone()),
        );
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
                &|_| None,
            );
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

    /// `path` names a folder, so one that ends in the name of a file placed straight inside it
    /// most likely meant that file's own place.
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
}
