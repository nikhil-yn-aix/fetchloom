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
                // the same bytes unpack to the same files, while the dataset still unpacks
                let unpacks_to = earlier
                    .filter(|file| entry.extract && known == Some((file.sha256, file.size)))
                    .and_then(|file| file.unpacks_to);
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
            let (from, divert) = source::fetch_plan(&remote.url);
            let fetch = Fetch {
                url: remote.url.clone(),
                from,
                divert,
                expect,
                revalidate: learn && remote.sha256.is_none() && remote.md5.is_none(),
                unpack: self.unpacks(file),
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
    })
}

/// the files `select` keeps and `exclude` leaves. a select pattern that matches nothing is a
/// mistake worth stopping for, with real paths to compare against.
pub(super) fn filter(name: &str, entry: &Entry, listing: &Listing) -> Result<Vec<Remote>, Error> {
    for glob in &entry.select {
        if !listing.files.iter().any(|file| glob.matches(&file.path)) {
            return Err(unmatched(name, entry, glob, listing));
        }
    }
    let kept: Vec<Remote> = listing
        .files
        .iter()
        .filter(|file| {
            entry.select.is_empty() || entry.select.iter().any(|glob| glob.matches(&file.path))
        })
        .filter(|file| !entry.exclude.iter().any(|glob| glob.matches(&file.path)))
        .cloned()
        .collect();
    if kept.is_empty() {
        let mut error = Error::new(
            Kind::ProjectInvalid,
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

/// where an entry's patterns were written. `add` and `get` clear the lines of an entry they make
/// from the command line, since no line of data.toml is to blame for it.
fn written(entry: &Entry) -> Patterns {
    if entry.lines.is_empty() {
        Patterns::OnCommandLine
    } else {
        Patterns::InDataToml
    }
}

fn unmatched(name: &str, entry: &Entry, glob: &Glob, listing: &Listing) -> Error {
    let mut error = Error::new(
        Kind::ProjectInvalid,
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
            glob.is_top_level_only() && listing.files.iter().any(|file| deeper.matches(&file.path))
        });
    let example = match &deeper {
        Some(deeper) => listing.files.iter().find(|file| deeper.matches(&file.path)),
        None => listing.files.first(),
    };
    if let Some(example) = example {
        let considered = units::count(listing.files.len() as u64, "file was", "files were");
        error = error.detail(format!(
            "{considered} considered, for example {}",
            example.path
        ));
    }
    match deeper {
        Some(deeper) => error.help(format!(
            "patterns match the whole path. try \"{}\"",
            deeper.as_str()
        )),
        None => error.help("compare the pattern with the paths above"),
    }
}
