//! bounds zip directory work before the decoder collects its metadata.

use std::io::{self, Read, Seek, SeekFrom};

use crate::archive::guard::Guard;
use crate::archive::names::LONGEST_PATH;
use crate::archive::tar::unreadable_stream;
use crate::archive::{cut_short, refused};
use crate::error::Error;

const METADATA: u64 = 256 << 20;
const TAIL: usize = 65_558;

pub(super) fn check(source: &mut (impl Read + Seek), guard: &Guard<'_>) -> Result<(), Error> {
    check_with(source, guard, METADATA)
}

fn check_with(
    source: &mut (impl Read + Seek),
    guard: &Guard<'_>,
    budget: u64,
) -> Result<(), Error> {
    guard.check_stopped()?;
    let memory = guard.limits().memory;
    let size = source
        .seek(SeekFrom::End(0))
        .map_err(|error| read_error(&error, memory))?;
    let length = usize::try_from(size).unwrap_or(TAIL).min(TAIL);
    let start = size - length as u64;
    source
        .seek(SeekFrom::Start(start))
        .map_err(|error| read_error(&error, memory))?;
    let mut tail = vec![0; length];
    source
        .read_exact(&mut tail)
        .map_err(|error| read_error(&error, memory))?;
    let mut probe = Probe {
        source,
        position: size,
        remaining: budget,
        budget,
        memory,
    };
    let ends = 0..length.saturating_sub(21);
    // every whole zip ends with its end record within the tail, as its comment is short
    if !ends.clone().any(|at| tail[at..at + 4] == *b"PK\x05\x06") {
        return Err(cut_short());
    }
    for at in ends.rev() {
        if tail[at..at + 4] != *b"PK\x05\x06" {
            continue;
        }
        guard.check_stopped()?;
        let end = &tail[at..at + 22];
        let offset = start + at as u64;
        if u64::from(word(end, 20)) > size - offset - 22 {
            continue;
        }
        let may_be_zip64 = word(end, 8) == u16::MAX
            || word(end, 10) == u16::MAX
            || dword(end, 12) == u32::MAX
            || dword(end, 16) == u32::MAX;
        if may_be_zip64
            && offset >= 20
            && let Some(locator) = probe.header::<20>(offset - 20)?
            && locator[..4] == *b"PK\x06\x07"
        {
            if probe.zip64(&locator, offset - 20, guard)? {
                return Ok(());
            }
            continue;
        }
        if word(end, 4) != 0 || word(end, 6) != 0 || word(end, 8) != word(end, 10) {
            continue;
        }
        let count = u64::from(word(end, 10));
        let declared = u64::from(dword(end, 16));
        if count == 0 {
            if declared <= offset {
                return Ok(());
            }
            continue;
        }
        if declared >= offset {
            continue;
        }
        guard.check_entries(count)?;
        let bytes = u64::from(dword(end, 12));
        probe.ensure(bytes)?;
        let abutting = offset.checked_sub(bytes).filter(|at| *at >= declared);
        let directory = if let Some(at) = abutting
            && probe.magic(at, *b"PK\x01\x02")?
        {
            Some(at)
        } else {
            probe.find(declared, offset, *b"PK\x01\x02", guard)?
        };
        if let Some(directory) = directory {
            probe.directory(directory, count, guard)?;
            if directory.checked_add(bytes) == Some(offset) {
                return Ok(());
            }
        }
    }
    Ok(())
}

struct Probe<'a, R> {
    source: &'a mut R,
    position: u64,
    remaining: u64,
    budget: u64,
    /// what decoders may take, which a read error's message names.
    memory: u64,
}

