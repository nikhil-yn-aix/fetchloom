//! pinned files the project's data folder holds, taken into the cache when it lacks them, before
//! any mirror or source is asked. a project copied with its data folder to a machine whose cache
//! never held its files then pushes, builds and places them with no download. a file there is
//! only read: the cache gets a clone or a copy, kept only when it hashes to the pin, so an edit is
//! never taken, and the file is never changed, moved or linked, since a link would let a later
//! edit of it change the cache's copy.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::cache::{Cache, Copied, Sealed};
use crate::cli::Context;
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
}

/// a path below the project folder, with `/` between folders, that may hold an object.
struct Place {
    path: String,
    /// how the file looked when the project's state recorded other bytes there: while it still
    /// looks so, it holds those, and is not read.
    other: Option<Look>,
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
            .map(|(digest, path)| (digest, Place { path, other: None }));
        DataFolder::of(project, manifest, found)
    }

    /// every file data.lock places, laid out as `lock` plans it with what `cache` records its
    /// archives unpacked to. a path the project's state records holding other bytes is read only
    /// once its file no longer looks as recorded. a data folder that is a link, or a lock that
    /// does not plan, gives none.
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
        let (state, _) = project.state();
        let places = planned.targets.into_iter().map(|target| {
            let other = state
                .files
                .get(&target.path)
                .filter(|placed| placed.sha256 != target.sha256)
                .map(|placed| placed.look);
            let path = target.path;
            (target.sha256, Place { path, other })
        });
        DataFolder::of(project, manifest, places)
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
        }
    }

    /// whether no file here may hold any object, so there is nothing to look for.
    pub(in crate::cli) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// which of `wanted` a file here looks to hold, after one look at each path's name: a plain
    /// file of the pinned size in real folders, that the state does not record with other bytes.
    /// only [`DataFolder::take`] reads the bytes.
    pub(in crate::cli) fn holds(&self, context: &Context, wanted: &[Wanted]) -> Vec<bool> {
        if self.is_empty() {
            return vec![false; wanted.len()];
        }
        let placer = Placer::new(&self.folder);
        crate::place::run_all(wanted, context.threads.at_most(COPIES), |want| {
            self.places(want)
                .any(|path| readable(&placer, &path).is_some_and(|look| look.size == want.size))
        })
    }

    /// takes into the cache each of `wanted` a file here holds, from the first path whose bytes
    /// hash to the pin, and returns the rest. a file that holds other bytes, or cannot be read,
    /// is left as it is and the next path tried. says once how many files came in.
    ///
    /// # Errors
    ///
    /// a failure to write the cache, and a stop.
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
        for path in self.places(want) {
            // a link on the way may point anywhere, and fetchloom never places through one
            if placer.check_parent(&path).is_err() {
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

    /// the paths that may hold `want`, those the state records with other bytes and that still
    /// look so left out.
    fn places<'a>(&'a self, want: &Wanted) -> impl Iterator<Item = PathBuf> + 'a {
        self.paths
            .get(&want.digest)
            .into_iter()
            .flatten()
            .filter(|place| {
                place
                    .other
                    .is_none_or(|other| look(&self.root.join(&place.path)) != Some(other))
            })
            .map(|place| self.root.join(&place.path))
    }
}

/// how the plain file at `path` looks, when every folder between the data folder `placer` is
/// rooted at and it is a real one.
fn readable(placer: &Placer, path: &Path) -> Option<Look> {
    placer.check_parent(path).ok()?;
    std::fs::symlink_metadata(path)
        .ok()
        .filter(is_plain)
        .map(|found| Look::of(&found))
}

/// how the file at `path` looks, when there is one.
fn look(path: &Path) -> Option<Look> {
    std::fs::symlink_metadata(path)
        .ok()
        .map(|found| Look::of(&found))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::output::{Level, Style};

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

    /// the pinned bytes come into the cache as an object of their own, a clone or a copy, so the
    /// file stays as it was and an edit of it later never reaches the cache.
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

    /// a file of the pinned size with other bytes is left as it is, and the next path data.lock
    /// places the object at is tried; with none left, the object stays wanted.
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

    /// a path the state records with other bytes holds those while it still looks as recorded,
    /// so it is not read; once it changed, it is.
    #[test]
    fn a_file_the_state_records_with_other_bytes_is_read_only_once_it_changed() {
        let (_folder, project, manifest, cache) = project();
        let path = "inputs/weather/weather.csv";
        let look = placed(&project, path, PINNED);
        let place = |other| Place {
            path: path.to_owned(),
            other: Some(other),
        };
        let data = |other| {
            let places = std::iter::once((wanted().digest, place(other)));
            DataFolder::of(&project, &manifest, places)
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

    /// a link among the folders on a file's way may lead out of the project, and fetchloom never
    /// places through one, so nothing is read through it.
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

    /// one look says a file of the pinned size is there, as a dry run asks; only taking it reads
    /// its bytes.
    #[test]
    fn holding_is_told_by_size_alone() {
        let (_folder, project, manifest, _cache) = project();
        let (edited, short) = ("inputs/weather/weather.csv", "inputs/weather/short.csv");
        placed(&project, edited, EDITED);
        placed(&project, short, b"station\n");
        let context = context();
        let held = |paths: &[&str]| found(&project, &manifest, paths).holds(&context, &[wanted()]);
        assert_eq!(held(&[edited]), [true]);
        assert_eq!(held(&[short]), [false]);
        assert_eq!(held(&["inputs/weather/gone.csv"]), [false]);
    }
}
