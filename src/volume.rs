//! the volume under a folder fetchloom writes to, such as the cache or a data folder. FAT32 and
//! exFAT cannot hardlink, and on windows refuse to remove a read-only file, as every object is.

use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions, Permissions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// how a volume links, renames and removes files, and whether it can hardlink at all, learned
/// the first time a removal needs to know.
#[derive(Debug)]
pub(crate) struct Volume {
    /// a folder of fetchloom's own on the volume, where the probe link is made: the cache's
    /// `tmp/`, a project's `.fetchloom`, or the folder `get` fills.
    scratch: PathBuf,
    hard_link: fn(&Path, &Path) -> io::Result<()>,
    rename: fn(&Path, &Path) -> io::Result<()>,
    /// a rename that never replaces a file already at the new name.
    rename_new: fn(&Path, &Path) -> io::Result<()>,
    remove_file: fn(&Path) -> io::Result<()>,
    links: OnceLock<bool>,
    /// for a volume that only pretends, every existing path a removal named.
    pretended: Option<Mutex<Pretended>>,
}

/// the paths a pretending volume would have removed, in order, and as a set a sweep can ask.
#[derive(Debug, Default)]
struct Pretended {
    order: Vec<PathBuf>,
    paths: HashSet<PathBuf>,
}

impl Volume {
    pub(crate) fn new(scratch: PathBuf) -> Volume {
        Volume {
            scratch,
            hard_link: |original, link| fs::hard_link(original, link),
            rename: |from, to| fs::rename(from, to),
            rename_new: |from, to| rename_new(from, to),
            remove_file: |path| fs::remove_file(path),
            links: OnceLock::new(),
            pretended: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_removal(mut self, remove: fn(&Path) -> io::Result<()>) -> Volume {
        self.remove_file = remove;
        self
    }

    /// uses the already pinned folder for capability probes, not a path alias that may move.
    #[cfg(windows)]
    pub(crate) fn set_scratch(&mut self, scratch: &Path) {
        self.scratch = scratch.to_path_buf();
    }

    /// a volume for a dry run: it links, renames and removes nothing, and records each existing
    /// path a removal names, so the code deciding what to remove runs as it would for real.
    pub(crate) fn pretend(scratch: PathBuf) -> Volume {
        Volume {
            scratch,
            hard_link: |_, _| Err(io::Error::other("a dry run links nothing")),
            rename: |_, _| Err(io::Error::other("a dry run renames nothing")),
            rename_new: |_, _| Err(io::Error::other("a dry run renames nothing")),
            remove_file: |path| fs::symlink_metadata(path).map(|_| ()),
            links: OnceLock::new(),
            pretended: Some(Mutex::default()),
        }
    }

    pub(crate) fn is_pretending(&self) -> bool {
        self.pretended.is_some()
    }

    /// the paths a pretending volume would have removed, in order. empty for a real volume.
    pub(crate) fn pretended(&self) -> Vec<PathBuf> {
        self.pretended.as_ref().map_or_else(Vec::new, |paths| {
            paths
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .order
                .clone()
        })
    }

    /// whether a pretending volume already would have removed `path`.
    pub(crate) fn pretends_removed(&self, path: &Path) -> bool {
        self.pretended.as_ref().is_some_and(|paths| {
            paths
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .paths
                .contains(path)
        })
    }

    /// removes the empty folder at `path`.
    pub(crate) fn remove_folder(&self, path: &Path) -> io::Result<()> {
        if self.pretended.is_some() {
            return self.pretend_removal(path);
        }
        fs::remove_dir(path)
    }

    fn pretend_removal(&self, path: &Path) -> io::Result<()> {
        fs::symlink_metadata(path)?;
        if let Some(paths) = &self.pretended {
            let mut pretended = paths.lock().unwrap_or_else(PoisonError::into_inner);
            pretended.order.push(path.to_path_buf());
            pretended.paths.insert(path.to_path_buf());
        }
        Ok(())
    }

    pub(crate) fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        (self.hard_link)(original, link)
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        (self.rename)(from, to)
    }

    /// renames `from` to `to`, refusing a taken name with `AlreadyExists`.
    pub(crate) fn rename_new(&self, from: &Path, to: &Path) -> io::Result<()> {
        (self.rename_new)(from, to)
    }

    /// renames a verified replacement over an old file without leaving its name empty, clearing a
    /// read-only flag windows refuses it for and restoring it through the same handle, for placed
    /// hardlinks. where that fails, as on fat, the old file waits aside until the new has its name.
    pub(crate) fn replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        // FAT and exFAT on windows answer a rename over a read-only file with "already exists"
        let refused = match self.rename(from, to) {
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::AlreadyExists
                ) =>
            {
                err
            }
            renamed => return renamed,
        };
        let file = open_attributes(to)?;
        let metadata = file.metadata()?;
        let before = metadata.permissions();
        if !is_plain(&metadata) || !before.readonly() {
            return Err(refused);
        }
        file.set_permissions(writable(before.clone()))?;
        if self.can_hardlink() == Some(false) {
            // FAT and exFAT: an open handle blocks renaming over the old file, and some refuse to
            // rename over a file at all. they make no hardlinks, so the cleared flag reaches no
            // other name
            drop(file);
            if self.rename(from, to).is_ok() {
                return Ok(());
            }
            return self.replace_aside(from, to, &before);
        }
        // a hardlink placed in a project may still name the old file, so it gets its flag back
        let renamed = self.rename(from, to);
        let restored = file.set_permissions(before);
        renamed.and(restored)
    }

    /// replaces `to` with `from` by moving the old file aside first, and back when `from` cannot
    /// take its name, sealed again with the permissions it had, `before`.
    fn replace_aside(&self, from: &Path, to: &Path, before: &Permissions) -> io::Result<()> {
        let reseal = || {
            if let Ok(file) = open_attributes(to) {
                let _ = file.set_permissions(before.clone());
            }
        };
        let aside = self
            .scratch
            .join(format!(".fetchloom-{:016x}.old", fastrand::u64(..)));
        if let Err(err) = self.rename(to, &aside) {
            reseal();
            return Err(err);
        }
        match self.rename(from, to) {
            Ok(()) => {
                let _ = (self.remove_file)(&aside);
                Ok(())
            }
            Err(err) => {
                // a sealed object another writer published meanwhile refuses the move back, and
                // the old bytes then stay aside, where gc finds them
                if self.rename(&aside, to).is_ok() {
                    reseal();
                }
                Err(err)
            }
        }
    }

    /// removes the file at `path`. a read-only file refused on a volume that cannot hardlink, as
    /// fat on windows, has no other name, so it is made writable for a second try; elsewhere the
    /// refusal stands, as the flag is shared with every hardlink of an object.
    pub(crate) fn remove(&self, path: &Path) -> io::Result<()> {
        if self.pretended.is_some() {
            return self.pretend_removal(path);
        }
        match (self.remove_file)(path) {
            Err(refused) if refused.kind() == io::ErrorKind::PermissionDenied => {
                self.remove_read_only(path).unwrap_or(Err(refused))
            }
            removed => removed,
        }
    }

    /// removes the file at `path`, which fetchloom recorded as a hardlink of a cached object. its
    /// read-only flag is the object's too, so it is never cleared, whatever the volume.
    #[cfg(windows)]
    pub(crate) fn remove_hardlink(&self, path: &Path) -> io::Result<()> {
        if self.pretended.is_some() {
            return self.pretend_removal(path);
        }
        (self.remove_file)(path)
    }

    /// clears the read-only flag of the plain file at `path`, through a handle that never follows a
    /// link, and removes it, putting the flag back when that fails too. none when the file is not
    /// a read-only plain file or may have another name.
    fn remove_read_only(&self, path: &Path) -> Option<io::Result<()>> {
        let file = open_attributes(path).ok()?;
        let before = file.metadata().ok().filter(is_plain)?.permissions();
        if !before.readonly() || self.can_hardlink() != Some(false) {
            return None;
        }
        file.set_permissions(writable(before.clone())).ok()?;
        let removed = (self.remove_file)(path);
        if removed.is_err() {
            let _ = file.set_permissions(before);
        }
        Some(removed)
    }

    /// whether the volume can hardlink, learned once by linking a new hidden file in the scratch
    /// folder. none when the probe failed in a way that says nothing about links.
    pub(crate) fn can_hardlink(&self) -> Option<bool> {
        if let Some(links) = self.links.get() {
            return Some(*links);
        }
        let [probe, link] = ["probe", "link"].map(|ending| {
            self.scratch
                .join(format!(".fetchloom-{:016x}.{ending}", fastrand::u64(..)))
        });
        create_hidden(&probe).ok()?;
        let linked = (self.hard_link)(&probe, &link);
        let _ = fs::remove_file(&probe);
        let links = match linked {
            Ok(()) => {
                let _ = fs::remove_file(&link);
                true
            }
            Err(err) if refuses_links(&err) => false,
            Err(_) => return None,
        };
        Some(*self.links.get_or_init(|| links))
    }
}

