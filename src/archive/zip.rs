//! zip archives, read through their central directory. every entry is checked before the first
//! byte is written.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use ::zip::read::ZipFileEntry;
use ::zip::{SUPPORTED_COMPRESSION_METHODS, ZipArchive};

use super::guard::{Guard, Meter};
use super::names::Named;
use super::{BUFFER, Keep, damaged, refused, refused_member, unreadable};
use crate::error::Error;

/// the file type bits of a unix mode, as zip entries made on unix carry them.
const FILE_TYPE: u32 = 0o170_000;
const REGULAR: u32 = 0o100_000;
const DIRECTORY: u32 = 0o040_000;

pub(super) fn unpack(
    archive: &Path,
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
) -> Result<(), Error> {
    let file = File::open(archive).map_err(|err| unreadable(archive, &err))?;
    let source = Source {
        file: BufReader::with_capacity(BUFFER, file),
        position: 0,
        meter: guard.meter(),
    };
    let mut zip = ZipArchive::new(source).map_err(|err| damaged(None, &err))?;
    if zip.has_duplicate_names() {
        let name = repeated_name(archive, zip.central_directory_start())
            .unwrap_or_else(|| "a name".to_owned());
        return Err(refused(format!("the archive holds \"{name}\" twice")));
    }
    for (index, path) in checked(&zip, guard)? {
        let mut file = zip
            .by_index(index)
            .map_err(|err| damaged(Some(&path), &err))?;
        keep(guard, path, &mut file)?;
    }
    Ok(())
}

/// every file in the central directory, after each entry passed the guard.
fn checked(
    zip: &ZipArchive<Source<'_>>,
    guard: &mut Guard<'_>,
) -> Result<Vec<(usize, String)>, Error> {
    let mut files = Vec::new();
    for index in 0..zip.len() {
        guard.entry()?;
        let entry = zip
            .by_index_data(index)
            .map_err(|err| damaged(None, &err))?;
        let raw = entry.name().map_err(|err| damaged(None, &err))?;
        let is_folder = is_folder(&entry);
        if let Some(reason) = refusal(&entry, is_folder) {
            return Err(refused_member(&raw, &reason));
        }
        if let Named::File(path) = guard.name(&raw, is_folder)? {
            files.push((index, path));
        }
    }
    Ok(files)
}

fn is_folder(entry: &ZipFileEntry<'_>) -> bool {
    entry.is_dir()
        || entry
            .unix_mode()
            .is_some_and(|mode| mode & FILE_TYPE == DIRECTORY)
}

/// why fetchloom refuses an entry for what it is, whatever its name.
fn refusal(entry: &ZipFileEntry<'_>, is_folder: bool) -> Option<String> {
    if entry.encrypted() {
        return Some("it is encrypted".to_owned());
    }
    if is_folder {
        return None;
    }
    let file_type = entry.unix_mode().map_or(0, |mode| mode & FILE_TYPE);
    if file_type != 0 && file_type != REGULAR {
        return Some(if entry.is_symlink() {
            "it is a symlink".to_owned()
        } else {
            format!("it is a special file of type {file_type:o}")
        });
    }
    let method = entry.compression();
    (!SUPPORTED_COMPRESSION_METHODS.contains(&method)).then(|| {
        format!(
            "it is compressed with {}, a method fetchloom cannot read",
            method.to_string().to_lowercase()
        )
    })
}

/// the first name the central directory lists twice. the zip crate keeps one entry per name, so
/// only reading the directory again finds which name it was.
fn repeated_name(archive: &Path, directory: u64) -> Option<String> {
    let mut file = BufReader::new(File::open(archive).ok()?);
    file.seek(SeekFrom::Start(directory)).ok()?;
    let mut seen = HashSet::new();
    let mut header = [0; 46];
    loop {
        file.read_exact(&mut header).ok()?;
        if header[..4] != *b"PK\x01\x02" {
            return None;
        }
        let length = |at: usize| u16::from_le_bytes([header[at], header[at + 1]]);
        let mut name = vec![0; usize::from(length(28))];
        file.read_exact(&mut name).ok()?;
        let rest = i64::from(length(30)) + i64::from(length(32));
        file.seek_relative(rest).ok()?;
        if !seen.insert(name.clone()) {
            return Some(String::from_utf8_lossy(&name).into_owned());
        }
    }
}

