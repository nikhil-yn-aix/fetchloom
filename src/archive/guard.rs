//! one guard per archive. it checks every name and counts every byte actually written against the
//! limits, whatever the archive declares.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::Path;

use super::names::{Named, Names};
use super::tar::unreadable_stream;
use super::{Limits, Skipped, not_written, past};
use crate::error::{Error, Kind};
use crate::space::Filesystem;
use crate::units;

/// the ratio check waits until this much is written, so a small file of zeros cannot trip it.
const RATIO_FLOOR: u64 = 4 << 20;

/// compressed bytes read from the archive file, counted by the reader under the decompressor.
#[derive(Default)]
pub(super) struct Meter {
    compressed: Cell<u64>,
    ceiling: Cell<Option<u64>>,
}

impl Meter {
    /// seeks may reread compressed bytes, but cannot make the physical archive larger.
    pub(super) fn cap_at(&self, bytes: u64) {
        self.ceiling.set(Some(bytes));
        self.compressed.set(self.compressed.get().min(bytes));
    }

    pub(super) fn count(&self, read: usize) {
        let total = self.compressed.get().saturating_add(read as u64);
        self.compressed
            .set(total.min(self.ceiling.get().unwrap_or(u64::MAX)));
    }

    /// counts the archive as read to its end once its readers took every file from it, though
    /// bytes after the last file, such as a tar's closing blocks, stay unread.
    pub(super) fn read_to_end(&self) {
        if let Some(ceiling) = self.ceiling.get() {
            self.compressed.set(ceiling);
        }
    }
}

pub(super) struct Guard<'a> {
    limits: &'a Limits,
    watch: &'a dyn Fn(u64) -> bool,
    meter: &'a Meter,
    names: Names,
    entries: u64,
    written: u64,
    /// bytes the archive decompressed to that no member holds, such as the declared contents of
    /// a tar folder entry, which the reader reads only to skip.
    skipped: u64,
    /// the files a system writes by itself that the archive holds, left out.
    system: Skipped,
    /// hardlinks, each by its path and the path of the earlier file whose bytes it holds.
    links: Vec<(String, String)>,
    /// the cache the members are written to, and the most bytes one file holds on its disk.
    disk: Option<(&'a Path, u64)>,
}

impl<'a> Guard<'a> {
    pub(super) fn new(
        limits: &'a Limits,
        watch: &'a dyn Fn(u64) -> bool,
        meter: &'a Meter,
    ) -> Self {
        Guard {
            limits,
            watch,
            meter,
            names: Names::default(),
            entries: 0,
            written: 0,
            skipped: 0,
            system: Skipped::default(),
            links: Vec::new(),
            disk: None,
        }
    }

    pub(super) fn writing_to(mut self, cache: &'a Path, filesystem: Filesystem) -> Self {
        self.disk = filesystem.largest_file().map(|largest| (cache, largest));
        self
    }

    /// as if the cache's disk held no file past `largest` bytes, as FAT32 holds none past 4 GiB.
    #[cfg(test)]
    pub(super) fn holding_at_most(mut self, cache: &'a Path, largest: u64) -> Self {
        self.disk = Some((cache, largest));
        self
    }

    /// refuses the member at `path` before any of it is written when its declared `size` is more
    /// than the disk holds in one file.
    pub(super) fn fits(&self, path: &str, size: u64) -> Result<(), Error> {
        match self.disk {
            Some((cache, largest)) if size > largest => Err(Error::new(
                Kind::LocalIo,
                crate::space::too_large(
                    format_args!("the cache at {}", cache.display()),
                    &format!("\"{path}\" in the archive"),
                    size,
                ),
            )
            .help("set FETCHLOOM_CACHE to a folder on a disk formatted as exFAT, NTFS, APFS or ext4, or keep the archive packed with --no-extract, or extract = false in data.toml")),
            _ => Ok(()),
        }
    }

