//! new objects written in one go, such as the members of an archive or a mirror's copies. each is
//! created read-only in `tmp/`, written and hashed in one pass, dated by its birth, flushed and
//! closed, then published through the cache's hardlink-or-rename policy.

use std::fs::{File, FileTimes, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(unix, test))]
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

use super::{Cache, Staging, Stored, partial};
use crate::digest::{Block, Hasher, Sha256};
use crate::stop::Stop;
use crate::volume::{Volume, is_plain};

/// a new object being written into `tmp/`. dropped before it is sealed, it leaves nothing.
pub(crate) struct NewObject {
    file: File,
    temp: Temporary,
    hasher: Hasher,
    written: u64,
    begun: Option<SystemTime>,
}

/// what [`Cache::copy_in`] made of a file another program keeps, such as a mirror's copy.
pub(crate) enum Copied {
    /// the pinned bytes, sealed in `tmp/` and ready to publish.
    Sealed(Sealed),
    /// a file of another size, left unread, or one that grew while it was read.
    OtherSize,
    /// bytes other than the pinned ones, none of which the cache keeps.
    OtherBytes,
    /// a link or a special file, left unopened.
    NotPlain,
    /// the file could not be read: `NotFound` when nothing is there.
    Unread(io::Error),
}

/// what stopped a copy into the cache: reading the file given, or writing the cache's copy.
pub(super) enum Fault {
    Source(io::Error),
    Cache(io::Error),
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
    /// starts a batch of new objects that [`Batch::flush`] flushes with one call, as
    /// [`batch_in`] says.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn batch(&self) -> io::Result<Option<Batch>> {
        batch_in(&self.root.join("tmp"))
    }

    pub(crate) fn new_object(&self) -> io::Result<NewObject> {
        self.new_object_in(&self.root.join("tmp"), None)
    }

    /// a new object holding the file at `source`, which another program keeps, when its bytes are
    /// the ones `digest` and `size` pin. it is cloned where the disk makes reflinks, else read
    /// while hashed, never more than one byte past `size`; a link or a special file is never
    /// opened, and a file of another size never read. `source` is never linked or renamed, so a
    /// later change to it cannot reach the cache. with `flush` the object is on the disk once
    /// sealed; without, the caller flushes a [`Batch`] before publishing it.
    ///
    /// # Errors
    ///
    /// a failure to write the object, and `Interrupted` when `stop` asks. failing to read
    /// `source` is [`Copied::Unread`].
    pub(crate) fn copy_in(
        &self,
        source: &Path,
        digest: &Sha256,
        size: u64,
        flush: bool,
        stop: &Stop,
    ) -> io::Result<Copied> {
        let mut input = match opened(source, size) {
            Ok(input) => input,
            Err(copied) => return Ok(copied),
        };
        let most = size.saturating_add(1);
        let mut object = self.new_object()?;
        #[cfg(target_os = "linux")]
        let cloned = object.clone_from(&input, most, most, stop)?;
        #[cfg(not(target_os = "linux"))]
        let cloned = false;
        if !cloned {
            match object.copy_from(&mut input, most, stop) {
                Ok(()) => {}
                Err(Fault::Source(err)) => return Ok(Copied::Unread(err)),
                Err(Fault::Cache(err)) => return Err(err),
            }
        }
        if object.written != size {
            return Ok(Copied::OtherSize);
        }
        if object.hasher.sha256_so_far() != *digest {
            return Ok(Copied::OtherBytes);
        }
        object.seal(flush).map(Copied::Sealed)
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

/// `source` opened as [`Cache::copy_in`] reads it: a plain file of `size` bytes by its name and by
/// its open handle, never followed through a link. anything else is what `copy_in` makes of it.
fn opened(source: &Path, size: u64) -> Result<File, Copied> {
    let found = std::fs::symlink_metadata(source).map_err(Copied::Unread)?;
    if !is_plain(&found) {
        return Err(Copied::NotPlain);
    }
    if found.len() != size {
        return Err(Copied::OtherSize);
    }
    let input = match partial::read_unfollowed(source) {
        Ok(input) => input,
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => return Err(Copied::NotPlain),
        Err(err) => return Err(Copied::Unread(err)),
    };
    match input.metadata() {
        Ok(found) if found.len() == size => Ok(input),
        Ok(_) => Err(Copied::OtherSize),
        Err(err) => Err(Copied::Unread(err)),
    }
}

/// whether the file at `source`, which another program keeps, holds the bytes `digest` and
/// `size` pin, found as [`Cache::copy_in`] finds it before it keeps a copy, but writing nothing:
/// a link or a special file is never opened, a file of another size never read, and at most one
/// byte past `size` is read, a block at a time. a file that cannot be read holds nothing.
///
/// # Errors
///
/// `Interrupted` when `stop` asks.
pub(crate) fn holds_pinned(
    source: &Path,
    digest: &Sha256,
    size: u64,
    stop: &Stop,
) -> io::Result<bool> {
    let Ok(mut input) = opened(source, size) else {
        return Ok(false);
    };
    let (mut hasher, mut read) = (Hasher::default(), 0_u64);
    let most = size.saturating_add(1);
    let ended = each_block(&mut input, most, Block::sized(most), stop, |block| {
        hasher.update(block);
        read += block.len() as u64;
        Ok(())
    });
    match ended {
        Ok(()) => Ok(read == size && hasher.sha256_so_far() == *digest),
        Err(Fault::Source(_)) => Ok(false),
        Err(Fault::Cache(err)) => Err(err),
    }
}

/// the most files a run flushes one by one rather than with one flush of their filesystem, which
/// also waits for everything other programs wrote there. on a busy disk that flush took 0.4 to
/// 15 s, where 64 small files flushed one by one took 0.2 s. on an idle disk 64 small files cost
/// 3 to 9 ms more one by one, and one large file the same either way.
pub(crate) const MOST_FLUSHED_ALONE: usize = 64;

/// starts a batch of new files in `folder` that [`Batch::flush`] flushes with one call, as git's
/// batch mode does: on linux one flush per small file costs far more than writing it. `None`
/// means each file must be flushed on its own. the batch holds the folder's filesystem open from
/// before its first write, because linux reports a failed write-back only to descriptors opened
/// before the failure, and to syncfs only from 5.8. the cache's new objects take it, and so do
/// the files a push writes into a mirror.
#[cfg(target_os = "linux")]
pub(crate) fn batch_in(folder: &Path) -> io::Result<Option<Batch>> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    if !syncfs_reports_errors(&release) {
        return Ok(None);
    }
    Ok(Some(Batch::open(folder)?))
}

