//! `fetchloom build --dry-run`: what a build would do with each step, and the command of each step
//! that would or may run. each step is decided by [`course`], the decision the build itself
//! takes, against the cache as the build would find it at that step, so the dry run says what
//! the build then does. it runs, fetches and writes nothing, and asks no web mirror.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{BUILD, Course, course, pin, pinned_as_asked, selected, shown, sized};
use crate::cache::{Cache, Member};
use crate::cli::fetch::data_folder::DataFolder;
use crate::cli::sync::Scope;
use crate::cli::{Context, peek_cache};
use crate::digest::Sha256;
use crate::error::Error;
use crate::mirror::{Location, Mirrors, Wanted};
use crate::output::Output;
use crate::plan;
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::recipe::{self, Change, Slot, Step};
use crate::units;

/// why a step whose key data.lock pins runs: the result is nowhere a build looks.
const NOWHERE: &str = "its pinned result is not on this machine or a mirror";

/// what `build --dry-run` found.
#[derive(Serialize)]
pub(in crate::cli) struct Previewed {
    project: PathBuf,
    /// the recipes named, or every recipe when none was.
    selected: Vec<String>,
    /// each recipe the selected ones need, in the order the steps would run.
    steps: Vec<Planned>,
    dry_run: bool,
}

/// one step as a dry run finds it.
#[derive(Serialize)]
struct Planned {
    name: String,
    action: Action,
    /// the key of the result building it now would make, `sha256:` and its hash, once data.lock
    /// pins every dataset it reads, all its code is there and no step before it must run first
    /// to tell.
    key: Option<String>,
    /// why it would or may run: what moved since data.lock's result of it was built, or that
    /// its pinned result is nowhere a build looks.
    changes: Vec<String>,
    /// the program and its arguments, each folder in place.
    run: Vec<String>,
    /// the variables set for the step alone.
    env: BTreeMap<String, String>,
    /// each dataset it reads, with the folder the step finds a copy of its files in.
    inputs: Vec<Input>,
    /// the files and folders of code whose bytes enter its key.
    code: Vec<String>,
    /// the folder in the project its result goes in.
    output: String,
    /// what its line says after its name.
    #[serde(skip)]
    said: String,
}

#[derive(Serialize)]
struct Input {
    name: String,
    folder: String,
}

/// what a build would do with a step, each the dry run's word for what the build then reports:
/// `would_run` for `ran`, `would_fetch` for `fetched`, `would_reuse` for `reused`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// its step runs: data.lock pins no result for what building it now would make and no build
    /// left one, or the result data.lock pins is neither in the cache nor on a mirror.
    WouldRun,
    /// data.lock pins its result as building it now would make, and a folder mirror holds the
    /// files the cache lacks, which the build copies in.
    WouldFetch,
    /// a build of the same key left a result the cache holds whole, which the build pins.
    WouldReuse,
    /// what it does is known only once the build learns something first: the result of a step
    /// before it that runs, a dataset it pins anew since data.toml changed, or whether a web
    /// mirror holds its pinned result.
    MayRun,
    /// data.lock pins its result as building it now would make, and the cache holds it.
    UpToDate,
}

impl Action {
    fn verb(self) -> &'static str {
        match self {
            Action::WouldRun => "Would run",
            Action::WouldFetch => "Would fetch",
            Action::WouldReuse => "Would reuse",
            Action::MayRun => "May run",
            Action::UpToDate => "Up to date",
        }
    }

    /// whether its step's command may run, so the dry run shows it.
    fn runs(self) -> bool {
        matches!(self, Action::WouldRun | Action::MayRun)
    }
}

