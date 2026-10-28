//! the loop at the heart of a project: resolve what data.toml changed, learn every file's hash,
//! put what the lock names in the cache, then write data.lock.

mod draft;
pub(super) mod place;
pub(super) mod unpack;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use tokio::task::JoinSet;
use url::Url;

use self::draft::Draft;
use self::place::{Placement, Placing};
use super::Context;
use super::fetch::{self, Again, Job};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch, Fetched, Net, Origin};
use crate::output::Output;
use crate::project::Project;
use crate::project::lock::{self, Lock, Locked, LockedFile};
use crate::project::manifest::{Entry, Manifest};
use crate::source::{self, Listing, Reference, Remote};
use crate::units;

/// the most of a title the `Resolved` line shows.
const TITLE: usize = 50;

/// which datasets move to their newest version.
pub(super) enum Upgrade {
    Nothing,
    All,
    Only(BTreeSet<String>),
}

impl Upgrade {
    fn includes(&self, name: &str) -> bool {
        match self {
            Upgrade::Nothing => false,
            Upgrade::All => true,
            Upgrade::Only(names) => names.contains(name),
        }
    }
}

/// a project and what working on it needs.
pub(super) struct Loop<'a> {
    pub(super) context: &'a Context,
    pub(super) project: &'a Project,
    pub(super) net: Arc<Net>,
    pub(super) cache: Cache,
    /// archives this run unpacked, whose own bytes the cache may drop once their files are in.
    unpacked: Mutex<Vec<Sha256>>,
}

impl<'a> Loop<'a> {
    pub(super) fn new(
        context: &'a Context,
        project: &'a Project,
        net: Arc<Net>,
        cache: Cache,
    ) -> Self {
        Loop {
            context,
            project,
            net,
            cache,
            unpacked: Mutex::default(),
        }
    }

    /// makes the data folder match `lock`. what each file needs is decided first, so only the
    /// bytes placing lacks are downloaded. archives this machine never unpacked are unpacked
    /// first, since what they hold decides where files go.
    pub(super) async fn place(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        force: bool,
    ) -> Result<Placement, Error> {
        let mut placing = Placing::new(self.project, manifest, lock, &self.cache, force)?;
        let unknown = placing.unknown();
        if !unknown.is_empty() {
            self.unpack_locked(lock, &unknown).await?;
            placing = Placing::new(self.project, manifest, lock, &self.cache, force)?;
        }
        let (archives, files): (Vec<_>, Vec<_>) = placing
            .missing(&self.cache)
            .into_iter()
            .partition(|(_, file)| file.unpacks_to.is_some());
        let jobs = files
            .into_iter()
            .map(|(dataset, file)| job(lock, &dataset, file))
            .collect::<Result<Vec<_>, _>>()?;
        self.download(jobs).await?;
        self.unpack_locked(lock, &archives).await?;
        placing.apply(&self.context.output, &self.context.work, &self.cache)
    }

