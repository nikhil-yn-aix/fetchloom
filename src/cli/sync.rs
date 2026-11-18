//! the loop at the heart of a project: resolve what data.toml changed, learn every file's hash,
//! put what the lock names in the cache, then write data.lock.

pub(in crate::cli) mod cache_budget;
pub(super) mod disk;
mod draft;
mod fill;
pub(super) mod place;
pub(super) mod placed_archives;
pub(super) mod rename;
pub(super) mod unpack;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Instant, SystemTime};

use tokio::task::JoinSet;
use url::Url;

use self::disk::Survey;
use self::draft::Draft;
use self::place::{Download, Elsewhere, Placement, Placing, Preview};
use self::placed_archives::PlacedArchives;
use self::rename::Rename;
use super::Context;
use super::fetch::data_folder::DataFolder;
use super::fetch::{self, Again, Job};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch, Fetched, Net, Origin, estimate};
use crate::output::{Output, Warning};
use crate::plan;
use crate::project::lock::archives::{self, Archive};
use crate::project::lock::{self, Drift, Lock, Locked, LockedFile, Waiting};
use crate::project::manifest::{Entry, Manifest, Source};
use crate::project::state::Synced;
use crate::project::{Project, native};
use crate::source::{self, Listing, Parsed, Reference, Remote, local};
use crate::units;

/// the most of a title the `Resolved` line shows.
const TITLE: usize = 50;

/// the most files an error about missing files names; it counts the rest.
const NAMED: usize = 5;

/// which datasets move to their newest version.
pub(super) enum Upgrade {
    Nothing,
    All,
    Only(BTreeSet<String>),
}

impl Upgrade {
    pub(super) fn includes(&self, name: &str) -> bool {
        match self {
            Upgrade::Nothing => false,
            Upgrade::All => true,
            Upgrade::Only(names) => names.contains(name),
        }
    }
}

/// the datasets a run works on, every one or those a person named; a named run touches nothing of
/// any other. JSON reports it as `selected`: the names, or null for every dataset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    All,
    Only(BTreeSet<String>),
}

impl Scope {
    pub(crate) fn includes(&self, name: &str) -> bool {
        match self {
            Scope::All => true,
            Scope::Only(names) => names.contains(name),
        }
    }
}

impl serde::Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Scope::All => serializer.serialize_none(),
            Scope::Only(names) => serializer.collect_seq(names),
        }
    }
}

/// a dataset a command has in hand already, so relocking need not resolve it.
pub(super) enum Given {
    /// what a source listed for the dataset named first.
    Listing(String, Listing),
    /// the whole pin of files imported from this machine, whose archives relocking unpacks to
    /// learn what they hold.
    Locked(Locked),
}

pub(super) struct Loop<'a> {
    pub(super) context: &'a Context,
    pub(super) project: &'a Project,
    pub(super) net: Arc<Net>,
    pub(super) cache: Cache,
    /// archives this run unpacked, whose own bytes the cache may drop once their files are in.
    unpacked: Mutex<Vec<Sha256>>,
    /// excludes of each dataset this run listed that matched no listed file, for placing's warning.
    unlisted: Mutex<plan::Unlisted>,
    /// datasets the command's last lines tell about, whose lock and placement lines wait for `-v`.
    told: BTreeSet<String>,
    pub(super) scope: Scope,
    /// whether this run wrote data.lock, after which a cancel no longer leaves all as it was.
    wrote_lock: AtomicBool,
    /// whether a person said yes despite a short disk, which also covers every download's size.
    roomy: AtomicBool,
    /// the run as its questions and warnings name it, as `this sync`.
    run: &'static str,
    /// whether placing follows relocking, so the first downloads tell what placing adds.
    places: AtomicBool,
    /// whether relocking downloads the datasets only their sources' digests pin, as a sync does.
    fills: bool,
    /// whether placing hashes every cached file it takes first, as `sync --verify` asks.
    verifies: bool,
    /// what relocking told ahead and fetched, which placing's downloads finish telling.
    ahead: Mutex<Option<Ahead>>,
    /// the files of pins by what their sources state that relocking offline could not fill, which
    /// the run's refusal names with every other file it lacks.
    unfilled: Mutex<Vec<fill::Unfilled>>,
    /// the outputs whose files no cache or mirror the run reaches holds, which placing leaves
    /// out and the run names once, as made on another machine.
    made_elsewhere: Mutex<BTreeSet<String>>,
}

/// downloads a run told before its first batch: the hashes placing downloads later, and what
/// the first batch fetched since it started, so the run ends with one line for all of them.
struct Ahead {
    digests: HashSet<Sha256>,
    fetched: Vec<Fetched>,
    started: Instant,
}

