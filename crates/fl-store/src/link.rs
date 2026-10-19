//! Placing objects in a project with the best link mode a filesystem allows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::data_dir::Destination;
use crate::error::{StoreError, io};
use crate::mode;
use crate::parallel;
use crate::tier::Tier;

/// How a file was placed in a project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A copy on write clone: safe to edit, no extra space.
    Reflink,
    /// A hardlink to the object: no extra space, and an edit changes the object.
    Hardlink,
    /// A symbolic link to the object, for a tier on another filesystem.
    Symlink,
    /// A real copy, which costs the object's size.
    Copy,
}

/// The modes a [`Linker`] may use, in the order it tries them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ladder {
    modes: &'static [Mode],
    writable: bool,
}

impl Ladder {
    /// Reflink, then hardlink, then symlink, then copy. Every result is read only.
    pub const AUTO: Self = Self {
        modes: &[Mode::Reflink, Mode::Hardlink, Mode::Symlink, Mode::Copy],
        writable: false,
    };

    /// Real copies the project may write to, for tools that open files for writing.
    pub const COPY: Self = Self {
        modes: &[Mode::Copy],
        writable: true,
    };

    /// Only `mode`, with a read only result.
    #[must_use]
    pub const fn only(mode: Mode) -> Self {
        let modes: &'static [Mode] = match mode {
            Mode::Reflink => &[Mode::Reflink],
            Mode::Hardlink => &[Mode::Hardlink],
            Mode::Symlink => &[Mode::Symlink],
            Mode::Copy => &[Mode::Copy],
        };
        Self {
            modes,
            writable: false,
        }
    }
}

/// What placing one file did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Linked {
    /// The mode that placed it.
    pub mode: Mode,
    /// The bytes copied to place it: the object's size for a copy, nothing otherwise.
    pub bytes_copied: u64,
}

impl Linked {
    /// Adds this file to the work counters.
    pub fn add_to(&self, work: &mut Work) {
        work.files_linked += 1;
        work.bytes_copied += self.bytes_copied;
        let count = match self.mode {
            Mode::Reflink => &mut work.links.reflink,
            Mode::Hardlink => &mut work.links.hardlink,
            Mode::Symlink => &mut work.links.symlink,
            Mode::Copy => &mut work.links.copy,
        };
        *count += 1;
    }
}

type Create = fn(Mode, &Path, &Path) -> io::Result<u64>;

/// The number of files linked at once by [`Linker::link_all`].
const JOBS: usize = 4;

/// Links objects of one tier into a project.
///
/// The first file probes the ladder: a mode that fails is skipped for every later file, so each
/// run pays for a refused mode once. A hardlink refused because the object has reached its
/// filesystem's link limit falls to the next mode for that file only.
#[derive(Debug)]
pub struct Linker<'t> {
    tier: &'t Tier,
    ladder: Ladder,
    floor: AtomicUsize,
    create: Create,
}

impl<'t> Linker<'t> {
    /// A linker from `tier` that uses the modes of `ladder`.
    #[must_use]
    pub const fn new(tier: &'t Tier, ladder: Ladder) -> Self {
        Self {
            tier,
            ladder,
            floor: AtomicUsize::new(0),
            create,
        }
    }

