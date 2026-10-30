//! the optional ceiling on local worker groups.

use std::ffi::OsString;
use std::num::NonZeroUsize;

use crate::error::{Error, Kind};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Threads(Option<NonZeroUsize>);

impl Threads {
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Threads, Error> {
        let Some(value) = var("FETCHLOOM_THREADS").filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        value
            .to_str()
            .and_then(|text| text.trim().parse::<NonZeroUsize>().ok())
            .filter(|threads| threads.get() <= 256)
            .map(|threads| Self(Some(threads)))
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!(
                        "FETCHLOOM_THREADS is \"{}\", which is not a number from 1 to 256",
                        value.display()
                    ),
                )
                .help("unset FETCHLOOM_THREADS to use the default worker counts, or set it to a number from 1 to 256")
            })
    }

    pub(crate) fn is_limited(self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn limit(self, requested: usize) -> usize {
        self.0.map_or(requested, |cap| requested.min(cap.get()))
    }

    /// shares the archive budget between readers and their writers. zero writers means the
    /// reader stores members itself, so a cap of one never needs a second thread to drain a queue.
    pub(crate) fn archives(self, requested: usize) -> (usize, Option<usize>) {
        let Some(cap) = self.0 else {
            return (requested, None);
        };
        let readers = requested.max(1).min(cap.get());
        (readers, Some(cap.get() / readers - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_values_keep_defaults_and_accept_the_bounds() {
        for value in [None, Some("")] {
            let threads = Threads::read(&|_| value.map(Into::into)).unwrap();
            assert!(!threads.is_limited());
            assert_eq!(threads.limit(512), 512);
            assert_eq!(threads.archives(4), (4, None));
        }
        for (text, cap) in [("1", 1), (" 8 ", 8), ("256", 256)] {
            let threads = Threads::read(&|_| Some(text.into())).unwrap();
            assert!(threads.is_limited());
            assert_eq!(threads.limit(512), cap);
            assert_eq!(threads.limit(1), 1);
        }
    }

    #[test]
    fn invalid_thread_values_are_settings_errors() {
        for text in [
            " ",
            "0",
            "257",
            "-1",
            "1.5",
            "1K",
            "many",
            "999999999999999999999",
        ] {
            let error = Threads::read(&|_| Some(text.into())).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(error.kind.exit_code(), 2);
            assert!(error.message.contains("FETCHLOOM_THREADS"));
            assert!(error.help.is_some());
        }
    }

    #[test]
    fn archives_share_the_cap_between_readers_and_writers() {
        for cap in 1..=256 {
            let threads = Threads::read(&|_| Some(cap.to_string().into())).unwrap();
            for requested in 1..=4 {
                let (readers, writers) = threads.archives(requested);
                assert!(readers >= 1 && readers <= requested);
                assert!(readers * (writers.unwrap() + 1) <= cap);
            }
        }
        let one = Threads::read(&|_| Some("1".into())).unwrap();
        assert_eq!(one.archives(4), (1, Some(0)));
    }
}
