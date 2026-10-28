//! archives built at test time, and a scratch folder with a cache to unpack them into.

use std::fs;
use std::io::{Cursor, Write};
use std::path::PathBuf;

use ::tar::{Builder, EntryType, Header};
use ::zip::ZipWriter;
use ::zip::write::SimpleFileOptions;

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
        unpack(&path, format, limits, &self.cache, watch)
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

pub(super) fn compress(format: Format, bytes: &[u8]) -> Vec<u8> {
    match format {
        Format::Zip | Format::Tar => bytes.to_vec(),
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
