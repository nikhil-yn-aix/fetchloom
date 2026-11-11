//! an archive's bytes as its readers take them: one file, or the parts of a split archive read in
//! order as one, with one file open at a time. zip and 7z read them buffered and seekable through
//! [`Source`], which counts every byte read for the ratio check.

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::guard::Meter;
use super::{BUFFER, unreadable};
use crate::error::Error;

/// the bytes of an archive: one file, or the parts of a split archive, each with its size, in
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    File(PathBuf),
    Joined(Vec<(PathBuf, u64)>),
}

impl Input {
    /// the file an error reading the archive names: the archive, or its first part.
    pub(super) fn first(&self) -> &Path {
        match self {
            Input::File(path) => path,
            Input::Joined(parts) => parts.first().map_or(Path::new(""), |(path, _)| path),
        }
    }

    /// opens the archive, and caps `meter` at its size: seeks may read bytes again, but cannot
    /// make the archive larger.
    pub(super) fn open(&self, meter: &Meter) -> Result<BufReader<Joined>, Error> {
        let joined = Joined::open(self).map_err(|(path, err)| unreadable(path, &err))?;
        meter.cap_at(joined.size());
        Ok(BufReader::with_capacity(BUFFER, joined))
    }
}

/// an archive's bytes as one stream. only the part holding the next byte to read is open, so a
/// set of a hundred parts costs one file handle.
pub(crate) struct Joined {
    parts: Vec<PathBuf>,
    /// where each part starts in the archive, and last, where the archive ends.
    starts: Vec<u64>,
    /// the part open, by its index, its file, and where in it the file reads next.
    open: Option<(usize, File, u64)>,
    position: u64,
}

impl Joined {
    /// opens `input`, checking that each part holds the bytes its size says: one shorter or
    /// longer would move every byte after it. on failure, says which file failed.
    pub(crate) fn open(input: &Input) -> Result<Joined, (&Path, io::Error)> {
        match input {
            Input::File(path) => {
                let file = File::open(path).map_err(|err| (path.as_path(), err))?;
                let metadata = file.metadata().map_err(|err| (path.as_path(), err))?;
                plain(path, &metadata)?;
                let size = metadata.len();
                Ok(Joined {
                    parts: vec![path.clone()],
                    starts: vec![0, size],
                    open: Some((0, file, 0)),
                    position: 0,
                })
            }
            Input::Joined(parts) => {
                let mut starts = Vec::with_capacity(parts.len() + 1);
                starts.push(0);
                for (path, size) in parts {
                    let metadata = fs::metadata(path).map_err(|err| (path.as_path(), err))?;
                    plain(path, &metadata)?;
                    let found = metadata.len();
                    let end = starts
                        .last()
                        .and_then(|start: &u64| start.checked_add(*size))
                        .filter(|_| found == *size)
                        .ok_or_else(|| {
                            let changed = io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("it holds {found} bytes, not the {size} it was pinned at"),
                            );
                            (path.as_path(), changed)
                        })?;
                    starts.push(end);
                }
                Ok(Joined {
                    parts: parts.iter().map(|(path, _)| path.clone()).collect(),
                    starts,
                    open: None,
                    position: 0,
                })
            }
        }
    }

    /// the archive's size: every part's together.
    pub(crate) fn size(&self) -> u64 {
        self.starts.last().copied().unwrap_or_default()
    }
}

impl Read for Joined {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.position >= self.size() {
            return Ok(0);
        }
        // the last part starting at or before the position, which skips parts of no bytes
        let index = self
            .starts
            .partition_point(|start| *start <= self.position)
            .saturating_sub(1);
        let offset = self.position - self.starts[index];
        let left = self.starts[index + 1] - self.position;
        let (file, at) = match &mut self.open {
            Some((open, file, at)) if *open == index => (file, at),
            open => {
                *open = None;
                let file = File::open(&self.parts[index])?;
                let (_, file, at) = open.insert((index, file, 0));
                (file, at)
            }
        };
        if *at != offset {
            file.seek(SeekFrom::Start(offset))?;
            *at = offset;
        }
        let wanted = usize::try_from(left).map_or(buffer.len(), |left| left.min(buffer.len()));
        let read = file.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a part of the archive ended before its size",
            ));
        }
        *at += read as u64;
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for Joined {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(offset) => self.size().checked_add_signed(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
        };
        // the part to read from is found, and opened, only once something is read
        self.position = target.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

/// the archive, buffered, and counting every byte read for the ratio check. a seek that lands
/// inside the buffer keeps it, since every member means a jump from its header to its data.
pub(super) struct Source<'m> {
    file: BufReader<Joined>,
    position: u64,
    meter: &'m Meter,
}

