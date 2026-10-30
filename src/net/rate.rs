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
    // byte-nanoseconds keep fractional tokens without rounding each chunk's refill down.
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
            .and_then(parse)
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
        // a busy stream spend every refill before a sleeper wakes.
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

fn parse(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, power) = match text.as_bytes().last()? {
        b'K' => (&text[..text.len() - 1], 1),
        b'M' => (&text[..text.len() - 1], 2),
        b'G' => (&text[..text.len() - 1], 3),
        b'T' => (&text[..text.len() - 1], 4),
        _ => (text, 0),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if !digits(whole) || (number.contains('.') && !digits(fraction)) {
        return None;
    }
    let scale = 10_u128.checked_pow(u32::try_from(fraction.len()).ok()?)?;
    let whole = whole.parse::<u128>().ok()?.checked_mul(scale)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?
    };
    let bytes = whole
        .checked_add(fraction)?
        .checked_mul(1024_u128.pow(power))?
        / scale;
    u64::try_from(bytes).ok().filter(|bytes| *bytes > 0)
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
            assert_eq!(parse(text), Some(expected), "{text}");
        }
        assert!(Rate::read(&|_| None).unwrap().is_none());
        assert!(Rate::read(&|_| Some("".into())).unwrap().is_none());
        assert!(Rate::read(&|_| Some("2.5M".into())).unwrap().is_some());
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
            assert_eq!(parse(text), None, "{text}");
            let error = Rate::read(&|_| Some(text.into())).err().unwrap();
            assert_eq!(error.kind, crate::error::Kind::Usage);
            assert_eq!(error.kind.exit_code(), 2);
            assert!(error.message.contains("FETCHLOOM_RATE"));
            assert!(error.help.is_some());
        }
    }

    #[tokio::test]
    async fn concurrent_chunks_share_one_rate() {
        let rate = Rate::new(4 * 1024 * 1024);
        let stop = Stop::default();
        assert!(rate.take(4 * 1024 * 1024, &stop).await);
        let started = Instant::now();
        let (first, second) =
            tokio::join!(rate.take(1024 * 1024, &stop), rate.take(1024 * 1024, &stop),);
        assert!(first && second);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(450), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[tokio::test]
    async fn a_chunk_larger_than_the_burst_is_paced() {
        let rate = Rate::new(4 * 1024 * 1024);
        let stop = Stop::default();
        let started = Instant::now();
        assert!(rate.take(5 * 1024 * 1024, &stop).await);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(220), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    #[tokio::test]
    async fn stopping_interrupts_sleepers_and_queued_chunks() {
        let rate = Rate::new(1);
        let stop = Stop::default();
        assert!(rate.take(1024 * 1024, &stop).await);
        let waiting = async {
            let (first, second) = tokio::join!(rate.take(100, &stop), rate.take(100, &stop));
            assert!(!first && !second);
        };
        let stopping = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            stop.ask();
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(waiting, stopping);
        })
        .await
        .unwrap();
    }
}