    /// Places the object `digest` at `destination`, replacing what is there so the name always
    /// holds the old file or the new one.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Missing`] when the tier holds no such object, and
    /// [`StoreError::Io`] with the last mode's error when no mode of the ladder works.
    pub fn link(&self, digest: &Digest, destination: &Destination) -> Result<Linked, StoreError> {
        let object = self
            .tier
            .object_path(digest)
            .ok_or(StoreError::Missing { digest: *digest })?;
        let target = destination.path();
        let modes = self.ladder.modes;
        let start = self.floor.load(Ordering::Relaxed).min(modes.len() - 1);
        let mut last = None;
        for (index, mode) in modes.iter().enumerate().skip(start) {
            match self.place(*mode, &object, target) {
                Ok(bytes_copied) => {
                    return Ok(Linked {
                        mode: *mode,
                        bytes_copied,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && !object.exists() => {
                    return Err(StoreError::Missing { digest: *digest });
                }
                Err(error) if error.kind() == io::ErrorKind::TooManyLinks => last = Some(error),
                Err(error) => {
                    self.floor.fetch_max(index + 1, Ordering::Relaxed);
                    last = Some(error);
                }
            }
        }
        Err(io("link", target)(
            last.unwrap_or_else(|| io::ErrorKind::Unsupported.into()),
        ))
    }

    /// Links every pair of object and destination on a small pool of threads, and returns each
    /// result in order. One failure does not stop the others.
    pub fn link_all(&self, files: &[(Digest, Destination)]) -> Vec<Result<Linked, StoreError>> {
        parallel::map(files, JOBS, |(digest, destination)| {
            self.link(digest, destination)
        })
    }

    fn place(&self, mode: Mode, object: &Path, target: &Path) -> io::Result<u64> {
        if matches!(mode, Mode::Hardlink | Mode::Symlink) {
            match (self.create)(mode, object, target) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                done => return done,
            }
        }
        let staging = staging_path(target);
        match fs::remove_file(&staging) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let bytes = (self.create)(mode, object, &staging)?;
        let finished =
            finish(mode, self.ladder.writable, &staging).and_then(|()| replace(&staging, target));
        if finished.is_err() {
            let _ = fs::remove_file(&staging);
        }
        finished.map(|()| bytes)
    }
}

/// The name a file is built under beside its destination before it is renamed over it.
fn staging_path(target: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(".fl-tmp");
    target.with_file_name(name)
}

fn finish(mode: Mode, writable: bool, path: &Path) -> io::Result<()> {
    match mode {
        Mode::Reflink | Mode::Copy => mode::set_writable(path, writable),
        Mode::Hardlink | Mode::Symlink => Ok(()),
    }
}

fn create(mode: Mode, object: &Path, to: &Path) -> io::Result<u64> {
    match mode {
        Mode::Reflink => reflink(object, to).map(|()| 0),
        Mode::Hardlink => fs::hard_link(object, to).map(|()| 0),
        Mode::Symlink => symlink(object, to).map(|()| 0),
        Mode::Copy => fs::copy(object, to),
    }
}

#[cfg(unix)]
fn reflink(object: &Path, to: &Path) -> io::Result<()> {
    reflink_copy::reflink(object, to)
}

#[cfg(windows)]
fn reflink(_object: &Path, _to: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(unix)]
fn symlink(object: &Path, to: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(object, to)
}

#[cfg(windows)]
fn symlink(object: &Path, to: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_file(object, to)
}

#[cfg(unix)]
fn replace(staging: &Path, target: &Path) -> io::Result<()> {
    fs::rename(staging, target)
}

/// Renames `staging` over `target`. Windows refuses to rename over a read only file, and a
/// hardlinked destination shares its read only attribute with the object, so the attribute is
/// cleared through a handle, the rename runs, and the same handle sets it again on the file that
/// was replaced.
#[cfg(windows)]
fn replace(staging: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const READ_AND_WRITE_ATTRIBUTES: u32 = 0x80 | 0x100;
    match fs::rename(staging, target) {
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && fs::symlink_metadata(target).is_ok_and(|m| m.permissions().readonly()) =>
        {
            let replaced = fs::OpenOptions::new()
                .access_mode(READ_AND_WRITE_ATTRIBUTES)
                .open(target)?;
            mode::set_handle_readonly(&replaced, false)?;
            let renamed = fs::rename(staging, target);
            mode::set_handle_readonly(&replaced, true)?;
            renamed
        }
        done => done,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::data_dir::DataDir;
    use crate::tier::Sharing;

    thread_local! {
        static TRIED: RefCell<Vec<Mode>> = const { RefCell::new(Vec::new()) };
    }

    fn record(mode: Mode) {
        TRIED.with(|tried| tried.borrow_mut().push(mode));
    }

    fn without_reflink(mode: Mode, object: &Path, to: &Path) -> io::Result<u64> {
        record(mode);
        if mode == Mode::Reflink {
            return Err(io::ErrorKind::Unsupported.into());
        }
        create(mode, object, to)
    }

    fn at_link_limit(mode: Mode, object: &Path, to: &Path) -> io::Result<u64> {
        if mode == Mode::Hardlink {
            record(mode);
            return Err(io::ErrorKind::TooManyLinks.into());
        }
        without_reflink(mode, object, to)
    }

    fn refusing_all(mode: Mode, _: &Path, _: &Path) -> io::Result<u64> {
        record(mode);
        Err(io::Error::other(format!("{mode:?} refused")))
    }

    fn tried() -> Vec<Mode> {
        TRIED.with(|tried| tried.borrow_mut().drain(..).collect())
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        tier: Tier,
        data: DataDir,
        digest: Digest,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"bytes").unwrap();
        let digest = tier.ingest_file(&source).unwrap().digest;
        let data = DataDir::new(temp.path().join("data"));
        Fixture {
            _temp: temp,
            tier,
            data,
            digest,
        }
    }

    fn destination(fixture: &Fixture, name: &str) -> Destination {
        let dir = fl_model::path::DataPath::new("ds").unwrap();
        let file = fl_model::path::DataPath::new(name).unwrap();
        fixture.data.destination(&dir, &file).unwrap()
    }

    fn linker(fixture: &Fixture, ladder: Ladder, create: Create) -> Linker<'_> {
        let mut linker = Linker::new(&fixture.tier, ladder);
        linker.create = create;
        linker
    }

    #[test]
    fn a_refused_mode_is_skipped_for_every_later_file() {
        let fixture = fixture();
        let linker = linker(&fixture, Ladder::AUTO, without_reflink);
        let first = linker
            .link(&fixture.digest, &destination(&fixture, "a"))
            .unwrap();
        assert_eq!(first.mode, Mode::Hardlink);
        assert_eq!(tried(), [Mode::Reflink, Mode::Hardlink]);
        linker
            .link(&fixture.digest, &destination(&fixture, "b"))
            .unwrap();
        assert_eq!(tried(), [Mode::Hardlink]);
    }

    #[test]
    fn a_link_limit_moves_on_for_that_file_only() {
        let fixture = fixture();
        let linker = linker(&fixture, Ladder::AUTO, at_link_limit);
        let first = linker
            .link(&fixture.digest, &destination(&fixture, "a"))
            .unwrap();
        assert_eq!(first.mode, Mode::Symlink);
        assert_eq!(tried(), [Mode::Reflink, Mode::Hardlink, Mode::Symlink]);
        linker
            .link(&fixture.digest, &destination(&fixture, "b"))
            .unwrap();
        assert_eq!(tried(), [Mode::Hardlink, Mode::Symlink]);
    }

    #[test]
    fn when_every_mode_fails_the_last_error_is_returned() {
        let fixture = fixture();
        let linker = linker(&fixture, Ladder::AUTO, refusing_all);
        let error = linker
            .link(&fixture.digest, &destination(&fixture, "a"))
            .unwrap_err();
        assert!(error.to_string().ends_with("Copy refused"), "{error}");
        assert_eq!(tried(), Ladder::AUTO.modes);
        linker
            .link(&fixture.digest, &destination(&fixture, "b"))
            .unwrap_err();
        assert_eq!(tried(), [Mode::Copy]);
    }

    #[test]
    fn an_object_the_tier_does_not_hold_is_missing() {
        let fixture = fixture();
        let linker = Linker::new(&fixture.tier, Ladder::AUTO);
        let absent = Digest::Blake3([7; 32]);
        let error = linker
            .link(&absent, &destination(&fixture, "a"))
            .unwrap_err();
        assert!(matches!(error, StoreError::Missing { .. }), "{error}");
        assert!(tried().is_empty());
    }

    #[test]
    fn counters_add_up_by_mode() {
        let mut work = Work::default();
        for (mode, bytes_copied) in [(Mode::Hardlink, 0), (Mode::Copy, 5), (Mode::Copy, 7)] {
            Linked { mode, bytes_copied }.add_to(&mut work);
        }
        assert_eq!((work.files_linked, work.bytes_copied), (3, 12));
        assert_eq!((work.links.hardlink, work.links.copy), (1, 2));
    }
}
