//! A project on disk: `data.toml`, `data.lock`, the data directory and `.fetchloom/`, and the
//! lock held while a command changes them.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use fl_model::error::ErrorKind;
use fl_model::lock::Lock;
use fl_model::manifest::Manifest;
use serde::{Deserialize, Serialize};

use crate::failure::Failure;

/// The project's file of people's choices.
pub(crate) const MANIFEST: &str = "data.toml";
/// The project's file of what those choices resolved to.
pub(crate) const LOCK: &str = "data.lock";
const STATE_DIR: &str = ".fetchloom";

/// A project directory and its files.
#[derive(Debug)]
pub(crate) struct Project {
    root: PathBuf,
}

impl Project {
    /// The project at `root`, which holds `data.toml`.
    pub(crate) const fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The nearest project at or above `start`.
    pub(crate) fn find(start: &Path) -> Result<Self, Failure> {
        let start =
            std::path::absolute(start).map_err(|error| Failure::io("resolve", start, &error))?;
        start
            .ancestors()
            .find(|dir| dir.join(MANIFEST).is_file())
            .map(|dir| Self::at(dir.to_path_buf()))
            .ok_or_else(|| {
                Failure::new(
                    ErrorKind::ProjectMissing,
                    format!(
                        "no {MANIFEST} in `{}` or any directory above it",
                        start.display()
                    ),
                )
                .help("run `fl init` to start a project here, or `fl add REF` to start one with a dataset")
            })
    }

    /// The directory holding `data.toml`.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The path of `data.toml`.
    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST)
    }

    /// The path of `data.lock`.
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK)
    }

    /// The text of `data.toml`.
    pub(crate) fn manifest_text(&self) -> Result<String, Failure> {
        let path = self.manifest_path();
        fs::read_to_string(&path).map_err(|error| Failure::io("read", &path, &error))
    }

    /// `data.toml`, read and checked.
    pub(crate) fn manifest(&self) -> Result<Manifest, Failure> {
        let text = self.manifest_text()?;
        Manifest::from_toml(&text).map_err(|error| {
            let span = error.span.clone();
            let failure = Failure::from(error);
            match span {
                Some(span) => failure.snippet(MANIFEST, &text, span),
                None => failure,
            }
        })
    }

    /// `data.lock`, or `None` when there is none yet.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Failure> {
        let path = self.lock_path();
        match fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Lock::from_toml(&text)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Failure::io("read", &path, &error)),
        }
    }

    /// Writes `lock` as `data.lock` when its text differs from the file's.
    pub(crate) fn write_lock(&self, lock: &Lock) -> Result<bool, Failure> {
        write_if_changed(&self.lock_path(), &lock.to_toml())
    }

    /// Writes `text` as `data.toml`.
    pub(crate) fn write_manifest(&self, text: &str) -> Result<(), Failure> {
        write_if_changed(&self.manifest_path(), text).map(|_| ())
    }

    /// The data directory named in `manifest`, refused when any part of the way to it from the
    /// project root is a symlink, which a cloned repository could point anywhere.
    pub(crate) fn data_dir(&self, manifest: &Manifest) -> Result<PathBuf, Failure> {
        let mut path = self.root.clone();
        for component in manifest.project.data_dir.as_str().split('/') {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(Failure::new(
                        ErrorKind::ProjectInvalidManifest,
                        format!(
                            "data.toml: `data_dir` passes through the symlink `{}`",
                            path.display()
                        ),
                    )
                    .subject(MANIFEST)
                    .help("make it a real folder, or name another `data_dir` in data.toml"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(Failure::io("inspect", &path, &error)),
            }
        }
        Ok(self.root.join(manifest.project.data_dir.as_str()))
    }

    /// Removes `.fetchloom/` and `data.lock`, for an add that failed in a folder that had no
    /// project before it. Errors are ignored: what is left is only the start of a project.
    pub(crate) fn discard_state(&self) {
        let _ = fs::remove_file(self.lock_path());
        let _ = fs::remove_dir_all(self.root.join(STATE_DIR));
    }

    fn state_dir(&self) -> Result<PathBuf, Failure> {
        let dir = self.root.join(STATE_DIR);
        fs::create_dir_all(&dir).map_err(|error| Failure::io("create", &dir, &error))?;
        Ok(dir)
    }

    /// Takes the project lock, waiting for another `fl` that holds it. `waiting` is called once
    /// before waiting.
    pub(crate) fn hold(&self, waiting: impl FnOnce()) -> Result<Held, Failure> {
        let path = self.state_dir()?.join("lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| Failure::io("open", &path, &error))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                waiting();
                file.lock()
                    .map_err(|error| Failure::io("lock", &path, &error))?;
            }
            Err(TryLockError::Error(error)) => return Err(Failure::io("lock", &path, &error)),
        }
        Ok(Held { _file: file })
    }

    /// The fingerprints recorded when files were placed, and when they were recorded.
    pub(crate) fn state(&self) -> Result<State, Failure> {
        let path = self.root.join(STATE_DIR).join("state");
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
            Err(error) => return Err(Failure::io("read", &path, &error)),
        };
        let mut state: State = serde_json::from_slice(&bytes).unwrap_or_default();
        state.written = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        Ok(state)
    }

    /// Records the fingerprints of placed files. With `touch`, the file is written even when
    /// its text did not change, so its time moves past the files it vouches for.
    pub(crate) fn write_state(&self, state: &State, touch: bool) -> Result<(), Failure> {
        let path = self.state_dir()?.join("state");
        let text = serde_json::to_string(state)
            .map_err(|error| Failure::new(ErrorKind::LocalIo, error.to_string()))?;
        if touch {
            write(&path, &text)
        } else {
            write_if_changed(&path, &text).map(|_| ())
        }
    }
}

