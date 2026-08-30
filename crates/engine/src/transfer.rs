//! Moving bytes from a source into the store, once, with retry and resume.
//!
//! The bytes are hashed as they are written and never read back to hash. What
//! is on disk decides where a transfer starts, what the source says identifies
//! its bytes decides whether starting there is allowed, and every fall to a
//! lower rung is reported rather than inferred.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crate::degrade::DegradeQueue;
use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::event::{Event, EventPayload, Sequence};
use crate::limits::Limits;
use crate::partial_key::PartialKey;
use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::seam::observer::Observer;
use crate::seam::source::{ByteRange, Source, SourceIdentity, SourceMetadata};
use crate::seam::store::Store;
use crate::source_record::SourceRecord;

/// How many bytes move between the source and the store at a time.
const BUFFER: usize = 1 << 20;

/// What one transfer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transferred {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// How many bytes arrived from the source in this run.
    pub bytes_transferred: u64,
    /// How many bytes were already on disk and kept.
    pub bytes_kept: u64,
    /// The rung the transfer stood on.
    pub rung: ResumeRung,
    /// How many attempts were made.
    pub attempts: u32,
}

/// The two impure things a retry needs: a random fraction and a wait.
///
/// A test supplies one that records instead of sleeping, so the retry ladder is
/// asserted without a test that sleeps.
pub trait Pause: Send + Sync {
    /// Returns a value between zero and one for the jitter.
    fn fraction(&self) -> f64;

    /// Waits for the given duration.
    fn sleep(&self, duration: Duration);
}

/// How long to wait before an attempt, with full jitter.
///
/// Takes the limits, the attempt number counted from one, and a value between
/// zero and one standing for the randomness. Returns a wait no longer than the
/// ceiling.
#[must_use]
pub fn backoff(limits: &Limits, attempt: u32, fraction: f64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let full = Duration::from_secs(1u64 << exponent).min(limits.retry_ceiling);
    full.mul_f64(fraction.clamp(0.0, 1.0))
}

/// Reports whether a wait a source asked for is one this run may honor.
///
/// A wait longer than the ceiling is not waited out, because the ceiling is what
/// the run was told its longest wait is.
#[must_use]
pub fn honors(limits: &Limits, retry_after: Duration) -> bool {
    retry_after <= limits.retry_ceiling
}

/// Decides where a transfer starts, given what is on disk and what the source
/// says now.
///
/// Takes what the partial recorded, what the source reports now, how many bytes
/// are on disk, and whether an outboard tree is known. Returns the rung and how
/// many of those bytes may be kept.
#[must_use]
pub fn rung_for(
    recorded: Option<&SourceRecord>,
    now: &SourceMetadata,
    on_disk: u64,
    outboard_known: bool,
) -> (ResumeRung, u64) {
    if outboard_known {
        return (ResumeRung::Outboard, on_disk);
    }
    if on_disk == 0 {
        return (rung_of(&now.identity), 0);
    }
    let Some(recorded) = recorded else {
        return (ResumeRung::NoValidator, 0);
    };
    if !now.supports_ranges || !recorded.identifies_the_same_bytes_as(&now.identity) {
        return (ResumeRung::NoValidator, 0);
    }
    (rung_of(&now.identity), on_disk)
}

fn rung_of(identity: &SourceIdentity) -> ResumeRung {
    match identity {
        SourceIdentity::ContentAddress(_) | SourceIdentity::ImmutableVersion(_) => {
            ResumeRung::ImmutableIdentity
        }
        SourceIdentity::StrongValidator(_) => ResumeRung::StrongValidator,
        SourceIdentity::WeakValidator(_) => ResumeRung::WeakValidator,
        SourceIdentity::None => ResumeRung::NoValidator,
    }
}

/// Everything one transfer runs against.
pub struct Transfer<'a, S, T, P> {
    /// The store the bytes are written into.
    pub store: &'a T,
    /// The source the bytes are read from.
    pub source: &'a S,
    /// How long to wait and how far to jitter.
    pub pause: &'a P,
    /// The bounds this run may not exceed.
    pub limits: &'a Limits,
    /// Where a fallback is recorded.
    pub degradations: &'a DegradeQueue,
    /// Where the event stream is written.
    pub observer: &'a dyn Observer,
    /// The numbers the events are ordered by.
    pub sequence: &'a Sequence,
}

