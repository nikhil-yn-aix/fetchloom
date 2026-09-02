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

/// Orders candidate locations by the measured part of source selection:
/// recorded throughput for the host, higher first, then time to first byte,
/// lower first, with manifest order breaking every tie.
///
/// A candidate whose host carries no measurement is never placed ahead of one
/// that does, however low that measurement is: the run learned nothing about
/// it, so it cannot be preferred on the strength of what was measured. With no
/// measurement behind any candidate, the result is the input order unchanged.
#[must_use]
pub fn order_candidates(
    locations: &[String],
    measurement_for: &dyn Fn(&str) -> Option<HostMeasurement>,
) -> Vec<String> {
    let mut indexed: Vec<(usize, &String, Option<HostMeasurement>)> = locations
        .iter()
        .enumerate()
        .map(|(index, location)| (index, location, measurement_for(location)))
        .collect();
    indexed.sort_by(|left, right| candidate_order(left.2, right.2).then(left.0.cmp(&right.0)));
    indexed
        .into_iter()
        .map(|(_, location, _)| location.clone())
        .collect()
}

fn candidate_order(
    left: Option<HostMeasurement>,
    right: Option<HostMeasurement>,
) -> std::cmp::Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right
            .throughput
            .cmp(&left.throughput)
            .then(left.time_to_first_byte_ms.cmp(&right.time_to_first_byte_ms)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
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
    capped: u32,
    held_down: bool,
    bytes: u64,
    nanos: u64,
    before: Option<u64>,
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
            capped: ceiling,
            held_down: false,
            bytes: 0,
            nanos: 0,
            before: None,
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
            capped: at.get(),
            held_down: false,
            bytes: 0,
            nanos: 0,
            before: None,
        }
    }

    /// Returns how many transfers may be in flight now.
    #[must_use]
    pub fn permitted(&self) -> u32 {
        self.permitted
    }

    /// Records what the host delivered at the count it is now permitted, which
    /// is what the next clean answer is judged against.
    pub fn delivered(&mut self, bytes: u64, took: Duration) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.nanos = self
            .nanos
            .saturating_add(u64::try_from(took.as_nanos()).unwrap_or(u64::MAX));
    }

    /// Moves the count for what a host answered.
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

    /// Returns the rate the host delivered at the current count, and nothing
    /// until it has delivered a window at it.
    fn rate(&self) -> Option<u64> {
        (self.bytes >= WINDOW_BYTES && self.nanos > 0)
            .then(|| self.bytes.saturating_mul(1_000_000_000) / self.nanos)
    }

    /// Adds one to the count while the host is delivering more than it did at
    /// the count below, and gives up the count that stopped helping.
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

    /// Moves to a count and starts measuring it, because what was delivered at
    /// one count says nothing about another.
    fn settle_at(&mut self, count: u32) {
        if count == self.permitted {
            return;
        }
        self.permitted = count;
        self.bytes = 0;
        self.nanos = 0;
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

/// How far below what a run had been sustaining the store may accept bytes
/// before the volume counts as having collapsed under it.
pub const COLLAPSE_FRACTION: u64 = 4;

/// How many recent windows the rate a run is judged against is taken over. The
/// first writes into a new file are absorbed by the page cache at a rate no
/// volume sustains, so a peak taken over the whole transfer would read every
/// honest rate after it as a collapse and drive concurrency to one and leave it
/// there.
pub const SUSTAINED_WINDOWS: usize = 8;

/// How many bytes one window gathers before the rate it was accepted at is
/// judged, which is the length of the buffer a transfer writes through.
pub const WINDOW_BYTES: u64 = 1 << 20;

/// The rate at which the store has been accepting bytes, and whether it has
/// just collapsed.
#[derive(Clone, Copy, Debug, Default)]
pub struct WriteRate {
    recent: [u64; SUSTAINED_WINDOWS],
    next: usize,
    seen: bool,
    pending_bytes: u64,
    pending_nanos: u64,
}

impl WriteRate {
    /// Records bytes accepted by the store and reports whether the volume
    /// collapsed under them, judged against the fastest of the recent windows
    /// rather than against the fastest of the whole transfer.
    ///
    /// Bytes are gathered until a window is full before anything is judged. A
    /// body arrives in whatever lengths the socket hands over, and timing one
    /// of those measures the socket as much as the volume, so a window shorter
    /// than the write buffer says nothing about the disk. The first full window
    /// never collapses either, because nothing has been sustained for it to
    /// fall away from.
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

/// Whether this build's target can release a file's written range from the
/// page cache without constraining every write to sector alignment.
pub const CAN_RELEASE_PAGES: bool = cfg!(target_os = "linux");

/// Resolves the write path mode a run actually takes, recording a degrade
/// when an explicit request cannot be honored.
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
