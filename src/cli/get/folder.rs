//! the folder `get` fills: refused when it holds someone else's files, then each file put in by
//! reflink, hardlink or copy, and what happened said once.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::Context;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::place::{Method, Order, Placer, Unlinked};
use crate::plan::Target;
use crate::project::state::Look;
use crate::units;
use crate::work::Work;

/// what happened to one file in the folder.
#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Placed(Method),
    /// an earlier version, which the cache still holds, was swapped for this one.
    Replaced(Method),
    AlreadyThere,
}

/// refuses a file where the folder should be, and a folder that holds anything but `paths`.
/// without `paths`, only the first is checked, since the names are not known yet.
pub(super) fn check(folder: &Path, paths: Option<&[String]>) -> Result<(), Error> {
    match fs::metadata(folder) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(local(folder, &err)),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(Error::new(
                Kind::LocalNotEmpty,
                format!("{} already exists and is not a folder", folder.display()),
            )
            .help("choose another folder with -o"));
        }
        Ok(_) => {}
    }
    let Some(paths) = paths else {
        return Ok(());
    };
    let tops: std::collections::HashSet<_> = paths
        .iter()
        .map(|path| crate::name::as_disk_sees(path.split('/').next().unwrap_or(path)))
        .collect();
    let mut others: Vec<String> = fs::read_dir(folder)
        .map_err(|err| local(folder, &err))?
        .filter_map(Result::ok)
        .filter(|entry| {
            !entry.file_type().is_ok_and(|kind| kind.is_file())
                || !crate::place::is_copy_temporary(&entry.file_name().to_string_lossy())
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !tops.contains(&crate::name::as_disk_sees(name)))
        .collect();
    if others.is_empty() {
        return Ok(());
    }
    others.sort();
    Err(Error::new(
        Kind::LocalNotEmpty,
        format!("{} already holds other files", folder.display()),
    )
    .detail(format!("such as {}", others[0]))
    .help("choose another folder with -o, or empty this one"))
}

/// a generated name cannot grant the root-link exception an explicit output folder gets.
pub(super) fn check_generated_root(folder: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(folder) {
        Ok(metadata) => crate::place::found_folder(
            folder,
            &metadata,
            crate::place::FolderUse::Place { make: false },
        )
        .map_err(|err| local(folder, &err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(local(folder, &err)),
    }
}

pub(super) fn put_all(
    context: &Context,
    cache: &Cache,
    folder: &Path,
    targets: &[Target],
    named: bool,
) -> Result<Vec<Outcome>, Error> {
    context.stop.check()?;
    let mut placer = if named {
        Placer::in_named_folder(folder)
    } else {
        Placer::new(folder)
    }
    .with_threads(context.threads)
    .with_stop(std::sync::Arc::clone(&context.stop));
    // an empty archive still leaves the output folder, bound before later path changes.
    placer
        .check_folders(&folder.join(".fetchloom-root"), true)
        .map_err(|error| local(folder, &error))?;
    let mut outcomes = Vec::with_capacity(targets.len());
    let mut orders = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        context.stop.check()?;
        let destination = folder.join(&target.path);
        match inspect(cache, &mut placer, target, &destination)? {
            Checked::AlreadyThere => outcomes.push(Outcome::AlreadyThere),
            Checked::Place { object, before } => {
                // told once placed below
                outcomes.push(Outcome::AlreadyThere);
                orders.push((index, object, destination, before));
            }
        }
    }
    for (_, _, destination, before) in &orders {
        context.stop.check()?;
        if let Some(before) = before {
            match fs::symlink_metadata(destination) {
                Ok(metadata)
                    if crate::volume::is_plain(&metadata) && Look::of(&metadata) == *before =>
                {
                    placer
                        .remove(destination, false, None)
                        .map_err(|error| local(destination, &error))?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(local(destination, &error)),
                Ok(_) => return Err(occupied(destination)),
            }
        }
    }
    // placed as sync places, so a copy flushed with others takes its name only after the flush
    let results = placer.place_all(
        orders.len(),
        |order| Order {
            object: orders[order].1.clone(),
            destination: orders[order].2.clone(),
            writable: false,
        },
        |_| (),
    );
    let mut failure = None;
    for ((index, _, destination, before), result) in orders.iter().zip(results) {
        let (method, ()) = match result {
            Ok(done) => done,
            Err(err) => {
                failure.get_or_insert_with(|| local(destination, &err));
                continue;
            }
        };
        let counter = match method {
            Method::Reflink => &context.work.reflinks,
            Method::Hardlink => &context.work.hardlinks,
            Method::Copy => &context.work.copies,
        };
        Work::add(counter, 1);
        if context.work.cached_before(&targets[*index].sha256) {
            Work::add(&context.work.placed_from_cache, 1);
        }
        outcomes[*index] = if before.is_some() {
            Outcome::Replaced(method)
        } else {
            Outcome::Placed(method)
        };
    }
    context.stop.check()?;
    if let Some(error) = failure {
        return Err(error);
    }
    placer
        .flush_copies()
        .map_err(|err| Error::io("could not flush the copied files to disk", &err))?;
    tell_placed(context, &placer, folder, targets, &outcomes);
    Ok(outcomes)
}

/// a read-only decision made before any destination is removed.
enum Checked {
    AlreadyThere,
    Place {
        object: PathBuf,
        before: Option<Look>,
    },
}

fn inspect(
    cache: &Cache,
    placer: &mut Placer,
    target: &Target,
    destination: &Path,
) -> Result<Checked, Error> {
    if let Err(error) = placer.check_folders(destination, false)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(local(destination, &error));
    }
    let object = cache.find(&target.sha256, Some(target.size));
    let before = match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if !crate::volume::is_plain(&metadata) {
                return Err(occupied(destination));
            }
            if holds(destination, object.as_deref(), target)? {
                return Ok(Checked::AlreadyThere);
            }
            if !replaceable(destination, cache)? {
                return Err(occupied(destination));
            }
            Some(Look::of(&metadata))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(local(destination, &error)),
    };
    let object = object.ok_or_else(|| {
        Error::new(
            Kind::LocalIo,
            format!("the cache lost {} while placing it", target.path),
        )
        .help("run the same command again to download it")
    })?;
    Ok(Checked::Place { object, before })
}

