//! 7z archives, decoded a block at a time so every entry passes the guard in order, as tar's do.
//! the decoders get the memory [`Limits`](super::Limits) allows, and no password to decrypt with.

use std::borrow::Cow;
use std::io::{self, Read};

use sevenz_rust2::{ArchiveEntry, ArchiveReader, EncoderMethod, Password};

use super::guard::Guard;
use super::input::{Input, Source};
use super::names::Named;
use super::{Keep, damaged, past, refused, refused_member, unreadable};
use crate::error::Error;
use crate::units;

/// windows sets this attribute on a link or another reparse point.
const REPARSE_POINT: u32 = 0x400;
/// the attribute p7zip sets when the high 16 bits hold a unix mode.
const UNIX_EXTENSION: u32 = 0x8000;
const FILE_TYPE: u32 = 0o170_000;
const SYMLINK: u32 = 0o120_000;

pub(super) fn unpack(
    input: &Input,
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
) -> Result<(), Error> {
    let file = input.open(guard.meter())?;
    let source = Source::new(file, guard.meter()).map_err(|err| unreadable(input.first(), &err))?;
    let limit = usize::try_from(guard.limits().memory / 1024).unwrap_or(usize::MAX);
    let mut reader =
        ArchiveReader::new_with_memory_limit(source, Password::empty(), limit).map_err(failed)?;
    // lzma2's threaded decoder holds a block whole in memory, packed and unpacked, before any of
    // it reaches the guard
    reader.set_thread_count(1);
    let files = reader.archive().files.len();
    guard.check_entries(files as u64)?;
    if (0..files).any(|index| reader.archive().is_file_encrypted(index)) {
        return Err(encrypted());
    }
    // the closure can fail only with the crate's own error, so fetchloom's error waits here
    let mut stopped = None;
    let walked = reader.for_each_entries(|entry, read| {
        take(guard, keep, entry, read)
            .map(|()| true)
            .map_err(|error| {
                stopped = Some(error);
                sevenz_rust2::Error::from(io::Error::other("stopped"))
            })
    });
    if let Some(error) = stopped {
        return Err(error);
    }
    walked.map_err(failed)
}

/// passes one entry through the guard, and keeps it when it is a file.
fn take(
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
    entry: &ArchiveEntry,
    read: &mut dyn Read,
) -> Result<(), Error> {
    guard.entry()?;
    let raw = name(entry);
    if entry.is_anti_item {
        return guard.pass(&raw, read);
    }
    if is_link(entry) {
        return Err(refused_member(
            &raw,
            "it is a link, which could point outside its folder",
        ));
    }
    // the crate decodes an entry no file keeps anyway, so passing it counts toward the ratio
    match guard.name(&raw, entry.is_directory)? {
        Named::File(path) => {
            guard.fits(&path, entry.size)?;
            keep(guard, path, entry.size, read)
        }
        Named::System(path) => {
            guard.pass(&raw, read)?;
            guard.leave_out(path);
            Ok(())
        }
        // a name such as `data/` makes a folder of an entry that holds bytes
        Named::Folder => guard.pass(&raw, read),
    }
}

/// the entry's name with `/` between folders. names made on windows may use `\`, which windows
/// never allows inside a name; p7zip marks names made on unix, where `\` can be part of one.
fn name(entry: &ArchiveEntry) -> Cow<'_, str> {
    if entry.has_windows_attributes && entry.windows_attributes & UNIX_EXTENSION == 0 {
        Cow::Owned(entry.name.replace('\\', "/"))
    } else {
        Cow::Borrowed(&entry.name)
    }
}

/// whether the entry is a symbolic link or another reparse point, as p7zip and windows mark one.
fn is_link(entry: &ArchiveEntry) -> bool {
    if !entry.has_windows_attributes {
        return false;
    }
    let attributes = entry.windows_attributes;
    attributes & REPARSE_POINT != 0
        || (attributes & UNIX_EXTENSION != 0 && (attributes >> 16) & FILE_TYPE == SYMLINK)
}

fn encrypted() -> Error {
    refused("the archive is encrypted, and fetchloom unpacks no encrypted archive")
}

