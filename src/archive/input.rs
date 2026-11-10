//! the archive file as the zip and 7z readers take it: buffered, seekable, and counting every byte
//! read for the ratio check.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::guard::Meter;
use super::{BUFFER, unreadable};
use crate::error::Error;

/// opens the archive file, and caps `meter` at its size: seeks may read bytes again, but cannot
/// make the archive larger.
pub(super) fn open(archive: &Path, meter: &Meter) -> Result<BufReader<File>, Error> {
    let file = File::open(archive).map_err(|err| unreadable(archive, &err))?;
    let size = file
        .metadata()
        .map_err(|err| unreadable(archive, &err))?
        .len();
    meter.cap_at(size);
    Ok(BufReader::with_capacity(BUFFER, file))
}

/// the archive file, buffered, and counting every byte read for the ratio check. a seek that
/// lands inside the buffer keeps it, since every member means a jump from its header to its data.
pub(super) struct Source<'m> {
    file: BufReader<File>,
    position: u64,
    meter: &'m Meter,
}

impl<'m> Source<'m> {
    /// reads `file` on from where it stands.
    pub(super) fn new(mut file: BufReader<File>, meter: &'m Meter) -> io::Result<Source<'m>> {
        let position = file.stream_position()?;
        Ok(Source {
            file,
            position,
            meter,
        })
    }
}

impl Read for Source<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buffer)?;
        self.position += read as u64;
        self.meter.count(read);
        Ok(read)
    }
}

impl Seek for Source<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => self
                .position
                .checked_add_signed(offset)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(_) => {
                self.position = self.file.seek(to)?;
                return Ok(self.position);
            }
        };
        let offset = i64::try_from(i128::from(target) - i128::from(self.position))
            .map_err(|_| io::ErrorKind::InvalidInput)?;
        self.file.seek_relative(offset)?;
        self.position = target;
        Ok(target)
    }
}
