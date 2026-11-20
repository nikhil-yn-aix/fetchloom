//! an archive's bytes as its readers take them: one file, or a split archive's parts in order,
//! one file open at a time. zip and 7z read through [`Source`], which counts every byte read.
//! every open is of the file judged at the start, never through a link, so someone sharing the
//! cache who puts a link or another file at an archive's name meanwhile is never read.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::guard::Meter;
use super::{BUFFER, unreadable};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::Error;
use crate::volume::read_unfollowed;

/// an archive's bytes: one file, or a split archive's parts in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    File(Part),
    Joined(Vec<Part>),
}

/// a file of an archive: where it is, the size it must have, which file it was when judged, and
/// whether a key of this machine came with it, as the cache marks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    path: PathBuf,
    size: u64,
    id: Id,
    keyed: bool,
}

/// which file a handle has open: its disk and its index there, which a hardlink shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Id(u64, u64);

impl Id {
    fn of(file: &File) -> io::Result<Id> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let found = file.metadata()?;
            Ok(Id(found.dev(), found.ino()))
        }
        #[cfg(windows)]
        {
            let found = winapi_util::file::information(file)?;
            Ok(Id(found.volume_serial_number(), found.file_index()))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            Ok(Id(0, 0))
        }
    }
}

impl Part {
    /// the plain file `file` has open at `path`, which must hold `size` bytes.
    fn opened(path: PathBuf, size: u64, file: &File) -> io::Result<Part> {
        Ok(Part {
            path,
            size,
            id: Id::of(file)?,
            keyed: false,
        })
    }

    /// the cache's object holding `digest`, opened as [`Cache::open_object`] opens and judges it,
    /// of `size` when given.
    fn cached(cache: &Cache, digest: &Sha256, size: Option<u64>) -> io::Result<Part> {
        let file = cache.open_object(digest, size)?;
        let size = file.metadata()?.len();
        Ok(Part {
            keyed: cache.is_keyed(digest),
            ..Part::opened(cache.object_path(digest), size, &file)?
        })
    }

    /// opens the part to read, never through a link, refusing a file other than the one judged.
    fn open(&self) -> io::Result<File> {
        let file = read_unfollowed(&self.path)?;
        if Id::of(&file)? != self.id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "it is not the file fetchloom checked, as when someone sharing the cache put another one at its name",
            ));
        }
        Ok(file)
    }
}

impl Input {
    /// the one file of an archive that is not split, opened as it was judged; none for parts.
    pub(super) fn whole_file(&self) -> Option<io::Result<File>> {
        match self {
            Input::File(part) => Some(part.open()),
            Input::Joined(_) => None,
        }
    }

    /// the plain file at `path` as it is now, never followed as a link.
    #[cfg(test)]
    pub(crate) fn file(path: &Path) -> io::Result<Input> {
        let file = read_unfollowed(path)?;
        let size = file.metadata()?.len();
        Part::opened(path.to_path_buf(), size, &file).map(Input::File)
    }

    /// the parts at `parts`, in order, each the size given, as they are now.
    #[cfg(test)]
    pub(crate) fn joined(parts: Vec<(PathBuf, u64)>) -> io::Result<Input> {
        parts
            .into_iter()
            .map(|(path, size)| {
                let file = read_unfollowed(&path)?;
                Part::opened(path, size, &file)
            })
            .collect::<io::Result<Vec<Part>>>()
            .map(Input::Joined)
    }

    /// the cache's object holding `digest`, as [`Cache::open_object`] opens and judges it.
    pub(crate) fn cached(cache: &Cache, digest: &Sha256, size: Option<u64>) -> io::Result<Input> {
        Part::cached(cache, digest, size).map(Input::File)
    }

    /// a split archive's `parts`, each the SHA-256 and size of an object of the cache, in order.
    pub(crate) fn cached_parts(cache: &Cache, parts: &[(Sha256, u64)]) -> io::Result<Input> {
        parts
            .iter()
            .map(|(digest, size)| Part::cached(cache, digest, Some(*size)))
            .collect::<io::Result<Vec<Part>>>()
            .map(Input::Joined)
    }

    /// whether a key of this machine came with any part of the archive, so what it unpacks to
    /// did too.
    pub(super) fn keyed(&self) -> bool {
        match self {
            Input::File(part) => part.keyed,
            Input::Joined(parts) => parts.iter().any(|part| part.keyed),
        }
    }

    /// the file an error reading the archive names: the archive, or its first part.
    pub(super) fn first(&self) -> &Path {
        match self {
            Input::File(part) => &part.path,
            Input::Joined(parts) => parts.first().map_or(Path::new(""), |part| &part.path),
        }
    }

    /// opens the archive, capping `meter` at its size, since seeks may reread but not enlarge it.
    pub(super) fn open(&self, meter: &Meter) -> Result<BufReader<Joined>, Error> {
        let joined = Joined::open(self).map_err(|(path, err)| unreadable(path, &err))?;
        meter.cap_at(joined.size());
        Ok(BufReader::with_capacity(BUFFER, joined))
    }
}

/// an archive's bytes as one stream, with only the part holding the next byte open.
pub(crate) struct Joined {
    parts: Vec<Part>,
    /// where each part starts in the archive, and last, where the archive ends.
    starts: Vec<u64>,
    /// the part open, by its index, its file, and where in it the file reads next.
    open: Option<(usize, File, u64)>,
    position: u64,
}

impl Joined {
    /// opens `input`, checking that each part holds the bytes its size says: one shorter or
    /// longer would move every byte after it.
    pub(crate) fn open(input: &Input) -> Result<Joined, (&Path, io::Error)> {
        let parts = match input {
            Input::File(part) => std::slice::from_ref(part),
            Input::Joined(parts) => parts.as_slice(),
        };
        let mut starts = Vec::with_capacity(parts.len() + 1);
        starts.push(0);
        let mut open = None;
        for (index, part) in parts.iter().enumerate() {
            let at = |err| (part.path.as_path(), err);
            let file = part.open().map_err(at)?;
            let found = file.metadata().map_err(at)?.len();
            let end = starts
                .last()
                .and_then(|start: &u64| start.checked_add(part.size))
                .filter(|_| found == part.size)
                .ok_or_else(|| {
                    at(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "it holds {found} bytes, not the {} it was pinned at",
                            part.size
                        ),
                    ))
                })?;
            starts.push(end);
            // a lone file stays open; of a split archive's parts only the one read is
            if parts.len() == 1 && index == 0 {
                open = Some((0, file, 0));
            }
        }
        Ok(Joined {
            parts: parts.to_vec(),
            starts,
            open,
            position: 0,
        })
    }

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
                let file = self.parts[index].open()?;
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
        Input::joined(parts).unwrap()
    }

    /// a folder, which linux opens, is refused at once as a local problem, not read as an archive.
    #[test]
    fn a_folder_is_refused_as_no_file() {
        let folder = tempfile::tempdir().unwrap();
        let part = folder.path().join("part");
        std::fs::create_dir(&part).unwrap();
        for input in [Input::file(folder.path()), Input::joined(vec![(part, 0)])] {
            let Err(refused) = input else {
                panic!("a folder taken as an archive");
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

    /// parts cut anywhere, empty ones too, read and seek as the archive whole.
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
        pinned[0].size += 1;
        let off = Input::Joined(pinned.clone());
        let Err((path, err)) = Joined::open(&off) else {
            panic!("a part one byte off its pin opened");
        };
        assert_eq!(path, pinned[0].path);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

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
