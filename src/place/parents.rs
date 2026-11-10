//! directory handles keep later path swaps out of placement operations.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[cfg(target_os = "linux")]
use rustix::fs::ResolveFlags;
use rustix::fs::{self as native, AtFlags, Mode, OFlags};
use rustix::io::Errno;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(file: &File) -> io::Result<Self> {
        #[cfg(test)]
        tests::reading_identity();
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
    /// read when first needed: a copy needs it, a hardlink never does.
    identity: OnceLock<Identity>,
}

impl Parent {
    fn new(file: Arc<File>) -> Parent {
        Parent {
            file,
            identity: OnceLock::new(),
        }
    }

    fn identity(&self) -> io::Result<Identity> {
        if let Some(identity) = self.identity.get() {
            return Ok(*identity);
        }
        let identity = Identity::of(&self.file)?;
        Ok(*self.identity.get_or_init(|| identity))
    }
}

#[derive(Default)]
pub(super) struct Parents {
    root: OnceLock<Parent>,
}

pub(super) struct Location {
    root: Arc<File>,
    root_device: u64,
    relative: PathBuf,
    parent: Parent,
    name: OsString,
}

impl Parents {
    /// where `below`, a path below `root`, goes: its folder, opened through the root without
    /// following a link, and its name. with `make`, missing folders are made.
    pub(super) fn locate(
        &self,
        root: &Path,
        below: &Path,
        real_root: bool,
        make: bool,
    ) -> io::Result<Location> {
        let relative = below
            .parent()
            .ok_or_else(|| invalid("a placement path is outside its root"))?;
        parts(relative).try_for_each(|part| part.map(drop))?;
        let name = below
            .file_name()
            .ok_or_else(|| invalid("a placement has no filename"))?;
        let root = if let Some(root) = self.root.get() {
            root
        } else {
            let opened = Parent::new(Arc::new(open_root(root, make, real_root)?));
            self.root.get_or_init(|| opened)
        };
        // asked of the kept root, not a clone of it, so its device is read once for all placements
        let root_device = root.identity()?.device;
        let file = Arc::new(open_relative(&root.file, relative, make, false)?);
        let parent = Parent::new(file);
        Ok(Location {
            root_device,
            root: Arc::clone(&root.file),
            relative: relative.to_path_buf(),
            parent,
            name: name.to_owned(),
        })
    }
}

