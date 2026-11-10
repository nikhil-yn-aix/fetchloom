//! archives built at test time, and a scratch folder with a cache to unpack them into.

use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;

use ::tar::{Builder, EntryType, Header};
use ::zip::ZipWriter;
use ::zip::write::SimpleFileOptions;
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};

use super::{Format, Limits, Member, Unpacked, unpack};
use crate::cache::Cache;
use crate::error::Error;

pub(super) struct Scratch {
    dir: tempfile::TempDir,
    pub(super) cache: Cache,
}

impl Scratch {
    pub(super) fn new() -> Scratch {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        Scratch { dir, cache }
    }

    pub(super) fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    pub(super) fn unpack(&self, format: Format, bytes: &[u8]) -> Result<Unpacked, Error> {
        self.unpack_with(format, bytes, &Limits::default(), &|_| false)
    }

    pub(super) fn unpack_with(
        &self,
        format: Format,
        bytes: &[u8],
        limits: &Limits,
        watch: &dyn Fn(u64) -> bool,
    ) -> Result<Unpacked, Error> {
        let path = self.file("archive", bytes);
        unpack(&path, format, limits, &self.cache, watch, None)
    }

    /// unpacks an archive the guard must refuse, and checks it left nothing behind.
    pub(super) fn refuse(&self, format: Format, bytes: &[u8]) -> Error {
        let err = self.unpack(format, bytes).unwrap_err();
        self.assert_empty();
        err
    }

    /// the cache holds no object and nothing waits in its `tmp/`.
    pub(super) fn assert_empty(&self) {
        let objects = self.cache.objects().len();
        let temporary = fs::read_dir(self.cache.root().join("tmp")).unwrap().count();
        assert_eq!((objects, temporary), (0, 0), "objects and temporary files");
    }

    /// the member's object holds `bytes` and is read-only.
    pub(super) fn assert_stored(&self, member: &Member, bytes: &[u8]) {
        let path = self
            .cache
            .find(&member.sha256, Some(member.size))
            .unwrap_or_else(|| panic!("{} was not published", member.path));
        assert_eq!(fs::read(&path).unwrap(), bytes, "{}", member.path);
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
    }
}

/// a deflated zip of `files`, as most tools write them.
pub(super) fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    zip_with(
        ZipWriter::new(Cursor::new(Vec::new())),
        files,
        SimpleFileOptions::default(),
    )
    .into_inner()
}

pub(super) fn zip_with<W: Write + std::io::Seek>(
    mut zip: ZipWriter<W>,
    files: &[(&str, &[u8])],
    options: SimpleFileOptions,
) -> W {
    for (name, bytes) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap()
}

/// a GNU tar of `files`, with long names written the GNU way.
pub(super) fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Builder::new(Vec::new());
    for (name, bytes) in files {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        tar.append_data(&mut header, name, *bytes).unwrap();
    }
    tar.into_inner().unwrap()
}

/// appends an entry whose name and type go into the header as they are, unchecked, the way a
/// hostile tool would write them.
pub(super) fn raw_entry(tar: &mut Builder<Vec<u8>>, name: &[u8], kind: EntryType, bytes: &[u8]) {
    let mut header = Header::new_ustar();
    header.as_ustar_mut().unwrap().name[..name.len()].copy_from_slice(name);
    header.set_entry_type(kind);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, bytes).unwrap();
}

/// appends a pax header holding `records`, which describe the entry after it.
pub(super) fn pax(tar: &mut Builder<Vec<u8>>, records: &[(&str, &[u8])]) {
    let mut data = Vec::new();
    for (key, value) in records {
        // a record starts with its own length in digits, counting those digits
        let rest = key.len() + value.len() + 3;
        let mut length = rest + 1;
        while length != rest + length.to_string().len() {
            length = rest + length.to_string().len();
        }
        data.extend_from_slice(format!("{length} {key}=").as_bytes());
        data.extend_from_slice(value);
        data.push(b'\n');
    }
    raw_entry(tar, b"PaxHeader", EntryType::XHeader, &data);
}

/// a 7z holding `files` in one solid block, as 7-Zip writes them by default. a name ending in `/`
/// is a folder, and a file without bytes holds no data, as 7-Zip keeps an empty file.
pub(super) fn seven_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    seven_zip_with(|writer| {
        let mut entries = Vec::new();
        let mut readers = Vec::new();
        for (name, bytes) in files {
            let empty = match name.strip_suffix('/') {
                Some(folder) => ArchiveEntry::new_directory(folder),
                None if bytes.is_empty() => ArchiveEntry::new_file(name),
                None => {
                    entries.push(ArchiveEntry::new_file(name));
                    readers.push(SourceReader::from(*bytes));
                    continue;
                }
            };
            writer.push_archive_entry::<&[u8]>(empty, None).unwrap();
        }
        if !entries.is_empty() {
            writer.push_archive_entries(entries, readers).unwrap();
        }
    })
}

/// a 7z of what `write` pushes into it.
pub(super) fn seven_zip_with(write: impl FnOnce(&mut ArchiveWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    write(&mut writer);
    writer.finish().unwrap().into_inner()
}

pub(super) fn compress(format: Format, bytes: &[u8]) -> Vec<u8> {
    match format {
        Format::Zip | Format::Tar => bytes.to_vec(),
        Format::SevenZ => panic!("a 7z is no compressed tar; use seven_zip"),
        Format::TarGz => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        Format::TarXz => {
            let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 1);
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        Format::TarZst => zstd::encode_all(bytes, 3).unwrap(),
        Format::TarBz2 => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
    }
}

/// `xz`, an xz stream holding one LZMA2 block, with that block's dictionary property set to
/// `property`, which says how much memory decoding needs, and its header checksum to match.
pub(super) fn xz_asking_for(xz: &[u8], property: u8) -> Vec<u8> {
    let mut xz = xz.to_vec();
    // the block header follows the 12-byte stream header and ends with its CRC32
    let start = 12;
    let size = (usize::from(xz[start]) + 1) * 4;
    let flags = xz[start + 1];
    let mut at = start + 2;
    for present in [0x40, 0x80] {
        if flags & present != 0 {
            while xz[at] & 0x80 != 0 {
                at += 1;
            }
            at += 1;
        }
    }
    assert_eq!(xz[at..at + 2], [0x21, 0x01], "one LZMA2 filter");
    xz[at + 2] = property;
    let mut crc = flate2::Crc::new();
    crc.update(&xz[start..start + size - 4]);
    xz[start + size - 4..start + size].copy_from_slice(&crc.sum().to_le_bytes());
    xz
}

/// a zstd frame whose header asks for a window of 2^`log` bytes, holding `bytes` in one raw block.
pub(super) fn zstd_asking_for(log: u8, bytes: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, (log - 10) << 3];
    let header = (u32::try_from(bytes.len()).unwrap() << 3) | 1;
    frame.extend_from_slice(&header.to_le_bytes()[..3]);
    frame.extend_from_slice(bytes);
    frame
}
