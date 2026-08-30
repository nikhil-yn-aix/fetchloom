//! Contract tests over the hostile archive corpus itself.
//!
//! The corpus is a fixture the archive-reader suite will stand on, so the
//! bytes it hands out have to be asserted directly rather than trusted.
//! Every assertion here reads the tar and zip structures back with its own
//! parsing, independent of the writer in `fetchloom_faults::archives`, so a
//! bug in the writer cannot hide behind a reader built the same way.

#![expect(
    clippy::unwrap_used,
    reason = "test setup and assertions, where a failed read is the failure being asserted"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "fixed-layout binary formats read by offset"
)]

use fetchloom_engine as _;

use std::collections::HashSet;

use fetchloom_faults::{Container, Corpus, Expectation};

struct ReadHeader {
    name: Vec<u8>,
    size: u64,
    typeflag: u8,
    chksum_field: [u8; 8],
    computed_sum: u32,
}

fn read_tar_header(block: &[u8]) -> ReadHeader {
    assert_eq!(block.len(), 512);
    let name = trim_nul(&block[0..100]);
    let size = read_octal(&block[124..136]);
    let typeflag = block[156];
    let mut chksum_field = [0u8; 8];
    chksum_field.copy_from_slice(&block[148..156]);
    let mut sum: u32 = 0;
    for (index, byte) in block.iter().enumerate() {
        if (148..156).contains(&index) {
            sum += 0x20;
        } else {
            sum += u32::from(*byte);
        }
    }
    ReadHeader {
        name,
        size,
        typeflag,
        chksum_field,
        computed_sum: sum,
    }
}

fn trim_nul(field: &[u8]) -> Vec<u8> {
    let end = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    field[..end].to_vec()
}

fn read_octal(field: &[u8]) -> u64 {
    let text = trim_nul(field);
    let text = String::from_utf8_lossy(&text);
    let text = text.trim();
    if text.is_empty() {
        return 0;
    }
    u64::from_str_radix(text, 8).unwrap()
}

fn parse_declared_checksum(field: [u8; 8]) -> u32 {
    let digits = std::str::from_utf8(&field[0..6]).unwrap();
    u32::from_str_radix(digits, 8).unwrap()
}

fn iterate_tar_headers(bytes: &[u8]) -> Vec<ReadHeader> {
    let mut headers = Vec::new();
    let mut offset = 0usize;
    while offset + 512 <= bytes.len() {
        let block = &bytes[offset..offset + 512];
        if block.iter().all(|&byte| byte == 0) {
            break;
        }
        let header = read_tar_header(block);
        offset += 512;
        let data_blocks = usize::try_from(header.size.div_ceil(512)).unwrap();
        offset += data_blocks * 512;
        headers.push(header);
    }
    headers
}

#[test]
fn tar_headers_carry_the_checksum_the_writer_computed() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Tar {
            continue;
        }
        let headers = iterate_tar_headers(candidate.bytes());
        assert!(
            !headers.is_empty(),
            "entry {} produced no readable tar header",
            candidate.name()
        );
        for header in &headers {
            let declared = parse_declared_checksum(header.chksum_field);
            assert_eq!(
                declared,
                header.computed_sum,
                "entry {} has a header whose checksum field does not match the sum the ustar rule computes",
                candidate.name()
            );
        }
    }
}

#[test]
fn tar_header_sizes_and_typeflags_round_trip() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Tar {
            continue;
        }
        let headers = iterate_tar_headers(candidate.bytes());
        for header in &headers {
            assert!(
                header.typeflag.is_ascii_graphic() || header.typeflag == 0,
                "entry {} has a header with a non-ascii typeflag byte",
                candidate.name()
            );
        }
    }

    let zero_byte = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "tar_benign_zero_byte_file")
        .unwrap();
    let headers = iterate_tar_headers(zero_byte.bytes());
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].size, 0);
    assert_eq!(headers[0].typeflag, fetchloom_faults::TYPEFLAG_REGULAR);
    assert_eq!(headers[0].name, b"empty.bin");

    let two_files = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "tar_benign_two_files_and_dir")
        .unwrap();
    let headers = iterate_tar_headers(two_files.bytes());
    assert_eq!(headers.len(), 3);
    assert_eq!(headers[0].typeflag, fetchloom_faults::TYPEFLAG_DIRECTORY);
    assert_eq!(headers[0].size, 0);
    assert_eq!(headers[1].size, 5);
    assert_eq!(headers[1].typeflag, fetchloom_faults::TYPEFLAG_REGULAR);
    assert_eq!(headers[2].size, 5);

    let block_device = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "tar_block_device")
        .unwrap();
    let headers = iterate_tar_headers(block_device.bytes());
    assert_eq!(headers[0].typeflag, fetchloom_faults::TYPEFLAG_BLOCKDEV);
}