fn occupied(destination: &Path) -> Error {
    Error::new(
        Kind::LocalNotEmpty,
        format!(
            "{} already exists with other contents",
            destination.display()
        ),
    )
    .help("move it away to fetch this file, or choose another folder with -o")
}

/// whether the file at `path` already holds the target's bytes. a hardlink is the cached
/// object itself, so it needs no hash; a copy or a clone may carry the object's size and time
/// over other bytes, so only hashing it says.
fn holds(path: &Path, object: Option<&Path>, target: &Target) -> Result<bool, Error> {
    let there = fs::metadata(path).map_err(|err| local(path, &err))?;
    if !there.is_file() || there.len() != target.size {
        return Ok(false);
    }
    if object.is_some_and(|object| same_file::is_same_file(object, path).unwrap_or(false)) {
        return Ok(true);
    }
    Ok(Sha256::of_file(path).map_err(|err| local(path, &err))? == target.sha256)
}

/// whether the file at `path` may be replaced: only when the cache holds its bytes, so nothing
/// is ever lost.
fn replaceable(path: &Path, cache: &Cache) -> Result<bool, Error> {
    let size = fs::metadata(path).map_err(|err| local(path, &err))?.len();
    let digest = Sha256::of_file(path).map_err(|err| local(path, &err))?;
    Ok(cache.find(&digest, Some(size)).is_some())
}

fn tell_placed(
    context: &Context,
    placer: &Placer,
    folder: &Path,
    targets: &[Target],
    outcomes: &[Outcome],
) {
    let output = &context.output;
    let (changed, copies, how) = placement_summary(
        outcomes
            .iter()
            .copied()
            .zip(targets.iter().map(|target| target.size)),
    );
    match (targets, outcomes) {
        (_, _) if changed == 0 => {
            let shown = match targets {
                [only] => within(folder, &only.path).display().to_string(),
                _ => folder.display().to_string(),
            };
            output.done("Ok", &format!("{shown} is already up to date"));
            return;
        }
        ([only], [Outcome::Replaced(_)]) => output.step(
            "Replaced",
            &within(folder, &only.path).display().to_string(),
        ),
        _ => {}
    }
    let things = if targets.len() == 1 {
        "the file was"
    } else {
        "files were"
    };
    let why = placer.unlinked().filter(|_| copies > 0);
    if let Some(why @ Unlinked::Synced(_)) = why {
        let because = why.copied_because(folder.display(), things);
        match &because.help {
            Some(help) => output.warn_with(&because.said, help),
            None => output.warn(&because.said),
        }
    }
    let what = match targets {
        [only] => only.path.clone(),
        _ => units::count(targets.len() as u64, "file", "files"),
    };
    output.done(
        "Fetched",
        &format!("{what} into {} · {how}", folder.display()),
    );
    if targets.len() > 1 {
        let files = targets
            .iter()
            .map(|target| (target.path.as_str(), target.size));
        for line in top_level(files) {
            output.item(&line);
        }
    }
    if let Some(why @ (Unlinked::NoLinks | Unlinked::OtherDrive)) = why {
        output.hint(&why.copied_because(folder.display(), things).to_string());
    }
}

