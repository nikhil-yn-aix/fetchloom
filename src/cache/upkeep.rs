//! keeping the cache tidy: which projects use it, how big it is, freeing what no project needs,
//! and hashing every object again.

use std::cell::Cell;
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::{Cache, Failure};
use crate::digest::{self, Sha256};
use crate::error::Error;
use crate::stop::Stop;
use crate::threads::Threads;

/// a fan folder opened for removals, which only unix opens.
#[cfg(unix)]
type Opened = crate::place::Opened;
#[cfg(not(unix))]
type Opened = std::convert::Infallible;

/// the handle on a fan folder a sweep removes objects through, opened at its first removal.
#[cfg_attr(not(unix), expect(dead_code, reason = "only unix opens a fan folder"))]
enum FanFolder {
    Unopened,
    Open(Opened),
    /// it would not open, so each removal goes the way [`Cache::remove_inside`] goes.
    Refused,
}

/// how long an object no project needs stays, so a download a project has not locked yet, or a
/// `get` from this morning, survives a gc.
pub(crate) const GRACE: Duration = Duration::from_hours(24);

pub(crate) struct Object {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) path: PathBuf,
    /// when the object last changed, which gc ages it from: its modification time, or on unix its
    /// ctime when later, which a seal, publishing or a link moves, as a seal dates the modification
    /// time back to when the download began. windows keeps no ctime, so there a seal moves the
    /// birth up to the seal instead.
    pub(crate) changed: SystemTime,
    /// how many names its file has, more than one while a hardlink, such as a project's, shares
    /// it. none where a folder's listing does not tell, as on windows.
    pub(crate) names: Option<u64>,
    /// the bytes its file takes on the disk, in whole blocks, where the system says.
    pub(crate) disk: Option<u64>,
}

impl Object {
    /// whether a name outside the cache, as a project's hardlink, keeps the object's bytes, so
    /// removing it frees nothing. windows listings count no names, so there the object is opened.
    pub(crate) fn has_other_names(&self) -> bool {
        self.names.map_or_else(
            || crate::place::has_other_names(&self.path),
            |names| names > 1,
        )
    }
}

#[derive(Default)]
pub(crate) struct Scan {
    pub(crate) objects: Vec<Object>,
    pub(crate) unreadable: Vec<Failure>,
    /// the objects every user of this machine may write, though fetchloom keeps each read-only,
    /// so others can change the files projects placed from them.
    pub(crate) exposed: Vec<PathBuf>,
}

/// what the projects using a cache need from it.
#[derive(Debug, Default)]
pub(crate) struct Needed {
    /// objects a project places, and archives a project has not unpacked on this machine yet.
    pub(crate) objects: HashSet<Sha256>,
    /// archives whose record tells what a project places from them.
    pub(crate) unpacked: HashSet<Sha256>,
    /// the bytes of those archives, each part of a split one, whose own objects may be gone: the
    /// records of what their links served stay, so an upgrade can ask whether they changed.
    pub(crate) archives: HashSet<Sha256>,
}

/// the downloads a run stopped before they finished, whose bytes the cache keeps so a later run
/// can continue them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Unfinished {
    pub(crate) downloads: u64,
    /// what their files take on the disk.
    pub(crate) bytes: u64,
    /// of them, those with the record of their bytes beside them, which a run may continue: one
    /// stopped before its first record, or a small one, which keeps none, starts again.
    pub(crate) recorded: u64,
}

/// what a sweep removed, or a dry run would remove, and what it kept.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Freed {
    pub(crate) removed_objects: u64,
    /// the payload bytes of the objects removed.
    pub(crate) removed_bytes: u64,
    /// unfinished downloads and temporary files nobody came back for, removed.
    pub(crate) removed_leftovers: u64,
    /// what those leftovers took on the disk.
    pub(crate) removed_leftover_bytes: u64,
    pub(crate) not_removed: Vec<Failure>,
    /// objects no project needs that stay because they are younger than a day.
    pub(crate) kept_objects: u64,
    pub(crate) kept_bytes: u64,
    /// projects whose registration went because their data.toml is gone, as from a folder that
    /// moved, so what only they needed was freed too.
    pub(crate) forgotten: Vec<PathBuf>,
}

/// what freeing the objects of a fan folder asks: a sweep's `needed`, `unused` and `stop`, and the
/// most threads it may take.
struct Freeing<'f> {
    needed: &'f Needed,
    unused: &'f (dyn Fn(SystemTime) -> bool + Sync),
    stop: &'f Stop,
    threads: Threads,
}

/// what freeing the objects of one fan folder did, as [`Freed`] counts it.
#[derive(Default)]
struct FanFreed {
    removed_objects: u64,
    removed_bytes: u64,
    kept_objects: u64,
    kept_bytes: u64,
    not_removed: Vec<Failure>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Checked {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// objects whose bytes no longer matched their name.
    pub(crate) changed: Vec<Sha256>,
    /// changed objects whose repair marks could not be written.
    pub(crate) stuck: Vec<Sha256>,
    /// objects or folders that could not be read, left untouched without a new change mark.
    pub(crate) unreadable: Vec<Failure>,
}

#[derive(Serialize, Deserialize)]
struct Registration {
    root: PathBuf,
    /// the disk the project registered from, by the number it keeps while connected, which tells a
    /// project on a drive not connected now from one deleted: none where the number may change
    /// with each mount, as a network or memory disk's does, or a registration made before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disk: Option<u64>,
}

/// the number of the disk `path` is on, where it stays the disk's while it is connected: a device
/// number of its own on unix, which a network or memory disk does not have, and the volume's serial
/// number on windows.
fn disk_of(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let device = fs::metadata(path).ok()?.dev();
        // macos keeps a device number in an i32
        #[cfg(target_os = "macos")]
        let major = rustix::fs::major(i32::try_from(device).ok()?);
        #[cfg(not(target_os = "macos"))]
        let major = rustix::fs::major(device);
        (major != 0).then_some(device)
    }
    #[cfg(windows)]
    {
        let handle = winapi_util::Handle::from_path_any(path).ok()?;
        let information = winapi_util::file::information(&handle).ok()?;
        Some(information.volume_serial_number())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// the most bytes a project's registration may hold: its folder's path, as json.
const REGISTRATION_MOST: u64 = 64 * 1024;

/// the bytes of the registration at `path`, read without following a link or waiting on a fifo,
/// as a plain file of [`REGISTRATION_MOST`] bytes at most: anyone who can write a shared cache can
/// put anything among them, such as a fifo or a link to /dev/zero.
fn read_registration(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    crate::volume::read_unfollowed(path)?
        .take(REGISTRATION_MOST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > REGISTRATION_MOST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is larger than any record of a project", path.display()),
        ));
    }
    Ok(bytes)
}

impl Cache {
    /// remembers that the project at `root` uses this cache, so gc keeps what it needs. a
    /// registration turns read-only once its folder is flushed, so an unchanged read-only one costs
    /// nothing, and a writable one, whose flush may have failed, has its folder flushed again.
    pub(crate) fn register(&self, root: &Path) -> io::Result<()> {
        let path = self.registration(root);
        let text = serde_json::to_vec(&Registration {
            root: root.to_path_buf(),
            disk: disk_of(root),
        })
        .map_err(io::Error::other)?;
        match fs::symlink_metadata(&path) {
            Ok(found)
                if crate::volume::is_plain(&found)
                    && read_registration(&path).is_ok_and(|bytes| bytes == text) =>
            {
                if found.permissions().readonly() {
                    return Ok(());
                }
                super::sync_publication(&path, &path, crate::volume::sync_directory)?;
            }
            Ok(found) => {
                // windows renames nothing over a read-only file
                if crate::volume::is_plain(&found) {
                    crate::volume::set_read_only(&path, false)?;
                }
                self.write_atomically(&path, &text)?;
            }
            Err(_) => self.write_atomically(&path, &text)?,
        }
        crate::volume::set_read_only(&path, true)
    }

    /// how many unfinished downloads the partial folder keeps, and what they take on the disk. a
    /// folder that cannot be read holds none.
    pub(crate) fn unfinished(&self) -> Unfinished {
        self.unfinished_where(|_| true)
    }

    /// [`Cache::unfinished`] for the downloads no run holds, which `cache gc --now` frees: a run
    /// working on one, this one too, finishes it or deletes it itself. a lock is opened as the
    /// sweep opens it, never through a link nor waiting on a fifo someone put in its place.
    pub(crate) fn unfinished_unheld(&self) -> Unfinished {
        self.unfinished_where(|claim| {
            crate::volume::open_unfollowed(claim, false)
                .ok()
                .and_then(|lock| super::partial::unheld(claim, lock))
                .is_some()
        })
    }