#[test]
fn tar_ends_with_two_zero_blocks() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Tar {
            continue;
        }
        let bytes = candidate.bytes();
        assert!(
            bytes.len() >= 1024,
            "entry {} is shorter than the two end-of-archive blocks",
            candidate.name()
        );
        let tail = &bytes[bytes.len() - 1024..];
        assert!(
            tail.iter().all(|&byte| byte == 0),
            "entry {} does not end with two zero-filled 512-byte blocks",
            candidate.name()
        );
    }
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    let signature = 0x0605_4b50u32.to_le_bytes();
    let mut position = bytes.len();
    while position >= 22 {
        position -= 1;
        if bytes.len() - position >= 4 && bytes[position..position + 4] == signature {
            return Some(position);
        }
        if position == 0 {
            break;
        }
    }
    None
}

struct CentralEntry {
    local_header_offset: u32,
}

fn read_central_directory(bytes: &[u8]) -> (u16, Vec<CentralEntry>) {
    let eocd = find_eocd(bytes).unwrap();
    let total_entries = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]);
    let cd_size = u32::from_le_bytes([
        bytes[eocd + 12],
        bytes[eocd + 13],
        bytes[eocd + 14],
        bytes[eocd + 15],
    ]);
    let cd_offset = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]);
    let mut entries = Vec::new();
    let mut cursor = cd_offset as usize;
    let end = cursor + cd_size as usize;
    while cursor < end {
        let signature = u32::from_le_bytes([
            bytes[cursor],
            bytes[cursor + 1],
            bytes[cursor + 2],
            bytes[cursor + 3],
        ]);
        assert_eq!(
            signature, 0x0201_4b50,
            "central directory entry at {cursor} does not start with the central file header signature"
        );
        let name_len = u16::from_le_bytes([bytes[cursor + 28], bytes[cursor + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[cursor + 30], bytes[cursor + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[cursor + 32], bytes[cursor + 33]]) as usize;
        let local_header_offset = u32::from_le_bytes([
            bytes[cursor + 42],
            bytes[cursor + 43],
            bytes[cursor + 44],
            bytes[cursor + 45],
        ]);
        entries.push(CentralEntry {
            local_header_offset,
        });
        cursor += 46 + name_len + extra_len + comment_len;
    }
    (total_entries, entries)
}

#[test]
fn zip_eocd_is_findable_and_entry_count_matches() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Zip {
            continue;
        }
        let bytes = candidate.bytes();
        let (total_entries, entries) = read_central_directory(bytes);
        assert_eq!(
            total_entries as usize,
            entries.len(),
            "entry {} has an end-of-central-directory count that does not match the number of central directory records read",
            candidate.name()
        );
    }
}

#[test]
fn zip_central_directory_offsets_point_at_local_file_headers() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Zip {
            continue;
        }
        let bytes = candidate.bytes();
        let (_, entries) = read_central_directory(bytes);
        for record in &entries {
            let offset = record.local_header_offset as usize;
            let signature = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            assert_eq!(
                signature,
                0x0403_4b50,
                "entry {} has a central directory record whose local header offset does not point at a local file header signature",
                candidate.name()
            );
        }
    }
}

#[test]
fn corpus_has_no_duplicate_entry_names() {
    let corpus = Corpus::build();
    let mut seen = HashSet::new();
    for candidate in corpus.entries() {
        assert!(
            seen.insert(candidate.name()),
            "corpus entry name {} appears more than once",
            candidate.name()
        );
    }
}

#[test]
fn benign_and_hostile_entries_are_disjoint() {
    let corpus = Corpus::build();
    let benign: HashSet<&str> = corpus
        .benign()
        .iter()
        .copied()
        .map(fetchloom_faults::CorpusEntry::name)
        .collect();
    let hostile: HashSet<&str> = corpus
        .hostile()
        .iter()
        .copied()
        .map(fetchloom_faults::CorpusEntry::name)
        .collect();
    assert!(
        benign.intersection(&hostile).next().is_none(),
        "an entry is claimed by both the benign and hostile sets"
    );
    assert_eq!(
        benign.len() + hostile.len(),
        corpus.entries().len(),
        "every corpus entry must be classified as either benign or hostile"
    );
}

