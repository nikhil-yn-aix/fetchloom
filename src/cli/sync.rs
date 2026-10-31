//! the loop at the heart of a project: resolve what data.toml changed, learn every file's hash,
//! put what the lock names in the cache, then write data.lock.

pub(super) mod disk;
mod draft;
pub(super) mod place;
pub(super) mod unpack;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use tokio::task::JoinSet;
use url::Url;

use self::disk::Survey;
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
use crate::project::state::Synced;
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

    /// makes the data folder match `lock`, starting from `survey` of it. what each file needs is
    /// decided first, so only the bytes placing lacks are downloaded. archives this machine never
    /// unpacked are unpacked first, since what they hold decides where files go. `synced` names
    /// data.toml and data.lock as this run leaves them, for the state to record.
    pub(super) async fn place(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        force: bool,
        survey: Survey,
        synced: Option<Synced>,
    ) -> Result<Placement, Error> {
        let mut placing = Placing::new(self.project, manifest, lock, &self.cache, force, survey)?;
        let unknown = placing.unknown();
        if !unknown.is_empty() {
            self.unpack_locked(lock, &unknown).await?;
            let survey = placing.into_survey();
            placing = Placing::new(self.project, manifest, lock, &self.cache, force, survey)?;
        }
        let (archives, files): (Vec<_>, Vec<_>) = placing
            .missing(&self.cache)?
            .into_iter()
            .partition(|(_, file)| file.unpacks_to.is_some());
        let jobs = files
            .into_iter()
            .map(|(dataset, file)| job(lock, &dataset, file))
            .collect::<Result<Vec<_>, _>>()?;
        self.download(jobs).await?;
        self.unpack_locked(lock, &archives).await?;
        placing.apply(
            &self.context.output,
            &self.context.work,
            &self.cache,
            synced,
        )
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
        let _span = tracing::debug_span!("write_lock").entered();
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
        let mut asked = Vec::new();
        for (name, text) in asks {
            if let Some((given_name, listing)) = &given
                && *given_name == name
            {
                listings.push((name, listing.clone()));
                continue;
            }
            if self.net.offline().enabled() {
                return Err(Error::new(
                    Kind::OfflineMissing,
                    format!("offline, and {name} needs a new entry in data.lock"),
                )
                .help(format!(
                    "{}, or restore matching data.toml and data.lock",
                    self.net.offline().retry()
                )));
            }
            let entry = &manifest.datasets[&name];
            let parsed = Reference::parse(&text).map_err(|err| cite(err, entry, "ref"))?;
            // the line itself holds the secret, so only its place is shown
            parsed
                .reference
                .check_shareable()
                .map_err(|err| cite_place(err, entry, "ref"))?;
            if !resolves_locally(&parsed.reference) {
                asked.push(parsed.reference.to_string());
            }
            let net = self.net.clone();
            tasks.spawn(async move {
                let listing = parsed.reference.resolve(&*net).await;
                (name, listing)
            });
        }
        let output = &self.context.output;
        let _waiting = match asked.as_slice() {
            [] => None,
            [one] => Some(output.wait("Resolving", one.clone())),
            many => Some(output.wait(
                "Resolving",
                units::count(many.len() as u64, "dataset", "datasets"),
            )),
        };
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
        if self.net.offline().enabled() {
            let missing: Vec<&str> = jobs
                .iter()
                .filter(|job| !in_cache(&self.cache, &job.fetch.expect))
                .map(|job| job.label.as_str())
                .collect();
            if !missing.is_empty() {
                // a new link has no fingerprint yet, so the cache cannot be asked for it
                let unknown = jobs.iter().any(|job| {
                    job.fetch.expect.sha256.is_none() && !in_cache(&self.cache, &job.fetch.expect)
                });
                if unknown {
                    let mut error = Error::new(
                        Kind::OfflineMissing,
                        "offline, and a new link must be downloaded once to learn its fingerprint",
                    );
                    for label in missing.iter().take(5) {
                        error = error.detail((*label).to_owned());
                    }
                    return Err(error.help(format!(
                        "{} to pin it. `fetchloom get <link> --offline` reuses a file fetched before",
                        self.net.offline().retry()
                    )));
                }
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
                return Err(error.help(format!("{} to download them", self.net.offline().retry())));
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

/// resolves a reference, with progress or plain lines while its source takes its time.
pub(super) async fn resolve(
    output: &Output,
    reference: &Reference,
    net: &Net,
) -> Result<Listing, Error> {
    let _waiting =
        (!resolves_locally(reference)).then(|| output.wait("Resolving", reference.to_string()));
    reference.resolve(net).await
}

/// whether a reference resolves without asking anyone, as a plain link does. such a link may
/// also carry a token, which no progress line should print.
fn resolves_locally(reference: &Reference) -> bool {
    matches!(reference, Reference::Url(_) | Reference::Dropbox(_))
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
        // `fetchloom lock` keeps a dataset whose entry did not change, link and all
        .help(format!("restore data.lock from git, or run `fetchloom lock --upgrade {dataset}`, which resolves it again and may pin newer data"))
    })?;
    let pinned = lock.datasets.iter().find(|locked| locked.name == dataset);

    Ok(Job {
        fetch: Fetch {
            expect: Expect {
                sha256: Some(file.sha256),
                md5: file.md5,
                size: Some(file.size),
            },
            revalidate: false,
            unpack: false,
            ..source::fetch_plan(&url)
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

/// points an error at the data.toml line it comes from by its number alone, as `data.toml:7`.
fn cite_place(error: Error, entry: &Entry, key: &str) -> Error {
    match entry
        .lines
        .get(key)
        .and_then(|line| line.split_whitespace().next())
    {
        Some(place) => error.detail(place.to_owned()),
        None => error,
    }
}

pub(super) fn tell_raw(output: &Output, typed: &str, parsed: &Reference) {
    if let Reference::Url(raw) = parsed
        && Url::parse(typed.trim()).is_ok_and(|typed| typed != *raw)
    {
        output.step("Using", &format!("raw file {}", crate::net::Shown(raw)));
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
    facts.extend(
        size.filter(|_| listing.unavailable.is_empty())
            .map(units::size),
    );
    if !listing.unavailable.is_empty() {
        facts.push(units::count(
            listing.unavailable.len() as u64,
            "unfetchable entry",
            "unfetchable entries",
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::store;
    use crate::digest::{self, Md5};
    use crate::net::Offline;
    use crate::output::{Level, Style};
    use crate::source::recorded::block_on;

    fn context() -> Context {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.offline = Offline::Flag;
        context
    }

    fn locked(name: &str) -> Locked {
        Locked {
            name: name.to_owned(),
            reference: "zenodo:1".to_owned(),
            spec: String::new(),
            resolved: "zenodo:2".to_owned(),
            title: None,
            license: None,
            doi: None,
            timestamp: "2026-01-01T00:00:00Z".to_owned(),
            files: vec![LockedFile {
                path: "data.csv".to_owned(),
                size: 4,
                sha256: digest::sha256(b"data"),
                md5: Md5::from_hex("8d777f385d3dfec8815d20f7496026dc"),
                url: "https://example.org/data.csv".to_owned(),
                unpacks_to: None,
            }],
        }
    }

    #[test]
    fn filter_changes_keep_the_locked_revision_unless_upgrading() {
        let manifest =
            Manifest::parse("[datasets.set]\nref = 'zenodo:1'\nselect = '*.csv'\n").unwrap();
        let entry = &manifest.datasets["set"];
        let before = locked("set");
        assert_eq!(pinned(entry, Some(&before), false), "zenodo:2");
        assert_eq!(pinned(entry, Some(&before), true), "zenodo:1");
        assert_eq!(pinned(entry, None, false), "zenodo:1");
        let mut changed = entry.clone();
        changed.reference = "zenodo:3".to_owned();
        assert_eq!(pinned(&changed, Some(&before), false), "zenodo:3");
        let selected = Upgrade::Only(BTreeSet::from(["set".to_owned()]));
        assert!(selected.includes("set"));
        assert!(!selected.includes("other"));
        assert!(Upgrade::All.includes("other"));
        assert!(!Upgrade::Nothing.includes("set"));
    }

    #[test]
    fn jobs_pin_hashes_and_refresh_dead_links_at_the_locked_revision() {
        let lock = Lock {
            datasets: vec![locked("set")],
        };
        let file = &lock.datasets[0].files[0];
        let download = job(&lock, "set", file).unwrap();
        assert_eq!(download.label, "set/data.csv");
        assert_eq!(download.dataset.as_deref(), Some("set"));
        assert_eq!(download.fetch.expect.sha256, Some(file.sha256));
        assert_eq!(download.fetch.expect.md5, file.md5);
        assert_eq!(download.fetch.expect.size, Some(4));
        assert!(!download.fetch.revalidate);
        assert!(!download.fetch.unpack);
        let again = download.again.unwrap();
        assert_eq!(again.resolved, "zenodo:2");
        assert_eq!(again.path, "data.csv");
        assert!(again.alone);
        let mut broken = file.clone();
        broken.url = "not a URL".to_owned();
        let error = job(&lock, "set", &broken).err().unwrap();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(error.message.contains("set/data.csv"));
        assert!(error.help.unwrap().contains("lock --upgrade set"));
    }

    #[test]
    fn cache_checks_require_real_objects_not_just_md5_hints() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let stored = store(&cache, b"data");
        let md5 = Md5::from_hex("8d777f385d3dfec8815d20f7496026dc").unwrap();
        assert!(!in_cache(&cache, &Expect::default()));
        let sha = Expect {
            sha256: Some(stored.digest),
            size: Some(4),
            ..Expect::default()
        };
        assert!(in_cache(&cache, &sha));
        assert!(!in_cache(
            &cache,
            &Expect {
                size: Some(5),
                ..sha
            }
        ));
        let md5_only = Expect {
            md5: Some(md5),
            ..Expect::default()
        };
        assert!(!in_cache(&cache, &md5_only));
        cache.save_stated_md5(&md5, &stored.digest, 4).unwrap();
        assert!(in_cache(&cache, &md5_only));
        cache.drop_object(&stored.digest).unwrap();
        assert!(!in_cache(&cache, &md5_only));
        assert!(!in_cache(&cache, &sha));
    }

    #[test]
    fn offline_download_errors_list_missing_files_without_network_requests() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let lock = Lock {
            datasets: vec![locked("set")],
        };
        let jobs = (0..7)
            .map(|index| {
                let mut download = job(&lock, "set", &lock.datasets[0].files[0]).unwrap();
                download.label = format!("set/{index}.csv");
                download
            })
            .collect();
        let error = block_on(runner.download(jobs)).unwrap_err();
        assert_eq!(error.kind, Kind::OfflineMissing);
        assert!(error.message.contains("7 files"));
        assert_eq!(
            error.details,
            [
                "set/0.csv",
                "set/1.csv",
                "set/2.csv",
                "set/3.csv",
                "set/4.csv",
                "and 2 more"
            ]
        );
        assert!(error.help.unwrap().contains("without --offline"));
        assert_eq!(context.work.counts().requests, 0);
        store(&runner.cache, b"data");
        let fetched =
            block_on(runner.download(vec![job(&lock, "set", &lock.datasets[0].files[0]).unwrap()]))
                .unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].origin, Origin::Cache);
        assert_eq!(fetched[0].digest, digest::sha256(b"data"));
        assert_eq!(context.work.counts().requests, 0);
    }

    #[test]
    fn resolving_reports_manifest_locations_without_echoing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let mut context = context();
        context.offline = Offline::No;
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        for (reference, detail) in [
            ("not-a-reference", "data.toml:2   ref = 'not-a-reference'"),
            (
                "https://example.org/data?token=private-value",
                "data.toml:2",
            ),
        ] {
            let manifest =
                Manifest::parse(&format!("[datasets.set]\nref = '{reference}'\n")).unwrap();
            let error = block_on(runner.resolve_all(
                &manifest,
                vec![("set".to_owned(), reference.to_owned())],
                None,
            ))
            .unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid);
            assert_eq!(error.details, [detail]);
            assert!(!error.message.contains("private-value"));
            assert_eq!(context.work.counts().requests, 0);
        }
    }

    #[test]
    fn unchanged_locks_are_not_rewritten_and_removed_datasets_leave_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let lock = Lock {
            datasets: vec![locked("set")],
        };
        assert!(runner.write_lock(None, &lock).unwrap());
        let path = project.root.join(lock::FILE);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
            .unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!runner.write_lock(Some(&lock), &lock).unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(project.lock().unwrap(), Some(lock.clone()));
        assert!(runner.write_lock(Some(&lock), &Lock::default()).unwrap());
        assert_eq!(project.lock().unwrap(), Some(Lock::default()));
    }

    #[test]
    fn upgrade_reports_ignore_metadata_only_changes_and_new_datasets() {
        let context = context();
        let old = Lock {
            datasets: vec![locked("changed"), locked("metadata"), locked("revision")],
        };
        let mut new = old.clone();
        new.datasets[0].files[0].sha256 = digest::sha256(b"changed");
        new.datasets[1].timestamp = "2026-02-01T00:00:00Z".to_owned();
        new.datasets[2].resolved = "zenodo:3".to_owned();
        new.datasets.push(locked("new"));
        assert_eq!(
            tell_upgrades(&context.output, &old, &new),
            ["changed", "revision"]
        );
        assert_eq!(
            tell_upgrades(&context.output, &old, &old),
            Vec::<String>::new()
        );
    }

    #[test]
    fn license_restrictions_are_case_insensitive_and_do_not_warn_for_open_licenses() {
        assert_eq!(restriction("cc-by-nc-sa-4.0"), Some("commercial use"));
        assert_eq!(restriction("CC-BY-ND-4.0"), Some("changed copies"));
        assert_eq!(restriction("CC-BY-NC-ND-4.0"), Some("commercial use"));
        for license in ["MIT", "Apache-2.0", "CC-BY-4.0", "CC0-1.0"] {
            assert_eq!(restriction(license), None);
        }
    }
}
