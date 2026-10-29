//! a dataset on its way into data.lock: resolved and filtered, with every hash no source states
//! filled in from earlier locks or from the bytes themselves.

use super::super::fetch::Job;
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::net::{Expect, Fetch, Fetched};
use crate::plan::Patterns;
use crate::project::lock::{self, Locked, LockedFile};
use crate::project::manifest::Entry;
use crate::source::{self, Listing, Remote};
use crate::units;

pub(super) struct Draft {
    name: String,
    reference: String,
    spec: String,
    /// whether archives unpack, as data.toml says.
    extract: bool,
    listing: Listing,
    files: Vec<DraftFile>,
}

struct DraftFile {
    remote: Remote,
    path: String,
    /// the hash and size, once known: stated by the source, carried over from data.lock, or
    /// learned from the bytes.
    known: Option<(Sha256, u64)>,
    /// for an archive, the tree hash of what it unpacks to, once known.
    unpacks_to: Option<Sha256>,
}

/// an archive a draft needs unpacked to learn what it unpacks to.
pub(super) struct ToUnpack {
    pub(super) file: usize,
    pub(super) archive: Sha256,
    pub(super) size: u64,
    pub(super) label: String,
}

impl Draft {
    /// a dataset's files with every hash known without a download: stated by the source, carried
    /// over from data.lock, or remembered from an earlier download whose MD5 the source stated.
    /// an upgrade carries nothing over for a file no checksum pins, since only asking the server
    /// tells whether its bytes changed.
    pub(super) fn new(
        name: &str,
        entry: &Entry,
        listing: Listing,
        previous: Option<&Locked>,
        upgrading: bool,
        cache: &Cache,
    ) -> Result<Draft, Error> {
        let files = filter(name, entry, &listing)?
            .into_iter()
            .map(|remote| {
                let pinned = remote.sha256.is_some() || remote.md5.is_some();
                let earlier = previous
                    .and_then(|locked| carried(locked, &remote))
                    .filter(|_| pinned || !upgrading);
                let known = match (remote.sha256, remote.size) {
                    (Some(sha256), Some(size)) => Some((sha256, size)),
                    _ => earlier
                        .map(|file| (file.sha256, file.size))
                        .or_else(|| stated(cache, &remote)),
                };
                let path = match earlier {
                    Some(file) if remote.named_by_server => file.path.clone(),
                    _ => remote.path.clone(),
                };
                // the same bytes unpack to the same files, while the dataset still unpacks and
                // the name says archive. an upgrade pins what this machine's unpacking recorded
                // for them instead, so a tree another version of fetchloom pinned can be
                // replaced without downloading again
                let unpacks = entry.extract && archive::by_name(&path) == Some(Packed::Archive);
                let unpacks_to = earlier
                    .filter(|file| unpacks && known == Some((file.sha256, file.size)))
                    .and_then(|file| {
                        let recorded = if upgrading {
                            cache.unpacked_tree(&file.sha256)
                        } else {
                            None
                        };
                        recorded.or(file.unpacks_to)
                    });
                DraftFile {
                    remote,
                    path,
                    known,
                    unpacks_to,
                }
            })
            .collect();
        Ok(Draft {
            name: name.to_owned(),
            reference: entry.reference.clone(),
            spec: lock::spec(entry),
            extract: entry.extract,
            listing,
            files,
        })
    }