    /// the unfinished downloads whose claim file, beside each, `counted` takes.
    fn unfinished_where(&self, counted: impl Fn(&Path) -> bool) -> Unfinished {
        let mut unfinished = Unfinished::default();
        for folder in self.partial_folders() {
            let Ok(entries) = fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if let Some(key) = name.to_str().and_then(|name| name.strip_suffix(".part"))
                    && let Ok(metadata) = entry.metadata()
                    && metadata.is_file()
                    && counted(&folder.join(format!("{key}.lock")))
                {
                    unfinished.downloads += 1;
                    unfinished.bytes += on_disk(&metadata);
                    let record = fs::symlink_metadata(folder.join(format!("{key}.json")));
                    unfinished.recorded += u64::from(record.is_ok_and(|found| found.is_file()));
                }
            }
        }
        unfinished
    }

    /// reads every registration before gc can discard anything it might protect.
    pub(crate) fn registered_projects(&self, stop: &Stop) -> Result<Vec<PathBuf>, Error> {
        let folder = self.root.join("projects");
        stop.check()?;
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(unreadable(&folder, &err)),
        };
        let mut roots = Vec::new();
        for entry in entries {
            stop.check()?;
            let path = entry.map_err(|err| unreadable(&folder, &err))?.path();
            let bytes = read_registration(&path).map_err(|err| {
                let foreign = matches!(
                    err.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
                );
                let error = unreadable(&path, &err);
                if foreign {
                    error.help(
                        "fetchloom writes each project's record there as a small plain file, so this one is not its own: remove it; a sync in a project writes that project's record again",
                    )
                } else {
                    error
                }
            })?;
            let registration: Registration = serde_json::from_slice(&bytes).map_err(|err| {
                unreadable(&path, &io::Error::new(io::ErrorKind::InvalidData, err))
            })?;
            roots.push(registration.root);
        }
        roots.sort();
        Ok(roots)
    }

    /// whether the project registered at `root`, gone from it, is out of reach rather than
    /// deleted: the folder nearest `root` that is there is on another disk than the one the
    /// project registered from, or on none, as a drive not connected leaves it. false for a
    /// project deleted from a disk that is there, and for one whose registration names no disk.
    pub(crate) fn out_of_reach(&self, root: &Path) -> bool {
        let Some(disk) = read_registration(&self.registration(root))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Registration>(&bytes).ok())
            .and_then(|registered| registered.disk)
        else {
            return false;
        };
        root.ancestors()
            .find(|folder| fs::metadata(folder).is_ok())
            .is_none_or(|nearest| disk_of(nearest) != Some(disk))
    }

    /// removes `path`, refusing when a folder between it and the cache root was swapped for a
    /// link or a file since the cache opened, so a sweep never deletes outside the cache.
    pub(super) fn remove_inside(&self, path: &Path) -> io::Result<()> {
        self.check_inside(path)?;
        self.volume.remove_with(path, &|| self.unlink_inside(path))
    }

    /// removes the file at `path` through its folder, reached from the cache's root without
    /// following a link, so a folder swapped for one since [`Cache::check_inside`] sends it
    /// nowhere else: on windows through the cache's placer, which holds the folders on the way.
    pub(super) fn unlink_inside(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            crate::place::remove_below(&self.root, path)
        }
        #[cfg(not(unix))]
        {
            self.remover.placer(&self.root).delete_file(path)
        }
    }

    /// refuses `path` when a folder between it and the cache root is not a real folder.
    pub(super) fn check_inside(&self, path: &Path) -> io::Result<()> {
        let mut folder = path.parent();
        while let Some(current) =
            folder.filter(|folder| *folder != self.root && folder.starts_with(&self.root))
        {
            let metadata = fs::symlink_metadata(current)?;
            crate::place::found_folder(current, &metadata, crate::place::FolderUse::Cache)?;
            folder = current.parent();
        }
        Ok(())
    }

    /// removes the object `digest`, which a drop found elsewhere, and says whether it went,
    /// recording a refusal, as of a folder swapped for a link, in `not_removed`.
    pub(crate) fn free_input(&self, digest: &Sha256, not_removed: &mut Vec<Failure>) -> bool {
        let path = self.object_path(digest);
        removed(&path, self.remove_inside(&path), not_removed)
    }

    /// checks an archive member without letting a large writable object delay cancellation.
    pub(crate) fn contains_for_gc(
        &self,
        digest: &Sha256,
        size: u64,
        stop: &Stop,
    ) -> Result<bool, Error> {
        stop.check()?;
        let path = self.object_path(digest);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return Ok(false);
        };
        if !crate::volume::is_plain(&metadata) || metadata.len() != size {
            return Ok(false);
        }
        let intact = !super::changed::doubted(&path, &metadata, self.changed_at(digest)?)
            || hash(
                &path,
                digest::Block::sized(size.saturating_add(1)),
                stop,
                &|_| {},
            )
            .is_ok_and(|actual| actual == *digest);
        stop.check()?;
        Ok(intact)
    }

    /// every object in the cache, its fan folders read on up to `threads` workers, in the order
    /// a serial walk would give.
    pub(crate) fn scan_objects(&self, threads: Threads, stop: &Stop) -> Result<Scan, Error> {
        let mut scan = Scan::default();
        let folder = self.root.join("objects");
        // a file the system leaves behind, such as finder's .DS_Store, is no fan folder. a fan's
        // own name that is not a folder is damage, and is reported as unreadable
        let mut fans: Vec<(String, PathBuf)> = scan_entries(&folder, &mut scan.unreadable, stop)?
            .into_iter()
            .filter_map(|fan| {
                let prefix = fan.file_name().to_string_lossy().into_owned();
                (prefix.len() == 2 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| (prefix, fan.path()))
            })
            .collect();
        // a fan someone made a link is never listed, which would count, verify and remove the
        // files it leads to as if they were the cache's
        fans.retain(|(_, fan)| {
            let real = fs::symlink_metadata(fan).and_then(|found| {
                crate::place::found_folder(fan, &found, crate::place::FolderUse::Cache)
            });
            match real {
                Ok(()) => true,
                Err(err) if err.kind() == io::ErrorKind::NotFound => false,
                Err(err) => {
                    scan.unreadable.push(Failure::new(fan, &err));
                    false
                }
            }
        });
        let read = crate::place::run_coarse(&fans, threads, FANS_PER_WORKER, |(prefix, fan)| {
            scan_fan(prefix, fan, stop)
        });
        let read = read.into_iter().collect::<Result<Vec<_>, Error>>()?;
        scan.objects
            .reserve_exact(read.iter().map(|fan| fan.found.len()).sum());
        // each path is made here rather than on the worker, whose memory would stay spread over
        // threads that are gone
        for ((_, path), fan) in fans.iter().zip(read) {
            let mut paths = fan.paths.into_iter().peekable();
            for (at, found) in fan.found.into_iter().enumerate() {
                let path = if let Some((_, path)) = paths.next_if(|(place, _)| *place == at) {
                    path
                } else {
                    let hex = found.digest.hex();
                    let mut object = PathBuf::with_capacity(path.as_os_str().len() + 64);
                    object.push(path);
                    object.push(&hex[2..]);
                    object
                };
                if found.exposed {
                    scan.exposed.push(path.clone());
                }
                scan.objects.push(Object {
                    digest: found.digest,
                    size: found.size,
                    path,
                    changed: found.changed,
                    names: found.names,
                    disk: found.disk,
                });
            }
            scan.unreadable.extend(fan.unreadable);
        }
        super::failure::sort(&mut scan.unreadable);
        Ok(scan)
    }

    /// removes stale registrations and unneeded objects older than `grace`, and downloads no run
    /// holds, which keep a day unless `grace` is zero, recording each refused removal. a download
    /// in progress keeps its bytes, as its run holds its lock.
    pub(crate) fn sweep_with_stop(
        &self,
        needed: &Needed,
        stale_projects: &[PathBuf],
        now: SystemTime,
        grace: Duration,
        threads: Threads,
        stop: &Stop,
    ) -> Result<Freed, Error> {
        let old = |modified: SystemTime| now.duration_since(modified).is_ok_and(|age| age > GRACE);
        let unused = |modified: SystemTime| {
            // FAT rounds a new file's time up, into the future, so no age is asked of `--now`
            grace.is_zero() || now.duration_since(modified).is_ok_and(|age| age >= grace)
        };
        let scan = self.scan_objects(threads, stop)?;
        if let Some(failure) = scan.unreadable.first() {
            return Err(failure.error("cannot inspect; the cache stays unchanged:"));
        }
        let mut freed = Freed::default();
        for root in stale_projects {
            stop.check()?;
            let path = self.registration(root);
            if removed(&path, self.remove_inside(&path), &mut freed.not_removed) {
                freed.forgotten.push(root.clone());
            }
        }
        let freeing = Freeing {
            needed,
            unused: &unused,
            stop,
            threads,
        };
        let objects = scan.objects;
        self.free_objects(&objects, &freeing, &mut freed)?;
        // freed before the folders below are read, which make lists of their own
        drop(objects);
        for entry in scan_entries(&self.root.join("unpacked"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let Some(archive) = Sha256::from_hex(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let path = entry.path();
            let stale = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(unused);
            if !needed.unpacked.contains(&archive) && stale {
                removed(&path, self.remove_inside(&path), &mut freed.not_removed);
            }
        }
        let abandoned = |modified: SystemTime| grace.is_zero() || old(modified);
        self.sweep_partials(&abandoned, stop, &mut freed)?;
        freed.removed_leftovers += self.sweep_staging(now, stop, &mut freed.not_removed);
        stop.check()?;
        self.forget_changed_reporting(&needed.objects, stop, &mut freed.not_removed)?;
        self.forget_keyed_reporting(stop, &mut freed.not_removed)?;
        self.forget_url_records(needed, stop, &mut freed.not_removed)?;
        for entry in scan_entries(&self.root.join("tmp"), &mut freed.not_removed, stop)? {
            stop.check()?;
            let stale = entry
                .metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.modified().is_ok_and(old));
            let size = entry.metadata().map_or(0, |metadata| on_disk(&metadata));
            if stale
                && removed(
                    &entry.path(),
                    self.remove_inside(&entry.path()),
                    &mut freed.not_removed,
                )
            {
                freed.removed_leftovers += 1;
                freed.removed_leftover_bytes += size;
            }
        }
        stop.check()?;
        super::failure::sort(&mut freed.not_removed);
        Ok(freed)
    }

    /// frees the objects no project needs that `freeing` ages out, a fan folder at a time and in
    /// the scan's order, through one handle on each fan and on several threads. the scan lists
    /// `objects` fan by fan, so each fan is a run of them, freed where it lies.
    fn free_objects(
        &self,
        objects: &[Object],
        freeing: &Freeing<'_>,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        let fans: Vec<&[Object]> = objects
            .chunk_by(|one, next| one.path.parent() == next.path.parent())
            .collect();
        let removable = objects.iter().any(|object| {
            !freeing.needed.objects.contains(&object.digest) && (freeing.unused)(object.changed)
        });
        let root = self.fan_root(removable);
        let job = |fan: &&[Object]| self.free_fan(fan, freeing, root.as_ref());
        // a pretend sweep keeps one thread, so the removals it records keep the scan's order
        let freed_fans = if removable && !self.volume.is_pretending() {
            crate::place::run_coarse(&fans, freeing.threads, FANS_PER_WORKER, job)
        } else {
            fans.iter().map(job).collect()
        };
        for fan in freed_fans {
            let fan = fan?;
            freed.removed_objects += fan.removed_objects;
            freed.removed_bytes += fan.removed_bytes;
            freed.kept_objects += fan.kept_objects;
            freed.kept_bytes += fan.kept_bytes;
            freed.not_removed.extend(fan.not_removed);
        }
        Ok(())
    }

    /// the cache's root opened once for removals through fan handles, when `wanted` and the
    /// cache removes for real; none otherwise, and on systems that open no fan folder.
    #[cfg(unix)]
    fn fan_root(&self, wanted: bool) -> Option<Opened> {
        (wanted && !self.volume.is_pretending())
            .then(|| crate::place::Opened::root(&self.root).ok())
            .flatten()
    }

    #[cfg(not(unix))]
    #[expect(clippy::unused_self, reason = "unix opens the root it holds")]
    fn fan_root(&self, _: bool) -> Option<Opened> {
        None
    }

    /// frees what it may of `fan`, the objects of one fan folder.
    fn free_fan(
        &self,
        fan: &[Object],
        freeing: &Freeing<'_>,
        root: Option<&Opened>,
    ) -> Result<FanFreed, Error> {
        let mut done = FanFreed::default();
        let mut opened = FanFolder::Unopened;
        for object in fan {
            freeing.stop.check()?;
            if freeing.needed.objects.contains(&object.digest) {
                continue;
            }
            if !(freeing.unused)(object.changed) {
                done.kept_objects += 1;
                done.kept_bytes += object.size;
                continue;
            }
            let result = self.remove_object(&object.path, root, &mut opened);
            if removed(&object.path, result, &mut done.not_removed) {
                done.removed_objects += 1;
                done.removed_bytes += object.size;
            }
        }
        Ok(done)
    }

    /// removes the object at `path` through `opened`, the handle on its fan folder opened below
    /// `root` at the fan's first removal, or as [`Cache::remove_inside`] does without a root or
    /// once the fan would not open, which then says why each removal is refused.
    #[cfg(unix)]
    fn remove_object(
        &self,
        path: &Path,
        root: Option<&Opened>,
        opened: &mut FanFolder,
    ) -> io::Result<()> {
        let fan = path
            .parent()
            .and_then(|fan| fan.strip_prefix(&self.root).ok());
        let (Some(root), Some(fan), Some(name)) = (root, fan, path.file_name()) else {
            return self.remove_inside(path);
        };
        if matches!(opened, FanFolder::Unopened) {
            *opened = root.below(fan).map_or(FanFolder::Refused, FanFolder::Open);
        }
        match opened {
            FanFolder::Open(folder) => self.volume.remove_with(path, &|| folder.remove_file(name)),
            FanFolder::Unopened | FanFolder::Refused => self.remove_inside(path),
        }
    }

    #[cfg(not(unix))]
    fn remove_object(&self, path: &Path, _: Option<&Opened>, _: &mut FanFolder) -> io::Result<()> {
        self.remove_inside(path)
    }

    /// drops the record of each link whose object and unpack record are both gone, which would
    /// otherwise only grow, unless it served an archive or a part of one that `needed` keeps, and
    /// the older records of each link; the next download of a link writes a new one. a pretend
    /// sweep lists the shards it would remove, left with no record.
    fn forget_url_records(
        &self,
        needed: &Needed,
        stop: &Stop,
        not_removed: &mut Vec<Failure>,
    ) -> Result<(), Error> {
        // a pretend sweep keeps what it would remove, so their records go as well
        let held = |record: &super::UrlRecord| {
            let kept = |path: &Path| {
                !self.volume.pretends_removed(path) && path.try_exists().is_ok_and(|exists| exists)
            };
            needed.archives.contains(&record.sha256)
                || kept(&self.object_path(&record.sha256))
                || kept(&self.unpacked_path(&record.sha256))
        };
        let scratch = self.root.join("tmp");
        for shard in 0..super::urls::SHARDS {
            stop.check()?;
            let swept = if self.volume.is_pretending() {
                super::urls::Shards::emptied(&self.root, shard, &held).map(|emptied| {
                    for path in emptied {
                        removed(&path, self.remove_inside(&path), not_removed);
                    }
                })
            } else {
                self.urls.rewrite(&self.root, &scratch, shard, &held)
            };
            if let Err(err) = swept {
                not_removed.push(Failure::new(&self.root.join("urls"), &err));
            }
        }
        Ok(())
    }

    /// removes abandoned downloads and the lock files nobody holds, holding each lock; a run that
    /// opened one before it went locks a file with no name, which `claim` notices. a download whose
    /// files could not all be removed keeps its lock, by which the next sweep finds it again.
    fn sweep_partials(
        &self,
        abandoned: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        for folder in self.partial_folders() {
            self.sweep_partial_folder(&folder, abandoned, stop, freed)?;
        }
        Ok(())
    }

    fn sweep_partial_folder(
        &self,
        folder: &Path,
        abandoned: &dyn Fn(SystemTime) -> bool,
        stop: &Stop,
        freed: &mut Freed,
    ) -> Result<(), Error> {
        for entry in scan_entries(folder, &mut freed.not_removed, stop)? {
            stop.check()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".lock") else {
                continue;
            };
            let Some(_held) = crate::volume::open_unfollowed(&entry.path(), false)
                .ok()
                .and_then(|lock| super::partial::unheld(&entry.path(), lock))
            else {
                continue;
            };
            let bytes = folder.join(format!("{key}.part"));
            // aged as an object is: a seal dates a download back to its start
            let left = match fs::symlink_metadata(&bytes) {
                Ok(metadata) => last_changed(&metadata).is_ok_and(abandoned),
                Err(err) => err.kind() == io::ErrorKind::NotFound,
            };
            if !left {
                continue;
            }
            let mut kept = false;
            // the bytes go first, and their record only with them: bytes kept without it, as
            // windows keeps a file another program holds open, could never be continued
            for leftover in [bytes, folder.join(format!("{key}.json"))] {
                stop.check()?;
                if kept {
                    break;
                }
                let size = fs::symlink_metadata(&leftover).map_or(0, |metadata| on_disk(&metadata));
                let removal = self.remove_inside(&leftover);
                kept |= removal
                    .as_ref()
                    .is_err_and(|err| err.kind() != io::ErrorKind::NotFound);
                if removed(&leftover, removal, &mut freed.not_removed) {
                    freed.removed_leftovers += 1;
                    freed.removed_leftover_bytes += size;
                }
            }
            // a sweep finds what is left of a download by its lock, so the lock goes last
            if !kept {
                // an empty lock file frees nothing worth counting
                let _ = self.remove_inside(&entry.path());
            }
        }
        Ok(())
    }

    /// hashes every object on the requested threads, stopping between reads when asked.
    #[cfg(test)]
    fn verify_with_stop(
        &self,
        threads: usize,
        progress: &(dyn Fn(u64) + Sync),
        stop: &Stop,
    ) -> Result<Checked, Error> {
        self.verify_scanned(
            self.scan_objects(Threads::default(), stop)?,
            threads,
            progress,
            stop,
        )
    }

    /// hashes every object `scan` found, so a caller that sized a progress bar from the scan
    /// does not walk the cache twice.
    pub(crate) fn verify_scanned(
        &self,
        scan: Scan,
        threads: usize,
        progress: &(dyn Fn(u64) + Sync),
        stop: &Stop,
    ) -> Result<Checked, Error> {
        let objects = scan.objects;
        let next = AtomicUsize::new(0);
        let checked = Mutex::new(Checked {
            objects: objects.len() as u64,
            bytes: objects.iter().map(|object| object.size).sum(),
            unreadable: scan.unreadable,
            ..Checked::default()
        });
        let path: crate::place::FilePath<'_, Object> =
            &|object| std::borrow::Cow::Borrowed(object.path.as_path());
        let ahead = crate::place::ReadAhead::when_cold(&objects, path);
        let queued = digest::Queued::of(objects.len());
        let verify = || {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if let Some(ahead) = &ahead {
                    ahead.took(index);
                }
                let Some(object) = objects.get(index) else {
                    break;
                };
                queued.begun();
                if stop.is_asked() {
                    break;
                }
                let left = Cell::new(object.size);
                let block = digest::Block::sized(object.size.saturating_add(1));
                let actual = hash(&object.path, block, stop, &|read| {
                    let counted = read.min(left.get());
                    left.set(left.get() - counted);
                    progress(counted);
                });
                if stop.is_asked() {
                    break;
                }
                // an object that ended early or could not be read still fills its share of the bar
                if left.get() > 0 {
                    progress(left.get());
                }
                if stop.is_asked() {
                    break;
                }
                match actual {
                    Ok(actual) if actual == object.digest => continue,
                    Err(err) => {
                        checked
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unreadable
                            .push(Failure::new(&object.path, &err));
                        continue;
                    }
                    Ok(_) => {}
                }
                let marked = self.mark_changed(&object.digest).is_ok();
                let mut found = checked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                found.changed.push(object.digest);
                if !marked {
                    found.stuck.push(object.digest);
                }
            }
        };
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads.max(1))
                .map(|_| crate::threads::scoped(scope, &verify))
                .collect();
            for worker in workers {
                worker.join();
            }
        });
        stop.check()?;
        let mut checked = checked
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        checked.changed.sort();
        checked.stuck.sort();
        super::failure::sort(&mut checked.unreadable);
        Ok(checked)
    }

    pub(crate) fn registration(&self, root: &Path) -> PathBuf {
        let key = digest::sha256(root.as_os_str().as_encoded_bytes()).hex();
        self.root.join("projects").join(format!("{key}.json"))
    }
}

