//! zip archives, read through their central directory. every entry is checked before the first
//! byte is written.

mod directory;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use ::zip::read::ZipFileEntry;
use ::zip::{SUPPORTED_COMPRESSION_METHODS, System, ZipArchive};

use super::guard::{Guard, Meter};
use super::input::{Input, Joined, Source};
use super::names::Named;
use super::{Keep, damaged, refused, refused_member, unreadable};
use crate::error::Error;

/// the file type bits of a unix mode, as zip entries made on unix carry them.
const FILE_TYPE: u32 = 0o170_000;
const REGULAR: u32 = 0o100_000;
const DIRECTORY: u32 = 0o040_000;

pub(super) fn unpack(
    input: &Input,
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
) -> Result<(), Error> {
    let mut file = input.open(guard.meter())?;
    directory::check(&mut file, guard)?;
    let source = Source::new(file, guard.meter()).map_err(|err| unreadable(input.first(), &err))?;
    let memory = guard.limits().memory;
    let mut zip = ZipArchive::new(source).map_err(|err| damaged(None, &err, memory))?;
    if zip.has_duplicate_names() {
        let name = repeated_name(input, zip.central_directory_start())
            .unwrap_or_else(|| "a name".to_owned());
        return Err(refused(format!("the archive holds \"{name}\" twice")));
    }
    for (index, path) in checked(&zip, guard)? {
        let mut file = zip
            .by_index(index)
            .map_err(|err| damaged(Some(&path), &err, memory))?;
        guard.fits(&path, file.size())?;
        keep(guard, path, file.size(), &mut file)?;
    }
    Ok(())
}

/// every file in the central directory, after each entry passed the guard.
fn checked(
    zip: &ZipArchive<Source<'_>>,
    guard: &mut Guard<'_>,
) -> Result<Vec<(usize, String)>, Error> {
    let mut files = Vec::new();
    let memory = guard.limits().memory;
    for index in 0..zip.len() {
        guard.entry()?;
        let entry = zip
            .by_index_data(index)
            .map_err(|err| damaged(None, &err, memory))?;
        let raw = name(&entry).map_err(|err| damaged(None, &err, memory))?;
        let is_folder = is_folder(&entry);
        if let Some(reason) = refusal(&entry, is_folder) {
            return Err(refused_member(&raw, &reason));
        }
        match guard.name(&raw, is_folder)? {
            Named::File(path) => files.push((index, path)),
            Named::System(path) => guard.leave_out(path),
            Named::Folder => {}
        }
    }
    Ok(files)
}

