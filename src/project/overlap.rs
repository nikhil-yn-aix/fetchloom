//! where a project's data may never go: the cache folder. a dataset's files placed there would
//! pass for the cache's own records, as an unpacked archive's, and a cache placed among a
//! dataset's files would take theirs for its own.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::name;

/// how a destination in a project and the cache folder overlap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overlap {
    /// the destination is the cache folder, or a folder inside it.
    Inside,
    /// the cache folder is inside the destination.
    Holds,
}

/// how `destination` and `cache` overlap, each found as the disk finds it, through every link in
/// the folders of it that exist, with names compared as a disk that ignores case compares them;
/// none when neither holds the other.
pub(crate) fn overlap(destination: &Path, cache: &Path) -> Option<Overlap> {
    let destination = folded(&resolved(destination));
    let cache = folded(&resolved(cache));
    if destination.starts_with(&cache) {
        Some(Overlap::Inside)
    } else if cache.starts_with(&destination) {
        Some(Overlap::Holds)
    } else {
        None
    }
}

/// the names of `path`, each as [`name::fold`] keys it.
fn folded(path: &Path) -> Vec<String> {
    path.components()
        .map(|part| name::fold(&part.as_os_str().to_string_lossy()))
        .collect()
}

/// `path` as the disk finds it: made whole from the current folder, then the longest part of it
/// that exists followed through every link, with the rest as written, `.` dropped and `..` taken
/// back.
fn resolved(path: &Path) -> PathBuf {
    let whole = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |here| here.join(path))
    };
    let parts: Vec<Component<'_>> = whole.components().collect();
    for kept in (1..=parts.len()).rev() {
        let head: PathBuf = parts[..kept].iter().collect();
        if let Ok(mut found) = fs::canonicalize(&head) {
            for part in &parts[kept..] {
                match part {
                    Component::ParentDir => {
                        found.pop();
                    }
                    Component::Normal(name) => found.push(name),
                    _ => {}
                }
            }
            return found;
        }
    }
    whole
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a destination overlaps the cache when either holds the other, however much of each exists,
    /// with names compared as a disk that ignores case compares them; a sibling whose name starts
    /// the same, or another folder beside the cache, is apart.
    #[test]
    fn a_destination_overlaps_the_cache_when_either_holds_the_other() {
        let folder = tempfile::tempdir().unwrap();
        let project = fs::canonicalize(folder.path()).unwrap();
        let cache = project.join(".fetchloom-cache");
        fs::create_dir_all(cache.join("unpacked")).unwrap();
        for (destination, expected) in [
            (".fetchloom-cache", Some(Overlap::Inside)),
            (".fetchloom-cache/unpacked", Some(Overlap::Inside)),
            (".fetchloom-cache/not/made/yet", Some(Overlap::Inside)),
            (".FETCHLOOM-CACHE/unpacked", Some(Overlap::Inside)),
            ("data/../.fetchloom-cache/x", Some(Overlap::Inside)),
            ("", Some(Overlap::Holds)),
            ("data", None),
            (".fetchloom-cache-old", None),
            (".fetchloom", None),
        ] {
            assert_eq!(
                overlap(&project.join(destination), &cache),
                expected,
                "{destination:?}"
            );
        }
        let made_later = project.join("data").join("cache");
        assert_eq!(
            overlap(&project.join("data"), &made_later),
            Some(Overlap::Holds)
        );
        assert_eq!(overlap(&project.join("elsewhere"), &made_later), None);
    }

    /// a cache, or a project, named through a link in a folder above it is the folder the link
    /// leads to.
    #[cfg(unix)]
    #[test]
    fn a_folder_named_through_a_link_is_the_folder_it_leads_to() {
        let folder = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(folder.path()).unwrap();
        let project = root.join("project");
        fs::create_dir_all(project.join(".fetchloom-cache")).unwrap();
        let via = root.join("via");
        std::os::unix::fs::symlink(&project, &via).unwrap();
        let unpacked = project.join(".fetchloom-cache").join("unpacked");
        let through = via.join(".fetchloom-cache");
        assert_eq!(overlap(&unpacked, &through), Some(Overlap::Inside));
        assert_eq!(
            overlap(
                &via.join(".fetchloom-cache/unpacked"),
                &project.join(".fetchloom-cache")
            ),
            Some(Overlap::Inside)
        );
        assert_eq!(overlap(&project.join("data"), &through), None);
    }
}
