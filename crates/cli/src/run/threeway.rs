//! Applying a three way compare to a staging directory holding upstream's tree.

use super::context::Materialization;
use super::dataset::with_ancestor_directories;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::merge::{Merged, Resolution};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::tree::{EntryPath, TreeEntry};
use std::collections::HashMap;
use std::path::Path;

pub(crate) const ASIDE: &str = ".upstream";

#[derive(Clone, Copy, Debug)]
pub(crate) struct Sides<'a> {
    pub(crate) yours: &'a [TreeEntry],
    pub(crate) upstream: &'a [TreeEntry],
}

#[derive(Debug)]
pub(crate) struct Applied {
    pub(crate) entries: Vec<TreeEntry>,
    pub(crate) conflicts: Vec<String>,
}

fn named(entries: &[TreeEntry]) -> HashMap<&str, &TreeEntry> {
    entries
        .iter()
        .map(|entry| (entry.path().as_str(), entry))
        .collect()
}

fn moved_aside(entry: &TreeEntry, to: &EntryPath) -> TreeEntry {
    match entry {
        TreeEntry::File {
            mode,
            size,
            content,
            ..
        } => TreeEntry::File {
            path: to.clone(),
            mode: *mode,
            size: *size,
            content: *content,
        },
        TreeEntry::Symlink { size, content, .. } => TreeEntry::Symlink {
            path: to.clone(),
            size: *size,
            content: *content,
        },
        TreeEntry::Directory { .. } => TreeEntry::Directory { path: to.clone() },
    }
}

fn aside_path(path: &str) -> Result<EntryPath, Error> {
    EntryPath::new(&format!("{path}{ASIDE}")).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("rename {path}, because {reason}"),
        )
    })
}

#[derive(Debug, Default)]
struct Decided<'a> {
    conflicts: Vec<String>,
    renames: Vec<(String, String)>,
    kept: Vec<TreeEntry>,
    from_yours: Vec<&'a TreeEntry>,
    surviving: std::collections::HashSet<String>,
}

fn decide<'a>(sides: Sides<'a>, decisions: &[Merged]) -> Result<Decided<'a>, Error> {
    let yours = named(sides.yours);
    let upstream = named(sides.upstream);
    let Decided {
        mut conflicts,
        mut renames,
        mut kept,
        mut from_yours,
        mut surviving,
    } = Decided::default();

    for decision in decisions {
        let path = decision.path.as_str();
        match decision.resolution {
            Resolution::Unchanged | Resolution::TakeUpstream => {
                if let Some(entry) = upstream.get(path) {
                    kept.push((*entry).clone());
                    surviving.insert(path.to_owned());
                }
            }
            Resolution::StaysDeleted => {}
            Resolution::KeepYours => {
                if let Some(entry) = yours.get(path) {
                    kept.push((*entry).clone());
                    from_yours.push(entry);
                    surviving.insert(path.to_owned());
                }
            }
            Resolution::Conflict => {
                conflicts.push(path.to_owned());
                if let Some(entry) = upstream.get(path) {
                    let aside = aside_path(path)?;
                    if yours.contains_key(aside.as_str()) || upstream.contains_key(aside.as_str()) {
                        return Err(Error::new(
                            ErrorKind::DestinationConflict,
                            format!(
                                "move {} out of the way and run this again, because upstream's \
                                 {path} cannot be written beside yours while something else \
                                 already holds that name",
                                aside.as_str()
                            ),
                        ));
                    }
                    renames.push((path.to_owned(), aside.as_str().to_owned()));
                    kept.push(moved_aside(entry, &aside));
                    surviving.insert(aside.as_str().to_owned());
                }
                if let Some(entry) = yours.get(path) {
                    kept.push((*entry).clone());
                    from_yours.push(entry);
                    surviving.insert(path.to_owned());
                }
            }
        }
    }

    Ok(Decided {
        conflicts,
        renames,
        kept,
        from_yours,
        surviving,
    })
}

