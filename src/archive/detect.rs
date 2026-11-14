//! what kind of file a download is, read from its first bytes.

use std::io::{self, BufReader, Read, Seek};

use super::input::{Input, Joined};
use super::{tar, unreadable};
use crate::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Detected {
    Archive(Format),
    /// an archive fetchloom does not unpack: `rar`.
    Unsupported(&'static str),
    /// any other file, including a single compressed file such as `reads.fastq.gz`, which stays
    /// as it is.
    Plain,
}

const BLOCK: usize = 512;

/// tells an archive from any other file by its first bytes, `local.io` when they cannot be read.
/// the cache names files by hash, so documents that are zips, as `.xlsx`, are kept whole earlier.
/// `memory` bounds the decoder that looks for a tar inside a compressed stream.
pub(crate) fn detect(input: &Input, memory: u64) -> Result<Detected, Error> {
    let mut file = Joined::open(input).map_err(|(path, err)| unreadable(path, &err))?;
    let unread = |err: io::Error| unreadable(input.first(), &err);
    let mut block = [0; BLOCK];
    let len = read_up_to(&mut file, &mut block).map_err(unread)?;
    let head = &block[..len];
    if let Some(detected) = signed(head) {
        return Ok(detected);
    }
    if let Some(format) = compression(head) {
        file.rewind().map_err(unread)?;
        return Ok(if holds_tar(format, file, memory) {
            Detected::Archive(format)
        } else {
            Detected::Plain
        });
    }
    Ok(if is_tar_header(head) {
        Detected::Archive(Format::Tar)
    } else {
        Detected::Plain
    })
}

/// the tar format a compressed stream would hold, judged by its magic number.
fn compression(head: &[u8]) -> Option<Format> {
    // zstd streams may open with a skippable frame, as pzstd writes them
    let skippable = head.len() >= 4 && head[0] & 0xf0 == 0x50 && head[1..4] == *b"\x2a\x4d\x18";
    if head.starts_with(b"\x1f\x8b") {
        Some(Format::TarGz)
    } else if head.starts_with(b"\xfd7zXZ\x00") {
        Some(Format::TarXz)
    } else if head.starts_with(b"\x28\xb5\x2f\xfd") || skippable {
        Some(Format::TarZst)
    } else if head.len() >= 4 && head.starts_with(b"BZh") && (b'1'..=b'9').contains(&head[3]) {
        Some(Format::TarBz2)
    } else {
        None
    }
}

/// what the first bytes of `input` alone tell: a zip, a 7z or a rar, an archive fetchloom does not
/// unpack. none for any other file, which only [`detect`] tells apart, by looking further.
pub(crate) fn signature(input: &Input) -> Result<Option<Detected>, Error> {
    let mut file = Joined::open(input).map_err(|(path, err)| unreadable(path, &err))?;
    let mut block = [0; BLOCK];
    let len = read_up_to(&mut file, &mut block).map_err(|err| unreadable(input.first(), &err))?;
    Ok(signed(&block[..len]))
}

/// the archive the first bytes `head` name by their signature alone.
fn signed(head: &[u8]) -> Option<Detected> {
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Some(Detected::Archive(Format::Zip));
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Some(Detected::Archive(Format::SevenZ));
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return Some(Detected::Unsupported("rar"));
    }
    None
}

/// whether the stream decompresses to a tar header. one that fails is no archive, unless xz or
/// zstd asks for too much memory: unpacking then refuses it and says why, never leaving it packed.
fn holds_tar(format: Format, file: Joined, memory: u64) -> bool {
    let Ok(mut decoded) = tar::decompressed(format, BufReader::new(file), memory) else {
        return false;
    };
    let mut block = [0; BLOCK];
    match read_up_to(&mut decoded, &mut block) {
        Ok(len) => is_tar_header(&block[..len]),
        Err(err) => tar::needs_too_much_memory(&err) || tar::needs_too_large_window(&err),
    }
}

/// a tar header carries the ustar magic or, in the oldest tars, only a checksum that adds up.
fn is_tar_header(block: &[u8]) -> bool {
    // an empty tar is only zeros, as is many a data file, which must never unpack to nothing
    if block.len() < BLOCK || block.iter().all(|byte| *byte == 0) {
        return false;
    }
    if &block[257..262] == b"ustar" {
        return true;
    }
    let sum: u32 = block
        .iter()
        .enumerate()
        .map(|(at, byte)| {
            if (148..156).contains(&at) {
                u32::from(b' ')
            } else {
                u32::from(*byte)
            }
        })
        .sum();
    let field = String::from_utf8_lossy(&block[148..156]);
    let digits = field.trim_matches([' ', '\0']);
    u32::from_str_radix(digits, 8).is_ok_and(|stated| stated == sum)
}

