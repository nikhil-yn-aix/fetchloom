//! pinned files the project's data folder holds, taken into the cache before any mirror or source
//! is asked, so a project copied with its data needs no download. a file there is only read: the
//! cache gets a clone or copy that hashes to the pin, never a link a later edit would reach.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::cache::{Cache, Copied, Sealed, holds_pinned};
use crate::cli::Context;
use crate::cli::sync::disk::{apart, vouched_before};
use crate::digest::Sha256;
use crate::error::Error;
use crate::mirror::Wanted;
use crate::output::Output;
use crate::place::Placer;
use crate::plan;
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::project::state::Look;
use crate::stop::Stop;
use crate::units;
use crate::volume::is_plain;
use crate::work::Work;

/// files copied from the data folder at once, as from a folder mirror.
const COPIES: usize = 8;

/// the files of a project's data folder that may hold pinned objects: for each object, the paths
/// data.lock places it at. none means a run asks the mirrors and sources alone.
#[derive(Default)]
pub(in crate::cli) struct DataFolder {
    /// the project folder, which every path is below.
    root: PathBuf,
    /// the data folder, where its files are checked to sit in real folders.
    folder: PathBuf,
    /// the data folder as data.toml names it, for what a run says.
    name: String,
    /// by object, each path that may hold it, in the order of the plan.
    paths: HashMap<Sha256, Vec<Place>>,
    /// the time before which the project's state vouches for a file that still looks as it
    /// records, as sync trusts it.
    before: u64,
}

/// a path below the project folder, with `/` between folders, that may hold an object, with what
/// the project's state records of the file there.
struct Place {
    path: String,
    record: Option<Record>,
}

/// what the project's state recorded of a file: whether it held the pinned bytes, and how it
/// looked then.
#[derive(Clone, Copy)]
struct Record {
    pinned: bool,
    look: Look,
}

/// what the state says of the file at a path, by how it looks now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Said {
    /// it holds the pinned bytes, which counts without reading it.
    Pinned,
    /// it holds other bytes, so it is never read.
    Other,
    /// nothing, so only its bytes tell.
    Nothing,
}

/// what `record` says of a file that looks as `now`: something only while the file looks
/// exactly as recorded, and was recorded before `before`, as sync trusts a recorded file.
fn said(record: Option<Record>, now: Look, before: u64) -> Said {
    match record {
        Some(record) if record.look == now && record.look.modified < before => {
            if record.pinned {
                Said::Pinned
            } else {
                Said::Other
            }
        }
        _ => Said::Nothing,
    }
}

impl DataFolder {
    /// the files `at` names, each a path data.lock places an object at that placing found holding
    /// its pinned bytes, by the state or by hashing.
    pub(in crate::cli) fn found(
        project: &Project,
        manifest: &Manifest,
        at: Vec<(Sha256, String)>,
    ) -> DataFolder {
        let found = at
            .into_iter()
            .map(|(digest, path)| (digest, Place { path, record: None }));
        DataFolder::of(project, manifest, found)
    }

    /// every file data.lock places, laid out as `lock` plans it, with what the project's state
    /// records of each. a data folder that is a link, or a lock that does not plan, gives none.
    pub(in crate::cli) fn planned(
        project: &Project,
        manifest: &Manifest,
        lock: &Lock,
        cache: &Cache,
    ) -> DataFolder {
        let planned = match plan::from_cache(manifest, lock, cache) {
            Ok(planned) => planned,
            Err(error) => {
                tracing::debug!(%error.message, "no plan, so nothing comes from the data folder");
                return DataFolder::default();
            }
        };
        let (state, written) = project.state();
        let places = planned.targets.into_iter().map(|target| {
            let record = state.files.get(&target.path).map(|placed| Record {
                pinned: placed.sha256 == target.sha256,
                look: placed.look,
            });
            let path = target.path;
            (target.sha256, Place { path, record })
        });
        let mut data = DataFolder::of(project, manifest, places);
        if !data.is_empty() && !state.files.is_empty() {
            let other_device = apart(&data.folder, &project.own_folder());
            data.before = vouched_before(written, other_device);
        }
        data
    }

