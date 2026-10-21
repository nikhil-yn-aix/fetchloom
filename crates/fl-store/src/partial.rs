//! Unfinished downloads: their bytes and their record in a tier's `partial/`, held by one
//! writer at a time and kept across runs so a later run continues them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use fl_model::digest::Digest;
use fl_model::partial::PartialRecord;

use crate::error::{StoreError, io};
use crate::layout::entries;
use crate::platform;
use crate::staged::{Scratch, create_unique};
use crate::tier::{Ingested, Tier};

/// A partial untouched for this long belongs to a writer that is gone, so another may take it.
/// A running download writes into it far more often, unless it is allowed to stay silent for
/// longer than this.
pub const ABANDONED: Duration = Duration::from_mins(10);

const PART: &str = "part";
const RECORD: &str = "json";

/// An unfinished download held by this process: `<key>.<owner>.part` and its record
/// `<key>.<owner>.json`.
///
/// Dropped, both files stay, so a later run can continue from the record.
#[derive(Debug)]
pub struct Partial {
    file: File,
    part: PathBuf,
    record_path: PathBuf,
    record: Option<PartialRecord>,
    tmp: PathBuf,
}

impl Tier {
    /// The unfinished download named `key`: one abandoned by an earlier writer when there is
    /// one, taken over by renaming it to this writer's name, or a new empty one.
    ///
    /// A partial another writer touched within [`ABANDONED`] of `now` is left to it, and so is
    /// one that cannot be renamed. A partial taken over is touched at `now`, so no other writer
    /// takes it in turn.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when `partial/` cannot be listed or a file cannot be opened.
    pub fn partial(&self, key: &Digest, now: SystemTime) -> Result<Partial, StoreError> {
        let dir = self.layout().dir("partial");
        let prefix = format!("{}.", hex(key));
        let own = format!("{prefix}{}", owner());
        let part = dir.join(format!("{own}.{PART}"));
        let record_path = dir.join(format!("{own}.{RECORD}"));
        for found in entries(&dir)? {
            let Some(stem) = found
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(&format!(".{PART}")))
                .filter(|stem| stem.starts_with(&prefix))
            else {
                continue;
            };
            if !abandoned(&found, now) {
                continue;
            }
            if fs::rename(&found, &part).is_err() {
                continue;
            }
            let old_record = dir.join(format!("{stem}.{RECORD}"));
            let record = match fs::rename(&old_record, &record_path) {
                Ok(()) => read_record(&record_path),
                Err(_) => None,
            };
            let taken = self.open_partial(part, record_path, record, false)?;
            taken
                .file
                .set_modified(now)
                .map_err(io("touch", &taken.part))?;
            return Ok(taken);
        }
        self.open_partial(part, record_path, None, true)
    }

    fn open_partial(
        &self,
        part: PathBuf,
        record_path: PathBuf,
        record: Option<PartialRecord>,
        new: bool,
    ) -> Result<Partial, StoreError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(new)
            .open(&part)
            .map_err(io("open", &part))?;
        Ok(Partial {
            file,
            part,
            record_path,
            record,
            tmp: self.layout().dir("tmp"),
        })
    }

    /// Makes the first `length` bytes of `partial` a read only object named by `blake3`, the
    /// digest the download computed over exactly the bytes it wrote, and removes its record.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] when the file cannot be cut, sealed or placed.
    pub fn ingest_partial(
        &self,
        partial: Partial,
        blake3: [u8; 32],
        length: u64,
    ) -> Result<Ingested, StoreError> {
        let Partial {
            file,
            part,
            record_path,
            ..
        } = partial;
        file.set_len(length).map_err(io("cut", &part))?;
        platform::seal(&file).map_err(io("make read only", &part))?;
        drop(file);
        let digest = Digest::Blake3(blake3);
        let target = self.layout().object(&blake3);
        let new = self.place(Scratch::new(part), &target)?;
        if !new {
            let _ = platform::freshen(&target);
        }
        crate::files::remove(&record_path)?;
        Ok(Ingested {
            digest,
            new,
            bytes: length,
        })
    }
}

impl Partial {
    /// The record an earlier writer saved, if any.
    #[must_use]
    pub const fn record(&self) -> Option<&PartialRecord> {
        self.record.as_ref()
    }

    /// Reserves room for `length` bytes.
    ///
    /// # Errors
    ///
    /// Returns the filesystem's error.
    pub fn allocate(&mut self, length: u64) -> io::Result<()> {
        platform::reserve(&self.file, length)
    }