/// what a batch of downloads fetched, and when it told placing's ahead, their hashes and the
/// moment it started.
#[derive(Default)]
struct Batch {
    fetched: Vec<Fetched>,
    ahead: Option<(HashSet<Sha256>, Instant)>,
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
            unlisted: Mutex::default(),
            told: BTreeSet::new(),
            scope: Scope::All,
            wrote_lock: AtomicBool::new(false),
            roomy: AtomicBool::new(false),
            run: "this sync",
            places: AtomicBool::new(false),
            fills: false,
            verifies: false,
            ahead: Mutex::default(),
            unfilled: Mutex::default(),
            made_elsewhere: Mutex::default(),
        }
    }

    /// relocking downloads each dataset in scope only its source's digests pin, and pins it whole.
    pub(super) fn filling(mut self) -> Self {
        self.fills = true;
        self
    }

    /// placing hashes every cached file it takes first, when `verify` says so, so a file changed
    /// in the cache is fetched again rather than placed.
    pub(super) fn verifying(mut self, verify: bool) -> Self {
        self.verifies = verify;
        self
    }

    /// says placing follows the next relock, so downloads that learn hashes tell placing's too.
    pub(super) fn placing_next(&self) {
        self.places.store(true, Ordering::Relaxed);
    }

    /// names the run `run` in its questions and warnings, as `this add`.
    pub(super) fn running(mut self, run: &'static str) -> Self {
        self.run = run;
        self
    }

    /// the run adds `name`, which the command's last lines tell about, and works on the datasets
    /// it adds alone, since an add touches no other.
    pub(super) fn adding(mut self, name: &str) -> Self {
        self.told.insert(name.to_owned());
        self.scope = match self.scope {
            Scope::Only(mut names) => {
                names.insert(name.to_owned());
                Scope::Only(names)
            }
            Scope::All => Scope::Only(BTreeSet::from([name.to_owned()])),
        };
        self
    }

    /// the run works on `scope` alone, keeping every other dataset's entry and files as they are.
    pub(super) fn within(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// the datasets the person named: the scope's, none for a run of every dataset.
    pub(super) fn named(&self) -> BTreeSet<String> {
        match &self.scope {
            Scope::Only(names) => names.clone(),
            Scope::All => BTreeSet::new(),
        }
    }

    /// makes the data folder match `lock` from `survey` of it, downloading only what placing lacks;
    /// `synced` names data.toml and data.lock as this run leaves them.
    pub(super) async fn place(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        plan: plan::Plan<'_>,
        force: bool,
        survey: Survey,
        synced: Option<Synced>,
    ) -> Result<Placement, Error> {
        // what hashing found changed, as `--verify` does, holds for the datasets placed whole
        let changed = (survey.damaged.clone(), survey.elsewhere.clone());
        // preparing waits on the heap, so the run's own future, which holds what the survey found
        // changed, stays small
        match Box::pin(self.prepare(manifest, lock, plan, force, survey)).await {
            Ok(mut placing) => {
                self.leave_out(&mut placing, self.elsewhere(manifest, lock));
                let placement = self.apply(manifest, lock, placing, synced)?;
                self.warn_elsewhere(&placement.elsewhere);
                Ok(placement)
            }
            // a sync still places the datasets it has whole, of those named or of all, and names
            // the rest
            Err(error) if lacks_files(&error) => {
                let placed = self.place_whole(manifest, lock, force, synced, changed);
                match &placed {
                    Ok(Some(partial)) => self.warn_elsewhere(&partial.placement.elsewhere),
                    _ => self.warn_elsewhere(&self.elsewhere(manifest, lock)),
                }
                match placed {
                    Ok(Some(partial)) => Err(self.partly(error, partial)),
                    Ok(None) => Err(self.before_placing(error)),
                    Err(other) if other.kind == Kind::Interrupted => {
                        Err(self.before_placing(error))
                    }
                    Err(other) => Err(self.before_placing(error.detail(format!(
                        "could not place the datasets this machine has whole either: {}",
                        other.message
                    )))),
                }
            }
            Err(error) => Err(self.before_placing(error)),
        }
    }

    /// the outputs this run leaves out, as no cache or mirror it reaches holds their files, as
    /// `lock` pins them.
    fn elsewhere(&self, manifest: &Manifest, lock: &Lock) -> Vec<Elsewhere> {
        self.made_elsewhere
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|name| Elsewhere::new(name, self.shown(manifest, name), lock))
            .collect()
    }

    /// leaves out of `placing` the outputs of `elsewhere`, whose files this machine lacks, and
    /// notes its gone outputs whose files the cache lacks with no mirror to ask, so their ways on
    /// name no sync, which would put back nothing.
    fn leave_out(&self, placing: &mut Placing<'_>, elsewhere: Vec<Elsewhere>) {
        placing.leave_out(elsewhere);
        placing.note_lost(
            |digest, size| self.cache.find(digest, Some(size)).is_some(),
            !self.context.mirrors.mirrors.list.is_empty(),
        );
    }

    /// the folder of `name` as a command run here names it.
    fn shown(&self, manifest: &Manifest, name: &str) -> String {
        super::folder_from_here(&self.project.root, &plan::folder(manifest, name))
    }

    /// warns of each output of `elsewhere`, made on another machine, with both ways to bring
    /// its files here: once per output and pin, though in every json document, which a script
    /// reads alone.
    fn warn_elsewhere(&self, elsewhere: &[Elsewhere]) {
        let output = &self.context.output;
        let mirrored = !self.context.mirrors.mirrors.list.is_empty();
        for left in elsewhere.iter().filter(|left| output.json() || !left.told) {
            let said = local::made_elsewhere(&[(&left.name, &left.shown)], mirrored, left.here);
            output.warn_with(
                Warning::OutputElsewhere,
                &said.message,
                said.help.as_deref().unwrap_or_default(),
            );
        }
    }

    /// `missing` without the files of outputs, which no link fetches, so that what this run
    /// could not fill it lacks for good: each such output is noted as made on another machine,
    /// for placing to leave out and the run to name once.
    fn set_aside<'f>(
        &self,
        manifest: &Manifest,
        missing: Vec<(String, &'f LockedFile)>,
    ) -> Vec<(String, &'f LockedFile)> {
        let (made, rest): (Vec<_>, Vec<_>) = missing
            .into_iter()
            .partition(|(dataset, _)| manifest.datasets.get(dataset).is_some_and(Entry::is_output));
        self.made_elsewhere
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(made.into_iter().map(|(dataset, _)| dataset));
        rest
    }

    /// `error` with the datasets the run placed whole and those that wait, in its details and,
    /// for `--json`, in its result, shaped as a sync's.
    fn partly(&self, error: Error, partial: Partial) -> Error {
        let Partial {
            placed,
            waiting,
            placement,
        } = partial;
        let error = error.detail(format!(
            "placed {}, whose files are all on this machine",
            placed.join(", ")
        ));
        let explained = self
            .made_elsewhere
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let unexplained: Vec<&str> = waiting
            .iter()
            .map(String::as_str)
            .filter(|name| !explained.contains(*name))
            .collect();
        drop(explained);
        let error = if unexplained.is_empty() {
            error
        } else {
            error.detail(format!(
                "{} {} for files this run could not get",
                unexplained.join(", "),
                if unexplained.len() == 1 {
                    "waits"
                } else {
                    "wait"
                }
            ))
        };
        error.with_result(serde_json::json!({
            "project": self.project.root,
            "selected": self.scope,
            "datasets": placed,
            "waiting": waiting,
            "lock_changed": self.wrote_lock.load(Ordering::Relaxed),
            "placement": placement,
        }))
    }

    /// places the datasets of `lock` in the run's scope whose files the cache holds whole, once
    /// the run could not get the rest: the names it placed, those that wait and what placing did,
    /// or none when no dataset is whole. `changed` holds the files the run's survey found damaged
    /// and those changed through a link elsewhere, which a look at the folder alone misses.
    fn place_whole(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        force: bool,
        synced: Option<Synced>,
        changed: (HashSet<String>, HashSet<String>),
    ) -> Result<Option<Partial>, Error> {
        let mut survey = Survey::read(
            self.project,
            &disk::Folders::of(manifest),
            self.context.threads,
            &self.context.stop,
        )?;
        survey.check_doubted(&self.project.root, &self.cache)?;
        let (damaged, elsewhere) = changed;
        survey.damaged.extend(damaged);
        survey.elsewhere.extend(elsewhere);
        let plan = plan::from_cache(manifest, lock, &self.cache)?;
        let placing = Placing::from_plan(self.project, manifest, plan, force, &self.scope, survey)?;
        // a dataset whose bytes a download that failed was to bring waits with it, as one whose
        // pin by what its source states this run could not fill does
        let unfilled = self.unfilled.lock().unwrap_or_else(PoisonError::into_inner);
        let waiting: BTreeSet<String> = placing
            .short(|digest, size| self.cache.find(digest, Some(size)).is_some())
            .into_iter()
            .chain(placing.unknown().into_iter().map(|(dataset, _)| dataset))
            .chain(unfilled.iter().map(|file| file.dataset.clone()))
            .collect();
        drop(unfilled);
        // one waiting for its file list has nothing to place either, which the run warns of
        let whole: BTreeSet<String> = manifest
            .datasets
            .iter()
            .filter(|(name, entry)| {
                entry.place
                    && self.scope.includes(name)
                    && !waiting.contains(*name)
                    && lock.waits(name).is_none()
            })
            .map(|(name, _)| name.clone())
            .collect();
        if whole.is_empty() || waiting.is_empty() {
            return Ok(None);
        }
        let survey = placing.into_survey();
        let plan = plan::from_cache(manifest, lock, &self.cache)?;
        let scope = Scope::Only(whole.clone());
        let mut placing = Placing::from_plan(self.project, manifest, plan, force, &scope, survey)?;
        self.leave_out(&mut placing, self.elsewhere(manifest, lock));
        let placement = self.apply(manifest, lock, placing, synced)?;
        Ok(Some(Partial {
            placed: whole.into_iter().collect(),
            waiting: waiting.into_iter().collect(),
            placement,
        }))
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
        self.warn_doubtful_paths(manifest, &plan);
        let mut placing =
            Placing::from_plan(self.project, manifest, plan, force, &self.scope, survey)?;
        placing.listed(self.unlisted());
        // an archive whose files the data folder holds in place needs neither a list from a
        // mirror nor its own bytes
        let found = self.placed_archives(manifest, lock, &placing)?;
        found.tell_unproven(&self.context.output, self.net.offline().enabled());
        if !found.is_empty() {
            found.record(&self.cache, &self.context.output)?;
            placing = self.placed_again(manifest, lock, force, placing)?;
        }
        if self.verifies {
            placing.check_cached(&self.cache, &self.context.output)?;
        }
        self.ask_room(lock, &placing)?;
        // the first failure that leaves files for a later run waits until the rest is in
        let mut failed = None;
        let mut unknown = archives::of(lock, placing.unknown());
        let unpacks = !unknown.is_empty();
        if unpacks {
            unknown = fetch::mirrors::trees(self.context, &self.net, &self.cache, unknown)
                .await
                .map_err(|error| self.as_left(error))?;
        }
        let mut missing = None;
        if self.net.offline().enabled() || self.context.mirrors.mirrors.only {
            if unpacks {
                placing = self.placed_again(manifest, lock, force, placing)?;
            }
            let found = self
                .missing_after_fill(manifest, &placing, &unknown)
                .await?;
            failed = first_left(
                failed,
                self.refuse_lacking(manifest, lock, &unknown, &found),
            )?;
            // with no archive to unpack first, placing and what it lacks stay as they are
            missing = (!unpacks).then_some(found);
        }
        if unpacks {
            let unpacked = self.unpack_locked(manifest, lock, &unknown).await;
            failed = first_left(failed, unpacked)?;
            placing = self.placed_again(manifest, lock, force, placing)?;
            // the copies of what the archives unpacked to count only now that their files are known
            self.ask_room(lock, &placing)?;
        }
        let missing = match missing {
            Some(missing) => missing,
            None => self.missing_after_fill(manifest, &placing, &[]).await?,
        };
        let (unpacking, files): (Vec<_>, Vec<_>) = missing
            .into_iter()
            .partition(|(_, file)| file.unpacks_to.is_some());
        let cached = crate::place::run_all(&files, self.context.threads, |(_, file)| {
            place::cached_at(&self.cache, &file.sha256, file.size)
        });
        let mut available = Vec::with_capacity(files.len());
        for ((dataset, file), cached) in files.iter().zip(cached) {
            // no download brings a SHA-256's bytes at another size than the cache holds them
            if let place::Cached::Other(actual) = cached {
                let label = format!("{dataset}/{}", file.path);
                return Err(place::mispinned(&label, file.size, actual));
            }
            available.push(cached == place::Cached::Here);
        }
        let downloaded = self
            .download_prepared(manifest, lock, &files, Some(&available))
            .await;
        failed = first_left(failed, downloaded.map(drop))?;
        let unpacked = self
            .unpack_locked(manifest, lock, &archives::of(lock, unpacking))
            .await;
        match first_left(failed, unpacked)? {
            Some(error) => Err(*error),
            None => Ok(placing),
        }
    }

    /// the lists of files of the archives `placing` knows none of, as found where placing puts
    /// their files and checked against the trees data.lock pins.
    fn placed_archives(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        placing: &Placing<'_>,
    ) -> Result<PlacedArchives, Error> {
        let unknown = archives::of(lock, placing.unknown());
        if unknown.is_empty() {
            return Ok(PlacedArchives::default());
        }
        PlacedArchives::find(
            self.project,
            (manifest, lock),
            placing.survey(),
            &unknown,
            (self.context.threads, &self.context.stop),
        )
    }

    /// `placing` made again from what the cache now knows, as once an archive's list of files
    /// came, so its files are placed too.
    fn placed_again<'p>(
        &'p self,
        manifest: &'p Manifest,
        lock: &'p Lock,
        force: bool,
        placing: Placing<'p>,
    ) -> Result<Placing<'p>, Error> {
        let survey = placing.into_survey();
        let mut placing = Placing::new(
            self.project,
            manifest,
            lock,
            &self.cache,
            force,
            &self.scope,
            survey,
        )?;
        placing.listed(self.unlisted());
        Ok(placing)
    }

    /// takes in from the data folder, the reuse folders and the mirrors what `placing` and the
    /// archives `unknown`, whose lists of files no mirror gave, need, and returns what placing
    /// still lacks: whole files, and archives whose files it lacks.
    async fn missing_after_fill<'f>(
        &self,
        manifest: &Manifest,
        placing: &Placing<'f>,
        unknown: &[(String, Archive<'_>)],
    ) -> Result<Vec<(String, &'f LockedFile)>, Error> {
        let folder = DataFolder::found(self.project, manifest, placing.holding());
        fetch::mirrors::fill(self.context, &self.net, &self.cache, &folder, || {
            let mut wanted = placing.wanted(&self.cache);
            for (dataset, archive) in unknown {
                let files = archive.files().into_iter();
                wanted.extend(files.filter(|file| !self.holds(file)).map(|file| {
                    crate::mirror::Wanted {
                        digest: file.sha256,
                        size: file.size,
                        label: format!("{dataset}/{}", file.path),
                        link: (!file.url.is_empty()).then(|| file.url.clone()),
                    }
                }));
            }
            wanted
        })
        .await
        .map_err(|error| self.as_left(error))?;
        Ok(self.set_aside(manifest, placing.missing(&self.cache)?))
    }

    /// refuses at once every file that no download may fetch and the cache still lacks: of the
    /// archives `unknown`, whose lists of files no mirror gave, and of `missing`, what placing
    /// lacks. a run that may ask no source thus names them all, as its dry run does, not only
    /// those of the first step that needs one.
    fn refuse_lacking<'f>(
        &self,
        manifest: &Manifest,
        lock: &'f Lock,
        unknown: &[(String, Archive<'f>)],
        missing: &[(String, &'f LockedFile)],
    ) -> Result<(), Error> {
        let (unpacking, files): (Vec<_>, Vec<_>) = missing
            .iter()
            .cloned()
            .partition(|(_, file)| file.unpacks_to.is_some());
        let unpacking = archives::of(lock, unpacking);
        let mut seen = HashSet::new();
        let lacking: Vec<(&str, &LockedFile)> = unknown
            .iter()
            .chain(&unpacking)
            .flat_map(|(dataset, archive)| {
                let files = archive.files().into_iter();
                files.map(move |file| (dataset.as_str(), file))
            })
            .chain(
                files
                    .iter()
                    .map(|(dataset, file)| (dataset.as_str(), *file)),
            )
            .filter(|(dataset, file)| {
                !self.holds(file) && seen.insert((*dataset, file.path.as_str()))
            })
            .collect();
        self.refuse_unfetchable(manifest, &lacking)
    }

    /// whether the cache holds the bytes of `file`.
    fn holds(&self, file: &LockedFile) -> bool {
        self.cache.find(&file.sha256, Some(file.size)).is_some()
    }

    /// keeps the exclude patterns of `name` that match no path `listing` names, for placing.
    fn note_unlisted(&self, name: &str, source: &Source, listing: &Listing) {
        self.unlisted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), draft::unlisted(source, listing));
    }

    fn unlisted(&self) -> plan::Unlisted {
        self.unlisted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// says of each dataset `new` keeps at the version `before` pinned, though data.toml took that
    /// version out of its ref, that it stays there and which command takes the newest. an upgrade
    /// of it asked for the newest, so it says nothing.
    pub(super) fn tell_versions_kept(
        &self,
        manifest: &Manifest,
        before: &Before,
        new: &Lock,
        upgrade: &Upgrade,
    ) {
        for dataset in &new.datasets {
            let Some(old) = before.get(new, &dataset.name) else {
                continue;
            };
            let kept = old.resolved == dataset.resolved
                && !upgrade.includes(&dataset.name)
                && manifest
                    .datasets
                    .get(&dataset.name)
                    .is_some_and(|entry| keeps_version(old, &entry.source));
            if kept {
                self.context.output.hint(&format!(
                    "data.toml names no version of {}, so it stays at {}, as data.lock pins it; `{}` takes the newest",
                    dataset.name,
                    Reference::parse(&dataset.resolved).map_or_else(
                        |_| dataset.resolved.clone(),
                        |parsed| parsed.reference.shown_short()
                    ),
                    super::project::command(&format!("lock --upgrade {}", dataset.name))
                ));
            }
        }
    }

    /// puts in the cache every file of the fetched datasets `names`, as a push needs: from the
    /// data folder, else the mirrors, else their sources, unpacking an archive again for a missing
    /// file. nothing in the project changes.
    pub(super) async fn ready(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        names: &BTreeSet<&str>,
    ) -> Result<(), Error> {
        let sources = Lock {
            datasets: lock
                .datasets
                .iter()
                .filter(|locked| names.contains(locked.name.as_str()))
                .cloned()
                .collect(),
            stated: Vec::new(),
            waiting: Vec::new(),
        };
        let planned = plan::from_cache(manifest, &sources, &self.cache)?;
        let unknown: Vec<(String, &LockedFile)> = planned
            .unknown
            .iter()
            .map(|(dataset, file)| ((*dataset).to_owned(), *file))
            .collect();
        let unknown = archives::of(&sources, unknown);
        if !unknown.is_empty() {
            let unknown = fetch::mirrors::trees(self.context, &self.net, &self.cache, unknown)
                .await
                .map_err(|error| self.as_left(error))?;
            self.unpack_locked(manifest, &sources, &unknown).await?;
        }
        let planned = plan::from_cache(manifest, &sources, &self.cache)?;
        let lacking = || -> Vec<&plan::Target> {
            planned
                .targets
                .iter()
                .filter(|target| self.cache.find(&target.sha256, Some(target.size)).is_none())
                .collect()
        };
        let lacked = lacking();
        if lacked.is_empty() {
            return Ok(());
        }
        let folder = DataFolder::planned(self.project, manifest, lock, &self.cache);
        fetch::mirrors::fill(self.context, &self.net, &self.cache, &folder, || {
            lacked
                .into_iter()
                .map(|target| crate::mirror::Wanted {
                    digest: target.sha256,
                    size: target.size,
                    label: target.path.clone(),
                    link: target.link().map(str::to_owned),
                })
                .collect()
        })
        .await
        .map_err(|error| self.as_left(error))?;
        let mut seen = HashSet::new();
        let (unpacking, files): (Vec<_>, Vec<_>) = lacking()
            .into_iter()
            .map(|target| (target.dataset.to_owned(), target.file))
            .filter(|(_, file)| seen.insert(file.sha256))
            .partition(|(_, file)| file.unpacks_to.is_some());
        let unpacking = archives::of(&sources, unpacking);
        // no link fetches an import, and the data folder held none of its pinned bytes
        let imported = unpacking.iter().flat_map(|(dataset, archive)| {
            archive.files().into_iter().map(move |file| (dataset, file))
        });
        if let Some((dataset, _)) = files
            .iter()
            .map(|(dataset, file)| (dataset, *file))
            .chain(imported)
            .find(|(_, file)| file.url.is_empty())
        {
            let mirrored = !self.context.mirrors.mirrors.list.is_empty();
            if manifest.datasets.get(dataset).is_some_and(Entry::is_output) {
                let shown = self.shown(manifest, dataset);
                let (state, _) = self.project.state();
                let here = place::authored(&state).contains(dataset.as_str());
                return Err(local::made_elsewhere(&[(dataset, &shown)], mirrored, here));
            }
            let folder = plan::folder(manifest, dataset);
            return Err(
                local::imported_elsewhere(dataset, &folder, mirrored).help(format!(
                    "copy them to {folder} and push again, or run `{}` where they are",
                    super::project::command(&format!("add <folder> --as {dataset}"))
                )),
            );
        }
        self.download_prepared(manifest, &sources, &files, None)
            .await?;
        self.unpack_locked(manifest, &sources, &unpacking).await
    }

    /// puts in the cache each of `files`, files of fetched datasets downloaded whole, as a push
    /// needs an archive it sends whole: from the mirrors, else their sources.
    pub(super) async fn ready_files(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        files: &[(String, &LockedFile)],
    ) -> Result<(), Error> {
        let lacked: Vec<(String, &LockedFile)> = files
            .iter()
            .filter(|(_, file)| self.cache.find(&file.sha256, Some(file.size)).is_none())
            .cloned()
            .collect();
        if lacked.is_empty() {
            return Ok(());
        }
        fetch::mirrors::fill(
            self.context,
            &self.net,
            &self.cache,
            &DataFolder::default(),
            || {
                lacked
                    .iter()
                    .map(|(dataset, file)| crate::mirror::Wanted {
                        digest: file.sha256,
                        size: file.size,
                        label: format!("{dataset}/{}", file.path),
                        link: (!file.url.is_empty()).then(|| file.url.clone()),
                    })
                    .collect()
            },
        )
        .await
        .map_err(|error| self.as_left(error))?;
        self.download_prepared(manifest, lock, &lacked, None)
            .await
            .map(drop)
    }

    /// places what [`Loop::prepare`] readied and records `synced`, then says how to unpack each
    /// archive it placed packed, as a clone's first sync places a rar, or that came inside another.
    pub(super) fn apply(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        placing: Placing<'_>,
        synced: Option<Synced>,
    ) -> Result<Placement, Error> {
        let packed = self.packed_in(manifest, lock, &placing);
        let nested = placing.nested();
        let files = units::count(placing.files() as u64, "file", "files");
        let _placing = self.context.output.lasting("Placing", files);
        // a pin only its source's digests make waits for a sync, so no run leaves the project
        // settled while data.lock holds one
        let synced = synced.filter(|_| lock.stated.is_empty());
        let placement = placing.apply(
            &self.context.output,
            &self.context.work,
            &self.cache,
            synced,
            Some(&self.told),
            &self.context.stop,
        )?;
        for (dataset, path, why) in packed {
            self.tell_packed(manifest, dataset, path, &why);
        }
        for (dataset, path, archive) in nested {
            self.tell_nested(manifest, dataset, &path, &archive.path);
        }
        Ok(placement)
    }

    /// the lock data.toml asks for beside data.lock: changed entries in scope resolved again, files
    /// no source hashes downloaded to learn it, the rest kept; `given` is what a command has.
    pub(super) async fn relock(
        &self,
        manifest: &Manifest,
        old: Option<Lock>,
        upgrade: &Upgrade,
        given: Vec<Given>,
    ) -> Result<(Lock, Before), Error> {
        let existed = old.is_some();
        let (old, given, carried) = self.stated(manifest, old, upgrade, given, true).await?;
        let Drafted {
            kept,
            mut drafts,
            mut pinned,
            others,
            renamed,
        } = self
            .draft(manifest, old.unwrap_or_default(), upgrade, given, true)
            .await?;
        let mut jobs = Vec::new();
        let mut slots = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            for (file, job) in draft.jobs(&self.cache) {
                slots.push((index, file));
                jobs.push(job);
            }
        }
        let mut ahead = Vec::new();
        if !jobs.is_empty() {
            // a clash the listings already show stops the run before it downloads anything
            let foreseen: Vec<Locked> = drafts
                .iter()
                .map(|draft| draft.foreseen(&self.cache))
                .collect();
            plan::foresee(
                manifest,
                kept.iter().chain(&foreseen),
                &self.cache,
                &self.told,
            )?;
            if self.places.load(Ordering::Relaxed) && !self.net.offline().enabled() {
                // placing downloads each SHA-256 once
                let mut seen: HashSet<Sha256> = jobs
                    .iter()
                    .filter_map(|job| job.fetch.expect.sha256)
                    .collect();
                ahead = drafts
                    .iter()
                    .flat_map(|draft| draft.ahead(&self.cache))
                    .filter(|job| {
                        let entry = job
                            .dataset
                            .as_deref()
                            .and_then(|name| manifest.datasets.get(name));
                        entry.is_none_or(|entry| entry.place)
                            && job
                                .fetch
                                .expect
                                .sha256
                                .is_some_and(|sha256| seen.insert(sha256))
                    })
                    .collect();
            }
        }
        // the drafts whose split archives' parts were asked whether they changed
        let asked: HashSet<usize> = slots
            .iter()
            .zip(&jobs)
            .filter(|(_, job)| job.fetch.held.is_some())
            .map(|((draft, _), _)| *draft)
            .collect();
        let batch = self.download_with(jobs, ahead).await?;
        self.learn_fetched(&mut drafts, slots, &batch.fetched)?;
        if let Some((digests, started)) = batch.ahead {
            *self.ahead.lock().unwrap_or_else(PoisonError::into_inner) = Some(Ahead {
                digests,
                fetched: batch.fetched,
                started,
            });
        }
        // a part a server said is unchanged keeps its bytes out of the cache, which an archive
        // needs after all when another of its parts changed and it unpacks again
        let again: Vec<Job> = asked
            .into_iter()
            .flat_map(|draft| drafts[draft].jobs(&self.cache))
            .map(|(_, job)| job)
            .collect();
        self.download_with(again, Vec::new()).await?;
        self.unpack_drafts(manifest, &mut drafts).await?;
        self.unpack_pinned(manifest, &mut pinned).await?;
        let now = crate::units::timestamp(SystemTime::now());
        let mut drafted = drafts
            .into_iter()
            .map(|draft| draft.finish(&now))
            .collect::<Result<Vec<_>, _>>()?;
        drafted.extend(pinned.into_iter().map(|mut locked| {
            locked.timestamp.clone_from(&now);
            locked
        }));
        let (mut lock, kept) = settled(&others, kept, drafted);
        let gone = carried.back(&mut lock);
        Ok((lock, Before::new(existed, others, gone, kept, renamed)))
    }

    /// tells each draft what the download of its file in `slots` fetched, then records the blobs
    /// of git repositories those downloads brought.
    fn learn_fetched(
        &self,
        drafts: &mut [Draft],
        slots: Vec<(usize, Option<usize>)>,
        fetched: &[Fetched],
    ) -> Result<(), Error> {
        let mut learned = Vec::new();
        for ((draft, file), fetched) in slots.into_iter().zip(fetched) {
            if let Some(file) = file {
                drafts[draft].learn(file, fetched, &self.cache);
                if let Some(blob) = drafts[draft].blob(file) {
                    learned.push((blob.clone(), fetched.digest, fetched.size));
                }
            }
        }
        draft::remember_blobs(
            &self.cache,
            &learned,
            self.context.threads,
            &self.context.stop,
        )
    }

    /// the lock [`Loop::relock`] would make, found without downloading or unpacking, and that work;
    /// a hash only the work learns has a stand-in, so the lock is only for planning.
    pub(super) async fn relock_dry(
        &self,
        manifest: &Manifest,
        old: Option<Lock>,
        upgrade: &Upgrade,
        given: Vec<Given>,
    ) -> Result<DryRelock, Error> {
        let existed = old.is_some();
        let (old, given, carried) = self.stated(manifest, old, upgrade, given, false).await?;
        let Drafted {
            kept,
            mut drafts,
            pinned,
            others,
            renamed,
        } = self
            .draft(manifest, old.unwrap_or_default(), upgrade, given, false)
            .await?;
        let mut jobs = Vec::new();
        let mut unpack = Vec::new();
        for draft in &mut drafts {
            // archives this machine unpacked before are known from their records
            unpack.extend(
                draft
                    .unknown_archives(&self.cache)
                    .into_iter()
                    .map(|(_, archive)| archive.label),
            );
            jobs.extend(draft.jobs(&self.cache).into_iter().map(|(_, job)| job));
        }
        let now = crate::units::timestamp(SystemTime::now());
        let mut guessed = HashSet::new();
        let mut drafted = Vec::with_capacity(drafts.len() + pinned.len());
        for draft in drafts {
            let (locked, stood_in) = draft.stand_in(&now)?;
            guessed.extend(stood_in);
            drafted.push(locked);
        }
        for mut locked in pinned {
            unpack.extend(draft::stand_in_trees(&mut locked, &self.cache));
            locked.timestamp.clone_from(&now);
            drafted.push(locked);
        }
        let (mut lock, kept) = settled(&others, kept, drafted);
        let gone = carried.back(&mut lock);
        let before = Before::new(existed, others, gone, kept, renamed);
        Ok(DryRelock {
            lock,
            before,
            jobs,
            unpack,
            guessed,
        })
    }

    /// data.lock's unchanged or out-of-scope entries as they are, renamed ones under their new
    /// names, and drafts of the rest with every hash known without a download.
    async fn draft(
        &self,
        manifest: &Manifest,
        old: Lock,
        upgrade: &Upgrade,
        given: Vec<Given>,
        download: bool,
    ) -> Result<Drafted, Error> {
        let (mut listed, mut whole) = (Vec::new(), Vec::new());
        for given in given {
            match given {
                Given::Listing(name, listing) => listed.push((name, listing)),
                Given::Locked(locked) => whole.push(locked),
            }
        }
        let Sorted {
            mut kept,
            mut others,
            mut drafts,
        } = self.sorted(manifest, old.datasets, upgrade, &mut whole)?;
        // a dataset waiting for its list is never resolved: relocking puts its entry back
        let waiting: HashSet<&str> = old
            .waiting
            .iter()
            .map(|waiting| waiting.name.as_str())
            .collect();
        let given: HashSet<&str> = listed
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(whole.iter().map(|locked| locked.name.as_str()))
            .chain(waiting.iter().copied())
            .collect();
        let renamed = rename::carry(
            manifest,
            &self.scope,
            upgrade,
            &given,
            &mut kept,
            &mut others,
        );
        let others = Lock {
            datasets: others,
            stated: Vec::new(),
            waiting: Vec::new(),
        };
        let mut asks = Vec::new();
        for (name, entry) in &manifest.datasets {
            let source = &entry.source;
            let is_kept = kept
                .binary_search_by(|locked| locked.name.as_str().cmp(name))
                .is_ok();
            let is_whole = whole.iter().any(|given| given.name == *name);
            if self.scope.includes(name)
                && !is_kept
                && !is_whole
                && !old.stated.iter().any(|stated| stated.name() == name)
                && !waiting.contains(name.as_str())
                && !drafts.iter().any(|(drafted, _)| drafted == name)
            {
                asks.push((
                    name.clone(),
                    pinned(source, others.get(name), upgrade.includes(name)),
                ));
            }
        }
        refuse_imports(manifest, &asks, &others, &self.project.root)?;
        let added: HashSet<String> = listed.iter().map(|(name, _)| name.clone()).collect();
        let listings = self.resolve_all(manifest, asks, listed).await?;
        for (name, listing) in listings {
            let entry = &manifest.datasets[&name];
            let source = &entry.source;
            self.note_unlisted(&name, source, &listing);
            let mut draft = Draft::new(
                &name,
                entry,
                source,
                listing,
                others.get(&name),
                upgrade.includes(&name),
                &self.cache,
            )?;
            draft.recall_blobs(
                &self.cache,
                (others.get(&name), download),
                self.context.threads,
                &self.context.stop,
            )?;
            // an upgrade of an unchanged entry told its hint when it was new
            if others
                .get(&name)
                .is_none_or(|locked| locked.drift(entry).is_some())
            {
                self.hint_shards(&name, &draft, added.contains(&name));
            }
            drafts.push((name, draft));
        }
        drafts.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Drafted {
            kept,
            drafts: drafts.into_iter().map(|(_, draft)| draft).collect(),
            pinned: whole,
            others,
            renamed,
        })
    }

    /// sorts data.lock's `datasets` into kept, to replace or drop, and drafted again from their
    /// files when only `extract` changed; one a command gave in `whole` is replaced by it.
    fn sorted(
        &self,
        manifest: &Manifest,
        datasets: Vec<Locked>,
        upgrade: &Upgrade,
        whole: &mut Vec<Locked>,
    ) -> Result<Sorted, Error> {
        let mut kept: Vec<Locked> = Vec::new();
        let mut others = Vec::new();
        let mut drafts = Vec::new();
        for locked in datasets {
            // another dataset's entry stays as it is, even one data.toml changed or dropped
            if !self.scope.includes(&locked.name) {
                kept.push(locked);
                continue;
            }
            if whole.iter().any(|given| given.name == locked.name) {
                others.push(locked);
                continue;
            }
            match manifest.datasets.get(&locked.name) {
                // an import has no newer version anywhere, so even an upgrade keeps it
                Some(entry) if !upgrade.includes(&locked.name) || entry.is_import() => {
                    let source = &entry.source;
                    match locked.drift(entry) {
                        None => kept.push(locked),
                        // an import's files stand as data.lock pins them, so a switch only
                        // learns what its archives unpack to, or where the output lives
                        Some(Drift::Unpack { .. } | Drift::At)
                            if local::is_imported(&source.reference) =>
                        {
                            whole.push(draft::switched_import(entry, &locked));
                            others.push(locked);
                        }
                        Some(Drift::Unpack { .. }) => {
                            let draft = Draft::switched(entry, source, &locked)?;
                            drafts.push((locked.name.clone(), draft));
                            others.push(locked);
                        }
                        Some(Drift::At) => {
                            whole.push(draft::moved(entry, &locked));
                            others.push(locked);
                        }
                        Some(Drift::Spec) => others.push(locked),
                    }
                }
                _ => others.push(locked),
            }
        }
        Ok(Sorted {
            kept,
            others,
            drafts,
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
            let block = crate::digest::Block::sized(placed.look.size.saturating_add(1));
            let actual = crate::volume::read_unfollowed(&object)
                .and_then(|file| Sha256::of_reader(file, block));
            match actual {
                Ok(actual) if actual == placed.sha256 => {}
                Ok(_) => {
                    damaged.insert(placed.sha256);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    damaged.insert(placed.sha256);
                }
                Err(err) => return Err(place::local(&native(&self.project.root, path), &err)),
            }
        }
        Ok(damaged)
    }

    /// warns of each dataset whose `path` is likely not what was meant, as a sync and its dry run
    /// both do before placing.
    fn warn_doubtful_paths(&self, manifest: &Manifest, plan: &plan::Plan<'_>) {
        for (said, help) in plan::doubtful_paths(manifest, plan) {
            self.context
                .output
                .warn_with(Warning::PlacePathFolder, &said, &help);
        }
    }

    /// what [`Loop::place`] would do, without downloading, unpacking or writing, given the `jobs`
    /// relocking would download first; it stops where an offline or mirror-only run would, with
    /// the same refusal. files `guessed` by the dry relock that something stands at are told as
    /// files to check.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one finding of the dry run that the preview builds on"
    )]
    pub(super) async fn preview(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        force: bool,
        survey: Survey,
        mut jobs: Vec<Job>,
        damaged: &HashSet<Sha256>,
        guessed: &HashSet<String>,
    ) -> Result<Preview, Error> {
        let plan = plan::from_cache(manifest, lock, &self.cache)?;
        self.warn_doubtful_paths(manifest, &plan);
        let mut placing =
            Placing::from_plan(self.project, manifest, plan, force, &self.scope, survey)?;
        placing.listed(self.unlisted());
        // the archives whose files sit in place are known as the sync would know them, unrecorded
        let found = self.placed_archives(manifest, lock, &placing)?;
        found.tell_unproven(&self.context.output, self.net.offline().enabled());
        if !found.is_empty() {
            let survey = placing.into_survey();
            let plan = found.plan(manifest, lock, &self.cache)?;
            placing = Placing::from_plan(self.project, manifest, plan, force, &self.scope, survey)?;
            placing.listed(self.unlisted());
        }
        // what the data folder holds a sync takes from there; only that sync hashes it
        let held: HashSet<Sha256> = placing
            .holding()
            .into_iter()
            .map(|(digest, _)| digest)
            .collect();
        let mut unpack = Vec::new();
        let wanted: Vec<(String, &LockedFile)> = placing
            .unknown()
            .into_iter()
            .chain(placing.missing_as_is(&self.cache, damaged, &held)?)
            .collect();
        // no link fetches an output: the run takes its files from the reuse folders or a mirror,
        // else it leaves the output out
        let (made, wanted): (Vec<_>, Vec<_>) = wanted
            .into_iter()
            .partition(|(dataset, _)| manifest.datasets.get(dataset).is_some_and(Entry::is_output));
        let (mirrored, elsewhere) = self.made_where(manifest, lock, made).await?;
        self.leave_out(&mut placing, elsewhere);
        let archives = archives::of(lock, wanted);
        for (dataset, archive) in &archives {
            if archive.file.unpacks_to.is_some() {
                unpack.push(format!("{dataset}/{}", archive.file.path));
            }
            // an import's archive the cache holds needs no link, and has none to give
            let cached = |file: &LockedFile| {
                !damaged.contains(&file.sha256)
                    && self.cache.find(&file.sha256, Some(file.size)).is_some()
            };
            for file in archive.files().into_iter().filter(|file| !cached(file)) {
                jobs.push(job(manifest, lock, dataset, file)?);
            }
        }
        let mut seen = BTreeSet::new();
        jobs.retain(|job| seen.insert(job.label.clone()));
        let mut seen = BTreeSet::new();
        unpack.retain(|label| seen.insert(label.clone()));
        let available =
            |job: &Job| in_cache_where(&self.cache, &job.fetch, |digest| !damaged.contains(digest));
        jobs.retain(|job| !available(job));
        let elsewhere = if self.net.offline().enabled() || self.context.mirrors.mirrors.only {
            self.held_elsewhere(&archives).await?
        } else {
            HashSet::new()
        };
        let gettable = |job: &Job| available(job) || elsewhere.contains(&job.label);
        self.refuse_offline_with(&jobs, gettable)?;
        // a pin relocking could not fill is refused though nothing else lacks, as placing does
        self.refuse_unreachable(manifest, &[])?;
        self.refuse_unmirrored(jobs.iter().filter(|job| !gettable(job)))?;
        self.refuse_unasked(jobs.iter().map(|job| &job.fetch.from))?;
        // the run itself would ask before downloads the cache's disk may lack room for
        if !self.net.offline().enabled() {
            let needed = jobs
                .iter()
                .filter_map(|job| job.fetch.expect.size)
                .fold(0, u64::saturating_add);
            // a dry run makes no cache, so its disk is the one the nearest folder is on
            let free = self.cache.root().ancestors().find_map(crate::space::free);
            if crate::space::short(needed, free).is_some() {
                super::warn_short(
                    &self.context.output,
                    self.run,
                    (needed, free.unwrap_or_default()),
                    &format!("the cache at {}", units::path(self.cache.root())),
                );
            }
        }
        let mut preview = placing.preview(guessed);
        self.warn_elsewhere(&preview.placement.elsewhere);
        preview.changes.to_download = Some(fetch::planned(
            self.context,
            jobs.iter()
                .map(|job| (fetch::host_of(job), fetch::size_of(job))),
        ));
        preview.changes.download = jobs
            .into_iter()
            .map(|job| Download {
                path: fetch::foreseen(&job),
                size: job.fetch.expect.size,
            })
            .chain(mirrored)
            .collect();
        preview.changes.unpack = unpack;
        Ok(preview)
    }

    /// of `made`, files of outputs the cache lacks, those a run takes from the reuse folders or a
    /// mirror, as downloads, and the outputs it leaves out, since no such place holds all their
    /// files.
    async fn made_where(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        made: Vec<(String, &LockedFile)>,
    ) -> Result<(Vec<Download>, Vec<Elsewhere>), Error> {
        if made.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let made = archives::of(lock, made);
        let held = self.held_elsewhere(&made).await?;
        let mut lacking = BTreeSet::new();
        let mut found = Vec::new();
        for (dataset, archive) in &made {
            for file in archive.files() {
                let path = format!("{dataset}/{}", file.path);
                if held.contains(&path) {
                    let size = Some(file.size);
                    found.push((dataset, Download { path, size }));
                } else {
                    lacking.insert(dataset.clone());
                }
            }
        }
        let mirrored = found
            .into_iter()
            .filter(|(dataset, _)| !lacking.contains(*dataset))
            .map(|(_, download)| download)
            .collect();
        let elsewhere = lacking
            .into_iter()
            .map(|name| Elsewhere::new(&name, self.shown(manifest, &name), lock))
            .collect();
        Ok((mirrored, elsewhere))
    }

    /// the labels of the files of `archives` a dry run would download that a run asking no source
    /// still gets without one: from the reuse folders or a mirror, looked at without taking
    /// anything. a split archive whose list of files a mirror holds has all its parts there.
    async fn held_elsewhere(
        &self,
        archives: &[(String, Archive<'_>)],
    ) -> Result<HashSet<String>, Error> {
        let label = |dataset: &str, file: &LockedFile| format!("{dataset}/{}", file.path);
        let files: Vec<(&str, &LockedFile)> = archives
            .iter()
            .flat_map(|(dataset, archive)| {
                let files = archive.files().into_iter();
                files.map(move |file| (dataset.as_str(), file))
            })
            .filter(|(_, file)| !self.holds(file))
            .collect();
        let wanted: Vec<crate::mirror::Wanted> = files
            .iter()
            .map(|(dataset, file)| crate::mirror::Wanted {
                digest: file.sha256,
                size: file.size,
                label: label(dataset, file),
                link: (!file.url.is_empty()).then(|| file.url.clone()),
            })
            .collect();
        let reused = self
            .context
            .reuse
            .holding(self.context, &self.cache, &wanted)?;
        let asked: Vec<super::probe::Pinned<'_>> = files
            .iter()
            .filter(|(_, file)| !reused.contains(&file.sha256))
            .map(|(dataset, file)| super::probe::Pinned::locked(dataset, file, false))
            .collect();
        let found =
            super::probe::find(self.context, &self.net, &asked, super::probe::Ask::Mirrors).await?;
        let mirrored: HashSet<String> = asked
            .iter()
            .zip(found)
            .filter(|(_, found)| matches!(found, super::probe::Found::Mirror { .. }))
            .map(|(pinned, _)| pinned.label())
            .collect();
        let mut held = HashSet::new();
        for (dataset, archive) in archives {
            let listed = archive.file.unpacks_to.is_some()
                && mirrored.contains(&label(dataset, archive.file));
            for file in archive.files() {
                let label = label(dataset, file);
                if listed || reused.contains(&file.sha256) || mirrored.contains(&label) {
                    held.insert(label);
                }
            }
        }
        Ok(held)
    }

    /// refuses, in mirror-only mode, `jobs`, files no mirror gave, but those the mirror
    /// `HF_ENDPOINT` names serves.
    fn refuse_unmirrored<'j>(&self, jobs: impl IntoIterator<Item = &'j Job>) -> Result<(), Error> {
        if !self.context.mirrors.mirrors.only {
            return Ok(());
        }
        let lacking: Vec<(String, Option<Sha256>)> = jobs
            .into_iter()
            .filter(|job| !fetch::mirrors::at_hf_endpoint(self.context, &job.fetch.from))
            .map(|job| (job.label.clone(), job.fetch.expect.sha256))
            .collect();
        fetch::mirrors::sources_allowed(self.context, &lacking)
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

    /// writes `text`, `lock` rendered, to data.lock, telling what changed since `before`.
    pub(super) fn write_rendered(
        &self,
        before: &Before,
        lock: &Lock,
        text: String,
    ) -> Result<String, Error> {
        let _span = tracing::debug_span!("write_lock").entered();
        // the lists first, so data.lock never names a part that is not there yet
        let written = self.project.write_lists(lock)?;
        self.context.wrote_lists(&self.project.root, written);
        self.project.write(lock::FILE, text.as_bytes())?;
        self.wrote_lock.store(true, Ordering::Relaxed);
        tell_lock(
            &self.context.output,
            before,
            lock,
            Verbs::DONE,
            Some(&self.told),
            &HashMap::new(),
        );
        Ok(text)
    }

    async fn resolve_all(
        &self,
        manifest: &Manifest,
        asks: Vec<(String, String)>,
        given: Vec<(String, Listing)>,
    ) -> Result<Vec<(String, Listing)>, Error> {
        let mut given: HashMap<String, Listing> = given.into_iter().collect();
        let mut listings = Vec::with_capacity(asks.len());
        let mut tasks = JoinSet::new();
        let mut asked = Vec::new();
        for (name, text) in asks {
            if let Some(listing) = given.remove(&name) {
                listings.push((name, listing));
                continue;
            }
            let entry = &manifest.datasets[&name];
            let parsed = Reference::parse(&text);
            // a list in the project is read from its file there, which needs no network
            if let Ok(Parsed {
                reference: Reference::List(list),
                ..
            }) = &parsed
                && list.in_project().is_some()
            {
                let listing = list
                    .read_in(&self.project.root, crate::net::Api::most_files(&*self.net))
                    .map_err(|err| cite_place(err, entry, "ref"))?;
                match filter(&name, entry, &listing) {
                    Ok(kept) => tell_kept(&self.context.output, &listing, &kept),
                    Err(_) => tell_resolved(&self.context.output, &listing),
                }
                listings.push((name, listing));
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
            let select = &entry.source.select;
            let parsed = parsed.map_err(|err| {
                let err = if crate::source::is_sftp(&text) {
                    err.help(crate::source::sftp_help(Some(&name)))
                } else {
                    err
                };
                cite_place(err, entry, "ref")
            })?;
            if self.context.mirrors.mirrors.only
                && !fetch::mirrors::resolves_at_hf_endpoint(self.context, &parsed.reference)
            {
                return Err(fetch::mirrors::refused(
                    self.context,
                    &format!("{name} needs a new entry in data.lock, which only its source gives"),
                ));
            }
            // the line itself may hold a secret, so only its place is shown
            parsed
                .check_shareable()
                .map_err(|err| cite_place(err, entry, "ref"))?;
            if !resolves_locally(&parsed.reference) {
                asked.push(parsed.reference.to_string());
            }
            let net = self.net.clone();
            let select = select.clone();
            let reference = parsed.reference.taking_page(entry.source.web_page);
            tasks.spawn(async move {
                let listing = reference.resolve_in(&*net, &select).await;
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
            let listing = listing.map_err(|error| fetch::page_in_entry(error, &name))?;
            // the files the dataset keeps, counted as an add counts them
            match filter(&name, &manifest.datasets[&name], &listing) {
                Ok(kept) => tell_kept(&self.context.output, &listing, &kept),
                Err(_) => tell_resolved(&self.context.output, &listing),
            }
            listings.push((name, listing));
        }
        listings.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(listings)
    }

    /// puts every job's file in the cache, asking first when a lot is about to download.
    async fn download(&self, jobs: Vec<Job>) -> Result<Vec<Fetched>, Error> {
        Ok(self.download_with(jobs, Vec::new()).await?.fetched)
    }

    /// [`Loop::download`], whose line before it also tells `ahead`, the downloads placing makes
    /// later; their hashes come back with what it fetched, and the line after waits for placing's.
    async fn download_with(&self, jobs: Vec<Job>, mut ahead: Vec<Job>) -> Result<Batch, Error> {
        if jobs.is_empty() {
            return Ok(Batch::default());
        }
        fetch::mirrors::fill(
            self.context,
            &self.net,
            &self.cache,
            &DataFolder::default(),
            || {
                let mut wanted = fetch::mirrors::pinned(&jobs, &self.cache);
                wanted.extend(fetch::mirrors::pinned(&ahead, &self.cache));
                wanted
            },
        )
        .await
        .map_err(|error| self.as_left(error))?;
        ahead.retain(|job| !in_cache(&self.cache, &job.fetch));
        self.refuse_offline(&jobs)?;
        self.refuse_unmirrored(jobs.iter().filter(|job| !in_cache(&self.cache, &job.fetch)))?;
        let lacking: Vec<&Job> = jobs
            .iter()
            .filter(|job| !in_cache(&self.cache, &job.fetch))
            .collect();
        self.refuse_unasked(
            lacking
                .iter()
                .copied()
                .chain(&ahead)
                .map(|job| &job.fetch.from),
        )
        .map_err(|error| self.as_left(error))?;
        crate::space::cache_holds(
            self.cache.root(),
            lacking
                .iter()
                .filter_map(|job| Some((job.label.as_str(), job.fetch.expect.size?))),
        )
        .map_err(|error| self.as_left(error))?;
        let told = || lacking.iter().copied().chain(&ahead);
        let room = budget(told());
        let planned = fetch::planned(
            self.context,
            told().map(|job| (fetch::host_of(job), fetch::size_of(job))),
        );
        let called = fetch::called(told().map(|job| job.label.as_str()), planned.files);
        // the question covers the files the line after it tells, placing's later ones too
        let what = units::count(planned.files, "file", "files");
        self.ask_budget(&format!("downloading {what}"), room, None)?;
        let roomy = super::confirm(
            &self.context.output,
            self.yes(),
            self.context.settings.ask_above.value,
            &what,
            planned.size(),
            Some((self.cache.root(), room)),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.fetch_or(roomy, Ordering::Relaxed);
        fetch::tell_planned(&self.context.output, &called, &planned);
        let started = Instant::now();
        let fetched = fetch::all(self.context, &self.net, &self.cache, jobs)
            .await
            .map_err(|error| self.as_left(error))?;
        if ahead.is_empty() {
            self.tell_downloaded(&fetched, started);
            fetch::tell_found(&self.context.output, &called, &planned, &fetched);
            return Ok(Batch {
                fetched,
                ahead: None,
            });
        }
        let digests = ahead
            .iter()
            .filter_map(|job| job.fetch.expect.sha256)
            .collect();
        Ok(Batch {
            fetched,
            ahead: Some((digests, started)),
        })
    }

    /// `error`, saying what a stop before placing left: nothing changed, or once this run wrote
    /// data.lock, the data folder as it was. a cancel says so through [`Loop::as_left`].
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

    /// whether downloads go on without asking: `--yes`, or a yes to going on without room.
    fn yes(&self) -> bool {
        self.context.yes || self.roomy.load(Ordering::Relaxed)
    }

    /// asks once before anything downloads when the cache's disk may lack room for all placing
    /// needs, which per-download questions miss, then likewise for the data folder's copies.
    fn ask_room(&self, lock: &Lock, placing: &Placing<'_>) -> Result<(), Error> {
        if self.roomy.load(Ordering::Relaxed) {
            return Ok(());
        }
        let needed = if self.net.offline().enabled() {
            0
        } else {
            self.download_room(lock, placing)?
        };
        self.ask_budget(self.run, needed, Some(lock))?;
        let free = crate::space::free(self.cache.root());
        if crate::space::short(needed, free).is_none() {
            return self.ask_room_to_place(placing, needed);
        }
        if self.context.yes {
            super::warn_short(
                &self.context.output,
                self.run,
                (needed, free.unwrap_or_default()),
                &format!("the cache at {}", units::path(self.cache.root())),
            );
            return self.ask_room_to_place(placing, needed);
        }
        super::ask_room(
            &self.context.output,
            self.run,
            needed,
            free.unwrap_or_default(),
            self.cache.root(),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// asks once, as for a disk without room, when the `adding` bytes `doing` downloads would take
    /// the cache past the `cache_budget` setting. only then are the cache's files read, and what
    /// frees room told: gc's share and the largest datasets of `lock`, else of data.lock.
    fn ask_budget(&self, doing: &str, adding: u64, lock: Option<&Lock>) -> Result<(), Error> {
        // a run that found its downloads take the cache past the budget asks once
        let output = &self.context.output;
        if output.said_once("cache budget") {
            return Ok(());
        }
        let (threads, stop) = (self.context.threads, &self.context.stop);
        let mut scan = None;
        let budget = self.context.settings.cache_budget.value;
        let Some(past) = cache_budget::weigh(budget, adding, || {
            let (held, found) = cache_budget::held(&self.cache, threads, stop)?;
            scan = Some(found);
            Some(held)
        }) else {
            return Ok(());
        };
        output.once("cache budget");
        let now = SystemTime::now();
        let freeable =
            scan.and_then(|scan| super::cache::unneeded_in(&self.cache, &scan, now, threads, stop));
        let pinned = match lock {
            Some(_) => None,
            None => self.project.lock().ok().flatten(),
        };
        // a dataset this add brings is no candidate for removal
        let mut largest = lock
            .or(pinned.as_ref())
            .map(|lock| cache_budget::largest(lock, usize::MAX))
            .unwrap_or_default();
        largest.retain(|(name, _)| !self.told.contains(name));
        largest.truncate(cache_budget::LARGEST);
        let over = cache_budget::Over {
            doing,
            past,
            freeable,
            largest,
            by_variable: self.context.settings.cache_budget.origin
                == crate::settings::Origin::Environment,
            projectless: false,
        };
        cache_budget::ask(&self.context.output, self.context.yes, &over)
            .map_err(|error| self.as_left(error))
    }

    /// asks before placing downloads `untold`, the files relocking did not ask about and tell
    /// ahead, when their size or the `room` all its downloads take needs it; then tells them.
    fn ask_then_tell(&self, untold: &[&fetch::Description], room: u64) -> Result<(), Error> {
        let size = untold
            .iter()
            .filter_map(|description| description.size)
            .fold(0, u64::saturating_add);
        let planned = fetch::planned(
            self.context,
            untold
                .iter()
                .map(|description| (description.host.clone(), description.size)),
        );
        let roomy = super::confirm(
            &self.context.output,
            self.yes(),
            self.context.settings.ask_above.value,
            &units::count(planned.files, "file", "files"),
            estimate::Size::Bytes(size),
            Some((self.cache.root(), room)),
        )
        .map_err(|error| self.as_left(error))?;
        self.roomy.fetch_or(roomy, Ordering::Relaxed);
        let called = fetch::called(
            untold.iter().map(|description| description.label.as_str()),
            planned.files,
        );
        fetch::tell_planned(&self.context.output, &called, &planned);
        Ok(())
    }

    /// what downloading for placing takes on the cache's disk; an archive of unknown files counts
    /// twice. a file the disk cannot hold, as FAT32 none of 4 GiB, is refused first.
    fn download_room(&self, lock: &Lock, placing: &Placing<'_>) -> Result<u64, Error> {
        let lacks = |file: &LockedFile| self.cache.find(&file.sha256, Some(file.size)).is_none();
        let none = HashSet::new();
        let missing = archives::of(lock, placing.missing_as_is(&self.cache, &none, &none)?);
        let unknown = archives::of(lock, placing.unknown());
        let files = |archives: &[(String, Archive<'_>)]| -> Vec<(String, LockedFile)> {
            archives
                .iter()
                .flat_map(|(dataset, archive)| {
                    let files = archive.files().into_iter();
                    files.map(move |file| (dataset.clone(), file.clone()))
                })
                .collect()
        };
        let (missing, unknown_files) = (files(&missing), files(&unknown));
        let large: Vec<(String, u64)> = missing
            .iter()
            .chain(&unknown_files)
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
        for (_, file) in &unknown_files {
            let download = if lacks(file) { file.size } else { 0 };
            needed = needed.saturating_add(download).saturating_add(file.size);
        }
        Ok(needed)
    }

    /// asks, as [`Loop::ask_room`] does, when the data folder's disk may lack room for the files
    /// placing copies there, less what `downloads` take of it when the cache shares that disk.
    fn ask_room_to_place(&self, placing: &Placing<'_>, downloads: u64) -> Result<(), Error> {
        let clones = || self.cache.clones();
        let Some(copies) = placing.copies(self.cache.root(), &clones)? else {
            return Ok(());
        };
        let needed = if copies.beside_cache {
            copies.bytes.saturating_add(downloads)
        } else {
            copies.bytes
        };
        let free = crate::space::free(&copies.at);
        if crate::space::short(needed, free).is_none() {
            return Ok(());
        }
        let place = format!("the data folder at {}", units::path(&copies.folder));
        if self.context.yes {
            let room = (needed, free.unwrap_or_default());
            super::warn_short(&self.context.output, self.run, room, &place);
            return Ok(());
        }
        super::ask_room_on(
            &self.context.output,
            self.run,
            (needed, free.unwrap_or_default()),
            &place,
            "free space on that disk, then run the same command again. add --yes to go on anyway, as on a disk that compresses what it stores",
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

    /// downloads `files`, of which `available` says, when known, which the cache holds already.
    async fn download_prepared(
        &self,
        manifest: &Manifest,
        lock: &Lock,
        files: &[(String, &LockedFile)],
        available: Option<&[bool]>,
    ) -> Result<Vec<Fetched>, Error> {
        let told = self
            .ahead
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if files.is_empty() {
            if let Some(told) = told {
                self.tell_downloaded(&told.fetched, told.started);
            }
            return Ok(Vec::new());
        }
        let looked;
        let available = if let Some(available) = available {
            available
        } else {
            looked = crate::place::run_all(files, self.context.threads, |(_, file)| {
                self.cache.find(&file.sha256, Some(file.size)).is_some()
            });
            &looked
        };
        let lacking: Vec<(&str, &LockedFile)> = files
            .iter()
            .zip(available)
            .filter(|(_, available)| !**available)
            .map(|((dataset, file), _)| (dataset.as_str(), *file))
            .collect();
        self.refuse_unreachable(manifest, &lacking)?;
        let (fetchable, held): (Vec<(String, &LockedFile)>, Vec<bool>) = files
            .iter()
            .zip(available)
            .filter(|((_, file), available)| **available || !file.url.is_empty())
            .map(|(entry, available)| (entry.clone(), *available))
            .unzip();
        let (files, available) = (fetchable.as_slice(), held.as_slice());
        let mut descriptions = Vec::with_capacity(files.len());
        let mut urls = Vec::with_capacity(files.len());
        for (dataset, file) in files {
            let (description, url) = fetch::Description::take(job(manifest, lock, dataset, file)?);
            descriptions.push(description);
            urls.push(url);
        }
        self.refuse_unasked(
            urls.iter()
                .zip(available)
                .filter(|(_, available)| !**available)
                .map(|(url, _)| url),
        )
        .map_err(|error| self.as_left(error))?;
        crate::space::cache_holds(
            self.cache.root(),
            descriptions
                .iter()
                .zip(available)
                .filter(|(_, available)| !**available)
                .filter_map(|(description, _)| {
                    Some((description.label.as_str(), description.size?))
                }),
        )
        .map_err(|error| self.as_left(error))?;
        let size = descriptions
            .iter()
            .zip(available)
            .filter(|(_, available)| !**available)
            .filter_map(|(description, _)| description.size)
            .fold(0, u64::saturating_add);
        let untold = untold(&descriptions, files, available, told.as_ref());
        self.ask_then_tell(&untold, size)?;
        let started = Instant::now();
        let fetched = fetch::prepared(
            self.context,
            &self.net,
            &self.cache,
            descriptions,
            urls,
            |index, url| {
                let (dataset, file) = &files[index];
                make_job(lock, dataset, file, &url, asks_page(manifest, dataset))
            },
        )
        .await
        .map_err(|error| self.as_left(error))?;
        match told {
            Some(told) => self.tell_downloaded(told.fetched.iter().chain(&fetched), told.started),
            None => self.tell_downloaded(&fetched, started),
        }
        // an import no link fetches waits with its own error until the other files are in
        self.unimported(manifest, &lacking).map_or(Ok(fetched), Err)
    }

    fn tell_downloaded<'f>(
        &self,
        fetched: impl IntoIterator<Item = &'f Fetched>,
        started: Instant,
    ) {
        let downloaded: Vec<&Fetched> = fetched
            .into_iter()
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
                    self.context
                        .output
                        .took(Fetched::transfer(downloaded.iter().copied(), started))
                )
            );
            self.context.output.step("Downloaded", &text);
        }
    }

    /// refuses the first of `links` a request would be refused before it is sent, as a private
    /// address `allowed_origins` does not list, so no line says it downloads.
    pub(super) fn refuse_unasked<'u>(
        &self,
        links: impl IntoIterator<Item = &'u Url>,
    ) -> Result<(), Error> {
        match links.into_iter().find_map(|link| self.net.refusal(link)) {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }

    /// refuses, when offline, the jobs whose bytes the cache lacks; one that learns its hash counts
    /// as held when the cache has what its link served last, as after a `get` of it.
    fn refuse_offline(&self, jobs: &[Job]) -> Result<(), Error> {
        self.refuse_offline_with(jobs, |job| {
            in_cache(&self.cache, &job.fetch)
                || (job.fetch.revalidate && job.fetch.remembered(&self.cache).is_some())
        })
    }

    fn refuse_offline_with(
        &self,
        jobs: &[Job],
        available: impl Fn(&Job) -> bool,
    ) -> Result<(), Error> {
        if !self.net.offline().enabled() {
            return Ok(());
        }
        let lacking: Vec<&Job> = jobs.iter().filter(|job| !available(job)).collect();
        if lacking.is_empty() {
            return Ok(());
        }
        let unfilled = self.unfilled.lock().unwrap_or_else(PoisonError::into_inner);
        // a new link has no fingerprint yet, so the cache cannot be asked for it. a pin data.lock
        // keeps by what its source states, an md5 or a size, is no new link
        let new = |expect: &Expect| {
            expect.sha256.is_none() && expect.md5.is_none() && expect.size.is_none()
        };
        if lacking.iter().any(|job| new(&job.fetch.expect)) {
            let missing: Vec<&str> = lacking
                .iter()
                .map(|job| job.label.as_str())
                .chain(unfilled.iter().map(|file| file.label.as_str()))
                .collect();
            let said = "offline, and a new link must be downloaded once to learn its SHA-256";
            let error = Error::new(Kind::OfflineMissing, said).details_up_to(&missing, NAMED);
            return Err(error.help(self.net.offline().unlearned_help()));
        }
        let (unlearned, missing) = split_lacking(
            &unfilled,
            lacking
                .iter()
                .map(|job| (job.label.as_str(), job.fetch.expect.sha256.is_none())),
        );
        Err(self.offline_missing(&unlearned, &missing))
    }

    /// refuses, before any link is read, what `lacking` still lacks after the data folder and
    /// mirrors that no download may fetch: anything offline or mirror-only, else an import's files.
    fn refuse_unfetchable(
        &self,
        manifest: &Manifest,
        lacking: &[(&str, &LockedFile)],
    ) -> Result<(), Error> {
        self.refuse_unreachable(manifest, lacking)?;
        self.unimported(manifest, lacking).map_or(Ok(()), Err)
    }

    /// refuses what `lacking` still lacks when no download may fetch anything of it: offline, or
    /// a source only a mirror may stand in for.
    fn refuse_unreachable(
        &self,
        manifest: &Manifest,
        lacking: &[(&str, &LockedFile)],
    ) -> Result<(), Error> {
        let named: Vec<(String, Option<Sha256>)> = lacking
            .iter()
            .filter(|(_, file)| {
                self.net.offline().enabled()
                    || !url::Url::parse(&file.url)
                        .is_ok_and(|url| fetch::mirrors::at_hf_endpoint(self.context, &url))
            })
            .map(|(dataset, file)| (format!("{dataset}/{}", file.path), Some(file.sha256)))
            .collect();
        let unfilled = self.unfilled.lock().unwrap_or_else(PoisonError::into_inner);
        if self.net.offline().enabled() && !(named.is_empty() && unfilled.is_empty()) {
            if let Some(import) = lacking
                .iter()
                .filter(|(_, file)| file.url.is_empty())
                .map(|(dataset, _)| *dataset)
                .min()
            {
                return Err(self.offline_imported(manifest, import, lacking));
            }
            let labels = named.iter().map(|(label, _)| (label.as_str(), false));
            let (unlearned, missing) = split_lacking(&unfilled, labels);
            return Err(self.offline_missing(&unlearned, &missing));
        }
        fetch::mirrors::sources_allowed(self.context, &named)
    }

    /// the error for the imports among `lacking`, whose files no link fetches, naming each one's
    /// files, or none when it holds no import.
    fn unimported(&self, manifest: &Manifest, lacking: &[(&str, &LockedFile)]) -> Option<Error> {
        let imports: BTreeSet<&str> = lacking
            .iter()
            .filter(|(_, file)| file.url.is_empty())
            .map(|(dataset, _)| *dataset)
            .collect();
        let named: Vec<(String, Option<Sha256>)> = lacking
            .iter()
            .map(|(dataset, file)| (format!("{dataset}/{}", file.path), Some(file.sha256)))
            .collect();
        let mirrored = !self.context.mirrors.mirrors.list.is_empty();
        let error = match imports.iter().copied().collect::<Vec<_>>().as_slice() {
            [] => return None,
            [import] => {
                local::imported_elsewhere(import, &plan::folder(manifest, import), mirrored)
            }
            several => local::several_imported_elsewhere(several, mirrored),
        };
        // each import's first file before any import's second, so the details name every one
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let mut imported: Vec<_> = lacking
            .iter()
            .zip(named)
            .filter(|((_, file), _)| file.url.is_empty())
            .map(|((dataset, _), named)| {
                let rank = seen.entry(dataset).or_default();
                *rank += 1;
                ((*rank, *dataset), named)
            })
            .collect();
        imported.sort_by_key(|(order, _)| *order);
        let imported: Vec<_> = imported.into_iter().map(|(_, named)| named).collect();
        Some(
            fetch::mirrors::lacked(self.context, &imported)
                .into_iter()
                .fold(error, Error::detail),
        )
    }

    /// the error for files an offline run lacks, `unlearned` first: those pinned by what their
    /// sources state with no SHA-256, which only a download learns, then `missing`, which
    /// data.lock names by theirs, with ways that need no network. offline asks no web mirror, so
    /// the help says so rather than leave the mirror looking empty.
    fn offline_missing(&self, unlearned: &[&str], missing: &[&str]) -> Error {
        let offline = self.net.offline();
        if missing.is_empty() {
            return Error::new(
                Kind::OfflineMissing,
                "offline, and a file pinned by what its source states, not by a SHA-256, must be downloaded once to learn it",
            )
            .details_up_to(unlearned, NAMED)
            .help(format!("{} to download it", offline.retry()));
        }
        let message = match unlearned.len() {
            0 => format!(
                "offline, and {} data.lock names {} not in the cache",
                units::count(missing.len() as u64, "file", "files"),
                if missing.len() == 1 { "is" } else { "are" }
            ),
            stated => {
                let (whose, their) = if stated == 1 {
                    ("its source states", "its")
                } else {
                    ("their sources state", "their")
                };
                format!(
                    "offline, and {} files are not in the cache: {} data.lock names, and {} pinned by what {whose}, which must be downloaded once to learn {their} SHA-256",
                    units::grouped((missing.len() + stated) as u64),
                    units::grouped(missing.len() as u64),
                    units::grouped(stated as u64)
                )
            }
        };
        let lacking: Vec<&str> = unlearned.iter().chain(missing).copied().collect();
        let error = Error::new(Kind::OfflineMissing, message).details_up_to(&lacking, NAMED);
        // a folder mirror is read by SHA-256, which an unlearned file has none of yet
        let help = if unlearned.is_empty() {
            format!(
                "{}, or name a folder mirror that holds them with `--mirror <folder>`",
                offline.missing_help("them")
            )
        } else {
            offline.missing_help("them")
        };
        let remote: Vec<&crate::mirror::Location> = self
            .context
            .mirrors
            .mirrors
            .list
            .iter()
            .filter(|mirror| !matches!(mirror, crate::mirror::Location::Folder(_)))
            .collect();
        let buckets = remote
            .iter()
            .filter(|mirror| matches!(mirror, crate::mirror::Location::S3(_)))
            .count();
        let kind = match buckets {
            0 => "web mirror",
            all if all == remote.len() => "S3 mirror",
            _ => "web or S3 mirror",
        };
        let named: Vec<String> = remote.iter().map(ToString::to_string).collect();
        error.help(match named.as_slice() {
            [] => help,
            [mirror] => format!("offline asks no {kind}, so {mirror} was not asked: {help}"),
            many => format!(
                "offline asks no {kind}, so {} were not asked: {help}",
                many.join(", ")
            ),
        })
    }

    /// the error for the files of `import` an offline run lacks: no link fetches an import, so its
    /// help offers only the folder it came from or a folder mirror.
    fn offline_imported(
        &self,
        manifest: &Manifest,
        import: &str,
        lacking: &[(&str, &LockedFile)],
    ) -> Error {
        let folder = plan::folder(manifest, import);
        let mirrored = !self.context.mirrors.mirrors.list.is_empty();
        let message = local::imported_elsewhere(import, &folder, mirrored).message;
        let labels: Vec<String> = lacking
            .iter()
            .filter(|(dataset, file)| *dataset == import && file.url.is_empty())
            .map(|(dataset, file)| format!("{dataset}/{}", file.path))
            .collect();
        let help = format!(
            "no link fetches an import, so going online does not help: copy its files from the folder they came from to {folder} by hand, then run `{}`, or sync with `--mirror <folder>` naming a folder mirror that holds them",
            super::project::command("sync")
        );
        Error::new(Kind::OfflineMissing, message)
            .details_up_to(&labels, NAMED)
            .help(help)
    }
}

/// refuses `asks`, the datasets a relock would resolve again, when one is an import: it names no
/// source to list it again, so the run stops before it asks anyone, and an output's folder is
/// read again only by its add. `others` holds the old pins, and `root` is the project's folder.
fn refuse_imports(
    manifest: &Manifest,
    asks: &[(String, String)],
    others: &Lock,
    root: &std::path::Path,
) -> Result<(), Error> {
    let Some((name, _)) = asks
        .iter()
        .find(|(name, _)| manifest.datasets[name].is_import())
    else {
        return Ok(());
    };
    let entry = &manifest.datasets[name];
    let at = entry
        .is_output()
        .then(|| super::folder_from_here(root, &plan::folder(manifest, name)));
    let error = if others.get(name).is_some() {
        local::changed(name, at.as_deref())
    } else {
        local::unpinned(name, at.as_deref())
    };
    Err(cite_place(error, entry, "ref"))
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

/// data.lock's entries as relocking sorts them, before it resolves anything.
struct Sorted {
    /// entries whose data.toml entry did not change, or that the run leaves alone.
    kept: Vec<Locked>,
    /// entries to replace or drop.
    others: Vec<Locked>,
    /// drafts of entries whose `extract` alone changed.
    drafts: Vec<(String, Draft)>,
}

/// datasets on their way into data.lock.
struct Drafted {
    /// entries whose data.toml entry did not change, moved from data.lock.
    kept: Vec<Locked>,
    drafts: Vec<Draft>,
    /// whole pins of imports, given or with `extract` switched, whose archives may need unpacking.
    pinned: Vec<Locked>,
    /// the entries of data.lock not kept: those to replace or drop.
    others: Lock,
    /// the entries carried over to the names data.toml gives them now.
    renamed: Vec<Rename>,
}

/// the lock of `kept` and `drafted` entries, by name, with whether each dataset was kept; a pin
/// keeps its date while its data stays, so resolving it again changes nothing.
fn settled(others: &Lock, kept: Vec<Locked>, drafted: Vec<Locked>) -> (Lock, Vec<bool>) {
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
    (
        Lock {
            datasets,
            stated: Vec::new(),
            waiting: Vec::new(),
        },
        kept,
    )
}

/// data.lock as it was before relocking, beside the new lock. entries relocking kept as they
/// were are moved into the new lock instead of copied, so they are found there.
pub(super) struct Before {
    existed: bool,
    /// the old entries relocking did not keep: those it replaced or dropped, and the entries
    /// waiting for their file lists whose pins data.toml no longer asks for.
    others: Lock,
    /// for each dataset of the new lock, in its order, whether it was kept from the old one.
    kept: Vec<bool>,
    /// the entries relocking carried over to the names data.toml gives them now.
    renamed: Vec<Rename>,
}

impl Before {
    /// data.lock as relocking left it: `others`, the old entries it did not keep, with `gone`, the
    /// entries waiting for their lists whose pins data.toml no longer asks for.
    fn new(
        existed: bool,
        others: Lock,
        gone: Vec<Waiting>,
        kept: Vec<bool>,
        renamed: Vec<Rename>,
    ) -> Before {
        Before {
            existed,
            others: Lock {
                waiting: gone,
                ..others
            },
            kept,
            renamed,
        }
    }

    /// data.lock, or its absence, as a lock of which nothing was kept.
    #[cfg(test)]
    pub(super) fn whole(old: Option<Lock>) -> Before {
        Before {
            existed: old.is_some(),
            others: old.unwrap_or_default(),
            kept: Vec::new(),
            renamed: Vec::new(),
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
            && self.renamed.is_empty()
            && lock
                .datasets
                .iter()
                .all(|dataset| self.get(lock, &dataset.name) == Some(dataset))
            && self.dropped(lock).next().is_none()
            && self.others.waiting.is_empty()
    }

    /// the old datasets `lock` no longer has.
    fn dropped<'a>(&'a self, lock: &'a Lock) -> impl Iterator<Item = &'a Locked> {
        self.others
            .datasets
            .iter()
            .filter(|dataset| lock.get(&dataset.name).is_none())
    }

    /// the names of the old entries that waited for their file lists and that `lock` pins no
    /// more under those names.
    fn unwaited<'a>(&'a self, lock: &'a Lock) -> impl Iterator<Item = &'a String> {
        self.others
            .waiting
            .iter()
            .map(|waiting| &waiting.name)
            .filter(|name| !lock.pins(name))
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
    /// for entries carried over to the names data.toml gives them now.
    renamed: &'static str,
}

impl Verbs {
    pub(super) const DONE: Verbs = Verbs {
        locked: "Locked",
        pinned: "Pinned",
        dropped: "Removed",
        renamed: "Renamed",
    };
    pub(super) const WOULD: Verbs = Verbs {
        locked: "Would lock",
        pinned: "Would pin",
        dropped: "Would remove",
        renamed: "Would rename",
    };
}

/// the names of the datasets whose pins `lock` changes beside `before`: new, moved or gone, a
/// rename counting its new name and, when the old entry went, its old one.
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
        .chain(
            before
                .others
                .waiting
                .iter()
                .map(|waiting| waiting.name.clone()),
        )
        .chain(before.renamed.iter().flat_map(|rename| {
            let old = rename.moved.then(|| rename.old.clone());
            std::iter::once(rename.new.clone()).chain(old)
        }))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// the most datasets pinned for the first time that get a line each; the rest are counted.
const FIRST_PINS: usize = 20;

/// the labels of the `jobs` whose size only their download can tell, each with what is known of
/// it: about what a folder listing rounds it to, or nothing.
pub(super) fn sizeless(jobs: &[Job]) -> HashMap<String, estimate::Size> {
    jobs.iter()
        .filter(|job| job.fetch.expect.size.is_none())
        .map(|job| (job.label.clone(), fetch::size_of(job)))
        .collect()
}

/// says which datasets `lock` pins anew or no longer, beside `before`, with `verbs`. a first pin
/// gets its own line with what it pinned, so a typo that pins an unrelated record stands out;
/// datasets `told` names wait for `-v`, as a later line tells each.
pub(super) fn tell_lock(
    output: &Output,
    before: &Before,
    lock: &Lock,
    verbs: Verbs,
    told: Option<&BTreeSet<String>>,
    sizeless: &HashMap<String, estimate::Size>,
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
    told: Option<&BTreeSet<String>>,
    sizeless: &HashMap<String, estimate::Size>,
) -> Vec<(&'static str, String)> {
    let told = told.filter(|_| !output.shows_details());
    let is_told = |name: &str| told.is_some_and(|told| told.contains(name));
    let mut first = Vec::new();
    let mut again: Vec<&Locked> = Vec::new();
    for dataset in &lock.datasets {
        if is_told(&dataset.name) {
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
    // the first pins past those told are counted as first pins, never as pinned again
    let more = &first[shown..];
    if !more.is_empty() {
        let files: usize = more.iter().map(|dataset| dataset.files.len()).sum();
        lines.push((
            verbs.pinned,
            format!(
                "{} more {}, new in {} · {}",
                units::grouped(more.len() as u64),
                if more.len() == 1 {
                    "dataset"
                } else {
                    "datasets"
                },
                lock::FILE,
                units::count(files as u64, "file", "files")
            ),
        ));
    }
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
    for rename in &before.renamed {
        if is_told(&rename.new) {
            continue;
        }
        lines.push(if rename.moved {
            (
                verbs.renamed,
                format!(
                    "{} to {} in {} · its pin stays",
                    rename.old,
                    rename.new,
                    lock::FILE
                ),
            )
        } else {
            (
                verbs.pinned,
                format!(
                    "{}, renamed from {} · its pin stays",
                    rename.new, rename.old
                ),
            )
        });
    }
    let dropped = before
        .dropped(lock)
        .map(|dataset| &dataset.name)
        .chain(before.unwaited(lock));
    for name in dropped {
        lines.push((verbs.dropped, format!("{name} from {}", lock::FILE)));
    }
    lines
}

/// the line for a dataset data.lock pins for the first time; `sizeless` labels the files whose size
/// only a download can tell, as in a dry run, with about what a listing rounds one to.
fn first_pin(
    output: &Output,
    locked: &Locked,
    sizeless: &HashMap<String, estimate::Size>,
) -> String {
    let resolved = Reference::parse(&locked.resolved).map_or_else(
        |_| locked.resolved.clone(),
        |parsed| parsed.reference.shown_short(),
    );
    let mut facts = vec![resolved];
    facts.extend(
        locked
            .title
            .as_deref()
            .map(|title| output.clip(title, TITLE)),
    );
    facts.push(units::count(locked.files.len() as u64, "file", "files"));
    let mut total = estimate::Total {
        files: locked.files.len() as u64,
        ..estimate::Total::default()
    };
    for file in &locked.files {
        let label = format!("{}/{}", locked.name, file.path);
        match sizeless
            .get(&label)
            .copied()
            .unwrap_or(estimate::Size::Bytes(file.size))
        {
            estimate::Size::Bytes(size) => total.bytes = total.bytes.saturating_add(size),
            estimate::Size::About(size) => {
                total.bytes = total.bytes.saturating_add(size);
                total.sizes_rounded += 1;
            }
            estimate::Size::Unknown => total.sizes_unknown += 1,
        }
    }
    facts.push(total.facts(false));
    format!(
        "{}, new in {}: {}",
        locked.name,
        lock::FILE,
        facts.join(" · ")
    )
}

pub(super) use draft::{filter, without_files};

/// resolves a reference, with progress or plain lines while its source takes its time, listing
/// only the folders that may hold a file the `select` patterns match where the source can.
pub(super) async fn resolve(
    output: &Output,
    reference: &Reference,
    net: &Net,
    select: &[crate::glob::Glob],
) -> Result<Listing, Error> {
    let _waiting =
        (!resolves_locally(reference)).then(|| output.wait("Resolving", reference.to_string()));
    reference.resolve_in(net, select).await
}

/// whether a reference resolves without asking anyone, as a plain link does. such a link may
/// also carry a token, which no progress line should print.
pub(super) fn resolves_locally(reference: &Reference) -> bool {
    matches!(reference, Reference::Url(_) | Reference::Dropbox(_))
}

/// what to resolve for a changed entry: a changed filter, or a version taken out of the ref,
/// resolves the version already locked, so tidying data.toml never moves data, as uv keeps a
/// locked version, while a new ref or an upgrade asks for the newest.
fn pinned(source: &Source, previous: Option<&Locked>, upgrading: bool) -> String {
    match previous {
        Some(locked)
            if !upgrading
                && (locked.reference == source.reference || keeps_version(locked, source)) =>
        {
            locked.resolved.clone()
        }
        _ => source.reference.clone(),
    }
}

/// whether `source`'s ref is the one `locked` was pinned for with its version taken out, and
/// `locked` pins a version of that same dataset, which a lock then keeps until an upgrade.
fn keeps_version(locked: &Locked, source: &Source) -> bool {
    Reference::version_left_out(&locked.reference, &source.reference)
        && Reference::version_left_out(&locked.resolved, &source.reference)
}

/// the download of one locked file. when its recorded link is dead, the source is asked again
/// at the version the lock pins.
fn job(manifest: &Manifest, lock: &Lock, dataset: &str, file: &LockedFile) -> Result<Job, Error> {
    let url = source_link(manifest, dataset, file)?;
    Ok(make_job(
        lock,
        dataset,
        file,
        &url,
        asks_page(manifest, dataset),
    ))
}

/// whether data.toml asks for the web page the link of `dataset` answers with.
fn asks_page(manifest: &Manifest, dataset: &str) -> bool {
    manifest
        .datasets
        .get(dataset)
        .is_some_and(|entry| entry.source.web_page)
}

/// where to download `file` of `dataset` from: the link data.lock pins. an import pins none,
/// since only the folder it was imported from held its bytes.
fn source_link(manifest: &Manifest, dataset: &str, file: &LockedFile) -> Result<Url, Error> {
    if file.url.is_empty() {
        return Err(local::imported_elsewhere(
            dataset,
            &plan::folder(manifest, dataset),
            false,
        ));
    }
    draft::link(dataset, file)
}

fn make_job(lock: &Lock, dataset: &str, file: &LockedFile, url: &Url, page: bool) -> Job {
    let pinned = lock.get(dataset);

    Job {
        fetch: Fetch {
            expect: Expect {
                sha256: Some(file.sha256),
                md5: file.md5,
                size: Some(file.size),
            },
            revalidate: false,
            record: pinned
                .is_some_and(|locked| draft::vouched_by_server(&locked.resolved, &file.url)),
            unpack: false,
            page,
            ..source::fetch_plan(url)
        },
        label: format!("{dataset}/{}", file.path),
        dataset: Some(dataset.to_owned()),
        again: pinned.map(|locked| Again {
            resolved: locked.resolved.clone(),
            path: file.path.clone(),
            alone: locked.files.len() == 1,
            changed: locked.timestamp.clone(),
        }),
    }
}

fn in_cache(cache: &Cache, fetch: &Fetch) -> bool {
    in_cache_where(cache, fetch, |_| true)
}

/// the datasets a sync placed whole though it could not get every file, those that wait, and
/// what placing did.
struct Partial {
    placed: Vec<String>,
    waiting: Vec<String>,
    placement: Placement,
}

/// the labels of the files an offline run lacks, `unfilled`'s, then `others` with whether only a
/// download learns each one's SHA-256: those it must learn, and those data.lock names by theirs.
fn split_lacking<'l>(
    unfilled: &'l [fill::Unfilled],
    others: impl Iterator<Item = (&'l str, bool)>,
) -> (Vec<&'l str>, Vec<&'l str>) {
    let mut split = (Vec::new(), Vec::new());
    let unfilled = unfilled
        .iter()
        .map(|file| (file.label.as_str(), file.unlearned));
    for (label, unlearned) in unfilled.chain(others) {
        if unlearned {
            split.0.push(label);
        } else {
            split.1.push(label);
        }
    }
    split
}

/// whether `error` stopped a run from getting files, which a later run may get: a source or the
/// network failing, no network, a key needed or bytes other than the pins, never a person's no.
fn lacks_files(error: &Error) -> bool {
    matches!(error.kind.exit_code(), 10 | 20 | 30 | 40)
}

/// `failed`, else the failure of `result` when it [`lacks_files`], so the run goes on getting
/// what it can; any other failure stops the run at once. the one kept is boxed, as it waits
/// across downloads in the run's future.
fn first_left(
    failed: Option<Box<Error>>,
    result: Result<(), Error>,
) -> Result<Option<Box<Error>>, Error> {
    match result {
        Ok(()) => Ok(failed),
        Err(error) if lacks_files(&error) => Ok(failed.or_else(|| Some(Box::new(error)))),
        Err(error) => Err(error),
    }
}

/// what placing downloads: the `files` `available` says the cache lacks, described by
/// `descriptions`, but for those relocking `told` ahead.
fn untold<'d>(
    descriptions: &'d [fetch::Description],
    files: &[(String, &LockedFile)],
    available: &[bool],
    told: Option<&Ahead>,
) -> Vec<&'d fetch::Description> {
    descriptions
        .iter()
        .zip(files)
        .zip(available)
        .filter(|((_, (_, file)), available)| {
            !**available && told.is_none_or(|told| !told.digests.contains(&file.sha256))
        })
        .map(|((description, _), _)| description)
        .collect()
}

/// the room `jobs` of known size need on the cache's disk, twice the size of an archive whose
/// files nobody listed: a lower bound, as it may unpack to much more.
fn budget<'j>(jobs: impl Iterator<Item = &'j Job>) -> u64 {
    jobs.filter_map(|job| Some((job.fetch.expect.size?, job.fetch.unpack)))
        .fold(0, |room, (size, unpacks)| {
            let unpacked = if unpacks { size } else { 0 };
            room.saturating_add(size).saturating_add(unpacked)
        })
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
    } else if upper.contains("ARXIV.ORG/LICENSES/") {
        // arXiv's own license lets arXiv alone share a paper, and nobody reuse it
        Some("sharing copies")
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
        output.step("Using", &format!("raw file {}", crate::shown::Shown(raw)));
    }
}

/// prints what a reference resolved to, before any file downloads, then its source's warnings.
pub(super) fn tell_resolved(output: &Output, listing: &Listing) {
    tell_kept(output, listing, &listing.files);
}

/// prints what a reference resolved to as [`tell_resolved`] does, counting the files of it `kept`
/// when they are fewer than it lists, as `1 of 4 files`, with their size alone.
pub(super) fn tell_kept(output: &Output, listing: &Listing, kept: &[Remote]) {
    tell_facts(output, listing, kept);
    // a hugging face repository's .gitattributes, which every one holds, goes unsaid
    if matches!(listing.resolved, Reference::GitFolder(_)) {
        let unselected: Vec<&str> = listing
            .files
            .iter()
            .filter(|file| {
                listing.kept_only_if_selected(&file.path)
                    && !kept.iter().any(|held| held.path == file.path)
            })
            .map(|file| file.path.as_str())
            .collect();
        if !unselected.is_empty() {
            output.hint(&format!(
                "git's own files hold no data, so these are left out unless a select pattern keeps them: {}",
                units::some_of(&unselected, 10)
            ));
        }
    }
    for warning in &listing.warnings {
        output.warn(Warning::SourcePartial, warning);
    }
}

fn tell_facts(output: &Output, listing: &Listing, kept: &[Remote]) {
    // a plain link has nothing to tell before its bytes arrive
    if matches!(listing.resolved, Reference::Url(_)) {
        return;
    }
    let mut facts = vec![listing.resolved.shown_short()];
    facts.extend(
        listing
            .title
            .as_deref()
            .map(|title| output.clip(title, TITLE)),
    );
    let listed = units::count(listing.files.len() as u64, "file", "files");
    let narrowed = kept.len() < listing.files.len();
    let files = if narrowed {
        format!("{} of {listed}", units::grouped(kept.len() as u64))
    } else {
        listed
    };
    let within: Vec<&str> = listing
        .within
        .iter()
        .map(|folder| match folder.as_str() {
            "" => "the top folder",
            folder => folder,
        })
        .collect();
    facts.push(if within.is_empty() {
        files
    } else {
        format!("{files} in {}", within.join(", "))
    });
    let size = kept.iter().fold(estimate::Size::Bytes(0), |total, file| {
        total.and(estimate::Size::of(file.size, listing.listed(file)))
    });
    if size != estimate::Size::Unknown && (narrowed || listing.unavailable.is_empty()) {
        facts.push(size.words());
    }
    if !narrowed && !listing.unavailable.is_empty() {
        facts.push(units::count(
            listing.unavailable.len() as u64,
            "unfetchable entry",
            "unfetchable entries",
        ));
    }
    facts.extend(
        listing
            .license
            .as_ref()
            .map(|license| format!("license {license}")),
    );
    output.step("Resolved", &facts.join(" · "));
    if let Some(license) = &listing.license
        && let Some(limit) = restriction(license)
    {
        output.warn(
            Warning::LicenseRestricted,
            &format!("the license {license} does not allow {limit}"),
        );
    }
}

/// prints each dataset that moved to another version since `before`, and returns their names:
/// `Upgraded` where its ref is the one it was pinned for, so its source moved on, and `Moved`
/// where data.toml names another version, which may be older; `Would` ones for a `dry_run`. files
/// `guessed` by a dry run never count as changed.
pub(super) fn tell_upgrades(
    output: &Output,
    before: &Before,
    new: &Lock,
    dry_run: bool,
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
        let mut facts = vec![if before.resolved == dataset.resolved {
            dataset.resolved.clone()
        } else {
            format!("{} → {}", before.resolved, dataset.resolved)
        }];
        for (n, one, many) in [
            (changed, "file changed", "files changed"),
            (added, "file added", "files added"),
            (removed, "file removed", "files removed"),
        ] {
            if n > 0 {
                facts.push(units::count(n as u64, one, many));
            }
        }
        let verb = match (before.reference == dataset.reference, dry_run) {
            (true, false) => "Upgraded",
            (true, true) => "Would upgrade",
            (false, false) => "Moved",
            (false, true) => "Would move",
        };
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

    /// an archive to unpack needs room for itself and its files; a file kept as it came adds its
    /// size alone, one of unknown size none, and the sum saturates rather than wrapping.
    #[test]
    fn an_archive_to_unpack_needs_room_for_its_files_too() {
        const MB: u64 = 1_000_000;
        let url = Url::parse("https://example.org/a.zip").unwrap();
        let job = |size: Option<u64>, unpack: bool| {
            let file = LockedFile {
                path: "a.zip".to_owned(),
                size: size.unwrap_or_default(),
                sha256: digest::sha256(b"a"),
                md5: None,
                url: url.to_string(),
                unpacks_to: None,
            };
            let mut job = make_job(&Lock::default(), "a", &file, &url, false);
            job.fetch.expect.size = size;
            job.fetch.unpack = unpack;
            job
        };
        let jobs = [
            job(Some(MB), true),
            job(Some(MB / 2), false),
            job(None, true),
        ];
        assert_eq!(budget(jobs.iter()), 2 * MB + MB / 2);
        let huge = [job(Some(u64::MAX), true)];
        assert_eq!(budget(huge.iter()), u64::MAX);
    }

    fn context() -> Context {
        let mut context = Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
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
            at: None,
            extract: true,
            decompress: false,
            web_page: false,
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
        let entry = &manifest.datasets["set"].source;
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

    /// a version taken out of a ref resolves the one data.lock pins until an upgrade; another
    /// version, another dataset, an `@` in a link, which names other data, or a pin of something
    /// else than that dataset asks anew.
    #[test]
    fn a_version_taken_out_resolves_the_locked_one_unless_upgrading() {
        let manifest = Manifest::parse("[datasets.set]\nref = 'kaggle:uciml/iris'\n").unwrap();
        let entry = &manifest.datasets["set"].source;
        let commit = "0123456789abcdef0123456789abcdef01234567";
        for (before, resolved, now, kept) in [
            (
                "kaggle:uciml/iris@1",
                "kaggle:uciml/iris@1",
                "kaggle:uciml/iris",
                true,
            ),
            (
                "hf:datasets/x/y@main",
                &format!("hf:datasets/x/y@{commit}"),
                "hf:datasets/x/y",
                true,
            ),
            (
                "openneuro:ds000001@1.0.0",
                "openneuro:ds000001@1.0.0",
                "openneuro:ds000001",
                true,
            ),
            (
                "kaggle:uciml/iris@1",
                "kaggle:uciml/iris@1",
                "kaggle:uciml/iris@2",
                false,
            ),
            (
                "kaggle:uciml/iris@1",
                "kaggle:uciml/iris@1",
                "kaggle:uciml/wine",
                false,
            ),
            (
                "kaggle:uciml/iris",
                "kaggle:uciml/iris@3",
                "kaggle:uciml/iris@1",
                false,
            ),
            (
                "https://example.org/data@2",
                "https://example.org/data@2",
                "https://example.org/data",
                false,
            ),
            (
                "kaggle:uciml/iris@1",
                "https://example.org/elsewhere",
                "kaggle:uciml/iris",
                false,
            ),
        ] {
            let mut previous = locked("set");
            previous.reference = before.to_owned();
            previous.resolved = resolved.to_owned();
            let mut source = entry.clone();
            source.reference = now.to_owned();
            let expected = if kept { resolved } else { now };
            assert_eq!(
                pinned(&source, Some(&previous), false),
                expected,
                "{before} to {now}"
            );
            assert_eq!(
                pinned(&source, Some(&previous), true),
                now,
                "{before} to {now}"
            );
        }
    }

    #[test]
    fn jobs_pin_hashes_and_refresh_dead_links_at_the_locked_revision() {
        let lock = Lock {
            datasets: vec![locked("set")],
            ..Lock::default()
        };
        let file = &lock.datasets[0].files[0];
        let manifest = Manifest::parse("").unwrap();
        let download = job(&manifest, &lock, "set", file).unwrap();
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
        let error = job(&manifest, &lock, "set", &broken).err().unwrap();
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
            .save_stated_md5(
                &md5,
                url.as_str(),
                &crate::cache::Stated {
                    digest: stored.digest,
                    size: 4,
                    version: None,
                },
            )
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
        let project = Project::at(dir.path().to_path_buf());
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let lock = Lock {
            datasets: vec![locked("set")],
            ..Lock::default()
        };
        let jobs = (0..7)
            .map(|index| {
                let mut download = job(
                    &Manifest::parse("").unwrap(),
                    &lock,
                    "set",
                    &lock.datasets[0].files[0],
                )
                .unwrap();
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
        let fetched = block_on(runner.download(vec![
                job(
                    &Manifest::parse("").unwrap(),
                    &lock,
                    "set",
                    &lock.datasets[0].files[0],
                )
                .unwrap(),
            ]))
        .unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].origin, Origin::Cache);
        assert_eq!(fetched[0].digest, digest::sha256(b"data"));
        assert_eq!(context.work.counts().requests, 0);
    }

    /// an offline refusal of files of both kinds counts each kind, and names first those whose
    /// SHA-256 only a download learns, so they show though many others are missing too.
    #[test]
    fn an_offline_refusal_counts_each_kind_and_names_the_unlearned_first() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let runner = Loop::new(&context, &project, Arc::new(context.net()), cache);
        let missing = ["a/1.csv", "a/2.csv", "a/3.csv", "a/4.csv"];
        let error = runner.offline_missing(&["z/1.csv", "z/2.csv"], &missing);
        assert_eq!(
            error.message,
            "offline, and 6 files are not in the cache: 4 data.lock names, and 2 pinned by what their sources state, which must be downloaded once to learn their SHA-256"
        );
        assert_eq!(
            error.details,
            [
                "z/1.csv",
                "z/2.csv",
                "a/1.csv",
                "a/2.csv",
                "a/3.csv",
                "and 1 more"
            ]
        );
    }

    #[test]
    fn resolving_reports_manifest_locations_without_echoing_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
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
                Vec::new(),
            ))
            .unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid);
            assert_eq!(error.details, [detail]);
            assert!(!error.message.contains("private-value"));
            assert_eq!(context.work.counts().requests, 0);
        }
    }

    /// a named run resolves only its datasets: the others stay as data.lock has them though
    /// data.toml changed, dropped and added some, and no source of theirs is asked anything.
    #[test]
    fn relock_in_scope_keeps_other_records() {
        use crate::cli::project::tests::{listed, pinned, pinned_file, stated};
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
        let context = context();
        let manifest = Manifest::parse(
            "[datasets.a]\nref = \"https://example.org/a\"\n\n[datasets.b]\nref = \"https://example.org/b2\"\n\n[datasets.d]\nref = \"https://example.org/d\"\n",
        )
        .unwrap();
        let file = |name: &str| {
            vec![pinned_file(
                &format!("{name}.csv"),
                &format!("https://example.org/{name}"),
                name.as_bytes(),
            )]
        };
        let old = Lock {
            datasets: vec![
                pinned("a", lock::spec(&manifest.datasets["a"].source), file("a")),
                pinned("b", "sha256:an older entry".to_owned(), file("b")),
                pinned("c", "sha256:a dropped entry".to_owned(), file("c")),
            ],
            ..Lock::default()
        };
        let relock = |names: &[&str], given| {
            let scope = Scope::Only(names.iter().map(|name| (*name).to_owned()).collect());
            let cache = Cache::open(dir.path().join("cache")).unwrap();
            let runner =
                Loop::new(&context, &project, Arc::new(context.net()), cache).within(scope);
            block_on(runner.relock(&manifest, Some(old.clone()), &Upgrade::Nothing, given))
        };

        let (alone, before) = relock(&["a"], Vec::new()).unwrap();
        let listing = listed(
            "https://example.org/d",
            vec![stated("d.csv", "https://example.org/d", b"d")],
        );
        let (with_d, before_d) =
            relock(&["a", "d"], vec![Given::Listing("d".to_owned(), listing)]).unwrap();

        assert_eq!(alone, old);
        assert!(before.holds(&alone));
        assert_eq!(repinned(&before_d, &with_d), ["d"]);
        for name in ["a", "b", "c"] {
            assert_eq!(with_d.get(name), old.get(name), "{name}");
        }
        assert_eq!(context.work.counts().requests, 0);
    }

    #[test]
    fn unchanged_locks_are_not_rewritten_and_removed_datasets_leave_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::at(dir.path().to_path_buf());
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
                .write_lock(&Before::whole(Some(lock)), &Lock::default())
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
                false,
                &HashSet::new()
            ),
            ["changed", "revision"]
        );
        assert_eq!(
            tell_upgrades(
                &context.output,
                &Before::whole(Some(old.clone())),
                &old,
                false,
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
        let sized = HashMap::new();
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
                Some(&BTreeSet::from(["climate-precip".to_owned()])),
                &sized
            ),
            [("Removed", "gone from data.lock".to_owned())],
            "the dataset the last line tells about is left out"
        );
        let many = Lock {
            datasets: (0..25).map(|n| locked(&format!("set-{n:02}"))).collect(),
            ..Lock::default()
        };
        let sizeless = HashMap::from([("set-00/data.csv".to_owned(), estimate::Size::Unknown)]);
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
        // the rest are pinned for the first time too, never told as locked again
        assert_eq!(
            lines[20],
            (
                "Would pin",
                "5 more datasets, new in data.lock · 5 files".to_owned()
            )
        );
    }

    #[test]
    fn a_dry_run_tells_a_removal_as_the_real_run_does() {
        let context = context();
        let before = Before::whole(Some(Lock {
            datasets: vec![locked("gone")],
            ..Lock::default()
        }));
        let told = |verbs| {
            lock_lines(
                &context.output,
                &before,
                &Lock::default(),
                verbs,
                None,
                &HashMap::new(),
            )
        };
        assert_eq!(
            told(Verbs::DONE),
            [("Removed", "gone from data.lock".to_owned())]
        );
        assert_eq!(
            told(Verbs::WOULD),
            [("Would remove", "gone from data.lock".to_owned())]
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
                true,
                &guessed
            ),
            ["moved"],
            "only a new version is told when the bytes are a guess"
        );
    }

    /// a license is told with its label, since a source's own may be a bare `unknown`.
    #[test]
    fn the_resolved_line_labels_the_license() {
        let output = Output::new(
            Style {
                color: false,
                unicode: true,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        );
        let listing = Listing {
            within: Vec::new(),
            access: source::Access::Unknown,
            resolved: Reference::parse("zenodo:3608990").unwrap().reference,
            title: Some("Movie reviews".to_owned()),
            license: Some("unknown".to_owned()),
            doi: None,
            files: Vec::new(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
            rounded: None,
        };
        tell_resolved(&output, &listing);
        assert_eq!(
            output.said(),
            ["   Resolved zenodo:3608990 · Movie reviews · 0 files · 0 B · license unknown"]
        );
    }

    /// the files kept and those listed are counted alike, with their thousands apart.
    #[test]
    fn the_resolved_line_counts_kept_files_as_it_counts_listed_ones() {
        let output = Output::new(
            Style {
                color: false,
                unicode: true,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        );
        let remote = |index: usize| Remote {
            path: format!("{index}.csv"),
            size: Some(1),
            sha256: None,
            md5: None,
            url: Url::parse(&format!("https://example.org/{index}.csv")).unwrap(),
            named_by_server: false,
            etag: None,
            blob: None,
        };
        let listing = Listing {
            within: Vec::new(),
            access: source::Access::Unknown,
            resolved: Reference::parse("zenodo:3608990").unwrap().reference,
            title: None,
            license: None,
            doi: None,
            files: (0..1001).map(remote).collect(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
            rounded: None,
        };
        tell_kept(&output, &listing, &listing.files[..1000]);
        let said = output.said().join("\n");
        assert!(said.contains("1,000 of 1,001 files"), "{said}");
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
            rounded: None,
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
        let others = Lock {
            datasets: vec![locked("b"), locked("c")],
            ..Lock::default()
        };
        let (lock, kept) = settled(&others, vec![locked("a")], vec![changed, locked("d")]);
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
            renamed: Vec::new(),
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
            &Lock {
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
            renamed: Vec::new(),
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
        // arXiv's own license, and the one it assumes for papers from before it asked
        for arxiv in [
            "http://arxiv.org/licenses/nonexclusive-distrib/1.0/",
            "http://arxiv.org/licenses/assumed-1991-2003/",
        ] {
            assert_eq!(restriction(arxiv), Some("sharing copies"), "{arxiv}");
        }
        assert_eq!(
            restriction("http://creativecommons.org/licenses/by-nc-sa/4.0/"),
            Some("commercial use")
        );
        for license in [
            "MIT",
            "Apache-2.0",
            "CC-BY-4.0",
            "CC0-1.0",
            "http://creativecommons.org/licenses/by/4.0/",
        ] {
            assert_eq!(restriction(license), None);
        }
    }
}