/// the entries of `folder`, none when it does not exist, as in a cache a dry run left unmade.
/// a folder that cannot be read is recorded in `unreadable`.
pub(super) fn scan_entries(
    folder: &Path,
    unreadable: &mut Vec<Failure>,
    stop: &Stop,
) -> Result<Vec<fs::DirEntry>, Error> {
    stop.check()?;
    let mut found = Vec::new();
    match fs::read_dir(folder) {
        Ok(entries) => {
            for entry in entries {
                stop.check()?;
                match entry {
                    Ok(entry) => found.push(entry),
                    Err(err) => unreadable.push(Failure::new(folder, &err)),
                }
            }
        }
        Err(err)
            if err.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(folder)
                    .is_err_and(|err| err.kind() == io::ErrorKind::NotFound) => {}
        Err(err) => unreadable.push(Failure::new(folder, &err)),
    }
    Ok(found)
}

/// fan folders a scan worker is worth, each holding many objects in a large cache.
const FANS_PER_WORKER: usize = 32;

/// one fan folder's objects as a scan worker reads them, without their paths.
struct Fan {
    found: Vec<Found>,
    /// the paths of the objects at these places in `found` whose names are not in lowercase, so
    /// not the ones their digests give.
    paths: Vec<(usize, PathBuf)>,
    unreadable: Vec<Failure>,
}