    /// the downloads this dataset needs, each with the index of the file whose hash it learns:
    /// every file whose hash is unknown, and every archive whose bytes must be unpacked to learn
    /// what it unpacks to. a file no checksum pins is asked about with what its URL served
    /// before, so bytes the cache already holds are not sent again.
    pub(super) fn jobs(&self, cache: &Cache) -> Vec<(Option<usize>, Job)> {
        let mut jobs = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            let remote = &file.remote;
            let (expect, learn) = match file.known {
                None => (
                    Expect {
                        sha256: remote.sha256,
                        md5: remote.md5,
                        size: remote.size,
                    },
                    true,
                ),
                Some((sha256, size))
                    if self.unpacks(file)
                        && file.unpacks_to.is_none()
                        && cache.unpacked_tree(&sha256).is_none()
                        && cache.find(&sha256, Some(size)).is_none() =>
                {
                    let expect = Expect {
                        sha256: Some(sha256),
                        md5: remote.md5,
                        size: Some(size),
                    };
                    (expect, false)
                }
                Some(_) => continue,
            };

            let fetch = Fetch {
                expect,
                revalidate: learn && remote.sha256.is_none() && remote.md5.is_none(),
                unpack: self.unpacks(file),
                ..source::fetch_plan(&remote.url)
            };
            let job = Job {
                fetch,
                label: format!("{}/{}", self.name, file.path),
                dataset: Some(self.name.clone()),
                again: None,
            };
            jobs.push((learn.then_some(index), job));
        }
        jobs
    }

    /// records what the bytes of file `index` turned out to be.
    pub(super) fn learn(&mut self, index: usize, fetched: &Fetched) {
        let file = &mut self.files[index];
        file.known = Some((fetched.digest, fetched.size));
        if file.remote.named_by_server
            && let Some(given) = fetched.name.as_deref().and_then(name::sanitize)
        {
            file.path = given;
        }
    }

    /// the archives that must be unpacked to learn what they unpack to. an archive this machine
    /// unpacked before is known from its record and needs no unpacking.
    pub(super) fn unknown_archives(&mut self, cache: &Cache) -> Vec<ToUnpack> {
        let mut todo = Vec::new();
        for index in 0..self.files.len() {
            let file = &self.files[index];
            let Some((archive, size)) = file.known else {
                continue;
            };
            if !self.unpacks(file) || file.unpacks_to.is_some() {
                continue;
            }
            match cache.unpacked_tree(&archive) {
                Some(tree) => self.files[index].unpacks_to = Some(tree),
                None => todo.push(ToUnpack {
                    file: index,
                    archive,
                    size,
                    label: format!("{}/{}", self.name, file.path),
                }),
            }
        }
        todo
    }

    /// records what file `index` unpacked to, or that its bytes are no archive after all.
    pub(super) fn unpacked(&mut self, index: usize, tree: Option<Sha256>) {
        self.files[index].unpacks_to = tree;
    }

    /// archives in this dataset that fetchloom leaves packed, each with its format.
    pub(super) fn left_packed(&self) -> Vec<(String, &'static str)> {
        self.files
            .iter()
            .filter(|_| self.extract)
            .filter_map(|file| match archive::by_name(&file.path) {
                Some(Packed::Unsupported(format)) => {
                    Some((format!("{}/{}", self.name, file.path), format))
                }
                _ => None,
            })
            .collect()
    }

    /// whether the file is an archive that unpacks, by its name and data.toml.
    fn unpacks(&self, file: &DraftFile) -> bool {
        self.extract && archive::by_name(&file.path) == Some(Packed::Archive)
    }

    /// the dataset's entry in data.lock.
    pub(super) fn finish(self, now: &str) -> Locked {
        let mut files: Vec<LockedFile> = self
            .files
            .into_iter()
            .filter_map(|file| {
                let (sha256, size) = file.known?;
                Some(LockedFile {
                    path: file.path,
                    size,
                    sha256,
                    md5: file.remote.md5,
                    url: file.remote.url.to_string(),
                    unpacks_to: file.unpacks_to,
                })
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Locked {
            name: self.name,
            reference: self.reference,
            spec: self.spec,
            resolved: self.listing.resolved.to_string(),
            title: self.listing.title,
            license: self.listing.license,
            doi: self.listing.doi,
            timestamp: now.to_owned(),
            files,
        }
    }
}

/// the hash and size of the bytes an earlier download proved match the MD5 a source states.
fn stated(cache: &Cache, remote: &Remote) -> Option<(Sha256, u64)> {
    let (sha256, size) = cache.stated_md5(&remote.md5?)?;
    remote
        .size
        .is_none_or(|stated| stated == size)
        .then_some((sha256, size))
}

/// the file data.lock already pins for this remote file, when it is the same file: the same
/// link, and the same path unless the server names the file.
fn carried<'a>(locked: &'a Locked, remote: &Remote) -> Option<&'a LockedFile> {
    let url = remote.url.as_str();
    locked.files.iter().find(|file| {
        file.url == url
            && (remote.named_by_server || file.path == remote.path)
            && remote.size.is_none_or(|size| size == file.size)
            && remote.sha256.is_none_or(|sha256| sha256 == file.sha256)
            && remote
                .md5
                .is_none_or(|md5| file.md5.is_none_or(|was| was == md5))
    })
}