    fn of(
        project: &Project,
        manifest: &Manifest,
        places: impl Iterator<Item = (Sha256, Place)>,
    ) -> DataFolder {
        let mut paths: HashMap<Sha256, Vec<Place>> = HashMap::new();
        for (digest, place) in places {
            paths.entry(digest).or_default().push(place);
        }
        if paths.is_empty() {
            return DataFolder::default();
        }
        let Ok(folder) = project.planned_data_dir(manifest) else {
            return DataFolder::default();
        };
        DataFolder {
            root: project.root.clone(),
            folder,
            name: manifest.data_dir.clone(),
            paths,
            before: 0,
        }
    }

    /// whether no file here may hold any object, so there is nothing to look for.
    pub(in crate::cli) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// which of `wanted` a file here holds, as a dry run tells it: one the state vouches for
    /// unread, any other once it hashes to its pin. a stop leaves the rest uncounted.
    pub(in crate::cli) fn holds(&self, context: &Context, wanted: &[Wanted]) -> Vec<bool> {
        self.counted(context, wanted, true)
    }

    /// which of `wanted` a file here may hold, by one look and no read: one the state vouches for,
    /// or of the pinned size. it only spares a question when nothing here can hold a file.
    pub(in crate::cli) fn may_hold(&self, context: &Context, wanted: &[Wanted]) -> Vec<bool> {
        self.counted(context, wanted, false)
    }

    /// which of `wanted` a file here holds by what the state says of it, hashing a file it says
    /// nothing of when `read`, else counting it by its size.
    fn counted(&self, context: &Context, wanted: &[Wanted], read: bool) -> Vec<bool> {
        if self.is_empty() {
            return vec![false; wanted.len()];
        }
        let placer = Placer::new(&self.folder);
        crate::place::run_all(wanted, context.threads.at_most(COPIES), |want| {
            self.places(want).any(|(place, path)| {
                let Some(now) = readable(&placer, &path) else {
                    return false;
                };
                match said(place.record, now, self.before) {
                    Said::Pinned => true,
                    Said::Other => false,
                    Said::Nothing if now.size != want.size => false,
                    Said::Nothing if !read => true,
                    Said::Nothing => {
                        holds_pinned(&path, &want.digest, want.size, &context.stop).unwrap_or(false)
                    }
                }
            })
        })
    }

    /// takes into the cache each of `wanted` a file here holds, from the first path that hashes to
    /// the pin, and returns the rest; a file with other bytes, or unreadable, is left. fails at a
    /// stop or when the cache cannot be written.
    pub(in crate::cli) fn take(
        &self,
        context: &Context,
        cache: &Cache,
        mut wanted: Vec<Wanted>,
    ) -> Result<Vec<Wanted>, Error> {
        let asked: Vec<&Wanted> = wanted
            .iter()
            .filter(|want| self.paths.contains_key(&want.digest))
            .collect();
        if asked.is_empty() {
            return Ok(wanted);
        }
        let stored = self.copy_all(cache, &asked, context).map_err(|err| {
            if err.kind() == io::ErrorKind::Interrupted {
                Stop::stopped()
            } else {
                Error::io(
                    format_args!("could not copy from {} into the cache", self.name),
                    &err,
                )
            }
        })?;
        let (mut files, mut bytes) = (0, 0);
        for object in &stored {
            if object.new {
                context.work.wrote(&object.digest);
            }
            files += 1;
            bytes += object.size;
        }
        Work::add(&context.work.data_folder_objects, files);
        Work::add(&context.work.data_folder_bytes, bytes);
        if files > 0 {
            self.tell(&context.output, "Copied", files, bytes);
        }
        let taken: HashSet<Sha256> = stored.iter().map(|object| object.digest).collect();
        wanted.retain(|want| !taken.contains(&want.digest));
        Ok(wanted)
    }

    /// says with `verb` that `files` of `bytes` come from the data folder into the cache.
    pub(in crate::cli) fn tell(&self, output: &Output, verb: &str, files: u64, bytes: u64) {
        output.step(
            verb,
            &format!(
                "{} \u{b7} {} from {} into the cache",
                units::count(files, "file", "files"),
                units::size(bytes),
                self.name
            ),
        );
    }