/// what building `names` would do, told as a dry run tells it, with nothing run or changed. with
/// `current`, as `--locked` asks, data.lock must pin what data.toml asks first.
pub(super) fn preview(
    context: &Context,
    project: &Project,
    manifest: &Manifest,
    names: &[String],
    current: Option<&str>,
) -> Result<Previewed, Error> {
    let read = project.lock_and_text()?;
    let selected = selected(manifest, names);
    let roots: Vec<&str> = selected.iter().map(String::as_str).collect();
    let closure = recipe::closure(&manifest.datasets, &roots);
    if let Some(instead) = current {
        let scope = Scope::Only(closure.iter().map(|name| (*name).to_owned()).collect());
        pinned_as_asked(project, manifest, read.as_ref(), instead, &scope)?;
    }
    let lock = read.map(|(lock, _)| lock).unwrap_or_default();
    let cache = peek_cache()?;
    let ahead = Ahead::new(context, project, manifest, &cache);
    let steps = foresee(project, manifest, &lock, &closure, ahead)?;
    tell(&context.output, &steps);
    Ok(Previewed {
        project: project.root.clone(),
        selected,
        steps,
        dry_run: true,
    })
}

/// the cache as a build would find it at each step, and where the build would look for the
/// files of a pinned result the cache lacks: the data folder, then the mirrors.
struct Ahead<'a> {
    context: &'a Context,
    project: &'a Project,
    manifest: &'a Manifest,
    cache: &'a Cache,
    /// the objects the steps before would have put in the cache: the files of the fetched
    /// datasets a step that runs reads, and the pinned files a mirror gives or a step makes again.
    added: HashSet<Sha256>,
    /// the folder mirrors there now, in the order a build asks them.
    folders: Vec<&'a Path>,
    /// the web mirrors a build would ask, which a dry run does not: none offline.
    web: Vec<String>,
}

/// where the files of a pinned result that the cache lacks would come from.
enum Found {
    /// these hold every one between them: the data folder, by its name, and folder mirrors.
    Here(Vec<String>),
    /// nothing here holds them all, and only asking a web mirror would tell.
    Web,
    Nowhere,
}

impl<'a> Ahead<'a> {
    fn new(
        context: &'a Context,
        project: &'a Project,
        manifest: &'a Manifest,
        cache: &'a Cache,
    ) -> Ahead<'a> {
        let mirrors: &Mirrors = &context.mirrors.mirrors;
        let offline = context.offline.enabled();
        let folders = mirrors
            .list
            .iter()
            .filter_map(|location| match location {
                Location::Folder(root) => Some(root.as_path()),
                Location::Web(_) => None,
            })
            .filter(|root| crate::mirror::folder::probe(root).is_ok())
            .collect();
        let web = mirrors
            .list
            .iter()
            .filter(|location| !offline && matches!(location, Location::Web(_)))
            .map(ToString::to_string)
            .collect();
        Ahead {
            context,
            project,
            manifest,
            cache,
            added: HashSet::new(),
            folders,
            web,
        }
    }

    fn holds(&self, file: &Member) -> bool {
        self.added.contains(&file.sha256)
            || self.cache.find(&file.sha256, Some(file.size)).is_some()
    }

    fn whole(&self, files: &[Member]) -> bool {
        files.iter().all(|file| self.holds(file))
    }

    /// where the files of `files` the cache lacks would come from, as a build takes them: from
    /// the data folder where `lock` places them and they hold the pinned bytes, then from each
    /// mirror in order.
    fn found(&self, files: &[Member], lock: &Lock) -> Found {
        let lacking: Vec<&Member> = files.iter().filter(|file| !self.holds(file)).collect();
        let wanted: Vec<Wanted> = lacking
            .iter()
            .map(|file| Wanted {
                digest: file.sha256,
                size: file.size,
                label: file.path.clone(),
            })
            .collect();
        let folder = DataFolder::planned(self.project, self.manifest, lock, self.cache);
        let here = folder.holds(self.context, &wanted);
        let mut lacking: Vec<&Member> = lacking
            .into_iter()
            .zip(here)
            .filter_map(|(file, held)| (!held).then_some(file))
            .collect();
        let mut sources = Vec::new();
        if lacking.len() < wanted.len() {
            sources.push(self.manifest.data_dir.clone());
        }
        if lacking.is_empty() {
            return Found::Here(sources);
        }
        for root in &self.folders {
            let before = lacking.len();
            lacking.retain(|file| !crate::mirror::folder::has(root, &file.sha256, file.size));
            if lacking.len() < before {
                sources.push(root.display().to_string());
            }
            if lacking.is_empty() {
                return Found::Here(sources);
            }
        }
        if self.web.is_empty() {
            Found::Nowhere
        } else {
            Found::Web
        }
    }

    fn add(&mut self, files: &[Member]) {
        self.added.extend(files.iter().map(|file| file.sha256));
    }

    /// adds what a build puts in the cache before it runs `step`: every file of the fetched
    /// datasets it reads, and each archive it downloads to unpack a file the cache lacks.
    ///
    /// # Errors
    ///
    /// the refusals of [`plan::inputs`], which the build would meet there too.
    fn readied(&mut self, manifest: &Manifest, lock: &Lock, step: &Step) -> Result<(), Error> {
        let sources = Lock {
            datasets: lock
                .datasets
                .iter()
                .filter(|locked| {
                    step.inputs
                        .iter()
                        .any(|read| !read.made && read.name == locked.name)
                })
                .cloned()
                .collect(),
            derived: Vec::new(),
        };
        let planned = plan::inputs(manifest, &sources, self.cache)?;
        let mut added = Vec::new();
        for target in &planned.targets {
            let lacked = !self.added.contains(&target.sha256)
                && self.cache.find(&target.sha256, Some(target.size)).is_none();
            if let Some(file) = target.file().filter(|_| lacked) {
                added.push(file.sha256);
            }
            added.push(target.sha256);
        }
        self.added.extend(added);
        Ok(())
    }
}

