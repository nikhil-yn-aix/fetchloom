//! the archives among a dataset's pinned files: a file that unpacks, or the parts of an archive
//! split into several files. data.lock gives `unpacks_to` to a split archive's first part only;
//! its other parts are known by their names, as [`parts`] reads them.

use std::collections::HashMap;

use super::{Lock, LockedFile};
use crate::archive::parts;
use crate::cache::{Cache, Member, parts_key};
use crate::digest::Sha256;

/// an archive data.lock pins to unpack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Archive<'a> {
    /// the file data.lock gives `unpacks_to`: the archive itself, or a split archive's first part,
    /// whose path names it in members tables and messages.
    pub(crate) file: &'a LockedFile,
    /// a split archive's parts in order, its first part among them; none for an archive in one
    /// file.
    pub(crate) parts: Vec<&'a LockedFile>,
}

impl<'a> Archive<'a> {
    /// the key the cache's record of what it unpacks to goes by.
    pub(crate) fn key(&self) -> Sha256 {
        if self.parts.is_empty() {
            self.file.sha256
        } else {
            parts_key(self.parts.iter().map(|part| (part.sha256, part.size)))
        }
    }

    /// what this machine recorded it unpacks to, when that is the tree data.lock pins: the
    /// lookup every plan of what a lock places takes, by [`Archive::key`], never by a split
    /// archive's first part alone.
    pub(crate) fn recorded(&self, cache: &Cache) -> Option<Vec<Member>> {
        cache.unpacked(&self.key(), self.file.unpacks_to.as_ref())
    }

    /// the path it unpacks from as placing takes it: `raw/scans.zip` for the parts from
    /// `raw/scans.zip.001` on.
    pub(crate) fn path(&self) -> &'a str {
        let path = self.file.path.as_str();
        if self.parts.is_empty() {
            path
        } else {
            parts::whole(path).unwrap_or(path)
        }
    }

    /// the files holding its bytes, in order.
    pub(crate) fn files(&self) -> Vec<&'a LockedFile> {
        if self.parts.is_empty() {
            vec![self.file]
        } else {
            self.parts.clone()
        }
    }
}

/// a dataset's pinned file as placing takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pinned<'a> {
    /// a file placed as it is.
    File(&'a LockedFile),
    /// an archive, placed as the files it unpacks to.
    Archive(Archive<'a>),
}

/// a dataset's pinned `files`, in their order, as placing takes them: each archive once, a split
/// one at its first part when data.lock gives that part `unpacks_to`, and every other file as it
/// is, the parts of a split archive that stays packed too.
pub(crate) fn pinned(files: &[LockedFile]) -> Vec<Pinned<'_>> {
    let split = parts::split(files.iter().map(|file| file.path.as_str()));
    let mut sets: HashMap<usize, Vec<&LockedFile>> = HashMap::new();
    let mut later = vec![false; files.len()];
    for set in split.sets {
        if files[set[0]].unpacks_to.is_none() {
            continue;
        }
        for index in &set[1..] {
            later[*index] = true;
        }
        sets.insert(set[0], set.iter().map(|index| &files[*index]).collect());
    }
    files
        .iter()
        .enumerate()
        .filter(|(index, _)| !later[*index])
        .map(|(index, file)| match sets.remove(&index) {
            Some(parts) => Pinned::Archive(Archive { file, parts }),
            None if file.unpacks_to.is_some() => Pinned::Archive(Archive {
                file,
                parts: Vec::new(),
            }),
            None => Pinned::File(file),
        })
        .collect()
}

/// the archive `file`, one of a dataset's pinned `files`, unpacks as when data.lock gives it
/// `unpacks_to`: the file, or each part of the split archive it is the first part of. a split
/// archive's parts share the path before their counters, and data.lock sorts files by path, so
/// only the files sharing that path are read.
pub(crate) fn archive<'a>(files: &'a [LockedFile], file: &'a LockedFile) -> Archive<'a> {
    let whole = || Archive {
        file,
        parts: Vec::new(),
    };
    let Some(path) = parts::whole(&file.path) else {
        return whole();
    };
    let start = files.partition_point(|other| other.path.as_str() < path);
    let near = &files[start..];
    let near = &near[..near.partition_point(|other| other.path.starts_with(path))];
    let split = parts::split(near.iter().map(|other| other.path.as_str()));
    let set = split
        .sets
        .into_iter()
        .find(|set| near[set[0]].path == file.path);
    set.map_or_else(whole, |set| Archive {
        file,
        parts: set.iter().map(|index| &near[*index]).collect(),
    })
}

