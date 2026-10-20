//! A sink on disk that outlives its process: the bytes in one file, the record beside them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fl_model::partial::PartialRecord;
use fl_transfer::sink::Sink;

/// `bytes` and `record.json` in one directory.
#[derive(Debug)]
pub struct Disk {
    file: File,
    record_path: PathBuf,
    record: Option<PartialRecord>,
}

impl Disk {
    /// Opens the sink in `dir`, with whatever an earlier process left there.
    pub fn open(dir: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("bytes"))?;
        let record_path = dir.join("record.json");
        Ok(Self {
            file,
            record: read_record(&record_path),
            record_path,
        })
    }
}

/// The record saved in `path`, when there is one that parses.
pub fn read_record(path: &Path) -> Option<PartialRecord> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

impl Sink for Disk {
    fn record(&self) -> Option<PartialRecord> {
        self.record.clone()
    }

    fn allocate(&mut self, length: u64) -> io::Result<()> {
        self.file.set_len(length)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buffer)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.record = None;
        match fs::remove_file(&self.record_path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
        let staging = self.record_path.with_extension("new");
        fs::write(
            &staging,
            serde_json::to_vec(record).map_err(io::Error::other)?,
        )?;
        fs::rename(&staging, &self.record_path)?;
        self.record = Some(record.clone());
        Ok(())
    }
}