    pub(super) fn meter(&self) -> &'a Meter {
        self.meter
    }

    pub(super) fn limits(&self) -> &'a Limits {
        self.limits
    }

    /// counts one entry of any kind, refusing the archive past the member limit.
    pub(super) fn entry(&mut self) -> Result<(), Error> {
        self.check_stopped()?;
        self.entries += 1;
        self.check_entries(self.entries)
    }

    pub(super) fn check_entries(&self, entries: u64) -> Result<(), Error> {
        if entries > self.limits.members {
            return Err(past(
                format!(
                    "the archive holds more than {}, the most fetchloom unpacks from one archive",
                    units::count(self.limits.members, "entry", "entries")
                ),
                "FETCHLOOM_ARCHIVE_FILES",
                "archive_files",
            ));
        }
        Ok(())
    }

    /// checks an entry's name, as [`Names::add`] describes.
    pub(super) fn name(&mut self, raw: &str, is_folder: bool) -> Result<Named, Error> {
        self.names.add(raw, is_folder)
    }

    /// the file an earlier entry named `raw`, as [`Names::earlier_file`] finds it.
    pub(super) fn earlier_file(&self, raw: &str) -> Option<String> {
        self.names.earlier_file(raw)
    }

    /// notes the file at `path` as a hardlink to the earlier file at `target`, holding its bytes.
    pub(super) fn link(&mut self, path: String, target: String) {
        self.links.push((path, target));
    }

    /// what [`Guard::link`] noted, in the archive's order.
    pub(super) fn links(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.links)
    }

    /// copies one member into `to`, refusing the archive once what was written passes a limit.
    pub(super) fn copy(
        &mut self,
        path: &str,
        from: &mut dyn Read,
        to: &mut dyn Write,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        loop {
            let read = match from.read(buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(unreadable_stream(Some(path), &err)),
            };
            self.written += read as u64;
            self.check_limits(path)?;
            to.write_all(&buffer[..read])
                .map_err(|err| not_written(path, &err))?;
            self.check_stopped()?;
        }
    }

    /// counts bytes read past without writing them, which count against the expansion ratio as
    /// written bytes do, so an archive cannot make fetchloom decompress endlessly for nothing.
    pub(super) fn skip(&mut self, bytes: u64) -> Result<(), Error> {
        self.skipped = self.skipped.saturating_add(bytes);
        self.check_ratio(None)
    }

    /// reads past the entry at `path`, which no file keeps, counting it as [`Guard::skip`] does.
    pub(super) fn pass(&mut self, path: &str, read: &mut dyn Read) -> Result<(), Error> {
        let mut buffer = [0; 16 << 10];
        loop {
            match read.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(passed) => self.skip(passed as u64)?,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(unreadable_stream(Some(path), &err)),
            }
            self.check_stopped()?;
        }
    }

    /// notes the file at `path`, one a system writes by itself, as left out.
    pub(super) fn leave_out(&mut self, path: String) {
        self.system.files += 1;
        self.system.first.get_or_insert(path);
    }

    pub(super) fn left_out(&mut self) -> Skipped {
        std::mem::take(&mut self.system)
    }

    pub(super) fn check_stopped(&self) -> Result<(), Error> {
        if (self.watch)(self.meter.compressed.get()) {
            return Err(Error::new(
                Kind::Interrupted,
                "stopped while unpacking the archive",
            ));
        }
        Ok(())
    }

    fn check_limits(&self, path: &str) -> Result<(), Error> {
        if self.written > self.limits.bytes {
            return Err(past(
                format!(
                    "\"{path}\" takes the archive past {}, the most fetchloom unpacks from one archive",
                    units::size(self.limits.bytes)
                ),
                "FETCHLOOM_ARCHIVE_SIZE",
                "archive_size",
            ));
        }
        self.check_ratio(Some(path))
    }

    fn check_ratio(&self, at: Option<&str>) -> Result<(), Error> {
        let expanded = self.written.saturating_add(self.skipped);
        let compressed = self.meter.compressed.get();
        if expanded >= RATIO_FLOOR
            && u128::from(expanded) > u128::from(compressed) * u128::from(self.limits.ratio)
        {
            let at = at.map(|path| format!(" at \"{path}\"")).unwrap_or_default();
            return Err(past(
                format!(
                    "the archive grew past {} times its compressed size{at}, which is how zip bombs fill disks",
                    self.limits.ratio
                ),
                "FETCHLOOM_ARCHIVE_RATIO",
                "archive_ratio",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guarded(limits: Limits, test: impl FnOnce(&mut Guard<'_>, &Meter)) {
        let meter = Meter::default();
        let never = |_: u64| false;
        let mut guard = Guard::new(&limits, &never, &meter);
        test(&mut guard, &meter);
    }

    fn copy(guard: &mut Guard<'_>, bytes: &[u8]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        let mut buffer = [0; 1024];
        guard.copy("x", &mut &bytes[..], &mut out, &mut buffer)?;
        Ok(out)
    }

    #[test]
    fn the_byte_limit_allows_the_exact_total_across_members() {
        let limits = Limits {
            bytes: 10,
            ..Limits::default()
        };
        guarded(limits, |guard, _| {
            assert_eq!(copy(guard, b"123456").unwrap(), b"123456");
            assert_eq!(copy(guard, b"7890").unwrap(), b"7890");
            let err = copy(guard, b"x").unwrap_err();
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert!(err.message.contains("past 10 B"), "{}", err.message);
        });
    }

    #[test]
    fn the_ratio_limit_allows_the_exact_ratio_then_refuses_more() {
        let limits = Limits {
            ratio: 2,
            ..Limits::default()
        };
        guarded(limits, |guard, meter| {
            meter.count(2 << 20);
            let bytes = vec![0; 4 << 20];
            assert_eq!(copy(guard, &bytes).unwrap(), bytes);
            assert_eq!(copy(guard, b"x").unwrap_err().kind, Kind::SourceInvalid);
        });
    }

    #[test]
    fn an_interrupted_read_resumes_without_losing_bytes() {
        /// hands out two bytes a read, whatever the buffer, and is interrupted once after the
        /// first two.
        struct Interrupted {
            inner: io::Cursor<&'static [u8]>,
            interrupted: bool,
        }
        impl Read for Interrupted {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.inner.position() == 2 && !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let end = buffer.len().min(2);
                self.inner.read(&mut buffer[..end])
            }
        }
        guarded(Limits::default(), |guard, meter| {
            meter.count(4);
            let mut reader = Interrupted {
                inner: io::Cursor::new(b"good"),
                interrupted: false,
            };
            let mut bytes = Vec::new();
            guard
                .copy("x", &mut reader, &mut bytes, &mut [0; 2])
                .unwrap();
            assert!(reader.interrupted);
            assert_eq!(bytes, b"good");
            let mut reader = Interrupted {
                inner: io::Cursor::new(b"skip"),
                interrupted: false,
            };
            guard.pass("y", &mut reader).unwrap();
            assert!(reader.interrupted);
            assert_eq!(guard.skipped, 4);
        });
    }

    #[test]
    fn a_seekable_meter_never_exceeds_its_file() {
        let meter = Meter::default();
        meter.count(5);
        meter.cap_at(3);
        assert_eq!(meter.compressed.get(), 3);
        meter.count(usize::MAX);
        assert_eq!(meter.compressed.get(), 3);
    }

    #[test]
    fn the_member_limit_counts_every_entry() {
        let limits = Limits {
            members: 2,
            ..Limits::default()
        };
        guarded(limits, |guard, _| {
            guard.entry().unwrap();
            guard.entry().unwrap();
            let err = guard.entry().unwrap_err();
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert!(
                err.message.contains("more than 2 entries"),
                "{}",
                err.message
            );
        });
    }

    #[test]
    fn the_ratio_waits_for_a_few_mib() {
        let floor = 4_u64 << 20;
        for (expanded, compressed, allowed) in [
            (floor - 1, 1, true),
            (floor, 1, false),
            (floor + 1, 1, false),
            (floor * 1000, floor, true),
        ] {
            guarded(Limits::default(), |guard, meter| {
                meter.count(usize::try_from(compressed).unwrap());
                assert_eq!(copy(guard, b"x").unwrap(), b"x");
                let result = guard.skip(expanded - 1);
                assert_eq!(
                    result.is_ok(),
                    allowed,
                    "{expanded} expanded bytes from {compressed} compressed bytes"
                );
                if !allowed {
                    let err = result.unwrap_err();
                    assert_eq!(err.kind, Kind::SourceInvalid);
                    assert!(err.message.contains("1000 times"), "{}", err.message);
                }
            });
        }
    }

    #[test]
    fn the_ratio_allows_what_compression_can_honestly_reach() {
        guarded(Limits::default(), |guard, meter| {
            meter.count(1 << 20);
            assert!(copy(guard, &vec![0; 100 << 20]).is_ok(), "100 to 1");
        });
    }

    #[test]
    fn a_stop_ends_the_copy() {
        let meter = Meter::default();
        let limits = Limits::default();
        let stopped = |_: u64| true;
        let mut guard = Guard::new(&limits, &stopped, &meter);
        let err = copy(&mut guard, b"abc").unwrap_err();
        assert_eq!(err.kind, Kind::Interrupted);
        assert_eq!(guard.entry().unwrap_err().kind, Kind::Interrupted);
    }

    #[test]
    fn a_failed_read_is_a_damaged_archive_and_a_failed_write_is_local() {
        /// fails its first read as a damaged archive does, then ends, so a retried failure
        /// finishes rather than hanging the test.
        #[derive(Default)]
        struct Failing {
            failed: bool,
        }
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                if std::mem::replace(&mut self.failed, true) {
                    return Ok(0);
                }
                Err(io::Error::new(io::ErrorKind::InvalidData, "bad crc"))
            }
        }
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        guarded(Limits::default(), |guard, _| {
            let mut buffer = [0; 16];
            let err = guard
                .copy(
                    "a.csv",
                    &mut Failing::default(),
                    &mut Vec::new(),
                    &mut buffer,
                )
                .unwrap_err();
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert!(err.message.contains("\"a.csv\""), "{}", err.message);
            let err = guard.pass("b.csv", &mut Failing::default()).unwrap_err();
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert!(err.message.contains("\"b.csv\""), "{}", err.message);
            let err = guard
                .copy(
                    "a.csv",
                    &mut &b"abc"[..],
                    &mut Failing::default(),
                    &mut buffer,
                )
                .unwrap_err();
            assert_eq!(err.kind, Kind::LocalIo);
        });
    }
}

#[cfg(test)]
mod errors_messages_read_proof {
    use super::*;

    #[test]
    fn errors_messages_local_read_failure_keeps_its_kind() {
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(5))
            }
        }
        let meter = Meter::default();
        let limits = Limits::default();
        let mut guard = Guard::new(&limits, &|_| false, &meter);
        let error = guard
            .copy("a.csv", &mut Failed, &mut Vec::new(), &mut [0; 64])
            .unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
        assert!(error.message.contains("a.csv"), "{error:?}");
    }
}
