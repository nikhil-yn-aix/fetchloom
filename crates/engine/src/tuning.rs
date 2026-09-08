//! What a run measures, what it decides from it, and what bounds both.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::capability::{Backing, Scanner, VolumeCapabilities};
use crate::degrade::DegradeQueue;
use crate::limits::{Bandwidth, Limits};
use crate::seam::policy::IoMode;
use crate::threads::ThreadBudget;
use crate::timestamp::Timestamp;

pub const TRANSFERS_CEILING: u32 = 8;

pub const FIRST_PER_HOST: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostMeasurement {
    pub concurrency: u32,
    pub throughput: u64,
    pub time_to_first_byte_ms: u64,
    pub observed_at: Timestamp,
}

impl HostMeasurement {
    #[must_use]
    pub fn describe(&self, host: &str) -> String {
        format!(
            "{host} {} in flight at {} bytes per second, {} ms to first byte, taken {}",
            self.concurrency, self.throughput, self.time_to_first_byte_ms, self.observed_at
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceilings {
    pub global: NonZeroU32,
    pub per_host: NonZeroU32,
}

impl Ceilings {
    #[must_use]
    pub fn resolve(
        budget: ThreadBudget,
        limits: &Limits,
        requested_global: Option<NonZeroU32>,
        requested_per_host: Option<NonZeroU32>,
        aggressive: bool,
    ) -> Self {
        let detected = u32::try_from(budget.threads().get()).unwrap_or(u32::MAX);
        let measured_global = detected.clamp(1, TRANSFERS_CEILING);
        let global = requested_global.map_or(measured_global, NonZeroU32::get);
        let polite = u32::try_from(limits.connections_per_host)
            .unwrap_or(u32::MAX)
            .max(1);
        let allowed_per_host = if aggressive {
            global
        } else {
            polite.min(global)
        };
        let per_host = requested_per_host
            .map_or(allowed_per_host, NonZeroU32::get)
            .min(allowed_per_host);
        Self {
            global: NonZeroU32::new(global.max(1)).unwrap_or(NonZeroU32::MIN),
            per_host: NonZeroU32::new(per_host.max(1)).unwrap_or(NonZeroU32::MIN),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Clean,
    RateLimited,
    Faltered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller {
    permitted: u32,
    ceiling: u32,
    fixed: bool,
    capped: u32,
    held_down: bool,
    bytes: u64,
    nanos: u64,
    before: Option<u64>,
}

impl Controller {
    #[must_use]
    pub fn start(recorded: Option<u32>, ceiling: NonZeroU32) -> Self {
        let ceiling = ceiling.get();
        let permitted = recorded.unwrap_or(FIRST_PER_HOST).clamp(1, ceiling);
        Self {
            permitted,
            ceiling,
            fixed: false,
            capped: ceiling,
            held_down: false,
            bytes: 0,
            nanos: 0,
            before: None,
        }
    }

    #[must_use]
    pub fn fixed(at: NonZeroU32) -> Self {
        Self {
            permitted: at.get(),
            ceiling: at.get(),
            fixed: true,
            capped: at.get(),
            held_down: false,
            bytes: 0,
            nanos: 0,
            before: None,
        }
    }

    #[must_use]
    pub fn permitted(&self) -> u32 {
        self.permitted
    }

    pub fn delivered(&mut self, bytes: u64, took: Duration) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.nanos = self
            .nanos
            .saturating_add(u64::try_from(took.as_nanos()).unwrap_or(u64::MAX));
    }

    pub fn answered(&mut self, answer: Answer) {
        if self.fixed {
            return;
        }
        match answer {
            Answer::Clean if self.held_down => self.held_down = false,
            Answer::Clean => self.rise_while_it_helps(),
            Answer::RateLimited => {
                self.held_down = true;
                self.settle_at((self.permitted / 2).max(1));
            }
            Answer::Faltered => {
                self.held_down = true;
                self.settle_at(self.permitted.saturating_sub(1).max(1));
            }
        }
    }

    fn rate(&self) -> Option<u64> {
        (self.bytes >= WINDOW_BYTES && self.nanos > 0)
            .then(|| self.bytes.saturating_mul(1_000_000_000) / self.nanos)
    }

    fn rise_while_it_helps(&mut self) {
        let Some(rate) = self.rate() else {
            self.settle_at(self.next_up());
            return;
        };
        match self.before {
            Some(before) if rate <= before => {
                self.capped = self.permitted.saturating_sub(1).max(1);
                self.settle_at(self.capped);
            }
            _ => {
                self.before = Some(rate);
                self.settle_at(self.next_up());
            }
        }
    }

    fn next_up(&self) -> u32 {
        self.permitted
            .saturating_add(1)
            .min(self.ceiling)
            .min(self.capped)
    }

    fn settle_at(&mut self, count: u32) {
        if count == self.permitted {
            return;
        }
        self.permitted = count;
        self.bytes = 0;
        self.nanos = 0;
    }
}

#[must_use]
pub fn debt(rate: u64, moved: u64, elapsed: Duration) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    let whole = Duration::from_secs(moved / rate);
    let part = Duration::from_nanos((moved % rate).saturating_mul(1_000_000_000) / rate);
    (whole + part).saturating_sub(elapsed)
}

const COLLAPSE_FRACTION: u64 = 4;

pub const SUSTAINED_WINDOWS: usize = 8;

pub const WINDOW_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, Default)]
pub struct WriteRate {
    recent: [u64; SUSTAINED_WINDOWS],
    next: usize,
    seen: bool,
    pending_bytes: u64,
    pending_nanos: u64,
}

impl WriteRate {
    pub fn observed(&mut self, bytes: u64, elapsed: Duration) -> bool {
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
        self.pending_nanos = self
            .pending_nanos
            .saturating_add(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX));
        if self.pending_bytes < WINDOW_BYTES || self.pending_nanos == 0 {
            return false;
        }
        let rate = self.pending_bytes.saturating_mul(1_000_000_000) / self.pending_nanos;
        self.pending_bytes = 0;
        self.pending_nanos = 0;
        let sustained = self.recent.iter().copied().max().unwrap_or(0);
        self.recent[self.next] = rate;
        self.next = (self.next + 1) % SUSTAINED_WINDOWS;
        let judged = self.seen;
        self.seen = true;
        judged && rate < sustained / COLLAPSE_FRACTION
    }
}

#[derive(Debug)]
pub struct Meter {
    rate: u64,
    start: Instant,
    moved: AtomicU64,
}

impl Meter {
    #[must_use]
    pub fn new(rate: Bandwidth) -> Self {
        Self {
            rate: rate.bytes_per_second(),
            start: Instant::now(),
            moved: AtomicU64::new(0),
        }
    }

    pub fn moved(&self, bytes: u64) -> Duration {
        let total = self.moved.fetch_add(bytes, Ordering::Relaxed) + bytes;
        debt(self.rate, total, self.start.elapsed())
    }
}

pub const CAN_RELEASE_PAGES: bool = cfg!(target_os = "linux");

#[must_use]
pub fn resolve_io_mode(
    requested: IoMode,
    capabilities: &VolumeCapabilities,
    can_release_pages: bool,
    degradations: &DegradeQueue,
) -> IoMode {
    match requested {
        IoMode::Buffered => IoMode::Buffered,
        IoMode::Uncached => {
            if can_release_pages {
                IoMode::Uncached
            } else {
                degradations.record(
                    "uncached",
                    "buffered",
                    "the platform offers no way to release written pages without constraining every write to sector alignment",
                );
                IoMode::Buffered
            }
        }
        IoMode::Auto => {
            let local_and_unwatched =
                capabilities.backing == Backing::Local && capabilities.scanner == Scanner::Absent;
            if can_release_pages && local_and_unwatched {
                IoMode::Uncached
            } else {
                IoMode::Buffered
            }
        }
    }
}