    /// copies each of `asked` from the first of its paths that holds its pinned bytes, a few at
    /// once, then publishes them, after one flush when many came, and says which it stored.
    fn copy_all(
        &self,
        cache: &Cache,
        asked: &[&Wanted],
        context: &Context,
    ) -> io::Result<Vec<crate::cache::Stored>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let batch = if asked.len() > crate::cache::MOST_FLUSHED_ALONE {
            cache.batch()?
        } else {
            None
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let flush = batch.is_none();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let flush = true;
        let placer = Placer::new(&self.folder);
        let copied = crate::place::run_all(asked, context.threads.at_most(COPIES), |want| {
            self.copy(cache, &placer, want, flush, &context.stop)
        });
        let mut sealed = Vec::new();
        for copied in copied {
            sealed.extend(copied?);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(batch) = batch {
            batch.flush()?;
        }
        sealed
            .into_iter()
            .map(|object| object.publish(cache))
            .collect()
    }

    /// the object `want` sealed from the first of its paths whose bytes are the pinned ones.
    fn copy(
        &self,
        cache: &Cache,
        placer: &Placer,
        want: &Wanted,
        flush: bool,
        stop: &Stop,
    ) -> io::Result<Option<Sealed>> {
        for (place, path) in self.places(want) {
            let Some(now) = readable(placer, &path) else {
                continue;
            };
            if said(place.record, now, self.before) == Said::Other {
                continue;
            }
            match cache.copy_in(&path, &want.digest, want.size, flush, stop)? {
                Copied::Sealed(object) => return Ok(Some(object)),
                Copied::OtherSize | Copied::OtherBytes | Copied::NotPlain => {
                    tracing::debug!(path = %path.display(), "holds other bytes than data.lock pins");
                }
                Copied::Unread(err) => {
                    tracing::debug!(path = %path.display(), %err, "could not be read");
                }
            }
        }
        Ok(None)
    }

    /// the places that may hold `want`, in the order of the plan, each with its path.
    fn places<'a>(&'a self, want: &Wanted) -> impl Iterator<Item = (&'a Place, PathBuf)> + 'a {
        self.paths
            .get(&want.digest)
            .into_iter()
            .flatten()
            .map(|place| (place, self.root.join(&place.path)))
    }
}

/// how the plain file at `path` looks, when every folder from `placer`'s root to it is a real one,
/// since a link on the way may point anywhere and fetchloom never places through one.
fn readable(placer: &Placer, path: &Path) -> Option<Look> {
    placer.check_parent(path).ok()?;
    std::fs::symlink_metadata(path)
        .ok()
        .filter(is_plain)
        .map(|found| Look::of(&found))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::output::{Level, Style};
    use crate::project::lock::{Locked, LockedFile};
    use crate::project::state::{Placed, State, nanos};

    const PINNED: &[u8] = b"station,temperature\nlerwick,9.1\n";
    /// [`PINNED`] with one byte changed, as an edit that keeps the size leaves it.
    const EDITED: &[u8] = b"station,temperature\nlerwick,9.2\n";

    fn context() -> Context {
        Context::new(Output::new(
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
        ))
    }

    /// a project whose data folder is `inputs`, with a cache beside it.
    fn project() -> (tempfile::TempDir, Project, Manifest, Cache) {
        let folder = tempfile::tempdir().unwrap();
        let project = Project {
            root: folder.path().join("project"),
        };
        fs::create_dir_all(project.root.join("inputs").join("weather")).unwrap();
        let manifest = Manifest::parse("[project]\ndata_dir = \"inputs\"\n").unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        (folder, project, manifest, cache)
    }

    fn wanted() -> Wanted {
        Wanted {
            digest: crate::digest::sha256(PINNED),
            size: PINNED.len() as u64,
            label: "weather/weather.csv".to_owned(),
        }
    }

    /// writes `bytes` at `path` below the project, read-only as sync places a file, and says how
    /// it looks.
    fn placed(project: &Project, path: &str, bytes: &[u8]) -> Look {
        let file = project.root.join(path);
        fs::write(&file, bytes).unwrap();
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();
        Look::of(&fs::metadata(&file).unwrap())
    }

    fn found(project: &Project, manifest: &Manifest, paths: &[&str]) -> DataFolder {
        let digest = crate::digest::sha256(PINNED);
        let at = paths.iter().map(|path| (digest, (*path).to_owned()));
        DataFolder::found(project, manifest, at.collect())
    }

    /// the cache gets a clone or copy, so the file stays put and its edits never reach the cache.
    #[test]
    fn a_file_holding_its_pin_comes_into_the_cache_as_a_copy_of_its_own() {
        let (_folder, project, manifest, cache) = project();
        let path = "inputs/weather/weather.csv";
        let before = placed(&project, path, PINNED);
        let context = context();
        let data = found(&project, &manifest, &[path]);
        let rest = data.take(&context, &cache, vec![wanted()]).unwrap();
        assert_eq!(rest, Vec::<Wanted>::new());
        let object = cache.find(&wanted().digest, Some(wanted().size)).unwrap();
        assert_eq!(fs::read(&object).unwrap(), PINNED);
        let file = project.root.join(path);
        assert_eq!(fs::read(&file).unwrap(), PINNED);
        assert_eq!(Look::of(&fs::metadata(&file).unwrap()), before);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let (file, object) = (fs::metadata(&file).unwrap(), fs::metadata(&object).unwrap());
            assert_eq!(file.nlink(), 1);
            assert_ne!(file.ino(), object.ino());
        }
        let counts = context.work.counts();
        assert_eq!(
            (counts.data_folder_objects, counts.data_folder_bytes),
            (1, PINNED.len() as u64)
        );
        assert_eq!(counts.objects_written, 1);
    }