impl<R: Read + Seek> Probe<'_, R> {
    fn ensure(&self, bytes: u64) -> Result<(), Error> {
        if bytes > self.remaining {
            return Err(refused(format!(
                "the archive's zip metadata takes more than {}, the most fetchloom reads before unpacking",
                crate::units::size(self.budget)
            )));
        }
        Ok(())
    }

    fn spend(&mut self, bytes: u64) -> Result<(), Error> {
        self.ensure(bytes)?;
        self.remaining -= bytes;
        Ok(())
    }

    fn read_at(&mut self, at: u64, buffer: &mut [u8]) -> Result<bool, Error> {
        self.spend(buffer.len() as u64)?;
        let distance = i128::from(at) - i128::from(self.position);
        if let Ok(distance) = i64::try_from(distance) {
            self.source
                .seek_relative(distance)
                .map_err(|error| read_error(&error, self.memory))?;
        } else {
            self.source
                .seek(SeekFrom::Start(at))
                .map_err(|error| read_error(&error, self.memory))?;
        }
        self.position = at;
        let mut filled = 0;
        while filled < buffer.len() {
            match self.source.read(&mut buffer[filled..]) {
                Ok(0) => return Ok(false),
                Ok(read) => {
                    filled += read;
                    self.position += read as u64;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(read_error(&error, self.memory)),
            }
        }
        Ok(true)
    }

    fn header<const N: usize>(&mut self, at: u64) -> Result<Option<[u8; N]>, Error> {
        let mut header = [0; N];
        Ok(self.read_at(at, &mut header)?.then_some(header))
    }

    fn magic(&mut self, at: u64, magic: [u8; 4]) -> Result<bool, Error> {
        Ok(self.header::<4>(at)?.is_some_and(|header| header == magic))
    }

    fn find(
        &mut self,
        mut at: u64,
        end: u64,
        magic: [u8; 4],
        guard: &Guard<'_>,
    ) -> Result<Option<u64>, Error> {
        self.ensure(end.saturating_sub(at))?;
        let mut buffer = vec![0; 64 << 10];
        while end.saturating_sub(at) >= 4 {
            guard.check_stopped()?;
            let length =
                usize::try_from((end - at).min(buffer.len() as u64)).unwrap_or(buffer.len());
            if !self.read_at(at, &mut buffer[..length])? {
                return Ok(None);
            }
            if let Some(found) = buffer[..length].windows(4).position(|bytes| bytes == magic) {
                return Ok(Some(at + found as u64));
            }
            at += (length - 3) as u64;
        }
        Ok(None)
    }

    fn directory(&mut self, mut at: u64, count: u64, guard: &Guard<'_>) -> Result<(), Error> {
        guard.check_entries(count)?;
        for _ in 0..count {
            guard.check_stopped()?;
            let Some(header) = self.header::<46>(at)? else {
                break;
            };
            if header[..4] != *b"PK\x01\x02" {
                break;
            }
            let name = usize::from(word(&header, 28));
            if name > LONGEST_PATH {
                return Err(refused(
                    "the archive holds a zip name longer than 4,096 bytes",
                ));
            }
            let rest = name as u64 + u64::from(word(&header, 30)) + u64::from(word(&header, 32));
            self.spend(rest)?;
            let Some(next) = at.checked_add(46).and_then(|at| at.checked_add(rest)) else {
                break;
            };
            at = next;
        }
        Ok(())
    }

    fn zip64(&mut self, locator: &[u8; 20], end: u64, guard: &Guard<'_>) -> Result<bool, Error> {
        let declared = qword(locator, 8);
        if declared >= end || dword(locator, 16) > 1 {
            return Ok(false);
        }
        let mut at = declared;
        while let Some(found) = self.find(at, end, *b"PK\x06\x06", guard)? {
            let Some(header) = self.header::<56>(found)? else {
                break;
            };
            let record = qword(&header, 4);
            if record >= 44
                && found.checked_add(12).and_then(|at| at.checked_add(record)) == Some(end)
            {
                self.spend(record - 44)?;
                let count = qword(&header, 32);
                if dword(&header, 16) == 0
                    && dword(&header, 20) == dword(locator, 4)
                    && qword(&header, 24) == count
                {
                    guard.check_entries(count)?;
                    if found < count.saturating_mul(46).saturating_add(qword(&header, 48)) {
                        at = found + 1;
                        continue;
                    }
                    let bytes = qword(&header, 40);
                    self.ensure(bytes)?;
                    if let Some(directory) = qword(&header, 48)
                        .checked_add(found - declared)
                        .filter(|at| *at <= found)
                    {
                        self.directory(directory, count, guard)?;
                        return Ok(directory.checked_add(bytes) == Some(found));
                    }
                    break;
                }
            }
            at = found + 1;
        }
        Ok(false)
    }
}

fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn dword(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn qword(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes([
        bytes[at],
        bytes[at + 1],
        bytes[at + 2],
        bytes[at + 3],
        bytes[at + 4],
        bytes[at + 5],
        bytes[at + 6],
        bytes[at + 7],
    ])
}

fn read_error(error: &io::Error, memory: u64) -> Error {
    unreadable_stream(None, error, memory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Limits;
    use crate::archive::guard::Meter;
    use crate::archive::samples::zip_of;
    use std::io::Cursor;

    #[test]
    fn directory_budget_is_checked_before_collection() {
        let bytes = zip_of(&[("member", b"1")]);
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let guard = Guard::new(&limits, &never, &meter);
        assert!(check_with(&mut Cursor::new(&bytes), &guard, 128).is_ok());
        let error = check_with(&mut Cursor::new(&bytes), &guard, 32).unwrap_err();
        assert!(error.message.contains("zip metadata"), "{}", error.message);
    }

    #[test]
    fn a_stop_precedes_directory_reads() {
        let limits = Limits::default();
        let meter = Meter::default();
        let stop = |_: u64| true;
        let guard = Guard::new(&limits, &stop, &meter);
        let error = check(&mut Cursor::new(b"broken"), &guard).unwrap_err();
        assert_eq!(error.kind, crate::error::Kind::Interrupted);
    }

    #[test]
    fn zip64_extensible_metadata_shares_the_budget() {
        let mut writer = ::zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.set_raw_zip64_extensible_data_sector(vec![0; 4096].into_boxed_slice());
        let bytes = crate::archive::samples::zip_with(
            writer,
            &[("member", b"1")],
            ::zip::write::SimpleFileOptions::default().large_file(true),
        )
        .into_inner();
        let limits = Limits::default();
        let meter = Meter::default();
        let never = |_: u64| false;
        let guard = Guard::new(&limits, &never, &meter);
        assert!(check_with(&mut Cursor::new(&bytes), &guard, 16 << 10).is_ok());
        let error = check_with(&mut Cursor::new(&bytes), &guard, 512).unwrap_err();
        assert!(error.message.contains("zip metadata"), "{}", error.message);
    }
}
