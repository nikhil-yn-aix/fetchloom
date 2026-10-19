//! A tier on a local or network filesystem: a directory of objects and the records beside them.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::error::{StoreError, io};
use crate::layout::{DIRECTORIES, Layout};
use crate::mode;
use crate::staged::{Scratch, Staged};

/// Who shares a tier, which decides the modes of the directories it creates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sharing {
    /// One user: directories are 0755.
    Private,
    /// A group: directories are setgid 2775, so everything inside keeps the tier's group.
    Group,
}

/// A tier on a filesystem.
#[derive(Debug)]
pub struct Tier {
    layout: Layout,
    sharing: Sharing,
    writable: bool,
}

/// An object found in a tier.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Located {
    /// Where the object's file is.
    pub path: PathBuf,
}

/// What an ingest did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ingested {
    /// The object's BLAKE3 digest, its name in the tier.
    pub digest: Digest,
    /// Whether the tier did not hold the object before.
    pub new: bool,
    /// The bytes written to stage it.
    pub bytes: u64,
}

impl Ingested {
    /// Adds this ingest to the work counters.
    pub fn add_to(&self, work: &mut Work) {
        work.objects_ingested += u64::from(self.new);
        work.bytes_written += self.bytes;
    }
}

impl Tier {
    /// Opens the tier at `root`, creating its layout where it is missing.
    ///
    /// A tier this process cannot write is still opened for reading when its layout exists.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the layout can neither be created nor found.
    pub fn open(root: &Path, sharing: Sharing) -> Result<Self, StoreError> {
        let layout = Layout::new(root);
        let writable = match create_layout(root, &layout, sharing) {
            Ok(()) => can_write(&layout.dir("tmp"))?,
            Err(error) if is_read_only(&error) && layout.dir("objects").is_dir() => false,
            Err(error) => return Err(error),
        };
        Ok(Self {
            layout,
            sharing,
            writable,
        })
    }

    /// Whether this process can add to the tier.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.writable
    }

    /// Finds the object named by `digest`. Only BLAKE3 digests name objects.
    #[must_use]
    pub fn locate(&self, digest: &Digest) -> Option<Located> {
        let Digest::Blake3(blake3) = digest else {
            return None;
        };
        let path = self.layout.object(blake3);
        fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_file())
            .then_some(Located { path })
    }

    /// Starts a new object in `tmp/`.
    ///
    /// With a `length`, the space is reserved now, so a full disk fails here rather than at the
    /// end, and [`Tier::ingest`] refuses any other length.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the file cannot be created or its space reserved.
    pub fn stage(&self, length: Option<u64>) -> Result<Staged, StoreError> {
        let (file, path) = create_unique(&self.layout.dir("tmp"))?;
        let scratch = Scratch::new(path);
        if let Some(length) = length {
            reserve(&file, length).map_err(io("reserve space for", scratch.path()))?;
        }
        Ok(Staged {
            file,
            scratch,
            hasher: blake3::Hasher::new(),
            written: 0,
            expected: length,
        })
    }

    /// Makes `staged` a read only object named by its BLAKE3 digest.
    ///
    /// When the tier already holds the object, from an earlier ingest or from another process
    /// racing this one, the staged file is removed and the object reported as not new.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Length`] when fewer or more bytes were written than promised, and
    /// [`StoreError::Io`] when the file cannot be sealed or renamed into place.
    pub fn ingest(&self, staged: Staged) -> Result<Ingested, StoreError> {
        let Staged {
            file,
            scratch,
            hasher,
            written,
            expected,
        } = staged;
        if let Some(expected) = expected
            && expected != written
        {
            return Err(StoreError::Length { expected, written });
        }
        mode::seal(&file).map_err(io("make read only", scratch.path()))?;
        drop(file);
        let blake3 = *hasher.finalize().as_bytes();
        let new = self.place(scratch, &self.layout.object(&blake3))?;
        Ok(Ingested {
            digest: Digest::Blake3(blake3),
            new,
            bytes: written,
        })
    }

    fn place(&self, scratch: Scratch, object: &Path) -> Result<bool, StoreError> {
        if exists(object) {
            return Ok(false);
        }
        let mut result = fs::rename(scratch.path(), object);
        if let Err(error) = &result
            && error.kind() == io::ErrorKind::NotFound
            && let Some(fan) = object.parent()
        {
            mode::create_dir(fan, self.sharing).map_err(io("create directory", fan))?;
            result = fs::rename(scratch.path(), object);
        }
        match result {
            Ok(()) => {
                scratch.keep();
                Ok(true)
            }
            Err(_) if exists(object) => Ok(false),
            Err(error) => Err(io("rename into place", scratch.path())(error)),
        }
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn create_layout(root: &Path, layout: &Layout, sharing: Sharing) -> Result<(), StoreError> {
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent).map_err(io("create directory", parent))?;
    }
    let mut dirs = vec![root.to_owned(), layout.root().to_owned()];
    dirs.extend(DIRECTORIES.iter().map(|name| layout.dir(name)));
    for dir in &dirs {
        mode::create_dir(dir, sharing).map_err(io("create directory", dir))?;
    }
    Ok(())
}

fn is_read_only(error: &StoreError) -> bool {
    matches!(error, StoreError::Io { source, .. } if refuses_writes(source))
}

