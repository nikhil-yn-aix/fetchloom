//! new objects written in one go, such as the members of an archive. each is created read-only in
//! `tmp/`, written and hashed in one pass, dated by its birth, flushed and closed, then published
//! through the cache's hardlink-or-rename policy.

use std::fs::{File, FileTimes, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(unix, test))]
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

use super::{Cache, Staging, Stored};
use crate::digest::{Hasher, Sha256};
use crate::volume::Volume;

/// a new object being written into `tmp/`. dropped before it is sealed, it leaves nothing.
pub(crate) struct NewObject {
    file: File,
    temp: Temporary,
    hasher: Hasher,
    written: u64,
    begun: Option<SystemTime>,
}

/// an object written in full and flushed to disk, waiting in `tmp/` to be put in place. dropped
/// before it is published, it leaves nothing.
pub(crate) struct Sealed {
    temp: Temporary,
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
}

/// the name of a file this process made in `tmp/`. dropped before the file is published, it
/// removes the file.
struct Temporary {
    path: PathBuf,
    volume: Arc<Volume>,
    published: bool,
    // a member may outlive the caller's staging handle while a writer finishes sealing it
    staging: Option<Arc<Staging>>,
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if !self.published {
            if let Some(staging) = &self.staging {
                let _ = staging.remove_member(&self.path);
            } else {
                let _ = self.volume.remove(&self.path);
            }
        }
    }
}

impl Cache {
    /// starts a batch of new objects that [`Batch::flush`] flushes with one call, as git's batch
    /// mode does: on linux one flush per small file costs far more than writing it. `None` means
    /// each object must be flushed on its own. the batch holds the cache's filesystem open from
    /// before its first write, because linux reports a failed write-back only to descriptors
    /// opened before the failure, and to syncfs only from 5.8.
    #[cfg(target_os = "linux")]
    pub(crate) fn batch(&self) -> io::Result<Option<Batch>> {
        let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        if !syncfs_reports_errors(&release) {
            return Ok(None);
        }
        Ok(Some(Batch::open(&self.root.join("tmp"))?))
    }

    /// on macos fsync only hands a file to the drive, and only `F_FULLFSYNC` empties the drive's
    /// cache, for every file at once. each object gets the cheap fsync as it is sealed, and the
    /// batch one `F_FULLFSYNC`, as git's batch mode does, instead of a full flush per object.
    #[cfg(target_os = "macos")]
    #[expect(clippy::unnecessary_wraps, reason = "linux may fail to open the batch")]
    pub(crate) fn batch(&self) -> io::Result<Option<Batch>> {
        Ok(Some(Batch(self.root.join("tmp"))))
    }

    #[cfg(test)]
    pub(crate) fn new_object(&self) -> io::Result<NewObject> {
        self.new_object_in(&self.root.join("tmp"), None)
    }

