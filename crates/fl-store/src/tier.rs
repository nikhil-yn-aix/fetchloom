//! A tier on a local or network filesystem: a directory of objects and the records beside them.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::error::{StoreError, io};
use crate::layout::{DIRECTORIES, Layout};
use crate::mode;
use crate::platform;
use crate::staged::{BUFFER, Scratch, Staged, create_unique};

/// Files up to this size are written in one go, so reserving space first buys nothing and costs
/// a call.
const RESERVE_ABOVE: u64 = 1 << 20;

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
        let root = std::path::absolute(root).map_err(io("resolve", root))?;
        let root = root.as_path();
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
        let span = tracing::debug_span!(
            "locate",
            tier = %self.layout.root().display(),
            %digest,
            found = tracing::field::Empty,
        )
        .entered();
        let path = self.object_path(digest)?;
        let found = fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file());
        span.record("found", found);
        found.then_some(Located { path })
    }

    /// Where the object named by `digest` lives or would live, for BLAKE3 digests.
    pub(crate) fn object_path(&self, digest: &Digest) -> Option<PathBuf> {
        match digest {
            Digest::Blake3(blake3) => Some(self.layout.object(blake3)),
            _ => None,
        }
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
        if let Some(length) = length.filter(|length| *length > RESERVE_ABOVE) {
            platform::reserve(&file, length).map_err(io("reserve space for", scratch.path()))?;
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
        platform::seal(&file).map_err(io("make read only", scratch.path()))?;
        drop(file);
        let blake3 = *hasher.finalize().as_bytes();
        let digest = Digest::Blake3(blake3);
        let span = tracing::debug_span!(
            "ingest",
            tier = %self.layout.root().display(),
            %digest,
            bytes = written,
            new = tracing::field::Empty,
        )
        .entered();
        let target = self.layout.object(&blake3);
        let new = self.place(scratch, &target)?;
        if !new {
            let _ = platform::freshen(&target);
        }
        span.record("new", new);
        Ok(Ingested {
            digest,
            new,
            bytes: written,
        })
    }

    /// Copies the file at `path` into the tier, reading, hashing and writing in one pass.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the file cannot be read or the object written, and
    /// [`StoreError::Length`] when the file changed size while it was read.
    pub fn ingest_file(&self, path: &Path) -> Result<Ingested, StoreError> {
        let _span = tracing::debug_span!("ingest_file", path = %path.display()).entered();
        let source = File::open(path).map_err(io("open", path))?;
        let length = source
            .metadata()
            .map_err(io("read metadata of", path))?
            .len();
        let mut staged = self.stage(Some(length))?;
        io::copy(&mut BufReader::with_capacity(BUFFER, source), &mut staged)
            .map_err(io("copy into the tier", path))?;
        self.ingest(staged)
    }

    /// The tier's paths.
    pub(crate) const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Publishes the finished file of `scratch` at `target` unless something is there already,
    /// and says whether this call placed it. Publishing fails when the name is taken, so of
    /// writers racing for one name exactly one places it. A failure while another writer's file
    /// holds the name counts as already there.
    pub(crate) fn place(&self, scratch: Scratch, target: &Path) -> Result<bool, StoreError> {
        if exists(target) {
            return Ok(false);
        }
        let mut result = platform::publish(scratch.path(), target);
        if let Err(error) = &result
            && error.kind() == io::ErrorKind::NotFound
            && let Some(parent) = target.parent()
        {
            mode::create_dir(parent, self.sharing).map_err(io("create directory", parent))?;
            result = platform::publish(scratch.path(), target);
        }
        match result {
            Ok(renamed) => {
                if renamed {
                    scratch.keep();
                }
                Ok(true)
            }
            Err(_) if exists(target) => Ok(false),
            Err(error) => Err(io("place", scratch.path())(error)),
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
    fn a_local_file_is_ingested_in_one_pass() {
        let (temp, tier) = tier();
        let source = temp.path().join("source.bin");
        let bytes: Vec<u8> = (0..3 * BUFFER + 17)
            .map(|i| (i % 251).to_le_bytes()[0])
            .collect();
        fs::write(&source, &bytes).unwrap();
        let ingested = tier.ingest_file(&source).unwrap();
        assert_eq!(
            ingested.digest,
            Digest::Blake3(*blake3::hash(&bytes).as_bytes())
        );
        assert_eq!(ingested.bytes, bytes.len() as u64);
        let located = tier.locate(&ingested.digest).unwrap();
        assert_eq!(fs::read(located.path).unwrap(), bytes);
        let missing = tier.ingest_file(&temp.path().join("missing")).unwrap_err();
        assert!(
            missing.to_string().starts_with("could not open `"),
            "{missing}"
        );
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
        assert!(
            !rustix::process::geteuid().is_root(),
            "file modes do not bind root, so run the tests as another user"
        );
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

    #[test]
    fn ingested_adds_new_objects_and_bytes_to_work() {
        let ingested = Ingested {
            digest: Digest::Blake3([1; 32]),
            new: true,
            bytes: 5,
        };
        let mut work = Work::default();
        ingested.add_to(&mut work);
        ingested.add_to(&mut work);
        assert_eq!(work.objects_ingested, 2);
        assert_eq!(work.bytes_written, 10);
        Ingested {
            new: false,
            ..ingested
        }
        .add_to(&mut work);
        assert_eq!(work.objects_ingested, 2);
        assert_eq!(work.bytes_written, 15);
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        assert!(
            !rustix::process::geteuid().is_root(),
            "file modes do not bind root, so run the tests as another user"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_new_tier_under_a_read_only_parent_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        fs::create_dir(&parent).unwrap();
        set_mode(&parent, 0o555);
        let opened = Tier::open(&parent.join("tier"), Sharing::Private);
        set_mode(&parent, 0o755);
        assert!(opened.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_tier_missing_a_directory_it_cannot_create_opens_for_reading() {
        let (temp, _) = tier();
        let v1 = temp.path().join("tier").join("v1");
        fs::remove_dir(v1.join("steps")).unwrap();
        set_mode(&v1, 0o555);
        let opened = Tier::open(&temp.path().join("tier"), Sharing::Private);
        set_mode(&v1, 0o755);
        assert!(!opened.unwrap().writable());
    }

    #[test]
    fn a_tmp_that_is_a_file_is_an_error_not_a_read_only_tier() {
        let (temp, _) = tier();
        let staging = temp.path().join("tier").join("v1").join("tmp");
        fs::remove_dir(&staging).unwrap();
        fs::write(&staging, b"").unwrap();
        let error = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap_err();
        assert!(matches!(error, StoreError::Io { .. }), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn an_object_that_cannot_be_placed_is_an_error() {
        let (_temp, tier) = tier();
        let blake3 = *blake3::hash(b"blocked").as_bytes();
        let fan = tier.layout.object(&blake3).parent().unwrap().to_owned();
        fs::create_dir(&fan).unwrap();
        set_mode(&fan, 0o555);
        let mut staged = tier.stage(None).unwrap();
        staged.write_all(b"blocked").unwrap();
        let placed = tier.ingest(staged);
        set_mode(&fan, 0o755);
        assert!(matches!(placed, Err(StoreError::Io { .. })), "{placed:?}");
        assert!(tier.locate(&Digest::Blake3(blake3)).is_none());
        assert_eq!(tmp_files(&tier), 0);
    }

    #[test]
    fn only_a_file_over_the_threshold_has_its_space_reserved_up_front() {
        let (_temp, tier) = tier();
        for (length, reserved) in [
            (RESERVE_ABOVE, 0),
            (RESERVE_ABOVE + 1, RESERVE_ABOVE + 1),
            (1, 0),
        ] {
            let staged = tier.stage(Some(length)).unwrap();
            let size = fs::metadata(staged.scratch.path()).unwrap().len();
            assert_eq!(size, reserved, "promised {length}");
        }
    }
}