struct Found {
    digest: Sha256,
    size: u64,
    changed: SystemTime,
    names: Option<u64>,
    disk: Option<u64>,
    /// whether every user may write it.
    exposed: bool,
}

/// the objects in the fan folder `fan`, whose name is `prefix`.
fn scan_fan(prefix: &str, fan: &Path, stop: &Stop) -> Result<Fan, Error> {
    let mut unreadable = Vec::new();
    let entries = scan_entries(fan, &mut unreadable, stop)?;
    let mut found = Vec::with_capacity(entries.len());
    let mut paths = Vec::new();
    for entry in &entries {
        stop.check()?;
        let name = entry.file_name();
        let name = name.as_encoded_bytes();
        if prefix.len() + name.len() != 64 {
            continue;
        }
        let mut hex = [0; 64];
        hex[..2].copy_from_slice(prefix.as_bytes());
        hex[2..].copy_from_slice(name);
        let Some(digest) = std::str::from_utf8(&hex).ok().and_then(Sha256::from_hex) else {
            continue;
        };
        match entry.metadata().and_then(|metadata| {
            let changed = last_changed(&metadata)?;
            // the metadata unix gives counts names, so this takes no further call; what a windows
            // listing gives does not count them
            #[cfg(unix)]
            let (names, disk, exposed) = {
                use std::os::unix::fs::MetadataExt;
                // unix counts blocks of 512 bytes, whatever the disk's own block size
                (
                    Some(metadata.nlink()),
                    metadata.blocks().checked_mul(512),
                    metadata.mode() & 0o002 != 0,
                )
            };
            #[cfg(not(unix))]
            let (names, disk, exposed) = (None, None, false);
            Ok((metadata.len(), changed, names, disk, exposed))
        }) {
            Ok((size, changed, names, disk, exposed)) => {
                if !hex
                    .iter()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                {
                    paths.push((found.len(), entry.path()));
                }
                found.push(Found {
                    digest,
                    size,
                    changed,
                    names,
                    disk,
                    exposed,
                });
            }
            Err(err) => unreadable.push(Failure::new(&entry.path(), &err)),
        }
    }
    Ok(Fan {
        found,
        paths,
        unreadable,
    })
}

/// when the file `metadata` describes last changed, which gc ages it from: its modification time,
/// or on unix its ctime when later, which a seal, publishing or a link moves, as a seal dates the
/// modification time back to when the download began.
fn last_changed(metadata: &fs::Metadata) -> io::Result<SystemTime> {
    let modified = metadata.modified()?;
    Ok(super::object::changed(metadata).map_or(modified, |changed| changed.max(modified)))
}

pub(super) fn removed(path: &Path, result: io::Result<()>, not_removed: &mut Vec<Failure>) -> bool {
    match result {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            not_removed.push(Failure::new(path, &err));
            false
        }
    }
}

fn unreadable(path: &Path, error: &io::Error) -> Error {
    Error::io(format!("cannot read {}", path.display()), error)
}