/// what a build of `closure` would do with each recipe in it, in the order it takes them, each
/// decided by [`course`] with data.lock as the build would hold it then: each result a step
/// before pinned, and the files it put in the cache. a dataset the build pins anew and the result
/// of a step that runs for a key data.lock does not pin are known only once the build has them,
/// so each step that reads one may run.
fn foresee(
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    closure: &[&str],
    mut ahead: Ahead<'_>,
) -> Result<Vec<Planned>, Error> {
    let mut now = lock.clone();
    let mut unknown: BTreeSet<&str> = closure
        .iter()
        .copied()
        .filter(|name| pinned_anew(manifest, lock, name))
        .collect();
    let mut steps = Vec::new();
    for &name in closure {
        let Some(step) = Step::read(&project.root, manifest, &now, name)? else {
            continue;
        };
        let moved = step.changes(lock.result(name));
        let mut changes: Vec<String> = moved.iter().map(ToString::to_string).collect();
        changes.extend(
            step.inputs
                .iter()
                .filter(|read| {
                    !read.made
                        && lock.get(&read.name).is_some()
                        && unknown.contains(read.name.as_str())
                })
                .map(|read| format!("{} changed in data.toml", read.name)),
        );
        let waits: Vec<&str> = step
            .inputs
            .iter()
            .filter(|read| unknown.contains(read.name.as_str()))
            .map(|read| read.name.as_str())
            .collect();
        let key = step.key().filter(|_| waits.is_empty());
        let (action, said) = match key {
            None => {
                unknown.insert(name);
                if waits.is_empty() {
                    (Action::WouldRun, changes.join("; "))
                } else {
                    let said = waiting(&ahead, &now, &step, &moved, &changes, &waits);
                    (Action::MayRun, said)
                }
            }
            Some(key) => match course(&now, name, key, |files| ahead.whole(files), ahead.cache) {
                Course::Held(_) => (Action::UpToDate, String::new()),
                Course::Lacking(pinned) => {
                    let files = pinned.files.clone();
                    let decided = match ahead.found(&files, &now) {
                        Found::Here(sources) => {
                            let said = format!(
                                "{} as data.lock pins it, from {}",
                                sized(&files),
                                units::listed(&sources, "and")
                            );
                            (Action::WouldFetch, said)
                        }
                        Found::Web => {
                            changes.push(web_said(&ahead));
                            (Action::MayRun, web_said(&ahead))
                        }
                        Found::Nowhere => {
                            ahead.readied(manifest, &now, &step)?;
                            changes.push(NOWHERE.to_owned());
                            (Action::WouldRun, NOWHERE.to_owned())
                        }
                    };
                    // fetched, or made again as data.lock pins it, or the build stops there
                    ahead.add(&files);
                    decided
                }
                Course::Built(built) => {
                    let said = format!(
                        "built before from the same inputs, {}",
                        sized(&built.members)
                    );
                    pin(&mut now, &step, key, built.tree, built.members);
                    (Action::WouldReuse, said)
                }
                Course::New => {
                    ahead.readied(manifest, &now, &step)?;
                    unknown.insert(name);
                    (Action::WouldRun, changes.join("; "))
                }
            },
        };
        steps.push(planned(manifest, &step, key, changes, action, said));
    }
    Ok(steps)
}

