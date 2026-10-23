//! The commands, and the pieces most of them share: the context they run in, how a locked file
//! compares with the data directory, and how wanted files are fetched.

pub(crate) mod add;
pub(crate) mod get;
pub(crate) mod init;
pub(crate) mod lock;
pub(crate) mod path;
pub(crate) mod remove;
pub(crate) mod restore;
pub(crate) mod status;
pub(crate) mod sync;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use fl_model::digest::Digest;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use fl_model::lock::{LockedDataset, LockedFile};
use fl_model::manifest::{Link, Manifest};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target, UrlScheme};
use fl_model::suggest::closest;
use fl_model::units::size;
use fl_sources::Entry;
use fl_store::fingerprint::{self, Fingerprint};
use fl_transfer::url::SafeUrl;

use crate::ask::Ask;
use crate::engine::{Engine, Got, Want};
use crate::failure::Failure;
use crate::project::{Project, State};
use crate::report::Reporter;

/// Above this many bytes to download, a dataset is only fetched once the user agrees.
pub(crate) const LARGE: u64 = 1 << 30;

/// The parts a command works with.
pub(crate) struct Context<'a> {
    /// Resolving, fetching and linking.
    pub(crate) engine: &'a Engine,
    /// Where events go.
    pub(crate) reporter: &'a dyn Reporter,
    /// Where the command runs.
    pub(crate) cwd: &'a Path,
    /// Who answers questions.
    pub(crate) ask: &'a dyn Ask,
    /// `--yes` answers every question.
    pub(crate) yes: bool,
    /// No network at all.
    pub(crate) offline: bool,
    /// How files are placed when a dataset does not say.
    pub(crate) link: Link,
}

impl Context<'_> {
    fn emit(&self, event: Event) {
        self.reporter.emit(event);
    }

    /// Takes the project lock, saying so when another `fl` holds it.
    pub(crate) fn hold(&self, project: &Project) -> Result<crate::project::Held, Failure> {
        project.hold(|| {
            self.emit(Event::Warning(
                "another fl is changing this project, waiting for it".to_owned(),
            ));
        })
    }

    /// Goes on when the user agrees to `question`, or gave `--yes`. Without a terminal and
    /// without `--yes` it stops, saying `why` it would have asked.
    pub(crate) fn agree(&self, question: &str, why: &str, doing: &str) -> Result<(), Failure> {
        if self.yes {
            return Ok(());
        }
        let refusal = Failure::new(
            ErrorKind::Usage,
            format!("{why}, and fetchloom asks first, but there is no terminal to ask on"),
        )
        .help("run the same command with --yes to go ahead");
        if self.ask.confirm(question, refusal)? {
            Ok(())
        } else {
            Err(Failure::new(
                ErrorKind::Usage,
                format!("stopped as asked, before {doing}; nothing was changed"),
            )
            .help("run the same command with --yes to go ahead"))
        }
    }

    /// Asks before downloading `bytes` of `dataset` when they are more than [`LARGE`].
    pub(crate) fn agree_to_download(&self, dataset: &Name, bytes: u64) -> Result<(), Failure> {
        if bytes <= LARGE {
            return Ok(());
        }
        self.agree(
            &format!(
                "Dataset `{dataset}` is {} to download. Download it?",
                size(bytes)
            ),
            &format!("dataset `{dataset}` is {} to download", size(bytes)),
            &format!("downloading {} of {dataset}", size(bytes)),
        )
    }
}

/// The dataset of `manifest` called `wanted`, or a usage error that suggests the closest name.
pub(crate) fn dataset_named(manifest: &Manifest, wanted: &str) -> Result<Name, Failure> {
    if let Some(name) = Name::new(wanted)
        .ok()
        .filter(|name| manifest.datasets.contains_key(name))
    {
        return Ok(name);
    }
    let known: Vec<&str> = manifest.datasets.keys().map(Name::as_str).collect();
    let failure = Failure::new(
        ErrorKind::Usage,
        format!("data.toml has no dataset `{wanted}`"),
    )
    .subject(wanted);
    Err(match closest(wanted, known.iter().copied()) {
        Some(near) => failure.help(format!("did you mean `{near}`?")),
        None if known.is_empty() => failure.help("add one with `fl add REF`"),
        None => failure.help(format!("datasets here: {}", known.join(", "))),
    })
}