/// the top level of a filled folder, largest first, as a person looking for the main file
/// wants it: up to ten names with their sizes, a folder with how many files it holds.
fn top_level<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> Vec<String> {
    const SHOWN: usize = 10;
    // a name's files, bytes, and whether it is a folder
    let mut entries: std::collections::BTreeMap<&str, (u64, u64, bool)> =
        std::collections::BTreeMap::new();
    for (path, size) in files {
        let (name, folder) = path
            .split_once('/')
            .map_or((path, false), |(head, _)| (head, true));
        let entry = entries.entry(name).or_insert((0, 0, folder));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(size);
        entry.2 |= folder;
    }
    let mut entries: Vec<(&str, (u64, u64, bool))> = entries.into_iter().collect();
    entries
        .sort_by(|(a, (_, a_bytes, _)), (b, (_, b_bytes, _))| b_bytes.cmp(a_bytes).then(a.cmp(b)));
    let shown: Vec<(String, String)> = entries
        .iter()
        .take(SHOWN)
        .map(|(name, (files, bytes, folder))| {
            if *folder {
                (
                    format!("{name}/"),
                    format!(
                        "{} · {}",
                        units::count(*files, "file", "files"),
                        units::size(*bytes)
                    ),
                )
            } else {
                ((*name).to_owned(), units::size(*bytes))
            }
        })
        .collect();
    let width = shown
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = shown
        .into_iter()
        .map(|(name, size)| format!("{name:<width$}  {size}"))
        .collect();
    if entries.len() > SHOWN {
        lines.push(format!("and {} more", entries.len() - SHOWN));
    }
    lines
}

fn placement_summary(files: impl Iterator<Item = (Outcome, u64)>) -> (u64, u64, String) {
    let (mut changed, mut copies, mut bytes) = (0, 0, 0);
    for (outcome, size) in files {
        if matches!(outcome, Outcome::AlreadyThere) {
            continue;
        }
        changed += 1;
        if matches!(
            outcome,
            Outcome::Placed(Method::Copy) | Outcome::Replaced(Method::Copy)
        ) {
            copies += 1;
            bytes += size;
        }
    }
    let how = match (changed - copies, copies) {
        (_, 0) => "linked to the cache".to_owned(),
        (0, _) => format!("copied, {}", units::size(bytes)),
        (_, _) => format!("partly copied, {}", units::size(bytes)),
    };
    (changed, copies, how)
}

fn local(path: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not write {}", path.display()), err)
}