impl<'m> Source<'m> {
    /// reads `file` on from where it stands.
    pub(super) fn new(mut file: BufReader<Joined>, meter: &'m Meter) -> io::Result<Source<'m>> {
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

/// refuses `path` unless it is a plain file: linux opens a folder too, and the size its disk gives
/// one would be read as an archive's.
fn plain<'a>(path: &'a Path, metadata: &fs::Metadata) -> Result<(), (&'a Path, io::Error)> {
    if metadata.is_file() {
        return Ok(());
    }
    Err((
        path,
        io::Error::new(io::ErrorKind::InvalidInput, "it is not a file"),
    ))
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom};

    use super::{Input, Joined};

    /// `bytes` cut where `cuts` say, each part written to a file of its own.
    fn parts(folder: &std::path::Path, bytes: &[u8], cuts: &[usize]) -> Input {
        let mut ends: Vec<usize> = cuts.iter().map(|cut| cut % (bytes.len() + 1)).collect();
        ends.push(bytes.len());
        ends.sort_unstable();
        let mut start = 0;
        let mut parts = Vec::new();
        for (index, end) in ends.into_iter().enumerate() {
            let path = folder.join(format!("archive.zip.{:03}", index + 1));
            std::fs::write(&path, &bytes[start..end]).unwrap();
            parts.push((path, (end - start) as u64));
            start = end;
        }
        Input::Joined(parts)
    }

    /// a folder is no archive file, though linux opens one: it is refused at once as a local
    /// problem, whatever size its disk gives a folder, never read as an archive cut short.
    #[test]
    fn a_folder_is_refused_as_no_file() {
        let folder = tempfile::tempdir().unwrap();
        let part = folder.path().join("part");
        std::fs::create_dir(&part).unwrap();
        for input in [
            Input::File(folder.path().to_path_buf()),
            Input::Joined(vec![(part, 0)]),
        ] {
            let Err((_, refused)) = Joined::open(&input) else {
                panic!("a folder opened as an archive");
            };
            // windows will not open a folder at all; elsewhere it opens and the check refuses it
            assert!(
                matches!(
                    refused.kind(),
                    ErrorKind::InvalidInput | ErrorKind::PermissionDenied
                ),
                "{refused}"
            );
        }
    }

    /// a read of up to `amount` bytes, whatever reads that takes, or a seek from the start, the
    /// end or where the stream stands.
    fn step(
        stream: &mut (impl Read + Seek),
        (kind, amount): (u8, i16),
    ) -> Result<Vec<u8>, ErrorKind> {
        let amount = i64::from(amount);
        let to = match kind % 4 {
            0 => {
                let mut taken = Vec::new();
                let read = stream
                    .by_ref()
                    .take(amount.unsigned_abs())
                    .read_to_end(&mut taken);
                return read.map(|_| taken).map_err(|err| err.kind());
            }
            1 => SeekFrom::Start(amount.unsigned_abs()),
            2 => SeekFrom::End(amount),
            _ => SeekFrom::Current(amount),
        };
        let position = stream.seek(to).map_err(|err| err.kind())?;
        Ok(position.to_le_bytes().to_vec())
    }

    /// reads and seeks over parts cut anywhere, parts of no bytes too, give what they give over
    /// the archive whole.
    #[test]
    fn joined_parts_read_and_seek_as_the_whole() {
        bolero::check!()
            .with_type::<(Vec<u8>, Vec<u16>, Vec<(u8, i16)>)>()
            .for_each(|(bytes, cuts, steps)| {
                let folder = tempfile::tempdir().unwrap();
                let cuts: Vec<usize> = cuts.iter().map(|cut| usize::from(*cut)).collect();
                let mut joined = Joined::open(&parts(folder.path(), bytes, &cuts)).unwrap();
                let mut whole = Cursor::new(bytes.clone());
                for each in steps {
                    assert_eq!(
                        step(&mut joined, *each),
                        step(&mut whole, *each),
                        "{each:?}"
                    );
                }
                let (mut ours, mut theirs) = (Vec::new(), Vec::new());
                joined.read_to_end(&mut ours).unwrap();
                whole.read_to_end(&mut theirs).unwrap();
                assert_eq!(ours, theirs);
            });
    }

    #[test]
    fn a_part_of_another_size_than_its_pin_is_refused() {
        let folder = tempfile::tempdir().unwrap();
        let Input::Joined(mut pinned) = parts(folder.path(), b"0123456789", &[4]) else {
            unreachable!()
        };
        pinned[0].1 += 1;
        let off = Input::Joined(pinned.clone());
        let Err((path, err)) = Joined::open(&off) else {
            panic!("a part one byte off its pin opened");
        };
        assert_eq!(path, pinned[0].0);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// what joining costs: one file handle, whatever the number of parts.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_the_part_being_read_is_open() {
        let folder = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..=255).collect();
        let cuts: Vec<usize> = (1..100).map(|cut| cut * 2).collect();
        let input = parts(folder.path(), &bytes, &cuts);
        let open = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    std::fs::read_link(entry.path())
                        .is_ok_and(|target| target.starts_with(folder.path()))
                })
                .count()
        };
        let mut joined = Joined::open(&input).unwrap();
        let mut whole = Vec::new();
        let mut buffer = [0; 3];
        loop {
            let read = joined.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            assert!(open() <= 1, "{} parts open", open());
            whole.extend_from_slice(&buffer[..read]);
        }
        assert_eq!(whole, bytes);
    }
}
