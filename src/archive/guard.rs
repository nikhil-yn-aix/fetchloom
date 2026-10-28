//! one guard per archive. it checks every name and counts every byte actually written against the
//! limits, whatever the archive declares.

use std::cell::Cell;
use std::io::{self, Read, Write};

use super::names::{Named, Names};
use super::{Limits, damaged, not_written, refused};
use crate::error::{Error, Kind};
use crate::units;

/// the ratio check waits until this much is written, so a small file of zeros cannot trip it.
const RATIO_FLOOR: u64 = 4 << 20;

/// compressed bytes read from the archive file, counted by the reader under the decompressor.
#[derive(Default)]
pub(super) struct Meter {
    compressed: Cell<u64>,
}

impl Meter {
    pub(super) fn count(&self, read: usize) {
        self.compressed.set(self.compressed.get() + read as u64);
    }
}

pub(super) struct Guard<'a> {
    limits: &'a Limits,
    stopped: &'a dyn Fn() -> bool,
    meter: &'a Meter,
    names: Names,
    entries: u64,
    written: u64,
}

impl<'a> Guard<'a> {
    pub(super) fn new(limits: &'a Limits, stopped: &'a dyn Fn() -> bool, meter: &'a Meter) -> Self {
        Guard {
            limits,
            stopped,
            meter,
            names: Names::default(),
            entries: 0,
            written: 0,
        }
    }

    pub(super) fn meter(&self) -> &'a Meter {
        self.meter
    }

    /// counts one entry of any kind, refusing the archive past the member limit.
    pub(super) fn entry(&mut self) -> Result<(), Error> {
        self.check_stopped()?;
        self.entries += 1;
        if self.entries > self.limits.members {
            return Err(refused(format!(
                "the archive holds more than {}, the most fetchloom unpacks from one archive",
                units::count(self.limits.members, "entry", "entries")
            )));
        }
        Ok(())
    }

    /// checks an entry's name, as [`Names::add`] describes.
    pub(super) fn name(&mut self, raw: &str, is_folder: bool) -> Result<Named, Error> {
        self.names.add(raw, is_folder)
    }

    /// copies one member into `to`, refusing the archive as soon as what was written passes a
    /// limit.
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
                Err(err) => return Err(damaged(Some(path), &err)),
            };
            self.written += read as u64;
            self.check_limits(path)?;
            to.write_all(&buffer[..read])
                .map_err(|err| not_written(path, &err))?;
            self.check_stopped()?;
        }
    }

    pub(super) fn check_stopped(&self) -> Result<(), Error> {
        if (self.stopped)() {
            return Err(Error::new(
                Kind::Interrupted,
                "stopped while unpacking the archive",
            ));
        }
        Ok(())
    }

    fn check_limits(&self, path: &str) -> Result<(), Error> {
        if self.written > self.limits.bytes {
            return Err(refused(format!(
                "\"{path}\" takes the archive past {}, the most fetchloom unpacks from one archive",
                units::size(self.limits.bytes)
            )));
        }
        let compressed = self.meter.compressed.get();
        if self.written >= RATIO_FLOOR
            && u128::from(self.written) > u128::from(compressed) * u128::from(self.limits.ratio)
        {
            return Err(refused(format!(
                "the archive grew past {} times its compressed size at \"{path}\", which is how zip bombs fill disks",
                self.limits.ratio
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guarded(limits: Limits, test: impl FnOnce(&mut Guard<'_>, &Meter)) {
        let meter = Meter::default();
        let never = || false;
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
    fn the_byte_limit_counts_what_is_written_across_members() {
        let limits = Limits {
            bytes: 10,
            ..Limits::default()
        };
        guarded(limits, |guard, _| {
            assert_eq!(copy(guard, b"123456").unwrap(), b"123456");
            let err = copy(guard, b"12345").unwrap_err();
            assert!(err.message.contains("past 10 B"), "{}", err.message);
        });
    }

    #[test]
    fn the_ratio_waits_for_a_few_mib() {
        guarded(Limits::default(), |guard, meter| {
            meter.count(1);
            let small = vec![0; 1 << 20];
            assert!(copy(guard, &small).is_ok(), "1 MiB from 1 byte is allowed");
            let more = vec![0; usize::try_from(RATIO_FLOOR).unwrap()];
            let err = copy(guard, &more).unwrap_err();
            assert!(err.message.contains("200 times"), "{}", err.message);
        });
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
        let stopped = || true;
        let mut guard = Guard::new(&limits, &stopped, &meter);
        let err = copy(&mut guard, b"abc").unwrap_err();
        assert_eq!(err.kind, Kind::Interrupted);
        assert_eq!(guard.entry().unwrap_err().kind, Kind::Interrupted);
    }

    #[test]
    fn a_failed_read_is_a_damaged_archive_and_a_failed_write_is_local() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
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
                .copy("a.csv", &mut Failing, &mut Vec::new(), &mut buffer)
                .unwrap_err();
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert!(err.message.contains("\"a.csv\""), "{}", err.message);
            let err = guard
                .copy("a.csv", &mut &b"abc"[..], &mut Failing, &mut buffer)
                .unwrap_err();
            assert_eq!(err.kind, Kind::LocalIo);
        });
    }
}