    /// with no path left to try, the object stays wanted.
    #[test]
    fn an_edit_is_left_and_the_next_path_tried() {
        let (_folder, project, manifest, cache) = project();
        let (edited, intact) = ("inputs/weather/weather.csv", "inputs/weather/copy.csv");
        let before = placed(&project, edited, EDITED);
        let context = context();
        let alone = found(&project, &manifest, &[edited]);
        assert_eq!(
            alone.take(&context, &cache, vec![wanted()]).unwrap(),
            vec![wanted()]
        );
        assert!(cache.find(&wanted().digest, None).is_none());
        assert!(cache.find(&crate::digest::sha256(EDITED), None).is_none());
        placed(&project, intact, PINNED);
        let both = found(&project, &manifest, &[edited, intact]);
        assert_eq!(
            both.take(&context, &cache, vec![wanted()]).unwrap(),
            Vec::<Wanted>::new()
        );
        assert!(cache.find(&wanted().digest, Some(wanted().size)).is_some());
        let file = project.root.join(edited);
        assert_eq!(fs::read(&file).unwrap(), EDITED);
        assert_eq!(Look::of(&fs::metadata(&file).unwrap()), before);
        assert_eq!(context.work.counts().data_folder_objects, 1);
    }

    #[test]
    fn a_file_the_state_records_with_other_bytes_is_read_only_once_it_changed() {
        let (_folder, project, manifest, cache) = project();
        let path = "inputs/weather/weather.csv";
        let look = placed(&project, path, PINNED);
        let place = |look| Place {
            path: path.to_owned(),
            record: Some(Record {
                pinned: false,
                look,
            }),
        };
        let data = |look| {
            let places = std::iter::once((wanted().digest, place(look)));
            DataFolder {
                before: u64::MAX,
                ..DataFolder::of(&project, &manifest, places)
            }
        };
        let context = context();
        let recorded = data(look);
        assert_eq!(
            recorded.take(&context, &cache, vec![wanted()]).unwrap(),
            vec![wanted()]
        );
        let changed = data(Look {
            modified: look.modified.wrapping_add(1),
            ..look
        });
        assert_eq!(
            changed.take(&context, &cache, vec![wanted()]).unwrap(),
            Vec::<Wanted>::new()
        );
    }

