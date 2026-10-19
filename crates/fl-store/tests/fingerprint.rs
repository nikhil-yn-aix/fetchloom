//! Fingerprints and hashing through the public interface: identity, and files that cannot be read.

use std::error::Error;
use std::fs;
use std::thread::sleep;
use std::time::Duration;

use fl_store::fingerprint::Fingerprint;
#[cfg(unix)]
use fl_store::fingerprint::hash_file;
#[cfg(unix)]
use fl_store::tier::{Sharing, Tier};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn identity_tells_two_files_apart_and_one_file_from_itself() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let first = temp.path().join("first");
    fs::write(&first, b"same bytes")?;
    sleep(Duration::from_millis(50));
    let second = temp.path().join("second");
    fs::write(&second, b"same bytes")?;
    let one = Fingerprint::of(&first)?;
    let other = Fingerprint::of(&second)?;
    assert_eq!(one.size, other.size);
    assert_ne!(one.identity, other.identity);
    assert_eq!(one.identity, Fingerprint::of(&first)?.identity);
    assert_ne!(one.identity, 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn identity_is_the_device_and_inode() -> TestResult {
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let path = temp.path().join("file");
    fs::write(&path, b"x")?;
    let metadata = fs::symlink_metadata(&path)?;
    let expected = (u128::from(metadata.dev()) << 64) | u128::from(metadata.ino());
    assert_eq!(Fingerprint::of(&path)?.identity, expected);
    Ok(())
}

#[cfg(unix)]
#[test]
fn hashing_a_directory_fails_instead_of_looping() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    assert!(hash_file(temp.path()).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn ingesting_a_directory_fails_instead_of_looping() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let directory = temp.path().join("directory");
    fs::create_dir(&directory)?;
    assert!(tier.ingest_file(&directory).is_err());
    assert_eq!(fs::read_dir(temp.path().join("tier/v1/tmp"))?.count(), 0);
    Ok(())
}