impl<S: Source, T: Store, P: Pause> Transfer<'_, S, T, P> {
    /// Moves one object from the first source that can serve it into the store.
    ///
    /// Takes the digest the run expects, when it states one, and the locations
    /// in the order the manifest gave them. Hashes the bytes as they are
    /// written. Resumes from what is already on disk when the source still
    /// identifies the same bytes, and starts from zero when it does not. A run
    /// that states no digest names its partial by the source identity a probe
    /// learns, and publishes the object under whatever digest the bytes hash
    /// to.
    ///
    /// # Errors
    ///
    /// Fails when every source is exhausted, when a source's bytes do not hash
    /// to the expected digest, and when the store cannot be written.
    pub fn run(
        &self,
        expected: Option<ContentDigest>,
        locations: &[String],
    ) -> Result<Transferred, Error> {
        let mut last = None;
        for (index, location) in locations.iter().enumerate() {
            match self.attempt_until_spent(expected, location) {
                Ok(done) => return Ok(done),
                Err(error) => {
                    if let Some(next) = locations.get(index + 1) {
                        self.emit(EventPayload::SourceFailover {
                            from: SafeUrl::new(location),
                            to: SafeUrl::new(next),
                            reason: error.next_action().to_owned(),
                        });
                    }
                    last = Some(error);
                }
            }
        }
        Err(last.unwrap_or_else(|| {
            Error::new(
                ErrorKind::ReferenceUnresolved,
                "name at least one source, because the manifest listed none",
            )
        }))
    }

    fn attempt_until_spent(
        &self,
        expected: Option<ContentDigest>,
        location: &str,
    ) -> Result<Transferred, Error> {
        let retry = Retry {
            limits: self.limits,
            pause: self.pause,
            observer: self.observer,
            sequence: self.sequence,
        };
        let mut buffer = vec![0u8; BUFFER];
        retry.until_spent(|attempt| {
            self.attempt_once(expected, location, &mut buffer)
                .map(|mut done| {
                    done.attempts = attempt;
                    done
                })
        })
    }

    fn attempt_once(
        &self,
        expected: Option<ContentDigest>,
        location: &str,
        buffer: &mut [u8],
    ) -> Result<Transferred, Error> {
        if let Some(digest) = expected
            && self.store.contains(digest)?
        {
            return Ok(Transferred {
                digest,
                bytes_transferred: 0,
                bytes_kept: 0,
                rung: ResumeRung::Outboard,
                attempts: 0,
            });
        }

        let metadata = self.source.probe(location, None)?;
        let key = match expected {
            Some(digest) => PartialKey::of_content(digest),
            None => PartialKey::of_source(&metadata),
        };
        let recorded = self.store.recorded_source(key)?;
        let on_disk = recorded.as_ref().map_or(0, |record| record.written);
        let outboard_known = match expected {
            Some(digest) => self.store.has_outboard(digest)?,
            None => false,
        };
        let (rung, keep) = rung_for(recorded.as_ref(), &metadata, on_disk, outboard_known);

        if on_disk > keep {
            self.degradations.record(
                "the bytes already on disk kept",
                "a transfer from zero",
                "the source no longer identifies the bytes the partial recorded, so appending to it would join two different objects",
            );
            self.store.discard_partial(key)?;
        }

        let lease = self.store.lease(key)?;
        self.emit(EventPayload::TransferStart {
            source: metadata.location.clone(),
            expected_bytes: metadata.size,
        });
        if keep > 0 {
            self.emit(EventPayload::TransferResume {
                rung,
                bytes_kept: keep,
            });
        }

        let range = (keep > 0).then(|| ByteRange {
            start: keep,
            end: metadata.size.unwrap_or(u64::MAX),
        });
        let body = self.source.fetch(location, range, None)?;
        self.store
            .record_source(key, &record_of(&metadata, rung, keep))?;

        let length = metadata.size.unwrap_or(0);
        let mut writer = self.store.resume(&lease, length, keep)?;

        let started = Instant::now();
        let mut moved = 0u64;
        let arrived = copy(body, &mut writer, location, &mut moved, buffer);
        self.store
            .record_source(key, &record_of(&metadata, rung, keep + moved))?;
        let short = metadata
            .size
            .is_some_and(|expected| keep + moved < expected);
        let timed_out = arrived
            .as_ref()
            .err()
            .is_some_and(|failure| failure.kind() == ErrorKind::NetworkTimeout);
        if short || arrived.is_err() {
            if short && !timed_out {
                return Err(Error::new(
                    ErrorKind::IntegrityTruncated,
                    format!(
                        "fetch the rest, because the source ended the body after {} of the {} bytes it said the object holds",
                        keep + moved,
                        metadata.size.unwrap_or_default()
                    ),
                )
                .with_source(location)
                .with_retryable(true));
            }
            arrived?;
        }
        let digest = self.store.commit(lease, writer)?;
        self.emit(EventPayload::TransferEnd {
            bytes: moved,
            duration_ms: duration_ms(started.elapsed()),
        });

        Ok(Transferred {
            digest,
            bytes_transferred: moved,
            bytes_kept: keep,
            rung,
            attempts: 0,
        })
    }

    fn emit(&self, payload: EventPayload) {
        self.observer.emit(&Event::new(self.sequence, payload));
    }
}

