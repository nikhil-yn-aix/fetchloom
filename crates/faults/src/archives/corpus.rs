//! The named corpus of hostile and benign archives the archive reader
//! adversarial suite is run against.

use super::deflate::deflate_repeated_byte;
use super::tar::{
    TYPEFLAG_BLOCKDEV, TYPEFLAG_CHARDEV, TYPEFLAG_DIRECTORY, TYPEFLAG_FIFO, TYPEFLAG_HARDLINK,
    TYPEFLAG_PAX, TYPEFLAG_REGULAR, TYPEFLAG_SYMLINK, TarHeader, TarWriter, pax_block, pax_record,
};
use super::zip::{METHOD_DEFLATE, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter, crc32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Tar,
    Zip,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    Rejected { kind: &'static str, member: String },
    Benign,
}

#[derive(Clone, Debug)]
pub struct CorpusEntry {
    pub(super) name: &'static str,
    pub(super) container: Container,
    pub(super) bytes: Vec<u8>,
    pub(super) attacks: &'static str,
    pub(super) expectation: Expectation,
}

impl CorpusEntry {
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    #[must_use]
    pub fn container(&self) -> Container {
        self.container
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn attacks(&self) -> &'static str {
        self.attacks
    }

    #[must_use]
    pub fn expectation(&self) -> &Expectation {
        &self.expectation
    }
}

#[derive(Clone, Debug)]
pub struct Corpus {
    entries: Vec<CorpusEntry>,
}

impl Corpus {
    #[must_use]
    pub fn build() -> Self {
        Self {
            entries: corpus_entries(),
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[CorpusEntry] {
        &self.entries
    }

    #[must_use]
    pub fn hostile(&self) -> Vec<&CorpusEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.expectation, Expectation::Rejected { .. }))
            .collect()
    }

    #[must_use]
    pub fn benign(&self) -> Vec<&CorpusEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.expectation, Expectation::Benign))
            .collect()
    }
}

fn tar_single(name: &[u8], typeflag: u8, mode: u32, linkname: &[u8], data: &[u8]) -> Vec<u8> {
    let mut header = TarHeader::ustar(name, typeflag);
    header.set_mode(mode).set_size(data.len() as u64);
    if !linkname.is_empty() {
        header.set_linkname(linkname);
    }
    let mut writer = TarWriter::new();
    writer.push(&header, data);
    writer.finish()
}

fn tar_device(name: &[u8], typeflag: u8, devmajor: u32, devminor: u32) -> Vec<u8> {
    let mut header = TarHeader::ustar(name, typeflag);
    header.set_devmajor(devmajor).set_devminor(devminor);
    let mut writer = TarWriter::new();
    writer.push(&header, b"");
    writer.finish()
}

fn rejected(kind: &'static str, member: &str) -> Expectation {
    Expectation::Rejected {
        kind,
        member: member.to_string(),
    }
}

fn entry(
    name: &'static str,
    container: Container,
    bytes: Vec<u8>,
    attacks: &'static str,
    expectation: Expectation,
) -> CorpusEntry {
    CorpusEntry {
        name,
        container,
        bytes,
        attacks,
        expectation,
    }
}