    /// Writes `bytes` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns the filesystem's error.
    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)
    }

    /// Reads into `buffer` from `offset`, returning how many bytes were read.
    ///
    /// # Errors
    ///
    /// Returns the filesystem's error.
    pub fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buffer)
    }

    /// Drops every byte and the record.
    ///
    /// # Errors
    ///
    /// Returns the filesystem's error.
    pub fn reset(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.record = None;
        match fs::remove_file(&self.record_path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Replaces the record with `record`, written beside the bytes in one rename.
    ///
    /// # Errors
    ///
    /// Returns the filesystem's error.
    pub fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
        let text = serde_json::to_vec(record).map_err(io::Error::other)?;
        let (mut file, path) = create_unique(&self.tmp).map_err(io::Error::other)?;
        let scratch = Scratch::new(path);
        file.write_all(&text)?;
        drop(file);
        fs::rename(scratch.path(), &self.record_path)?;
        scratch.keep();
        self.record = Some(record.clone());
        Ok(())
    }

    /// The file holding the bytes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.part
    }
}

fn hex(key: &Digest) -> String {
    let text = key.to_string();
    text.split_once(':')
        .map_or(text.as_str(), |(_, hex)| hex)
        .to_owned()
}

fn owner() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}",
        process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn abandoned(path: &Path, now: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| now.duration_since(modified).unwrap_or_default() >= ABANDONED)
}

fn read_record(path: &Path) -> Option<PartialRecord> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

#[cfg(test)]
mod tests {
    use fl_model::partial::Validator;

    use super::*;
    use crate::tier::Sharing;

    fn tier() -> (tempfile::TempDir, Tier) {
        let temp = tempfile::tempdir().unwrap();
        let tier = Tier::open(&temp.path().join("tier"), Sharing::Private).unwrap();
        (temp, tier)
    }

    fn record(received: u64) -> PartialRecord {
        PartialRecord {
            location: "https://host.org/a".to_owned(),
            length: Some(10),
            validator: Validator::StrongEtag("\"v\"".to_owned()),
            received,
        }
    }

    const KEY: Digest = Digest::Blake3([7; 32]);

    fn later() -> SystemTime {
        SystemTime::now() + ABANDONED
    }

    #[test]
    fn an_abandoned_partial_is_taken_over_with_its_bytes_and_record() {
        let (_temp, tier) = tier();
        let mut first = tier.partial(&KEY, SystemTime::now()).unwrap();
        assert!(first.record().is_none());
        first.write_at(0, b"hello").unwrap();
        first.save(&record(5)).unwrap();
        let first_path = first.path().to_owned();
        drop(first);
        let mut second = tier.partial(&KEY, later()).unwrap();
        assert_ne!(second.path(), first_path);
        assert!(!first_path.exists());
        assert_eq!(second.record(), Some(&record(5)));
        let mut buffer = [0; 8];
        assert_eq!(second.read_at(1, &mut buffer).unwrap(), 4);
        assert_eq!(&buffer[..4], b"ello");
    }

    #[test]
    fn a_partial_just_taken_over_is_not_taken_again() {
        let (_temp, tier) = tier();
        let mut first = tier.partial(&KEY, SystemTime::now()).unwrap();
        first.save(&record(3)).unwrap();
        drop(first);
        let second = tier.partial(&KEY, later()).unwrap();
        assert_eq!(second.record(), Some(&record(3)));
        let third = tier.partial(&KEY, later()).unwrap();
        assert_ne!(third.path(), second.path());
        assert!(second.path().exists());
        assert!(third.record().is_none());
    }

    #[test]
    fn a_partial_in_use_is_left_to_its_writer() {
        let (_temp, tier) = tier();
        let mut first = tier.partial(&KEY, SystemTime::now()).unwrap();
        first.save(&record(0)).unwrap();
        let second = tier.partial(&KEY, SystemTime::now()).unwrap();
        assert_ne!(second.path(), first.path());
        assert!(first.path().exists());
        assert!(second.record().is_none());
        let other = tier.partial(&Digest::Blake3([8; 32]), later()).unwrap();
        assert!(other.record().is_none());
    }

    #[test]
    fn a_reset_drops_bytes_and_record() {
        let (_temp, tier) = tier();
        let mut partial = tier.partial(&KEY, SystemTime::now()).unwrap();
        partial.write_at(0, b"hello").unwrap();
        partial.save(&record(5)).unwrap();
        partial.reset().unwrap();
        assert!(partial.record().is_none());
        assert_eq!(fs::metadata(partial.path()).unwrap().len(), 0);
        drop(partial);
        assert!(tier.partial(&KEY, later()).unwrap().record().is_none());
    }

    #[test]
    fn an_ingested_partial_is_cut_to_length_and_leaves_nothing_behind() {
        let (_temp, tier) = tier();
        let mut partial = tier.partial(&KEY, SystemTime::now()).unwrap();
        partial.allocate(4096).unwrap();
        partial.write_at(0, b"hello").unwrap();
        partial.save(&record(5)).unwrap();
        let blake3 = *blake3::hash(b"hello").as_bytes();
        let ingested = tier.ingest_partial(partial, blake3, 5).unwrap();
        assert!(ingested.new);
        let located = tier.locate(&ingested.digest).unwrap();
        assert_eq!(fs::read(located.path).unwrap(), b"hello");
        assert_eq!(entries(&tier.layout().dir("partial")).unwrap().len(), 0);
        assert_eq!(entries(&tier.layout().dir("tmp")).unwrap().len(), 0);
    }
}