/// the files `select` keeps and `exclude` leaves. a select pattern that matches nothing is a
/// mistake worth stopping for, with real paths to compare against.
pub(super) fn filter(name: &str, entry: &Entry, listing: &Listing) -> Result<Vec<Remote>, Error> {
    for glob in &entry.select {
        if !listing.paths().any(|path| glob.matches(path)) {
            return Err(unmatched(name, entry, glob, listing));
        }
    }
    let selected = |path: &str| {
        (entry.select.is_empty() || entry.select.iter().any(|glob| glob.matches(path)))
            && !entry.exclude.iter().any(|glob| glob.matches(path))
    };
    let unavailable: Vec<_> = listing
        .unavailable
        .iter()
        .filter(|item| selected(&item.path))
        .collect();
    if !unavailable.is_empty() {
        let has_files = listing.files.iter().any(|file| selected(&file.path));
        return Err(unavailable_selection(name, &unavailable, has_files));
    }
    let kept: Vec<Remote> = listing
        .files
        .iter()
        .filter(|file| selected(&file.path))
        .cloned()
        .collect();
    if kept.is_empty() {
        let mut error = Error::new(
            kind(entry),
            format!("select and exclude leave no file of {name}"),
        );
        if let Some(line) = entry.lines.get("exclude") {
            error = error.detail(line.clone());
        }
        return Err(error.help(match written(entry) {
            Patterns::InDataToml => "loosen the patterns in data.toml",
            Patterns::OnCommandLine => "loosen the patterns given to --select and --exclude",
        }));
    }
    Ok(kept)
}

fn unavailable_selection(
    name: &str,
    entries: &[&crate::source::Unavailable],
    has_files: bool,
) -> Error {
    let mut error = Error::new(
        Kind::SourceInvalid,
        format!(
            "{name} has {} that fetchloom cannot download",
            units::count(entries.len() as u64, "selected entry", "selected entries")
        ),
    );
    for entry in entries {
        error = error.detail(format!("{}: {}", entry.path, entry.reason));
    }
    let flags = entries
        .iter()
        .map(|entry| exclusion_flag(&entry.path))
        .collect::<Vec<_>>()
        .join(" ");
    let purpose = if has_files {
        "to fetch the other files"
    } else {
        "to leave the selected entries out"
    };
    let copy = if entries.len() == 1 {
        "it as a file"
    } else {
        "them as files"
    };
    error.help(format!(
        "add {flags} {purpose}, or ask the owner to add {copy} such as pdf or csv"
    ))
}

fn exclusion_flag(path: &str) -> String {
    let mut pattern = String::new();
    for character in path.chars() {
        match character {
            '[' => pattern.push_str("[[]"),
            '*' => pattern.push_str("[*]"),
            '?' => pattern.push_str("[?]"),
            other => pattern.push(other),
        }
    }
    let quoted = if pattern.contains(['\'', '"', '$', '\u{60}']) {
        if cfg!(windows) {
            format!("'{}'", pattern.replace('\'', "''"))
        } else {
            format!("'{}'", pattern.replace('\'', "'\"'\"'"))
        }
    } else {
        format!("\"{pattern}\"")
    };
    format!("--exclude {quoted}")
}