/// The datasets of `manifest` called `wanted`, each checked.
pub(crate) fn datasets_named(manifest: &Manifest, wanted: &[String]) -> Result<Vec<Name>, Failure> {
    wanted
        .iter()
        .map(|name| dataset_named(manifest, name))
        .collect()
}

/// The state of one locked file in the data directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    /// Placed and unchanged since.
    Clean,
    /// Not there.
    Missing,
    /// There, with other bytes than the lock records.
    Modified,
}

/// One locked file and where it belongs.
pub(crate) struct Slot<'l> {
    /// Its path relative to the data directory, as the state records it.
    pub(crate) key: String,
    /// Its path relative to the project root, for messages.
    pub(crate) shown: String,
    /// The file as locked.
    pub(crate) file: &'l LockedFile,
    /// What is there now.
    pub(crate) found: Found,
    /// Its fingerprint now, when it is there.
    pub(crate) print: Option<Fingerprint>,
}

/// How one dataset's directory compares with the lock.
pub(crate) struct Survey<'l> {
    /// The locked files.
    pub(crate) slots: Vec<Slot<'l>>,
    /// Files in the directory that the lock does not list and fetchloom did not place, by
    /// their path relative to the data directory.
    pub(crate) extras: Vec<String>,
    /// Files fetchloom placed, unchanged since, that the lock no longer lists, by the same
    /// paths. A placed file changed by hand counts as an extra.
    pub(crate) stale: Vec<String>,
    /// How many files were rehashed because their fingerprint could not be trusted, and found
    /// unchanged.
    pub(crate) confirmed: usize,
}

/// Compares every file of `locked` with the data directory at `root`, rehashing a file only
/// when its fingerprint cannot be trusted.
pub(crate) fn survey<'l>(
    context: &Context<'_>,
    root: &Path,
    shown_root: &str,
    dir: &DataPath,
    locked: &'l LockedDataset,
    state: &State,
) -> Result<Survey<'l>, Failure> {
    let on_disk = root.join(dir.as_str());
    let mut present: BTreeMap<String, Fingerprint> = if on_disk.is_dir() {
        fingerprint::scan(&on_disk)?.into_iter().collect()
    } else {
        BTreeMap::new()
    };
    let mut slots = Vec::new();
    let mut confirmed = 0;
    for file in locked.files.iter().flatten() {
        let key = format!("{dir}/{}", file.path);
        let print = present.remove(file.path.as_str());
        let found = match print {
            None => Found::Missing,
            Some(print) if state.trusts(&key, &file.blake3, &print) => Found::Clean,
            Some(_) => {
                context.engine.rehashed();
                let path = on_disk.join(file.path.as_str());
                if fingerprint::hash_file(&path)? == Digest::Blake3(file.blake3) {
                    confirmed += 1;
                    Found::Clean
                } else {
                    Found::Modified
                }
            }
        };
        slots.push(Slot {
            shown: format!("{shown_root}/{key}"),
            key,
            file,
            found,
            print,
        });
    }
    let mut dropped = Vec::new();
    let mut extras = Vec::new();
    for path in present.into_keys() {
        let key = format!("{dir}/{path}");
        if state.files.contains_key(&key) && state.untouched(&key, &on_disk.join(&path))? {
            dropped.push(key);
        } else {
            extras.push(key);
        }
    }
    Ok(Survey {
        slots,
        extras,
        stale: dropped,
        confirmed,
    })
}

/// The directory a dataset lands in under the data directory.
pub(crate) fn dataset_dir(manifest: &Manifest, name: &Name) -> Result<DataPath, Failure> {
    match manifest
        .datasets
        .get(name)
        .and_then(|spec| spec.path.clone())
    {
        Some(path) => Ok(path),
        None => DataPath::new(name.as_str())
            .map_err(|error| Failure::new(ErrorKind::ProjectInvalidManifest, error.to_string())),
    }
}

/// Whether `reference` is a URL the user wrote, whose host may be private.
pub(crate) fn is_typed_url(reference: &Reference) -> bool {
    matches!(
        reference.target(),
        Target::Url {
            scheme: UrlScheme::Http | UrlScheme::Https,
            ..
        }
    )
}