fn refuses_writes(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

fn can_write(tmp: &Path) -> Result<bool, StoreError> {
    match create_unique(tmp) {
        Ok((file, probe)) => {
            drop(file);
            fs::remove_file(&probe).map_err(io("remove", &probe))?;
            Ok(true)
        }
        Err(StoreError::Io { source, .. }) if refuses_writes(&source) => Ok(false),
        Err(error) => Err(error),
    }
}

const NAME_ATTEMPTS: usize = 64;

/// Creates a new file in `dir` under a name no other writer holds: the process id and a counter,
/// in hex, tried again with the next counter when the name exists, which happens only when a
/// dead process or another host left it.
pub(crate) fn create_unique(dir: &Path) -> Result<(File, PathBuf), StoreError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut path = dir.to_owned();
    for _ in 0..NAME_ATTEMPTS {
        let count = NEXT.fetch_add(1, Ordering::Relaxed);
        path = dir.join(format!("{:x}-{count:x}", process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io("create", &path)(error)),
        }
    }
    Err(io("create", &path)(io::ErrorKind::AlreadyExists.into()))
}

#[cfg(unix)]
fn reserve(file: &File, length: u64) -> io::Result<()> {
    use rustix::io::Errno;
    match rustix::fs::fallocate(file, rustix::fs::FallocateFlags::empty(), 0, length) {
        Ok(()) => Ok(()),
        Err(Errno::OPNOTSUPP | Errno::NOTSUP | Errno::NOSYS) => file.set_len(length),
        Err(errno) => Err(errno.into()),
    }
}

#[cfg(windows)]
fn reserve(file: &File, length: u64) -> io::Result<()> {
    file.set_len(length)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use fl_model::digest::Algorithm;
    use fl_model::error::ErrorKind;

    use super::*;

    fn tier() -> (tempfile::TempDir, Tier) {
        let temp = tempfile::tempdir().unwrap();
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap();
        (temp, tier)
    }

    fn ingest(tier: &Tier, bytes: &[u8]) -> Ingested {
        let mut staged = tier.stage(Some(bytes.len() as u64)).unwrap();
        staged.write_all(bytes).unwrap();
        tier.ingest(staged).unwrap()
    }

    fn tmp_files(tier: &Tier) -> usize {
        fs::read_dir(tier.layout.dir("tmp")).unwrap().count()
    }

    #[test]
    fn ingest_stores_read_only_bytes_under_their_hash() {
        let (_temp, tier) = tier();
        let ingested = ingest(&tier, b"hello");
        let expected = Digest::Blake3(*blake3::hash(b"hello").as_bytes());
        assert_eq!(
            ingested,
            Ingested {
                digest: expected,
                new: true,
                bytes: 5
            }
        );
        let located = tier.locate(&expected).unwrap();
        assert_eq!(fs::read(&located.path).unwrap(), b"hello");
        let metadata = fs::metadata(&located.path).unwrap();
        assert!(metadata.permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o444);
        }
        assert_eq!(tmp_files(&tier), 0);
    }

    #[test]
    fn an_object_ingested_again_is_not_new_and_leaves_nothing_behind() {
        let (_temp, tier) = tier();
        ingest(&tier, b"same");
        let again = ingest(&tier, b"same");
        assert!(!again.new);
        assert_eq!(tmp_files(&tier), 0);
        let mut work = Work::default();
        again.add_to(&mut work);
        assert_eq!((work.objects_ingested, work.bytes_written), (0, 4));
    }

    #[test]
    fn a_length_other_than_the_promised_one_is_refused() {
        let (_temp, tier) = tier();
        for promised in [4, 6] {
            let mut staged = tier.stage(Some(promised)).unwrap();
            staged.write_all(b"hello").unwrap();
            let error = tier.ingest(staged).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::IntegrityMismatch);
            assert_eq!(
                error.to_string(),
                format!("received 5 bytes where {promised} were promised")
            );
        }
        assert_eq!(tmp_files(&tier), 0);
    }

    #[test]
    fn a_staged_file_dropped_before_ingest_is_removed() {
        let (_temp, tier) = tier();
        let mut staged = tier.stage(None).unwrap();
        staged.write_all(b"partial").unwrap();
        assert_eq!(tmp_files(&tier), 1);
        drop(staged);
        assert_eq!(tmp_files(&tier), 0);
    }

    #[test]
    fn only_blake3_digests_of_held_objects_are_located() {
        let (_temp, tier) = tier();
        let hash = *blake3::hash(b"x").as_bytes();
        assert!(tier.locate(&Digest::Blake3(hash)).is_none());
        ingest(&tier, b"x");
        assert!(tier.locate(&Digest::Blake3(hash)).is_some());
        let sha256 = Digest::from_hex(Algorithm::Sha256, &"0".repeat(64)).unwrap();
        assert!(tier.locate(&sha256).is_none());
    }

    #[test]
    fn open_creates_the_layout_and_is_writable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tier");
        let tier = Tier::open(&root, Sharing::Private).unwrap();
        assert!(tier.writable());
        for name in DIRECTORIES {
            assert!(root.join("v1").join(name).is_dir(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_modes_ignore_the_umask() {
        use std::os::unix::fs::PermissionsExt;
        rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o077));
        let temp = tempfile::tempdir().unwrap();
        for (sharing, expected) in [(Sharing::Private, 0o755), (Sharing::Group, 0o2775)] {
            let root = temp.path().join(format!("{sharing:?}"));
            Tier::open(&root, sharing).unwrap();
            for dir in [
                root.clone(),
                root.join("v1"),
                root.join("v1").join("objects"),
            ] {
                let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o7777;
                assert_eq!(mode, expected, "{}", dir.display());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_tier_that_cannot_be_written_opens_for_reading() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tier");
        Tier::open(&root, Sharing::Private).unwrap();
        let v1 = root.join("v1");
        let dirs: Vec<PathBuf> = DIRECTORIES
            .iter()
            .map(|name| v1.join(name))
            .chain([v1.clone(), root.clone()])
            .collect();
        let set = |mode| {
            for dir in &dirs {
                fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
            }
        };
        set(0o555);
        let tier = Tier::open(&root, Sharing::Private).unwrap();
        set(0o755);
        assert!(!tier.writable());
    }
}
