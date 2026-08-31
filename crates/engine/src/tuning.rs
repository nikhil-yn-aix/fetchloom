//! What a run measures, what it decides from it, and what bounds both.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::limits::{Bandwidth, Limits};
use crate::threads::ThreadBudget;
use crate::timestamp::Timestamp;

/// The most transfers a run holds in flight across every host, whatever the
/// machine reports, because past this the path rather than the machine decides
/// how fast bytes arrive.
pub const TRANSFERS_CEILING: u32 = 8;

/// The transfers a run holds in flight for a host it has measured nothing
/// about: one to move bytes and one to learn whether a second helps.
pub const FIRST_PER_HOST: u32 = 2;

/// What a run learned about one host, kept so the next run starts where this
/// one finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostMeasurement {
    /// The transfers in flight the run settled on for this host.
    pub concurrency: u32,
    /// The fastest sustained rate a transfer from this host reached, in bytes
    /// per second.
    pub throughput: u64,
    /// How long the host took to answer with its headers, in milliseconds.
    pub time_to_first_byte_ms: u64,
    /// When the measurement was taken.
    pub observed_at: Timestamp,
}

impl HostMeasurement {
    /// Describes this measurement the way `explain` reports it.
    #[must_use]
    pub fn describe(&self, host: &str) -> String {
        format!(
            "{host} {} in flight at {} bytes per second, {} ms to first byte, taken {}",
            self.concurrency, self.throughput, self.time_to_first_byte_ms, self.observed_at
        )
    }
}

/// The two bounds no measurement may push a run past.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ceilings {
    /// The most transfers in flight across every host.
    pub global: NonZeroU32,
    /// The most transfers in flight for one host.
    pub per_host: NonZeroU32,
}

impl Ceilings {
    /// Resolves both ceilings from what the machine detected, what politeness
    /// allows, and what the user asked for.
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

/// Why a run's in-flight count moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// A transfer completed with no retry and no rate limit.
    Clean,
    /// The source asked to be left alone.
    RateLimited,
    /// The transfer failed in a way another attempt may get past.
    Faltered,
}

/// How many transfers a run holds in flight for one host, and how that number
/// moves as the host answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller {
    permitted: u32,
    ceiling: u32,
    fixed: bool,
}

impl Controller {
    /// Starts a controller at what was recorded for the host, or at the first
    /// count when nothing was.
    #[must_use]
    pub fn start(recorded: Option<u32>, ceiling: NonZeroU32) -> Self {
        let ceiling = ceiling.get();
        let permitted = recorded.unwrap_or(FIRST_PER_HOST).clamp(1, ceiling);
        Self {
            permitted,
            ceiling,
            fixed: false,
        }
    }

    /// Starts a controller that never moves, which is what a run bound to a
    /// stated ceiling or to deterministic work uses.
    #[must_use]
    pub fn fixed(at: NonZeroU32) -> Self {
        Self {
            permitted: at.get(),
            ceiling: at.get(),
            fixed: true,
        }
    }

    /// Returns how many transfers may be in flight now.
    #[must_use]
    pub fn permitted(&self) -> u32 {
        self.permitted
    }

    /// Moves the count for what a host answered.
    pub fn answered(&mut self, answer: Answer) {
        if self.fixed {
            return;
        }
        self.permitted = match answer {
            Answer::Clean => self.permitted.saturating_add(1).min(self.ceiling),
            Answer::RateLimited => (self.permitted / 2).max(1),
            Answer::Faltered => self.permitted.saturating_sub(1).max(1),
        };
    }
}

/// Returns how long a transfer must wait so that the bytes it has moved do not
/// exceed a rate.
#[must_use]
pub fn debt(rate: u64, moved: u64, elapsed: Duration) -> Duration {
    if rate == 0 {
        return Duration::ZERO;
    }
    let whole = Duration::from_secs(moved / rate);
    let part = Duration::from_nanos((moved % rate).saturating_mul(1_000_000_000) / rate);
    (whole + part).saturating_sub(elapsed)
}

/// The aggregate ceiling on how fast a run transfers.
#[derive(Debug)]
pub struct Meter {
    rate: u64,
    start: Instant,
    moved: AtomicU64,
}

impl Meter {
    /// Starts a meter at a rate in bytes per second.
    #[must_use]
    pub fn new(rate: Bandwidth) -> Self {
        Self {
            rate: rate.bytes_per_second(),
            start: Instant::now(),
            moved: AtomicU64::new(0),
        }
    }

    /// Counts bytes and returns how long to wait before moving more.
    pub fn moved(&self, bytes: u64) -> Duration {
        let total = self.moved.fetch_add(bytes, Ordering::Relaxed) + bytes;
        debt(self.rate, total, self.start.elapsed())
    }
}
