//! the loop at the heart of a project: resolve what data.toml changed, learn every file's hash,
//! put what the lock names in the cache, then write data.lock.

mod draft;
pub(super) mod place;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use tokio::task::JoinSet;
use url::Url;

use self::draft::Draft;
use super::Context;
use super::fetch::{self, Job};
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch, Fetched, Net, Origin};
use crate::output::Output;
use crate::project::Project;
use crate::project::lock::{self, Lock, Locked};
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
}

/// one file to put in the cache, and where its result goes.
enum Slot {
    /// a file whose hash only its bytes can tell: draft and file index.
    Learn(usize, usize),
    /// a file the lock already pins, missing from the cache.
    Fill,
}

impl Loop<'_> {
    /// the lock data.toml asks for. datasets whose entry changed are resolved again, files whose
    /// hash no source states are downloaded to learn it, and with `fill` every file the lock
    /// names is put in the cache. `given` is a listing already resolved for one dataset.
    pub(super) async fn relock(
        &self,
        manifest: &Manifest,
        old: &Lock,
        upgrade: &Upgrade,
        fill: bool,
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
            )?);
        }
        let mut jobs = Vec::new();
        let mut slots = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            for (file, job) in draft.jobs(&self.cache, fill) {
                slots.push(file.map_or(Slot::Fill, |file| Slot::Learn(index, file)));
                jobs.push(job);
            }
        }
        if fill {
            for locked in &kept {
                for file in &locked.files {
                    if self.cache.find(&file.sha256, Some(file.size)).is_none() {
                        slots.push(Slot::Fill);
                        jobs.push(job(
                            &locked.name,
                            &file.path,
                            &file.url,
                            Expect {
                                sha256: Some(file.sha256),
                                md5: file.md5,
                                size: Some(file.size),
                            },
                        )?);
                    }
                }
            }
        }
        let fetched = self.download(jobs).await?;
        for (slot, fetched) in slots.into_iter().zip(fetched) {
            if let Slot::Learn(draft, file) = slot {
                drafts[draft].learn(file, &fetched);
            }
        }
        let now = lock::timestamp(SystemTime::now());
        let mut datasets = kept;
        datasets.extend(drafts.into_iter().map(|draft| draft.finish(&now)));
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Lock { datasets })
    }

    /// writes data.lock when it changed, and says so.
    pub(super) fn write_lock(&self, old: Option<&Lock>, lock: &Lock) -> Result<bool, Error> {
        if old == Some(lock) {
            return Ok(false);
        }
        self.project.write(lock::FILE, lock.render().as_bytes())?;
        let files: usize = lock
            .datasets
            .iter()
            .map(|dataset| dataset.files.len())
            .sum();
        self.context.output.step(
            "Locked",
            &format!(
                "{} in {}",
                units::count(files as u64, "file", "files"),
                lock::FILE
            ),
        );
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

/// the download of one locked or listed file.
fn job(dataset: &str, path: &str, url: &str, expect: Expect) -> Result<Job, Error> {
    let url = Url::parse(url).map_err(|err| {
        Error::new(
            Kind::ProjectInvalid,
            format!("data.lock has a broken link for {dataset}/{path}: {err}"),
        )
        .help("run `fetchloom lock` to write it again")
    })?;
    let (from, divert) = source::fetch_plan(&url);
    Ok(Job {
        fetch: Fetch {
            url,
            from,
            divert,
            expect,
            revalidate: false,
        },
        label: format!("{dataset}/{path}"),
        dataset: Some(dataset.to_owned()),
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
