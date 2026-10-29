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
    let mut placer = Placer::in_named_folder(folder);
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
    let copied: u64 = outcomes
        .iter()
        .zip(targets)
        .filter(|(outcome, _)| {
            matches!(
                outcome,
                Outcome::Placed(Method::Copy) | Outcome::Replaced(Method::Copy)
            )
        })
        .map(|(_, target)| target.size)
        .sum();
    let how = if copied > 0 {
        format!("copied, {}", units::size(copied))
    } else {
        "linked, no extra disk".to_owned()
    };
    let changed = outcomes
        .iter()
        .filter(|outcome| !matches!(outcome, Outcome::AlreadyThere))
        .count();
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
                units::count(changed as u64, "file", "files")
            ),
        ),
    }
    let things = if targets.len() == 1 {
        "the file was"
    } else {
        "files were"
    };
    let why = placer.unlinked().filter(|_| copied > 0);
    if let Some(Unlinked::Synced(client)) = why {
        output.warn(&format!(
            "{} is inside {client}, which breaks links, so {things} copied",
            folder.display()
        ));
    }
    let what = match targets {
        [only] => only.path.clone(),
        _ => units::count(targets.len() as u64, "file", "files"),
    };
    output.done(&format!("fetched {what} into {}", folder.display()));
    if let Some(why) = why {
        output.hint(&copy_hint(why, folder, things));
    }
}

/// the hint after files were copied into `folder` for the reason `why`.
fn copy_hint(why: Unlinked, folder: &Path, things: &str) -> String {
    match why {
        Unlinked::Synced(client) => {
            format!("a folder outside {client} gets links that use no extra disk")
        }
        Unlinked::OtherDrive => "the cache is on another drive. set FETCHLOOM_CACHE to a folder on this drive to link files instead of copying them".to_owned(),
        Unlinked::NoLinks => format!(
            "the drive holding {} cannot link files, so {things} copied",
            folder.display()
        ),
    }
}

fn local(path: &Path, err: &io::Error) -> Error {
    Error::io(format_args!("could not write {}", path.display()), err)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_file_copied_onto_a_drive_that_links_nothing_says_why() {
        assert_eq!(
            copy_hint(Unlinked::NoLinks, Path::new("iris"), "the file was"),
            "the drive holding iris cannot link files, so the file was copied"
        );
    }
}