/// The project lock, released when dropped.
pub(crate) struct Held {
    _file: File,
}

/// What each placed file looked like, by its path relative to the data directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    /// The files.
    pub(crate) files: BTreeMap<String, Placed>,
    /// When the state was written, in nanoseconds since the unix epoch by the file's own time.
    #[serde(skip)]
    pub(crate) written: u128,
}

impl State {
    /// Whether the file at `key`, holding `blake3` by the lock, can be taken as unchanged from
    /// its fingerprint `print` alone. A file modified in the same clock tick as the state was
    /// written, or later, may have changed after it was recorded, so it is not trusted.
    pub(crate) fn trusts(
        &self,
        key: &str,
        blake3: &[u8; 32],
        print: &fl_store::fingerprint::Fingerprint,
    ) -> bool {
        print.modified < self.written
            && self
                .files
                .get(key)
                .is_some_and(|placed| placed.matches(blake3, print))
    }
}

/// A placed file: the object it was placed from and its fingerprint then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Placed {
    /// The object's BLAKE3 digest, as hex.
    pub(crate) blake3: String,
    /// The size in bytes.
    pub(crate) size: u64,
    /// The modification time in nanoseconds, as text since JSON numbers stop at 2^53.
    pub(crate) modified: String,
    /// The identity, as text for the same reason.
    pub(crate) identity: String,
}

impl Placed {
    /// The record for a file holding `blake3` whose fingerprint is `print`.
    pub(crate) fn new(blake3: &[u8; 32], print: &fl_store::fingerprint::Fingerprint) -> Self {
        Self {
            blake3: hex(blake3),
            size: print.size,
            modified: print.modified.to_string(),
            identity: print.identity.to_string(),
        }
    }

    /// Whether `print` and `blake3` match this record.
    pub(crate) fn matches(
        &self,
        blake3: &[u8; 32],
        print: &fl_store::fingerprint::Fingerprint,
    ) -> bool {
        *self == Self::new(blake3, print)
    }
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

fn write_if_changed(path: &Path, text: &str) -> Result<bool, Failure> {
    if fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    write(path, text)?;
    Ok(true)
}

fn write(path: &Path, text: &str) -> Result<(), Failure> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|error| Failure::io("write", &tmp, &error))?;
    fs::rename(&tmp, path).map_err(|error| Failure::io("replace", path, &error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_is_found_from_a_directory_below_it() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(MANIFEST), "").unwrap();
        let below = temp.path().join("a").join("b");
        fs::create_dir_all(&below).unwrap();
        let found = Project::find(&below).unwrap();
        assert_eq!(found.root, std::path::absolute(temp.path()).unwrap());
    }

    #[test]
    fn no_project_is_project_missing_with_help() {
        let temp = tempfile::tempdir().unwrap();
        let failure = Project::find(temp.path()).unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::ProjectMissing);
        assert!(failure.0.help.unwrap().contains("fl init"));
    }

    #[test]
    fn a_second_holder_waits_and_is_told_first() {
        let temp = tempfile::tempdir().unwrap();
        let project = Project::at(temp.path().to_owned());
        let held = project.hold(|| panic!("nothing held the lock")).unwrap();
        let (told, receive) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| project.hold(move || told.send(()).unwrap()).map(drop));
            receive.recv().unwrap();
            drop(held);
            waiter.join().unwrap().unwrap();
        });
    }

    #[test]
    fn state_round_trips_and_an_unreadable_one_counts_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let project = Project::at(temp.path().to_owned());
        assert_eq!(project.state().unwrap(), State::default());
        let mut state = State::default();
        state.files.insert(
            "iris/iris.csv".to_owned(),
            Placed {
                blake3: "ab".to_owned(),
                size: 1,
                modified: u128::MAX.to_string(),
                identity: "7".to_owned(),
            },
        );
        project.write_state(&state, false).unwrap();
        let read = project.state().unwrap();
        assert_eq!(read.files, state.files);
        assert!(read.written > 0);
        fs::write(temp.path().join(STATE_DIR).join("state"), "{").unwrap();
        assert!(project.state().unwrap().files.is_empty());
    }

    #[test]
    fn a_file_changed_in_the_tick_the_state_was_written_is_not_trusted() {
        let print = |modified| fl_store::fingerprint::Fingerprint {
            size: 3,
            modified,
            identity: 9,
        };
        let blake3 = [7; 32];
        let mut state = State::default();
        state
            .files
            .insert("a/x".to_owned(), Placed::new(&blake3, &print(100)));
        state.written = 101;
        assert!(state.trusts("a/x", &blake3, &print(100)));
        assert!(!state.trusts("a/x", &[8; 32], &print(100)));
        assert!(!state.trusts("a/y", &blake3, &print(100)));
        state.written = 100;
        assert!(!state.trusts("a/x", &blake3, &print(100)));
        state.written = 99;
        assert!(!state.trusts("a/x", &blake3, &print(100)));
    }

    #[test]
    fn hex_is_lowercase_and_two_digits_per_byte() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