/// whether the build pins the fetched dataset `name` anew before any step reads it: data.lock
/// has no pin of it, or one data.toml moved past, so its content is known only then.
fn pinned_anew(manifest: &Manifest, lock: &Lock, name: &str) -> bool {
    manifest
        .datasets
        .get(name)
        .filter(|entry| entry.source().is_some())
        .is_some_and(|entry| {
            lock.get(name)
                .is_none_or(|locked| locked.drift(entry).is_some())
        })
}

/// the line of `step`, which waits on `waits`, the datasets of `now` the build learns only as
/// it goes: what moved, then the steps it waits on that no change names. a step with nothing
/// moved whose pinned result is in the cache or on a folder mirror runs again only if a result
/// it reads changes.
fn waiting(
    ahead: &Ahead<'_>,
    now: &Lock,
    step: &Step,
    moved: &[Change],
    changes: &[String],
    waits: &[&str],
) -> String {
    let named = |name: &str| {
        moved.iter().any(|change| {
            matches!(change, Change::Input(input) | Change::Unmade(input) if input == name)
        })
    };
    let results: Vec<&str> = waits
        .iter()
        .copied()
        .filter(|name| {
            step.inputs
                .iter()
                .any(|read| read.made && read.name == *name)
        })
        .collect();
    let pinned = now
        .result(&step.name)
        .filter(|pinned| step.key() == Some(pinned.key));
    let found = pinned.map(|pinned| {
        if ahead.whole(&pinned.files) {
            Found::Here(Vec::new())
        } else {
            ahead.found(&pinned.files, now)
        }
    });
    let mut parts = changes.to_vec();
    match (changes.is_empty(), found) {
        (true, Some(Found::Here(_))) if !results.is_empty() => {
            return match results.as_slice() {
                [one] => format!("runs again if {one}'s result changes"),
                many => format!(
                    "runs again if the results of {} change",
                    units::listed(many, "and")
                ),
            };
        }
        (true, Some(Found::Web)) => parts.push(web_said(ahead)),
        (true, Some(Found::Nowhere)) => parts.push(NOWHERE.to_owned()),
        _ => {}
    }
    let unnamed: Vec<&str> = results.into_iter().filter(|name| !named(name)).collect();
    if !unnamed.is_empty() {
        let first = if unnamed.len() == 1 { "runs" } else { "run" };
        parts.push(format!(
            "it reads {}, which {first} first",
            units::listed(&unnamed, "and")
        ));
    }
    parts.join("; ")
}

/// why a step whose pinned result the cache lacks may run when only a web mirror could give it.
fn web_said(ahead: &Ahead<'_>) -> String {
    format!(
        "its pinned result is not on this machine; it runs unless {} holds it",
        units::listed(&ahead.web, "or")
    )
}

