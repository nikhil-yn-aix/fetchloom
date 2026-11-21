//! a folder of the cache held open from the cache's root without following a link, so the files
//! made, opened, renamed and removed in it stay in it, whatever anyone who can write the cache
//! puts at its name meanwhile. unix holds a handle; windows reaches the folder by its path, with
//! the folders from the cache's root down to it held, so none is renamed or replaced meanwhile.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use super::sharing::Sharing;
use crate::place::{FolderUse, found_folder};

/// a folder below the cache's root, and where it is.
#[derive(Debug)]
pub(super) struct Folder {
    #[cfg(unix)]
    handle: File,
    #[cfg(windows)]
    _held: Held,
    root: PathBuf,
    below: PathBuf,
    /// what the group the cache is shared with gets of each file made in the folder.
    #[cfg(unix)]
    sharing: Sharing,
}

/// on windows, the folders from the cache's root down to a [`Folder`], held while it lives.
#[cfg(windows)]
struct Held {
    _folders: Box<dyn std::any::Any + Send + Sync>,
}

#[cfg(windows)]
impl std::fmt::Debug for Held {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Held")
    }
}

impl Folder {
    /// opens the folder `below` the cache's `root`, making what is missing and refusing a link or a
    /// file where a folder should be. `root` itself may be a link, as a cache folder someone named.
    /// each folder and file made gets what `sharing` gives the group the cache is shared with.
    pub(super) fn open(root: &Path, below: &Path, sharing: Sharing) -> io::Result<Folder> {
        Folder::reach(root, below, true, sharing)
    }

    /// [`Folder::open`], making nothing: a missing folder is `NotFound`.
    pub(super) fn existing(root: &Path, below: &Path, sharing: Sharing) -> io::Result<Folder> {
        Folder::reach(root, below, false, sharing)
    }

