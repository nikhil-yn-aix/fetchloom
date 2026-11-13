//! one byte budget shared by every response in a run.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use super::Net;
use super::failure::{self, Failure};
use crate::error::{Error, Kind};
use crate::stop::Stop;

const NANOS: u128 = 1_000_000_000;

pub(crate) struct Rate {
    per_second: u64,
    capacity: u64,
    bucket: Mutex<Bucket>,
}

struct Bucket {
    // byte-nanoseconds keep fractional tokens without rounding each chunk's refill down
    credit: u128,
    updated: Instant,
}

impl Rate {
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Option<Arc<Rate>>, Error> {
        let Some(value) = var("FETCHLOOM_RATE").filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        value
            .to_str()
            .and_then(crate::units::bytes)
            .map(|rate| Some(Arc::new(Self::new(rate))))
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!(
                        "FETCHLOOM_RATE is \"{}\", which is not a positive byte rate",
                        value.display()
                    ),
                )
                .help(
                    "unset FETCHLOOM_RATE for no cap, or use bytes per second such as 500K or 2.5M",
                )
            })
    }

    /// the bytes a second the run may receive.
    pub(crate) fn per_second(&self) -> u64 {
        self.per_second
    }

    fn new(per_second: u64) -> Rate {
        let capacity = per_second.max(1024 * 1024);
        Rate {
            per_second,
            capacity,
            bucket: Mutex::new(Bucket {
                credit: u128::from(capacity) * NANOS,
                updated: Instant::now(),
            }),
        }
    }

    async fn take(&self, bytes: usize, stop: &Stop) -> bool {
        // keeping the lock across sleeps gives queued streams their turn instead of letting
        // a busy stream spend every refill before a sleeper wakes
        let mut bucket = tokio::select! {
            biased;
            () = stop.asked() => return false,
            bucket = self.bucket.lock() => bucket,
        };
        let capacity = u128::from(self.capacity) * NANOS;
        let mut left = bytes as u64;
        while left > 0 {
            let portion = left.min(self.capacity);
            let needed = u128::from(portion) * NANOS;
            let now = Instant::now();
            let refill = now
                .duration_since(bucket.updated)
                .as_nanos()
                .saturating_mul(u128::from(self.per_second));
            bucket.credit = bucket.credit.saturating_add(refill).min(capacity);
            bucket.updated = now;
            if bucket.credit >= needed {
                bucket.credit -= needed;
                left -= portion;
                continue;
            }
            let nanos = (needed - bucket.credit).div_ceil(u128::from(self.per_second));
            let wait = Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX));
            tokio::select! {
                biased;
                () = stop.asked() => return false,
                () = tokio::time::sleep(wait) => {}
            }
        }
        true
    }
}

impl Net {
    pub(crate) fn with_rate(mut self, rate: Option<Arc<Rate>>) -> Net {
        self.rate = rate;
        self
    }

    pub(super) async fn throttle(&self, url: &Url, bytes: usize) -> Result<(), Failure> {
        if let Some(rate) = &self.rate
            && !rate.take(bytes, &self.stop).await
        {
            return Err(Failure::Final(failure::interrupted(url)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_values_use_binary_units() {
        for (text, expected) in [
            ("1", 1),
            ("10M", 10 * 1024 * 1024),
            ("500K", 500 * 1024),
            ("2.5M", 2_621_440),
            (" 2G ", 2 * 1024 * 1024 * 1024),
            ("1T", 1024_u64.pow(4)),
            ("0.1K", 102),
            ("18446744073709551615", u64::MAX),
        ] {
            assert_eq!(crate::units::bytes(text), Some(expected), "{text}");
        }
        assert!(Rate::read(&|_| None).unwrap().is_none());
        assert!(Rate::read(&|_| Some("".into())).unwrap().is_none());
        assert!(Rate::read(&|_| Some("2.5M".into())).unwrap().is_some());
    }

    #[test]
    fn fractional_precision_does_not_change_the_rounded_rate() {
        let zeros = "0".repeat(128);
        let nines = "9".repeat(128);
        for (text, expected) in [
            (format!("1.{zeros}"), Some(1)),
            (format!("1.{nines}"), Some(1)),
            (format!("0.0009765625{zeros}K"), Some(1)),
            (format!("0.0009765624{nines}K"), None),
            (format!("0.0009765625{zeros}1K"), Some(1)),
            (format!("0.{nines}T"), Some((1 << 40) - 1)),
            (format!("16777215.{nines}T"), Some(u64::MAX)),
            (format!("18446744073709551615.{nines}"), Some(u64::MAX)),
            (format!("16777216.{zeros}T"), None),
            (format!("18446744073709551616.{zeros}"), None),
            (format!("{zeros}1.{zeros}M"), Some(1 << 20)),
        ] {
            assert_eq!(crate::units::bytes(&text), expected, "{text}");
        }
    }

    #[test]
    fn invalid_rate_values_are_settings_errors() {
        for text in [
            " ",
            "0",
            "0M",
            "0.1",
            "-1",
            "+1",
            "NaN",
            "inf",
            "1e3",
            "10MB",
            "10m",
            ".5M",
            "1.M",
            "1..5M",
            "1 M",
            "M",
            "18446744073709551616",
            "18446744073709551615T",
        ] {
            assert_eq!(crate::units::bytes(text), None, "{text}");
            let error = Rate::read(&|_| Some(text.into())).err().unwrap();
            assert_eq!(error.kind, crate::error::Kind::Usage);
            assert_eq!(error.kind.exit_code(), 2);
            assert!(error.message.contains("FETCHLOOM_RATE"));
            assert!(error.help.is_some());
        }
    }

    fn poll<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_chunks_share_one_rate() {
        let rate = Rate::new(4 * 1024 * 1024);
        let stop = Stop::default();
        assert!(rate.take(4 * 1024 * 1024, &stop).await);
        let mut first = std::pin::pin!(rate.take(1024 * 1024, &stop));
        let mut second = std::pin::pin!(rate.take(1024 * 1024, &stop));
        assert!(poll(first.as_mut()).is_pending());
        assert!(poll(second.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(249)).await;
        assert!(poll(first.as_mut()).is_pending());
        assert!(poll(second.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(poll(first.as_mut()), std::task::Poll::Ready(true));
        assert!(poll(second.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(250)).await;
        assert_eq!(poll(second.as_mut()), std::task::Poll::Ready(true));
    }

    #[tokio::test(start_paused = true)]
    async fn a_chunk_larger_than_the_burst_is_paced() {
        let rate = Rate::new(4 * 1024 * 1024);
        let stop = Stop::default();
        let mut chunk = std::pin::pin!(rate.take(5 * 1024 * 1024, &stop));
        assert!(poll(chunk.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(249)).await;
        assert!(poll(chunk.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(poll(chunk.as_mut()), std::task::Poll::Ready(true));
    }

    #[tokio::test(start_paused = true)]
    async fn stopping_interrupts_sleepers_and_queued_chunks() {
        let rate = Rate::new(1);
        let stop = Stop::default();
        assert!(rate.take(1024 * 1024, &stop).await);
        let mut waiting =
            std::pin::pin!(async { tokio::join!(rate.take(100, &stop), rate.take(100, &stop)) });
        assert!(poll(waiting.as_mut()).is_pending());
        stop.ask();
        assert_eq!(
            poll(waiting.as_mut()),
            std::task::Poll::Ready((false, false))
        );
    }
}