    /// the lock data.toml asks for. datasets whose entry changed are resolved again, and files
    /// whose hash no source states are downloaded to learn it. `given` is a listing already
    /// resolved for one dataset.
    pub(super) async fn relock(
        &self,
        manifest: &Manifest,
        old: &Lock,
        upgrade: &Upgrade,
        given: Option<(String, Listing)>,
    ) -> Result<Lock, Error> {
        let mut kept: Vec<Locked> = Vec::new();
        let mut asks = Vec::new();
        for (name, entry) in &manifest.datasets {
            let previous = old.get(name);
            match previous {
                Some(locked) if locked.spec == lock::spec(entry) && !upgrade.includes(name) => {
                    kept.push(locked.clone());
                }
                _ => asks.push((
                    name.clone(),
                    pinned(entry, previous, upgrade.includes(name)),
                )),
            }
        }
        let listings = self.resolve_all(manifest, asks, given).await?;
        let mut drafts = Vec::with_capacity(listings.len());
        for (name, listing) in listings {
            drafts.push(Draft::new(
                &name,
                &manifest.datasets[&name],
                listing,
                old.get(&name),
                upgrade.includes(&name),
                &self.cache,
            )?);
        }
        let mut jobs = Vec::new();
        let mut slots = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            for (file, job) in draft.jobs(&self.cache) {
                slots.push((index, file));
                jobs.push(job);
            }
        }
        let fetched = self.download(jobs).await?;
        for ((draft, file), fetched) in slots.into_iter().zip(fetched) {
            if let Some(file) = file {
                drafts[draft].learn(file, &fetched);
            }
        }
        self.unpack_drafts(&mut drafts).await?;
        let now = lock::timestamp(SystemTime::now());
        let mut datasets = kept;
        datasets.extend(drafts.into_iter().map(|draft| {
            let mut locked = draft.finish(&now);
            // a pin dates from when its data last changed, so resolving the same data again
            // leaves data.lock as it was
            if let Some(before) = old.get(&locked.name)
                && before.resolved == locked.resolved
                && before.files == locked.files
            {
                locked.timestamp.clone_from(&before.timestamp);
            }
            locked
        }));
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Lock { datasets })
    }

    /// writes data.lock when it changed, and says which datasets it pins anew or no longer.
    pub(super) fn write_lock(&self, old: Option<&Lock>, lock: &Lock) -> Result<bool, Error> {
        if old == Some(lock) {
            return Ok(false);
        }
        self.project.write(lock::FILE, lock.render().as_bytes())?;
        let output = &self.context.output;
        let pinned: Vec<&Locked> = lock
            .datasets
            .iter()
            .filter(|dataset| old.and_then(|old| old.get(&dataset.name)) != Some(*dataset))
            .collect();
        let files: usize = pinned.iter().map(|dataset| dataset.files.len()).sum();
        let which = match pinned.as_slice() {
            [] => None,
            [only] => Some(only.name.clone()),
            many => Some(units::count(many.len() as u64, "dataset", "datasets")),
        };
        if let Some(which) = which {
            output.step(
                "Locked",
                &format!(
                    "{which} · {} in {}",
                    units::count(files as u64, "file", "files"),
                    lock::FILE
                ),
            );
        }
        let dropped = old
            .iter()
            .flat_map(|old| &old.datasets)
            .filter(|dataset| lock.get(&dataset.name).is_none());
        for dataset in dropped {
            output.step("Removed", &format!("{} from {}", dataset.name, lock::FILE));
        }
        Ok(true)
    }

    async fn resolve_all(
        &self,
        manifest: &Manifest,
        asks: Vec<(String, String)>,
        given: Option<(String, Listing)>,
    ) -> Result<Vec<(String, Listing)>, Error> {
        let mut listings = Vec::with_capacity(asks.len());
        let mut tasks = JoinSet::new();
        for (name, text) in asks {
            if let Some((given_name, listing)) = &given
                && *given_name == name
            {
                listings.push((name, listing.clone()));
                continue;
            }
            if self.net.is_offline() {
                return Err(Error::new(Kind::OfflineMissing, format!("offline, and {name} must be resolved because its entry in data.toml changed"))
                    .help("run the same command without --offline, or undo the change in data.toml"));
            }
            let entry = &manifest.datasets[&name];
            let parsed = Reference::parse(&text).map_err(|err| cite(err, entry, "ref"))?;
            let net = self.net.clone();
            tasks.spawn(async move {
                let listing = parsed.reference.resolve(&*net).await;
                (name, listing)
            });
        }
        while let Some(joined) = tasks.join_next().await {
            let (name, listing) = joined.map_err(|err| {
                Error::new(
                    Kind::LocalIo,
                    format!("resolving stopped unexpectedly: {err}"),
                )
            })?;
            let listing = listing?;
            tell_resolved(&self.context.output, &listing);
            listings.push((name, listing));
        }
        listings.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(listings)
    }

    /// puts every job's file in the cache, asking first when a lot is about to download.
    async fn download(&self, jobs: Vec<Job>) -> Result<Vec<Fetched>, Error> {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        if self.net.is_offline() {
            let missing: Vec<&str> = jobs
                .iter()
                .filter(|job| !in_cache(&self.cache, &job.fetch.expect))
                .map(|job| job.label.as_str())
                .collect();
            if !missing.is_empty() {
                let mut error = Error::new(
                    Kind::OfflineMissing,
                    format!(
                        "offline, and {} the lock names {} not in the cache",
                        units::count(missing.len() as u64, "file", "files"),
                        if missing.len() == 1 { "is" } else { "are" }
                    ),
                );
                for label in missing.iter().take(5) {
                    error = error.detail((*label).to_owned());
                }
                if missing.len() > 5 {
                    error = error.detail(format!("and {} more", missing.len() - 5));
                }
                return Err(error.help("run the same command without --offline to download them"));
            }
        }
        let size: u64 = jobs
            .iter()
            .filter(|job| !in_cache(&self.cache, &job.fetch.expect))
            .filter_map(|job| job.fetch.expect.size)
            .sum();
        super::confirm(
            &self.context.output,
            self.context.yes,
            &units::count(jobs.len() as u64, "file", "files"),
            size,
        )?;
        let started = Instant::now();
        let fetched = fetch::all(self.context, &self.net, &self.cache, jobs).await?;
        let downloaded: Vec<&Fetched> = fetched
            .iter()
            .filter(|fetched| fetched.origin == Origin::Network)
            .collect();
        if !downloaded.is_empty() {
            let bytes: u64 = downloaded.iter().map(|fetched| fetched.size).sum();
            let elapsed = started.elapsed();
            let mut text = format!(
                "{} · {} in {}",
                units::count(downloaded.len() as u64, "file", "files"),
                units::size(bytes),
                units::duration(elapsed)
            );
            if elapsed.as_secs() >= 1 {
                let _ = write!(text, " · {}", units::rate(bytes, elapsed));
            }
            self.context.output.step("Downloaded", &text);
        }
        Ok(fetched)
    }
}