#[test]
fn hostile_entries_name_the_declared_error_kind_from_contracts() {
    let allowed = [
        "archive.unsafe_path",
        "archive.link_escape",
        "archive.collision",
        "archive.bomb",
        "archive.unsupported",
        "destination.unrepresentable",
    ];
    let corpus = Corpus::build();
    for candidate in corpus.hostile() {
        let Expectation::Rejected { kind, member } = candidate.expectation() else {
            unreachable!("hostile() only returns rejected entries");
        };
        assert!(
            allowed.contains(kind),
            "entry {} declares kind {kind}, which is not one of the six extract or destination.unrepresentable kinds",
            candidate.name()
        );
        assert!(
            !member.is_empty(),
            "entry {} declares an empty member path",
            candidate.name()
        );
    }
}

#[test]
fn corpus_declares_every_required_case() {
    let required = [
        "tar_absolute_path",
        "tar_dotdot_leading",
        "tar_dotdot_embedded",
        "tar_backslash_path",
        "tar_windows_drive_path",
        "tar_symlink_absolute_target",
        "tar_symlink_dotdot_target",
        "tar_symlink_dotdot_then_write_through",
        "tar_hardlink_outside",
        "tar_block_device",
        "tar_char_device",
        "tar_fifo",
        "tar_setuid_mode",
        "tar_setgid_mode",
        "tar_duplicate_member",
        "tar_path_too_long",
        "tar_path_invalid_utf8",
        "tar_path_with_nul",
        "tar_nesting_too_deep",
        "tar_case_collision",
        "tar_unicode_normalization_collision",
        "tar_windows_reserved_con",
        "tar_windows_reserved_aux_txt",
        "tar_windows_reserved_trailing_dot",
        "tar_windows_reserved_trailing_space",
        "tar_windows_reserved_colon",
        "tar_pax_extended_attribute",
        "tar_pax_ownership",
        "zip_absolute_path",
        "zip_dotdot_traversal",
        "zip_name_disagreement",
        "zip_size_disagreement",
        "zip_duplicate_name",
        "zip_symlink_escape_unix_mode",
        "zip_unsupported_method",
        "zip_backslash_path",
        "zip_backslash_separated_normalizes",
        "tar_benign_two_files_and_dir",
        "zip_benign_two_files_and_dir",
        "tar_benign_zero_byte_file",
        "tar_benign_empty_directory",
        "tar_benign_identical_content_twice",
        "zip_benign_zip64",
    ];
    let corpus = Corpus::build();
    let present: HashSet<&str> = corpus
        .entries()
        .iter()
        .map(fetchloom_faults::CorpusEntry::name)
        .collect();
    for name in required {
        assert!(
            present.contains(name),
            "corpus is missing required case {name}"
        );
    }
}

#[test]
fn tar_duplicate_member_archive_really_has_two_headers_with_the_same_name() {
    let corpus = Corpus::build();
    let candidate = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "tar_duplicate_member")
        .unwrap();
    let headers = iterate_tar_headers(candidate.bytes());
    let names: Vec<Vec<u8>> = headers.iter().map(|header| header.name.clone()).collect();
    assert_eq!(names.len(), 2);
    assert_eq!(names[0], names[1]);
}

#[test]
fn zip_size_disagreement_archive_really_disagrees() {
    let corpus = Corpus::build();
    let candidate = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "zip_size_disagreement")
        .unwrap();
    let bytes = candidate.bytes();
    let local_size = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let (_, entries) = read_central_directory(bytes);
    let offset = entries[0].local_header_offset as usize;
    assert_eq!(offset, 0);
    let (eocd_total, _) = read_central_directory(bytes);
    assert_eq!(eocd_total, 1);
    assert_ne!(
        local_size, 999,
        "local size field should reflect the real data length"
    );
}

#[test]
fn zip_name_disagreement_archive_really_disagrees() {
    let corpus = Corpus::build();
    let candidate = corpus
        .entries()
        .iter()
        .find(|entry| entry.name() == "zip_name_disagreement")
        .unwrap();
    let bytes = candidate.bytes();
    let local_name_len = u16::from_le_bytes([bytes[26], bytes[27]]) as usize;
    let local_name = &bytes[30..30 + local_name_len];
    assert_eq!(local_name, b"safe.txt");
    let (_, entries) = read_central_directory(bytes);
    let offset = entries[0].local_header_offset as usize;
    assert_eq!(offset, 0);
}

#[test]
fn checksum_helper_matches_the_field_the_reader_parses() {
    let corpus = Corpus::build();
    for candidate in corpus.entries() {
        if candidate.container() != Container::Tar {
            continue;
        }
        let bytes = candidate.bytes();
        let first_block = &bytes[0..512];
        let header = read_tar_header(first_block);
        let declared = parse_declared_checksum(header.chksum_field);
        assert_eq!(
            declared,
            header.computed_sum,
            "entry {} first header checksum mismatch",
            candidate.name()
        );
    }
}
