//! Placing objects in a project with the best link mode a filesystem allows.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use fl_model::digest::Digest;
use fl_model::document::Work;

use crate::data_dir::Destination;
use crate::error::{StoreError, io};
use crate::parallel;
use crate::platform;
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

/// The number of threads [`Linker::link_all`] links on.
const JOBS: usize = 8;

/// The files a thread links in one go, neighbours by path, so threads mostly work in different
/// directories and do not queue on one directory's lock.
const CHUNK: usize = 32;

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
        let span = tracing::debug_span!(
            "link",
            %digest,
            target = %target.display(),
            mode = tracing::field::Empty,
            bytes_copied = tracing::field::Empty,
        )
        .entered();
        let modes = self.ladder.modes;
        let start = self.floor.load(Ordering::Relaxed).min(modes.len() - 1);
        let mut last = None;
        for (index, mode) in modes.iter().enumerate().skip(start) {
            match self.place(*mode, &object, target) {
                Ok(bytes_copied) => {
                    span.record("mode", tracing::field::debug(mode));
                    span.record("bytes_copied", bytes_copied);
                    return Ok(Linked {
                        mode: *mode,
                        bytes_copied,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && !object.exists() => {
                    return Err(StoreError::Missing { digest: *digest });
                }
                Err(error) if moves_on(&error) => {
                    tracing::debug!(?mode, %error, "next mode for this file only");
                    last = Some(error);
                }
                Err(error) => {
                    tracing::debug!(?mode, %error, "mode refused, next mode from now on");
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
    ///
    /// Each thread takes a run of files that sit next to each other by path, so threads rarely
    /// create names in the same directory at the same moment.
    pub fn link_all(&self, files: &[(Digest, Destination)]) -> Vec<Result<Linked, StoreError>> {
        let mut order: Vec<usize> = (0..files.len()).collect();
        order.sort_by(|a, b| files[*a].1.path().cmp(files[*b].1.path()));
        let chunks: Vec<&[usize]> = order.chunks(CHUNK).collect();
        let linked = parallel::map(&chunks, JOBS, |chunk| {
            chunk
                .iter()
                .map(|index| {
                    let (digest, destination) = &files[*index];
                    (*index, self.link(digest, destination))
                })
                .collect::<Vec<_>>()
        });
        let mut results: Vec<_> = linked.into_iter().flatten().collect();
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
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
        let finished = finish(mode, self.ladder.writable, &staging)
            .and_then(|()| platform::replace(&staging, target));
        let _ = fs::remove_file(&staging);
        finished.map(|()| bytes)
    }
}

/// The name a file is built under beside its destination before it is renamed over it.
/// Whether an error concerns this one file or the disk rather than the mode, so the next mode
/// is tried for this file while later files still start from the same mode.
fn moves_on(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TooManyLinks
            | io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::NotADirectory
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::DirectoryNotEmpty
    )
}

fn staging_path(target: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(".fl-tmp");
    target.with_file_name(name)
}

fn finish(mode: Mode, writable: bool, path: &Path) -> io::Result<()> {
    match mode {
        Mode::Reflink | Mode::Copy => platform::set_writable(path, writable),
        Mode::Hardlink | Mode::Symlink => Ok(()),
    }
}

fn create(mode: Mode, object: &Path, to: &Path) -> io::Result<u64> {
    match mode {
        Mode::Reflink => platform::reflink(object, to).map(|()| 0),
        Mode::Hardlink => fs::hard_link(object, to).map(|()| 0),
        Mode::Symlink => platform::symlink(object, to).map(|()| 0),
        Mode::Copy => fs::copy(object, to),
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

    thread_local! {
        static HARDLINK_FAILS: std::cell::Cell<io::ErrorKind> =
            const { std::cell::Cell::new(io::ErrorKind::TooManyLinks) };
    }

    fn hardlink_failing(mode: Mode, object: &Path, to: &Path) -> io::Result<u64> {
        if mode == Mode::Hardlink {
            record(mode);
            return Err(HARDLINK_FAILS.with(std::cell::Cell::get).into());
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
    fn a_link_limit_or_a_full_disk_moves_on_for_that_file_only() {
        for kind in [
            io::ErrorKind::TooManyLinks,
            io::ErrorKind::StorageFull,
            io::ErrorKind::QuotaExceeded,
            io::ErrorKind::NotADirectory,
            io::ErrorKind::IsADirectory,
            io::ErrorKind::DirectoryNotEmpty,
        ] {
            HARDLINK_FAILS.with(|fails| fails.set(kind));
            let fixture = fixture();
            let linker = linker(&fixture, Ladder::AUTO, hardlink_failing);
            let first = linker
                .link(&fixture.digest, &destination(&fixture, "a"))
                .unwrap();
            assert_eq!(first.mode, Mode::Symlink, "{kind:?}");
            assert_eq!(tried(), [Mode::Reflink, Mode::Hardlink, Mode::Symlink]);
            linker
                .link(&fixture.digest, &destination(&fixture, "b"))
                .unwrap();
            assert_eq!(tried(), [Mode::Hardlink, Mode::Symlink], "{kind:?}");
        }
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