    pub(super) fn new_object_in(
        &self,
        folder: &Path,
        staging: Option<Arc<Staging>>,
    ) -> io::Result<NewObject> {
        loop {
            let path = folder.join(format!("{:016x}.new", fastrand::u64(..)));
            let created = staging.as_ref().map_or_else(
                || create_sealed(&path),
                |staging| staging.create_member(&path),
            );
            match created {
                Ok(file) => {
                    let begun = begun(&file);
                    return Ok(NewObject {
                        file,
                        temp: Temporary {
                            path,
                            volume: Arc::clone(&self.volume),
                            published: false,
                            staging,
                        },
                        hasher: Hasher::default(),
                        written: 0,
                        begun,
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        }
    }
}

/// new objects written since the batch began, flushed together.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct Batch {
    file: File,
    device: u64,
}

#[cfg(target_os = "linux")]
impl Batch {
    pub(super) fn open(folder: &Path) -> io::Result<Batch> {
        use std::os::unix::fs::MetadataExt as _;

        let file = File::open(folder)?;
        let device = file.metadata()?.dev();
        Ok(Batch { file, device })
    }

    #[cfg(test)]
    pub(super) fn with_file_for_test(file: File) -> Batch {
        use std::os::unix::fs::MetadataExt as _;

        let device = file.metadata().unwrap().dev();
        Batch { file, device }
    }

    pub(super) fn covers(&self, metadata: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt as _;

        self.device == metadata.dev()
    }

    /// flushes everything written to the cache's filesystem. each call reports a write-back
    /// error since the one before, so a batch may flush more than once.
    pub(crate) fn flush(&self) -> io::Result<()> {
        rustix::fs::syncfs(&self.file)?;
        Ok(())
    }
}

/// new objects sealed with a plain fsync since the batch began, made durable together.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub(crate) struct Batch(PathBuf);

#[cfg(target_os = "macos")]
impl Batch {
    pub(crate) fn flush(self) -> io::Result<()> {
        let file = tempfile::tempfile_in(&self.0)?;
        match rustix::fs::fcntl_fullfsync(&file) {
            // a drive that takes no `F_FULLFSYNC`, as some external and network ones, has had
            // each object's fsync, which is all it offers
            Err(rustix::io::Errno::NOTSUP | rustix::io::Errno::INVAL) => Ok(()),
            result => result.map_err(Into::into),
        }
    }
}

/// whether a linux kernel of `release`, as `uname -r` gives it, reports write-back errors to
/// syncfs, which it does from 5.8.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn syncfs_reports_errors(release: &str) -> bool {
    let mut numbers = release.split(['.', '-']).map(str::parse::<u32>);
    match (numbers.next(), numbers.next()) {
        (Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (5, 8),
        _ => false,
    }
}

/// creates a file read-only from the start. the handle that created it may still write, so the
/// file never needs sealing later.
fn create_sealed(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_READONLY: u32 = 1;
        options.attributes(FILE_ATTRIBUTE_READONLY);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o444);
    }
    options.open(path)
}

impl NewObject {
    /// closes the object. with `flush` its bytes reach the disk first, so a power cut cannot
    /// leave it empty; without, the caller flushes a whole batch at once before publishing any.
    pub(crate) fn seal(self, flush: bool) -> io::Result<Sealed> {
        let NewObject {
            file,
            temp,
            hasher,
            written,
            begun,
        } = self;
        // dated before the flush, so the time reaches the disk with the bytes
        if let Err(err) = date_by_birth(&file, begun) {
            tracing::debug!(%err, "could not date an object by its birth");
        }
        if flush {
            file.sync_all()?;
        }
        // a batched object still needs its plain fsync on macos for the batch's `F_FULLFSYNC` to
        // make it durable
        #[cfg(target_os = "macos")]
        if !flush {
            rustix::fs::fsync(&file)?;
        }
        drop(file);
        let (digest, _) = hasher.finish();
        Ok(Sealed {
            temp,
            digest,
            size: written,
        })
    }
}

impl Sealed {
    /// puts the object in place under its hash.
    pub(crate) fn publish(self, cache: &Cache) -> io::Result<Stored> {
        let Sealed {
            mut temp,
            digest,
            size,
        } = self;
        if let Some(staging) = &temp.staging {
            staging.check_member_parent(&temp.path)?;
        }
        let stored = cache.link_object(&temp.path, digest, size, None)?;
        // linking moved or removed the file, and whatever has its name now is not ours to touch
        temp.published = true;
        Ok(stored)
    }
}

impl Write for NewObject {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// how far apart a sealed object's modification and birth times may lie: FAT and exFAT round a
/// modification time up to two seconds.
const SEALED_WITHIN: Duration = Duration::from_secs(2);

/// how far past its start a seal dates an object, the least step a time takes.
const PAST_BIRTH: Duration = Duration::from_nanos(1);

/// dates a sealed object by its birth: its modification time becomes just past the time the
/// file was made, which no later write moves. a write through any link to the object moves the
/// modification time alone, so [`rewritten`] puts the object in doubt, as pnpm finds a changed
/// store file by its time. a filesystem that keeps no birth time leaves the object undated.
///
/// `begun` is the [`begun`] time of a file this process made. an nfs 4.2 server that lets its
/// client hold a file's times through a write delegation keeps the times the client hands back
/// only if they are newer than the ones the file had when the delegation began (RFC 9754), and
/// otherwise dates a written file to when the delegation ends, seconds or minutes later. making
/// a file there sets its times once more, a few milliseconds past its birth, so the seal dates
/// the object past that too.
pub(super) fn date_by_birth(file: &File, begun: Option<SystemTime>) -> io::Result<()> {
    let found = file.metadata()?;
    let born = found.created().ok();
    #[cfg(target_os = "linux")]
    let born = born.or_else(|| statx_birth(file, "", rustix::fs::AtFlags::EMPTY_PATH));
    match born {
        Some(born) => {
            let start = begun.map_or(born, |begun| begun.max(born));
            file.set_times(FileTimes::new().set_modified(start + PAST_BIRTH))
        }
        None => Ok(()),
    }
}

/// when `file`, just made, last had its times or details set: its ctime on unix. on nfs 4.2 a
/// write delegation the making began starts there. windows keeps no such time to read.
pub(super) fn begun(file: &File) -> Option<SystemTime> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let found = file.metadata().ok()?;
        let since = Duration::new(
            u64::try_from(found.ctime()).ok()?,
            u32::try_from(found.ctime_nsec()).ok()?,
        );
        UNIX_EPOCH.checked_add(since)
    }
    #[cfg(windows)]
    {
        let _ = file;
        None
    }
}

/// whether bytes may have been written into the object at `path` since it was sealed: its
/// modification time moved past its birth time, as any write moves it. a time moved back, as a
/// copy that keeps times gives, is no write. a filesystem that keeps no birth time cannot tell,
/// and the answer is no.
pub(crate) fn rewritten(path: &Path, metadata: &Metadata) -> bool {
    let (Some(born), Ok(modified)) = (birth(path, metadata), metadata.modified()) else {
        return false;
    };
    modified
        .duration_since(born)
        .is_ok_and(|later| later > SEALED_WITHIN)
}

