//! File removal shared by gc, verify and roots.

use std::fs;
use std::io;
use std::path::Path;

use crate::error::{StoreError, io};

/// Removes the file at `path`. A file that is already gone counts as removed, since another
/// process cleaning the same tier may have taken it first.
pub(crate) fn remove(path: &Path) -> Result<(), StoreError> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io("remove", path)(error)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_goes_a_missing_one_is_fine_and_a_directory_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file");
        fs::write(&file, b"x").unwrap();
        remove(&file).unwrap();
        assert!(!file.exists());
        remove(&file).unwrap();
        let error = remove(temp.path()).unwrap_err();
        assert!(
            matches!(error, StoreError::Io { op: "remove", .. }),
            "{error}"
        );
        assert!(temp.path().is_dir());
    }
}