fn tar_pax_entry(
    name: &'static str,
    attacks: &'static str,
    records: &[Vec<u8>],
    real_name: &[u8],
    data: &[u8],
    expectation: Expectation,
) -> CorpusEntry {
    let pax_data = pax_block(records);
    let mut pax_header = TarHeader::ustar(b"pax_header", TYPEFLAG_PAX);
    pax_header.set_size(pax_data.len() as u64);
    let mut file_header = TarHeader::ustar(real_name, TYPEFLAG_REGULAR);
    file_header.set_size(data.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&pax_header, &pax_data);
    writer.push(&file_header, data);
    entry(name, Container::Tar, writer.finish(), attacks, expectation)
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat list of named corpus entries reads more clearly than a spread of tiny helpers"
)]
fn corpus_entries() -> Vec<CorpusEntry> {
    let mut entries = Vec::new();

    entries.push(entry(
        "tar_absolute_path",
        Container::Tar,
        tar_single(b"/etc/passwd", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path rooted at the filesystem root",
        rejected("archive.unsafe_path", "/etc/passwd"),
    ));

    entries.push(entry(
        "tar_dotdot_leading",
        Container::Tar,
        tar_single(b"../evil.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path starting with a parent-directory component",
        rejected("archive.unsafe_path", "../evil.txt"),
    ));

    entries.push(entry(
        "tar_dotdot_embedded",
        Container::Tar,
        tar_single(b"docs/../../secret.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path with a parent-directory component after the first segment",
        rejected("archive.unsafe_path", "docs/../../secret.txt"),
    ));

    entries.push(entry(
        "tar_backslash_path",
        Container::Tar,
        tar_single(b"dir\\evil.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path using a backslash, which Windows cannot store in a name",
        rejected("archive.unsafe_path", "dir\\evil.txt"),
    ));

    entries.push(entry(
        "tar_windows_drive_path",
        Container::Tar,
        tar_single(b"C:/Windows/evil.dll", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path naming an absolute Windows drive location",
        rejected("archive.unsafe_path", "C:/Windows/evil.dll"),
    ));

    entries.push(entry(
        "tar_symlink_absolute_target",
        Container::Tar,
        tar_single(b"link1", TYPEFLAG_SYMLINK, 0o777, b"/etc/passwd", b""),
        "a symlink whose target is an absolute path",
        rejected("archive.link_escape", "link1"),
    ));

    entries.push(entry(
        "tar_symlink_dotdot_target",
        Container::Tar,
        tar_single(b"link2", TYPEFLAG_SYMLINK, 0o777, b"../../etc/passwd", b""),
        "a symlink whose target climbs out of the destination with parent-directory components",
        rejected("archive.link_escape", "link2"),
    ));

    entries.push(entry(
        "tar_symlink_dotdot_then_write_through",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let link = TarHeader::ustar(b"escape", TYPEFLAG_SYMLINK);
            let mut link = link;
            link.set_linkname(b"..");
            writer.push(&link, b"");
            let mut file = TarHeader::ustar(b"escape/evil.txt", TYPEFLAG_REGULAR);
            file.set_size(1);
            writer.push(&file, b"x");
            writer.finish()
        },
        "a symlink pointing at the parent directory, followed by a member written through it",
        rejected("archive.link_escape", "escape"),
    ));

    entries.push(entry(
        "tar_hardlink_outside",
        Container::Tar,
        tar_single(b"hard1", TYPEFLAG_HARDLINK, 0o644, b"../../etc/passwd", b""),
        "a hard link naming a target outside the destination",
        rejected("archive.link_escape", "hard1"),
    ));

    entries.push(entry(
        "tar_block_device",
        Container::Tar,
        tar_device(b"dev/sda", TYPEFLAG_BLOCKDEV, 8, 0),
        "a block device entry",
        rejected("archive.unsupported", "dev/sda"),
    ));

    entries.push(entry(
        "tar_char_device",
        Container::Tar,
        tar_device(b"dev/tty", TYPEFLAG_CHARDEV, 5, 0),
        "a character device entry",
        rejected("archive.unsupported", "dev/tty"),
    ));

    entries.push(entry(
        "tar_fifo",
        Container::Tar,
        tar_single(b"pipe", TYPEFLAG_FIFO, 0o644, b"", b""),
        "a FIFO entry",
        rejected("archive.unsupported", "pipe"),
    ));

    entries.push(entry(
        "tar_setuid_mode",
        Container::Tar,
        tar_single(b"suid.bin", TYPEFLAG_REGULAR, 0o4755, b"", b"x"),
        "a regular file carrying the setuid bit",
        rejected("archive.unsupported", "suid.bin"),
    ));

    entries.push(entry(
        "tar_setgid_mode",
        Container::Tar,
        tar_single(b"sgid.bin", TYPEFLAG_REGULAR, 0o2755, b"", b"x"),
        "a regular file carrying the setgid bit",
        rejected("archive.unsupported", "sgid.bin"),
    ));

    entries.push(entry(
        "tar_duplicate_member",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"dup.txt", TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(b"dup.txt", TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "the same member path written twice",
        rejected("archive.collision", "dup.txt"),
    ));

    let long_component: String = std::iter::repeat_n('a', 300).collect();
    let long_name = format!("{long_component}.txt");
    entries.push(tar_pax_entry(
        "tar_path_too_long",
        "a single path component longer than any platform's name limit",
        &[pax_record("path", &long_name)],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", &long_name),
    ));

    entries.push(entry(
        "tar_path_invalid_utf8",
        Container::Tar,
        tar_single(b"bad\xFF\xFE.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path that is not valid UTF-8",
        rejected("archive.unsafe_path", "bad\u{fffd}\u{fffd}.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_path_with_nul",
        "a member path containing an embedded NUL byte",
        &[pax_record("path", "bad\u{0}name.txt")],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", "bad\u{0}name.txt"),
    ));

    let deep_name = {
        use std::fmt::Write as _;
        let mut path = String::new();
        for index in 0..70 {
            let _ = write!(path, "d{index}/");
        }
        path.push_str("file.txt");
        path
    };
    entries.push(tar_pax_entry(
        "tar_nesting_too_deep",
        "a member path nested more than 64 components deep",
        &[pax_record("path", &deep_name)],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", &deep_name),
    ));

    entries.push(entry(
        "tar_case_collision",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"A.txt", TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(b"a.txt", TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "two member paths colliding only under case folding",
        rejected("archive.collision", "a.txt"),
    ));

    entries.push(entry(
        "tar_unicode_normalization_collision",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let precomposed = "caf\u{e9}.txt";
            let decomposed = "cafe\u{301}.txt";
            let mut first = TarHeader::ustar(precomposed.as_bytes(), TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(decomposed.as_bytes(), TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "two member paths colliding only under Unicode normalization",
        rejected("archive.collision", "cafe\u{301}.txt"),
    ));

    entries.push(entry(
        "tar_windows_reserved_con",
        Container::Tar,
        tar_single(b"CON", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path matching a Windows-reserved device name",
        rejected("destination.unrepresentable", "CON"),
    ));

    entries.push(entry(
        "tar_windows_reserved_aux_txt",
        Container::Tar,
        tar_single(b"aux.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path matching a Windows-reserved device name with an extension",
        rejected("destination.unrepresentable", "aux.txt"),
    ));

    entries.push(entry(
        "tar_windows_reserved_trailing_dot",
        Container::Tar,
        tar_single(b"trailing.", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path ending in a dot, which Windows cannot store",
        rejected("destination.unrepresentable", "trailing."),
    ));

    entries.push(entry(
        "tar_windows_reserved_trailing_space",
        Container::Tar,
        tar_single(b"trailing ", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path ending in a space, which Windows cannot store",
        rejected("destination.unrepresentable", "trailing "),
    ));

    entries.push(entry(
        "tar_windows_reserved_colon",
        Container::Tar,
        tar_single(b"weird:name.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path containing a colon, which Windows cannot store",
        rejected("destination.unrepresentable", "weird:name.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_pax_extended_attribute",
        "a pax extended header carrying an extended attribute",
        &[pax_record("SCHILY.xattr.user.comment", "hello")],
        b"xattr.txt",
        b"x",
        rejected("archive.unsupported", "xattr.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_pax_ownership",
        "a pax extended header carrying ownership",
        &[pax_record("uid", "1234"), pax_record("gid", "5678")],
        b"owned.txt",
        b"x",
        rejected("archive.unsupported", "owned.txt"),
    ));

    entries.push(entry(
        "zip_absolute_path",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"/etc/passwd", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a member path rooted at the filesystem root",
        rejected("archive.unsafe_path", "/etc/passwd"),
    ));

    entries.push(entry(
        "zip_dotdot_traversal",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"../../evil.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a member path climbing out of the destination with parent-directory components",
        rejected("archive.unsafe_path", "../../evil.txt"),
    ));

    entries.push(entry(
        "zip_name_disagreement",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"safe.txt", data);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.name = b"evil.txt".to_vec();
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a local file header whose name disagrees with its central directory entry",
        rejected("archive.unsafe_path", "evil.txt"),
    ));

    entries.push(entry(
        "zip_size_disagreement",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"size_mismatch.txt", data);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.compressed_size = 999;
            central.uncompressed_size = 999;
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a local file header whose size disagrees with its central directory entry",
        rejected("archive.unsupported", "size_mismatch.txt"),
    ));

    entries.push(entry(
        "zip_duplicate_name",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let first_data = b"a";
            let first_offset = writer.offset();
            let first_local = ZipLocalHeader::store(b"dup.txt", first_data);
            let first_central = ZipCentralHeader::from_local(&first_local, first_offset);
            writer.push(ZipMember {
                local: first_local,
                central: first_central,
                data: first_data.to_vec(),
            });
            let second_data = b"b";
            let second_offset = writer.offset();
            let second_local = ZipLocalHeader::store(b"dup.txt", second_data);
            let second_central = ZipCentralHeader::from_local(&second_local, second_offset);
            writer.push(ZipMember {
                local: second_local,
                central: second_central,
                data: second_data.to_vec(),
            });
            writer.finish()
        },
        "the same member name written twice",
        rejected("archive.collision", "dup.txt"),
    ));

    entries.push(entry(
        "zip_symlink_escape_unix_mode",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let target = b"../../etc/passwd";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"escape_link", target);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.external_attrs = 0o120_777u32 << 16;
            writer.push(ZipMember {
                local,
                central,
                data: target.to_vec(),
            });
            writer.finish()
        },
        "a symlink escaping the destination, expressed only through the Unix mode in the external attributes",
        rejected("archive.link_escape", "escape_link"),
    ));

    entries.push(entry(
        "zip_unsupported_method",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let mut local = ZipLocalHeader::store(b"lzma.txt", data);
            local.method = 14;
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "an entry using a compression method that is neither store nor deflate",
        rejected("archive.unsupported", "lzma.txt"),
    ));

    entries.push(entry(
        "zip_backslash_path",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let first_data = b"x";
            let first_offset = writer.offset();
            let first_local = ZipLocalHeader::store(b"a/b.txt", first_data);
            let first_central = ZipCentralHeader::from_local(&first_local, first_offset);
            writer.push(ZipMember {
                local: first_local,
                central: first_central,
                data: first_data.to_vec(),
            });
            let second_data = b"y";
            let second_offset = writer.offset();
            let second_local = ZipLocalHeader::store(b"dir\\evil.txt", second_data);
            let second_central = ZipCentralHeader::from_local(&second_local, second_offset);
            writer.push(ZipMember {
                local: second_local,
                central: second_central,
                data: second_data.to_vec(),
            });
            writer.finish()
        },
        "a member path using a backslash alongside another member that already uses a forward slash, so the backslash cannot be read as a separator",
        rejected("archive.unsafe_path", "dir\\evil.txt"),
    ));

    entries.push(entry(
        "zip_backslash_separated_normalizes",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"dir\\evil.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a zip whose only member holds a backslash and no member anywhere holds a forward slash, which is the Windows PowerShell Compress-Archive shape and is read as backslash-separated",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_two_files_and_dir",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut dir = TarHeader::ustar(b"docs/", TYPEFLAG_DIRECTORY);
            dir.set_mode(0o755);
            writer.push(&dir, b"");
            let mut readme = TarHeader::ustar(b"docs/readme.txt", TYPEFLAG_REGULAR);
            readme.set_size(5);
            writer.push(&readme, b"hello");
            let mut top = TarHeader::ustar(b"top.txt", TYPEFLAG_REGULAR);
            top.set_size(5);
            writer.push(&top, b"world");
            writer.finish()
        },
        "a small, well formed tar of two files and a directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "zip_benign_two_files_and_dir",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let dir_data = b"";
            let dir_offset = writer.offset();
            let dir_local = ZipLocalHeader::store(b"docs/", dir_data);
            let dir_central = ZipCentralHeader::from_local(&dir_local, dir_offset);
            writer.push(ZipMember {
                local: dir_local,
                central: dir_central,
                data: dir_data.to_vec(),
            });
            let readme_data = b"hello";
            let readme_offset = writer.offset();
            let readme_local = ZipLocalHeader::store(b"docs/readme.txt", readme_data);
            let readme_central = ZipCentralHeader::from_local(&readme_local, readme_offset);
            writer.push(ZipMember {
                local: readme_local,
                central: readme_central,
                data: readme_data.to_vec(),
            });
            let top_data = b"world";
            let top_offset = writer.offset();
            let top_local = ZipLocalHeader::store(b"top.txt", top_data);
            let top_central = ZipCentralHeader::from_local(&top_local, top_offset);
            writer.push(ZipMember {
                local: top_local,
                central: top_central,
                data: top_data.to_vec(),
            });
            writer.finish()
        },
        "a small, well formed zip of two files and a directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_zero_byte_file",
        Container::Tar,
        tar_single(b"empty.bin", TYPEFLAG_REGULAR, 0o644, b"", b""),
        "a tar holding a zero-byte file",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_empty_directory",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut dir = TarHeader::ustar(b"emptydir/", TYPEFLAG_DIRECTORY);
            dir.set_mode(0o755);
            writer.push(&dir, b"");
            writer.finish()
        },
        "a tar holding an empty directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_identical_content_twice",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"a.bin", TYPEFLAG_REGULAR);
            first.set_size(13);
            writer.push(&first, b"same content!");
            let mut second = TarHeader::ustar(b"b.bin", TYPEFLAG_REGULAR);
            second.set_size(13);
            writer.push(&second, b"same content!");
            writer.finish()
        },
        "a tar holding two members with identical content",
        Expectation::Benign,
    ));

    entries.push(entry(
        "zip_benign_zip64",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"hi";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"small.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish_zip64()
        },
        "a zip using Zip64 end-of-central-directory records",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_bomb_entry_count",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            for name in [b"a.txt".as_slice(), b"b.txt", b"c.txt", b"d.txt"] {
                let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
                header.set_size(1);
                writer.push(&header, b"x");
            }
            writer.finish()
        },
        "more entries than an overridden entry limit allows, discovered as they are actually written to staging rather than trusted from a declared count",
        rejected("archive.bomb", "tar_bomb_entry_count"),
    ));

    entries.push(entry(
        "tar_bomb_expanded_bytes",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            for name in [b"a.bin".as_slice(), b"b.bin", b"c.bin"] {
                let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
                let data = vec![b'x'; 64];
                header.set_size(64);
                writer.push(&header, &data);
            }
            writer.finish()
        },
        "more expanded bytes than an overridden byte limit allows, counted as the bytes are actually written to staging",
        rejected("archive.bomb", "tar_bomb_expanded_bytes"),
    ));

    entries.push(entry(
        "zip_bomb_expansion_ratio",
        Container::Zip,
        {
            let byte = b'A';
            let count: u32 = 1 + 258 * 2000;
            let compressed = deflate_repeated_byte(byte, count);
            let content = vec![byte; count as usize];
            let crc = crc32(&content);
            let mut writer = ZipWriter::new();
            let offset = writer.offset();
            let local = ZipLocalHeader {
                version_needed: 20,
                flags: 0,
                method: METHOD_DEFLATE,
                mod_time: 0,
                mod_date: 0,
                crc32: crc,
                compressed_size: u32::try_from(compressed.len()).unwrap_or(u32::MAX),
                uncompressed_size: count,
                name: b"bomb.bin".to_vec(),
                extra: Vec::new(),
            };
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: compressed,
            });
            writer.finish()
        },
        "a legitimately deflate-compressed member that expands far past the archive's own on-disk size",
        rejected("archive.bomb", "zip_bomb_expansion_ratio"),
    ));

    entries
}