/// dates `file`, an object a hash just vouched for, by its birth again, so the next use need not
/// hash it. a write since `seen` was read of it is no longer vouched for and keeps its time. so
/// does an object a project placed as a hardlink: its time is that project's file's, and moving
/// it would look like an edit there.
pub(super) fn redate(file: &File, seen: &Metadata) {
    let unchanged = file
        .metadata()
        .is_ok_and(|now| now.len() == seen.len() && now.modified().ok() == seen.modified().ok());
    if unchanged
        && alone(file)
        && let Err(err) = date_by_birth(file, None)
    {
        tracing::debug!(%err, "could not date an object by its birth");
    }
}

/// whether `file` has no name but its own.
fn alone(file: &File) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        file.metadata().is_ok_and(|found| found.nlink() == 1)
    }
    #[cfg(windows)]
    {
        winapi_util::file::information(file).is_ok_and(|found| found.number_of_links() == 1)
    }
}

/// when the file at `path`, which `metadata` describes, was made. std reads it everywhere but on
/// linux with musl, where statx does. none where the filesystem keeps no birth time.
pub(crate) fn birth(path: &Path, metadata: &Metadata) -> Option<SystemTime> {
    let born = metadata.created().ok();
    #[cfg(target_os = "linux")]
    let born =
        born.or_else(|| statx_birth(rustix::fs::CWD, path, rustix::fs::AtFlags::SYMLINK_NOFOLLOW));
    #[cfg(not(target_os = "linux"))]
    let _ = path;
    born
}

/// the birth time statx reads for `path` under `folder`, when the filesystem keeps one.
#[cfg(target_os = "linux")]
fn statx_birth<P: rustix::path::Arg>(
    folder: impl rustix::fd::AsFd,
    path: P,
    flags: rustix::fs::AtFlags,
) -> Option<SystemTime> {
    use rustix::fs::{StatxFlags, statx};
    let found = statx(folder, path, flags, StatxFlags::BTIME).ok()?;
    if found.stx_mask & StatxFlags::BTIME.bits() == 0 {
        return None;
    }
    let since = Duration::new(
        u64::try_from(found.stx_btime.tv_sec).ok()?,
        found.stx_btime.tv_nsec,
    );
    UNIX_EPOCH.checked_add(since)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::super::tests::{cache, fat};
    use super::*;

    /// an object whose bytes took long to arrive, as a large download's do, is dated by its birth
    /// once sealed, so only a later write moves its time away from it.
    #[test]
    fn sealing_dates_an_object_by_its_birth() {
        let (_dir, cache) = cache();
        let mut object = cache.new_object().unwrap();
        object.write_all(b"slow bytes").unwrap();
        let Some(born) = birth(&object.temp.path, &object.file.metadata().unwrap()) else {
            return;
        };
        object
            .file
            .set_modified(born + std::time::Duration::from_secs(3600))
            .unwrap();
        let stored = object.seal(true).unwrap().publish(&cache).unwrap();
        let path = cache.object_path(&stored.digest);
        assert!(!rewritten(&path, &fs::metadata(&path).unwrap()));
    }

    /// an nfs 4.2 server that lets its client hold a new file's times keeps the time the client
    /// hands back only if it is newer than the birth the file began with, so a seal dates an
    /// object just past its birth.
    #[test]
    fn sealing_dates_an_object_just_past_its_birth() {
        let (_dir, cache) = cache();
        let mut object = cache.new_object().unwrap();
        object.write_all(b"bytes").unwrap();
        let Some(born) = birth(&object.temp.path, &object.file.metadata().unwrap()) else {
            return;
        };
        let stored = object.seal(true).unwrap().publish(&cache).unwrap();
        let path = cache.object_path(&stored.digest);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        // ntfs keeps a time to 100 nanoseconds and fat to far coarser, and both drop the nanosecond
        let fine = born.duration_since(UNIX_EPOCH).unwrap().subsec_nanos() % 100 != 0;
        if fine {
            assert!(modified > born, "{modified:?} is not past {born:?}");
        }
        assert!(!rewritten(&path, &fs::metadata(&path).unwrap()));
    }

    #[test]
    fn objects_never_published_leave_nothing_on_fat() {
        let (_dir, cache) = fat();
        let mut written = cache.new_object().unwrap();
        written.write_all(b"abc").unwrap();
        let mut sealed = cache.new_object().unwrap();
        sealed.write_all(b"def").unwrap();
        let sealed = sealed.seal(true).unwrap();
        drop((written, sealed));
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn only_kernels_that_report_write_back_errors_flush_a_batch_at_once() {
        assert!(syncfs_reports_errors("6.6.87.2-microsoft-standard-WSL2"));
        assert!(syncfs_reports_errors("5.8.0"));
        assert!(syncfs_reports_errors("5.15.0-122-generic"));
        assert!(syncfs_reports_errors("5.14.0-503.el9.aarch64"));
        assert!(!syncfs_reports_errors("5.7.19"));
        assert!(!syncfs_reports_errors("4.18.0-553.el8_10.x86_64"));
        assert!(!syncfs_reports_errors("4.18.0-553.el8_10.aarch64"));
        assert!(!syncfs_reports_errors("3.10.0-1160.el7.x86_64"));
        assert!(!syncfs_reports_errors(""));
        assert!(!syncfs_reports_errors("unknown"));
    }
}