/// the archive each of `files` in `lock` unpacks as, with the name of its dataset; a file data.lock
/// gives no `unpacks_to` stands alone.
pub(crate) fn of<'a>(
    lock: &'a Lock,
    files: Vec<(String, &'a LockedFile)>,
) -> Vec<(String, Archive<'a>)> {
    files
        .into_iter()
        .map(|(dataset, file)| {
            let archive = match lock.get(&dataset) {
                Some(locked) if file.unpacks_to.is_some() => archive(&locked.files, file),
                _ => Archive {
                    file,
                    parts: Vec::new(),
                },
            };
            (dataset, archive)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;
    use crate::project::lock::Locked;

    fn file(path: &str, unpacks: bool) -> LockedFile {
        LockedFile {
            path: path.to_owned(),
            size: path.len() as u64,
            sha256: digest::sha256(path.as_bytes()),
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to: unpacks.then(|| digest::sha256(b"tree")),
        }
    }

    fn paths(pinned: &[Pinned<'_>]) -> Vec<String> {
        pinned
            .iter()
            .map(|pinned| match pinned {
                Pinned::File(file) => file.path.clone(),
                Pinned::Archive(archive) => {
                    let parts: Vec<&str> = archive
                        .parts
                        .iter()
                        .map(|part| part.path.as_str())
                        .collect();
                    format!("{} [{}]", archive.file.path, parts.join(" "))
                }
            })
            .collect()
    }

    #[test]
    fn a_split_archive_is_one_archive_at_its_first_part() {
        let files = [
            file("README.md", false),
            file("scans.zip.001", true),
            file("scans.zip.002", false),
            file("scans.zip.003", false),
            file("train.zip", true),
        ];
        let pinned = pinned(&files);
        assert_eq!(
            paths(&pinned),
            [
                "README.md",
                "scans.zip.001 [scans.zip.001 scans.zip.002 scans.zip.003]",
                "train.zip []",
            ]
        );
        let Pinned::Archive(scans) = &pinned[1] else {
            panic!("scans.zip.001 is no archive");
        };
        assert_eq!(scans.path(), "scans.zip");
        assert_eq!(
            scans.key(),
            parts_key(files[1..4].iter().map(|part| (part.sha256, part.size)))
        );
        assert_eq!(scans.files().len(), 3);
        let Pinned::Archive(train) = &pinned[2] else {
            panic!("train.zip is no archive");
        };
        assert_eq!((train.path(), train.key()), ("train.zip", files[4].sha256));
    }

    #[test]
    fn parts_whose_first_does_not_unpack_are_files() {
        let packed = [file("scans.zip.001", false), file("scans.zip.002", false)];
        assert_eq!(paths(&pinned(&packed)), ["scans.zip.001", "scans.zip.002"]);
    }

    #[test]
    fn a_first_part_finds_its_archive_in_the_lock() {
        let lock = Lock {
            datasets: vec![Locked {
                name: "scans".to_owned(),
                reference: String::new(),
                spec: String::new(),
                extract: true,
                resolved: String::new(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: String::new(),
                files: vec![
                    file("a.zip.001", true),
                    file("a.zip.001.md5", false),
                    file("a.zip.002", false),
                    file("b.csv", false),
                ],
            }],
            ..Lock::default()
        };
        let files = &lock.datasets[0].files;
        let found = of(
            &lock,
            vec![
                ("scans".to_owned(), &files[0]),
                ("scans".to_owned(), &files[3]),
            ],
        );
        let parts: Vec<&str> = found[0]
            .1
            .parts
            .iter()
            .map(|part| part.path.as_str())
            .collect();
        assert_eq!(parts, ["a.zip.001", "a.zip.002"]);
        assert_eq!(
            found[1].1.files(),
            [&files[3]],
            "a file that does not unpack"
        );
    }
}
