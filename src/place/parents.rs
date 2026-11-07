//! directory handles keep later path swaps out of placement operations.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

#[cfg(target_os = "linux")]
use rustix::fs::ResolveFlags;
use rustix::fs::{self as native, AtFlags, Mode, OFlags};
use rustix::io::Errno;

const RETAINED: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone)]
struct Parent {
    file: Arc<File>,
    identity: Identity,
}

#[derive(Default)]
pub(super) struct Parents {
    root: OnceLock<Parent>,
    retained: Mutex<HashMap<PathBuf, Parent>>,
}

pub(super) struct Location {
    root: Arc<File>,
    root_device: u64,
    relative: PathBuf,
    parent: Parent,
    name: OsString,
}

impl Parents {
    pub(super) fn locate(
        &self,
        root: &Path,
        destination: &Path,
        real_root: bool,
        make: bool,
        retain: bool,
    ) -> io::Result<Location> {
        let relative = destination
            .parent()
            .and_then(|parent| parent.strip_prefix(root).ok())
            .ok_or_else(|| invalid("a placement path is outside its root"))?;
        parts(relative)?;
        let name = destination
            .file_name()
            .ok_or_else(|| invalid("a placement has no filename"))?;
        let root = if let Some(root) = self.root.get() {
            root.clone()
        } else {
            let file = Arc::new(open_root(root, make, real_root)?);
            let opened = Parent {
                identity: Identity::of(&file)?,
                file,
            };
            let _ = self.root.set(opened.clone());
            self.root.get().cloned().unwrap_or(opened)
        };
        let cached = if retain {
            self.retained
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(relative)
                .cloned()
        } else {
            None
        };
        let parent = if let Some(parent) = cached {
            parent
        } else {
            let file = Arc::new(open_relative(&root.file, relative, make, false)?);
            let parent = Parent {
                identity: Identity::of(&file)?,
                file,
            };
            if retain {
                let mut retained = self.retained.lock().unwrap_or_else(PoisonError::into_inner);
                if retained.len() >= RETAINED
                    && let Some(old) = retained.keys().next().cloned()
                {
                    retained.remove(&old);
                }
                retained.insert(relative.to_path_buf(), parent.clone());
            }
            parent
        };
        Ok(Location {
            root_device: root.identity.device,
            root: root.file,
            relative: relative.to_path_buf(),
            parent,
            name: name.to_owned(),
        })
    }
    #[cfg(test)]
    pub(super) fn cached(&self) -> usize {
        self.retained
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub(super) fn forget(&self, relative: &Path) {
        self.retained
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(relative);
    }
}

impl Location {
    #[cfg(test)]
    pub(super) fn standalone(destination: &Path) -> io::Result<Self> {
        let destination = std::path::absolute(destination)?;
        let root = destination
            .parent()
            .ok_or_else(|| invalid("a placement has no parent"))?;
        Parents::default().locate(root, &destination, true, false, false)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn clone_file(&self, object: &File, writable: bool) -> io::Result<Metadata> {
        native::fclonefileat(
            object,
            &self.parent.file,
            &self.name,
            native::CloneFlags::empty(),
        )?;
        let file = self.open(OFlags::RDONLY)?;
        if let Err(error) = super::set_file_writable(&file, writable) {
            let _ = self.remove_file();
            return Err(error);
        }
        file.metadata()
    }

    #[cfg(target_os = "macos")]
    fn open(&self, flags: OFlags) -> io::Result<File> {
        native::openat(
            &self.parent.file,
            &self.name,
            flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    pub(super) fn remove_file(&self) -> io::Result<()> {
        native::unlinkat(&self.parent.file, &self.name, AtFlags::empty()).map_err(Into::into)
    }

    pub(super) fn can_hardlink(&self) -> Option<bool> {
        let (staged, file) = self.create(0o600, true).ok()?;
        let linked = format!(".fetchloom-{:016x}.probe", fastrand::u64(..));
        let result = native::linkat(
            &self.parent.file,
            staged.hidden.as_str(),
            &self.parent.file,
            linked.as_str(),
            AtFlags::empty(),
        );
        drop(file);
        match result {
            Ok(()) => {
                let _ = native::unlinkat(&self.parent.file, linked.as_str(), AtFlags::empty());
                Some(true)
            }
            Err(error) if crate::volume::refuses_links(&error.into()) => Some(false),
            Err(_) => None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn flush_handle(&self) -> io::Result<File> {
        native::openat(
            &self.root,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    pub(super) fn serializes(&self) -> bool {
        let Ok(stat) = native::fstatfs(&self.root) else {
            return false;
        };
        #[cfg(target_os = "linux")]
        {
            super::schedule::serializes_magic(i128::from(stat.f_type))
        }
        #[cfg(target_os = "macos")]
        {
            let name: Vec<u8> = stat
                .f_fstypename
                .iter()
                .take_while(|&&byte| byte != 0)
                .map(|byte| byte.cast_unsigned())
                .collect();
            super::schedule::serializes_name(&String::from_utf8_lossy(&name))
        }
    }

    pub(super) fn other_device(&self) -> bool {
        self.parent.identity.device != self.root_device
    }

    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.parent.file, &self.name)
    }

    pub(super) fn hard_link(&self, object: &Path) -> io::Result<Metadata> {
        native::linkat(
            native::CWD,
            object,
            &self.parent.file,
            &self.name,
            AtFlags::empty(),
        )?;
        self.metadata()
    }

    pub(super) fn create(&self, mode: u32, retained: bool) -> io::Result<(Staged, File)> {
        #[cfg(target_os = "macos")]
        let mode = u16::try_from(mode).map_err(|_| invalid("a placement mode is out of range"))?;
        loop {
            let hidden = format!(".fetchloom-{:016x}.copy", fastrand::u64(..));
            match native::openat(
                &self.parent.file,
                hidden.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(mode),
            ) {
                Ok(file) => {
                    return Ok((
                        Staged {
                            root: Arc::clone(&self.root),
                            relative: self.relative.clone(),
                            identity: self.parent.identity,
                            retained: retained.then(|| self.parent.clone()),
                            hidden,
                            published: false,
                            name: self.name.clone(),
                        },
                        File::from(file),
                    ));
                }
                Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub(super) fn remove_dir(&self) -> io::Result<()> {
        native::unlinkat(&self.parent.file, &self.name, AtFlags::REMOVEDIR).map_err(Into::into)
    }
}

pub(super) struct Staged {
    root: Arc<File>,
    relative: PathBuf,
    identity: Identity,
    // only synchronous reflinks retain a parent; queued copies share just the root descriptor.
    retained: Option<Parent>,
    hidden: String,
    published: bool,
    name: OsString,
}

impl Staged {
    fn parent(&self) -> io::Result<Arc<File>> {
        if let Some(parent) = &self.retained {
            return Ok(Arc::clone(&parent.file));
        }
        let parent = Arc::new(open_relative(&self.root, &self.relative, false, false)?);
        if Identity::of(&parent)? != self.identity {
            return Err(invalid("a placement parent changed before publication"));
        }
        Ok(parent)
    }

    pub(super) fn publish(mut self) -> io::Result<Metadata> {
        let parent = self.parent()?;
        self.name_in(&parent)?;
        metadata(&parent, &self.name)
    }

    fn name_in(&mut self, parent: &File) -> io::Result<()> {
        publish(parent, &self.hidden, &self.name, false)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.published
            && let Ok(parent) = self.parent()
        {
            let _ = native::unlinkat(&parent, self.hidden.as_str(), AtFlags::empty());
        }
    }
}

fn metadata(parent: &File, name: &OsStr) -> io::Result<Metadata> {
    #[cfg(target_os = "linux")]
    let mode = OFlags::PATH;
    #[cfg(not(target_os = "linux"))]
    let mode = OFlags::RDONLY | OFlags::NONBLOCK;
    let file = File::from(native::openat(
        parent,
        name,
        mode | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    file.metadata()
}

fn publish(parent: &File, hidden: &str, name: &OsStr, fallback: bool) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    let _ = fallback;
    #[cfg(target_os = "linux")]
    if !fallback {
        match native::renameat_with(parent, hidden, parent, name, native::RenameFlags::NOREPLACE) {
            Ok(()) => return Ok(()),
            Err(Errno::NOSYS | Errno::NOTSUP | Errno::INVAL | Errno::NOTDIR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    native::linkat(parent, hidden, parent, name, AtFlags::empty())?;
    native::unlinkat(parent, hidden, AtFlags::empty()).map_err(Into::into)
}

fn flags() -> OFlags {
    #[cfg(target_os = "linux")]
    let mode = OFlags::PATH;
    #[cfg(not(target_os = "linux"))]
    let mode = OFlags::RDONLY;
    mode | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn parts(path: &Path) -> io::Result<Vec<&OsStr>> {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(Ok(name)),
            Component::CurDir => None,
            _ => Some(Err(invalid("a placement parent must stay below its root"))),
        })
        .collect()
}

fn component(parent: &File, name: &OsStr, make: bool) -> io::Result<File> {
    let opened = native::openat(parent, name, flags(), Mode::empty());
    let opened = match opened {
        Err(Errno::NOENT) if make => {
            match native::mkdirat(parent, name, Mode::from_bits_truncate(0o777)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
            native::openat(parent, name, flags(), Mode::empty())
        }
        found => found,
    };
    match opened {
        Ok(file) => Ok(File::from(file)),
        Err(error @ (Errno::NOTDIR | Errno::LOOP)) => {
            if native::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
                native::FileType::from_raw_mode(stat.st_mode) == native::FileType::Symlink
            }) {
                return Err(invalid(
                    "a placement parent is a link, and fetchloom only places files in real folders",
                ));
            }
            Err(error.into())
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn open_root(path: &Path, make: bool, real: bool) -> io::Result<File> {
    let absolute = std::path::absolute(path)?;
    if absolute
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(invalid("a placement root contains a parent component"));
    }
    let Some(parent) = absolute.parent() else {
        return native::openat(native::CWD, absolute, flags(), Mode::empty())
            .map(File::from)
            .map_err(Into::into);
    };
    // ancestors of the chosen root may themselves have aliases, such as /var on macos.
    let folder = match File::open(parent) {
        Ok(folder) => folder,
        Err(error) if make && error.kind() == io::ErrorKind::NotFound => {
            open_root(parent, true, true)?
        }
        Err(error) => return Err(error),
    };
    let name = absolute
        .file_name()
        .ok_or_else(|| invalid("a placement root has no name"))?;
    if !real {
        if make {
            match native::mkdirat(&folder, name, Mode::from_bits_truncate(0o777)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
        return native::openat(&folder, name, flags() & !OFlags::NOFOLLOW, Mode::empty())
            .map(File::from)
            .map_err(Into::into);
    }
    component(&folder, name, make)
}

fn open_relative(root: &File, path: &Path, make: bool, fallback: bool) -> io::Result<File> {
    let parts = parts(path)?;
    #[cfg(not(target_os = "linux"))]
    let _ = fallback;
    #[cfg(target_os = "linux")]
    if !fallback {
        let path = if parts.is_empty() {
            Path::new(".")
        } else {
            path
        };
        match native::openat2(
            root,
            path,
            flags(),
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(file) => return Ok(File::from(file)),
            Err(Errno::NOSYS | Errno::NOTSUP | Errno::INVAL | Errno::NOTDIR) => {}
            Err(Errno::NOENT) if make => {}
            Err(Errno::LOOP) => {
                return Err(invalid(
                    "a placement parent is a link, and fetchloom only places files in real folders",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut folder = root.try_clone()?;
    for part in parts {
        folder = component(&folder, part, make)?;
    }
    Ok(folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;

    #[test]
    fn relative_resolution_rejects_links_and_escapes_in_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("real")).unwrap();
        symlink("real", dir.path().join("link")).unwrap();
        let root = open_root(dir.path(), false, true).unwrap();
        for fallback in [false, true] {
            for path in ["../escape", "/escape", "link"] {
                assert!(
                    open_relative(&root, Path::new(path), false, fallback).is_err(),
                    "{path}"
                );
            }
            assert!(open_relative(&root, Path::new("real/new"), true, fallback).is_ok());
        }
    }

    #[test]
    fn publication_and_cleanup_keep_the_parent_that_created_the_file() {
        for retain in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("data");
            let outside = dir.path().join("outside");
            fs::create_dir_all(root.join("set")).unwrap();
            fs::create_dir(&outside).unwrap();
            let parents = Parents::default();
            let location = parents
                .locate(&root, &root.join("set/file"), true, false, false)
                .unwrap();
            let (staged, mut output) = location.create(0o600, retain).unwrap();
            output.write_all(b"pinned").unwrap();
            let hidden = staged.hidden.clone();
            fs::rename(root.join("set"), root.join("saved")).unwrap();
            symlink(&outside, root.join("set")).unwrap();
            fs::write(outside.join(&hidden), b"foreign").unwrap();
            let result = staged.publish();
            assert_eq!(fs::read(outside.join(&hidden)).unwrap(), b"foreign");
            assert!(!outside.join("file").exists());
            if retain {
                assert!(result.is_ok());
                assert_eq!(fs::read(root.join("saved/file")).unwrap(), b"pinned");
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn publication_releases_ownership_of_the_hidden_name() {
        let dir = tempfile::tempdir().unwrap();
        let parents = Parents::default();
        let location = parents
            .locate(dir.path(), &dir.path().join("file"), true, false, false)
            .unwrap();
        let (mut staged, mut file) = location.create(0o600, true).unwrap();
        file.write_all(b"placed").unwrap();
        let hidden = dir.path().join(&staged.hidden);
        staged.name_in(&location.parent.file).unwrap();
        fs::write(&hidden, b"another owner").unwrap();
        drop(staged);
        assert_eq!(fs::read(hidden).unwrap(), b"another owner");
        assert_eq!(fs::read(dir.path().join("file")).unwrap(), b"placed");
    }

    #[test]
    fn removal_and_pruning_keep_the_resolved_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let outside = dir.path().join("outside");
        fs::create_dir_all(root.join("set")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("set/file"), b"placed").unwrap();
        fs::write(outside.join("file"), b"foreign").unwrap();
        let parents = Parents::default();
        let location = parents
            .locate(&root, &root.join("set/file"), true, false, false)
            .unwrap();
        let folder = parents
            .locate(&root, &root.join("set"), true, false, false)
            .unwrap();
        fs::rename(root.join("set"), root.join("saved")).unwrap();
        symlink(&outside, root.join("set")).unwrap();
        location.remove_file().unwrap();
        assert!(folder.remove_dir().is_err());
        assert_eq!(fs::read(outside.join("file")).unwrap(), b"foreign");
        assert!(!root.join("saved/file").exists());
    }

    #[test]
    fn publication_never_replaces_files_or_links_in_either_kernel_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = open_root(dir.path(), false, true).unwrap();
        fs::write(dir.path().join("source"), b"new").unwrap();
        fs::write(dir.path().join("target"), b"old").unwrap();
        symlink("target", dir.path().join("link")).unwrap();
        for fallback in [false, true] {
            for name in ["target", "link"] {
                assert_eq!(
                    publish(&root, "source", OsStr::new(name), fallback)
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::AlreadyExists
                );
                assert_eq!(fs::read(dir.path().join("source")).unwrap(), b"new");
                assert_eq!(fs::read(dir.path().join("target")).unwrap(), b"old");
            }
        }
    }

    #[test]
    fn queued_copies_do_not_keep_an_evicted_parent_open() {
        let dir = tempfile::tempdir().unwrap();
        let parents = Parents::default();
        let mut queued = Vec::new();
        for index in 0..RETAINED * 2 {
            let path = dir.path().join(format!("{index}/file"));
            let location = parents.locate(dir.path(), &path, true, true, true).unwrap();
            let weak = Arc::downgrade(&location.parent.file);
            let (staged, output) = location.create(0o600, false).unwrap();
            drop(output);
            drop(location);
            queued.push((staged, weak));
        }
        assert_eq!(parents.retained.lock().unwrap().len(), RETAINED);
        assert_eq!(
            queued
                .iter()
                .filter(|(_, parent)| parent.upgrade().is_some())
                .count(),
            RETAINED
        );
        drop(queued);
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
            fs::read_dir(entry.unwrap().path())
                .unwrap()
                .next()
                .is_none()
        }));
    }
}
