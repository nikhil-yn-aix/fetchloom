//! Modes the store sets on what it creates. The process umask is never trusted.

use std::fs;
use std::io;
use std::path::Path;

use crate::platform;
use crate::tier::Sharing;

/// Creates the directory `path` and sets the mode `sharing` asks for. An existing directory is
/// left as it is, since another user of a shared tier may own it.
pub(crate) fn create_dir(path: &Path, sharing: Sharing) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => platform::set_dir_mode(path, sharing),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_parent_is_an_error_and_an_existing_directory_is_left() {
        let temp = tempfile::tempdir().unwrap();
        let error = create_dir(&temp.path().join("a").join("b"), Sharing::Private).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let dir = temp.path().join("a");
        create_dir(&dir, Sharing::Private).unwrap();
        create_dir(&dir, Sharing::Private).unwrap();
        assert!(dir.is_dir());
    }
}