/// whether `err`, from making a reflink or a hardlink, says the volume makes no such link: FAT32
/// and exFAT answer code 1, `ERROR_INVALID_FUNCTION` or `EPERM`, though `EPERM` may refuse one file
/// alone under linux's `protected_hardlinks`, which only a probe of fetchloom's own tells apart.
pub(crate) fn refuses_links(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Unsupported || err.raw_os_error().is_some_and(refusal_code)
}

/// whether `err` says the two files sit on different volumes, as a link from a cache on another
/// drive does.
pub(crate) fn crosses_volumes(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        const NOT_SAME_DEVICE: u32 = 17;
        if err
            .raw_os_error()
            .is_some_and(|code| win32(code) == NOT_SAME_DEVICE)
        {
            return true;
        }
    }
    err.kind() == io::ErrorKind::CrossesDevices
}

/// `ERROR_INVALID_FUNCTION` and `ERROR_NOT_SUPPORTED`, the codes of a windows volume that cannot
/// link or clone at all.
#[cfg(windows)]
fn refusal_code(code: i32) -> bool {
    const INVALID_FUNCTION: u32 = 1;
    const NOT_SUPPORTED: u32 = 50;
    matches!(win32(code), INVALID_FUNCTION | NOT_SUPPORTED)
}

/// the windows error code `code` carries. reflink-copy reports windows errors as HRESULTs, which
/// keep the code in their low 16 bits.
#[cfg(windows)]
fn win32(code: i32) -> u32 {
    let code = code.cast_unsigned();
    if code >> 16 == 0x8007 {
        code & 0xffff
    } else {
        code
    }
}