/// where an entry's patterns were written. `add` and `get` clear the lines of an entry they make
/// from the command line, since no line of data.toml is to blame for it.
fn written(entry: &Entry) -> Patterns {
    if entry.lines.is_empty() {
        Patterns::OnCommandLine
    } else {
        Patterns::InDataToml
    }
}

/// a pattern that keeps no file is a mistake on the command line, or in data.toml.
fn kind(entry: &Entry) -> Kind {
    match written(entry) {
        Patterns::OnCommandLine => Kind::Usage,
        Patterns::InDataToml => Kind::ProjectInvalid,
    }
}

fn unmatched(name: &str, entry: &Entry, glob: &Glob, listing: &Listing) -> Error {
    let mut error = Error::new(
        kind(entry),
        format!(
            "the pattern \"{}\" matched no file in {name}",
            glob.as_str()
        ),
    );
    if let Some(line) = entry.lines.get("select") {
        error = error.detail(line.clone());
    }
    let deeper = Glob::new(&format!("**/{}", glob.as_str()))
        .ok()
        .filter(|deeper| {
            glob.is_top_level_only() && listing.paths().any(|path| deeper.matches(path))
        });
    let example = match &deeper {
        Some(deeper) => listing.paths().find(|path| deeper.matches(path)),
        None => listing.paths().next(),
    };
    if let Some(example) = example {
        let considered = if listing.unavailable.is_empty() {
            units::count(listing.files.len() as u64, "file was", "files were")
        } else {
            units::count(
                (listing.files.len() + listing.unavailable.len()) as u64,
                "entry was",
                "entries were",
            )
        };
        error = error.detail(format!("{considered} considered, for example {example}"));
    }
    match deeper {
        Some(deeper) => error.help(format!(
            "patterns match the whole path. try \"{}\"",
            deeper.as_str()
        )),
        None => error.help("compare the pattern with the paths above"),
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;
    use crate::cache::Member;
    use crate::cache::tests::store;
    use crate::digest;
    use crate::project::manifest::Manifest;
    use crate::source::Reference;

    #[test]
    fn an_upgrade_takes_the_tree_this_machine_unpacked() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let archive = store(&cache, b"the zip");
        let inside = store(&cache, b"a,b\n");
        let members = [Member {
            path: "a.csv".to_owned(),
            size: inside.size,
            sha256: inside.digest,
        }];
        let recorded = cache.record_unpacked(&archive.digest, &members).unwrap();
        let stale = digest::sha256(b"a tree another version of fetchloom made");
        let url = Url::parse("https://example.org/set.zip").unwrap();
        let manifest =
            Manifest::parse("[datasets.set]\nref = \"https://example.org/set.zip\"\n").unwrap();
        let listing = || Listing {
            unavailable: Vec::new(),
            resolved: Reference::Url(url.clone()),
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "set.zip".to_owned(),
                size: Some(archive.size),
                sha256: Some(archive.digest),
                md5: None,
                url: url.clone(),
                named_by_server: false,
            }],
        };
        let locked = Locked {
            name: "set".to_owned(),
            reference: url.to_string(),
            spec: String::new(),
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            timestamp: String::new(),
            files: vec![LockedFile {
                path: "set.zip".to_owned(),
                size: archive.size,
                sha256: archive.digest,
                md5: None,
                url: url.to_string(),
                unpacks_to: Some(stale),
            }],
        };
        let entry = &manifest.datasets["set"];
        let kept = Draft::new("set", entry, listing(), Some(&locked), false, &cache).unwrap();
        assert_eq!(
            kept.files[0].unpacks_to,
            Some(stale),
            "a relock keeps the pin"
        );
        let upgraded = Draft::new("set", entry, listing(), Some(&locked), true, &cache).unwrap();
        assert_eq!(
            upgraded.files[0].unpacks_to,
            Some(recorded),
            "an upgrade pins what this fetchloom unpacks"
        );
    }

    #[test]
    fn a_file_whose_name_is_no_archive_never_unpacks() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        // the same bytes as a zip that was unpacked under another name
        let bytes = store(&cache, b"zip bytes inside an npz");
        let tree = cache.record_unpacked(&bytes.digest, &[]).unwrap();
        let url = Url::parse("https://example.org/weights.npz").unwrap();
        let manifest =
            Manifest::parse("[datasets.w]\nref = \"https://example.org/weights.npz\"\n").unwrap();
        let listing = || Listing {
            unavailable: Vec::new(),
            resolved: Reference::Url(url.clone()),
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "weights.npz".to_owned(),
                size: Some(bytes.size),
                sha256: Some(bytes.digest),
                md5: None,
                url: url.clone(),
                named_by_server: false,
            }],
        };
        let locked = |unpacks_to| Locked {
            name: "w".to_owned(),
            reference: url.to_string(),
            spec: String::new(),
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            timestamp: String::new(),
            files: vec![LockedFile {
                path: "weights.npz".to_owned(),
                size: bytes.size,
                sha256: bytes.digest,
                md5: None,
                url: url.to_string(),
                unpacks_to,
            }],
        };
        let entry = &manifest.datasets["w"];
        for (upgrading, before) in [(true, None), (false, Some(tree)), (true, Some(tree))] {
            let draft = Draft::new(
                "w",
                entry,
                listing(),
                Some(&locked(before)),
                upgrading,
                &cache,
            )
            .unwrap();
            assert_eq!(draft.files[0].unpacks_to, None, "{upgrading} {before:?}");
        }
    }

    #[test]
    fn a_pin_is_not_carried_once_the_source_states_another_md5() {
        let url = Url::parse("https://example.org/data.csv").unwrap();
        let file = |md5: &str| LockedFile {
            path: "data.csv".to_owned(),
            size: 4,
            sha256: digest::sha256(b"a,b\n"),
            md5: crate::digest::Md5::from_hex(md5),
            url: url.to_string(),
            unpacks_to: None,
        };
        let locked = Locked {
            name: "d".to_owned(),
            reference: url.to_string(),
            spec: String::new(),
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            timestamp: String::new(),
            files: vec![file("900150983cd24fb0d6963f7d28e17f72")],
        };
        let remote = |md5: &str| Remote {
            path: "data.csv".to_owned(),
            size: Some(4),
            sha256: None,
            md5: crate::digest::Md5::from_hex(md5),
            url: url.clone(),
            named_by_server: false,
        };
        assert!(carried(&locked, &remote("900150983cd24fb0d6963f7d28e17f72")).is_some());
        assert!(
            carried(&locked, &remote("e80b5017098950fc58aad83c8c14978e")).is_none(),
            "the bytes changed under the same link"
        );
    }

    #[test]
    fn an_unknown_drive_size_is_pinned_from_downloaded_bytes() {
        use crate::source::recorded::{Recorded, block_on};

        let url =
            "https://drive.google.com/embeddedfolderview?id=1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U";
        let recorded = Recorded::default().fixture(url, "drive/folder-deep-1-1.html");
        let reference = Reference::parse("drive:folders/1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U")
            .unwrap()
            .reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.files[0].size, None);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.sample]\nref = \"drive:folders/1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U\"\n",
        )
        .unwrap();
        let mut draft = Draft::new(
            "sample",
            &manifest.datasets["sample"],
            listing,
            None,
            false,
            &cache,
        )
        .unwrap();
        let jobs = draft.jobs(&cache);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].1.fetch.expect.size, None);
        let stored = store(&cache, b"data");
        draft.learn(
            0,
            &Fetched {
                digest: stored.digest,
                size: stored.size,
                name: Some("other.txt".to_owned()),
                origin: crate::net::Origin::Network,
            },
        );
        let locked = draft.finish("2026-10-29T00:00:00Z");
        assert_eq!(locked.files.len(), 1);
        assert_eq!(locked.files[0].path, "file.txt");
        assert_eq!(locked.files[0].size, 4);
        assert_eq!(locked.files[0].sha256, stored.digest);
    }
}

#[cfg(test)]
mod drive_docs_tests;