/// `step` as a dry run reports it, its folders named by where a build puts them.
fn planned(
    manifest: &Manifest,
    step: &Step,
    key: Option<Sha256>,
    changes: Vec<String>,
    action: Action,
    said: String,
) -> Planned {
    let folder = |slot: Slot<'_>| match slot {
        Slot::Input(name) => OsString::from(format!("{BUILD}/in/{name}")),
        Slot::Out => OsString::from(format!("{BUILD}/out")),
    };
    let run = manifest.datasets[&step.name]
        .recipe()
        .map(|recipe| {
            recipe
                .run
                .iter()
                .map(|word| word.expand(&folder).to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    Planned {
        name: step.name.clone(),
        action,
        key: key.map(|key| format!("sha256:{key}")),
        changes,
        run,
        env: step.env.clone(),
        inputs: step
            .inputs
            .iter()
            .map(|read| Input {
                name: read.name.clone(),
                folder: format!("{BUILD}/in/{}", read.name),
            })
            .collect(),
        code: step.code.iter().map(|(path, _)| path.clone()).collect(),
        output: plan::folder(manifest, &step.name),
        said,
    }
}

/// a line for each step, and under one that would or may run its command and what it reads and
/// writes.
fn tell(output: &Output, steps: &[Planned]) {
    for step in steps {
        if step.said.is_empty() {
            output.step(step.action.verb(), &step.name);
        } else {
            output.step(
                step.action.verb(),
                &format!("{} \u{b7} {}", step.name, step.said),
            );
        }
        if !step.action.runs() {
            continue;
        }
        let words: Vec<String> = step.run.iter().map(|word| shown(word)).collect();
        output.item(&words.join(" "));
        let read: Vec<&str> = step
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .chain(step.code.iter().map(String::as_str))
            .collect();
        let with = if step.env.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = step
                .env
                .iter()
                .map(|(name, value)| shown(&format!("{name}={value}")))
                .collect();
            format!(", with {}", pairs.join(" "))
        };
        output.item(&format!(
            "reads {}{with}; writes {}",
            units::listed(&read, "and"),
            step.output
        ));
    }
    if steps.is_empty() {
        output.done(
            "Ok",
            "data.toml has no recipes, so there is nothing to build",
        );
    } else if steps.iter().all(|step| step.action == Action::UpToDate) {
        output.done(
            "Ok",
            "every recipe is built as data.lock pins it; dry run, nothing was run or changed",
        );
    } else if steps.iter().any(|step| step.action.runs()) {
        output.done("Ok", "dry run, nothing was run or changed");
        output.hint(&format!(
            "{BUILD} is a folder a build makes in the cache for each step: the step finds a copy of each dataset it reads in {BUILD}/in, and what it writes in {BUILD}/out becomes its dataset"
        ));
    } else {
        output.done(
            "Ok",
            "no step would run; dry run, nothing was run or changed",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{cache, store};
    use crate::digest;
    use crate::project::lock::{Locked, LockedFile, spec};

    const TOML: &str = "[datasets.raw]\nref = \"zenodo:1\"\n\n[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\n\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"t\", \"{clean}\", \"{out}\"]\n";

    /// the project of [`TOML`], with raw pinned as data.toml asks and the results of clean and
    /// tidy pinned as building them now would make, `done` and `tidied`.
    fn project() -> (tempfile::TempDir, Project, Manifest, Lock) {
        let root = tempfile::tempdir().unwrap();
        let manifest = Manifest::parse(TOML).unwrap();
        let source = manifest.datasets["raw"].source().unwrap();
        let mut lock = Lock {
            datasets: vec![Locked {
                name: "raw".to_owned(),
                reference: "zenodo:1".to_owned(),
                spec: spec(source),
                extract: source.extract,
                resolved: "zenodo:1@1".to_owned(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Unknown,
                timestamp: "2026-11-10T00:00:00Z".to_owned(),
                files: vec![LockedFile {
                    path: "a.csv".to_owned(),
                    size: 3,
                    sha256: digest::sha256(b"abc"),
                    md5: None,
                    url: "https://example.org/a.csv".to_owned(),
                    unpacks_to: None,
                }],
            }],
            ..Lock::default()
        };
        let project = Project {
            root: root.path().to_path_buf(),
        };
        for (name, bytes) in [("clean", &b"done"[..]), ("tidy", b"tidied")] {
            let step = Step::read(&project.root, &manifest, &lock, name)
                .unwrap()
                .unwrap();
            let members = vec![Member {
                path: format!("{name}.csv"),
                size: bytes.len() as u64,
                sha256: digest::sha256(bytes),
            }];
            let tree = crate::cache::tree_hash(&members);
            pin(&mut lock, &step, step.key().unwrap(), tree, members);
        }
        (root, project, manifest, lock)
    }

    /// a run that asks `mirrors`, offline or not, and prints nothing.
    fn context(mirrors: Mirrors, offline: bool) -> Context {
        let style = crate::output::Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let mut context = Context::new(Output::new(
            style,
            crate::output::Level::Quiet,
            false,
            false,
        ));
        context.mirrors = crate::cli::fetch::mirrors::Mirrored::new(mirrors);
        context.offline = crate::net::Offline::from_settings(offline, false);
        context
    }

    /// each step of `manifest` and `lock` as a dry run with `mirrors` finds it, with `cache`.
    fn foreseen(
        project: &Project,
        manifest: &Manifest,
        lock: &Lock,
        cache: &Cache,
        mirrors: &Mirrors,
        offline: bool,
    ) -> Vec<(String, Action, String)> {
        let closure = recipe::closure(&manifest.datasets, &["tidy"]);
        let context = context(mirrors.clone(), offline);
        let ahead = Ahead::new(&context, project, manifest, cache);
        foresee(project, manifest, lock, &closure, ahead)
            .unwrap()
            .into_iter()
            .map(|step| (step.name, step.action, step.said))
            .collect()
    }

    /// a pinned result the cache lacks runs again where no mirror could give it, as offline,
    /// where no web mirror is asked; with a web mirror it may run, since only asking it tells.
    /// a copy in the data folder comes first, as the build takes it from there.
    #[test]
    fn a_result_only_a_web_mirror_could_give_may_run() {
        let (_root, project, manifest, lock) = project();
        let (_folder, cache) = cache();
        store(&cache, b"tidied");
        let web = Location::Web(url::Url::parse("https://lab.example/fetchloom/").unwrap());
        let web = Mirrors::new(vec![web], &[], false);
        let lacking = "its pinned result is not on this machine; it runs unless https://lab.example/fetchloom holds it";
        let placed = "1 file, 4 B as data.lock pins it, from data";
        for (mirrors, offline, action, said) in [
            (&Mirrors::default(), false, Action::WouldRun, NOWHERE),
            (&web, true, Action::WouldRun, NOWHERE),
            (&web, false, Action::MayRun, lacking),
            (&web, false, Action::WouldFetch, placed),
        ] {
            if action == Action::WouldFetch {
                let folder = project.root.join("data").join("clean");
                std::fs::create_dir_all(&folder).unwrap();
                std::fs::write(folder.join("clean.csv"), b"done").unwrap();
            }
            let steps = foreseen(&project, &manifest, &lock, &cache, mirrors, offline);
            assert_eq!(
                steps,
                [
                    ("clean".to_owned(), action, said.to_owned()),
                    ("tidy".to_owned(), Action::UpToDate, String::new())
                ],
                "{mirrors:?}, offline {offline}"
            );
        }
    }

    /// a step that reads what a step running for a new key makes may run, with no key until
    /// then: again only if that changes while its own pinned result is here, and with what keeps
    /// it from being up to date otherwise. so may a step reading a dataset data.toml moved past
    /// its pin, which the build pins anew first.
    #[test]
    fn a_step_reading_what_the_build_learns_first_may_run() {
        let (_root, project, _, lock) = project();
        let changed = Manifest::parse(&TOML.replace("[\"t\", \"{raw}\"", "[\"u\", \"{raw}\""));
        let moved = Manifest::parse(&TOML.replace("zenodo:1", "zenodo:2"));
        let (changed, moved) = (changed.unwrap(), moved.unwrap());
        let again = "runs again if clean's result changes";
        let nowhere = format!("{NOWHERE}; it reads clean, which runs first");
        for (manifest, tidied, clean, said) in [
            (&changed, true, (Action::WouldRun, "run changed"), again),
            (
                &changed,
                false,
                (Action::WouldRun, "run changed"),
                nowhere.as_str(),
            ),
            (
                &moved,
                true,
                (Action::MayRun, "raw changed in data.toml"),
                again,
            ),
        ] {
            let (_folder, cache) = cache();
            store(&cache, b"done");
            if tidied {
                store(&cache, b"tidied");
            }
            let closure = recipe::closure(&manifest.datasets, &["tidy"]);
            let context = context(Mirrors::default(), false);
            let ahead = Ahead::new(&context, &project, manifest, &cache);
            let steps = foresee(&project, manifest, &lock, &closure, ahead).unwrap();
            let found: Vec<(&str, Action, &str)> = steps
                .iter()
                .map(|step| (step.name.as_str(), step.action, step.said.as_str()))
                .collect();
            assert_eq!(
                found,
                [("clean", clean.0, clean.1), ("tidy", Action::MayRun, said)]
            );
            assert_eq!(steps[1].key, None);
        }
    }
}