/// `EPERM`. linux reports the other refusals with codes std already calls unsupported.
#[cfg(not(windows))]
fn refusal_code(code: i32) -> bool {
    code == 1
}

/// makes a new empty file that file browsers do not show: windows hides it by its attribute,
/// other systems by the dot its name starts with.
pub(crate) fn create_hidden(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 2;
        options.attributes(FILE_ATTRIBUTE_HIDDEN);
    }
    options.open(path)
}

/// makes the plain file at `path` read-only for everyone, or writable by its owner again, through
/// a handle that never follows a link swapped in for it.
pub(crate) fn set_read_only(path: &Path, read_only: bool) -> io::Result<()> {
    let file = open_attributes(path)?;
    let metadata = file.metadata()?;
    if !is_plain(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a plain file", path.display()),
        ));
    }
    let permissions = metadata.permissions();
    if permissions.readonly() == read_only {
        return Ok(());
    }
    file.set_permissions(if read_only {
        let mut permissions = permissions;
        permissions.set_readonly(true);
        permissions
    } else {
        writable(permissions)
    })
}

/// opens `path` only to read and change its attributes, never following a link at its end.
fn open_attributes(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        // a fifo would hold an open for reading until something wrote to it
        options
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
    }
    options.open(path)
}

/// whether `metadata` describes a plain file, not a link, a folder or a device.
pub(crate) fn is_plain(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}

