//! the loop at the heart of a project: resolve what data.toml changed, learn every file's hash,
//! put what the lock names in the cache, then write data.lock.

pub(super) mod disk;
mod draft;
pub(super) mod place;
pub(super) mod unpack;

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use tokio::task::JoinSet;
use url::Url;

use self::disk::Survey;
use self::draft::Draft;
use self::place::{Download, Placement, Placing, Preview};
use super::Context;
use super::fetch::{self, Again, Job};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch, Fetched, Net, Origin};
use crate::output::Output;
use crate::plan;
use crate::project::Project;
use crate::project::lock::{self, Drift, Lock, Locked, LockedFile};
use crate::project::manifest::{Entry, Manifest};
use crate::project::state::Synced;
use crate::source::{self, Listing, Reference};
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
    /// the dataset the command's last line tells about, so its lock and placement lines stay out.
    told: Option<String>,
    /// whether this run wrote data.lock, so a download cancelled later no longer left everything
    /// as it was.
    wrote_lock: AtomicBool,
    /// whether a person said yes to going on though the cache's disk may lack room, which says
    /// yes to the size of every download in the run as well.
    roomy: AtomicBool,
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
            told: None,
            wrote_lock: AtomicBool::new(false),
            roomy: AtomicBool::new(false),
        }
    }

    pub(super) fn telling(mut self, name: &str) -> Self {
        self.told = Some(name.to_owned());
        self
    }

    /// makes the data folder match `lock`, starting from `survey` of it. what each file needs is
    /// decided first, so only the bytes placing lacks are downloaded. archives this machine never
    /// unpacked are unpacked first, since what they hold decides where files go. `synced` names
    /// data.toml and data.lock as this run leaves them, for the state to record.
    pub(super) async fn place(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        plan: plan::Plan<'_>,
        force: bool,
        survey: Survey,
        synced: Option<Synced>,
    ) -> Result<Placement, Error> {
        let placing = self
            .prepare(manifest, lock, plan, force, survey)
            .await
            .map_err(|error| self.before_placing(error))?;
        self.apply(placing, synced)
    }

    /// puts in the cache every byte placing `plan` needs, downloading and unpacking what it
    /// lacks, and returns the placing ready to apply. nothing in the project changes.
    pub(super) async fn prepare<'p>(
        &'p self,
        manifest: &'p Manifest,
        lock: &'p Lock,
        plan: plan::Plan<'p>,
        force: bool,
        survey: Survey,
    ) -> Result<Placing<'p>, Error> {
        let mut placing = Placing::from_plan(self.project, manifest, plan, force, survey)?;
        self.ask_room(&placing)?;
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
        self.download_prepared(lock, &files).await?;
        self.unpack_locked(lock, &archives).await?;
        Ok(placing)
    }

    /// places what [`Loop::prepare`] readied, and records `synced` in the state.
    pub(super) fn apply(
        &self,
        placing: Placing<'_>,
        synced: Option<Synced>,
    ) -> Result<Placement, Error> {
        placing.apply(
            &self.context.output,
            &self.context.work,
            &self.cache,
            synced,
            self.told.as_deref(),
            &self.context.stop,
        )
    }

    /// the lock data.toml asks for, beside data.lock as it was. datasets whose entry changed are
    /// resolved again, and files whose hash no source states are downloaded to learn it. `given`
    /// is a listing already resolved for one dataset.
    pub(super) async fn relock(
        &self,
        manifest: &Manifest,
        old: Option<Lock>,
        upgrade: &Upgrade,
        given: Option<(String, Listing)>,
    ) -> Result<(Lock, Before), Error> {
        let existed = old.is_some();
        let Drafted {
            kept,
            mut drafts,
            mut others,
        } = self
            .draft(manifest, old.unwrap_or_default(), upgrade, given)
            .await?;
        let mut jobs = Vec::new();
        let mut slots = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            for (file, job) in draft.jobs(&self.cache) {
                slots.push((index, file));
                jobs.push(job);
            }
        }
        if !jobs.is_empty() {
            // a clash the listings already show stops the run before it downloads anything
            let foreseen: Vec<Locked> = drafts
                .iter()
                .map(|draft| draft.foreseen(&self.cache))
                .collect();
            plan::foresee(manifest, kept.iter().chain(&foreseen), &self.cache)?;
        }
        let fetched = self.download(jobs).await?;
        for ((draft, file), fetched) in slots.into_iter().zip(fetched) {
            if let Some(file) = file {
                drafts[draft].learn(file, &fetched, &self.cache);
            }
        }
        self.unpack_drafts(&mut drafts).await?;
        let now = lock::timestamp(SystemTime::now());
        let drafted = drafts
            .into_iter()
            .map(|draft| draft.finish(&now))
            .collect::<Result<Vec<_>, _>>()?;
        let (lock, kept) = settled(&mut others, kept, drafted);
        Ok((
            lock,
            Before {
                existed,
                others,
                kept,
            },
        ))
    }

    /// the lock [`Loop::relock`] would make, beside data.lock as it was, found without
    /// downloading or unpacking, and the work it would do first. a hash only that work can learn
    /// has a stand-in, so the lock is only for planning.
    pub(super) async fn relock_dry(
        &self,
        manifest: &Manifest,
        old: Option<Lock>,
        upgrade: &Upgrade,
        given: Option<(String, Listing)>,
    ) -> Result<DryRelock, Error> {
        let existed = old.is_some();
        let Drafted {
            kept,
            mut drafts,
            mut others,
        } = self
            .draft(manifest, old.unwrap_or_default(), upgrade, given)
            .await?;
        let mut jobs = Vec::new();
        let mut unpack = Vec::new();
        for draft in &mut drafts {
            // archives this machine unpacked before are known from their records
            unpack.extend(
                draft
                    .unknown_archives(&self.cache)
                    .into_iter()
                    .map(|archive| archive.label),
            );
            jobs.extend(draft.jobs(&self.cache).into_iter().map(|(_, job)| job));
        }
        let now = lock::timestamp(SystemTime::now());
        let mut guessed = HashSet::new();
        let mut drafted = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let (locked, stood_in) = draft.stand_in(&now)?;
            guessed.extend(stood_in);
            drafted.push(locked);
        }
        let (lock, kept) = settled(&mut others, kept, drafted);
        let before = Before {
            existed,
            others,
            kept,
        };
        Ok(DryRelock {
            lock,
            before,
            jobs,
            unpack,
            guessed,
        })
    }

    /// datasets whose entry is unchanged, moved from data.lock as it has them, and drafts of the
    /// others with every hash known without a download: from data.lock's files when only
    /// `extract` changed, otherwise resolved again.
    async fn draft(
        &self,
        manifest: &Manifest,
        old: Lock,
        upgrade: &Upgrade,
        given: Option<(String, Listing)>,
    ) -> Result<Drafted, Error> {
        let mut kept: Vec<Locked> = Vec::new();
        let mut others = Vec::new();
        let mut drafts = Vec::new();
        for locked in old.datasets {
            match manifest.datasets.get(&locked.name) {
                Some(entry) if !upgrade.includes(&locked.name) => match locked.drift(entry) {
                    None => kept.push(locked),
                    Some(Drift::Extract) => {
                        drafts.push((locked.name.clone(), Draft::switched(entry, &locked)?));
                        others.push(locked);
                    }
                    Some(Drift::Spec) => others.push(locked),
                },
                _ => others.push(locked),
            }
        }
        let others = Lock {
            datasets: others,
            derived: old.derived,
        };
        let mut asks = Vec::new();
        for (name, entry) in &manifest.datasets {
            let is_kept = kept
                .binary_search_by(|locked| locked.name.as_str().cmp(name))
                .is_ok();
            if !is_kept && !drafts.iter().any(|(drafted, _)| drafted == name) {
                asks.push((
                    name.clone(),
                    pinned(entry, others.get(name), upgrade.includes(name)),
                ));
            }
        }
        let listings = self.resolve_all(manifest, asks, given).await?;
        for (name, listing) in listings {
            let draft = Draft::new(
                &name,
                &manifest.datasets[&name],
                listing,
                others.get(&name),
                upgrade.includes(&name),
                &self.cache,
            )?;
            drafts.push((name, draft));
        }
        drafts.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Drafted {
            kept,
            drafts: drafts.into_iter().map(|(_, draft)| draft).collect(),
            others,
        })
    }

    /// finds damaged objects behind edited hardlinks without writing their change marks.
    pub(super) fn damaged_objects(&self, survey: &Survey) -> Result<HashSet<Sha256>, Error> {
        let mut checked = HashSet::new();
        let mut damaged = HashSet::new();
        for (path, placed) in &survey.state.files {
            if placed.method != crate::place::Method::Hardlink
                || !(survey.edited.contains(path)
                    || survey
                        .disk
                        .get(path)
                        .is_some_and(|seen| *seen != placed.look))
                || !checked.insert(placed.sha256)
            {
                continue;
            }
            let object = self.cache.object_path(&placed.sha256);
            let actual = crate::cache::read_unfollowed(&object).and_then(Sha256::of_reader);
            match actual {
                Ok(actual) if actual == placed.sha256 => {}
                Ok(_) => {
                    damaged.insert(placed.sha256);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    damaged.insert(placed.sha256);
                }
                Err(err) => return Err(place::local(&self.project.root.join(path), &err)),
            }
        }
        Ok(damaged)
    }

    /// what [`Loop::place`] would do, decided without downloading, unpacking or writing, given
    /// `jobs` relocking would download first. a download an offline run could not make stops the
    /// preview as it would stop the run. files `guessed` by the dry relock, as `<dataset>/<path>`,
    /// that something already stands at are told as files to check.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one finding of the dry run that the preview builds on"
    )]
    pub(super) fn preview(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        force: bool,
        survey: Survey,
        mut jobs: Vec<Job>,
        damaged: &HashSet<Sha256>,
        guessed: &HashSet<String>,
    ) -> Result<Preview, Error> {
        let placing = Placing::new(self.project, manifest, lock, &self.cache, force, survey)?;
        let mut unpack = Vec::new();
        for (dataset, file) in placing
            .unknown()
            .into_iter()
            .chain(placing.missing_as_is(&self.cache, damaged)?)
        {
            if file.unpacks_to.is_some() {
                unpack.push(format!("{dataset}/{}", file.path));
            }
            jobs.push(job(lock, &dataset, file)?);
        }
        let mut seen = BTreeSet::new();
        jobs.retain(|job| seen.insert(job.label.clone()));
        let mut seen = BTreeSet::new();
        unpack.retain(|label| seen.insert(label.clone()));
        let available =
            |job: &Job| in_cache_where(&self.cache, &job.fetch, |digest| !damaged.contains(digest));
        jobs.retain(|job| !available(job));
        self.refuse_offline_with(&jobs, available)?;
        let mut preview = placing.preview(guessed);
        preview.changes.download = jobs
            .into_iter()
            .map(|job| Download {
                path: job.label,
                size: job.fetch.expect.size,
            })
            .collect();
        preview.changes.unpack = unpack;
        Ok(preview)
    }

    /// writes data.lock when it changed from what it was `before`, and says which datasets it
    /// pins anew or no longer. returns the text written, if any.
    pub(super) fn write_lock(&self, before: &Before, lock: &Lock) -> Result<Option<String>, Error> {
        if before.holds(lock) {
            return Ok(None);
        }
        let text = self.write_rendered(before, lock, lock.render())?;
        self.project.sweep_lists(lock);
        Ok(Some(text))
    }

    /// writes `text`, `lock` as rendered, to data.lock, and says which datasets it pins anew or
    /// no longer than `before`. returns the text.
    pub(super) fn write_rendered(
        &self,
        before: &Before,
        lock: &Lock,
        text: String,
    ) -> Result<String, Error> {
        let _span = tracing::debug_span!("write_lock").entered();
        // the lists first, so data.lock never names a part that is not there yet
        self.project.write_lists(lock)?;
        self.project.write(lock::FILE, text.as_bytes())?;
        self.wrote_lock.store(true, Ordering::Relaxed);
        tell_lock(
            &self.context.output,
            before,
            lock,
            Verbs::DONE,
            self.told.as_deref(),
            &HashSet::new(),
        );
        Ok(text)
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
            let parsed = Reference::parse(&text).map_err(|err| cite_place(err, entry, "ref"))?;
            // the line itself may hold a secret, so only its place is shown
            parsed
                .reference
                .check_shareable()
                .map_err(|err| cite_place(err, entry, "ref"))?;
            if !resolves_locally(&parsed.reference) {
                asked.push(parsed.reference.to_string());
            }
            let net = self.net.clone();
            let folders = crate::glob::folders(&entry.select);
            tasks.spawn(async move {
                let listing = parsed.reference.resolve_in(&*net, folders.as_deref()).await;
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
        self.refuse_offline(&jobs)?;
        crate::space::cache_holds(
            self.cache.root(),
            jobs.iter()
                .filter(|job| !in_cache(&self.cache, &job.fetch))
                .filter_map(|job| Some((job.label.as_str(), job.fetch.expect.size?))),
        )
        .map_err(|error| self.as_left(error))?;
        let size: u64 = jobs
            .iter()
            .filter(|job| !in_cache(&self.cache, &job.fetch))
            .filter_map(|job| job.fetch.expect.size)
            .fold(0, u64::saturating_add);
        let roomy = super::confirm(
            &self.context.output,
            self.yes(),
            self.context.settings.ask_above.value,
            &units::count(jobs.len() as u64, "file", "files"),
            size,
            Some((self.cache.root(), size)),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.fetch_or(roomy, Ordering::Relaxed);
        let started = Instant::now();
        let fetched = fetch::all(self.context, &self.net, &self.cache, jobs)
            .await
            .map_err(|error| self.as_left(error))?;
        self.tell_downloaded(&fetched, started);
        Ok(fetched)
    }

    /// `error`, saying what a stop before any file was placed left: nothing changed, or, once this
    /// run wrote data.lock, the data folder as it was. a cancel says so through
    /// [`Loop::as_left`], and any other error stays as it is.
    pub(super) fn before_placing(&self, mut error: Error) -> Error {
        if error.kind == Kind::Interrupted && error.cancelled.is_none() {
            let left = if self.wrote_lock.load(Ordering::Relaxed) {
                "the data folder is as it was"
            } else {
                "nothing changed"
            };
            error.message = format!("{}, so {left}", error.message);
        }
        error
    }

    /// whether downloads go on without asking: `--yes`, or a person already said yes to going on
    /// without room, which covers their size too.
    fn yes(&self) -> bool {
        self.context.yes || self.roomy.load(Ordering::Relaxed)
    }

    /// asks once, before anything downloads, when the disk holding the cache may lack room for
    /// all placing needs: the files and archives the cache lacks, and as much again for each
    /// archive whose files are not known yet, which unpacking adds at the least. each download
    /// asks for its own part too, which this covers when they only together pass the room. a file
    /// the disk cannot hold at all, as FAT32 holds none of 4 GiB, is refused first, since no room
    /// would let it in.
    fn ask_room(&self, placing: &Placing<'_>) -> Result<(), Error> {
        if self.yes() || self.net.offline().enabled() {
            return Ok(());
        }
        let lacks = |file: &LockedFile| self.cache.find(&file.sha256, Some(file.size)).is_none();
        let missing = placing.missing_as_is(&self.cache, &HashSet::new())?;
        let unknown = placing.unknown();
        let large: Vec<(String, u64)> = missing
            .iter()
            .chain(&unknown)
            .filter(|(_, file)| file.size > crate::space::FAT_LARGEST && lacks(file))
            .map(|(dataset, file)| (format!("{dataset}/{}", file.path), file.size))
            .collect();
        crate::space::cache_holds(
            self.cache.root(),
            large.iter().map(|(label, size)| (label.as_str(), *size)),
        )
        .map_err(|error| self.as_left(error))?;
        let mut needed = missing
            .iter()
            .filter(|(_, file)| lacks(file))
            .map(|(_, file)| file.size)
            .fold(0, u64::saturating_add);
        for (_, file) in &unknown {
            let download = if lacks(file) { file.size } else { 0 };
            needed = needed.saturating_add(download).saturating_add(file.size);
        }
        let free = crate::space::free(self.cache.root());
        if crate::space::short(needed, free).is_none() {
            return Ok(());
        }
        super::ask_room(
            &self.context.output,
            "this sync",
            needed,
            free.unwrap_or_default(),
            self.cache.root(),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// `error`, saying that a cancel left everything as it was, unless this run wrote data.lock
    /// before it: a run downloads before it places, and places only once every file is in.
    fn as_left(&self, error: Error) -> Error {
        if self.wrote_lock.load(Ordering::Relaxed) {
            error
        } else {
            error.unchanged()
        }
    }

    async fn download_prepared(
        &self,
        lock: &Lock,
        files: &[(String, &LockedFile)],
    ) -> Result<Vec<Fetched>, Error> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let mut descriptions = Vec::with_capacity(files.len());
        let mut urls = Vec::with_capacity(files.len());
        for (dataset, file) in files {
            let (description, url) = fetch::Description::take(job(lock, dataset, file)?);
            descriptions.push(description);
            urls.push(url);
        }
        let available =
            |file: &LockedFile| self.cache.find(&file.sha256, Some(file.size)).is_some();
        if self.net.offline().enabled() {
            let missing: Vec<_> = files
                .iter()
                .zip(&descriptions)
                .filter(|((_, file), _)| !available(file))
                .map(|(_, description)| description.label.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(self.offline_missing(&missing));
            }
        }
        crate::space::cache_holds(
            self.cache.root(),
            files
                .iter()
                .zip(&descriptions)
                .filter(|((_, file), _)| !available(file))
                .filter_map(|(_, description)| {
                    Some((description.label.as_str(), description.size?))
                }),
        )
        .map_err(|error| self.as_left(error))?;
        let size = files
            .iter()
            .zip(&descriptions)
            .filter(|((_, file), _)| !available(file))
            .filter_map(|(_, description)| description.size)
            .fold(0, u64::saturating_add);
        let roomy = super::confirm(
            &self.context.output,
            self.yes(),
            self.context.settings.ask_above.value,
            &units::count(files.len() as u64, "file", "files"),
            size,
            Some((self.cache.root(), size)),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.fetch_or(roomy, Ordering::Relaxed);
        let started = Instant::now();
        let fetched = fetch::prepared(
            self.context,
            &self.net,
            &self.cache,
            descriptions,
            urls,
            |index, url| {
                let (dataset, file) = &files[index];
                make_job(lock, dataset, file, &url)
            },
        )
        .await
        .map_err(|error| self.as_left(error))?;
        self.tell_downloaded(&fetched, started);
        Ok(fetched)
    }

    fn tell_downloaded(&self, fetched: &[Fetched], started: Instant) {
        let downloaded: Vec<&Fetched> = fetched
            .iter()
            .filter(|fetched| fetched.origin == Origin::Network)
            .collect();
        if !downloaded.is_empty() {
            let text = format!(
                "{} · {}",
                units::count(downloaded.len() as u64, "file", "files"),
                units::downloaded(
                    downloaded
                        .iter()
                        .map(|fetched| (fetched.size, fetched.moved)),
                    started.elapsed()
                )
            );
            self.context.output.step("Downloaded", &text);
        }
    }

    /// refuses, when offline, the jobs whose bytes the cache lacks.
    fn refuse_offline(&self, jobs: &[Job]) -> Result<(), Error> {
        self.refuse_offline_with(jobs, |job| in_cache(&self.cache, &job.fetch))
    }

    fn refuse_offline_with(
        &self,
        jobs: &[Job],
        available: impl Fn(&Job) -> bool,
    ) -> Result<(), Error> {
        if !self.net.offline().enabled() {
            return Ok(());
        }
        let missing: Vec<&str> = jobs
            .iter()
            .filter(|job| !available(job))
            .map(|job| job.label.as_str())
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        // a new link has no fingerprint yet, so the cache cannot be asked for it
        let unknown = jobs
            .iter()
            .any(|job| job.fetch.expect.sha256.is_none() && !available(job));
        if unknown {
            let mut error = Error::new(
                Kind::OfflineMissing,
                "offline, and a new link must be downloaded once to learn its SHA-256",
            );
            error = shown_five(error, &missing);
            return Err(error.help(format!(
                "{} to pin it. `fetchloom get <link> --offline` reuses a file fetched before",
                self.net.offline().retry()
            )));
        }
        Err(self.offline_missing(&missing))
    }

    fn offline_missing(&self, missing: &[&str]) -> Error {
        let mut error = Error::new(
            Kind::OfflineMissing,
            format!(
                "offline, and {} data.lock names {} not in the cache",
                units::count(missing.len() as u64, "file", "files"),
                if missing.len() == 1 { "is" } else { "are" }
            ),
        );
        error = shown_five(error, missing);
        error.help(self.net.offline().missing_help("them"))
    }
}

/// names the first five of `labels` in `error`, and how many more there are.
fn shown_five(mut error: Error, labels: &[&str]) -> Error {
    for label in labels.iter().take(5) {
        error = error.detail((*label).to_owned());
    }
    if labels.len() > 5 {
        error = error.detail(format!("and {} more", labels.len() - 5));
    }
    error
}

/// what [`Loop::relock_dry`] found relocking would do.
pub(super) struct DryRelock {
    /// the lock to plan with, never to write.
    pub(super) lock: Lock,
    pub(super) before: Before,
    /// the downloads relocking would make first.
    pub(super) jobs: Vec<Job>,
    /// archives, as `<dataset>/<path>`, it would unpack to learn what they hold.
    pub(super) unpack: Vec<String>,
    /// files, as `<dataset>/<path>`, whose hash in `lock` is a stand-in.
    pub(super) guessed: HashSet<String>,
}

/// datasets on their way into data.lock.
struct Drafted {
    /// entries whose data.toml entry did not change, moved from data.lock.
    kept: Vec<Locked>,
    drafts: Vec<Draft>,
    /// the entries of data.lock not kept: those to replace or drop, and the results of recipes,
    /// which [`settled`] moves into the new lock.
    others: Lock,
}

/// the lock of `kept` and `drafted` entries, by name, and for each of its datasets whether it
/// was kept, with the results of recipes moved from `others`. a pin dates from when its data
/// last changed, so resolving the same data again leaves data.lock as it was.
fn settled(others: &mut Lock, kept: Vec<Locked>, drafted: Vec<Locked>) -> (Lock, Vec<bool>) {
    let mut datasets: Vec<(Locked, bool)> = kept.into_iter().map(|locked| (locked, true)).collect();
    datasets.extend(drafted.into_iter().map(|mut locked| {
        if let Some(before) = others.get(&locked.name)
            && before.resolved == locked.resolved
            && before.files == locked.files
        {
            locked.timestamp.clone_from(&before.timestamp);
        }
        (locked, false)
    }));
    datasets.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let (datasets, kept): (Vec<Locked>, _) = datasets.into_iter().unzip();
    // only `fetchloom build` changes a recipe's result, so it stays unless a source took its name
    let mut derived = std::mem::take(&mut others.derived);
    derived.retain(|result| {
        datasets
            .binary_search_by(|dataset| dataset.name.cmp(&result.name))
            .is_err()
    });
    (Lock { datasets, derived }, kept)
}

/// data.lock as it was before relocking, beside the new lock. entries relocking kept as they
/// were are moved into the new lock instead of copied, so they are found there.
pub(super) struct Before {
    /// whether there was a data.lock.
    existed: bool,
    /// the old entries relocking did not keep: those it replaced or dropped.
    others: Lock,
    /// for each dataset of the new lock, in its order, whether it was kept from the old one.
    kept: Vec<bool>,
}

impl Before {
    /// data.lock, or its absence, as a lock of which nothing was kept.
    #[cfg(test)]
    pub(super) fn whole(old: Option<Lock>) -> Before {
        Before {
            existed: old.is_some(),
            others: old.unwrap_or_default(),
            kept: Vec::new(),
        }
    }

    /// the old entry named `name`, beside the new `lock`.
    fn get<'a>(&'a self, lock: &'a Lock, name: &str) -> Option<&'a Locked> {
        match lock
            .datasets
            .binary_search_by(|dataset| dataset.name.as_str().cmp(name))
        {
            Ok(index) if self.kept.get(index) == Some(&true) => lock.datasets.get(index),
            _ => self.others.get(name),
        }
    }

    /// whether `lock` is exactly what data.lock held.
    pub(super) fn holds(&self, lock: &Lock) -> bool {
        self.existed
            && lock
                .datasets
                .iter()
                .all(|dataset| self.get(lock, &dataset.name) == Some(dataset))
            && self.dropped(lock).next().is_none()
    }

    /// the old datasets `lock` no longer has.
    fn dropped<'a>(&'a self, lock: &'a Lock) -> impl Iterator<Item = &'a Locked> {
        self.others
            .datasets
            .iter()
            .filter(|dataset| lock.get(&dataset.name).is_none())
    }
}

/// the words lock changes are told with: as done, or as a dry run would do them.
#[derive(Clone, Copy)]
pub(super) struct Verbs {
    /// for datasets data.lock pinned before and pins anew.
    locked: &'static str,
    /// for datasets data.lock pins for the first time.
    pinned: &'static str,
    /// for datasets data.lock no longer has.
    dropped: &'static str,
}

impl Verbs {
    pub(super) const DONE: Verbs = Verbs {
        locked: "Locked",
        pinned: "Pinned",
        dropped: "Removed",
    };
    pub(super) const WOULD: Verbs = Verbs {
        locked: "Would lock",
        pinned: "Would pin",
        dropped: "Would drop",
    };
}

/// the datasets whose pins `lock` changes beside data.lock as it was `before`: pinned for the
/// first time, pinned to other data or files, or no longer pinned, by name.
pub(super) fn repinned(before: &Before, lock: &Lock) -> Vec<String> {
    let mut names: Vec<String> = lock
        .datasets
        .iter()
        .filter(|dataset| {
            before
                .get(lock, &dataset.name)
                .is_none_or(|old| old.resolved != dataset.resolved || old.files != dataset.files)
        })
        .map(|dataset| dataset.name.clone())
        .chain(before.dropped(lock).map(|dataset| dataset.name.clone()))
        .collect();
    names.sort();
    names
}

/// the most datasets pinned for the first time that get a line each; the rest are counted.
const FIRST_PINS: usize = 20;

/// the labels, as `<dataset>/<path>`, of the files among `jobs` whose size only their download
/// can tell.
pub(super) fn sizeless(jobs: &[Job]) -> HashSet<String> {
    jobs.iter()
        .filter(|job| job.fetch.expect.size.is_none())
        .map(|job| job.label.clone())
        .collect()
}

/// says which datasets `lock` pins anew and which it no longer has, beside data.lock as it was
/// `before`, with `verbs` for each. a dataset pinned for the first time gets a line of its own
/// with what it pinned, so a typo in data.toml that pins an unrelated record stands out. `told`
/// is left out, since a later line tells about it.
pub(super) fn tell_lock(
    output: &Output,
    before: &Before,
    lock: &Lock,
    verbs: Verbs,
    told: Option<&str>,
    sizeless: &HashSet<String>,
) {
    for (verb, text) in lock_lines(output, before, lock, verbs, told, sizeless) {
        output.step(verb, &text);
    }
}

/// the lines [`tell_lock`] prints, each a verb and its text.
fn lock_lines(
    output: &Output,
    before: &Before,
    lock: &Lock,
    verbs: Verbs,
    told: Option<&str>,
    sizeless: &HashSet<String>,
) -> Vec<(&'static str, String)> {
    let mut first = Vec::new();
    let mut again: Vec<&Locked> = Vec::new();
    for dataset in &lock.datasets {
        if told == Some(dataset.name.as_str()) {
            continue;
        }
        match before.get(lock, &dataset.name) {
            None => first.push(dataset),
            Some(old) if old != dataset => again.push(dataset),
            Some(_) => {}
        }
    }
    let shown = first.len().min(FIRST_PINS);
    let mut lines: Vec<(&'static str, String)> = first[..shown]
        .iter()
        .map(|dataset| (verbs.pinned, first_pin(output, dataset, sizeless)))
        .collect();
    again.extend(&first[shown..]);
    let files: usize = again.iter().map(|dataset| dataset.files.len()).sum();
    let which = match again.as_slice() {
        [] => None,
        [only] => Some(only.name.clone()),
        many => Some(units::count(many.len() as u64, "dataset", "datasets")),
    };
    if let Some(which) = which {
        lines.push((
            verbs.locked,
            format!(
                "{which} · {} in {}",
                units::count(files as u64, "file", "files"),
                lock::FILE
            ),
        ));
    }
    for dataset in before.dropped(lock) {
        lines.push((
            verbs.dropped,
            format!("{} from {}", dataset.name, lock::FILE),
        ));
    }
    lines
}

/// a dataset data.lock pins for the first time: its name, then what it resolved to, its title,
/// and how many files and bytes it pins. `sizeless` labels, as `<dataset>/<path>`, the files whose
/// size only a download can tell, as a dry run does not.
fn first_pin(output: &Output, locked: &Locked, sizeless: &HashSet<String>) -> String {
    let resolved = Reference::parse(&locked.resolved).map_or_else(
        |_| locked.resolved.clone(),
        |parsed| short_revision(parsed.reference.shown()),
    );
    let mut facts = vec![resolved];
    facts.extend(
        locked
            .title
            .as_deref()
            .map(|title| output.clip(title, TITLE)),
    );
    facts.push(units::count(locked.files.len() as u64, "file", "files"));
    let known = !locked
        .files
        .iter()
        .any(|file| sizeless.contains(&format!("{}/{}", locked.name, file.path)));
    facts.push(if known {
        units::size(
            locked
                .files
                .iter()
                .map(|file| file.size)
                .fold(0, u64::saturating_add),
        )
    } else {
        "size unknown".to_owned()
    });
    format!(
        "{}, new in {}: {}",
        locked.name,
        lock::FILE,
        facts.join(" · ")
    )
}

pub(super) use draft::filter;

/// resolves a reference, with progress or plain lines while its source takes its time, listing
/// only `folders` where they hold every file wanted.
pub(super) async fn resolve(
    output: &Output,
    reference: &Reference,
    net: &Net,
    folders: Option<&[String]>,
) -> Result<Listing, Error> {
    let _waiting =
        (!resolves_locally(reference)).then(|| output.wait("Resolving", reference.to_string()));
    reference.resolve_in(net, folders).await
}

/// whether a reference resolves without asking anyone, as a plain link does. such a link may
/// also carry a token, which no progress line should print.
pub(super) fn resolves_locally(reference: &Reference) -> bool {
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
    let url = draft::link(dataset, file)?;
    Ok(make_job(lock, dataset, file, &url))
}

fn make_job(lock: &Lock, dataset: &str, file: &LockedFile, url: &Url) -> Job {
    let pinned = lock.get(dataset);

    Job {
        fetch: Fetch {
            expect: Expect {
                sha256: Some(file.sha256),
                md5: file.md5,
                size: Some(file.size),
            },
            revalidate: false,
            unpack: false,
            ..source::fetch_plan(url)
        },
        label: format!("{dataset}/{}", file.path),
        dataset: Some(dataset.to_owned()),
        again: pinned.map(|locked| Again {
            resolved: locked.resolved.clone(),
            path: file.path.clone(),
            alone: locked.files.len() == 1,
        }),
    }
}

fn in_cache(cache: &Cache, fetch: &Fetch) -> bool {
    in_cache_where(cache, fetch, |_| true)
}

fn in_cache_where(cache: &Cache, fetch: &Fetch, usable: impl Fn(&Sha256) -> bool) -> bool {
    let expect = &fetch.expect;
    match (expect.sha256, expect.md5) {
        (Some(digest), _) => usable(&digest) && cache.find(&digest, expect.size).is_some(),
        (None, Some(md5)) => {
            cache
                .stated_md5(&md5, fetch.url.as_str())
                .is_some_and(|(digest, size)| {
                    usable(&digest)
                        && expect.size.is_none_or(|expected| expected == size)
                        && cache.find(&digest, Some(size)).is_some()
                })
        }
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

/// a reference pinned to a full 40-digit commit, cut to 7 digits as git shows them. data.lock
/// keeps all 40.
fn short_revision(reference: String) -> String {
    match reference.rsplit_once('@') {
        Some((head, revision))
            if revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            format!("{head}@{}", &revision[..7])
        }
        _ => reference,
    }
}

/// prints what a reference resolved to, before any file of it downloads, then what its source
/// warned about.
pub(super) fn tell_resolved(output: &Output, listing: &Listing) {
    tell_facts(output, listing);
    for warning in &listing.warnings {
        output.warn(warning);
    }
}

fn tell_facts(output: &Output, listing: &Listing) {
    // a plain link has nothing to tell before its bytes arrive
    if matches!(listing.resolved, Reference::Url(_)) {
        return;
    }
    let mut facts = vec![short_revision(listing.resolved.shown())];
    facts.extend(
        listing
            .title
            .as_deref()
            .map(|title| output.clip(title, TITLE)),
    );
    let files = units::count(listing.files.len() as u64, "file", "files");
    facts.push(if listing.within.is_empty() {
        files
    } else {
        format!("{files} in {}", listing.within.join(", "))
    });
    let size = listing
        .files
        .iter()
        .try_fold(0, |total: u64, file| Some(total.saturating_add(file.size?)));
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

/// prints each dataset that moved to another version, beside data.lock as it was `before`, with
/// `verb`, such as `Upgraded`, and returns their names. files `guessed` by a dry run, as
/// `<dataset>/<path>`, are not counted as changed, since only their bytes could tell.
pub(super) fn tell_upgrades(
    output: &Output,
    before: &Before,
    new: &Lock,
    verb: &str,
    guessed: &HashSet<String>,
) -> Vec<String> {
    let mut moved = Vec::new();
    for dataset in &new.datasets {
        let Some(before) = before.get(new, &dataset.name) else {
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
        let unsure = |path: &str| guessed.contains(&format!("{}/{path}", dataset.name));
        let added = now.keys().filter(|path| !was.contains_key(*path)).count();
        let removed = was.keys().filter(|path| !now.contains_key(*path)).count();
        let changed = now
            .iter()
            .filter(|(path, sha256)| {
                !unsure(path) && was.get(*path).is_some_and(|before| before != *sha256)
            })
            .count();
        if before.resolved == dataset.resolved
            && added + removed + changed == 0
            && now.keys().any(|path| unsure(path))
        {
            continue;
        }
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
        output.step(verb, &format!("{} · {}", dataset.name, facts.join(" · ")));
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

    #[test]
    fn a_full_commit_is_shown_short_and_anything_else_as_it_is() {
        let full = "0bda0ce801be0fa2f464ff845a9d5ceae99aad7d";
        assert_eq!(
            short_revision(format!("hf:datasets/scikit-learn/iris@{full}")),
            "hf:datasets/scikit-learn/iris@0bda0ce"
        );
        for kept in [
            "zenodo:3608990",
            "figshare:34744689@1",
            "hf:datasets/a/b@main",
            "kaggle:uciml/iris@2",
        ] {
            assert_eq!(short_revision(kept.to_owned()), kept);
        }
    }

    fn context() -> Context {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
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
            extract: true,
            resolved: "zenodo:2".to_owned(),
            title: None,
            license: None,
            doi: None,
            access: source::Access::Unknown,
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
            ..Lock::default()
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
        let url = Url::parse("https://example.org/data.csv").unwrap();
        let fetch = |expect| Fetch {
            expect,
            ..Fetch::new(url.clone())
        };
        assert!(!in_cache(&cache, &fetch(Expect::default())));
        let sha = Expect {
            sha256: Some(stored.digest),
            size: Some(4),
            ..Expect::default()
        };
        assert!(in_cache(&cache, &fetch(sha)));
        assert!(!in_cache(
            &cache,
            &fetch(Expect {
                size: Some(5),
                ..sha
            })
        ));
        let md5_only = fetch(Expect {
            md5: Some(md5),
            ..Expect::default()
        });
        assert!(!in_cache(&cache, &md5_only));
        cache
            .save_stated_md5(&md5, url.as_str(), &stored.digest, 4)
            .unwrap();
        assert!(in_cache(&cache, &md5_only));
        assert!(!in_cache_where(&cache, &md5_only, |digest| *digest != stored.digest));
        assert!(!in_cache_where(&cache, &fetch(sha), |_| false));
        assert!(
            !in_cache(
                &cache,
                &fetch(Expect {
                    size: Some(1 << 30),
                    ..md5_only.expect
                })
            ),
            "a hint for other bytes than the listed size is not those bytes"
        );
        let elsewhere = Fetch {
            expect: md5_only.expect,
            ..Fetch::new(Url::parse("https://example.org/other.csv").unwrap())
        };
        assert!(
            !in_cache(&cache, &elsewhere),
            "another link may serve other bytes"
        );
        cache.drop_object(&stored.digest).unwrap();
        assert!(!in_cache(&cache, &md5_only));
        assert!(!in_cache(&cache, &fetch(sha)));
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
            ..Lock::default()
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
            ("not-a-reference", "data.toml:2"),
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
            ..Lock::default()
        };
        assert!(
            runner
                .write_lock(&Before::whole(None), &lock)
                .unwrap()
                .is_some()
        );
        let path = project.root.join(lock::FILE);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
            .unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(
            runner
                .write_lock(&Before::whole(Some(lock.clone())), &lock)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        assert_eq!(project.lock().unwrap(), Some(lock.clone()));
        assert!(
            runner
                .write_lock(&Before::whole(Some(lock.clone())), &Lock::default())
                .unwrap()
                .is_some()
        );
        assert_eq!(project.lock().unwrap(), Some(Lock::default()));
    }

    #[test]
    fn upgrade_reports_ignore_metadata_only_changes_and_new_datasets() {
        let context = context();
        let old = Lock {
            datasets: vec![locked("changed"), locked("metadata"), locked("revision")],
            ..Lock::default()
        };
        let mut new = old.clone();
        new.datasets[0].files[0].sha256 = digest::sha256(b"changed");
        new.datasets[1].timestamp = "2026-02-01T00:00:00Z".to_owned();
        new.datasets[2].resolved = "zenodo:3".to_owned();
        new.datasets.push(locked("new"));
        assert_eq!(
            tell_upgrades(
                &context.output,
                &Before::whole(Some(old.clone())),
                &new,
                "Upgraded",
                &HashSet::new()
            ),
            ["changed", "revision"]
        );
        assert_eq!(
            tell_upgrades(
                &context.output,
                &Before::whole(Some(old.clone())),
                &old,
                "Upgraded",
                &HashSet::new()
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_first_pin_names_what_it_pinned_and_many_are_counted() {
        let context = context();
        let mut typo = locked("climate-precip");
        typo.resolved = "zenodo:8014494".to_owned();
        typo.title = Some(
            "Abb. 9 in Natur- und Fotoreise zu den Heuschrecken Mallorcas, Teil zwei".to_owned(),
        );
        let lock = Lock {
            datasets: vec![typo, locked("kept")],
            ..Lock::default()
        };
        let before = Before::whole(Some(Lock {
            datasets: vec![locked("gone"), locked("kept")],
            ..Lock::default()
        }));
        let sized = HashSet::new();
        assert_eq!(
            lock_lines(&context.output, &before, &lock, Verbs::DONE, None, &sized),
            [
                (
                    "Pinned",
                    "climate-precip, new in data.lock: zenodo:8014494 · Abb. 9 in Natur- und Fotoreise zu den Heuschrecken... · 1 file · 4 B".to_owned()
                ),
                ("Removed", "gone from data.lock".to_owned()),
            ]
        );
        assert_eq!(
            lock_lines(
                &context.output,
                &before,
                &lock,
                Verbs::DONE,
                Some("climate-precip"),
                &sized
            ),
            [("Removed", "gone from data.lock".to_owned())],
            "the dataset the last line tells about is left out"
        );
        let many = Lock {
            datasets: (0..25).map(|n| locked(&format!("set-{n:02}"))).collect(),
            ..Lock::default()
        };
        let sizeless = HashSet::from(["set-00/data.csv".to_owned()]);
        let lines = lock_lines(
            &context.output,
            &Before::whole(None),
            &many,
            Verbs::WOULD,
            None,
            &sizeless,
        );
        assert_eq!(lines.len(), 21);
        assert_eq!(
            lines[0].1,
            "set-00, new in data.lock: zenodo:2 · 1 file · size unknown"
        );
        assert!(lines[..20].iter().all(|(verb, _)| *verb == "Would pin"));
        assert_eq!(
            lines[20],
            ("Would lock", "5 datasets · 5 files in data.lock".to_owned())
        );
    }

    #[test]
    fn a_dry_upgrade_does_not_count_guessed_files_as_changed() {
        let context = context();
        let old = Lock {
            datasets: vec![locked("guessed"), locked("moved")],
            ..Lock::default()
        };
        let mut new = old.clone();
        new.datasets[0].files[0].sha256 = digest::sha256(b"a stand-in");
        new.datasets[1].resolved = "zenodo:3".to_owned();
        new.datasets[1].files[0].sha256 = digest::sha256(b"another stand-in");
        let guessed = HashSet::from(["guessed/data.csv".to_owned(), "moved/data.csv".to_owned()]);
        assert_eq!(
            tell_upgrades(
                &context.output,
                &Before::whole(Some(old)),
                &new,
                "Would upgrade",
                &guessed
            ),
            ["moved"],
            "only a new version is told when the bytes are a guess"
        );
    }

    #[test]
    fn a_source_warning_is_told_with_its_listing() {
        let context = context();
        let url = Url::parse("https://example.org/data.csv").unwrap();
        let listing = Listing {
            within: Vec::new(),
            access: source::Access::Unknown,
            resolved: Reference::Url(url),
            title: None,
            license: None,
            doi: None,
            files: Vec::new(),
            unavailable: Vec::new(),
            warnings: vec!["osf:ezcuj has 2 components".to_owned()],
        };
        tell_resolved(&context.output, &listing);
        assert_eq!(context.output.warned(), ["osf:ezcuj has 2 components"]);
    }

    #[test]
    fn kept_entries_move_into_the_new_lock_and_still_compare_with_the_old_one() {
        let mut changed = locked("b");
        changed.resolved = "zenodo:3".to_owned();
        let old = Lock {
            datasets: vec![locked("a"), locked("b"), locked("c")],
            ..Lock::default()
        };
        let mut others = Lock {
            datasets: vec![locked("b"), locked("c")],
            ..Lock::default()
        };
        let (lock, kept) = settled(
            &mut others,
            vec![locked("a")],
            vec![changed.clone(), locked("d")],
        );
        assert_eq!(
            lock.datasets
                .iter()
                .map(|dataset| dataset.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "d"]
        );
        assert_eq!(kept, [true, false, false]);
        let before = Before {
            existed: true,
            others,
            kept,
        };
        for name in ["a", "b", "c", "d", "e"] {
            assert_eq!(before.get(&lock, name), old.get(name), "{name}");
        }
        assert_eq!(
            before
                .dropped(&lock)
                .map(|dataset| dataset.name.as_str())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert!(!before.holds(&lock));
        let (same, kept) = settled(
            &mut Lock {
                datasets: vec![locked("b")],
                ..Lock::default()
            },
            vec![locked("a")],
            vec![locked("b")],
        );
        let unchanged = Before {
            existed: true,
            others: Lock {
                datasets: vec![locked("b")],
                ..Lock::default()
            },
            kept,
        };
        assert!(unchanged.holds(&same));
        let absent = Before {
            existed: false,
            ..unchanged
        };
        assert!(!absent.holds(&same), "a missing data.lock is written");
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