    /// the folder `below` the cache's `root` that `user` alone may enter, as their own records in
    /// a cache others write are kept, its parent already there. with `make`, a missing one is
    /// made for its owner alone. one that is another user's, or that others may enter, is none of
    /// `user`'s, since others may have written what it holds: `NotFound`, as a missing one is.
    #[cfg(unix)]
    pub(super) fn private(root: &Path, below: &Path, user: u32, make: bool) -> io::Result<Folder> {
        use std::os::unix::fs::MetadataExt;

        use rustix::fs::{Mode, OFlags};
        let (Some(parent), Some(name)) = (below.parent(), below.file_name()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a private folder of the cache needs a folder to sit in",
            ));
        };
        let parent = walk(root, parent, None)?;
        if make {
            match rustix::fs::mkdirat(&parent, name, Mode::from_bits_truncate(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(err) => return Err(err.into()),
            }
        }
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let handle = File::from(rustix::fs::openat(&parent, name, flags, Mode::empty())?);
        let found = handle.metadata()?;
        if found.uid() != user || found.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} is another user's or lets others in, so what it holds may be anyone's",
                    root.join(below).display()
                ),
            ));
        }
        Ok(Folder {
            handle,
            root: root.to_path_buf(),
            below: below.to_path_buf(),
            sharing: Sharing::default(),
        })
    }

    fn reach(root: &Path, below: &Path, make: bool, sharing: Sharing) -> io::Result<Folder> {
        #[cfg(unix)]
        let handle = walk(root, below, make.then_some(sharing))?;
        #[cfg(not(unix))]
        {
            let _ = sharing;
            checked(root, below, make)?;
        }
        Ok(Folder {
            #[cfg(unix)]
            handle,
            #[cfg(windows)]
            _held: Held {
                _folders: Box::new(crate::place::hold_below(
                    root,
                    &root.join(below).join("member"),
                )?),
            },
            below: below.to_path_buf(),
            root: root.to_path_buf(),
            #[cfg(unix)]
            sharing,
        })
    }

    pub(super) fn path(&self) -> PathBuf {
        self.root.join(&self.below)
    }

    /// where the file `name` in the folder is, for messages.
    pub(super) fn join(&self, name: &str) -> PathBuf {
        self.path().join(name)
    }

    /// fails when the folder's path no longer leads to the folder held, as when someone swapped it
    /// for a link, so what was written in it is not trusted.
    pub(super) fn check(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let now = rustix::fs::fstat(walk(&self.root, &self.below, None)?)?;
            let held = rustix::fs::fstat(&self.handle)?;
            if (now.st_dev, now.st_ino) == (held.st_dev, held.st_ino) {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} was replaced while fetchloom wrote in it, and the cache keeps only its own folders",
                    self.path().display()
                ),
            ))
        }
        #[cfg(not(unix))]
        checked(&self.root, &self.below, false)
    }

    /// makes the file `name`, to read and write, failing when one is there.
    pub(super) fn create_new(&self, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            self.create_new_with(name, 0o666)
        }
        #[cfg(not(unix))]
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.join(name))
    }

    /// [`Folder::create_new`] with the permissions `mode` before the umask takes its part, and the
    /// group the cache is shared with given its own.
    #[cfg(unix)]
    pub(super) fn create_new_with(&self, name: &str, mode: u32) -> io::Result<File> {
        use rustix::fs::{Mode, OFlags};
        #[cfg(target_os = "macos")]
        let mode = u16::try_from(mode).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "a file mode is out of range")
        })?;
        let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
        let mode = Mode::from_bits_truncate(mode);
        let file: File =
            rustix::fs::openat(&self.handle, name, flags | OFlags::CLOEXEC, mode)?.into();
        self.sharing.share(&file);
        Ok(file)
    }

    /// the names the folder holds.
    pub(super) fn list(&self) -> io::Result<Vec<std::ffi::OsString>> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(names_at(&self.handle)?
                .into_iter()
                .map(|name| std::ffi::OsString::from_vec(name.into_bytes()))
                .collect())
        }
        #[cfg(not(unix))]
        std::fs::read_dir(self.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    }

    /// gives the file `name` the name `to` in the folder `into`, replacing a file there.
    pub(super) fn rename_into(&self, name: &str, into: &Folder, to: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            Ok(rustix::fs::renameat(&self.handle, name, &into.handle, to)?)
        }
        #[cfg(not(unix))]
        {
            let to = into.join(to);
            crate::volume::stall::naming(&to, || std::fs::rename(self.join(name), &to))
        }
    }

    /// flushes the folder's names, which flushing a file does not make last on unix.
    #[cfg(unix)]
    pub(super) fn flush(&self) -> io::Result<()> {
        self.handle.sync_all()
    }

    /// opens the plain file `name` to read and write, never through a link nor when it has another
    /// name.
    pub(super) fn open_file(&self, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            self.open_plain(name, rustix::fs::OFlags::RDWR)
        }
        #[cfg(not(unix))]
        {
            let file = crate::volume::open_unfollowed(&self.join(name), false)?;
            #[cfg(windows)]
            if winapi_util::file::information(&file)?.number_of_links() > 1 {
                return Err(linked(&self.join(name)));
            }
            Ok(file)
        }
    }

    /// [`Folder::open_file`] of `name`, making it where it is missing, in two steps that writers
    /// making the name at once cannot trip over, as [`open_or_make`] says.
    pub(super) fn open_or_make(&self, name: &str) -> io::Result<File> {
        open_or_make(|| self.open_file(name), || self.create_new(name))
    }

    /// opens the plain file `name` to read, never through a link.
    pub(super) fn read_file(&self, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            self.open_plain(name, rustix::fs::OFlags::RDONLY)
        }
        #[cfg(not(unix))]
        crate::volume::read_unfollowed(&self.join(name))
    }

    /// the bytes of the plain file `name`, never read through a link, when they are at most
    /// `most`.
    pub(super) fn read(&self, name: &str, most: u64) -> io::Result<Vec<u8>> {
        crate::volume::read_at_most(self.read_file(name)?, most)
    }

    /// whether `name` still names the file `file` has open.
    pub(super) fn names(&self, name: &str, file: &File) -> io::Result<bool> {
        #[cfg(unix)]
        {
            use rustix::fs::AtFlags;
            let held = rustix::fs::fstat(file)?;
            match rustix::fs::statat(&self.handle, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(named) => Ok((named.st_dev, named.st_ino) == (held.st_dev, held.st_ino)),
                Err(rustix::io::Errno::NOENT) => Ok(false),
                Err(err) => Err(err.into()),
            }
        }
        #[cfg(not(unix))]
        super::partial::names(&self.join(name), file)
    }

    /// whether `name` is gone from the folder.
    pub(super) fn lacks(&self, name: &str) -> bool {
        #[cfg(unix)]
        {
            use rustix::fs::AtFlags;
            let found = rustix::fs::statat(&self.handle, name, AtFlags::SYMLINK_NOFOLLOW);
            matches!(found, Err(rustix::io::Errno::NOENT))
        }
        #[cfg(not(unix))]
        std::fs::symlink_metadata(self.join(name))
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
    }

    /// removes the file or link `name`, as itself.
    pub(super) fn remove(&self, name: &str) -> io::Result<()> {
        #[cfg(unix)]
        {
            let empty = rustix::fs::AtFlags::empty();
            Ok(rustix::fs::unlinkat(&self.handle, name, empty)?)
        }
        #[cfg(not(unix))]
        std::fs::remove_file(self.join(name))
    }

    /// puts `bytes` in the file `name` in one step: written to a new file in `scratch` first,
    /// then renamed over it.
    pub(super) fn replace(&self, name: &str, bytes: &[u8], scratch: &Folder) -> io::Result<()> {
        let (temporary, mut file) = loop {
            let temporary = format!("{:016x}.tmp", fastrand::u64(..));
            match scratch.create_new(&temporary) {
                Ok(file) => break (temporary, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
        };
        let written = file.write_all(bytes);
        drop(file);
        written
            .and_then(|()| scratch.rename_into(&temporary, self, name))
            .inspect_err(|_| {
                let _ = scratch.remove(&temporary);
            })
    }

    #[cfg(unix)]
    fn open_plain(&self, name: &str, flags: rustix::fs::OFlags) -> io::Result<File> {
        use std::os::unix::fs::MetadataExt;

        use rustix::fs::{Mode, OFlags};
        // a fifo must not block opening before its handle can be refused
        let open = flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let mode = Mode::from_bits_truncate(0o666);
        let file = File::from(rustix::fs::openat(&self.handle, name, open, mode).map_err(
            |err| {
                if err == rustix::io::Errno::LOOP {
                    refused(&self.join(name))
                } else {
                    err.into()
                }
            },
        )?);
        let found = file.metadata()?;
        if !found.is_file() {
            return Err(refused(&self.join(name)));
        }
        // where anyone may link any file, a co-user's hardlink would take the writes elsewhere
        if flags.contains(OFlags::RDWR) && found.nlink() > 1 {
            return Err(linked(&self.join(name)));
        }
        Ok(file)
    }
}

/// the refusal to write the file at `path`, which has another name.
#[cfg(any(unix, windows))]
fn linked(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} has another name, as a hardlink to a file elsewhere does, so fetchloom never writes it",
            path.display()
        ),
    )
}