/// opens `path` to read and write without following a link at its end, and refuses anything
/// that is not a plain file. `create` makes a missing file.
pub(crate) fn open_unfollowed(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    open_plain(path, &mut options)
}

/// shares the no-follow checks so reading an object never needs write access to its bytes.
pub(crate) fn read_unfollowed(path: &Path) -> io::Result<File> {
    open_plain(path, OpenOptions::new().read(true))
}

/// opens an object to hash it and then date it again: to read, and on windows also to set its
/// times, which a read-only file allows. unix lets the owner set times through any handle.
pub(crate) fn open_to_redate(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const GENERIC_READ: u32 = 0x8000_0000;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        open_plain(
            path,
            OpenOptions::new().access_mode(GENERIC_READ | FILE_WRITE_ATTRIBUTES),
        )
    }
    #[cfg(not(windows))]
    read_unfollowed(path)
}

fn open_plain(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        // a fifo must not block opening before its handle can be refused
        options.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // a link opens as itself rather than as what it points at, and is refused below
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a link or a special file, which fetchloom never reads or writes",
                path.display()
            ),
        )
    };
    let opened = options.open(path);
    // what unix answers when the name is a link
    #[cfg(unix)]
    let opened = opened.map_err(|err| {
        if err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
            refused()
        } else {
            err
        }
    });
    let file = opened?;
    if is_plain(&file.metadata()?) {
        Ok(file)
    } else {
        Err(refused())
    }
}

/// flushes directory entries on unix, where flushing a file does not publish its name durably.
/// other platforms keep their own publication behavior.
#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "unix directory flush errors share this platform-independent interface"
    )
)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// the most files a run flushes one by one rather than with one flush of their filesystem, which
/// also waits for everything other programs wrote there. on a busy disk that flush took 0.4 to
/// 15 s, where 64 small files flushed one by one took 0.2 s. on an idle disk 64 small files cost
/// 3 to 9 ms more one by one, and one large file the same either way.
pub(crate) const MOST_FLUSHED_ALONE: usize = 64;

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

/// renames `from` to `to`, refusing a taken name with `AlreadyExists`. a disk that can neither
/// link nor rename that way fails with its refusal of the link.
#[cfg(unix)]
pub(crate) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    rename_new_at(rustix::fs::CWD, from, rustix::fs::CWD, to, false)
}

/// renames `from` in `from_parent` to `to` in `to_parent` without replacing anything: by the
/// rename linux and macos have that refuses a taken name, else, or with `fallback`, by a link to
/// the new name and the old name's removal.
#[cfg(unix)]
pub(crate) fn rename_new_at<P: rustix::path::Arg + Copy, Q: rustix::path::Arg + Copy>(
    from_parent: std::os::fd::BorrowedFd<'_>,
    from: P,
    to_parent: std::os::fd::BorrowedFd<'_>,
    to: Q,
    fallback: bool,
) -> io::Result<()> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = fallback;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if !fallback {
        match rustix::fs::renameat_with(
            from_parent,
            from,
            to_parent,
            to,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => return Ok(()),
            Err(error) if !lacks_rename_new(error) => return Err(error.into()),
            Err(_) => {}
        }
    }
    rustix::fs::linkat(
        from_parent,
        from,
        to_parent,
        to,
        rustix::fs::AtFlags::empty(),
    )?;
    rustix::fs::unlinkat(from_parent, from, rustix::fs::AtFlags::empty()).map_err(Into::into)
}

/// whether `error`, from a rename asked never to replace, says the system or the filesystem has
/// no such rename.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn lacks_rename_new(error: rustix::io::Errno) -> bool {
    use rustix::io::Errno;
    matches!(
        error,
        Errno::NOSYS | Errno::NOTSUP | Errno::INVAL | Errno::NOTDIR
    )
}