/// the entry's name with `/` between folders. zips made on windows may use `\` instead, as
/// powershell's Compress-Archive does, and windows never allows `\` inside a name.
fn name(entry: &ZipFileEntry<'_>) -> ::zip::result::ZipResult<String> {
    let name = entry.name()?;
    Ok(
        if matches!(
            entry.system(),
            System::Dos | System::WindowsNTFS | System::VFAT
        ) {
            name.replace('\\', "/")
        } else {
            name.into_owned()
        },
    )
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
fn repeated_name(input: &Input, directory: u64) -> Option<String> {
    let mut file = BufReader::new(Joined::open(input).ok()?);
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

/// the name of the one entry a zip holds, when it holds exactly one and that one is a file.
pub(super) fn sole_member(archive: &Path) -> Option<String> {
    let mut file = BufReader::new(File::open(archive).ok()?);
    let limits = crate::archive::Limits {
        members: 1,
        ..crate::archive::Limits::default()
    };
    let meter = Meter::default();
    let never = |_: u64| false;
    let guard = Guard::new(&limits, &never, &meter);
    directory::check(&mut file, &guard).ok()?;
    let zip = ZipArchive::new(file).ok()?;
    if zip.len() != 1 {
        return None;
    }
    let entry = zip.by_index_data(0).ok()?;
    if is_folder(&entry) {
        return None;
    }
    let name = name(&entry).ok()?;
    Some(name.trim_start_matches("./").to_owned())
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::{Cursor, Write};

    use ::zip::write::SimpleFileOptions;
    use ::zip::{CompressionMethod, System, ZipWriter};

    use super::super::samples::{Scratch, zip_of, zip_with};
    use super::super::{Format, Unpacked, tree_hash};
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

    fn assert_files(scratch: &Scratch, unpacked: &Unpacked) {
        let paths: Vec<_> = unpacked
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect();
        assert_eq!(paths, ["README.md", "data/empty.txt", "data/train.csv"]);
        for member in &unpacked.members {
            let (_, bytes) = FILES.iter().find(|(path, _)| *path == member.path).unwrap();
            assert_eq!(member.size, bytes.len() as u64, "{}", member.path);
            scratch.assert_stored(member, bytes);
        }
    }

    #[test]
    fn every_method_fetchloom_reads_unpacks() {
        for method in [
            CompressionMethod::Stored,
            CompressionMethod::Bzip2,
            CompressionMethod::Zstd,
        ] {
            let scratch = Scratch::new();
            let options = SimpleFileOptions::default().compression_method(method);
            let zip = zip_with(ZipWriter::new(Cursor::new(Vec::new())), FILES, options);
            let unpacked = scratch.unpack(Format::Zip, &zip.into_inner()).unwrap();
            assert_files(&scratch, &unpacked);
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
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(Format::Zip, &bytes).unwrap();
        assert_files(&scratch, &unpacked);
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
    fn backslashes_separate_folders_only_in_zips_made_on_windows() {
        let files: &[(&str, &[u8])] = &[("data\\a.csv", b"1")];
        let on = |system| {
            let options = SimpleFileOptions::default().system(system);
            zip_with(ZipWriter::new(Cursor::new(Vec::new())), files, options).into_inner()
        };
        let unpacked = Scratch::new()
            .unpack(Format::Zip, &on(System::Dos))
            .unwrap();
        assert_eq!(unpacked.members[0].path, "data/a.csv");
        let err = Scratch::new().refuse(Format::Zip, &on(System::Unix));
        assert!(err.message.contains("folder separator"), "{}", err.message);
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

    /// a zip lists its files at its end, so one cut short is told in those words rather than in
    /// the decoder's.
    #[test]
    fn a_zip_cut_short_says_so() {
        let zip = zip_of(FILES);
        let err = Scratch::new().refuse(Format::Zip, &zip[..zip.len() / 2]);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive is cut short: a zip lists its files at its end, and that list is missing"
        );
        assert!(err.help.unwrap().contains("fixed copy"));
    }

    #[test]
    fn a_dropbox_zip_ignores_its_root_directory_entry() {
        let mut bytes = zip_of(&[
            ("/", b""),
            ("readme.md", b"readme\n"),
            ("nested/", b""),
            ("nested/data.csv", b"x,y\n1,2\n"),
        ]);
        // dropbox marks entries as unix, with permission bits and DOS directory or archive flags
        for at in signatures(&bytes, *b"PK\x01\x02") {
            bytes[at + 5] = 3;
            let length = usize::from(u16::from_le_bytes([bytes[at + 28], bytes[at + 29]]));
            let directory = bytes[at + 46..at + 46 + length].ends_with(b"/");
            let attributes = if directory {
                0x01e4_0010_u32
            } else {
                0x01e4_0020_u32
            };
            bytes[at + 38..at + 42].copy_from_slice(&attributes.to_le_bytes());
        }
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(Format::Zip, &bytes).unwrap();
        let paths: Vec<_> = unpacked
            .members
            .iter()
            .map(|member| member.path.as_str())
            .collect();
        assert_eq!(paths, ["nested/data.csv", "readme.md"]);
        scratch.assert_stored(&unpacked.members[0], b"x,y\n1,2\n");
        scratch.assert_stored(&unpacked.members[1], b"readme\n");
    }

    /// a zip of `count` members that all share the bytes of one stored member of `size` bytes.
    pub(in crate::archive) fn overlapping(count: usize, size: usize) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "member-0000.bin",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(&vec![b'x'; size]).unwrap();
        for index in 1..count {
            writer
                .shallow_copy_file("member-0000.bin", &format!("member-{index:04}.bin"))
                .unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// members that share one member's bytes count each time they unpack: 8 sharing 1 MiB pass 4
    /// times the archive at the fifth, once past the 4 MiB the ratio waits for. few large members
    /// keep this quick where each unpacked file is flushed on its own, as on windows.
    #[test]
    fn overlapping_members_cannot_expand_past_the_archive_ratio() {
        let payload = vec![b'x'; 1 << 20];
        let bytes = overlapping(8, payload.len());
        let limits = crate::archive::Limits {
            ratio: 4,
            ..crate::archive::Limits::default()
        };
        assert!((payload.len() as u64) * 8 > bytes.len() as u64 * limits.ratio);
        assert!(
            ::zip::ZipArchive::new(Cursor::new(&bytes))
                .unwrap()
                .has_overlapping_files()
                .unwrap()
        );
        let scratch = Scratch::new();
        let path = scratch.file("shared.zip", &bytes);
        let result = crate::archive::unpack(
            &crate::archive::Input::File(path),
            Format::Zip,
            &limits,
            &scratch.cache,
            &|_| false,
            Some(0),
        );
        let Err(error) = result else {
            panic!("shared zip data bypassed the archive's physical expansion limit");
        };
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("past 4 times"), "{}", error.message);
        scratch.assert_empty();
    }

    #[test]
    fn directory_entry_limit_precedes_later_directory_damage() {
        let mut bytes = zip_of(&[("first", b"1"), ("second", b"2")]);
        let headers = signatures(&bytes, *b"PK\x01\x02");
        bytes[headers[1]] = b'Q';
        let limits = crate::archive::Limits {
            members: 1,
            ..crate::archive::Limits::default()
        };
        let error = Scratch::new()
            .unpack_with(Format::Zip, &bytes, &limits, &|_| false)
            .unwrap_err();
        assert!(
            error.message.contains("more than 1 entry"),
            "{}",
            error.message
        );
    }

    #[test]
    fn directory_name_limit_precedes_later_directory_damage() {
        let name = "x".repeat(4097);
        let mut bytes = zip_of(&[(&name, b"1"), ("second", b"2")]);
        let headers = signatures(&bytes, *b"PK\x01\x02");
        bytes[headers[1]] = b'Q';
        let error = Scratch::new().refuse(Format::Zip, &bytes);
        assert!(
            error.message.contains("longer than 4,096 bytes"),
            "{}",
            error.message
        );
    }

    #[test]
    fn footer_signatures_inside_a_member_are_not_directory_metadata() {
        let mut payload = vec![0; 22];
        payload[..4].copy_from_slice(b"PK\x05\x06");
        payload[8..12].copy_from_slice(&[2, 0, 2, 0]);
        payload[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let zip = zip_with(
            ZipWriter::new(Cursor::new(Vec::new())),
            &[("data", &payload)],
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        );
        let scratch = Scratch::new();
        let result = scratch.unpack(Format::Zip, &zip.into_inner()).unwrap();
        scratch.assert_stored(&result.members[0], &payload);
    }

    #[test]
    fn an_empty_zip_ignores_footer_shaped_prefixes() {
        let mut bytes = vec![0; 23];
        bytes[1..5].copy_from_slice(b"PK\x05\x06");
        bytes[9..13].copy_from_slice(&[2, 0, 2, 0]);
        bytes.extend(zip_of(&[]));
        assert_eq!(
            ::zip::ZipArchive::new(Cursor::new(&bytes)).unwrap().len(),
            0
        );
        let limits = crate::archive::Limits {
            members: 1,
            ..crate::archive::Limits::default()
        };
        let unpacked = Scratch::new()
            .unpack_with(Format::Zip, &bytes, &limits, &|_| false)
            .unwrap();
        assert_eq!(unpacked.members.len(), 0);
    }
}