/// The file of a listing as wanted in the store, its hash known when `known` holds its URL.
pub(crate) fn want(
    entry: &Entry,
    known: &HashMap<&str, [u8; 32]>,
    typed: bool,
) -> Result<Want, Failure> {
    let url = entry.urls.first().ok_or_else(|| {
        Failure::new(
            ErrorKind::ReferenceUnresolved,
            format!("the source names no location for `{}`", entry.path),
        )
    })?;
    Ok(Want {
        url: url.clone(),
        size: entry.size,
        stated: entry.stated.clone(),
        blake3: known.get(url.written().as_str()).copied(),
        typed,
    })
}

/// The locked file as wanted in the store, from its location number `at` in the lock.
pub(crate) fn locked_want(
    file: &LockedFile,
    at: usize,
    local: bool,
) -> Result<Option<Want>, Failure> {
    let Some(location) = file.at.get(at) else {
        return Ok(None);
    };
    let url = SafeUrl::parse(location).map_err(|error| {
        Failure::new(
            ErrorKind::ProjectInvalidLock,
            format!("data.lock holds `{location}` for `{}`: {error}", file.path),
        )
    })?;
    if !local && url.to_file_path().is_some() {
        return Err(Failure::new(
            ErrorKind::ProjectInvalidLock,
            format!(
                "data.lock points `{}` at a file on this machine, `{location}`, but its dataset is not a local folder",
                file.path
            ),
        )
        .help("run `fl lock --upgrade` to resolve the dataset again"));
    }
    let mut stated = Vec::new();
    if let Some(md5) = file.md5 {
        stated.push(Digest::Md5(md5));
    }
    if let Some(sha1) = file.sha1 {
        stated.push(Digest::Sha1(sha1));
    }
    if let Some(sha256) = file.sha256 {
        stated.push(Digest::Sha256(sha256));
    }
    Ok(Some(Want {
        url,
        size: Some(file.size),
        stated,
        blake3: Some(file.blake3),
        typed: false,
    }))
}

/// Fetches `wants` for `dataset`, says how much came over the network, and returns every
/// result in order.
pub(crate) async fn fetch_all(
    context: &Context<'_>,
    dataset: &Name,
    wants: Vec<Want>,
) -> Vec<Result<Got, Failure>> {
    let started = Instant::now();
    let results = context.engine.fetch(dataset.as_str(), wants).await;
    let fetched: Vec<u64> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .map(|got| got.fetched)
        .filter(|bytes| *bytes > 0)
        .collect();
    if !fetched.is_empty() {
        context.emit(Event::Fetched {
            dataset: dataset.clone(),
            files: fetched.len() as u64,
            bytes: fetched.iter().sum(),
            elapsed: started.elapsed(),
        });
    }
    results
}

/// Fetches `wants` for `dataset`, and stops at the first failure after reporting the others.
pub(crate) async fn fetch_every(
    context: &Context<'_>,
    dataset: &Name,
    wants: Vec<Want>,
) -> Result<Vec<Got>, Failure> {
    let mut gots = Vec::with_capacity(wants.len());
    let mut failures = Vec::new();
    for result in fetch_all(context, dataset, wants).await {
        match result {
            Ok(got) => gots.push(got),
            Err(failure) => failures.push(failure),
        }
    }
    match worst(context, failures) {
        Some(failure) => Err(failure),
        None => Ok(gots),
    }
}

/// Reports every failure but the most severe one, which is returned: the one with the highest
/// exit code, the first of those.
pub(crate) fn worst(context: &Context<'_>, failures: Vec<Failure>) -> Option<Failure> {
    let index = failures
        .iter()
        .enumerate()
        .max_by_key(|(index, failure)| (failure.kind().exit_code(), std::cmp::Reverse(*index)))
        .map(|(index, _)| index)?;
    let mut failures = failures;
    let first = failures.remove(index);
    for failure in failures {
        context.emit(Event::Failed(failure.into_problem()));
    }
    Some(first)
}

/// `n` and the noun that fits it.
pub(crate) fn plural(n: u64, one: &str, many: &str) -> String {
    fl_model::units::count(n, one, many).to_string()
}

/// At most `shown` of `items` joined with commas, and how many more there are.
pub(crate) fn some_of(items: &[String], shown: usize) -> String {
    let mut text = items
        .iter()
        .take(shown)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > shown {
        let _ = write!(text, " and {} more", items.len() - shown);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn some_of_names_a_few_and_counts_the_rest() {
        let items: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(some_of(&items, 5), "a, b, c");
        assert_eq!(some_of(&items, 2), "a, b and 1 more");
        assert_eq!(some_of(&[], 2), "");
    }
}
