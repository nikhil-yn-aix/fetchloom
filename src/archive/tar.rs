//! tar archives, plain or compressed, read in one pass from start to end.

use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use ::tar::{Archive, Entry, EntryType};
use bzip2::bufread::MultiBzDecoder;
use flate2::bufread::MultiGzDecoder;
use liblzma::bufread::XzDecoder;
use liblzma::stream::{CONCATENATED, Stream};

use super::guard::{Guard, Meter};
use super::names::Named;
use super::{BUFFER, Format, Keep, damaged, refused, refused_member, unreadable};
use crate::error::Error;
use crate::units;

/// the most the tar crate may read on its own for one member: headers, long names and pax
/// records. it holds long names and pax records in memory whole.
const HEADERS_CAP: u64 = 16 << 20;

/// the most memory an xz stream may ask for to be decompressed. `xz -9e` streams ask for 65 MiB,
/// while a header of a few bytes may ask for 4 GiB before any output.
pub(super) const XZ_MEMORY: u64 = 256 << 20;

/// the largest window a zstd frame may ask for: 128 MiB, libzstd's own default. `zstd --long`
/// stays within it, while `--long=28` and above go past it.
pub(super) const ZSTD_WINDOW: u64 = 1 << ZSTD_WINDOW_LOG;

const ZSTD_WINDOW_LOG: u32 = 27;

/// the stream a tar sits in, decompressed. gzip, xz and bzip2 streams may be several joined end
/// to end, as pigz, bgzip and pbzip2 write them.
pub(super) fn decompressed<'r>(
    format: Format,
    compressed: impl BufRead + 'r,
) -> io::Result<Box<dyn Read + 'r>> {
    Ok(match format {
        Format::TarGz => Box::new(MultiGzDecoder::new(compressed)),
        Format::TarXz => Box::new(XzDecoder::new_stream(
            compressed,
            Stream::new_auto_decoder(XZ_MEMORY, CONCATENATED)?,
        )),
        Format::TarZst => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(compressed)?;
            decoder.window_log_max(ZSTD_WINDOW_LOG)?;
            Box::new(decoder)
        }
        Format::TarBz2 => Box::new(MultiBzDecoder::new(compressed)),
        Format::Tar | Format::Zip => Box::new(compressed),
    })
}

/// whether `err` is an xz stream refused for asking more than [`XZ_MEMORY`] to be decompressed.
pub(super) fn needs_too_much_memory(err: &(dyn std::error::Error + 'static)) -> bool {
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .and_then(|inner| inner.downcast_ref::<liblzma::stream::Error>())
        .is_some_and(|inner| *inner == liblzma::stream::Error::MemLimit)
}

/// whether `err` is a zstd frame refused for asking a window larger than [`ZSTD_WINDOW`].
pub(super) fn needs_too_large_window(err: &(dyn std::error::Error + 'static)) -> bool {
    use zstd::zstd_safe::{get_error_name, zstd_sys::ZSTD_ErrorCode};
    // the zstd crate keeps only libzstd's name for the error, so the name is what tells it apart
    let code = ZSTD_ErrorCode::ZSTD_error_frameParameter_windowTooLarge as usize;
    let window_too_large = get_error_name(code.wrapping_neg());
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .is_some_and(|inner| inner.to_string() == window_too_large)
}

/// an archive whose decompressed stream could not be read: refused when its zstd frame asks for
/// too large a window, and damaged otherwise.
pub(super) fn unreadable_stream(
    at: Option<&str>,
    err: &(dyn std::error::Error + 'static),
) -> Error {
    if needs_too_large_window(err) {
        return refused(format!(
            "the archive's zstd stream needs a window larger than {} to unpack",
            units::size(ZSTD_WINDOW)
        ));
    }
    damaged(at, err)
}

pub(super) fn unpack(
    archive: &Path,
    format: Format,
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
) -> Result<(), Error> {
    let file = File::open(archive).map_err(|err| unreadable(archive, &err))?;
    let counted = Counted {
        inner: BufReader::with_capacity(BUFFER, file),
        meter: guard.meter(),
    };
    let decoded = decompressed(format, counted).map_err(|err| unreadable_stream(None, &err))?;
    let headers = Headers::default();
    let mut tar = Archive::new(Capped {
        inner: decoded,
        headers: &headers,
    });
    let entries = tar.entries().map_err(|err| unreadable_stream(None, &err))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| {
            headers
                .refusal()
                .unwrap_or_else(|| unreadable_stream(None, &err))
        })?;
        headers.read.set(0);
        guard.entry()?;
        let raw = String::from_utf8(entry.path_bytes().into_owned()).map_err(|err| {
            refused(format!(
                "the archive holds \"{}\", a name that is not valid UTF-8",
                String::from_utf8_lossy(err.as_bytes())
            ))
        })?;
        let is_folder = match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => false,
            EntryType::Directory => true,
            // pax global headers carry defaults for the whole archive, such as the commit id
            // git archive writes, and name no file
            EntryType::XGlobalHeader => continue,
            other => return Err(refused_member(&raw, &special(other))),
        };
        if is_sparse(&mut entry) {
            return Err(refused_member(&raw, "it is a sparse file"));
        }
        if let Named::File(path) = guard.name(&raw, is_folder)? {
            headers.in_member.set(true);
            let kept = keep(guard, path, &mut entry);
            headers.in_member.set(false);
            kept?;
        }
    }
    // a compressed stream's own check, such as gzip's CRC, comes after the tar's end
    if !matches!(format, Format::Tar) {
        let mut rest = tar.into_inner().inner.take(HEADERS_CAP + 1);
        let trailing =
            io::copy(&mut rest, &mut io::sink()).map_err(|err| unreadable_stream(None, &err))?;
        if trailing > HEADERS_CAP {
            return Err(refused(format!(
                "the archive holds more than {} after its end",
                units::size(HEADERS_CAP)
            )));
        }
    }
    Ok(())
}

