//! one byte budget shared by every response in a run.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;
use url::Url;

use super::Net;
use super::failure::{self, Failure};
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
    /// a cap of `per_second` from the first byte on, which lets through at most a second's
    /// bytes at once after a pause.
    pub(crate) fn new(per_second: u64) -> Rate {
        Rate {
            per_second,
            capacity: per_second,
            bucket: Mutex::new(Bucket {
                credit: 0,
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

    fn poll<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
        future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    }

    /// `rate = "500K"` in the settings file caps a run as `FETCHLOOM_RATE=500K` does: the same
    /// budget, which lets 500 KiB through after a second and not before.
    #[tokio::test(start_paused = true)]
    async fn the_settings_files_rate_caps_as_the_variable_does() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("settings.toml");
        std::fs::write(&path, "rate = \"500K\"\n").unwrap();
        let read = |variable: Option<&str>| {
            let var = |name: &str| match name {
                "FETCHLOOM_SETTINGS" => Some(path.clone().into_os_string()),
                "FETCHLOOM_RATE" => variable.map(Into::into),
                _ => None,
            };
            let file = crate::settings::File::read(&var).unwrap();
            let settings =
                crate::settings::Settings::read(&var, file, crate::machine::Machine::default())
                    .unwrap();
            Rate::new(settings.rate.value.unwrap())
        };
        let stop = Stop::default();
        let (from_file, from_variable) = (read(None), read(Some("500K")));
        let mut capped = std::pin::pin!(async {
            tokio::join!(
                from_file.take(500 << 10, &stop),
                from_variable.take(500 << 10, &stop)
            )
        });
        assert!(poll(capped.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(poll(capped.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(poll(capped.as_mut()), std::task::Poll::Ready((true, true)));
    }

    /// the first bytes wait their share too, so a cap holds from the start rather than after a
    /// free first MiB, and a pause saves up no more than a second's bytes.
    #[tokio::test(start_paused = true)]
    async fn the_cap_holds_from_the_first_byte() {
        let rate = Rate::new(2048);
        let stop = Stop::default();
        let mut first = std::pin::pin!(rate.take(1024, &stop));
        assert!(poll(first.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(499)).await;
        assert!(poll(first.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(poll(first.as_mut()), std::task::Poll::Ready(true));
        tokio::time::advance(Duration::from_mins(10)).await;
        assert!(rate.take(2048, &stop).await);
        let mut after = std::pin::pin!(rate.take(1, &stop));
        assert!(poll(after.as_mut()).is_pending());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_chunks_share_one_rate() {
        let rate = Rate::new(4 * 1024 * 1024);
        let stop = Stop::default();
        tokio::time::advance(Duration::from_secs(1)).await;
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
        tokio::time::advance(Duration::from_secs(1)).await;
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