impl Location {
    #[cfg(test)]
    pub(super) fn standalone(destination: &Path) -> io::Result<Self> {
        let destination = std::path::absolute(destination)?;
        let (Some(root), Some(name)) = (destination.parent(), destination.file_name()) else {
            return Err(invalid("a placement has no parent"));
        };
        Parents::default().locate(root, Path::new(name), true, false)
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

    pub(super) fn create_file(&self, readonly: bool) -> io::Result<File> {
        let mode = if readonly { 0o444 } else { 0o600 };
        native::openat(
            &self.parent.file,
            &self.name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(mode),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    pub(super) fn create_folder(&self) -> io::Result<()> {
        native::mkdirat(
            &self.parent.file,
            &self.name,
            Mode::from_bits_truncate(0o777),
        )
        .map_err(Into::into)
    }

    pub(super) fn rename_to(&self, target: &Location) -> io::Result<()> {
        native::renameat(
            &self.parent.file,
            &self.name,
            &target.parent.file,
            &target.name,
        )
        .map_err(Into::into)
    }

    /// renames to `target`'s name without replacing anything there: a name already taken is
    /// refused with `AlreadyExists`.
    pub(super) fn rename_new_to(&self, target: &Location) -> io::Result<()> {
        rename_new(
            &self.parent.file,
            &self.name,
            &target.parent.file,
            &target.name,
            false,
        )
    }

    pub(super) fn link_to(&self, target: &Location) -> io::Result<()> {
        native::linkat(
            &self.parent.file,
            &self.name,
            &target.parent.file,
            &target.name,
            AtFlags::empty(),
        )
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
        // a folder that cannot be read is treated as another drive, which flushes its own copy
        self.parent
            .identity()
            .map_or(true, |identity| identity.device != self.root_device)
    }

    pub(super) fn link(&self, object: &Path) -> io::Result<()> {
        native::linkat(
            native::CWD,
            object,
            &self.parent.file,
            &self.name,
            AtFlags::empty(),
        )
        .map_err(Into::into)
    }

    /// gives `object` a second name hidden in this location's folder, which goes again when the
    /// result is dropped, unless it took this location's name.
    pub(super) fn stage_link(&self, object: &Path) -> io::Result<Staged> {
        let identity = self.parent.identity()?;
        loop {
            let hidden = format!(".fetchloom-{:016x}.copy", fastrand::u64(..));
            match native::linkat(
                native::CWD,
                object,
                &self.parent.file,
                hidden.as_str(),
                AtFlags::empty(),
            ) {
                Ok(()) => {
                    return Ok(Staged {
                        root: Arc::clone(&self.root),
                        relative: self.relative.clone(),
                        identity,
                        retained: Some(self.parent.clone()),
                        hidden,
                        published: false,
                        name: self.name.clone(),
                    });
                }
                Err(Errno::EXIST) => {}
                Err(error) => return Err(error.into()),
            }
        }
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
                            identity: self.parent.identity()?,
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

    /// gives the staged file its name in place of the plain file there, while `unchanged` says
    /// that file still looks as it did, and returns how the file now at the name looks. none, with
    /// the staged file gone, when the file there changed or is no plain file.
    pub(super) fn replace(
        mut self,
        unchanged: impl FnOnce(&Metadata) -> bool,
    ) -> io::Result<Option<Metadata>> {
        let parent = self.parent()?;
        let found = metadata(&parent, &self.name)?;
        if !found.file_type().is_file() || !unchanged(&found) {
            return Ok(None);
        }
        native::renameat(&parent, self.hidden.as_str(), &parent, &self.name)?;
        self.published = true;
        metadata(&parent, &self.name).map(Some)
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
    rename_new(parent, OsStr::new(hidden), parent, name, fallback)
}

/// renames `from` in `from_parent` to `to` in `to_parent`, never replacing a file or link there:
/// linux's rename that refuses an existing name, else, or with `fallback`, a link to the new name
/// and the old name's removal, which refuses one too.
fn rename_new(
    from_parent: &File,
    from: &OsStr,
    to_parent: &File,
    to: &OsStr,
    fallback: bool,
) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    let _ = fallback;
    #[cfg(target_os = "linux")]
    if !fallback {
        match native::renameat_with(
            from_parent,
            from,
            to_parent,
            to,
            native::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => return Ok(()),
            Err(Errno::NOSYS | Errno::NOTSUP | Errno::INVAL | Errno::NOTDIR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    native::linkat(from_parent, from, to_parent, to, AtFlags::empty())?;
    native::unlinkat(from_parent, from, AtFlags::empty()).map_err(Into::into)
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

/// the folder names along `path`, refusing any part that would leave its root.
fn parts(path: &Path) -> impl Iterator<Item = io::Result<&OsStr>> {
    path.components().filter_map(|part| match part {
        Component::Normal(name) => Some(Ok(name)),
        Component::CurDir => None,
        _ => Some(Err(invalid("a placement parent must stay below its root"))),
    })
}

/// opens the folder `name` in `parent` without following a link. with `make`, a missing one is
/// made; `absent` says it was just found missing, so it is made before it is looked for.
fn component(parent: &File, name: &OsStr, make: bool, absent: bool) -> io::Result<File> {
    let create = || match native::mkdirat(parent, name, Mode::from_bits_truncate(0o777)) {
        Ok(()) | Err(Errno::EXIST) => Ok(()),
        Err(error) => Err(error),
    };
    let opened = if make && absent {
        create()?;
        native::openat(parent, name, flags(), Mode::empty())
    } else {
        match native::openat(parent, name, flags(), Mode::empty()) {
            Err(Errno::NOENT) if make => {
                create()?;
                native::openat(parent, name, flags(), Mode::empty())
            }
            found => found,
        }
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
    let Some(name) = absolute.file_name() else {
        return open_root(&std::fs::canonicalize(&absolute)?, make, real);
    };
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
    component(&folder, name, make, false)
}

fn open_relative(root: &File, path: &Path, make: bool, fallback: bool) -> io::Result<File> {
    parts(path).try_for_each(|part| part.map(drop))?;
    #[cfg(target_os = "linux")]
    let absent = if fallback {
        false
    } else {
        let below = if parts(path).next().is_none() {
            Path::new(".")
        } else {
            path
        };
        match native::openat2(
            root,
            below,
            flags(),
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(file) => return Ok(File::from(file)),
            Err(Errno::NOSYS | Errno::NOTSUP | Errno::INVAL | Errno::NOTDIR) => false,
            // the path is missing, so its last folder surely is
            Err(Errno::NOENT) if make => true,
            Err(Errno::LOOP) => {
                return Err(invalid(
                    "a placement parent is a link, and fetchloom only places files in real folders",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let absent = {
        let _ = fallback;
        false
    };
    let mut folder: Option<File> = None;
    let mut parts = parts(path).peekable();
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        let parent = folder.as_ref().unwrap_or(root);
        folder = Some(component(parent, part?, make, absent && last)?);
    }
    folder.map_or_else(|| root.try_clone(), Ok)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;

    thread_local! {
        static IDENTITY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(super) fn reading_identity() {
        IDENTITY_READS.set(IDENTITY_READS.get() + 1);
    }

    /// the root's device is read once and kept, so placing many files does not ask the
    /// filesystem about the root again for each.
    #[test]
    fn locating_reads_the_root_once() {
        let dir = tempfile::tempdir().unwrap();
        let parents = Parents::default();
        IDENTITY_READS.set(0);
        for name in ["a", "b"] {
            let location = parents
                .locate(dir.path(), Path::new(name), true, false)
                .unwrap();
            assert_eq!(
                location.root_device,
                fs::metadata(dir.path()).unwrap().dev()
            );
        }
        assert_eq!(IDENTITY_READS.get(), 1);
    }

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
                .locate(&root, Path::new("set/file"), true, false)
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
            .locate(dir.path(), Path::new("file"), true, false)
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
            .locate(&root, Path::new("set/file"), true, false)
            .unwrap();
        let folder = parents
            .locate(&root, Path::new("set"), true, false)
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
    fn queued_copies_keep_only_the_root_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let parents = Parents::default();
        let mut queued = Vec::new();
        for index in 0..256 {
            let path = PathBuf::from(format!("{index}/file"));
            let location = parents.locate(dir.path(), &path, true, true).unwrap();
            let weak = Arc::downgrade(&location.parent.file);
            let (staged, output) = location.create(0o600, false).unwrap();
            drop(output);
            drop(location);
            queued.push((staged, weak));
        }
        assert_eq!(
            queued
                .iter()
                .filter(|(_, parent)| parent.upgrade().is_some())
                .count(),
            0
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