fn record_of(metadata: &SourceMetadata, rung: ResumeRung, written: u64) -> SourceRecord {
    SourceRecord {
        location: metadata.location.clone(),
        host: metadata.host.as_str().to_owned(),
        size: metadata.size,
        etag: validator_of(&metadata.identity),
        identity: metadata.identity.clone(),
        last_modified: None,
        accepts_ranges: metadata.supports_ranges,
        written,
        rung,
    }
}

fn validator_of(identity: &SourceIdentity) -> Option<String> {
    match identity {
        SourceIdentity::StrongValidator(tag) | SourceIdentity::WeakValidator(tag) => {
            Some(tag.clone())
        }
        _ => None,
    }
}

fn duration_ms(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn copy(
    mut body: impl Read,
    writer: &mut impl Write,
    location: &str,
    moved: &mut u64,
    buffer: &mut [u8],
) -> Result<(), Error> {
    loop {
        let filled = body
            .read(buffer)
            .map_err(|reason| body_failure(location, &reason))?;
        if filled == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..filled]).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("make room in the cache, because the bytes could not be written: {reason}"),
            )
        })?;
        *moved += filled as u64;
    }
}

/// Everything an attempt is retried against.
pub struct Retry<'a, P> {
    /// The bounds this run may not exceed.
    pub limits: &'a Limits,
    /// How long to wait and how far to jitter.
    pub pause: &'a P,
    /// Where the event stream is written.
    pub observer: &'a dyn Observer,
    /// The numbers the events are ordered by.
    pub sequence: &'a Sequence,
}

impl<P: Pause> Retry<'_, P> {
    /// Runs an attempt until it succeeds, fails terminally, or runs out.
    ///
    /// Takes the work, which receives the attempt number counted from one.
    /// Waits between attempts with exponential backoff and full jitter, and
    /// emits one retry event per wait.
    ///
    /// # Errors
    ///
    /// Returns the last failure, carrying how many attempts were made.
    pub fn until_spent<T>(
        &self,
        mut work: impl FnMut(u32) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let failure = match work(attempt) {
                Ok(done) => return Ok(done),
                Err(failure) => failure,
            };
            if !failure.retryable() || attempt >= self.limits.retry_attempts {
                return Err(failure.with_attempts(attempt));
            }
            self.observer.emit(&Event::new(
                self.sequence,
                EventPayload::TransferRetry {
                    attempt,
                    reason: failure.next_action().to_owned(),
                },
            ));
            self.pause
                .sleep(backoff(self.limits, attempt, self.pause.fraction()));
        }
    }
}

/// A pause that really sleeps, with jitter drawn from the clock.
#[derive(Debug, Default)]
pub struct SleepingPause;

impl Pause for SleepingPause {
    fn fraction(&self) -> f64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        f64::from(nanos) / f64::from(u32::MAX)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Names why a body stopped arriving.
///
/// Takes where the bytes were coming from and what the read reported. Returns a
/// timeout when the source went quiet and a refusal when it closed the
/// connection. Every answer is retryable, because neither is terminal.
#[must_use]
pub fn body_failure(location: &str, reason: &std::io::Error) -> Error {
    let quiet = matches!(
        reason.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || reason.to_string().to_lowercase().contains("timeout");
    let kind = if quiet {
        ErrorKind::NetworkTimeout
    } else {
        ErrorKind::NetworkRefused
    };
    Error::new(
        kind,
        format!("try the source again, because the body stopped arriving: {reason}"),
    )
    .with_source(location)
    .with_retryable(true)
}