fn special(kind: EntryType) -> String {
    match kind {
        EntryType::Symlink => "it is a symlink".to_owned(),
        EntryType::Link => "it is a hardlink".to_owned(),
        EntryType::Char | EntryType::Block => "it is a device".to_owned(),
        EntryType::Fifo => "it is a fifo".to_owned(),
        EntryType::GNUSparse => "it is a sparse file".to_owned(),
        other => format!(
            "it is a special entry of type '{}'",
            char::from(other.as_byte()).escape_default()
        ),
    }
}

/// whether pax records mark the entry as a GNU sparse file. the tar crate would hand over its
/// sparse map as if it were the file's bytes.
fn is_sparse(entry: &mut Entry<'_, impl Read>) -> bool {
    entry
        .pax_extensions()
        .ok()
        .flatten()
        .is_some_and(|mut records| {
            records.any(|record| {
                record.is_ok_and(|record| record.key_bytes().starts_with(b"GNU.sparse."))
            })
        })
}

/// the compressed stream, counting each byte the decompressor takes.
struct Counted<'m, R> {
    inner: R,
    meter: &'m Meter,
}

impl<R: BufRead> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.meter.count(read);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for Counted<'_, R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.meter.count(amount);
        self.inner.consume(amount);
    }
}

/// what the tar crate read on its own since the last member.
#[derive(Default)]
struct Headers {
    in_member: Cell<bool>,
    read: Cell<u64>,
}

impl Headers {
    fn refusal(&self) -> Option<Error> {
        (self.read.get() > HEADERS_CAP).then(|| {
            refused(format!(
                "the archive holds a member whose tar headers take more than {}",
                units::size(HEADERS_CAP)
            ))
        })
    }
}

/// the decompressed stream, failing once the tar crate reads more than [`HEADERS_CAP`] outside
/// a member's bytes.
struct Capped<'h, R> {
    inner: R,
    headers: &'h Headers,
}