/// on macos fsync only hands a file to the drive, and only `F_FULLFSYNC` empties the drive's
/// cache, for every file at once. each file gets the cheap fsync as it is sealed, and the batch
/// one `F_FULLFSYNC` in `folder`, as git's batch mode does, instead of a full flush per file.
#[cfg(target_os = "macos")]
#[expect(clippy::unnecessary_wraps, reason = "linux may fail to open the batch")]
pub(crate) fn batch_in(folder: &Path) -> io::Result<Option<Batch>> {
    Ok(Some(Batch(folder.to_path_buf())))
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
    pub(crate) fn with_file_for_test(file: File) -> Batch {
        use std::os::unix::fs::MetadataExt as _;

        let device = file.metadata().unwrap().dev();
        Batch { file, device }
    }

    /// whether the batch's flush reaches the file `metadata` describes: it is on the batch's
    /// filesystem.
    pub(crate) fn covers(&self, metadata: &std::fs::Metadata) -> bool {
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
    /// writes what `input` holds into the object, at most `most` bytes, looking for a stop
    /// between blocks.
    pub(super) fn copy_from(
        &mut self,
        input: &mut impl Read,
        most: u64,
        stop: &Stop,
    ) -> Result<(), Fault> {
        each_block(input, most, Block::sized(most), stop, |block| {
            self.write_all(block)
        })
    }

    /// makes the object a clone of `source`, sharing its blocks, and hashes the clone itself, at
    /// most `most` bytes of it, through a buffer for about `expected` bytes. says whether the disk
    /// made the clone; one that makes none leaves the object empty.
    #[cfg(target_os = "linux")]
    pub(super) fn clone_from(
        &mut self,
        source: &File,
        most: u64,
        expected: u64,
        stop: &Stop,
    ) -> io::Result<bool> {
        if let Err(err) = rustix::fs::ioctl_ficlone(&self.file, source) {
            tracing::debug!(%err, "no reflink, so the bytes are copied");
            return Ok(false);
        }
        let mut clone = partial::read_unfollowed(&self.temp.path)?;
        let (hasher, written) = (&mut self.hasher, &mut self.written);
        each_block(&mut clone, most, Block::sized(expected), stop, |block| {
            hasher.update(block);
            *written += block.len() as u64;
            Ok(())
        })
        .map_err(|(Fault::Source(err) | Fault::Cache(err))| err)?;
        Ok(true)
    }

    /// another handle on the object's open file. a flush through it reports what a flush through
    /// the writing handle would, since both share one open file.
    pub(crate) fn handle(&self) -> io::Result<File> {
        self.file.try_clone()
    }

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

/// hands `each` what `input` holds a block at a time, read through `block`, at most `most` bytes
/// in all, and stops between blocks when `stop` asks, which is the cache's side's `Interrupted`.
fn each_block(
    input: &mut impl Read,
    most: u64,
    mut block: Block,
    stop: &Stop,
    mut each: impl FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), Fault> {
    let block = block.get();
    let mut left = most;
    while left > 0 {
        if stop.is_asked() {
            return Err(Fault::Cache(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped at your request",
            )));
        }
        let want = usize::try_from(left).map_or(block.len(), |left| left.min(block.len()));
        let read = match input.read(&mut block[..want]) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(Fault::Source(err)),
        };
        each(&block[..read]).map_err(Fault::Cache)?;
        left -= read as u64;
    }
    Ok(())
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

/// when `file`, just made, last had its times or details set. on nfs 4.2 a write delegation
/// the making began starts there.
pub(super) fn begun(file: &File) -> Option<SystemTime> {
    changed(&file.metadata().ok()?)
}

/// when a file's details last changed, as `metadata` tells: its ctime on unix. windows keeps no
/// such time that std reads.
pub(super) fn changed(metadata: &Metadata) -> Option<SystemTime> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let since = Duration::new(
            u64::try_from(metadata.ctime()).ok()?,
            u32::try_from(metadata.ctime_nsec()).ok()?,
        );
        UNIX_EPOCH.checked_add(since)
    }
    #[cfg(windows)]
    {
        let _ = metadata;
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

    /// a file holding `bytes`, as a mirror keeps one.
    fn kept(folder: &Path, bytes: &[u8]) -> PathBuf {
        let path = folder.join("kept");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn copy_in_keeps_the_pinned_bytes_and_leaves_their_file_alone() {
        let (dir, cache) = cache();
        let source = kept(dir.path(), b"pinned bytes");
        let pin = crate::digest::sha256(b"pinned bytes");
        let copied = cache.copy_in(&source, &pin, 12, true, &Stop::default());
        let Ok(Copied::Sealed(sealed)) = copied else {
            panic!("the pinned bytes were not copied");
        };
        assert_eq!(sealed.publish(&cache).unwrap().digest, pin);
        let object = cache.find(&pin, Some(12)).unwrap();
        assert_eq!(fs::read(object).unwrap(), b"pinned bytes");
        assert_eq!(fs::read(&source).unwrap(), b"pinned bytes");
    }

    #[test]
    fn copy_in_publishes_nothing_when_bytes_differ() {
        let (dir, cache) = cache();
        let source = kept(dir.path(), b"other bytes!");
        let pin = crate::digest::sha256(b"pinned bytes");
        let copied = cache.copy_in(&source, &pin, 12, true, &Stop::default());
        assert!(matches!(copied, Ok(Copied::OtherBytes)));
        assert!(cache.find(&pin, None).is_none());
        assert!(
            cache
                .find(&crate::digest::sha256(b"other bytes!"), None)
                .is_none()
        );
        assert_eq!(fs::read_dir(cache.root().join("tmp")).unwrap().count(), 0);
    }

    /// a file of another size is never opened, so one nobody may read gives the same answer
    /// whether fetchloom runs as root or not.
    #[cfg(unix)]
    #[test]
    fn copy_in_never_opens_a_file_of_another_size() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = cache();
        let source = kept(dir.path(), b"longer than the pin");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o000)).unwrap();
        let pin = crate::digest::sha256(b"pinned");
        let copied = cache.copy_in(&source, &pin, 6, true, &Stop::default());
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(copied, Ok(Copied::OtherSize)));
    }

    /// a file that grows while it is read is read one byte past its pin and no further.
    #[test]
    fn copy_in_stops_one_byte_past_the_pin() {
        struct Counted<'a>(&'a [u8], usize);
        impl io::Read for Counted<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let read = io::Read::read(&mut self.0, buffer)?;
                self.1 += read;
                Ok(read)
            }
        }
        let (_dir, cache) = cache();
        let mut object = cache.new_object().unwrap();
        let mut input = Counted(&[7; 100], 0);
        assert!(object.copy_from(&mut input, 11, &Stop::default()).is_ok());
        assert_eq!((object.written, input.1), (11, 11));
    }

    #[cfg(unix)]
    #[test]
    fn copy_in_refuses_a_link_in_place_of_an_object() {
        let (dir, cache) = cache();
        let target = kept(dir.path(), b"pinned bytes");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let pin = crate::digest::sha256(b"pinned bytes");
        let copied = cache.copy_in(&link, &pin, 12, true, &Stop::default());
        assert!(matches!(copied, Ok(Copied::NotPlain)));
        assert!(cache.find(&pin, None).is_none());
    }

    /// the cache's copy is a file of its own, so a later edit of the mirror's cannot reach it.
    #[cfg(unix)]
    #[test]
    fn a_copied_object_shares_no_inode_with_the_mirror() {
        use std::os::unix::fs::MetadataExt;
        let (dir, cache) = cache();
        let source = kept(dir.path(), b"pinned bytes");
        let pin = crate::digest::sha256(b"pinned bytes");
        let Ok(Copied::Sealed(sealed)) = cache.copy_in(&source, &pin, 12, true, &Stop::default())
        else {
            panic!("the pinned bytes were not copied");
        };
        sealed.publish(&cache).unwrap();
        let object = fs::metadata(cache.object_path(&pin)).unwrap();
        let mirror = fs::metadata(&source).unwrap();
        assert_ne!((object.dev(), object.ino()), (mirror.dev(), mirror.ino()));
        assert_eq!(mirror.nlink(), 1);
    }

    #[test]
    fn a_stopped_copy_leaves_nothing() {
        let (dir, cache) = cache();
        let source = kept(dir.path(), b"pinned bytes");
        let pin = crate::digest::sha256(b"pinned bytes");
        let stop = Stop::default();
        stop.ask();
        let copied = cache.copy_in(&source, &pin, 12, true, &stop);
        assert!(copied.is_err_and(|err| err.kind() == io::ErrorKind::Interrupted));
        assert!(cache.find(&pin, None).is_none());
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