/// `path`, a dataset path with `/` between its parts, inside `folder` with the system's own
/// separators, as people see paths on windows.
fn within(folder: &Path, path: &str) -> std::path::PathBuf {
    path.split('/')
        .fold(folder.to_path_buf(), |inside, part| inside.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filled_folder_lists_its_top_level_largest_first() {
        let files = [
            (".gitattributes", 2307),
            ("README.md", 913),
            ("data/red-00000-of-00001.parquet", 30886),
            ("data/white-00000-of-00001.parquet", 73265),
            ("winequality-red.csv", 84200),
            ("winequality-white.csv", 264_427),
            ("winequality.names", 3234),
        ];
        assert_eq!(
            top_level(files),
            [
                "winequality-white.csv  258.2 KiB",
                "data/                  2 files · 101.7 KiB",
                "winequality-red.csv    82.2 KiB",
                "winequality.names      3.2 KiB",
                ".gitattributes         2.3 KiB",
                "README.md              913 B",
            ]
        );
    }

    #[test]
    fn a_long_top_level_shows_ten_and_counts_the_rest() {
        let names: Vec<String> = (0..13).map(|n| format!("f{n:02}.csv")).collect();
        let lines = top_level(
            names
                .iter()
                .zip(100..)
                .map(|(name, size)| (name.as_str(), size)),
        );
        assert_eq!(lines.len(), 11, "{lines:?}");
        assert_eq!(lines[0], "f12.csv  112 B");
        assert_eq!(lines[9], "f03.csv  103 B");
        assert_eq!(lines[10], "and 3 more");
    }

    #[test]
    fn mixed_placements_count_links_and_copies() {
        use Outcome::{AlreadyThere, Placed, Replaced};
        for (files, counts, expected) in [
            (
                &[(Placed(Method::Copy), 0), (Placed(Method::Hardlink), 5)][..],
                (2, 1),
                "partly copied, 0 B",
            ),
            (
                &[
                    (Replaced(Method::Copy), 5),
                    (Placed(Method::Reflink), 7),
                    (AlreadyThere, 9),
                ],
                (2, 1),
                "partly copied, 5 B",
            ),
            (
                &[(Placed(Method::Copy), 0), (AlreadyThere, 7)],
                (1, 1),
                "copied, 0 B",
            ),
            (
                &[(Placed(Method::Copy), 3), (Replaced(Method::Copy), 5)],
                (2, 2),
                "copied, 8 B",
            ),
            (
                &[
                    (Placed(Method::Hardlink), 0),
                    (Replaced(Method::Reflink), 5),
                ],
                (2, 0),
                "linked to the cache",
            ),
        ] {
            let (changed, copies, how) = placement_summary(files.iter().copied());
            assert_eq!((changed, copies), counts);
            assert_eq!(how, expected);
        }
    }

    #[test]
    fn an_interrupted_copy_does_not_block_a_later_get() {
        let folder = tempfile::tempdir().unwrap();
        let leftover = folder.path().join(".fetchloom-0123456789abcdef.copy");
        fs::write(&leftover, b"a partial copy").unwrap();
        let paths = ["iris.csv".to_owned()];

        assert!(
            check(folder.path(), Some(&paths)).is_ok(),
            "get must ignore its unfinished copies"
        );
        assert_eq!(fs::read(&leftover).unwrap(), b"a partial copy");
        fs::write(folder.path().join(".fetchloom-not-a-copy.copy"), b"keep").unwrap();
        assert_eq!(
            check(folder.path(), Some(&paths)).unwrap_err().kind,
            Kind::LocalNotEmpty
        );
    }

    #[test]
    fn every_file_is_placed_whole_and_told_in_order() {
        use crate::cache::tests::store;
        use crate::output::{Level, Output, Style};
        use crate::project::lock::LockedFile;

        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let folder = dir.path().join("got");
        let contents: [&[u8]; 5] = [b"one", b"two", b"old", b"new", b"kept"];
        for bytes in contents {
            store(&cache, bytes);
        }
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("replaced.csv"), b"old").unwrap();
        fs::write(folder.join("there.csv"), b"kept").unwrap();
        let wanted = [
            ("one.csv", &b"one"[..]),
            ("deep/two.csv", b"two"),
            ("replaced.csv", b"new"),
            ("there.csv", b"kept"),
        ];
        let locked: Vec<LockedFile> = wanted
            .iter()
            .map(|(path, bytes)| LockedFile {
                path: (*path).to_owned(),
                size: bytes.len() as u64,
                sha256: crate::digest::sha256(bytes),
                md5: None,
                url: format!("https://example.org/{path}"),
                unpacks_to: None,
            })
            .collect();
        let targets: Vec<Target> = locked
            .iter()
            .map(|file| Target {
                dataset: "got",
                path: file.path.clone(),
                sha256: file.sha256,
                size: file.size,
                copy: false,
                locked: file,
            })
            .collect();
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
        };
        let context = Context::new(Output::new(style, Level::Quiet, false, false));

        let outcomes = put_all(&context, &cache, &folder, &targets, true).unwrap();

        assert!(matches!(
            outcomes.as_slice(),
            [
                Outcome::Placed(_),
                Outcome::Placed(_),
                Outcome::Replaced(_),
                Outcome::AlreadyThere
            ]
        ));
        for (path, bytes) in wanted {
            assert_eq!(fs::read(within(&folder, path)).unwrap(), bytes, "{path}");
        }
        let counts = context.work.counts();
        assert_eq!(counts.reflinks + counts.hardlinks + counts.copies, 3);
        for inside in [folder.clone(), folder.join("deep")] {
            for entry in fs::read_dir(&inside).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(!crate::place::is_copy_temporary(&name), "{name}");
            }
        }
    }

    #[test]
    fn preflight_preserves_old_files_when_a_later_object_is_missing() {
        use crate::cache::tests::store;
        use crate::output::{Level, Output, Style};
        use crate::project::lock::LockedFile;
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        store(&cache, b"old");
        let first = store(&cache, b"new");
        let folder = dir.path().join("got");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a"), b"old").unwrap();
        let locked: Vec<_> = [("a", first.digest), ("b", crate::digest::sha256(b"lost"))]
            .into_iter()
            .map(|(path, sha256)| LockedFile {
                path: path.into(),
                sha256,
                size: if path == "a" { 3 } else { 4 },
                md5: None,
                url: format!("https://example.org/{path}"),
                unpacks_to: None,
            })
            .collect();
        let targets: Vec<_> = locked
            .iter()
            .map(|file| Target {
                dataset: "got",
                path: file.path.clone(),
                sha256: file.sha256,
                size: file.size,
                copy: false,
                locked: file,
            })
            .collect();
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        let result = put_all(&context, &cache, &folder, &targets, true);
        assert!(matches!(result, Err(error) if error.kind == Kind::LocalIo));
        assert_eq!(fs::read(folder.join("a")).unwrap(), b"old");
        assert!(!folder.join("b").exists());
    }

    #[test]
    fn a_write_the_disk_refuses_says_what_to_do() {
        let full = local(
            Path::new("iris/iris.csv"),
            &io::ErrorKind::StorageFull.into(),
        );
        assert_eq!(
            full.help.as_deref(),
            Some("free some space on that disk, then run the command again")
        );
    }

    #[test]
    fn a_stop_asked_before_placement_places_no_files() {
        use crate::cache::tests::store;
        use crate::output::{Level, Output, Style};
        use crate::project::lock::LockedFile;

        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = store(&cache, b"pinned");
        let locked = LockedFile {
            path: "file".to_owned(),
            size: 6,
            sha256: stored.digest,
            md5: None,
            url: "https://example.org/file".to_owned(),
            unpacks_to: None,
        };
        let targets: Vec<_> = (0..512)
            .map(|index| Target {
                dataset: "set",
                path: format!("{index}.csv"),
                sha256: stored.digest,
                size: 6,
                copy: false,
                locked: &locked,
            })
            .collect();
        let context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.stop.ask();
        let result = put_all(&context, &cache, &dir.path().join("got"), &targets, true);
        let counts = context.work.counts();
        assert_eq!(counts.hardlinks + counts.reflinks + counts.copies, 0);
        assert!(matches!(result, Err(error) if error.kind == Kind::Interrupted));
    }
    #[cfg(unix)]
    #[test]
    fn generated_roots_cannot_follow_a_link_at_placement_time() {
        use crate::output::{Level, Output, Style};
        use crate::project::lock::LockedFile;
        for named in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let cache = Cache::open(dir.path().join("cache")).unwrap();
            let stored = crate::cache::tests::store(&cache, b"pinned");
            let outside = dir.path().join("outside");
            fs::create_dir(&outside).unwrap();
            let folder = dir.path().join("result");
            std::os::unix::fs::symlink(&outside, &folder).unwrap();
            let locked = LockedFile {
                path: "file".into(),
                size: stored.size,
                sha256: stored.digest,
                md5: None,
                url: "https://example.org/file".into(),
                unpacks_to: None,
            };
            let targets = [Target {
                dataset: "set",
                path: "file".into(),
                size: stored.size,
                sha256: stored.digest,
                copy: false,
                locked: &locked,
            }];
            let context = Context::new(Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                },
                Level::Quiet,
                false,
                false,
            ));
            let result = put_all(&context, &cache, &folder, &targets, named);
            assert_eq!(result.is_ok(), named);
            assert_eq!(outside.join("file").exists(), named);
        }
    }
}
