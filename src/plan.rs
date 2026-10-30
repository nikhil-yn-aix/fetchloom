//! where every locked file lands in the project, checked for clashes before anything is written.
//! names are compared the way a case-insensitive disk compares them, so a lock that places
//! cleanly here places cleanly on windows too.

use std::collections::{HashMap, HashSet};

use crate::archive;
use crate::cache::{Cache, Member};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::project::lock::{Lock, LockedFile};
use crate::project::manifest::Manifest;

/// one file a lock places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target<'a> {
    pub(crate) dataset: String,
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

/// the folder a dataset lands in, relative to the project.
pub(crate) fn folder(manifest: &Manifest, name: &str) -> String {
    let inner = manifest
        .datasets
        .get(name)
        .and_then(|entry| entry.path.as_deref())
        .unwrap_or(name);
    format!("{}/{inner}", manifest.data_dir)
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

/// every file the lock places for the datasets data.toml lists, or the first clash. an archive
/// is replaced by the files `members` says it unpacked to.
pub(crate) fn plan<'a>(
    manifest: &Manifest,
    lock: &'a Lock,
    members: &dyn Fn(&LockedFile) -> Option<Vec<Member>>,
) -> Result<Plan<'a>, Error> {
    let span = tracing::debug_span!("plan", files = tracing::field::Empty).entered();
    let mut plan = Plan::default();
    let mut folders = Vec::new();
    for dataset in &lock.datasets {
        let Some(entry) = manifest.datasets.get(&dataset.name) else {
            continue;
        };
        let folder = folder(manifest, &dataset.name);
        expand(
            &mut plan,
            &dataset.name,
            &folder,
            entry.copy,
            &dataset.files,
            members,
        );
        folders.push((folder, dataset.name.as_str()));
    }
    overlapping(&folders)?;
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
            dataset: dataset.to_owned(),
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

fn overlapping(folders: &[(String, &str)]) -> Result<(), Error> {
    for (index, (a, name_a)) in folders.iter().enumerate() {
        for (b, name_b) in &folders[index + 1..] {
            let (a_key, b_key) = (key(a), key(b));
            let inside = a_key == b_key
                || b_key.starts_with(&format!("{a_key}/"))
                || a_key.starts_with(&format!("{b_key}/"));
            if inside {
                return Err(Error::new(
                    Kind::ProjectInvalid,
                    format!(
                        "datasets {name_a} and {name_b} would share files: {a} and {b} overlap"
                    ),
                )
                .help("give one of them another path in data.toml"));
            }
        }
    }
    Ok(())
}

/// refuses targets that cannot all be stored: a name this machine cannot store, two files on one
/// path as a case-insensitive disk sees it, or a file where a folder must be.
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
            folders.entry(folder.to_owned()).or_insert(target);
            parent = folder;
        }
    }
    let file_keys: HashSet<&String> = files.keys().collect();
    for (folder, inside) in &folders {
        if file_keys.contains(folder) {
            let file = files[folder];
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "{} is a file, but {} needs a folder there",
                    file.path, inside.path
                ),
            )
            .help(fix(file, inside, patterns)));
        }
    }
    Ok(())
}

fn clash(first: &Target, second: &Target, patterns: Patterns) -> Error {
    let message = if first.path == second.path {
        format!("two files would land on {}", first.path)
    } else {
        format!(
            "{} and {} would be the same file on a disk that ignores case",
            first.path, second.path
        )
    };
    Error::new(Kind::ProjectInvalid, message).help(fix(first, second, patterns))
}

/// how to resolve a clash between `first` and `second`: keep an archive involved packed, or leave
/// a file out.
fn fix(first: &Target, second: &Target, patterns: Patterns) -> String {
    let unpacked = [first, second]
        .into_iter()
        .find(|target| target.locked.unpacks_to.is_some());
    match (unpacked, patterns) {
        (Some(target), Patterns::InDataToml) => format!(
            "set extract = false on datasets.{} in data.toml to keep its archives packed",
            target.dataset
        ),
        (Some(_), Patterns::OnCommandLine) => {
            "pass --no-extract to keep archives packed".to_owned()
        }
        (None, _) => leave_out("one of them", second, patterns),
    }
}

fn leave_out(what: &str, target: &Target, patterns: Patterns) -> String {
    match patterns {
        Patterns::InDataToml => format!(
            "leave {what} out with an exclude pattern in datasets.{}",
            target.dataset
        ),
        Patterns::OnCommandLine => format!("leave {what} out with --exclude"),
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
                    resolved: String::new(),
                    title: None,
                    license: None,
                    doi: None,
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
        let nested =
            manifest("[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"A/inner\"\n");
        assert!(
            targets(&nested, &lock(&[("a", &["x"]), ("b", &["y"])])).is_err(),
            "one dataset inside another"
        );
        assert_eq!(
            targets(&one, &lock(&[("a", &["2020-01-01T10:00:00.csv"])])).is_err(),
            cfg!(windows)
        );
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
}