pub(crate) fn apply(
    with: &Materialization<'_>,
    destination: &Path,
    staging: &Path,
    sides: Sides<'_>,
    decisions: &[Merged],
) -> Result<Applied, Error> {
    let Decided {
        mut conflicts,
        renames,
        kept,
        from_yours,
        mut surviving,
    } = decide(sides, decisions)?;

    let entries = with_ancestor_directories(kept)?;
    for entry in &entries {
        surviving.insert(entry.path().as_str().to_owned());
    }

    for (from, to) in &renames {
        let at = staging.join(from);
        if at.exists() {
            std::fs::rename(&at, staging.join(to))
                .map_err(|reason| filesystem_failure(Surface::Destination, &at, &reason))?;
        }
    }

    let mut stale: Vec<&str> = sides
        .upstream
        .iter()
        .map(|entry| entry.path().as_str())
        .filter(|path| !surviving.contains(*path))
        .collect();
    stale.sort_unstable();
    for path in stale.into_iter().rev() {
        let at = staging.join(path);
        let outcome = if at.is_dir() {
            std::fs::remove_dir_all(&at)
        } else {
            std::fs::remove_file(&at)
        };
        match outcome {
            Ok(()) => {}
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {}
            Err(reason) => return Err(filesystem_failure(Surface::Destination, &at, &reason)),
        }
    }

    let mut directories: Vec<&TreeEntry> = entries
        .iter()
        .filter(|entry| matches!(entry, TreeEntry::Directory { .. }))
        .collect();
    directories.sort_by_key(|entry| entry.path().as_str().matches('/').count());
    for entry in directories {
        with.platform
            .create_directories(&staging.join(entry.path().as_str()))?;
    }

    for entry in from_yours {
        let path = entry.path().as_str();
        let to = staging.join(path);
        let parent = super::paths::containing_directory(&to);
        with.platform.create_directories(&parent)?;
        match entry {
            TreeEntry::Directory { .. } => with.platform.create_directories(&to)?,
            TreeEntry::Symlink { .. } => {
                let target = std::fs::read_link(destination.join(path))
                    .map_err(|reason| filesystem_failure(Surface::Source, &to, &reason))?;
                let bytes = target.to_string_lossy().replace('\\', "/").into_bytes();
                with.platform.create_symlink(&bytes, &to)?;
            }
            TreeEntry::File { .. } => {
                let from = destination.join(path);
                if to.exists() {
                    std::fs::remove_file(&to)
                        .map_err(|reason| filesystem_failure(Surface::Destination, &to, &reason))?;
                }
                with.platform.clone_or_copy(&from, &to)?;
                with.work.read_bytes(super::local::entry_size(entry));
                with.work.wrote_bytes(super::local::entry_size(entry));
            }
        }
    }

    conflicts.sort();
    Ok(Applied { entries, conflicts })
}

fn states_what_the_record_states(
    rebuilt: &[TreeEntry],
    record: &fetchloom_engine::receipt::Receipt,
) -> Result<(), Error> {
    let wanted = fetchloom_engine::canonical::tree_digest(record.resolved_entries());
    let built = fetchloom_engine::canonical::tree_digest(rebuilt);
    if built == wanted {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "fetch this destination again rather than restoring it, because rebuilding what the \
             record states produced {built} where the record states {wanted}"
        ),
    ))
}

pub(crate) fn to_record(
    with: &Materialization<'_>,
    destination: &Path,
    record: &fetchloom_engine::receipt::Receipt,
    yours: &[TreeEntry],
    decisions: &[Merged],
    emit: &dyn Fn(fetchloom_engine::event::EventPayload),
) -> Result<Applied, Error> {
    held_by_the_cache(with, record)?;
    let staging = super::staged::open_staging(destination, with)?;
    let built = fill_from_record(with, record, &staging, emit).and_then(|rebuilt| {
        states_what_the_record_states(&rebuilt, record)?;
        apply(
            with,
            destination,
            &staging,
            Sides {
                yours,
                upstream: record.resolved_entries(),
            },
            decisions,
        )
    });
    let applied = match built {
        Ok(applied) => applied,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    super::staged::publish(with, &staging, destination)?;
    Ok(applied)
}

fn fill_from_record(
    with: &Materialization<'_>,
    record: &fetchloom_engine::receipt::Receipt,
    staging: &Path,
    emit: &dyn Fn(fetchloom_engine::event::EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    if !record.artifacts.is_empty() {
        let placed = super::dataset::placed_by_record(record);
        return super::dataset::fill_dataset_staging(with, &placed, staging, emit);
    }
    let mut directories: Vec<&TreeEntry> = record
        .resolved_entries()
        .iter()
        .filter(|entry| matches!(entry, TreeEntry::Directory { .. }))
        .collect();
    directories.sort_by_key(|entry| entry.path().as_str().matches('/').count());
    for entry in directories {
        with.platform
            .create_directories(&staging.join(entry.path().as_str()))?;
    }
    for entry in record.resolved_entries() {
        match entry {
            TreeEntry::Directory { .. } => {}
            TreeEntry::Symlink { path, .. } => {
                return Err(Error::new(
                    ErrorKind::CacheCorrupt,
                    format!(
                        "fetch this destination again rather than restoring it, because the record \
                         names the symbolic link {path} by what it points at and not by the text \
                         of the target, and nothing here holds that text"
                    ),
                ));
            }
            TreeEntry::File { path, content, .. } => {
                let at = staging.join(path.as_str());
                with.platform
                    .create_directories(&super::paths::containing_directory(&at))?;
                super::object::place_object(with, *content, &at)?;
            }
        }
    }
    Ok(record.resolved_entries().to_vec())
}

fn held_by_the_cache(
    with: &Materialization<'_>,
    record: &fetchloom_engine::receipt::Receipt,
) -> Result<(), Error> {
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because restoring an entry reads the object it came from out of the cache",
        ));
    };
    let mut absent = Vec::new();
    for (id, artifact) in &record.artifacts {
        if cache.locate(artifact.digest).is_none() {
            absent.push(format!(
                "{id} ({}), which fetchloom get {} would bring back",
                artifact.digest,
                artifact.source_used.as_str()
            ));
        }
    }
    if record.artifacts.is_empty() {
        for entry in record.resolved_entries() {
            if let TreeEntry::File { path, content, .. } = entry
                && cache.locate(*content).is_none()
            {
                absent.push(format!("{path} ({content})"));
            }
        }
    }
    if absent.is_empty() {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "fetch what the cache no longer holds before reverting, because these objects are gone: {}",
            absent.join(", ")
        ),
    ))
}