/// renames `from` to `to`, refusing a taken name with `AlreadyExists`: by a second link and the
/// old name's removal, since std's rename replaces, or on fat and exfat, which make no links, by
/// tempfile's rename, which clears attributes, so a read-only file is made read-only again.
#[cfg(windows)]
pub(crate) fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    match fs::hard_link(from, to) {
        Ok(()) => fs::remove_file(from).inspect_err(|_| {
            let _ = fs::remove_file(to);
        }),
        Err(error) if refuses_links(&error) => {
            let read_only = fs::symlink_metadata(from)?.permissions().readonly();
            let mut named = tempfile::TempPath::try_from_path(from)?;
            // the file is someone's, so a failed rename must not delete it
            named.disable_cleanup(true);
            let renamed = named.persist_noclobber(to).map_err(|error| error.error);
            if read_only {
                let now = if renamed.is_ok() { to } else { from };
                let _ = set_read_only(now, true);
            }
            renamed
        }
        Err(error) => Err(error),
    }
}

/// `permissions` that let the file's owner write it.
fn writable(mut permissions: Permissions) -> Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "windows has one read-only flag, not write bits for other users"
    )]
    permissions.set_readonly(false);
    permissions
}

#[cfg(test)]
impl Volume {
    /// a volume that refuses to remove a read-only file, as FAT32 and exFAT do on windows, and
    /// that hardlinks only when `links` says so, which FAT cannot.
    pub(crate) fn refusing(scratch: PathBuf, links: bool) -> Volume {
        let hard_link: fn(&Path, &Path) -> io::Result<()> = if links {
            |original, link| fs::hard_link(original, link)
        } else {
            // what FAT32 and exFAT answer
            |_, _| Err(io::Error::from_raw_os_error(1))
        };
        Volume {
            scratch,
            hard_link,
            rename: |from, to| {
                if fs::symlink_metadata(to).is_ok_and(|metadata| metadata.permissions().readonly())
                {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                fs::rename(from, to)
            },
            rename_new: |from, to| rename_new(from, to),
            remove_file: |path| {
                if fs::symlink_metadata(path)?.permissions().readonly() {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                fs::remove_file(path)
            },
            links: OnceLock::new(),
            pretended: None,
        }
    }

    /// this volume, losing every rename to another writer, which publishes the same bytes at the
    /// destination just before, read-only and dated [`other_writers_time`]: a rename that replaces
    /// takes the name from it, as on linux and macos, and one that never replaces is refused.
    pub(crate) fn losing_renames(self) -> Volume {
        Volume {
            rename: |from, to| {
                publish_first(from, to)?;
                fs::rename(from, to)
            },
            rename_new: |from, to| {
                publish_first(from, to)?;
                rename_new(from, to)
            },
            ..self
        }
    }

    /// this volume without a rename that refuses a taken name: asked for one, it answers as its
    /// link refuses, as a disk without either does once linux falls back to a link.
    pub(crate) fn replacing_only(self) -> Volume {
        Volume {
            rename_new: |_, _| Err(io::Error::from_raw_os_error(1)),
            ..self
        }
    }
}

/// when the other writer of [`Volume::losing_renames`] dated the file it published.
#[cfg(test)]
pub(crate) fn other_writers_time() -> std::time::SystemTime {
    // 2001 began on an even second, which FAT's two-second clock keeps
    std::time::UNIX_EPOCH + std::time::Duration::from_hours(271_752)
}

/// another writer publishing the bytes of `from` at `to`, as [`Volume::losing_renames`] has it.
#[cfg(test)]
fn publish_first(from: &Path, to: &Path) -> io::Result<()> {
    fs::write(to, fs::read(from)?)?;
    OpenOptions::new()
        .write(true)
        .open(to)?
        .set_modified(other_writers_time())?;
    crate::cache::tests::seal(to);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{hardlinks, hold, seal};

    /// a read-only file, as every object is.
    fn sealed(path: &Path) {
        fs::write(path, b"abc").unwrap();
        seal(path);
    }

    fn volume(dir: &Path, links: bool) -> Volume {
        let tmp = dir.join("tmp");
        fs::create_dir(&tmp).unwrap();
        Volume::refusing(tmp, links)
    }

    fn probes_left(dir: &Path) -> usize {
        fs::read_dir(dir.join("tmp")).unwrap().count()
    }

    /// a dry run removes nothing and keeps each removal, in the order made, for a sweep to ask
    /// about. the names go in out of sorted order, so a sorted list could not pass for that order.
    #[test]
    fn a_dry_run_preserves_removal_membership_order_and_files() {
        let dir = tempfile::tempdir().unwrap();
        let volume = Volume::pretend(dir.path().to_path_buf());
        let paths: Vec<PathBuf> = ["object2", "object0", "object1"]
            .iter()
            .map(|name| dir.path().join(name))
            .collect();
        for path in &paths {
            fs::write(path, b"").unwrap();
            volume.remove(path).unwrap();
        }
        for path in &paths {
            assert!(volume.pretends_removed(path));
        }
        assert!(!volume.pretends_removed(&dir.path().join("other")));
        assert_eq!(volume.pretended(), paths, "in the order removed");
        assert!(paths.iter().all(|path| path.exists()), "nothing removed");
    }

    #[test]
    fn a_volume_without_hardlinks_removes_a_read_only_file() {
        let dir = tempfile::tempdir().unwrap();
        let fat = volume(dir.path(), false);
        let object = dir.path().join("object");
        sealed(&object);
        fat.remove(&object).unwrap();
        assert!(!object.exists());
        assert_eq!(probes_left(dir.path()), 0);
    }

    #[test]
    fn a_volume_with_hardlinks_keeps_the_flag_a_placed_link_shares() {
        let dir = tempfile::tempdir().unwrap();
        // FAT32 and exFAT cannot hardlink, so no placed file can share an object there
        if !hardlinks(dir.path()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let volume = volume(dir.path(), true);
        let object = dir.path().join("object");
        sealed(&object);
        let placed = dir.path().join("placed.csv");
        fs::hard_link(&object, &placed).unwrap();
        let refused = volume.remove(&object).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(object.exists());
        assert!(fs::metadata(&placed).unwrap().permissions().readonly());
        assert_eq!(probes_left(dir.path()), 0);
    }

    /// fat on windows may rename over no file at all, so the old file moves aside first and, when
    /// the new one cannot take its name, goes back, read-only as it was.
    #[test]
    fn a_replacement_on_fat_keeps_the_old_file_until_the_new_one_has_its_name() {
        let renaming_over_nothing: fn(&Path, &Path) -> io::Result<()> = |from, to| {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            fs::rename(from, to)
        };
        let refusing_the_new_file: fn(&Path, &Path) -> io::Result<()> = |from, to| {
            if fs::symlink_metadata(to).is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            if from.file_name().is_some_and(|name| name == "new") {
                return Err(io::Error::other("the disk refused the new file"));
            }
            fs::rename(from, to)
        };
        for (rename, replaced) in [
            (refusing_the_new_file, false),
            (renaming_over_nothing, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let fat = Volume {
                rename,
                ..volume(dir.path(), false)
            };
            let old = dir.path().join("object");
            sealed(&old);
            let new = dir.path().join("new");
            fs::write(&new, b"new").unwrap();
            seal(&new);

            let result = fat.replace(&new, &old);

            assert_eq!(result.is_ok(), replaced, "{result:?}");
            let expected: &[u8] = if replaced { b"new" } else { b"abc" };
            assert_eq!(fs::read(&old).unwrap(), expected);
            assert_eq!(new.exists(), !replaced);
            if !replaced {
                assert!(fs::metadata(&old).unwrap().permissions().readonly());
            }
            assert_eq!(probes_left(dir.path()), 0, "nothing is left aside");
        }
    }

    #[test]
    fn a_file_refused_again_stays_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let fat = volume(dir.path(), false);
        let folder = dir.path().join("objects");
        fs::create_dir(&folder).unwrap();
        let object = folder.join("object");
        sealed(&object);
        let held = hold(&object);
        let removed = fat.remove(&object).is_ok();
        drop(held);
        // root on unix removes it anyway
        assert_eq!(removed, !object.exists());
        if !removed {
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
        }
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
