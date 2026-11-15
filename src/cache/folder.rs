//! a folder of the cache held open from the cache's root without following a link, so the files
//! made, opened, renamed and removed in it stay in it, whatever anyone who can write the cache
//! puts at its name meanwhile. unix holds a handle; windows reaches the folder by its path.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::place::{FolderUse, found_folder};

/// a folder below the cache's root, and where it is.
#[derive(Debug)]
pub(super) struct Folder {
    #[cfg(unix)]
    handle: File,
    root: PathBuf,
    below: PathBuf,
}

impl Folder {
    /// opens the folder `below` the cache's `root`, making what is missing and refusing a link or a
    /// file where a folder should be. `root` itself may be a link, as a cache folder someone named.
    pub(super) fn open(root: &Path, below: &Path) -> io::Result<Folder> {
        Folder::reach(root, below, true)
    }

    /// [`Folder::open`], making nothing: a missing folder is `NotFound`.
    pub(super) fn existing(root: &Path, below: &Path) -> io::Result<Folder> {
        Folder::reach(root, below, false)
    }

    fn reach(root: &Path, below: &Path, make: bool) -> io::Result<Folder> {
        Ok(Folder {
            #[cfg(unix)]
            handle: walk(root, below, make)?,
            #[cfg(not(unix))]
            below: checked(root, below, make)?,
            #[cfg(unix)]
            below: below.to_path_buf(),
            root: root.to_path_buf(),
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
            let now = rustix::fs::fstat(walk(&self.root, &self.below, false)?)?;
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
        checked(&self.root, &self.below, false).map(drop)
    }

    /// makes the file `name`, to read and write, failing when one is there.
    pub(super) fn create_new(&self, name: &str) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
            let mode = Mode::from_bits_truncate(0o666);
            Ok(rustix::fs::openat(&self.handle, name, flags | OFlags::CLOEXEC, mode)?.into())
        }
        #[cfg(not(unix))]
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.join(name))
    }

    /// opens the plain file `name` to read and write, never through a link; `create` makes a
    /// missing one.
    pub(super) fn open_file(&self, name: &str, create: bool) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::OFlags;
            let create = if create {
                OFlags::CREATE
            } else {
                OFlags::empty()
            };
            self.open_plain(name, OFlags::RDWR | create)
        }
        #[cfg(not(unix))]
        crate::volume::open_unfollowed(&self.join(name), create)
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

    /// the bytes of the plain file `name`, never read through a link.
    pub(super) fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.read_file(name)?.read_to_end(&mut bytes)?;
        Ok(bytes)
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
        #[cfg(unix)]
        let renamed = || -> io::Result<()> {
            Ok(rustix::fs::renameat(
                &scratch.handle,
                temporary.as_str(),
                &self.handle,
                name,
            )?)
        };
        #[cfg(not(unix))]
        let renamed = || std::fs::rename(scratch.join(&temporary), self.join(name));
        written.and_then(|()| renamed()).inspect_err(|_| {
            let _ = scratch.remove(&temporary);
        })
    }

    #[cfg(unix)]
    fn open_plain(&self, name: &str, flags: rustix::fs::OFlags) -> io::Result<File> {
        use rustix::fs::{Mode, OFlags};
        // a fifo must not block opening before its handle can be refused
        let flags = flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let mode = Mode::from_bits_truncate(0o666);
        let file = File::from(rustix::fs::openat(&self.handle, name, flags, mode).map_err(
            |err| {
                if err == rustix::io::Errno::LOOP {
                    refused(&self.join(name))
                } else {
                    err.into()
                }
            },
        )?);
        if file.metadata()?.is_file() {
            Ok(file)
        } else {
            Err(refused(&self.join(name)))
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

/// the folder `below` `root`, opened folder by folder without following a link, making a missing
/// one when `make` says so.
#[cfg(unix)]
fn walk(root: &Path, below: &Path, make: bool) -> io::Result<File> {
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
        let opened = match rustix::fs::openat(&folder, name, flags, Mode::empty()) {
            Err(Errno::NOENT) if make => {
                match rustix::fs::mkdirat(&folder, name, Mode::from_bits_truncate(0o777)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => return Err(err.into()),
                }
                rustix::fs::openat(&folder, name, flags, Mode::empty())
            }
            opened => opened,
        };
        folder = match opened {
            Ok(opened) => File::from(opened),
            Err(Errno::LOOP | Errno::NOTDIR) => {
                found_folder(&path, &std::fs::symlink_metadata(&path)?, FolderUse::Cache)?;
                return Err(Errno::NOTDIR.into());
            }
            Err(err) => return Err(err.into()),
        };
    }
    Ok(folder)
}

/// `below`, once each folder from `root` down to it is seen to be a real one, made where missing
/// when `make` says so.
#[cfg(not(unix))]
fn checked(root: &Path, below: &Path, make: bool) -> io::Result<PathBuf> {
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
    Ok(below.to_path_buf())
}
