//! Fingerprints: what a placed file looked like, so a later check can tell a change cheaply.

use std::fs::{self, File, Metadata};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fl_model::digest::Digest;

use crate::error::{StoreError, io};
use crate::parallel;
use crate::platform;
use crate::staged::BUFFER;

/// The number of directories listed at once by [`scan`].
const JOBS: usize = 8;

/// What a file looked like: its size, modification time and identity.
///
/// The identity is the device and inode on unix. On windows it is the creation time, since the
/// file index needs a handle per file and a directory listing does not carry it. A file whose
/// fingerprint moved is hashed again; one whose fingerprint held is trusted.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Fingerprint {
    /// The size in bytes.
    pub size: u64,
    /// The modification time in nanoseconds since the unix epoch, or 0 before it.
    pub modified: u128,
    /// The device and inode on unix, the creation time in nanoseconds on windows.
    pub identity: u128,
}

impl Fingerprint {
    /// The fingerprint of the file at `path`, without following a symlink.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when its metadata cannot be read.
    pub fn of(path: &Path) -> Result<Self, StoreError> {
        let metadata = fs::symlink_metadata(path).map_err(io("read metadata of", path))?;
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified: nanos(metadata.modified().ok()),
            identity: platform::identity(metadata),
        }
    }
}

/// Every file and symlink below `dir`, as its path relative to `dir` written with `/`, with its
/// fingerprint, sorted by path. Directories are listed in parallel, one depth at a time.
///
/// Metadata comes from the directory listings: on windows it costs nothing beyond the listing,
/// and on unix one `fstatat` per entry. NTFS refreshes a listed size and time only for the name a
/// file was written through, so an edit made through another hardlink is not seen here.
///
/// # Errors
///
/// Returns [`StoreError::Io`] when a directory cannot be listed.
pub fn scan(dir: &Path) -> Result<Vec<(String, Fingerprint)>, StoreError> {
    let mut found = Vec::new();
    let mut level = vec![(dir.to_owned(), String::new())];
    while !level.is_empty() {
        let mut next = Vec::new();
        for listed in parallel::map(&level, JOBS, |(path, prefix)| list(path, prefix)) {
            let (files, dirs) = listed?;
            found.extend(files);
            next.extend(dirs);
        }
        level = next;
    }
    found.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

type Listed = (Vec<(String, Fingerprint)>, Vec<(PathBuf, String)>);

fn list(path: &Path, prefix: &str) -> Result<Listed, StoreError> {
    let (mut files, mut dirs) = (Vec::new(), Vec::new());
    for entry in fs::read_dir(path).map_err(io("list", path))? {
        let entry = entry.map_err(io("list", path))?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        let kind = entry
            .file_type()
            .map_err(io("read the type of", &entry.path()))?;
        if kind.is_dir() {
            dirs.push((entry.path(), format!("{name}/")));
        } else {
            let metadata = entry
                .metadata()
                .map_err(io("read metadata of", &entry.path()))?;
            files.push((name, Fingerprint::from_metadata(&metadata)));
        }
    }
    Ok((files, dirs))
}

/// The BLAKE3 digest of the file at `path`, for a file whose fingerprint moved.
///
/// # Errors
///
/// Returns [`StoreError::Io`] when the file cannot be read.
pub fn hash_file(path: &Path) -> Result<Digest, StoreError> {
    let file = File::open(path).map_err(io("open", path))?;
    let length = file.metadata().map_err(io("read metadata of", path))?.len();
    let size = usize::try_from(length).map_or(BUFFER, |length| length.clamp(1, BUFFER));
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut BufReader::with_capacity(size, file), &mut hasher).map_err(io("read", path))?;
    Ok(Digest::Blake3(*hasher.finalize().as_bytes()))
}

/// Nanoseconds since the unix epoch, or 0 for a time before it or none.
pub(crate) fn nanos(time: Option<SystemTime>) -> u128 {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_listing_agrees_with_reading_each_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        fs::create_dir_all(root.join("a").join("b")).unwrap();
        fs::write(root.join("top"), b"1").unwrap();
        fs::write(root.join("a").join("b").join("deep"), b"22").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("top"), root.join("a").join("link")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(root.join("top"), root.join("a").join("link")).unwrap();
        let scanned = scan(&root).unwrap();
        let names: Vec<&str> = scanned.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a/b/deep", "a/link", "top"]);
        for (name, fingerprint) in &scanned {
            assert_eq!(
                *fingerprint,
                Fingerprint::of(&root.join(name)).unwrap(),
                "{name}"
            );
        }
    }

    #[test]
    fn an_edit_moves_the_fingerprint() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("file");
        fs::write(&path, b"abc").unwrap();
        let before = Fingerprint::of(&path).unwrap();
        fs::write(&path, b"xyz").unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000))
            .unwrap();
        drop(file);
        let same_size = Fingerprint::of(&path).unwrap();
        assert_eq!(same_size.size, before.size);
        assert_ne!(same_size, before);
        fs::write(&path, b"longer").unwrap();
        assert_ne!(Fingerprint::of(&path).unwrap().size, before.size);
    }
}
