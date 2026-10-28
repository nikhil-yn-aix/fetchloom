//! a dataset on its way into data.lock: resolved and filtered, with every hash no source states
//! filled in from earlier locks or from the bytes themselves.

use super::super::fetch::Job;
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::net::{Expect, Fetch, Fetched};
use crate::project::lock::{self, Locked, LockedFile};
use crate::project::manifest::Entry;
use crate::source::{self, Listing, Remote};
use crate::units;

pub(super) struct Draft {
    name: String,
    reference: String,
    spec: String,
    listing: Listing,
    files: Vec<DraftFile>,
}

struct DraftFile {
    remote: Remote,
    path: String,
    /// the hash and size, once known: stated by the source, carried over from data.lock, or
    /// learned from the bytes.
    known: Option<(Sha256, u64)>,
}

impl Draft {
    pub(super) fn new(
        name: &str,
        entry: &Entry,
        listing: Listing,
        previous: Option<&Locked>,
    ) -> Result<Draft, Error> {
        let files = filter(name, entry, &listing)?
            .into_iter()
            .map(|remote| {
                let earlier = previous.and_then(|locked| carried(locked, &remote));
                let known = match (remote.sha256, remote.size) {
                    (Some(sha256), Some(size)) => Some((sha256, size)),
                    _ => earlier.map(|file| (file.sha256, file.size)),
                };
                let path = match earlier {
                    Some(file) if remote.named_by_server => file.path.clone(),
                    _ => remote.path.clone(),
                };
                DraftFile {
                    remote,
                    path,
                    known,
                }
            })
            .collect();
        Ok(Draft {
            name: name.to_owned(),
            reference: entry.reference.clone(),
            spec: lock::spec(entry),
            listing,
            files,
        })
    }

    /// the downloads this dataset needs: each file whose hash is unknown, with its index, and
    /// with `fill`, each known file missing from the cache.
    pub(super) fn jobs(&self, cache: &Cache, fill: bool) -> Vec<(Option<usize>, Job)> {
        let mut jobs = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            let learn = file.known.is_none();
            let missing = file
                .known
                .is_some_and(|(sha256, size)| cache.find(&sha256, Some(size)).is_none());
            if !(learn || fill && missing) {
                continue;
            }
            let expect = match file.known {
                Some((sha256, size)) => Expect {
                    sha256: Some(sha256),
                    md5: file.remote.md5,
                    size: Some(size),
                },
                None => Expect {
                    sha256: file.remote.sha256,
                    md5: file.remote.md5,
                    size: file.remote.size,
                },
            };
            let (from, divert) = source::fetch_plan(&file.remote.url);
            let fetch = Fetch {
                url: file.remote.url.clone(),
                from,
                divert,
                expect,
                revalidate: false,
            };
            jobs.push((
                learn.then_some(index),
                Job {
                    fetch,
                    label: format!("{}/{}", self.name, file.path),
                    dataset: Some(self.name.clone()),
                },
            ));
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
                    unpacks_to: None,
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
        return Err(error.help("loosen the patterns in data.toml"));
    }
    Ok(kept)
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
