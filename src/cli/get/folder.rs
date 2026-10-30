//! the folder `get` fills: refused when it holds someone else's files, then each file put in by
//! reflink, hardlink or copy, and what happened said once.

use std::fs;
use std::io;
use std::path::Path;

use super::super::Context;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::place::{Method, Placer, Unlinked};
use crate::plan::Target;
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
    let tops: std::collections::HashSet<&str> = paths
        .iter()
        .map(|path| path.split('/').next().unwrap_or(path))
        .collect();
    let mut others: Vec<String> = fs::read_dir(folder)
        .map_err(|err| local(folder, &err))?
        .filter_map(Result::ok)
        .filter(|entry| {
            !entry.file_type().is_ok_and(|kind| kind.is_file())
                || !crate::place::is_copy_temporary(&entry.file_name().to_string_lossy())
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !tops.contains(name.as_str()))
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

/// puts every file into the folder, and says how.
pub(super) fn put_all(
    context: &Context,
    cache: &Cache,
    folder: &Path,
    targets: &[Target],
) -> Result<Vec<Outcome>, Error> {
    // an archive that holds nothing still leaves the folder get answers with
    fs::create_dir_all(folder).map_err(|err| local(folder, &err))?;
    let mut placer = Placer::in_named_folder(folder).with_threads(context.threads);
    let mut outcomes = Vec::with_capacity(targets.len());
    for target in targets {
        outcomes.push(put(
            &context.work,
            cache,
            &mut placer,
            target,
            &folder.join(&target.path),
        )?);
    }
    tell_placed(context, &placer, folder, targets, &outcomes);
    Ok(outcomes)
}

fn put(
    work: &Work,
    cache: &Cache,
    placer: &mut Placer,
    target: &Target,
    destination: &Path,
) -> Result<Outcome, Error> {
    let object = cache.find(&target.sha256, Some(target.size));
    let mut replaced = false;
    if fs::symlink_metadata(destination).is_ok() {
        if holds(destination, object.as_deref(), target)? {
            return Ok(Outcome::AlreadyThere);
        }
        if !replaceable(destination, cache)? {
            return Err(Error::new(
                Kind::LocalNotEmpty,
                format!(
                    "{} already exists with other contents",
                    destination.display()
                ),
            )
            .help("move it away to fetch this file, or choose another folder with -o"));
        }
        // get keeps no record of how it placed a file, so only the volume says how to remove it
        placer
            .remove(destination, false, None)
            .map_err(|err| local(destination, &err))?;
        replaced = true;
    }
    let object = object.ok_or_else(|| {
        Error::new(
            Kind::LocalIo,
            format!("the cache lost {} while placing it", target.path),
        )
        .help("run the same command again to download it")
    })?;
    let method = placer
        .place(&object, destination, false)
        .map_err(|err| local(destination, &err))?;
    let counter = match method {
        Method::Reflink => &work.reflinks,
        Method::Hardlink => &work.hardlinks,
        Method::Copy => &work.copies,
    };
    Work::add(counter, 1);
    Ok(if replaced {
        Outcome::Replaced(method)
    } else {
        Outcome::Placed(method)
    })
}

/// whether the file at `path` already holds the target's bytes. a link or a copy keeps the
/// object's size and modification time, so only a mismatch costs a hash.
fn holds(path: &Path, object: Option<&Path>, target: &Target) -> Result<bool, Error> {
    let there = fs::metadata(path).map_err(|err| local(path, &err))?;
    if !there.is_file() || there.len() != target.size {
        return Ok(false);
    }
    let same_time = object
        .and_then(|object| fs::metadata(object).ok())
        .and_then(|object| object.modified().ok())
        .is_some_and(|time| there.modified().is_ok_and(|there| there == time));
    if same_time {
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
                [only] => folder.join(&only.path).display().to_string(),
                _ => folder.display().to_string(),
            };
            output.done(&format!("{shown} is already up to date"));
            return;
        }
        ([only], [outcome]) => {
            let verb = if matches!(outcome, Outcome::Replaced(_)) {
                "Replaced"
            } else {
                "Placed"
            };
            output.step(
                verb,
                &format!("{} · {how}", folder.join(&only.path).display()),
            );
        }
        _ => output.step(
            "Placed",
            &format!(
                "{} · {} {how}",
                folder.display(),
                units::count(changed, "file", "files")
            ),
        ),
    }
    let things = if targets.len() == 1 {
        "the file was"
    } else {
        "files were"
    };
    let why = placer.unlinked().filter(|_| copies > 0);
    if let Some(why @ Unlinked::Synced(_)) = why {
        output.warn(&why.copied_because(folder.display(), things));
    }
    let what = match targets {
        [only] => only.path.clone(),
        _ => units::count(targets.len() as u64, "file", "files"),
    };
    output.done(&format!("fetched {what} into {}", folder.display()));
    if let Some(why @ (Unlinked::NoLinks | Unlinked::OtherDrive)) = why {
        output.hint(&why.copied_because(folder.display(), things));
    }
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
        (_, 0) => "linked, no extra disk".to_owned(),
        (0, _) => format!("copied, {}", units::size(bytes)),
        (_, _) => format!("partly copied, {}", units::size(bytes)),
    };
    (changed, copies, how)
}

fn local(path: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not write {}", path.display()), err)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                "linked, no extra disk",
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
}