/// fills as much of `buffer` as the reader has, and says how much that was.
fn read_up_to(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::samples::{
        Scratch, compress, tar_of, xz_asking_for, zip_of, zstd_asking_for,
    };

    fn detected(name: &str, bytes: &[u8]) -> Detected {
        let scratch = Scratch::new();
        detect(
            &Input::File(scratch.file(name, bytes)),
            crate::archive::Limits::default().memory,
        )
        .unwrap()
    }

    #[test]
    fn a_zip_is_an_archive_whatever_its_name() {
        let zip = zip_of(&[("a.csv", b"1,2\n")]);
        assert_eq!(detected("data.zip", &zip), Detected::Archive(Format::Zip));
        assert_eq!(detected("download", &zip), Detected::Archive(Format::Zip));
        let empty = zip_of(&[]);
        assert!(empty.starts_with(b"PK\x05\x06"));
        assert_eq!(
            detected("empty.zip", &empty),
            Detected::Archive(Format::Zip)
        );
    }

    #[test]
    fn every_compressed_tar_is_an_archive() {
        let tar = tar_of(&[("a.csv", b"1,2\n")]);
        for format in [Format::TarGz, Format::TarXz, Format::TarZst, Format::TarBz2] {
            let packed = compress(format, &tar);
            assert_eq!(
                detected("data", &packed),
                Detected::Archive(format),
                "{format:?}"
            );
        }
        assert_eq!(detected("data.tar", &tar), Detected::Archive(Format::Tar));
    }

    #[test]
    fn one_compressed_file_stays_as_it_is() {
        let csv = b"id,value\n1,2\n".repeat(100);
        for format in [Format::TarGz, Format::TarXz, Format::TarZst, Format::TarBz2] {
            assert_eq!(
                detected("data.csv.gz", &compress(format, &csv)),
                Detected::Plain,
                "{format:?}"
            );
        }
        let fastq = b"@read1\nACGT\n+\nIIII\n".repeat(200);
        assert_eq!(
            detected("reads.fastq.gz", &compress(Format::TarGz, &fastq)),
            Detected::Plain
        );
    }

    #[test]
    fn an_xz_stream_too_costly_to_read_is_left_for_unpacking_to_refuse() {
        let tar = tar_of(&[("a.csv", b"1,2\n")]);
        let packed = xz_asking_for(&compress(Format::TarXz, &tar), 34);
        assert_eq!(detected("data", &packed), Detected::Archive(Format::TarXz));
    }

    #[test]
    fn a_zstd_frame_too_costly_to_read_is_left_for_unpacking_to_refuse() {
        let tar = tar_of(&[("a.csv", b"1,2\n")]);
        for log in [28, 31] {
            assert_eq!(
                detected("data", &zstd_asking_for(log, &tar)),
                Detected::Archive(Format::TarZst),
                "a window of 2^{log} bytes"
            );
        }
    }

    #[test]
    fn the_oldest_tars_are_known_by_their_checksum() {
        let mut header = ::tar::Header::new_old();
        header.set_path("old.txt").unwrap();
        header.set_size(0);
        header.set_cksum();
        let mut block = header.as_bytes().to_vec();
        assert_ne!(&block[257..262], b"ustar");
        assert!(is_tar_header(&block));
        block[0] ^= 1;
        assert!(!is_tar_header(&block), "a wrong checksum is no header");
    }

    #[test]
    fn a_7z_is_an_archive_and_a_rar_is_named() {
        assert_eq!(
            detected("a.7z", b"7z\xbc\xaf\x27\x1c\x00\x04rest"),
            Detected::Archive(Format::SevenZ)
        );
        assert_eq!(
            detected("a.rar", b"Rar!\x1a\x07\x00rest"),
            Detected::Unsupported("rar")
        );
        assert_eq!(
            detected("a.rar", b"Rar!\x1a\x07\x01\x00rest"),
            Detected::Unsupported("rar")
        );
    }

    #[test]
    fn anything_else_is_plain() {
        assert_eq!(detected("notes.txt", b"just text"), Detected::Plain);
        assert_eq!(detected("empty", b""), Detected::Plain);
        assert_eq!(detected("zeros.bin", &[0; 2048]), Detected::Plain);
        assert_eq!(
            detected("fake.tar.gz", b"\x1f\x8bnot really gzip"),
            Detected::Plain
        );
        assert_eq!(detected("fake.zip", b"PK but not a zip"), Detected::Plain);
    }
}