/// the files a dataset's patterns keep from its listing, or why the patterns are wrong.
pub(super) fn filter(name: &str, entry: &Entry, listing: &Listing) -> Result<Vec<Remote>, Error> {
    draft::filter(name, entry, listing)
}

/// what to resolve for a dataset whose entry changed. a changed filter resolves the version
/// already locked, so tidying data.toml never moves data. a new ref, or an upgrade, asks for the
/// newest.
fn pinned(entry: &Entry, previous: Option<&Locked>, upgrading: bool) -> String {
    match previous {
        Some(locked) if !upgrading && locked.reference == entry.reference => {
            locked.resolved.clone()
        }
        _ => entry.reference.clone(),
    }
}

/// the download of one locked file. when its recorded link is dead, the source is asked again
/// at the version the lock pins.
fn job(lock: &Lock, dataset: &str, file: &LockedFile) -> Result<Job, Error> {
    let url = Url::parse(&file.url).map_err(|err| {
        Error::new(
            Kind::ProjectInvalid,
            format!(
                "data.lock has a broken link for {dataset}/{}: {err}",
                file.path
            ),
        )
        .help("run `fetchloom lock` to write it again")
    })?;
    let pinned = lock.datasets.iter().find(|locked| locked.name == dataset);
    let (from, divert) = source::fetch_plan(&url);
    Ok(Job {
        fetch: Fetch {
            url,
            from,
            divert,
            expect: Expect {
                sha256: Some(file.sha256),
                md5: file.md5,
                size: Some(file.size),
            },
            revalidate: false,
            unpack: false,
        },
        label: format!("{dataset}/{}", file.path),
        dataset: Some(dataset.to_owned()),
        again: pinned.map(|locked| Again {
            resolved: locked.resolved.clone(),
            path: file.path.clone(),
            alone: locked.files.len() == 1,
        }),
    })
}

fn in_cache(cache: &Cache, expect: &Expect) -> bool {
    match (expect.sha256, expect.md5) {
        (Some(digest), _) => cache.find(&digest, expect.size).is_some(),
        (None, Some(md5)) => cache
            .stated_md5(&md5)
            .is_some_and(|(digest, size)| cache.find(&digest, Some(size)).is_some()),
        (None, None) => false,
    }
}

/// what a license id forbids, for the licenses that forbid something people often assume.
fn restriction(license: &str) -> Option<&'static str> {
    let upper = license.to_ascii_uppercase();
    if upper.contains("-NC") {
        Some("commercial use")
    } else if upper.contains("-ND") {
        Some("changed copies")
    } else {
        None
    }
}

/// points an error at the data.toml line it comes from.
fn cite(error: Error, entry: &Entry, key: &str) -> Error {
    match entry.lines.get(key) {
        Some(line) => error.detail(line.clone()),
        None => error,
    }
}
/// prints what a reference resolved to, before any file of it downloads.
pub(super) fn tell_resolved(output: &Output, listing: &Listing) {
    // a plain link has nothing to tell before its bytes arrive
    if matches!(listing.resolved, Reference::Url(_)) {
        return;
    }
    let mut facts = vec![listing.resolved.to_string()];
    facts.extend(
        listing
            .title
            .as_deref()
            .map(|title| output.clip(title, TITLE)),
    );
    facts.push(units::count(listing.files.len() as u64, "file", "files"));
    let size: Option<u64> = listing.files.iter().map(|file| file.size).sum();
    facts.extend(size.map(units::size));
    facts.extend(listing.license.clone());
    output.step("Resolved", &facts.join(" · "));
    if let Some(license) = &listing.license
        && let Some(limit) = restriction(license)
    {
        output.warn(&format!("the license {license} does not allow {limit}"));
    }
}

/// prints each dataset that moved to another version, and returns their names.
pub(super) fn tell_upgrades(output: &Output, old: &Lock, new: &Lock) -> Vec<String> {
    let mut moved = Vec::new();
    for dataset in &new.datasets {
        let Some(before) = old.get(&dataset.name) else {
            continue;
        };
        if before.resolved == dataset.resolved && before.files == dataset.files {
            continue;
        }
        let paths = |locked: &Locked| {
            locked
                .files
                .iter()
                .map(|file| (file.path.clone(), file.sha256))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let (was, now) = (paths(before), paths(dataset));
        let added = now.keys().filter(|path| !was.contains_key(*path)).count();
        let removed = was.keys().filter(|path| !now.contains_key(*path)).count();
        let changed = now
            .iter()
            .filter(|(path, sha256)| was.get(*path).is_some_and(|before| before != *sha256))
            .count();
        let mut facts = vec![format!("{} → {}", before.resolved, dataset.resolved)];
        for (n, one, many) in [
            (changed, "file changed", "files changed"),
            (added, "file added", "files added"),
            (removed, "file removed", "files removed"),
        ] {
            if n > 0 {
                facts.push(units::count(n as u64, one, many));
            }
        }
        output.step(
            "Upgraded",
            &format!("{} · {}", dataset.name, facts.join(" · ")),
        );
        moved.push(dataset.name.clone());
    }
    moved
}
