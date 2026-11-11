//! a dataset on its way into data.lock: resolved and filtered, with every hash no source states
//! filled in from earlier locks or from the bytes themselves.

use std::collections::{HashMap, HashSet};

use url::Url;

use super::super::fetch::{self, Job};
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::net::{Expect, Fetch, Fetched};
use crate::plan::Patterns;
use crate::project::lock::{self, Locked, LockedFile};
use crate::project::manifest::{Entry, Source};
use crate::source::{self, Access, Listing, Remote};
use crate::units;

pub(super) struct Draft {
    name: String,
    reference: String,
    spec: String,
    /// whether archives unpack, as data.toml says.
    extract: bool,
    /// where the entry's patterns were written, for the help of an error.
    patterns: Patterns,
    /// what the listing resolved to and says about the dataset, as data.lock records them.
    resolved: String,
    title: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    access: Access,
    /// whether file links pin the object version their bytes were, as S3's do.
    versions: bool,
    files: Vec<DraftFile>,
}

struct DraftFile {
    remote: Remote,
    path: String,
    /// the S3 version the bytes were, once known, which the file's link in data.lock pins.
    version: Option<String>,
    /// the hash and size, once known: stated by the source, carried over from data.lock, or
    /// learned from the bytes.
    known: Option<(Sha256, u64)>,
    /// for an archive, the tree hash of what it unpacks to, once known.
    unpacks_to: Option<Sha256>,
    /// the pin data.lock had for a file an upgrade asks about again, since no checksum tells
    /// whether its bytes changed. only a dry run, which cannot ask, plans with it.
    was: Option<LockedFile>,
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
        source: &Source,
        listing: Listing,
        previous: Option<&Locked>,
        upgrading: bool,
        cache: &Cache,
    ) -> Result<Draft, Error> {
        // the files data.lock pins by their link, so a dataset of many files is not searched
        // once for each. a link pinned to an object version is found by the link without it,
        // as the listing gives it
        let versions = listing.resolved.keeps_versions();
        let mut by_url: HashMap<String, Vec<&LockedFile>> = HashMap::new();
        for file in previous.iter().flat_map(|locked| &locked.files) {
            let link = Url::parse(&file.url)
                .ok()
                .filter(|_| versions)
                .and_then(|url| source::unversioned(&url))
                .map_or_else(|| file.url.clone(), |(url, _)| url.to_string());
            by_url.entry(link).or_default().push(file);
        }
        let files = filter(name, entry, source, &listing)?
            .into_iter()
            .map(|remote| {
                let pinned = remote.sha256.is_some() || remote.md5.is_some();
                let found = carried(&by_url, &remote);
                let earlier = found.filter(|_| pinned || !upgrading);
                let was = found.filter(|_| earlier.is_none()).cloned();
                let stated = match (remote.sha256, remote.size) {
                    (Some(_), Some(_)) => None,
                    _ if earlier.is_some() => None,
                    _ => stated(cache, &remote),
                };
                let known = match (remote.sha256, remote.size) {
                    (Some(sha256), Some(size)) => Some((sha256, size)),
                    _ => earlier
                        .map(|file| (file.sha256, file.size))
                        .or_else(|| stated.as_ref().map(|stated| (stated.digest, stated.size))),
                };
                // a carried pin keeps its link as data.lock has it, version and all
                let version = stated.and_then(|stated| stated.version);
                // carried over, a file keeps the path data.lock gave it: a name its server gave,
                // or the zip its source wrapped it in
                let path = earlier.map_or_else(|| remote.path.clone(), |file| file.path.clone());
                // the same bytes unpack to the same files, while the dataset still unpacks and
                // the name says archive. an upgrade pins what this machine's unpacking recorded
                // for them instead, so a tree another version of fetchloom pinned can be
                // replaced without downloading again
                let unpacks = source.extract && archive::by_name(&path) == Some(Packed::Archive);
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
                let remote = match earlier {
                    Some(file) if versions => Remote {
                        url: Url::parse(&file.url).unwrap_or(remote.url),
                        ..remote
                    },
                    _ => remote,
                };
                DraftFile {
                    remote,
                    path,
                    version,
                    known,
                    unpacks_to,
                    was,
                }
            })
            .collect();
        Ok(Draft {
            name: name.to_owned(),
            reference: source.reference.clone(),
            spec: lock::spec(source),
            extract: source.extract,
            patterns: written(entry),
            resolved: listing.resolved.to_string(),
            title: listing.title,
            license: listing.license,
            doi: listing.doi,
            access: listing.access,
            versions,
            files,
        })
    }

    /// a dataset whose entry changed only in `extract`, from the files data.lock pins, so it
    /// needs no listing. what each archive unpacks to is learned again as for any draft: from
    /// this machine's record of it, or from its bytes, downloaded only when the cache lacks them.
    pub(super) fn switched(
        entry: &Entry,
        source: &Source,
        locked: &Locked,
    ) -> Result<Draft, Error> {
        let files = locked
            .files
            .iter()
            .map(|file| {
                let remote = Remote {
                    path: file.path.clone(),
                    size: Some(file.size),
                    sha256: Some(file.sha256),
                    md5: file.md5,
                    url: link(&locked.name, file)?,
                    named_by_server: false,
                };
                Ok(DraftFile {
                    remote,
                    path: file.path.clone(),
                    version: None,
                    known: Some((file.sha256, file.size)),
                    unpacks_to: None,
                    was: None,
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Draft {
            name: locked.name.clone(),
            reference: source.reference.clone(),
            spec: lock::spec(source),
            extract: source.extract,
            patterns: written(entry),
            resolved: locked.resolved.clone(),
            title: locked.title.clone(),
            license: locked.license.clone(),
            doi: locked.doi.clone(),
            access: locked.access,
            versions: false,
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
                        size: remote.expected_size(),
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

    /// records what the bytes of file `index` turned out to be, and the zip they are when their
    /// source wrapped the file in one.
    pub(super) fn learn(&mut self, index: usize, fetched: &Fetched, cache: &Cache) {
        let file = &mut self.files[index];
        file.known = Some((fetched.digest, fetched.size));
        file.version.clone_from(&fetched.version);
        if file.remote.named_by_server
            && let Some(given) = fetched.name.as_deref().and_then(name::sanitize)
        {
            file.path = given;
        }
        let path = std::mem::take(&mut file.path);
        file.path = fetch::wrapped(cache, &file.remote, path, &fetched.digest);
    }

    /// the entry as the clash check before any download sees it: each file at the path data.lock
    /// would pin, with a stand-in for a hash only a download can learn, and each archive with the
    /// tree this machine recorded for it. a file whose path [`Draft::learn`] may still change,
    /// and an archive this machine never unpacked, stand as archives with no record, so the
    /// check after the downloads takes them.
    pub(super) fn foreseen(&self, cache: &Cache) -> Locked {
        let mut files: Vec<LockedFile> = self
            .files
            .iter()
            .map(|file| {
                let label = format!("{}/{}", self.name, file.path);
                let unrecorded = || Some(stand_in(&format!("{label} unpacked")));
                let unpacks_to = if renamed_by_download(file) {
                    unrecorded()
                } else if self.unpacks(file) {
                    file.unpacks_to
                        .or_else(|| {
                            file.known
                                .and_then(|(sha256, _)| cache.unpacked_tree(&sha256))
                        })
                        .or_else(unrecorded)
                } else {
                    None
                };
                let (sha256, size) = file
                    .known
                    .unwrap_or_else(|| (stand_in(&label), file.remote.size.unwrap_or_default()));
                LockedFile {
                    path: file.path.clone(),
                    size,
                    sha256,
                    md5: file.remote.md5,
                    url: file.remote.url.to_string(),
                    unpacks_to,
                }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Locked {
            name: self.name.clone(),
            reference: self.reference.clone(),
            spec: self.spec.clone(),
            extract: self.extract,
            resolved: self.resolved.clone(),
            title: None,
            license: None,
            doi: None,
            access: self.access,
            timestamp: String::new(),
            files,
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

    /// archives in this dataset that fetchloom leaves packed: the dataset's name, each archive's
    /// path in it and its format.
    pub(super) fn left_packed(&self) -> Vec<(&str, &str, &'static str)> {
        self.files
            .iter()
            .filter(|_| self.extract)
            .filter_map(|file| match archive::by_name(&file.path) {
                Some(Packed::Unsupported(format)) => {
                    Some((self.name.as_str(), file.path.as_str(), format))
                }
                _ => None,
            })
            .collect()
    }

    /// whether the file is an archive that unpacks, by its name and data.toml.
    fn unpacks(&self, file: &DraftFile) -> bool {
        self.extract && archive::by_name(&file.path) == Some(Packed::Archive)
    }

    /// the entry a dry run plans with: [`Draft::finish`], with a stand-in for each hash only a
    /// download can learn and for what each archive not yet unpacked unpacks to. a stand-in
    /// matches no cached object, so planning treats its file as missing and its archive as never
    /// unpacked here. it is never written. a file an upgrade asks about again keeps the pin it
    /// had, since only its bytes could tell a change. also returns the files, as
    /// `<dataset>/<path>`, whose hash is a stand-in.
    pub(super) fn stand_in(mut self, now: &str) -> Result<(Locked, Vec<String>), Error> {
        let extract = self.extract;
        let mut guessed = Vec::new();
        for file in &mut self.files {
            if file.known.is_none() {
                if let Some(was) = &file.was {
                    file.known = Some((was.sha256, was.size));
                    file.path.clone_from(&was.path);
                    file.unpacks_to = was.unpacks_to;
                } else {
                    let label = format!("{}/{}", self.name, file.path);
                    let size = file.remote.size.unwrap_or_default();
                    file.known = Some((stand_in(&label), size));
                    guessed.push(label);
                }
            }
            let label = format!("{}/{}", self.name, file.path);
            let unpacks = extract && archive::by_name(&file.path) == Some(Packed::Archive);
            if unpacks && file.unpacks_to.is_none() {
                file.unpacks_to = Some(stand_in(&format!("{label} unpacked")));
            }
        }
        Ok((self.finish(now)?, guessed))
    }

    /// the entry for data.lock. two files pinned on one path would leave a data.lock no run can
    /// read, as when a source wrapped a file in a zip named like another file it lists.
    pub(super) fn finish(self, now: &str) -> Result<Locked, Error> {
        let patterns = self.patterns;
        let versions = self.versions;
        let mut files: Vec<LockedFile> = self
            .files
            .into_iter()
            .filter_map(|file| {
                let (sha256, size) = file.known?;
                let url = match file.version.as_deref().filter(|_| versions) {
                    Some(version) => source::with_version(&file.remote.url, version),
                    None => file.remote.url,
                };
                Some(LockedFile {
                    path: file.path,
                    size,
                    sha256,
                    md5: file.remote.md5,
                    url: url.to_string(),
                    unpacks_to: file.unpacks_to,
                })
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(pair) = files.windows(2).find(|pair| pair[0].path == pair[1].path) {
            let help = match patterns {
                Patterns::InDataToml => format!(
                    "leave one of them out with an exclude pattern in datasets.{} in data.toml",
                    self.name
                ),
                Patterns::OnCommandLine => "leave one of them out with --exclude".to_owned(),
            };
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "two files of {} would be pinned as {}",
                    self.name, pair[0].path
                ),
            )
            .help(help));
        }
        Ok(Locked {
            name: self.name,
            reference: self.reference,
            spec: self.spec,
            extract: self.extract,
            resolved: self.resolved,
            title: self.title,
            license: self.license,
            doi: self.doi,
            access: self.access,
            timestamp: now.to_owned(),
            files,
        })
    }
}

/// the pin of `locked`, an import, once data.toml switched its `extract` to `source`'s: the same
/// files, with what each archive unpacks to left to learn again.
pub(super) fn switched_import(source: &Source, locked: &Locked) -> Locked {
    let mut switched = locked.clone();
    switched.extract = source.extract;
    for file in &mut switched.files {
        file.unpacks_to = None;
    }
    switched
}

/// fills in what each archive of `locked`, an import pinned whole, unpacks to, for a dry run: as
/// this machine recorded it, or a stand-in. returns the archives, as `<dataset>/<path>`, that a
/// run would unpack to learn it.
pub(super) fn stand_in_trees(locked: &mut Locked, cache: &Cache) -> Vec<String> {
    let mut unknown = Vec::new();
    if !locked.extract {
        return unknown;
    }
    for file in &mut locked.files {
        if file.unpacks_to.is_some() || archive::by_name(&file.path) != Some(Packed::Archive) {
            continue;
        }
        let label = format!("{}/{}", locked.name, file.path);
        file.unpacks_to = Some(cache.unpacked_tree(&file.sha256).unwrap_or_else(|| {
            let tree = stand_in(&format!("{label} unpacked"));
            unknown.push(label);
            tree
        }));
    }
    unknown
}

/// the link data.lock pins for `file` of `dataset`.
pub(super) fn link(dataset: &str, file: &LockedFile) -> Result<Url, Error> {
    Url::parse(&file.url).map_err(|err| {
        Error::new(
            Kind::ProjectInvalid,
            format!(
                "data.lock has a broken link for {dataset}/{}: {err}",
                file.path
            ),
        )
        // `fetchloom lock` keeps a dataset whose entry did not change, link and all
        .help(format!("restore data.lock from git, or run `fetchloom lock --upgrade {dataset}`, which resolves it again and may pin newer data"))
    })
}

/// a hash no bytes have, standing for one only a download can learn, for a dry run or the clash
/// check before any download.
fn stand_in(what: &str) -> Sha256 {
    crate::digest::sha256(format!("fetchloom dry run stand-in for {what}").as_bytes())
}

/// whether downloading the file may still change its path, as [`Draft::learn`] changes it: a
/// server's name for the file replaces a guess from its link, and a source may send it wrapped
/// in a zip.
fn renamed_by_download(file: &DraftFile) -> bool {
    file.known.is_none()
        && (file.remote.named_by_server
            || (source::may_wrap(&file.remote.url) && archive::by_name(&file.path).is_none()))
}

/// the bytes an earlier download of this link proved match the MD5 its source states, and the
/// S3 version they were.
fn stated(cache: &Cache, remote: &Remote) -> Option<crate::cache::Stated> {
    let stated = cache.stated(&remote.md5?, remote.url.as_str())?;
    if remote.sha256.is_some_and(|sha256| sha256 != stated.digest) {
        return None;
    }
    remote
        .size
        .is_none_or(|size| size == stated.size)
        .then_some(stated)
}

/// the file data.lock already pins for this remote file, when it is the same file: the same
/// link, and the same path unless the server names the file or its source wrapped it in a zip.
/// a listed size says nothing about the zip. a pin made without an MD5 says nothing of whether
/// its bytes have the one the source now states, unless the source states their SHA-256 too.
fn carried<'a>(
    by_url: &HashMap<String, Vec<&'a LockedFile>>,
    remote: &Remote,
) -> Option<&'a LockedFile> {
    let candidates = by_url.get(remote.url.as_str())?;
    candidates.iter().copied().find(|file| {
        let wrapped = source::may_wrap(&remote.url) && file.path == format!("{}.zip", remote.path);
        (remote.named_by_server || file.path == remote.path || wrapped)
            && (wrapped || remote.size.is_none_or(|size| size == file.size))
            && remote.sha256.is_none_or(|sha256| sha256 == file.sha256)
            && remote
                .md5
                .is_none_or(|md5| file.md5.map_or(remote.sha256.is_some(), |was| was == md5))
    })
}

/// the files `select` keeps and `exclude` leaves. a select pattern that matches nothing is a
/// mistake worth stopping for, with real paths to compare against.
pub(in crate::cli) fn filter(
    name: &str,
    entry: &Entry,
    source: &Source,
    listing: &Listing,
) -> Result<Vec<Remote>, Error> {
    for glob in &source.select {
        if !listing.paths().any(|path| glob.matches(path)) {
            return Err(unmatched(name, entry, glob, listing));
        }
    }
    let selected = |path: &str| {
        (source.select.is_empty() || source.select.iter().any(|glob| glob.matches(path)))
            && !source.exclude.iter().any(|glob| glob.matches(path))
    };
    let unavailable: Vec<_> = listing
        .unavailable
        .iter()
        .filter(|item| selected(&item.path))
        .collect();
    if !unavailable.is_empty() {
        let has_files = listing.files.iter().any(|file| selected(&file.path));
        return Err(unavailable_selection(
            name,
            &unavailable,
            has_files,
            written(entry),
        ));
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
    patterns: Patterns,
) -> Error {
    let mut error = Error::new(
        Kind::SourceInvalid,
        format!(
            "{name} has {} that fetchloom cannot download",
            units::count(entries.len() as u64, "selected entry", "selected entries")
        ),
    );
    for entry in entries {
        error = error.detail(entry.path.clone());
    }
    let mut reasons = HashSet::new();
    for entry in entries {
        if reasons.insert(entry.reason.as_str()) {
            error = error.detail(entry.reason.clone());
        }
    }
    let flags = if entries.len() <= 5 {
        entries
            .iter()
            .map(|entry| exclusion_flag(&entry.path))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        "an --exclude for each entry above".to_owned()
    };
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
    let change = match patterns {
        Patterns::InDataToml => {
            format!("add exclude patterns in datasets.{name} in data.toml for the entries above")
        }
        Patterns::OnCommandLine => format!("add {flags}"),
    };
    error.help(format!(
        "{change} {purpose}, or ask the owner to add {copy} such as pdf or csv"
    ))
}

fn exclusion_flag(path: &str) -> String {
    let pattern = crate::glob::escape(path);
    // inside double quotes, bash and zsh still expand `!` from the history
    let single =
        pattern.contains(['\'', '"', '$', '\u{60}']) || (!cfg!(windows) && pattern.contains('!'));
    let quoted = if single {
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
        None => error.help(
            "compare the pattern with the paths above, then fix --select or select in data.toml",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::cache::tests::store;
    use crate::digest::{self, Md5};
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
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
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
            extract: true,
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
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
        let kept = Draft::new(
            "set",
            entry,
            entry.source().unwrap(),
            listing(),
            Some(&locked),
            false,
            &cache,
        )
        .unwrap();
        assert_eq!(
            kept.files[0].unpacks_to,
            Some(stale),
            "a relock keeps the pin"
        );
        let upgraded = Draft::new(
            "set",
            entry,
            entry.source().unwrap(),
            listing(),
            Some(&locked),
            true,
            &cache,
        )
        .unwrap();
        assert_eq!(
            upgraded.files[0].unpacks_to,
            Some(recorded),
            "an upgrade pins what this fetchloom unpacks"
        );
    }

    /// a switch of extract keeps every pin data.lock has and learns only what archives unpack
    /// to: from this machine's record, with nothing to download or unpack, and nothing at all
    /// when they stay packed.
    #[test]
    fn an_extract_switch_keeps_the_pins_and_learns_only_trees() {
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
        let entry = |extra: &str| {
            Manifest::parse(&format!(
                "[datasets.set]\nref = \"https://example.org/set.zip\"\n{extra}"
            ))
            .unwrap()
            .datasets["set"]
                .clone()
        };
        let packed = entry("extract = false\n");
        let file = |path: &str, sha256, size| LockedFile {
            path: path.to_owned(),
            size,
            sha256,
            md5: Md5::from_hex("900150983cd24fb0d6963f7d28e17f72"),
            url: format!("https://example.org/{path}"),
            unpacks_to: None,
        };
        let locked = Locked {
            name: "set".to_owned(),
            reference: packed.source().unwrap().reference.clone(),
            spec: lock::spec(packed.source().unwrap()),
            extract: false,
            resolved: "https://example.org/set.zip".to_owned(),
            title: Some("set".to_owned()),
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            files: vec![
                file("notes.txt", digest::sha256(b"notes"), 5),
                file("set.zip", archive.digest, archive.size),
            ],
        };
        let unpacking = entry("");
        let mut draft = Draft::switched(&unpacking, unpacking.source().unwrap(), &locked).unwrap();
        assert!(draft.jobs(&cache).is_empty());
        assert!(draft.unknown_archives(&cache).is_empty());
        let unpacked = draft.finish("2026-02-02T00:00:00Z").unwrap();
        assert!(unpacked.extract);
        assert_eq!(unpacked.files[1].unpacks_to, Some(recorded));
        assert_eq!(
            Locked {
                extract: false,
                timestamp: locked.timestamp.clone(),
                files: locked.files.clone(),
                ..unpacked.clone()
            },
            locked,
            "every other field is as data.lock had it"
        );
        let repacked = Draft::switched(&packed, packed.source().unwrap(), &unpacked)
            .unwrap()
            .finish(&locked.timestamp)
            .unwrap();
        assert_eq!(repacked, locked);

        let mut broken = locked.clone();
        broken.files[1].url = "not a link".to_owned();
        let Err(error) = Draft::switched(&unpacking, unpacking.source().unwrap(), &broken) else {
            panic!("a broken link drafts nothing");
        };
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(
            error
                .message
                .starts_with("data.lock has a broken link for set/set.zip"),
            "{}",
            error.message
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
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
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
            extract: true,
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
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
                entry.source().unwrap(),
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
            extract: true,
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
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
        let by_url = HashMap::from([(locked.files[0].url.clone(), vec![&locked.files[0]])]);
        assert!(carried(&by_url, &remote("900150983cd24fb0d6963f7d28e17f72")).is_some());
        assert!(
            carried(&by_url, &remote("e80b5017098950fc58aad83c8c14978e")).is_none(),
            "the bytes changed under the same link"
        );
    }

    #[test]
    fn an_unknown_md5_in_data_lock_never_vouches_for_a_stated_one() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let url = Url::parse("https://example.org/data.csv").unwrap();
        let manifest =
            Manifest::parse("[datasets.d]\nref = \"https://example.org/data.csv\"\n").unwrap();
        let locked = Locked {
            name: "d".to_owned(),
            reference: url.to_string(),
            spec: String::new(),
            extract: true,
            resolved: url.to_string(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: String::new(),
            files: vec![LockedFile {
                path: "data.csv".to_owned(),
                size: 4,
                sha256: digest::sha256(b"a,b\n"),
                md5: None,
                url: url.to_string(),
                unpacks_to: None,
            }],
        };
        // the same link now states the MD5 of other bytes of the same size
        let stated = crate::digest::Md5::from_hex("e80b5017098950fc58aad83c8c14978e");
        let listing = || Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Url(url.clone()),
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "data.csv".to_owned(),
                size: Some(4),
                sha256: None,
                md5: stated,
                url: url.clone(),
                named_by_server: false,
            }],
        };
        let entry = &manifest.datasets["d"];
        for upgrading in [true, false] {
            let draft = Draft::new(
                "d",
                entry,
                entry.source().unwrap(),
                listing(),
                Some(&locked),
                upgrading,
                &cache,
            )
            .unwrap();
            assert_eq!(draft.files[0].known, None, "upgrading {upgrading}");
            let jobs = draft.jobs(&cache);
            assert_eq!(jobs.len(), 1, "upgrading {upgrading}");
            assert_eq!(jobs[0].1.fetch.expect.md5, stated);
        }
        let by_url = HashMap::from([(locked.files[0].url.clone(), vec![&locked.files[0]])]);
        let mut remote = listing().files.remove(0);
        assert!(carried(&by_url, &remote).is_none());
        remote.sha256 = Some(locked.files[0].sha256);
        assert!(
            carried(&by_url, &remote).is_some(),
            "a stated SHA-256 vouches for the pinned bytes"
        );
    }

    #[test]
    fn a_stated_sha256_outweighs_an_md5_hint_without_a_size() {
        use crate::source::recorded::{Recorded, block_on, fixture_bytes};

        let mut body: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("osf/root.json")).unwrap();
        let size = body["data"][0]["attributes"]["size"].as_u64().unwrap();
        body["data"][0]["attributes"]["size"] = serde_json::Value::Null;
        let recorded = Recorded::default()
            .fixture(
                "https://api.osf.io/v2/nodes/ezcuj/?embed=license&embed=identifiers&related_counts=children",
                "osf/embedded.json",
            )
            .on(
                "https://api.osf.io/v2/nodes/ezcuj/files/osfstorage/?page%5Bsize%5D=100",
                200,
                serde_json::to_vec(&body).unwrap(),
            );
        let reference = Reference::parse("osf:ezcuj").unwrap().reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        let remote = &listing.files[0];
        assert_eq!(remote.size, None);
        let previous = remote.sha256.unwrap();
        let md5 = remote.md5.unwrap();
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        cache
            .save_stated_md5(
                &md5,
                remote.url.as_str(),
                &crate::cache::Stated {
                    digest: previous,
                    size,
                    version: None,
                },
            )
            .unwrap();
        let manifest = Manifest::parse("[datasets.d]\nref = \"osf:ezcuj\"\n").unwrap();
        let changed = digest::sha256(b"changed bytes");
        for (sha256, listed_size, known) in [
            (Some(previous), None, Some((previous, size))),
            (None, None, Some((previous, size))),
            (Some(changed), Some(13), Some((changed, 13))),
            (Some(changed), None, None),
            (None, Some(13), None),
        ] {
            for upgrading in [false, true] {
                let mut listing = listing.clone();
                listing.files[0].sha256 = sha256;
                listing.files[0].size = listed_size;
                let draft = Draft::new(
                    "d",
                    &manifest.datasets["d"],
                    manifest.datasets["d"].source().unwrap(),
                    listing,
                    None,
                    upgrading,
                    &cache,
                )
                .unwrap();
                assert_eq!(draft.files[0].known, known, "{sha256:?}, {listed_size:?}");
                let jobs = draft.jobs(&cache);
                assert_eq!(jobs.len(), usize::from(known.is_none()));
                if let Some((_, job)) = jobs.first() {
                    assert_eq!(job.fetch.expect.sha256, sha256);
                    assert_eq!(job.fetch.expect.md5, Some(md5));
                    assert_eq!(job.fetch.expect.size, listed_size);
                }
            }
        }
        assert_eq!(recorded.asked().len(), 2);
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
            manifest.datasets["sample"].source().unwrap(),
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
                moved: stored.size,
                name: Some("other.txt".to_owned()),
                origin: crate::net::Origin::Network,
                version: None,
            },
            &cache,
        );
        let locked = draft.finish("2026-10-29T00:00:00Z").unwrap();
        assert_eq!(locked.files.len(), 1);
        assert_eq!(locked.files[0].path, "file.txt");
        assert_eq!(locked.files[0].size, 4);
        assert_eq!(locked.files[0].sha256, stored.digest);
    }

    /// an S3 object is pinned at the version its bytes were: learned from its download, or from
    /// what an earlier download of the link recorded, and kept by a later lock, which finds the
    /// pin by the link the listing gives. a plain link keeps its link whatever its server says.
    #[test]
    fn an_s3_object_is_pinned_at_the_version_its_bytes_were() {
        use md5::Digest as _;

        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let stored = store(&cache, b"a,b\n");
        let md5 = Md5::from_hex(&digest::hex(&md5::Md5::digest(b"a,b\n"))).unwrap();
        let now = "2026-11-11T00:00:00Z";
        let fetched = |version: &str| Fetched {
            digest: stored.digest,
            size: stored.size,
            moved: stored.size,
            name: None,
            origin: crate::net::Origin::Network,
            version: Some(version.to_owned()),
        };
        let listing = |reference: &str, link: &Url, md5: Option<Md5>| Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::parse(reference).unwrap().reference,
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "a.csv".to_owned(),
                size: Some(stored.size),
                sha256: None,
                md5,
                url: link.clone(),
                named_by_server: false,
            }],
        };
        let manifest = Manifest::parse("[datasets.raw]\nref = \"s3://lab-raw/raw/\"\n").unwrap();
        let entry = &manifest.datasets["raw"];
        let link = Url::parse("https://lab-raw.s3.amazonaws.com/raw/a.csv").unwrap();
        let bucket = || listing("s3://lab-raw/raw/", &link, None);
        let raw = |listing: Listing, previous: Option<&Locked>| {
            Draft::new(
                "raw",
                entry,
                entry.source().unwrap(),
                listing,
                previous,
                false,
                &cache,
            )
            .unwrap()
        };
        let mut draft = raw(bucket(), None);
        assert_eq!(draft.jobs(&cache).len(), 1);
        draft.learn(0, &fetched("v+1/2"), &cache);
        let locked = draft.finish(now).unwrap();
        assert_eq!(
            locked.files[0].url,
            "https://lab-raw.s3.amazonaws.com/raw/a.csv?versionId=v%2B1%2F2"
        );
        let again = raw(bucket(), Some(&locked));
        assert!(again.jobs(&cache).is_empty());
        assert_eq!(again.finish(now).unwrap().files[0].url, locked.files[0].url);

        let stated = crate::cache::Stated {
            digest: stored.digest,
            size: stored.size,
            version: Some("v2".to_owned()),
        };
        cache.save_stated_md5(&md5, link.as_str(), &stated).unwrap();
        let draft = raw(listing("s3://lab-raw/raw/", &link, Some(md5)), None);
        assert!(draft.jobs(&cache).is_empty());
        assert_eq!(
            draft.finish(now).unwrap().files[0].url,
            "https://lab-raw.s3.amazonaws.com/raw/a.csv?versionId=v2"
        );

        let plain = Url::parse("https://example.org/a.csv").unwrap();
        let manifest =
            Manifest::parse("[datasets.a]\nref = \"https://example.org/a.csv\"\n").unwrap();
        let listed = listing("https://example.org/a.csv", &plain, None);
        let mut draft = Draft::new(
            "a",
            &manifest.datasets["a"],
            manifest.datasets["a"].source().unwrap(),
            listed,
            None,
            false,
            &cache,
        )
        .unwrap();
        draft.learn(0, &fetched("v3"), &cache);
        assert_eq!(draft.finish(now).unwrap().files[0].url, plain.as_str());
    }

    /// what kaggle's zip of train.csv in `kaggle/train.csv.zip` holds.
    const TRAIN: &[u8] = b"PassengerId,Survived,Pclass\n1,0,3\n2,1,1\n3,1,3\n";

    const KAGGLE: &str = "https://api.kaggle.com/v1/competitions";

    /// titanic listed for an account that accepted its rules, with the sizes of the files
    /// themselves for the bytes these tests use, as kaggle lists them.
    fn titanic() -> Listing {
        use crate::source::recorded::{Recorded, block_on, fixture_bytes};

        let json = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&fixture_bytes(name)).unwrap()
        };
        let mut about = json("kaggle/titanic-competition.json");
        about["userHasEntered"] = true.into();
        let mut files = json("kaggle/titanic-files.json");
        files["files"][1]["totalBytes"] = fixture_bytes("kaggle/test.csv").len().into();
        files["files"][2]["totalBytes"] = TRAIN.len().into();
        let recorded = Recorded::default()
            .on(
                &format!("{KAGGLE}/get/titanic"),
                200,
                serde_json::to_vec(&about).unwrap(),
            )
            .on(
                &format!("{KAGGLE}/data/list/titanic?pageSize=200"),
                200,
                serde_json::to_vec(&files).unwrap(),
            );
        let reference = Reference::parse("kaggle:competitions/titanic")
            .unwrap()
            .reference;
        block_on(reference.resolve(&recorded)).unwrap()
    }

    /// where a dataset's locked files land, each as its path, hash and size.
    fn placed(cache: &Cache, files: &[LockedFile]) -> Vec<(String, Sha256, u64)> {
        let mut planned = crate::plan::Plan::default();
        let members = |file: &LockedFile| cache.unpacked(&file.sha256, file.unpacks_to.as_ref());
        crate::plan::expand(
            &mut planned,
            "titanic",
            "data/titanic",
            false,
            files,
            &members,
        );
        assert_eq!(planned.unknown.len(), 0, "every archive's files are known");
        planned
            .targets
            .iter()
            .map(|target| (target.path.clone(), target.sha256, target.size))
            .collect()
    }

    #[test]
    fn a_competition_file_sent_wrapped_in_a_zip_unpacks_and_syncs_again_alike() {
        use crate::source::recorded::fixture_bytes;

        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let manifest = Manifest::parse(
            "[datasets.titanic]\nref = \"kaggle:competitions/titanic\"\nselect = [\"t*.csv\"]\n",
        )
        .unwrap();
        let entry = &manifest.datasets["titanic"];

        let mut draft = Draft::new(
            "titanic",
            entry,
            entry.source().unwrap(),
            titanic(),
            None,
            false,
            &cache,
        )
        .unwrap();
        let jobs = draft.jobs(&cache);
        let labels: Vec<&str> = jobs.iter().map(|(_, job)| job.label.as_str()).collect();
        assert_eq!(labels, ["titanic/test.csv", "titanic/train.csv"]);
        assert!(
            jobs.iter().all(|(_, job)| job.fetch.expect.size.is_none()),
            "a listed size is no expectation for bytes kaggle may zip"
        );
        let indexes: Vec<usize> = jobs.iter().map(|(index, _)| index.unwrap()).collect();
        // kaggle sent test.csv as it is and train.csv as a zip of it
        let plain = store(&cache, &fixture_bytes("kaggle/test.csv"));
        let wrapped = store(&cache, &fixture_bytes("kaggle/train.csv.zip"));
        for (index, stored) in indexes.into_iter().zip([&plain, &wrapped]) {
            let fetched = Fetched {
                digest: stored.digest,
                size: stored.size,
                moved: stored.size,
                name: None,
                origin: crate::net::Origin::Network,
                version: None,
            };
            draft.learn(index, &fetched, &cache);
        }
        for todo in draft.unknown_archives(&cache) {
            let unpacked =
                archive::unpack_cached(&cache, &todo.archive, todo.size, None, &|_| false, None)
                    .unwrap()
                    .unwrap();
            draft.unpacked(todo.file, Some(unpacked.tree));
        }
        let locked = draft.finish("2026-11-01T00:00:00Z").unwrap();
        let pinned: Vec<(&str, Sha256, bool)> = locked
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.sha256, file.unpacks_to.is_some()))
            .collect();
        assert_eq!(
            pinned,
            [
                ("test.csv", plain.digest, false),
                ("train.csv.zip", wrapped.digest, true),
            ]
        );
        assert_eq!(
            locked.files[1].url,
            format!("{KAGGLE}/data/download/titanic/train.csv")
        );
        let expected = vec![
            ("data/titanic/test.csv".to_owned(), plain.digest, plain.size),
            (
                "data/titanic/train.csv".to_owned(),
                digest::sha256(TRAIN),
                TRAIN.len() as u64,
            ),
        ];
        assert_eq!(placed(&cache, &locked.files), expected);
        assert!(cache.find(&digest::sha256(TRAIN), None).is_some());

        // a sync once the zip's own bytes are dropped, and a relock from data.lock, need no
        // download and pin the same
        cache.drop_object(&wrapped.digest).unwrap();
        assert_eq!(placed(&cache, &locked.files), expected);
        let again = Draft::new(
            "titanic",
            entry,
            entry.source().unwrap(),
            titanic(),
            Some(&locked),
            false,
            &cache,
        )
        .unwrap();
        assert_eq!(again.jobs(&cache).len(), 0, "nothing to download");
        assert_eq!(
            again.finish("2026-11-01T00:00:00Z").unwrap().files,
            locked.files
        );
    }
}

#[cfg(test)]
mod drive_docs_tests;