/// the archive file, buffered, and counting every byte read for the ratio check. a seek that
/// lands inside the buffer keeps it, since every member means a jump from its header to its data.
struct Source<'m> {
    file: BufReader<File>,
    position: u64,
    meter: &'m Meter,
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
    use std::io::{Cursor, Write};

    use ::zip::write::SimpleFileOptions;
    use ::zip::{CompressionMethod, System, ZipWriter};

    use super::super::samples::{Scratch, zip_of, zip_with};
    use super::super::{Format, tree_hash};
    use crate::digest;
    use crate::error::Kind;

    const FILES: &[(&str, &[u8])] = &[
        ("data/", b""),
        ("data/train.csv", b"x,y\n1,2\n"),
        ("data/empty.txt", b""),
        ("README.md", b"# readme\n"),
    ];

    fn signatures(bytes: &[u8], signature: [u8; 4]) -> Vec<usize> {
        let found = bytes.windows(4).enumerate();
        found
            .filter(|(_, window)| *window == signature)
            .map(|(at, _)| at)
            .collect()
    }

    /// edits a 16-bit field in every local header and every central directory header.
    fn patch(zip: &mut [u8], local: usize, central: usize, edit: impl Fn(u16) -> u16) {
        for (signature, offset) in [(*b"PK\x03\x04", local), (*b"PK\x01\x02", central)] {
            for at in signatures(zip, signature) {
                let field = at + offset;
                let value = edit(u16::from_le_bytes([zip[field], zip[field + 1]]));
                zip[field..field + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
    }

    #[test]
    fn a_zip_unpacks_into_read_only_objects() {
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(Format::Zip, &zip_of(FILES)).unwrap();
        let paths: Vec<_> = unpacked.members.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "data/empty.txt", "data/train.csv"]);
        for member in &unpacked.members {
            let (_, bytes) = FILES.iter().find(|(name, _)| *name == member.path).unwrap();
            assert_eq!(member.size, bytes.len() as u64);
            assert_eq!(member.sha256, digest::sha256(bytes));
            scratch.assert_stored(member, bytes);
        }
        assert_eq!(unpacked.tree, tree_hash(&unpacked.members));
    }

    #[test]
    fn every_method_fetchloom_reads_unpacks() {
        let scratch = Scratch::new();
        for method in [
            CompressionMethod::Stored,
            CompressionMethod::Bzip2,
            CompressionMethod::Zstd,
        ] {
            let options = SimpleFileOptions::default().compression_method(method);
            let zip = zip_with(ZipWriter::new(Cursor::new(Vec::new())), FILES, options);
            let unpacked = scratch.unpack(Format::Zip, &zip.into_inner()).unwrap();
            assert_eq!(unpacked.members.len(), 3, "{method}");
        }
    }

    #[test]
    fn zip64_records_are_read() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.set_raw_zip64_extensible_data_sector(Box::new([]));
        let options = SimpleFileOptions::default().large_file(true);
        let bytes = zip_with(zip, FILES, options).into_inner();
        assert!(
            !signatures(&bytes, *b"PK\x06\x06").is_empty(),
            "a zip64 end record"
        );
        let unpacked = Scratch::new().unpack(Format::Zip, &bytes).unwrap();
        assert_eq!(unpacked.members.len(), 3);
    }

    #[test]
    fn data_descriptors_are_read() {
        let mut zip = ZipWriter::new_stream(Vec::new());
        for (name, bytes) in &FILES[1..] {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        assert!(
            !signatures(&bytes, *b"PK\x07\x08").is_empty(),
            "a data descriptor"
        );
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(Format::Zip, &bytes).unwrap();
        let train = &unpacked.members[2];
        scratch.assert_stored(train, b"x,y\n1,2\n");
    }

    #[test]
    fn an_empty_zip_unpacks_to_nothing() {
        let unpacked = Scratch::new().unpack(Format::Zip, &zip_of(&[])).unwrap();
        assert_eq!(unpacked.members, []);
        assert_eq!(unpacked.tree, digest::sha256(b""));
    }

    #[test]
    fn an_encrypted_entry_is_refused() {
        let mut zip = zip_of(FILES);
        patch(&mut zip, 6, 8, |flags| flags | 1);
        let err = Scratch::new().refuse(Format::Zip, &zip);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(
            err.message.ends_with("because it is encrypted"),
            "{}",
            err.message
        );
        assert!(err.help.is_some());
    }

    #[test]
    fn a_symlink_is_refused() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.add_symlink("data/link", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let err = Scratch::new().refuse(Format::Zip, &bytes);
        assert_eq!(
            err.message,
            "the archive holds \"data/link\", which fetchloom refuses because it is a symlink"
        );
    }

    #[test]
    fn a_fifo_is_refused() {
        let options = SimpleFileOptions::default()
            .system(System::Unix)
            .external_attributes(0o010_644 << 16);
        let zip = zip_with(
            ZipWriter::new(Cursor::new(Vec::new())),
            &[("pipe", b"")],
            options,
        );
        let err = Scratch::new().refuse(Format::Zip, &zip.into_inner());
        assert!(err.message.contains("special file"), "{}", err.message);
    }

    #[test]
    fn a_name_listed_twice_is_refused_and_named() {
        let mut zip = zip_of(&[("dup1.txt", b"a"), ("dup2.txt", b"b")]);
        for at in signatures(&zip, *b"dup2") {
            zip[at + 3] = b'1';
        }
        let err = Scratch::new().refuse(Format::Zip, &zip);
        assert_eq!(err.message, "the archive holds \"dup1.txt\" twice");
    }

    #[test]
    fn one_unsafe_name_refuses_the_whole_zip() {
        let zip = zip_of(&[("a.txt", b"a"), ("../evil.txt", b"b")]);
        let err = Scratch::new().refuse(Format::Zip, &zip);
        assert!(err.message.contains("\"../evil.txt\""), "{}", err.message);
    }

    #[test]
    fn an_unknown_method_is_refused() {
        let mut zip = zip_of(FILES);
        patch(&mut zip, 8, 10, |_| 14);
        let err = Scratch::new().refuse(Format::Zip, &zip);
        assert!(
            err.message.contains("compressed with lzma"),
            "{}",
            err.message
        );
    }

    #[test]
    fn damaged_bytes_are_caught_by_the_checksum() {
        let mut zip = zip_of(&[("a.txt", &b"hello world, hello zip"[..])]);
        let data = signatures(&zip, *b"PK\x03\x04")[0] + 30 + "a.txt".len();
        zip[data + 2] ^= 0x55;
        let err = Scratch::new().refuse(Format::Zip, &zip);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(
            err.message.starts_with("the archive is damaged"),
            "{}",
            err.message
        );
    }
}