/// opens a file with `open`, or where it is missing makes it with `make`, which must refuse a
/// taken name: a writer whose make is refused, as another writer made the file in between, opens
/// that file. two saves into one url shard at once failed as not found on macos while each opened
/// the shard's lock file with one call that creates it. each step is tried a few times at most.
fn open_or_make(
    open: impl Fn() -> io::Result<File>,
    make: impl Fn() -> io::Result<File>,
) -> io::Result<File> {
    let mut tries = 0;
    loop {
        match open() {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            opened => return opened,
        }
        match make() {
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound
                ) && tries < 8 =>
            {
                tries += 1;
            }
            answer => return answer,
        }
    }
}

#[cfg(unix)]
fn refused(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{} is a link or a special file, which fetchloom never reads or writes",
            path.display()
        ),
    )
}

/// the folder `below` `root`, opened folder by folder without following a link. with `make`,
/// a missing one is made, and gets what that sharing gives the group the cache is shared with.
#[cfg(unix)]
pub(super) fn walk(root: &Path, below: &Path, make: Option<Sharing>) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    use rustix::io::Errno;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut folder = File::open(root)?;
    let mut path = root.to_path_buf();
    for part in below.components() {
        let Component::Normal(name) = part else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a cache folder must stay below the cache's root",
            ));
        };
        path.push(name);
        let mut share = None;
        let opened = match rustix::fs::openat(&folder, name, flags, Mode::empty()) {
            Err(Errno::NOENT) if make.is_some() => {
                match rustix::fs::mkdirat(&folder, name, Mode::from_bits_truncate(0o777)) {
                    Ok(()) => share = make,
                    Err(Errno::EXIST) => {}
                    Err(err) => return Err(err.into()),
                }
                rustix::fs::openat(&folder, name, flags, Mode::empty())
            }
            opened => opened,
        };
        folder = match opened {
            Ok(opened) => {
                let opened = File::from(opened);
                if let Some(sharing) = share {
                    sharing.share(&opened);
                }
                opened
            }
            Err(Errno::LOOP | Errno::NOTDIR) => {
                found_folder(&path, &std::fs::symlink_metadata(&path)?, FolderUse::Cache)?;
                return Err(Errno::NOTDIR.into());
            }
            Err(err) => return Err(err.into()),
        };
    }
    Ok(folder)
}

