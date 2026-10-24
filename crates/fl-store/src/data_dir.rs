//! A project's data directory: the only place links land, and the only way to name a place there.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use fl_model::path::DataPath;

use crate::error::{StoreError, io};

/// A project's data directory.
#[derive(Debug)]
pub struct DataDir {
    root: PathBuf,
    made: Mutex<HashSet<PathBuf>>,
}

/// A file's place inside a data directory, whose parents exist as real directories.
///
/// Only [`DataDir::destination`] makes one, from validated dataset paths, so no destination can
/// lie outside its data directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Destination {
    path: PathBuf,
}

impl Destination {
    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl DataDir {
    /// The data directory at `root`. The root itself may be a symlink, for a project that keeps
    /// its data elsewhere; nothing is created until a destination needs it.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            made: Mutex::new(HashSet::new()),
        }
    }

    /// The place of `file` in the dataset directory `dir`, with its parents created.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unrepresentable`] for a component this platform cannot hold,
    /// [`StoreError::NotADirectory`] when a parent below the root exists as something other than
    /// a real directory, such as a symlink, and [`StoreError::Io`] when a parent cannot be made.
    pub fn destination(&self, dir: &DataPath, file: &DataPath) -> Result<Destination, StoreError> {
        representable(dir, file)?;
        let components: Vec<&str> = dir
            .as_str()
            .split('/')
            .chain(file.as_str().split('/'))
            .collect();
        self.make(&self.root)?;
        let mut path = self.root.clone();
        let (name, parents) = components.split_last().unwrap_or((&"", &[]));
        for parent in parents {
            path.push(parent);
            self.make(&path)?;
        }
        path.push(name);
        Ok(Destination { path })
    }

    fn make(&self, path: &Path) -> Result<(), StoreError> {
        if self.remembered(path) {
            return Ok(());
        }
        let made = if *path == self.root {
            fs::create_dir_all(path)
        } else {
            fs::create_dir(path)
        };
        match made {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = if *path == self.root {
                    fs::metadata(path)
                } else {
                    fs::symlink_metadata(path)
                }
                .map_err(io("read metadata of", path))?;
                if !metadata.is_dir() {
                    return Err(StoreError::NotADirectory {
                        path: path.to_owned(),
                    });
                }
            }
            Err(error) => return Err(io("create directory", path)(error)),
        }
        self.made
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned());
        Ok(())
    }

    fn remembered(&self, path: &Path) -> bool {
        self.made
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(path)
    }
}

/// Removes the file fetchloom placed at `path`, read only as it is, without making the object
/// it was linked from writable. A file that is not there is not an error.
///
/// # Errors
///
/// Returns [`StoreError::Io`] when the file cannot be removed.
pub fn remove(path: &Path) -> Result<(), StoreError> {
    match crate::platform::remove(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io("remove", path)(error)),
        _ => Ok(()),
    }
}

/// Refuses `file` of the dataset directory `dir` when this platform cannot hold one of its
/// components, so a listing can be checked before anything is written.
///
/// # Errors
///
/// Returns [`StoreError::Unrepresentable`] naming the path and the reason.
pub fn representable(dir: &DataPath, file: &DataPath) -> Result<(), StoreError> {
    if cfg!(windows) {
        for component in dir.as_str().split('/').chain(file.as_str().split('/')) {
            if let Some(reason) = windows_refusal(component) {
                return Err(StoreError::Unrepresentable {
                    path: format!("{dir}/{file}"),
                    reason,
                });
            }
        }
    }
    Ok(())
}

const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
const NUMBERED: [&str; 2] = ["COM", "LPT"];
const NUMBERS: [&str; 13] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "\u{b9}", "\u{b2}", "\u{b3}",
];