/// what the file `metadata` describes takes on its disk, which for a split download with holes is
/// less than its length; windows makes no holes in these files, so there it is the length.
pub(super) fn on_disk(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

/// hashes the object at `path` through `block`, telling `progress` the bytes of each block as it
/// reads them.
fn hash(
    path: &Path,
    block: digest::Block,
    stop: &Stop,
    progress: &dyn Fn(u64),
) -> io::Result<Sha256> {
    let ahead = block.reads_ahead();
    hash_ahead(path, block, stop, progress, ahead)
}

/// [`hash`], reading on a second thread when `ahead`.
fn hash_ahead(
    path: &Path,
    mut block: digest::Block,
    stop: &Stop,
    progress: &dyn Fn(u64),
    ahead: bool,
) -> io::Result<Sha256> {
    let mut file = crate::volume::read_unfollowed(path)?;
    let _hashing = digest::Hashing::begin();
    let mut hasher = digest::Hasher::default();
    if ahead {
        if stop.is_asked() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let taken = digest::taken_ahead(block, &mut |buffer| file.read(buffer), &mut |bytes| {
            hasher.update(bytes);
            progress(bytes.len() as u64);
            if stop.is_asked() {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        });
        return match taken? {
            Some(()) => Err(io::ErrorKind::Interrupted.into()),
            None => Ok(hasher.finish().0),
        };
    }
    let buffer = block.get();
    loop {
        if stop.is_asked() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let read = file.read(buffer)?;
        if read == 0 {
            return Ok(hasher.finish().0);
        }
        hasher.update(&buffer[..read]);
        progress(read as u64);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::error::Kind;

    use super::super::tests::{cache, fat, hold, seal, store};
    use super::*;

    impl Cache {
        pub(crate) fn projects(&self) -> Vec<PathBuf> {
            self.registered_projects(&Stop::default()).unwrap()
        }

        pub(crate) fn objects(&self) -> Vec<Object> {
            self.scan_objects(Threads::default(), &Stop::default())
                .unwrap()
                .objects
        }

        pub(crate) fn sweep(&self, needed: &Needed, now: SystemTime) -> Freed {
            self.sweep_with_stop(
                needed,
                &[],
                now,
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap()
        }

        pub(crate) fn verify_all(
            &self,
            threads: usize,
            progress: &(dyn Fn(u64) + Sync),
        ) -> Checked {
            self.verify_with_stop(threads, progress, &Stop::default())
                .unwrap()
        }
    }

    #[test]
    fn maintenance_failures_keep_their_io_causes() {
        for (remove, kind, message) in [
            (
                (|_: &Path| Err(io::Error::other("injected storage error")))
                    as fn(&Path) -> io::Result<()>,
                "Other",
                "injected storage error",
            ),
            (
                (|_: &Path| {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected access error",
                    ))
                }) as fn(&Path) -> io::Result<()>,
                "PermissionDenied",
                "injected access error",
            ),
        ] {
            let (_dir, mut cache) = cache();
            let digest = store(&cache, b"good").digest;
            let path = cache.object_path(&digest);
            cache.volume = std::sync::Arc::new(
                crate::volume::Volume::new(cache.root.join("tmp")).with_removal(remove),
            );
            let freed = cache
                .sweep_with_stop(
                    &Needed::default(),
                    &[],
                    SystemTime::now(),
                    Duration::ZERO,
                    Threads::default(),
                    &Stop::default(),
                )
                .unwrap();
            let report = serde_json::to_value(&freed).unwrap();
            assert_eq!(report["not_removed"].as_array().unwrap().len(), 1);
            assert_eq!(report["not_removed"][0]["path"], serde_json::json!(path));
            assert_eq!(report["not_removed"][0]["kind"], kind, "{report}");
            assert_eq!(report["not_removed"][0]["message"], message, "{report}");
            assert_eq!(fs::read(path).unwrap(), b"good");
        }
    }

    #[test]
    fn failed_registration_removals_keep_the_original_cause() {
        let (directory, mut cache) = cache();
        let root = directory.path().join("missing-project");
        cache.register(&root).unwrap();
        let path = cache.registration(&root);
        cache.volume = std::sync::Arc::new(
            crate::volume::Volume::new(cache.root.join("tmp"))
                .with_removal(|_| Err(io::Error::other("registration storage failure"))),
        );
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        let report = serde_json::to_value(&freed).unwrap();
        assert_eq!(report["not_removed"].as_array().unwrap().len(), 1);
        assert_eq!(report["not_removed"][0]["path"], serde_json::json!(path));
        assert_eq!(report["not_removed"][0]["kind"], "Other");
        assert_eq!(
            report["not_removed"][0]["message"],
            "registration storage failure"
        );
        assert!(path.exists());
    }

    #[test]
    fn verification_runs_on_the_caller_when_workers_are_refused() {
        let (_dir, cache) = cache();
        store(&cache, b"good");
        let caller = std::thread::current().id();
        let _refusing = crate::threads::refuse_after(0);
        let checked = cache.verify_all(2, &|_| assert_eq!(std::thread::current().id(), caller));
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 4);
        assert_eq!(checked.changed, []);
        assert_eq!(checked.unreadable, []);
    }

    #[test]
    fn stopped_upkeep_does_not_read_or_remove_objects() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"keep");
        let root = cache.root.join("missing-project");
        cache.register(&root).unwrap();
        let registration = cache.registration(&root);
        let stop = Stop::default();
        stop.ask();
        let checked = cache.verify_with_stop(1, &|_| {}, &stop).unwrap_err();
        assert_eq!(checked.kind, Kind::Interrupted);
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now() + GRACE + Duration::from_secs(60),
                GRACE,
                Threads::default(),
                &stop,
            )
            .unwrap_err();
        assert_eq!(freed.kind, Kind::Interrupted);
        assert!(cache.object_path(&stored.digest).exists());
        assert!(registration.exists());
        assert!(cache.changed().is_empty());
    }

    #[test]
    fn stopping_verification_does_not_mark_the_current_object() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        damage(&cache.object_path(&stored.digest));
        let stop = Stop::default();
        let error = cache
            .verify_with_stop(1, &|_| stop.ask(), &stop)
            .unwrap_err();
        assert_eq!(error.kind, Kind::Interrupted);
        assert!(cache.changed().is_empty());
    }

    /// the fan folders are read on several workers, yet the scan is what a walk on one thread
    /// gives: the same objects in the same order, each at the path its folder gives it.
    #[test]
    fn a_scan_on_workers_matches_one_on_a_single_thread() {
        let (_dir, cache) = cache();
        let digests: Vec<Sha256> = (0..300)
            .map(|n| store(&cache, format!("object {n}").as_bytes()).digest)
            .collect();
        let fans: HashSet<String> = digests
            .iter()
            .map(|digest| digest.hex()[..2].to_owned())
            .collect();
        assert!(fans.len() >= 2 * FANS_PER_WORKER, "{} fans", fans.len());
        // a name in capitals still names its object, at its own path. the rename goes through
        // another name, since fat and exfat volumes ignore one that changes only the case
        let shouted = cache.object_path(&digests[0]);
        let loud = shouted.with_file_name(digests[0].hex()[2..].to_uppercase());
        let aside = shouted.with_file_name("renaming");
        fs::rename(&shouted, &aside).unwrap();
        fs::rename(&aside, &loud).unwrap();
        let fan = shouted.parent().unwrap();
        fs::write(fan.join("notes.txt"), b"not an object").unwrap();
        let free = (0..=255u8)
            .map(|byte| format!("{byte:02x}"))
            .find(|prefix| !fans.contains(prefix))
            .unwrap();
        fs::write(
            cache.root.join("objects").join(&free),
            b"a fan that is a file",
        )
        .unwrap();
        let listed = |threads: Threads| {
            let scan = cache.scan_objects(threads, &Stop::default()).unwrap();
            let objects: Vec<(Sha256, PathBuf, u64)> = scan
                .objects
                .into_iter()
                .map(|object| (object.digest, object.path, object.size))
                .collect();
            let unreadable: Vec<String> = scan
                .unreadable
                .iter()
                .map(|failure| failure.path.display().to_string())
                .collect();
            (objects, unreadable)
        };
        let (many, unreadable) = listed(Threads::default());
        assert_eq!(
            (many.clone(), unreadable.clone()),
            listed(Threads::default().at_most(1))
        );
        assert_eq!(many.len(), digests.len());
        // each fan's objects come as one run, which the sweep frees as that fan's
        let parents: Vec<&Path> = many
            .iter()
            .map(|(_, path, _)| path.parent().unwrap())
            .collect();
        let runs = parents.chunk_by(|one, next| one == next).count();
        assert_eq!(runs, parents.iter().collect::<HashSet<_>>().len());
        for (digest, path, size) in &many {
            let expected = if *digest == digests[0] {
                loud.clone()
            } else {
                cache.object_path(digest)
            };
            assert_eq!(path, &expected);
            assert_eq!(fs::symlink_metadata(path).unwrap().len(), *size);
        }
        assert_eq!(
            unreadable,
            [cache.root.join("objects").join(&free).display().to_string()]
        );
    }

    /// what freeing one object after another in the scan's order, the way a sweep freed before it
    /// went a fan folder at a time, would free of `cache` at `now` with `grace`, freeing nothing:
    /// the removed and the kept, as count and bytes, and the paths removed, in that order.
    fn freed_in_turn(
        cache: &Cache,
        needed: &Needed,
        (now, grace): (SystemTime, Duration),
    ) -> ((u64, u64), (u64, u64), Vec<PathBuf>) {
        let (mut removed, mut kept, mut paths) = ((0, 0), (0, 0), Vec::new());
        for object in cache.objects() {
            if needed.objects.contains(&object.digest) {
                continue;
            }
            if grace.is_zero()
                || now
                    .duration_since(object.changed)
                    .is_ok_and(|age| age >= grace)
            {
                removed = (removed.0 + 1, removed.1 + object.size);
                paths.push(object.path);
            } else {
                kept = (kept.0 + 1, kept.1 + object.size);
            }
        }
        (removed, kept, paths)
    }

    /// a sweep frees, keeps and counts what freeing one object after another in the scan's order
    /// did, over enough fan folders for several threads and with every object in one fan, and a
    /// pretend sweep, which keeps one, records those removals in that order.
    #[test]
    fn a_sweep_frees_what_freeing_in_turn_frees() {
        let spread: Vec<String> = (0..600).map(|n| format!("object {n}")).collect();
        let crowded: Vec<String> = (0..)
            .map(|n| format!("crowded {n}"))
            .filter(|body| crate::digest::sha256(body.as_bytes()).bytes()[0] == 0)
            .take(40)
            .collect();
        for (bodies, pretend, grace) in [
            (&spread, false, Duration::ZERO),
            (&spread, true, Duration::ZERO),
            (&spread, false, GRACE),
            (&crowded, false, Duration::ZERO),
            (&crowded, true, Duration::ZERO),
        ] {
            let (_dir, cache) = cache();
            let digests: Vec<Sha256> = bodies
                .iter()
                .map(|body| store(&cache, body.as_bytes()).digest)
                .collect();
            let needed = Needed {
                objects: digests.iter().step_by(3).copied().collect(),
                ..Needed::default()
            };
            let now = SystemTime::now();
            let (removed, kept, paths) = freed_in_turn(&cache, &needed, (now, grace));
            assert!(removed.0 > 0 || !grace.is_zero(), "nothing to free");
            let cache = if pretend { cache.pretending() } else { cache };
            let freed = cache
                .sweep_with_stop(
                    &needed,
                    &[],
                    now,
                    grace,
                    Threads::default(),
                    &Stop::default(),
                )
                .unwrap();
            let case = format!(
                "{} objects, pretend {pretend}, grace {grace:?}",
                bodies.len()
            );
            assert_eq!(
                (freed.removed_objects, freed.removed_bytes),
                removed,
                "{case}"
            );
            assert_eq!((freed.kept_objects, freed.kept_bytes), kept, "{case}");
            assert!(freed.not_removed.is_empty(), "{case}");
            let objects = cache.root().join("objects");
            let pretended: Vec<PathBuf> = cache
                .pretended()
                .into_iter()
                .filter(|path| path.starts_with(&objects))
                .collect();
            let mut gone: Vec<PathBuf> = digests
                .iter()
                .map(|digest| cache.object_path(digest))
                .filter(|path| !path.exists())
                .collect();
            if pretend {
                assert_eq!(pretended, paths, "{case}");
                assert!(gone.is_empty(), "{case}");
            } else {
                let mut paths = paths;
                paths.sort();
                gone.sort();
                assert_eq!(gone, paths, "{case}");
            }
        }
    }

    /// a removal the disk refuses is reported for each object, with its own cause, and a stop
    /// asked first frees nothing.
    #[test]
    fn a_sweep_reports_each_refusal_and_a_stop_frees_nothing() {
        let (_dir, mut cache) = cache();
        let digests: Vec<Sha256> = (0..100)
            .map(|n| store(&cache, format!("refused {n}").as_bytes()).digest)
            .collect();
        cache.volume = std::sync::Arc::new(
            crate::volume::Volume::new(cache.root.join("tmp"))
                .with_removal(|_| Err(io::Error::other("injected storage error"))),
        );
        let now = SystemTime::now();
        let stop = Stop::default();
        let sweep = || {
            cache.sweep_with_stop(
                &Needed::default(),
                &[],
                now,
                Duration::ZERO,
                Threads::default(),
                &stop,
            )
        };
        let freed = sweep().unwrap();
        assert_eq!(freed.removed_objects, 0);
        assert_eq!(freed.not_removed.len(), digests.len());
        let report = serde_json::to_value(&freed).unwrap();
        assert_eq!(
            report["not_removed"][0]["message"],
            "injected storage error"
        );
        stop.ask();
        assert_eq!(sweep().unwrap_err().kind, Kind::Interrupted);
        assert!(
            digests
                .iter()
                .all(|digest| cache.object_path(digest).exists())
        );
    }

    /// a fan the scan cannot read, the last it lists among hundreds, stops the sweep before any
    /// removal: the whole cache is inspected first, and stays as it was.
    #[cfg(unix)]
    #[test]
    fn a_fan_that_cannot_be_read_stops_the_sweep_before_any_removal() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let (_dir, cache) = cache();
        let digests: Vec<Sha256> = (0..300)
            .map(|n| store(&cache, format!("object {n}").as_bytes()).digest)
            .collect();
        let last = cache
            .objects()
            .last()
            .unwrap()
            .path
            .parent()
            .unwrap()
            .to_owned();
        let before = fs::metadata(&last).unwrap().permissions();
        fs::set_permissions(&last, fs::Permissions::from_mode(0o000)).unwrap();
        let swept = cache.sweep_with_stop(
            &Needed::default(),
            &[],
            SystemTime::now(),
            Duration::ZERO,
            Threads::default(),
            &Stop::default(),
        );
        fs::set_permissions(&last, before).unwrap();
        let refused = swept.unwrap_err();
        assert!(
            refused
                .message
                .starts_with("cannot inspect; the cache stays unchanged:"),
            "{}",
            refused.message
        );
        assert!(
            digests
                .iter()
                .all(|digest| cache.object_path(digest).exists())
        );
    }

    #[cfg(unix)]
    #[test]
    fn verification_reports_directory_and_metadata_permission_errors() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        for (root, mode) in [(true, 0), (false, 0), (false, 0o400)] {
            let (_dir, cache) = cache();
            let stored = store(&cache, b"abc");
            let object = cache.object_path(&stored.digest);
            let folder = if root {
                cache.root.join("objects")
            } else {
                object.parent().unwrap().to_path_buf()
            };
            let before = fs::metadata(&folder).unwrap().permissions();
            fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
            let checked = cache.verify_with_stop(1, &|_| {}, &Stop::default());
            fs::set_permissions(&folder, before).unwrap();
            let checked = checked.unwrap();
            let unreadable = if mode == 0o400 {
                object.clone()
            } else {
                folder
            };
            assert_eq!(checked.unreadable.len(), 1);
            assert_eq!(checked.unreadable[0].path, unreadable);
            assert_eq!(
                serde_json::to_value(&checked.unreadable[0]).unwrap()["kind"],
                "PermissionDenied"
            );
            assert_eq!(checked.objects, 0);
            assert_eq!(checked.changed, []);
            assert!(object.exists());
        }
    }

    #[test]
    fn gc_now_frees_an_unneeded_object_whose_time_is_ahead_of_the_clock() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"from the future");
        let object = cache.object_path(&stored.digest);
        // FAT rounds a new file's time up by as much as two seconds
        let ahead = SystemTime::now() + Duration::from_secs(2);
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test moves a sealed object's time"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).unwrap();
        fs::File::options()
            .write(true)
            .open(&object)
            .unwrap()
            .set_modified(ahead)
            .unwrap();
        let young = cache
            .sweep_with_stop(
                &Needed::default(),
                &[],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(young.kept_objects, 1);
        let now = cache
            .sweep_with_stop(
                &Needed::default(),
                &[],
                SystemTime::now(),
                Duration::ZERO,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(now.removed_objects, 1);
        assert!(!object.exists());
    }

    #[test]
    fn projects_register_once_and_can_be_forgotten() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root.clone()]);
        let freed = cache
            .sweep_with_stop(
                &Needed::default(),
                std::slice::from_ref(&root),
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(freed.not_removed, []);
        assert_eq!(freed.forgotten, std::slice::from_ref(&root));
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());
        let absent = cache
            .sweep_with_stop(
                &Needed::default(),
                &[root],
                SystemTime::now(),
                GRACE,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
        assert_eq!(absent.not_removed, []);
        assert_eq!(absent.forgotten, Vec::<PathBuf>::new());
    }

    #[test]
    fn a_registration_is_sealed_once_flushed_and_written_again_when_it_changed() {
        let (dir, cache) = cache();
        let root = dir.path().join("thesis");
        cache.register(&root).unwrap();
        let path = cache.registration(&root);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        crate::volume::set_read_only(&path, false).unwrap();
        fs::write(&path, b"{}").unwrap();
        cache.register(&root).unwrap();
        assert_eq!(cache.projects(), vec![root]);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
    }

    /// a record someone put among the projects' records of a cache they share, a link, a fifo or
    /// a file larger than any record, is refused at once rather than followed, waited on or read
    /// to its end, as a fifo or a link to /dev/zero would hang gc.
    #[test]
    fn a_planted_project_record_is_refused_unread() {
        let planted = |found: &Cache| {
            let planted = found.root.join("projects").join("0000-planted.json");
            fs::create_dir_all(planted.parent().unwrap()).unwrap();
            planted
        };
        let record = |root: &Path| {
            serde_json::to_vec(&Registration {
                root: root.to_path_buf(),
                disk: None,
            })
            .unwrap()
        };
        let (dir, large) = cache();
        let mut padded = vec![b' '; 1024 * 1024];
        padded.extend(record(dir.path()));
        fs::write(planted(&large), padded).unwrap();
        assert!(
            large.registered_projects(&Stop::default()).is_err(),
            "too large"
        );

        #[cfg(unix)]
        {
            let (dir, linked) = cache();
            let elsewhere = dir.path().join("elsewhere.json");
            fs::write(&elsewhere, record(dir.path())).unwrap();
            std::os::unix::fs::symlink(&elsewhere, planted(&linked)).unwrap();
            assert!(
                linked.registered_projects(&Stop::default()).is_err(),
                "a link"
            );

            let (dir, waiting) = cache();
            let fifo = planted(&waiting);
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap();
            assert!(made.success());
            // a writer answers a reader that waits for one, so an open that waits still ends
            let (path, written) = (fifo.clone(), record(dir.path()));
            let writer = std::thread::spawn(move || {
                let mut opened = fs::OpenOptions::new().write(true).open(path).unwrap();
                // a reader that refused the fifo is gone, so the write may find none
                let _ = io::Write::write_all(&mut opened, &written);
            });
            let registered = waiting.registered_projects(&Stop::default());
            let flags = rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK;
            let released = rustix::fs::open(&fifo, flags, rustix::fs::Mode::empty()).unwrap();
            writer.join().unwrap();
            drop(released);
            assert!(registered.is_err(), "a fifo: {registered:?}");
        }
    }

    #[test]
    fn a_sweep_reports_refused_removals() {
        let (_dir, mut cache) = cache();
        let stored = store(&cache, b"cannot remove");
        let object = cache.object_path(&stored.digest);
        let leftover = cache.root.join("tmp").join("left.new");
        fs::write(&leftover, b"leftover").unwrap();
        seal(&leftover);
        cache.volume = std::sync::Arc::new(
            crate::volume::Volume::new(cache.root.join("tmp"))
                .with_removal(|_| Err(io::ErrorKind::PermissionDenied.into())),
        );
        let freed = cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        assert_eq!(
            (
                freed.removed_objects,
                freed.removed_bytes,
                freed.removed_leftovers
            ),
            (0, 0, 0)
        );
        let report = serde_json::to_value(&freed).unwrap();
        let paths = report["not_removed"].as_array().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(
            paths
                .iter()
                .any(|failure| failure["path"] == serde_json::json!(object))
        );
        assert!(
            paths
                .iter()
                .any(|failure| failure["path"] == serde_json::json!(leftover))
        );
        assert!(object.exists());
        assert!(leftover.exists());
    }

    /// a download a later run finished is dated back to its first run's start, so a sweep ages an
    /// object by its ctime, which its seal, publishing or a link moves.
    #[cfg(unix)]
    #[test]
    fn an_object_dated_back_to_its_birth_still_gets_a_day() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"resumed");
        let object = cache.object_path(&stored.digest);
        let began = SystemTime::now() - GRACE - Duration::from_hours(24);
        fs::File::open(&object)
            .unwrap()
            .set_modified(began)
            .unwrap();
        let freed = cache.sweep(&Needed::default(), SystemTime::now());
        assert_eq!(freed.removed_objects, 0);
        assert!(object.exists());
    }

    /// a download whose last byte came long after its first is sealed with its time dated back
    /// to its start, and one a run died before publishing waits for the next run to publish it,
    /// so a sweep ages it by its ctime, which the seal moved, as it ages an object.
    #[cfg(unix)]
    #[test]
    fn a_sealed_download_dated_back_to_its_start_still_gets_a_day() {
        let (_dir, cache) = cache();
        let url = "https://example.org/resumed";
        let claim = || match cache.claim(url).unwrap() {
            crate::cache::Claim::Owned(partial) => partial,
            crate::cache::Claim::Busy => panic!("nothing else holds {url}"),
        };
        let partial = claim();
        let mut writer = partial
            .writer(0, None, crate::cache::Validators::default(), false, false)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        drop((writer, partial));
        // as the seal leaves the part of a download begun two days ago
        let key = digest::sha256(url.as_bytes()).hex();
        let part = cache.partial_folder(&key).join(format!("{key}.part"));
        let began = SystemTime::now() - GRACE - Duration::from_hours(24);
        fs::File::options()
            .write(true)
            .open(&part)
            .unwrap()
            .set_modified(began)
            .unwrap();
        seal(&part);

        let freed = cache.sweep(&Needed::default(), SystemTime::now());

        assert_eq!(freed.removed_leftovers, 0, "{:?}", freed.not_removed);
        let resumed = claim().resumable().map(|(kept, _)| kept);
        assert_eq!(resumed, Some(3), "the next run publishes it");
    }

    /// a sweep removes what no project needs once it is old, and the mark of a file a key came
    /// with goes with its file.
    #[test]
    fn a_sweep_keeps_what_is_needed_and_what_is_new() {
        for (_dir, cache) in [cache(), fat()] {
            let needed = store(&cache, b"needed");
            let unneeded = store(&cache, b"unneeded");
            for stored in [&needed, &unneeded] {
                cache.mark_keyed(&stored.digest).unwrap();
            }
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            let keep = Needed {
                objects: HashSet::from([needed.digest]),
                ..Needed::default()
            };
            assert_eq!(
                cache.sweep(&keep, SystemTime::now()).removed_objects,
                0,
                "nothing is old yet"
            );
            assert!(
                cache.is_keyed(&unneeded.digest),
                "a mark stays with its file"
            );
            let freed = cache.sweep(&keep, later);
            assert_eq!((freed.removed_objects, freed.removed_bytes), (1, 8));
            assert!(cache.find(&needed.digest, Some(6)).is_some());
            assert!(cache.find(&unneeded.digest, Some(8)).is_none());
            assert!(cache.is_keyed(&needed.digest));
            assert!(!cache.is_keyed(&unneeded.digest));
        }
    }

    #[test]
    fn a_sweep_removes_what_stopped_runs_left_sealed() {
        for (_dir, cache) in [cache(), fat()] {
            // a download sealed by a run that died while publishing it, and a member sealed by
            // one that died while unpacking
            let key = "ab".repeat(32);
            let partial = cache.partial_folder(&key);
            fs::create_dir_all(&partial).unwrap();
            fs::write(partial.join(format!("{key}.lock")), b"").unwrap();
            fs::write(partial.join(format!("{key}.json")), b"{}").unwrap();
            let sealed = [
                partial.join(format!("{key}.part")),
                cache.root.join("tmp").join("0123456789abcdef.new"),
            ];
            for path in &sealed {
                fs::write(path, b"abc").unwrap();
                seal(path);
            }
            let later = SystemTime::now() + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 3);
            assert!(sealed.iter().all(|path| !path.exists()));
            let left: Vec<String> = fs::read_dir(partial)
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(left, Vec::<String>::new(), "nothing holds the lock");
        }
    }

    #[test]
    fn a_sweep_keeps_the_lock_of_a_download_in_progress_and_drops_finished_ones() {
        let (_dir, cache) = cache();
        let running = cache.claim("https://example.org/running").unwrap();
        let lock = |url: &str| {
            let key = digest::sha256(url.as_bytes()).hex();
            cache.partial_folder(&key).join(format!("{key}.lock"))
        };
        let finished = lock("https://example.org/finished");
        fs::create_dir_all(finished.parent().unwrap()).unwrap();
        fs::write(&finished, b"").unwrap();
        let freed = cache.sweep(&Needed::default(), SystemTime::now());
        assert_eq!(freed.removed_leftovers, 0, "a lock file frees nothing");
        assert!(lock("https://example.org/running").exists());
        assert!(!lock("https://example.org/finished").exists());
        drop(running);
    }

    /// a sweep finds an unfinished download by its lock, so one whose bytes it could not remove,
    /// as windows refuses for a file another program holds open, keeps its lock for the next.
    #[test]
    fn a_download_a_sweep_could_not_remove_keeps_its_lock() {
        let (_dir, mut cache) = cache();
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        let [part, record, lock] =
            ["part", "json", "lock"].map(|end| shard.join(format!("{key}.{end}")));
        fs::write(&part, b"abc").unwrap();
        fs::write(&record, b"{}").unwrap();
        fs::write(&lock, b"").unwrap();
        let held = crate::volume::Volume::new(cache.root.join("tmp")).with_removal(|path| {
            if path.extension().is_some_and(|end| end == "part") {
                return Err(io::Error::other("another program holds it open"));
            }
            fs::remove_file(path)
        });
        let free = std::mem::replace(&mut cache.volume, std::sync::Arc::new(held));
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(cache.sweep(&Needed::default(), later).not_removed.len(), 1);
        assert!(part.exists());
        assert!(
            lock.exists(),
            "the lock a later sweep finds the bytes by is gone"
        );
        // bytes kept without their record could never be continued, though a report counts them
        assert!(record.exists(), "the record of the bytes kept is gone");
        cache.volume = free;
        cache.sweep(&Needed::default(), later);
        assert_eq!(fs::read_dir(&shard).unwrap().count(), 0);
        assert_eq!(cache.unfinished().downloads, 0);
    }

    /// the same with the file held open for real, shared for reading alone, as a scanner may.
    #[cfg(windows)]
    #[test]
    fn a_download_another_program_holds_is_swept_once_it_lets_go() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let (_dir, cache) = cache();
        let key = "cd".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        let [part, lock] = ["part", "lock"].map(|end| shard.join(format!("{key}.{end}")));
        fs::write(&part, b"abc").unwrap();
        fs::write(&lock, b"").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&part)
            .unwrap();
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(cache.sweep(&Needed::default(), later).not_removed.len(), 1);
        assert!(part.exists() && lock.exists());
        drop(held);
        cache.sweep(&Needed::default(), later);
        assert_eq!(fs::read_dir(&shard).unwrap().count(), 0);
    }

    #[test]
    fn unfinished_downloads_in_shards_are_counted_and_swept() {
        let (_dir, cache) = cache();
        let url = "https://example.org/sharded";
        let crate::cache::Claim::Owned(partial) = cache.claim(url).unwrap() else {
            panic!("nothing else holds the download")
        };
        let key = digest::sha256(url.as_bytes()).hex();
        let shard = cache.root.join("partial").join(&key[..1]);
        assert!(shard.join(format!("{key}.lock")).is_file());
        let mut writer = partial
            .writer(
                0,
                Some(6),
                crate::cache::Validators::default(),
                false,
                false,
            )
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        drop(writer);
        let part = shard.join(format!("{key}.part"));
        assert!(part.is_file());
        let unfinished = cache.unfinished();
        assert_eq!(
            (unfinished.downloads, unfinished.bytes),
            (1, on_disk(&fs::metadata(&part).unwrap()))
        );
        drop(partial);
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(
            cache.sweep(&Needed::default(), later).removed_leftovers,
            2,
            "the bytes and the record that lets them resume"
        );
        assert_eq!(fs::read_dir(&shard).unwrap().count(), 0);
        assert_eq!(cache.unfinished().downloads, 0);
    }

    /// a split download's file has holes, so what unfinished downloads hold, and what gc frees of
    /// them, is what the disk holds, not their files' length.
    #[test]
    fn unfinished_downloads_count_what_the_disk_holds() {
        let (_dir, cache) = cache();
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        let part = shard.join(format!("{key}.part"));
        let record = shard.join(format!("{key}.json"));
        let mut file = fs::File::create(&part).unwrap();
        file.write_all(b"abc").unwrap();
        file.set_len(64 << 20).unwrap();
        drop(file);
        fs::write(&record, b"{}").unwrap();
        fs::write(shard.join(format!("{key}.lock")), b"").unwrap();
        #[cfg(unix)]
        let held = |path: &Path| {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(path).unwrap().blocks() * 512
        };
        #[cfg(unix)]
        let (part_held, record_held) = (held(&part), held(&record));
        let unfinished = cache.unfinished();
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        let freed = cache.sweep(&Needed::default(), later);
        assert_eq!((unfinished.downloads, freed.removed_leftovers), (1, 2));
        // windows makes no holes in these files, so there their length is what the disk holds
        #[cfg(unix)]
        assert_eq!(
            (unfinished.bytes, freed.removed_leftover_bytes),
            (part_held, part_held + record_held)
        );
    }

    /// the downloads the full-disk hint says `cache gc --now` frees are those whose lock it can
    /// take as its sweep does: a lock someone made a fifo, or a link to a file elsewhere, counts
    /// for none, and is never waited on or followed.
    #[cfg(unix)]
    #[test]
    fn unheld_downloads_are_those_a_sweep_frees() {
        let (dir, cache) = cache();
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        fs::write(shard.join(format!("{key}.part")), b"abc").unwrap();
        let lock = shard.join(format!("{key}.lock"));
        let outside = dir.path().join("outside");
        fs::write(&outside, b"").unwrap();
        std::os::unix::fs::symlink(&outside, &lock).unwrap();
        assert_eq!(cache.unfinished_unheld().downloads, 0, "a link");

        fs::remove_file(&lock).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&lock)
            .status()
            .unwrap();
        assert!(made.success());
        // a writer answers a reader that waits for one, so an open that waits still ends
        let path = lock.clone();
        let writer = std::thread::spawn(move || {
            drop(fs::OpenOptions::new().write(true).open(path).unwrap());
        });
        let fifo = cache.unfinished_unheld().downloads;
        let flags = rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK;
        let released = rustix::fs::open(&lock, flags, rustix::fs::Mode::empty()).unwrap();
        writer.join().unwrap();
        drop(released);
        assert_eq!(fifo, 0, "a fifo");

        fs::remove_file(&lock).unwrap();
        fs::write(&lock, b"").unwrap();
        let held = fs::File::options().write(true).open(&lock).unwrap();
        held.lock().unwrap();
        assert_eq!(cache.unfinished_unheld().downloads, 0, "a lock a run holds");
        drop(held);
        assert_eq!(
            cache.unfinished_unheld().downloads,
            1,
            "a lock nobody holds"
        );
    }

    /// only the 16 shards under `partial/` count, so a file at its top, in a folder that is no
    /// shard or behind a link in a shard's place, is neither counted nor freed.
    #[test]
    fn only_the_shards_under_partial_hold_unfinished_downloads() {
        let (_dir, cache) = cache();
        let partial = cache.root.join("partial");
        let key = "ab".repeat(32);
        let mut kept = Vec::new();
        for folder in [partial.clone(), partial.join("ab")] {
            fs::create_dir_all(&folder).unwrap();
            for extension in ["part", "json", "lock"] {
                let path = folder.join(format!("{key}.{extension}"));
                fs::write(&path, b"abc").unwrap();
                kept.push(path);
            }
        }
        #[cfg(unix)]
        {
            let outside = cache.root.with_file_name("outside");
            fs::create_dir(&outside).unwrap();
            for extension in ["part", "json", "lock"] {
                let path = outside.join(format!("{key}.{extension}"));
                fs::write(&path, b"abc").unwrap();
                kept.push(path);
            }
            std::os::unix::fs::symlink(&outside, partial.join("a")).unwrap();
        }
        assert_eq!(cache.partial_folders(), Vec::<PathBuf>::new());
        assert_eq!(cache.unfinished(), Unfinished::default());
        let later = SystemTime::now() + GRACE + Duration::from_secs(60);
        assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 0);
        assert!(kept.iter().all(|path| path.exists()));
    }

    #[test]
    fn a_sweep_removes_unlocked_staging_without_waiting() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir_all(folder.join("03")).unwrap();
            fs::write(folder.join("lock"), b"").unwrap();
            let member = folder.join("03/member.new");
            fs::write(&member, b"unfinished").unwrap();
            seal(&member);
            let freed = cache.sweep(&Needed::default(), SystemTime::now());
            assert_eq!(freed.removed_leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn staging_without_locking_keeps_the_age_rule() {
        for (_dir, cache) in [cache(), fat()] {
            let folder = cache.root().join("tmp").join("0123456789abcdef.stage");
            fs::create_dir_all(folder.join("03")).unwrap();
            fs::write(folder.join("unlocked"), b"").unwrap();
            let member = folder.join("03/member.new");
            fs::write(&member, b"unfinished").unwrap();
            let now = SystemTime::now();
            let written = now + GRACE;
            fs::OpenOptions::new()
                .write(true)
                .open(&member)
                .unwrap()
                .set_modified(written)
                .unwrap();
            seal(&member);
            assert_eq!(cache.sweep(&Needed::default(), now).removed_leftovers, 0);
            assert_eq!(
                cache
                    .sweep(&Needed::default(), written + Duration::from_secs(60))
                    .removed_leftovers,
                0,
                "a recent write keeps a member in an older folder"
            );
            assert!(member.exists());
            let later = written + GRACE + Duration::from_secs(60);
            assert_eq!(cache.sweep(&Needed::default(), later).removed_leftovers, 1);
            assert!(!folder.exists());
        }
    }

    #[test]
    fn a_run_that_opened_a_lock_before_a_sweep_removed_it_never_owns_the_download_twice() {
        let (_dir, cache) = cache();
        let url = "https://example.org/raced";
        let key = digest::sha256(url.as_bytes()).hex();
        let lock = cache.partial_folder(&key).join(format!("{key}.lock"));
        fs::create_dir_all(cache.partial_folder(&key)).unwrap();
        // a run waiting for the download opens the lock, gc sweeps, then the run takes the lock
        let opened = crate::volume::open_unfollowed(&lock, true).unwrap();
        cache.sweep(
            &Needed::default(),
            SystemTime::now() + GRACE + Duration::from_secs(60),
        );
        // fat and exfat keep a removed file's name until its last handle closes, so there the
        // new claim is refused instead; either way no two runs own the download
        let claimed = cache.claim(url);
        let old = super::super::partial::take_lock(&lock, opened);
        match claimed {
            Ok(super::super::Claim::Owned(_owner)) => assert!(
                matches!(
                    old,
                    Err(std::fs::TryLockError::WouldBlock | std::fs::TryLockError::Error(_))
                ),
                "a second run owns the same download"
            ),
            Ok(super::super::Claim::Busy) => {}
            Err(err) => assert!(
                cfg!(windows) && err.kind() == io::ErrorKind::PermissionDenied,
                "{err}"
            ),
        }
    }

    #[test]
    fn a_sweep_keeps_the_records_of_archives_in_use() {
        let (_dir, cache) = cache();
        let used = digest::sha256(b"used archive");
        let unused = digest::sha256(b"unused archive");
        for archive in [used, unused] {
            cache.record_unpacked(&archive, &[]).unwrap();
        }
        let keep = Needed {
            unpacked: HashSet::from([used]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert_eq!(cache.unpacked_archives(), [used]);
    }

    #[test]
    fn a_sweep_forgets_links_whose_bytes_are_gone() {
        let (_dir, cache) = cache();
        let held = store(&cache, b"held");
        let record = |url: &str, sha256| super::super::UrlRecord {
            url: url.to_owned(),
            sha256,
            size: 4,
            name: None,
            validators: super::super::Validators::default(),
            page: false,
        };
        cache
            .save_url_record(&record("https://example.org/held", held.digest))
            .unwrap();
        cache
            .save_url_record(&record("https://example.org/gone", digest::sha256(b"gone")))
            .unwrap();
        let keep = Needed {
            objects: HashSet::from([held.digest]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now());
        assert!(cache.url_record("https://example.org/held").is_some());
        assert!(cache.url_record("https://example.org/gone").is_none());
    }

    #[test]
    fn a_sweep_keeps_the_link_of_an_archive_whose_files_are_unpacked() {
        let (_dir, cache) = cache();
        let member = store(&cache, b"member");
        let archive = digest::sha256(b"archive dropped after unpacking");
        cache
            .record_unpacked(
                &archive,
                &[super::super::Member {
                    path: "a.csv".to_owned(),
                    sha256: member.digest,
                    size: member.size,
                }],
            )
            .unwrap();
        cache
            .save_url_record(&super::super::UrlRecord {
                url: "https://example.org/data.zip".to_owned(),
                sha256: archive,
                size: 99,
                name: None,
                validators: super::super::Validators::default(),
                page: false,
            })
            .unwrap();
        let keep = Needed {
            objects: HashSet::from([member.digest]),
            unpacked: HashSet::from([archive]),
            ..Needed::default()
        };
        cache.sweep(&keep, SystemTime::now() + GRACE + Duration::from_secs(60));
        assert!(
            cache.url_record("https://example.org/data.zip").is_some(),
            "the link's record serves its unpacked files"
        );
    }

    #[test]
    fn verifying_marks_objects_whose_bytes_changed() {
        let (_dir, cache) = cache();
        let good = store(&cache, b"good");
        let bad = store(&cache, b"bad!");
        let mut permissions = fs::metadata(cache.object_path(&bad.digest))
            .unwrap()
            .permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(cache.object_path(&bad.digest), permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(cache.object_path(&bad.digest))
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
        let seen = AtomicUsize::new(0);
        let checked = cache.verify_all(3, &|bytes| {
            seen.fetch_add(usize::try_from(bytes).unwrap(), Ordering::Relaxed);
        });
        assert_eq!(checked.objects, 2);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(seen.load(Ordering::Relaxed), 8);
        assert!(cache.find(&good.digest, Some(4)).is_some());
        assert!(cache.find(&bad.digest, Some(4)).is_none());
        assert_eq!(
            cache.changed().into_keys().collect::<Vec<_>>(),
            [bad.digest],
            "projects linking to it can learn it changed"
        );
    }

    /// a bar sized from the scan moves as each block of a large object is read, never only once
    /// the whole object is hashed.
    #[test]
    fn verifying_tells_the_bytes_of_each_block_it_reads() {
        let (_dir, cache) = cache();
        let block = 1024 * 1024;
        store(&cache, &vec![7; 2 * block + 3]);
        let told = Mutex::new(Vec::new());
        let checked = cache.verify_all(1, &|bytes| told.lock().unwrap().push(bytes));
        assert_eq!(checked.changed, []);
        let block = block as u64;
        assert_eq!(told.into_inner().unwrap(), [block, block, 3]);
    }

    #[test]
    fn unreadable_objects_are_kept_without_a_change_mark() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"intact");
        let object = cache.object_path(&stored.digest);
        #[cfg(windows)]
        let held = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_DELETE: u32 = 4;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_DELETE)
                .open(&object)
                .unwrap()
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if rustix::process::geteuid().is_root() {
                return crate::test_skips::skip("unprivileged", "root ignores file permissions");
            }
            fs::set_permissions(&object, fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert!(cache.verify(&stored.digest).is_err());
        assert!(object.exists());
        assert!(cache.changed().is_empty());
        let checked = cache.verify_all(2, &|_| {});
        #[cfg(windows)]
        drop(held);
        assert!(
            checked.changed.is_empty(),
            "a read error is not changed bytes"
        );
        assert_eq!(checked.stuck, vec![]);
        assert!(object.exists(), "unreadable objects must stay in the cache");
        assert!(cache.changed().is_empty());
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 6);
        assert_eq!(
            serde_json::to_value(&checked).unwrap()["unreadable"][0]["path"],
            serde_json::json!(object)
        );
    }

    #[test]
    fn a_changed_object_held_open_is_still_marked() {
        let (_dir, cache) = cache();
        let bad = store(&cache, b"bad!");
        let object = cache.object_path(&bad.digest);
        damage(&object);
        let held = hold(&object);
        let checked = cache.verify_all(2, &|_| {});
        drop(held);
        assert_eq!(checked.changed, vec![bad.digest]);
        assert_eq!(checked.stuck, []);
        assert_eq!(fs::read(&object).unwrap(), b"BAD!");
    }

    #[test]
    fn objects_whose_bytes_rotted_wait_for_replacement() {
        for (_dir, cache) in [cache(), fat()] {
            let rotten = [store(&cache, b"rot1"), store(&cache, b"rot2")];
            for object in &rotten {
                damage(&cache.object_path(&object.digest));
                // bytes that rot on disk leave the read-only flag as it was
                seal(&cache.object_path(&object.digest));
            }
            assert!(!cache.verify(&rotten[0].digest).unwrap());
            let checked = cache.verify_all(2, &|_| {});
            assert_eq!(checked.changed.len(), 2);
            assert!(
                rotten
                    .iter()
                    .all(|object| checked.changed.contains(&object.digest))
            );
            assert_eq!(checked.stuck, vec![]);
            assert!(
                rotten
                    .iter()
                    .all(|object| cache.object_path(&object.digest).exists())
            );
        }
    }

    fn damage(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .write_all(b"BAD!")
            .unwrap();
    }

    /// an object read ahead on a second thread hashes as it does read in turn, tells its bar all its
    /// bytes, and stops after the same count of blocks when a stop is asked.
    #[test]
    fn an_object_read_ahead_hashes_and_stops_as_one_read_in_turn() {
        let _alone = crate::digest::AloneInTests::set(true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        let bytes: Vec<u8> = (0..=250)
            .cycle()
            .take(usize::try_from(digest::AHEAD_FROM).unwrap() + 5)
            .collect();
        fs::write(&path, &bytes).unwrap();
        let most = bytes.len() as u64 + 1;
        for stop_after in [None, Some(0), Some(2)] {
            let mut seen = Vec::new();
            for ahead in [false, true] {
                let stop = Stop::default();
                let told = std::cell::RefCell::new(Vec::new());
                let progress = |read| {
                    told.borrow_mut().push(read);
                    if stop_after.is_some_and(|after| told.borrow().len() >= after) {
                        stop.ask();
                    }
                };
                if stop_after == Some(0) {
                    stop.ask();
                }
                let hashed = hash_ahead(&path, digest::Block::sized(most), &stop, &progress, ahead)
                    .map_err(|err| err.kind());
                let told = told.into_inner();
                let whole = told.iter().sum::<u64>() == bytes.len() as u64;
                seen.push((hashed, stop_after.map(|_| told.len()), whole));
            }
            assert_eq!(seen[0], seen[1], "{stop_after:?}");
            let whole = stop_after.is_none().then(|| digest::sha256(&bytes));
            assert_eq!(seen[0].0.ok(), whole, "{stop_after:?}");
        }
    }
}