/// checks that each folder from `root` down to `below` is a real one, making one where missing
/// when `make` says so.
#[cfg(not(unix))]
fn checked(root: &Path, below: &Path, make: bool) -> io::Result<()> {
    let mut path = root.to_path_buf();
    for part in below.components() {
        let Component::Normal(name) = part else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a cache folder must stay below the cache's root",
            ));
        };
        path.push(name);
        if make {
            crate::place::real_folder(&path, FolderUse::Cache)?;
        } else {
            found_folder(&path, &std::fs::symlink_metadata(&path)?, FolderUse::Cache)?;
        }
    }
    Ok(())
}

/// the names in the folder `folder` holds open.
#[cfg(unix)]
fn names_at(folder: &File) -> io::Result<Vec<std::ffi::CString>> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(folder)? {
        let entry = entry?;
        if entry.file_name() != c"." && entry.file_name() != c".." {
            names.push(entry.file_name().to_owned());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::io::{Seek, SeekFrom};

    use super::*;

    /// a record file with a second name, as a co-user's hardlink to someone's own file would give
    /// where the system lets anyone link any file, as windows does, is never opened to write, so
    /// no record lands in that file; reading it is harmless.
    #[test]
    fn a_record_with_a_second_name_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let urls = Folder::open(dir.path(), Path::new("urls"), Sharing::default()).unwrap();
        let theirs = dir.path().join(".bashrc");
        std::fs::write(&theirs, b"someone's own file\n").unwrap();
        if std::fs::hard_link(&theirs, urls.join("0.log")).is_err() {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }

        let opened = urls.open_or_make("0.log");

        assert_eq!(
            opened.err().map(|err| err.kind()),
            Some(io::ErrorKind::InvalidInput)
        );
        assert_eq!(std::fs::read(&theirs).unwrap(), b"someone's own file\n");
        assert!(urls.read_file("0.log").is_ok());
    }

    /// on windows, where the cache's folders are reached by path, the folders from the cache's root
    /// down to a folder of it stay held while it is open, so no one renames one and puts a junction
    /// in its place.
    #[cfg(windows)]
    #[test]
    fn a_folder_of_the_cache_cannot_be_swapped_while_it_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let shard = Folder::open(dir.path(), Path::new("partial/ab"), Sharing::default()).unwrap();
        let aside = dir.path().join("aside");

        for held in [shard.path(), dir.path().join("partial")] {
            let renamed = std::fs::rename(&held, &aside);
            assert!(renamed.is_err(), "{} was renamed", held.display());
        }
        drop(shard);
        std::fs::rename(dir.path().join("partial"), &aside).unwrap();
    }

    /// a writer that finds the file missing, and is refused making it because another writer made
    /// it in between, opens that writer's file, so what both write lands in one file.
    #[test]
    fn a_file_another_writer_makes_between_the_look_and_the_make_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let urls = Folder::open(dir.path(), Path::new("urls"), Sharing::default()).unwrap();
        let theirs = RefCell::new(None);

        let mine = open_or_make(
            || urls.open_file("0.log"),
            || {
                theirs
                    .borrow_mut()
                    .get_or_insert_with(|| urls.create_new("0.log").unwrap());
                urls.create_new("0.log")
            },
        );

        let mut mine = mine.unwrap();
        let mut theirs = theirs.into_inner().unwrap();
        theirs.write_all(b"theirs\n").unwrap();
        mine.seek(SeekFrom::End(0)).unwrap();
        mine.write_all(b"mine\n").unwrap();
        let written = std::fs::read(dir.path().join("urls").join("0.log")).unwrap();
        assert_eq!(written, b"theirs\nmine\n");
    }

    /// a make refused as not found is tried again, a few times at most.
    #[test]
    fn a_make_that_finds_nothing_is_tried_again_a_few_times() {
        let dir = tempfile::tempdir().unwrap();
        let urls = Folder::open(dir.path(), Path::new("urls"), Sharing::default()).unwrap();
        let refusals = Cell::new(1);
        let made = open_or_make(
            || urls.open_file("0.lock"),
            || {
                if refusals.replace(0) > 0 {
                    return Err(io::ErrorKind::NotFound.into());
                }
                urls.create_new("0.lock")
            },
        );
        made.unwrap();
        assert!(dir.path().join("urls").join("0.lock").is_file());

        let makes = Cell::new(0);
        let missing = || Err(io::ErrorKind::NotFound.into());
        let made = open_or_make(missing, || {
            makes.set(makes.get() + 1);
            missing()
        });
        assert_eq!(made.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(makes.get(), 9);
    }
}