    /// a link on a file's way may lead out of the project, and fetchloom never places through one.
    #[cfg(unix)]
    #[test]
    fn a_link_on_the_way_is_not_followed() {
        let (folder, project, manifest, cache) = project();
        let elsewhere = folder.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("weather.csv"), PINNED).unwrap();
        let linked = project.root.join("inputs").join("linked");
        std::os::unix::fs::symlink(&elsewhere, &linked).unwrap();
        let data = found(&project, &manifest, &["inputs/linked/weather.csv"]);
        let context = context();
        assert_eq!(data.holds(&context, &[wanted()]), [false]);
        assert_eq!(
            data.take(&context, &cache, vec![wanted()]).unwrap(),
            vec![wanted()]
        );
        assert!(cache.find(&wanted().digest, None).is_none());
    }

    /// a file of the dataset `weather`: whether it holds an edit of its pinned bytes, when it was
    /// last written, what the state records of it, the pinned bytes or others and how much
    /// earlier than the file's time, and whether a dry run counts it.
    struct Case {
        name: &'static str,
        edited: bool,
        modified: SystemTime,
        record: Option<(bool, Duration)>,
        counts: bool,
    }

    /// the pinned bytes of `name`, or an edit of them of the same size.
    fn bytes(name: &str, edited: bool) -> Vec<u8> {
        format!("{name},9.{}\n", if edited { 2 } else { 1 }).into_bytes()
    }

    /// data.lock pinning `files` of the dataset `weather`.
    fn weather(files: Vec<LockedFile>) -> Lock {
        Lock {
            datasets: vec![Locked {
                name: "weather".to_owned(),
                reference: "https://example.org/weather".to_owned(),
                spec: "sha256:00".to_owned(),
                extract: true,
                resolved: "https://example.org/weather".to_owned(),
                title: None,
                license: None,
                doi: None,
                access: crate::source::Access::Public,
                timestamp: "2026-11-11T00:00:00Z".to_owned(),
                files,
            }],
            derived: Vec::new(),
        }
    }

    /// the state vouches when it records the pinned bytes at the path and the file looks as
    /// recorded, from before the state was written. any other file must hash to its pin.
    #[test]
    fn a_file_counts_unread_only_where_the_state_vouches_for_it() {
        let (_folder, project, _, cache) = project();
        let manifest = Manifest::parse(
            "[project]\ndata_dir = \"inputs\"\n\n[datasets.weather]\nref = \"https://example.org/weather\"\n",
        )
        .unwrap();
        let written = SystemTime::UNIX_EPOCH + Duration::from_hours(500_000);
        let before = written - Duration::from_secs(10);
        let (now, earlier) = (Duration::ZERO, Duration::from_secs(5));
        let case = |name, edited, modified, record, counts| Case {
            name,
            edited,
            modified,
            record,
            counts,
        };
        let cases = [
            // the state vouches, so its record stands, unread, though the file holds an edit
            case("vouched", true, before, Some((true, now)), true),
            case("moved", true, before, Some((true, earlier)), false),
            case("unrecorded", true, before, None, false),
            case("intact", false, before, None, true),
            // as new as the state, so the same tick of a coarse clock could hide an edit
            case("racy", true, written, Some((true, now)), false),
            case("other", false, before, Some((false, now)), false),
        ];
        let (mut files, mut state) = (Vec::new(), State::default());
        for case in &cases {
            let path = format!("inputs/weather/{}.csv", case.name);
            let file = project.root.join(&path);
            fs::write(&file, bytes(case.name, case.edited)).unwrap();
            let opened = fs::File::options().write(true).open(&file).unwrap();
            opened.set_modified(case.modified).unwrap();
            let look = Look::of(&fs::metadata(&file).unwrap());
            let pin = bytes(case.name, false);
            if let Some((pinned, shift)) = case.record {
                let recorded = if pinned {
                    pin.clone()
                } else {
                    b"other\n".to_vec()
                };
                let placed = Placed {
                    dataset: "weather".to_owned(),
                    sha256: crate::digest::sha256(&recorded),
                    method: crate::place::Method::Copy,
                    look: Look {
                        modified: nanos(case.modified - shift),
                        ..look
                    },
                    writable: false,
                    checked: 0,
                    adopted: false,
                    racy_intact: std::sync::OnceLock::new(),
                };
                state.files.insert(path, placed);
            }
            files.push(LockedFile {
                path: format!("{}.csv", case.name),
                size: pin.len() as u64,
                sha256: crate::digest::sha256(&pin),
                md5: None,
                url: format!("https://example.org/weather/{}.csv", case.name),
                unpacks_to: None,
            });
        }
        project.write_state(&state).unwrap();
        let opened = fs::File::options()
            .write(true)
            .open(project.own_folder().join("state"));
        opened.unwrap().set_modified(written).unwrap();
        let lock = weather(files);
        let wanted: Vec<Wanted> = lock.datasets[0]
            .files
            .iter()
            .map(|file| Wanted {
                digest: file.sha256,
                size: file.size,
                label: file.path.clone(),
            })
            .collect();
        let counted =
            DataFolder::planned(&project, &manifest, &lock, &cache).holds(&context(), &wanted);
        let found: Vec<(&str, bool)> = cases.iter().map(|case| case.name).zip(counted).collect();
        let expected: Vec<(&str, bool)> =
            cases.iter().map(|case| (case.name, case.counts)).collect();
        assert_eq!(found, expected);
    }
}