impl<R: Read> Read for Capped<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if !self.headers.in_member.get() {
            let total = self.headers.read.get() + read as u64;
            self.headers.read.set(total);
            if total > HEADERS_CAP {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "tar headers too large",
                ));
            }
        }
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use ::tar::{Builder, EntryType, Header};

    use super::super::Format;
    use super::super::samples::{
        Scratch, compress, pax, raw_entry, tar_of, xz_asking_for, zstd_asking_for,
    };
    use crate::digest;
    use crate::error::Kind;

    const FILES: &[(&str, &[u8])] = &[
        ("data/train.csv", b"x,y\n1,2\n"),
        ("data/empty.txt", b""),
        ("README.md", b"# readme\n"),
    ];

    const COMPRESSED: [Format; 4] = [Format::TarGz, Format::TarXz, Format::TarZst, Format::TarBz2];

    fn paths(scratch: &Scratch, format: Format, tar: &[u8]) -> Vec<String> {
        let unpacked = scratch.unpack(format, tar).unwrap();
        unpacked
            .members
            .into_iter()
            .map(|member| member.path)
            .collect()
    }

    fn built(entries: impl FnOnce(&mut Builder<Vec<u8>>)) -> Vec<u8> {
        let mut tar = Builder::new(Vec::new());
        entries(&mut tar);
        tar.into_inner().unwrap()
    }

    #[test]
    fn every_compression_unpacks() {
        let tar = tar_of(FILES);
        for format in [Format::Tar].into_iter().chain(COMPRESSED) {
            let scratch = Scratch::new();
            let unpacked = scratch.unpack(format, &compress(format, &tar)).unwrap();
            let paths: Vec<_> = unpacked.members.iter().map(|m| m.path.as_str()).collect();
            assert_eq!(
                paths,
                ["README.md", "data/empty.txt", "data/train.csv"],
                "{format:?}"
            );
            scratch.assert_stored(&unpacked.members[1], b"");
            scratch.assert_stored(&unpacked.members[2], b"x,y\n1,2\n");
        }
    }

    #[test]
    fn streams_joined_end_to_end_unpack() {
        let tar = tar_of(FILES);
        let (head, tail) = tar.split_at(1024);
        for format in COMPRESSED {
            let mut joined = compress(format, head);
            joined.extend(compress(format, tail));
            let scratch = Scratch::new();
            assert_eq!(paths(&scratch, format, &joined).len(), 3, "{format:?}");
        }
    }

    #[test]
    fn a_damaged_check_after_the_tar_end_is_caught() {
        let mut packed = compress(Format::TarGz, &tar_of(FILES));
        let crc = packed.len() - 8;
        packed[crc] ^= 0xff;
        let err = Scratch::new().refuse(Format::TarGz, &packed);
        assert!(err.message.contains("damaged"), "{err:?}");
    }

    #[test]
    fn an_empty_tar_unpacks_to_nothing() {
        let unpacked = Scratch::new()
            .unpack(Format::TarGz, &compress(Format::TarGz, &tar_of(&[])))
            .unwrap();
        assert_eq!(unpacked.members, []);
        assert_eq!(unpacked.tree, digest::sha256(b""));
    }

    #[test]
    fn long_names_unpack_from_gnu_and_pax_headers() {
        let long = format!("{}/data.csv", "deep".repeat(40));
        let gnu = tar_of(&[(&long, b"1")]);
        let pax = built(|tar| {
            pax(tar, &[("path", long.as_bytes())]);
            raw_entry(tar, b"short", EntryType::Regular, b"1");
        });
        for tar in [gnu, pax] {
            assert_eq!(
                paths(&Scratch::new(), Format::Tar, &tar),
                std::slice::from_ref(&long)
            );
        }
    }

    #[test]
    fn links_devices_and_fifos_are_refused() {
        let kinds = [
            (EntryType::Symlink, "a symlink"),
            (EntryType::Link, "a hardlink"),
            (EntryType::Char, "a device"),
            (EntryType::Block, "a device"),
            (EntryType::Fifo, "a fifo"),
            (EntryType::new(b'V'), "a special entry of type 'V'"),
        ];
        for (kind, reason) in kinds {
            let tar = built(|tar| {
                raw_entry(tar, b"good.txt", EntryType::Regular, b"fine");
                raw_entry(tar, b"special", kind, b"");
            });
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert_eq!(
                err.message,
                format!(
                    "the archive holds \"special\", which fetchloom refuses because it is {reason}"
                )
            );
        }
    }

    #[test]
    fn sparse_files_are_refused() {
        let gnu = built(|tar| {
            let mut header = Header::new_gnu();
            header.set_path("disk.img").unwrap();
            header.set_entry_type(EntryType::GNUSparse);
            header.set_size(0);
            header.as_gnu_mut().unwrap().set_real_size(0);
            header.set_cksum();
            tar.append(&header, &[][..]).unwrap();
        });
        let pax = built(|tar| {
            pax(
                tar,
                &[("GNU.sparse.major", b"1"), ("GNU.sparse.name", b"disk.img")],
            );
            raw_entry(
                tar,
                b"GNUSparseFile.0/disk.img",
                EntryType::Regular,
                b"1\n0\n0\n",
            );
        });
        for tar in [gnu, pax] {
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert!(
                err.message.ends_with("it is a sparse file"),
                "{}",
                err.message
            );
        }
    }

    #[test]
    fn one_unsafe_name_refuses_files_already_written() {
        for name in ["../evil.txt", "/etc/passwd", "a/../../b"] {
            let tar = built(|tar| {
                raw_entry(tar, b"good.txt", EntryType::Regular, b"fine");
                raw_entry(tar, name.as_bytes(), EntryType::Regular, b"evil");
            });
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert!(err.message.contains(name), "{}", err.message);
        }
    }

    #[test]
    fn the_root_entry_and_leading_dot_slashes_are_dropped() {
        let tar = built(|tar| {
            raw_entry(tar, b"./", EntryType::Directory, b"");
            raw_entry(tar, b"./data/", EntryType::Directory, b"");
            raw_entry(tar, b"./data/a.csv", EntryType::Regular, b"1");
            raw_entry(tar, b"old/", EntryType::Regular, b"");
        });
        assert_eq!(paths(&Scratch::new(), Format::Tar, &tar), ["data/a.csv"]);
    }

    #[test]
    fn a_pax_global_header_names_no_file() {
        let tar = built(|tar| {
            let comment = b"52 comment=0123456789abcdef0123456789abcdef01234567\n";
            raw_entry(tar, b"pax_global_header", EntryType::XGlobalHeader, comment);
            raw_entry(tar, b"a.csv", EntryType::Regular, b"1");
        });
        assert_eq!(paths(&Scratch::new(), Format::Tar, &tar), ["a.csv"]);
    }

    #[test]
    fn huge_headers_are_refused_before_they_fill_memory() {
        let tar = built(|tar| {
            pax(tar, &[("comment", &vec![b'a'; 17 << 20])]);
            raw_entry(tar, b"a.csv", EntryType::Regular, b"1");
        });
        let err = Scratch::new().refuse(Format::TarZst, &compress(Format::TarZst, &tar));
        assert!(err.message.contains("tar headers"), "{}", err.message);
    }

    #[test]
    fn a_name_that_is_not_utf8_is_refused() {
        let tar = built(|tar| raw_entry(tar, b"caf\xe9.txt", EntryType::Regular, b"1"));
        let err = Scratch::new().refuse(Format::Tar, &tar);
        assert!(err.message.contains("not valid UTF-8"), "{}", err.message);
    }

    #[test]
    fn a_cut_short_archive_is_damaged() {
        let packed = compress(Format::TarGz, &tar_of(FILES));
        let err = Scratch::new().refuse(Format::TarGz, &packed[..packed.len() / 2]);
        assert!(
            err.message.starts_with("the archive is damaged"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_xz_stream_asking_for_too_much_memory_is_refused() {
        // a 512 MiB dictionary, twice what an xz stream may ask for
        let packed = xz_asking_for(&compress(Format::TarXz, &tar_of(FILES)), 34);
        let err = Scratch::new().refuse(Format::TarXz, &packed);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's xz stream needs more than 256.0 MiB of memory to unpack"
        );
        let within = xz_asking_for(&compress(Format::TarXz, &tar_of(FILES)), 18);
        assert_eq!(paths(&Scratch::new(), Format::TarXz, &within).len(), 3);
    }

    #[test]
    fn a_zstd_frame_asking_for_too_much_memory_is_refused() {
        let packed = zstd_asking_for(31, &tar_of(FILES));
        let err = Scratch::new().refuse(Format::TarZst, &packed);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's zstd stream needs a window larger than 128.0 MiB to unpack"
        );
        let within = zstd_asking_for(20, &tar_of(FILES));
        assert_eq!(paths(&Scratch::new(), Format::TarZst, &within).len(), 3);
    }
}