fn failed(err: sevenz_rust2::Error) -> Error {
    use sevenz_rust2::Error as Seven;
    match err {
        Seven::PasswordRequired | Seven::MaybeBadPassword(_) => encrypted(),
        // built without its aes decoder, the crate names the cipher as it would a compression
        Seven::UnsupportedCompressionMethod(method)
            if method == EncoderMethod::AES256_SHA256.name() =>
        {
            encrypted()
        }
        Seven::UnsupportedCompressionMethod(method) => refused(format!(
            "the archive is compressed with {}, which fetchloom does not unpack",
            method.to_lowercase()
        )),
        Seven::MaxMemLimited { max_kb, actaul_kb } => {
            let kib = |count: usize| units::size((count as u64).saturating_mul(1024));
            past(
                format!(
                    "the archive needs {} of memory to unpack, more than the {} one archive may take",
                    kib(actaul_kb),
                    kib(max_kb)
                ),
                "FETCHLOOM_MEMORY, which gives each archive a quarter of it",
                "memory",
            )
        }
        Seven::Io(error, _) | Seven::FileOpen(error, _) => damaged(None, &error),
        other => damaged(None, &other),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{self, Cursor};

    use sevenz_rust2::encoder_options::Lzma2Options;
    use sevenz_rust2::{Archive, ArchiveEntry, Error as Seven, Password};

    use super::super::samples::{Scratch, seven_zip, seven_zip_with};
    use super::super::{Format, Limits, tree_hash, unpack};
    use super::{REPARSE_POINT, UNIX_EXTENSION, failed};
    use crate::error::Kind;

    const ENCRYPTED: &str = "the archive is encrypted, and fetchloom unpacks no encrypted archive";
    /// the attribute windows gives a file it has not backed up since it changed.
    const ARCHIVE: u32 = 0x20;

    /// a folder, three files and an empty file.
    const TREE: &[(&str, &[u8])] = &[
        ("data/", b""),
        ("data/train.csv", b"x,y\n1,2\n"),
        ("data/test.csv", b"x,y\n3,4\n"),
        ("data/empty.txt", b""),
        ("README.md", b"# readme\n"),
    ];

    /// an archive in tests/fixtures/archives. sevenz-rust2 0.25 wrote the two encrypted 7z ones
    /// with the password `fetchloom`: `sevenz-rust2-encrypted.7z` holds README.md in the clear,
    /// then data/train.csv encrypted, as `7z u -p` adds a file; the other encrypts its header too.
    fn fixture(name: &str) -> Vec<u8> {
        let path = crate::test_paths::workspace()
            .join("tests/fixtures/archives")
            .join(name);
        std::fs::read(path).unwrap()
    }

    /// a 7z of one file named `name`, whose windows attributes are `attributes`.
    fn one_file(name: &str, attributes: u32) -> Vec<u8> {
        seven_zip_with(|writer| {
            let mut entry = ArchiveEntry::new_file(name);
            entry.has_windows_attributes = true;
            entry.windows_attributes = attributes;
            writer.push_archive_entry(entry, Some(&b"1"[..])).unwrap();
        })
    }

    fn blocks(bytes: &[u8]) -> usize {
        let archive = Archive::read(&mut Cursor::new(bytes), &Password::empty()).unwrap();
        archive.blocks.len()
    }

    #[test]
    fn a_solid_block_and_a_block_per_file_unpack_alike() {
        let per_file = seven_zip_with(|writer| {
            for (name, bytes) in TREE {
                let entry = match name.strip_suffix('/') {
                    Some(folder) => ArchiveEntry::new_directory(folder),
                    None => ArchiveEntry::new_file(name),
                };
                let data = (!bytes.is_empty()).then_some(*bytes);
                writer.push_archive_entry(entry, data).unwrap();
            }
        });
        let solid = seven_zip(TREE);
        assert_eq!((blocks(&solid), blocks(&per_file)), (1, 3));
        for bytes in [solid, per_file] {
            let scratch = Scratch::new();
            let unpacked = scratch.unpack(Format::SevenZ, &bytes).unwrap();
            let paths: Vec<_> = unpacked.members.iter().map(|m| m.path.as_str()).collect();
            assert_eq!(
                paths,
                [
                    "README.md",
                    "data/empty.txt",
                    "data/test.csv",
                    "data/train.csv"
                ]
            );
            for member in &unpacked.members {
                let (_, bytes) = TREE.iter().find(|(name, _)| *name == member.path).unwrap();
                scratch.assert_stored(member, bytes);
            }
            assert_eq!(unpacked.tree, tree_hash(&unpacked.members));
        }
    }

    #[test]
    fn unsafe_names_refuse_the_whole_archive() {
        for (archive, reason) in [
            (
                seven_zip(&[("a.csv", b"1"), ("../evil.csv", b"2")]),
                "\"../evil.csv\", which fetchloom refuses because it climbs out of its folder",
            ),
            (
                seven_zip(&[("/etc/passwd", b"1")]),
                "\"/etc/passwd\", which fetchloom refuses because it starts at the root of the disk",
            ),
            (
                seven_zip(&[("a.csv", b"1"), ("a.csv", b"2")]),
                "the archive holds \"a.csv\" twice",
            ),
        ] {
            let err = Scratch::new().refuse(Format::SevenZ, &archive);
            assert!(err.message.contains(reason), "{}", err.message);
        }
    }

    #[test]
    fn links_are_refused() {
        let symlink = UNIX_EXTENSION | 0o120_777 << 16;
        for attributes in [symlink, ARCHIVE | REPARSE_POINT] {
            let err = Scratch::new().refuse(Format::SevenZ, &one_file("link", attributes));
            assert_eq!(
                err.message,
                "the archive holds \"link\", which fetchloom refuses because it is a link, which could point outside its folder",
                "{attributes:#x}"
            );
        }
    }

    #[test]
    fn a_dictionary_past_the_memory_limit_names_its_setting() {
        let archive = seven_zip_with(|writer| {
            let mut options = Lzma2Options::from_level(1);
            options.set_dictionary_size(16 << 20);
            writer.set_content_methods(vec![options.into()]);
            let entry = ArchiveEntry::new_file("a.csv");
            writer.push_archive_entry(entry, Some(&b"1"[..])).unwrap();
        });
        // more than the 8 MiB the header's own dictionary takes. lzma2 needs its dictionary and
        // 104 KiB of buffers
        let limits = Limits {
            memory: 12 << 20,
            ..Limits::default()
        };
        let err = Scratch::new()
            .unpack_with(Format::SevenZ, &archive, &limits, &|_| false)
            .unwrap_err();
        assert_eq!(
            err.message,
            "the archive needs 16.1 MiB of memory to unpack, more than the 12.0 MiB one archive may take"
        );
        let help = err.help.unwrap_or_default();
        assert!(help.contains("FETCHLOOM_MEMORY"), "{help}");
    }

    #[test]
    fn the_entry_count_is_checked_before_any_block_decodes() {
        let mut bytes = seven_zip(&[("a.csv", b"1"), ("b.csv", b"2"), ("c.csv", b"3")]);
        assert_eq!(&bytes[35..38], b"123");
        bytes[35] ^= 0x20;
        let damaged = Scratch::new().refuse(Format::SevenZ, &bytes);
        assert!(
            damaged.message.starts_with("the archive is damaged"),
            "{}",
            damaged.message
        );
        let limits = Limits {
            members: 2,
            ..Limits::default()
        };
        let err = Scratch::new()
            .unpack_with(Format::SevenZ, &bytes, &limits, &|_| false)
            .unwrap_err();
        assert!(
            err.message.contains("more than 2 entries"),
            "{}",
            err.message
        );
    }

    #[test]
    fn cut_or_changed_bytes_are_a_damaged_archive() {
        let whole = seven_zip(&[("a.csv", b"1,2\n3,4\n")]);
        assert_eq!(&whole[35..43], b"1,2\n3,4\n");
        let mut data = whole.clone();
        data[36] ^= 0x01;
        let mut header = whole.clone();
        *header.last_mut().unwrap() ^= 0x01;
        for (case, bytes) in [
            ("cut", &whole[..whole.len() / 2]),
            ("data", &data[..]),
            ("header", &header[..]),
        ] {
            let err = Scratch::new().refuse(Format::SevenZ, bytes);
            assert_eq!(err.kind, Kind::SourceInvalid, "{case}");
            assert!(
                err.message.starts_with("the archive is damaged"),
                "{case}: {}",
                err.message
            );
        }
    }

    #[test]
    fn a_read_error_stays_local() {
        let scratch = Scratch::new();
        let folder = tempfile::tempdir().unwrap();
        let limits = Limits::default();
        let err = unpack(
            &crate::archive::Input::File(folder.path().to_path_buf()),
            Format::SevenZ,
            &limits,
            &scratch.cache,
            &|_| false,
            None,
        )
        .unwrap_err();
        assert_eq!(err.kind, Kind::LocalIo, "{}", err.message);
    }

    /// any bytes of a 7z changed: unpacking it never panics, and fails only as a damaged or
    /// refused archive.
    #[test]
    fn fuzz_seven_zip() {
        let archive = seven_zip(TREE);
        let scratch = Scratch::new();
        bolero::check!()
            .with_type::<Vec<(u16, u8)>>()
            .for_each(|edits| {
                let mut bytes = archive.clone();
                for (at, byte) in edits {
                    let at = usize::from(*at) % bytes.len();
                    bytes[at] = *byte;
                }
                if let Err(err) = scratch.unpack(Format::SevenZ, &bytes) {
                    assert_eq!(err.kind, Kind::SourceInvalid, "{}", err.message);
                }
            });
    }

    #[test]
    fn backslashes_separate_folders_only_in_names_made_on_windows() {
        let windows = one_file("data\\a.csv", ARCHIVE);
        let unpacked = Scratch::new().unpack(Format::SevenZ, &windows).unwrap();
        assert_eq!(unpacked.members[0].path, "data/a.csv");
        let unix = one_file("data\\a.csv", UNIX_EXTENSION | 0o100_644 << 16);
        let err = Scratch::new().refuse(Format::SevenZ, &unix);
        assert!(err.message.contains("folder separator"), "{}", err.message);
    }

    #[test]
    fn bytes_no_file_keeps_count_toward_the_ratio() {
        let zeros = vec![0; 5 << 20];
        let archive = seven_zip_with(|writer| {
            let named_as_folder = ArchiveEntry::new_file("zeros/");
            writer
                .push_archive_entry(named_as_folder, Some(&zeros[..]))
                .unwrap();
        });
        let err = Scratch::new().refuse(Format::SevenZ, &archive);
        assert!(err.message.contains("1000 times"), "{}", err.message);
    }

    #[test]
    fn a_stop_ends_a_block_before_it_is_read_whole() {
        let mut noise = vec![0; 2 << 20];
        fastrand::Rng::with_seed(7).fill(&mut noise);
        let archive = seven_zip(&[("noise.bin", &noise)]);
        let read = Cell::new(0);
        let stop = |compressed: u64| {
            read.set(compressed);
            compressed > 256 << 10
        };
        let err = Scratch::new()
            .unpack_with(Format::SevenZ, &archive, &Limits::default(), &stop)
            .unwrap_err();
        assert_eq!(err.kind, Kind::Interrupted);
        let half = archive.len() as u64 / 2;
        assert!(
            read.get() < half,
            "{} of {} read",
            read.get(),
            archive.len()
        );
    }

    #[test]
    fn an_encrypted_block_refuses_the_archive_before_any_block_decodes() {
        let mut bytes = fixture("sevenz-rust2-encrypted.7z");
        // damage README.md, whose block is in the clear and decodes first
        assert_eq!(&bytes[35..43], b"# sample");
        bytes[37] ^= 0x20;
        let err = Scratch::new().refuse(Format::SevenZ, &bytes);
        assert_eq!(err.message, ENCRYPTED);
    }

    #[test]
    fn each_failure_is_told_by_its_cause() {
        let disk = || io::Error::from_raw_os_error(5);
        let damaged = "the archive is damaged: ";
        let cases = [
            (Seven::PasswordRequired, Kind::SourceInvalid, ENCRYPTED),
            (
                Seven::MaybeBadPassword(io::ErrorKind::InvalidData.into()),
                Kind::SourceInvalid,
                ENCRYPTED,
            ),
            (
                Seven::UnsupportedCompressionMethod("AES256_SHA256".to_owned()),
                Kind::SourceInvalid,
                ENCRYPTED,
            ),
            (
                Seven::UnsupportedCompressionMethod("PPMD".to_owned()),
                Kind::SourceInvalid,
                "the archive is compressed with ppmd, which fetchloom does not unpack",
            ),
            (
                Seven::MaxMemLimited {
                    max_kb: 1024,
                    actaul_kb: 4096,
                },
                Kind::SourceInvalid,
                "the archive needs 4.0 MiB of memory to unpack, more than the 1.0 MiB one archive may take",
            ),
            (
                Seven::Io(disk(), "".into()),
                Kind::LocalIo,
                "could not read the cached archive: ",
            ),
            (
                Seven::FileOpen(disk(), "archive".to_owned()),
                Kind::LocalIo,
                "could not read the cached archive: ",
            ),
            (
                Seven::Io(
                    io::Error::other(Seven::ChecksumVerificationFailed),
                    "".into(),
                ),
                Kind::SourceInvalid,
                damaged,
            ),
            (
                Seven::Io(io::ErrorKind::UnexpectedEof.into(), "".into()),
                Kind::SourceInvalid,
                damaged,
            ),
            (Seven::NextHeaderCrcMismatch, Kind::SourceInvalid, damaged),
        ];
        for (cause, kind, message) in cases {
            let told = format!("{cause:?}");
            let err = failed(cause);
            assert_eq!(err.kind, kind, "{told}");
            assert!(err.message.starts_with(message), "{told}: {}", err.message);
        }
    }
}
