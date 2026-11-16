//! windows directory handles deny delete sharing while a placement uses their paths: each folder
//! from a placement's root down to the file is held with traverse access, which the system counts
//! in sharing as it counts no open that only reads attributes, so no one renames or removes one of
//! them meanwhile. a folder above the root that this user may not traverse is only looked at.

use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub(super) struct Parents {
    root: OnceLock<Arc<Root>>,
}

struct Root {
    path: PathBuf,
    _handles: Vec<File>,
}

pub(super) struct Location {
    root: Arc<Root>,
    path: PathBuf,
    _parents: Vec<File>,
}

impl Parents {
    /// where `below`, a path below `root`, goes: the folders on its way, each held so it cannot
    /// be replaced while in use. with `make`, missing folders are made.
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
        let parts = parts(relative)?;
        let name = below
            .file_name()
            .ok_or_else(|| invalid("a placement has no filename"))?;
        plain(name)?;
        let root = if let Some(root) = self.root.get() {
            Arc::clone(root)
        } else {
            let absolute = std::path::absolute(root)?;
            let selected =
                if let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) {
                    if make {
                        fs::create_dir_all(parent)?;
                    }
                    fs::canonicalize(parent)?.join(name)
                } else {
                    absolute
                };
            // aliases above the selected root are allowed; its own link policy is unchanged
            let mut handles = lock_chain(&selected, make, !real_root)?;
            let path = fs::canonicalize(&selected)?;
            handles.extend(lock_chain(&path, false, false)?);
            let opened = Arc::new(Root {
                path,
                _handles: handles,
            });
            let _ = self.root.set(Arc::clone(&opened));
            self.root.get().cloned().unwrap_or(opened)
        };
        let mut path = root.path.clone();
        let mut parents = Vec::new();
        for part in parts {
            path.push(part);
            parents.push(lock_folder(&path, make, false)?);
        }
        path.push(name);
        Ok(Location {
            root,
            path,
            _parents: parents,
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

    pub(super) fn root_path(&self) -> &Path {
        &self.root.path
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        fs::symlink_metadata(&self.path)
    }

    /// opens the file to read by its path, below folders this location holds open, which no one
    /// can swap meanwhile, never following a link in its place.
    pub(super) fn open_read(&self) -> io::Result<File> {
        crate::volume::read_unfollowed(&self.path)
    }

    pub(super) fn link(&self, object: &Path) -> io::Result<()> {
        fs::hard_link(object, &self.path)
    }

    /// gives `object` a second name hidden in this location's folder, which goes again when the
    /// result is dropped, unless it took this location's name.
    pub(super) fn stage_link(self: &Arc<Self>, object: &Path) -> io::Result<Staged> {
        let (staged, _) = self.claim(|path| {
            fs::hard_link(object, path)?;
            File::open(path).inspect_err(|_| {
                let _ = fs::remove_file(path);
            })
        })?;
        Ok(staged)
    }

    /// clones `object` under a name hidden in this location's folder, which goes again when the
    /// result is dropped, unless it took this location's name. a clone takes its object's
    /// attributes, so the read-only flag of a cached object comes with it.
    pub(super) fn stage_clone(self: &Arc<Self>, object: &Path) -> io::Result<Staged> {
        let (staged, _) = self.claim(|path| {
            reflink_copy::reflink(object, path)?;
            fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .open(path)
                .inspect_err(|_| {
                    let _ = fs::remove_file(path);
                })
        })?;
        Ok(staged)
    }

    pub(super) fn create_file(&self, readonly: bool) -> io::Result<File> {
        const FILE_ATTRIBUTE_READONLY: u32 = 1;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        if readonly {
            options.attributes(FILE_ATTRIBUTE_READONLY);
        }
        options.open(&self.path)
    }

    pub(super) fn create_folder(&self) -> io::Result<()> {
        fs::create_dir(&self.path)
    }

    pub(super) fn rename_to(&self, target: &Location) -> io::Result<()> {
        fs::rename(&self.path, &target.path)
    }

    /// renames to `target`'s name, refusing a taken name with `AlreadyExists`.
    pub(super) fn rename_new_to(&self, target: &Location) -> io::Result<()> {
        crate::volume::rename_new(&self.path, &target.path)
    }

    pub(super) fn link_to(&self, target: &Location) -> io::Result<()> {
        fs::hard_link(&self.path, &target.path)
    }

    pub(super) fn remove_dir(&self) -> io::Result<()> {
        fs::remove_dir(&self.path)
    }

    pub(super) fn create(
        self: &Arc<Self>,
        _mode: u32,
        _retained: bool,
    ) -> io::Result<(Staged, File)> {
        self.claim(crate::volume::create_hidden)
    }

    pub(super) fn clone_file(
        self: &Arc<Self>,
        object: &Path,
        writable: bool,
    ) -> io::Result<Metadata> {
        const FILE_READ_ATTRIBUTES: u32 = 0x80;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let (staged, file) = self.claim(|path| {
            reflink_copy::reflink(object, path)?;
            fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)
                .inspect_err(|_| {
                    let _ = fs::remove_file(path);
                })
        })?;
        staged.publish()?;
        if let Err(error) = super::set_named_writable(&file, &self.path, writable) {
            let _ = fs::remove_file(&self.path);
            return Err(error);
        }
        file.metadata()
    }

    fn claim(
        self: &Arc<Self>,
        create: impl Fn(&Path) -> io::Result<File>,
    ) -> io::Result<(Staged, File)> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("a placement has no parent"))?;
        loop {
            let path = parent.join(format!(".fetchloom-{:016x}.copy", fastrand::u64(..)));
            match create(&path) {
                Ok(file) => {
                    return Ok((
                        Staged {
                            temporary: Some(tempfile::TempPath::try_from_path(path)?),
                            location: Arc::clone(self),
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// the filesystem the root is on.
    pub(super) fn filesystem(&self) -> crate::space::Filesystem {
        crate::space::Filesystem::of(&self.root.path)
    }

    pub(super) fn can_hardlink(&self) -> Option<bool> {
        crate::volume::Volume::new(self.root.path.clone()).can_hardlink()
    }
}

pub(super) struct Staged {
    // this field is dropped before location releases the directory handles
    temporary: Option<tempfile::TempPath>,
    location: Arc<Location>,
}

impl Staged {
    pub(super) fn publish(mut self) -> io::Result<Metadata> {
        let temporary = self
            .temporary
            .take()
            .ok_or_else(|| invalid("a placement was already published"))?;
        temporary
            .persist_noclobber(&self.location.path)
            .map_err(|error| error.error)?;
        self.location.metadata()
    }

    /// gives the staged file its name in place of the plain file there while `unchanged` says that
    /// file still looks as it did, and returns the new look; none, the staged file gone, otherwise.
    /// windows refuses to rename over a read-only file, so the flag of one with no other name,
    /// as a copy has, is cleared through a handle for the rename and put back if it still fails,
    /// as while another program holds the file open without sharing its deletion.
    pub(super) fn replace(
        mut self,
        unchanged: impl FnOnce(&Metadata) -> bool,
    ) -> io::Result<Option<Metadata>> {
        let mut temporary = self
            .temporary
            .take()
            .ok_or_else(|| invalid("a placement was already published"))?;
        let found = self.location.metadata()?;
        if !crate::volume::is_plain(&found) || !unchanged(&found) {
            return Ok(None);
        }
        // tempfile's own rename clears the file's attributes, which a hardlink shares with the
        // cached object and every other link to it
        match fs::rename(&*temporary, &self.location.path) {
            Err(error)
                if error.kind() == io::ErrorKind::PermissionDenied
                    && found.permissions().readonly()
                    && !super::has_other_names(&self.location.path) =>
            {
                let held = crate::volume::open_attributes(&self.location.path)?;
                let before = held.metadata()?.permissions();
                let mut writable = before.clone();
                #[expect(
                    clippy::permissions_set_readonly_false,
                    reason = "the file loses its name next, or gets its flag back"
                )]
                writable.set_readonly(false);
                held.set_permissions(writable)?;
                if let Err(error) = fs::rename(&*temporary, &self.location.path) {
                    let _ = held.set_permissions(before);
                    return Err(error);
                }
            }
            renamed => renamed?,
        }
        temporary.disable_cleanup(true);
        self.location.metadata().map(Some)
    }
}

const FILE_READ_ATTRIBUTES: u32 = 0x80;
/// a folder's execute access. the system counts an open in sharing only when it asks to read,
/// write, execute or delete, so only a hold that asks for it keeps the delete sharing it withholds.
const FILE_TRAVERSE: u32 = 0x20;

/// opens the folder at `path` to hold it, as [`open_folder`] does.
fn lock_folder(path: &Path, make: bool, allow_link: bool) -> io::Result<File> {
    open_folder(path, make, allow_link, FILE_READ_ATTRIBUTES | FILE_TRAVERSE)
}

/// [`lock_folder`] for a folder above a placement's root, where the person chose the root: held
/// where this user may traverse it, else only looked at, since the bypass-traverse privilege lets
/// a person reach below a folder whose permissions refuse them traverse.
fn lock_above(path: &Path, make: bool, allow_link: bool) -> io::Result<File> {
    match lock_folder(path, make, allow_link) {
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            open_folder(path, make, allow_link, FILE_READ_ATTRIBUTES)
        }
        held => held,
    }
}

/// opens the folder at `path` with `access`, sharing reads and writes but not deletes, as itself
/// when it is a link, which only `allow_link` lets through; with `make`, a missing one is made.
fn open_folder(path: &Path, make: bool, allow_link: bool, access: u32) -> io::Result<File> {
    const FILE_SHARE_READ_WRITE: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let open = || {
        fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(FILE_SHARE_READ_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    };
    let file = match open() {
        Err(error) if make && error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            open()?
        }
        result => result?,
    };
    let metadata = file.metadata()?;
    // a link names another path; a folder a cloud drive keeps is a reparse point that does not
    if metadata.file_type().is_symlink() {
        if !allow_link {
            return Err(invalid(
                "a placement parent is a link, and fetchloom only places files in real folders",
            ));
        }
    } else if !metadata.is_dir() {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    Ok(file)
}

fn lock_chain(path: &Path, make: bool, allow_root_link: bool) -> io::Result<Vec<File>> {
    let path = std::path::absolute(path)?;
    let mut at = PathBuf::new();
    let mut handles = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => at.push(component),
            Component::Normal(name) => {
                at.push(name);
                let allow_link = allow_root_link && at == path;
                handles.push(if at == path {
                    lock_folder(&at, make, allow_link)?
                } else {
                    lock_above(&at, make, allow_link)?
                });
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid("a placement root contains a parent component"));
            }
        }
    }
    if handles.is_empty() {
        handles.push(lock_folder(&path, false, false)?);
    }
    Ok(handles)
}

fn parts(path: &Path) -> io::Result<Vec<&OsStr>> {
    path.components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(plain(name).map(|()| name)),
            Component::CurDir => None,
            _ => Some(Err(invalid("a placement parent must stay below its root"))),
        })
        .collect()
}

/// refuses a name with a colon: one with a drive, as `C:x`, starts a new path where it is joined,
/// and one such as `x:s` names a stream of the file `x`, so either leads elsewhere.
fn plain(name: &OsStr) -> io::Result<()> {
    if name.as_encoded_bytes().contains(&b':') {
        return Err(invalid(
            "a placement name holds a colon, which names a drive or a stream on windows",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