/// Why windows cannot hold a path component, or `None` when it can.
fn windows_refusal(component: &str) -> Option<&'static str> {
    if component.contains(['<', '>', ':', '"', '|', '?', '*']) {
        return Some("it holds a character windows does not allow in names: < > : \" | ? *");
    }
    if component.ends_with(['.', ' ']) {
        return Some("a name that ends in a dot or a space cannot exist on windows");
    }
    let stem = component.split('.').next().unwrap_or(component).trim_end();
    let reserved = RESERVED.iter().any(|name| stem.eq_ignore_ascii_case(name))
        || NUMBERED.iter().any(|prefix| {
            stem.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                && NUMBERS.contains(&&stem[prefix.len()..])
        });
    reserved.then_some("it is a device name that windows reserves")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> DataPath {
        DataPath::new(text).unwrap()
    }

    #[test]
    fn destinations_join_dataset_paths_below_the_root() {
        let temp = tempfile::tempdir().unwrap();
        let data = DataDir::new(temp.path().join("data"));
        let destination = data
            .destination(&path("eeg"), &path("sub-01/x.edf"))
            .unwrap();
        let expected = temp
            .path()
            .join("data")
            .join("eeg")
            .join("sub-01")
            .join("x.edf");
        assert_eq!(destination.path(), expected);
        assert!(expected.parent().unwrap().is_dir());
        assert!(!expected.exists());
    }

    #[test]
    fn a_parent_that_is_not_a_real_directory_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let root = temp.path().join("data");
        fs::create_dir_all(root.join("eeg")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, root.join("eeg").join("sub")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&elsewhere, root.join("eeg").join("sub")).unwrap();
        fs::write(root.join("eeg").join("file"), b"").unwrap();
        let data = DataDir::new(root);
        for file in ["sub/x", "file/x"] {
            let error = data.destination(&path("eeg"), &path(file)).unwrap_err();
            assert!(matches!(error, StoreError::NotADirectory { .. }), "{error}");
        }
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_root_is_followed() {
        let temp = tempfile::tempdir().unwrap();
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let root = temp.path().join("data");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, &root).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&elsewhere, &root).unwrap();
        DataDir::new(root)
            .destination(&path("eeg"), &path("x"))
            .unwrap();
        assert!(elsewhere.join("eeg").is_dir());
    }

    #[test]
    fn windows_refuses_reserved_names_and_characters() {
        for refused in [
            "CON",
            "nul",
            "Aux.txt",
            "com1",
            "LPT9.log",
            "com\u{b9}",
            "a:b",
            "a?",
            "x.",
            "x ",
            "a|b",
            "con .txt",
        ] {
            assert!(windows_refusal(refused).is_some(), "{refused}");
        }
        for allowed in [
            "CONSOLE", "com", "com10", "lpt", "nul-file", "x.y", ".hidden", "a b",
        ] {
            assert!(windows_refusal(allowed).is_none(), "{allowed}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn an_unrepresentable_destination_names_the_path_and_reason() {
        let temp = tempfile::tempdir().unwrap();
        let data = DataDir::new(temp.path().join("data"));
        let error = data
            .destination(&path("eeg"), &path("sub/aux.txt"))
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "`eeg/sub/aux.txt` cannot exist on this filesystem: it is a device name that windows reserves"
        );
        assert_eq!(
            error.kind(),
            fl_model::error::ErrorKind::LocalUnrepresentable
        );
    }

    #[test]
    fn removing_a_missing_file_is_fine_and_a_directory_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        remove(&temp.path().join("gone")).unwrap();
        let dir = temp.path().join("dir");
        fs::create_dir(&dir).unwrap();
        assert!(matches!(
            remove(&dir).unwrap_err(),
            StoreError::Io { op: "remove", .. }
        ));
    }

    #[test]
    fn a_parent_replaced_by_a_regular_file_reports_the_failed_create() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let data = DataDir::new(root.clone());
        data.destination(&path("eeg"), &path("a/x")).unwrap();
        fs::remove_dir_all(root.join("eeg")).unwrap();
        fs::write(root.join("eeg"), b"").unwrap();
        let error = data.destination(&path("eeg"), &path("b/x")).unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Io {
                    op: "create directory",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn a_second_data_dir_over_the_same_directories_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        for _ in 0..2 {
            let data = DataDir::new(root.clone());
            let destination = data.destination(&path("eeg"), &path("sub/x.edf")).unwrap();
            assert_eq!(
                destination.path(),
                root.join("eeg").join("sub").join("x.edf")
            );
        }
    }

    #[test]
    fn a_regular_file_where_a_dataset_directory_belongs_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("eeg"), b"").unwrap();
        let error = DataDir::new(root)
            .destination(&path("eeg"), &path("x"))
            .unwrap_err();
        assert!(matches!(error, StoreError::NotADirectory { .. }), "{error}");
    }

    #[test]
    fn a_root_that_is_a_regular_file_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        fs::write(&root, b"").unwrap();
        let error = DataDir::new(root)
            .destination(&path("eeg"), &path("x"))
            .unwrap_err();
        assert!(matches!(error, StoreError::NotADirectory { .. }), "{error}");
    }
}
