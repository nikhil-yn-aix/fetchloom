//! Moving bytes from a source into the store, once, with retry and resume.

use std::io::{Read, Write};
use std::num::NonZeroU32;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::candidate::{Probed, Separator, score};
use crate::credential::Credential;
use crate::degrade::DegradeQueue;
use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::event::{Event, EventPayload, Sequence, Span};
use crate::flights::Flights;
use crate::limits::Limits;
use crate::partial_key::PartialKey;
use crate::redact::SafeUrl;
use crate::reference::Host;
use crate::resume::ResumeRung;
use crate::seam::observer::Observer;
use crate::seam::source::{
    ByteRange, Revalidated, Source, SourceIdentity, SourceMetadata, Validator,
};
use crate::seam::store::Store;
use crate::source_record::SourceRecord;
use crate::split::{Refused, parts_for, spans};
use crate::tuning::{Answer, Controller, HostMeasurement, Meter, WriteRate};

/// How many bytes move between the source and the store at a time.
const BUFFER: usize = 1 << 20;

/// How many buffers one span of a split holds, which is what bounds how far
/// ahead of the writer a span may run.
const SPAN_BUFFERS: usize = 2;

/// What one transfer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transferred {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes, taken in the same pass.
    pub interop: Option<crate::digest::InteropDigest>,
    /// How many bytes arrived from the source in this run.
    pub bytes_transferred: u64,
    /// How many bytes were already on disk and kept.
    pub bytes_kept: u64,
    /// The rung the transfer stood on.
    pub rung: ResumeRung,
    /// How many attempts were made.
    pub attempts: u32,
    /// What the source said identifies these bytes. Empty when the run learned
    /// nothing.
    pub validator: Validator,
    /// The location that answered with the bytes, which is where the chain of
    /// redirects ended and not where it started.
    pub served: SafeUrl,
    /// The source this transfer took and why, when a run selected one.
    pub chosen: Option<Chosen>,
}

/// The two impure things a retry needs: a random fraction and a wait.
pub trait Pause: Send + Sync {
    /// Returns a value between zero and one for the jitter.
    fn fraction(&self) -> f64;

    /// Waits for the given duration.
    fn sleep(&self, duration: Duration);
}

/// How long to wait before an attempt, with full jitter.
#[must_use]
pub fn backoff(limits: &Limits, attempt: u32, fraction: f64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let full = Duration::from_secs(1u64 << exponent).min(limits.retry_ceiling);
    full.mul_f64(fraction.clamp(0.0, 1.0))
}

/// Reports whether a wait a source asked for is one this run may honor.
#[must_use]
pub fn honors(limits: &Limits, retry_after: Duration) -> bool {
    retry_after <= limits.retry_ceiling
}

/// What a run already holds for a reference no digest pins.
pub struct Prior {
    /// What the reference resolved to last time.
    pub digest: ContentDigest,
    /// What the source said identified those bytes.
    pub validator: Validator,
}

/// Decides where a transfer starts, given what is on disk and what the source
/// says now.
///
/// # Errors
///
/// Fails with `source.identity_changed` when the partial stands on the second
/// rung and the source has since served a different immutable identity for the
/// same location.
pub fn rung_for(
    recorded: Option<&SourceRecord>,
    now: &SourceMetadata,
    on_disk: u64,
    verified: u64,
) -> Result<(ResumeRung, u64), Error> {
    if verified > 0 {
        return Ok((ResumeRung::Outboard, verified));
    }
    if on_disk == 0 {
        return Ok((rung_of(&now.identity), 0));
    }
    let Some(recorded) = recorded else {
        return Ok((ResumeRung::NoValidator, 0));
    };
    if rung_of(&recorded.identity) == ResumeRung::ImmutableIdentity
        && rung_of(&now.identity) == ResumeRung::ImmutableIdentity
        && recorded.identity != now.identity
    {
        return Err(Error::new(
            ErrorKind::SourceIdentityChanged,
            "fetch this object from a source whose immutable identity is immutable, because this \
             one served a different one for the same location and so the identity it promised does \
             not hold",
        ));
    }
    if !now.supports_ranges || !recorded.identifies_the_same_bytes_as(&now.identity) {
        return Ok((ResumeRung::NoValidator, 0));
    }
    Ok((rung_of(&now.identity), on_disk))
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
    /// What this run has measured about a candidate's host, when it has
    /// measured anything.
    pub measurement: &'a (dyn Fn(&str) -> Option<HostMeasurement> + Sync),
    /// Where the event stream is written.
    pub observer: &'a dyn Observer,
    /// The numbers the events are ordered by.
    pub sequence: &'a Sequence,
    /// The in-flight counts and per-host controllers this run is held inside.
    pub flights: &'a Flights<'a>,
    /// The ceiling on how fast the run may move bytes, when one was set.
    pub meter: Option<&'a Meter>,
    /// Finds the credential a request to a host may carry, so that a transfer
    /// moving to a second host resolves that host's own.
    pub credential: &'a (dyn Fn(&str) -> Result<Option<Credential>, Error> + Sync),
    /// Names a host whose credential the run does not hold, and how much time
    /// holding it would have saved, so that the policy may offer it.
    pub offer: &'a (dyn Fn(&str, Duration) + Sync),
}

/// The source a transfer took, and why it took that one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    /// The source the bytes came from.
    pub source: SafeUrl,
    /// Why it was chosen over the alternatives.
    pub reason: String,
}

/// Returns a candidate no probe was spent on, which is scored on what the run
/// already knows about its host and nothing the source said.
fn unprobed(index: usize, location: &str, headroom: u32) -> Probed {
    Probed {
        index,
        location: location.to_owned(),
        metadata: None,
        throughput: None,
        time_to_first_byte_ms: None,
        refused: false,
        headroom,
    }
}

impl<S: Source + Sync, T: Store + Sync, P: Pause> Transfer<'_, S, T, P> {
    /// Moves one object from the source that scored best into the store.
    ///
    /// # Errors
    ///
    /// Fails when every source is exhausted, when a source's bytes do not hash
    /// to the expected digest, and when the store cannot be written.
    pub fn run(
        &self,
        expected: Option<ContentDigest>,
        prior: &dyn Fn(&str) -> Option<Prior>,
        locations: &[String],
    ) -> Result<Transferred, Error> {
        let (ordered, separator) = self.select(locations);
        let mut last = None;
        for (index, candidate) in ordered.iter().enumerate() {
            let already = candidate.metadata.clone();
            match self.attempt_until_spent(expected, prior, &candidate.location, already) {
                Ok(mut done) => {
                    done.chosen = Some(Chosen {
                        source: SafeUrl::new(&candidate.location),
                        reason: if index == 0 {
                            separator.because().to_owned()
                        } else {
                            "every source ahead of it was exhausted".to_owned()
                        },
                    });
                    return Ok(done);
                }
                Err(error) => {
                    if let Some(next) = ordered.get(index + 1) {
                        self.degradations.record(
                            SafeUrl::new(&candidate.location).to_string(),
                            SafeUrl::new(&next.location).to_string(),
                            error.next_action().to_owned(),
                        );
                        self.emit(EventPayload::SourceFailover {
                            from: SafeUrl::new(&candidate.location),
                            to: SafeUrl::new(&next.location),
                            reason: error.next_action().to_owned(),
                        });
                        self.emit(EventPayload::SourceSelected {
                            source: SafeUrl::new(&next.location),
                            reason: "every source ahead of it was exhausted".to_owned(),
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

    /// Probes the candidates this run may probe, scores every candidate, and
    /// reports which input decided.
    fn select(&self, locations: &[String]) -> (Vec<Probed>, Separator) {
        let Some(first) = locations.first() else {
            return (Vec::new(), Separator::TheOnlyOne);
        };
        if locations.len() == 1 {
            self.emit(EventPayload::SourceSelected {
                source: SafeUrl::new(first),
                reason: Separator::TheOnlyOne.because().to_owned(),
            });
            return (
                vec![unprobed(0, first, self.headroom(first))],
                Separator::TheOnlyOne,
            );
        }
        let limit = usize::try_from(self.limits.probed_candidates)
            .unwrap_or(usize::MAX)
            .max(1);
        let probing = locations.len().min(limit);
        let mut probed = self.probe_all(&locations[..probing]);
        let separator = score(&mut probed);
        probed.extend(
            locations[probing..]
                .iter()
                .enumerate()
                .map(|(offset, location)| {
                    unprobed(probing + offset, location, self.headroom(location))
                }),
        );
        if let Some(taken) = probed.first() {
            self.emit(EventPayload::SourceSelected {
                source: SafeUrl::new(&taken.location),
                reason: separator.because().to_owned(),
            });
            self.offer_what_a_credential_would_save(taken, &probed);
        }
        (probed, separator)
    }

    /// Offers a credential for a candidate the run could not read, when what
    /// that candidate's host has been measured at would save more time than the
    /// offer threshold allows to pass in silence.
    ///
    /// A refused probe says nothing about the object, so the only difference
    /// there is to measure is between the two hosts, over the length the taken
    /// candidate stated. With no measurement behind either host, or no stated
    /// length, nothing is projected and nothing is offered.
    fn offer_what_a_credential_would_save(&self, taken: &Probed, probed: &[Probed]) {
        let Some(length) = taken.metadata.as_ref().and_then(|found| found.size) else {
            return;
        };
        let Some(here) = taken.throughput.filter(|rate| *rate > 0) else {
            return;
        };
        for gated in probed.iter().filter(|one| one.refused) {
            let Some(there) = gated.throughput.filter(|rate| *rate > 0) else {
                continue;
            };
            if there <= here {
                continue;
            }
            let saved = taking(length, here).saturating_sub(taking(length, there));
            (self.offer)(Host::of_location(&gated.location).as_str(), saved);
        }
    }

    /// Makes one bounded metadata request against each candidate at once, so
    /// that scoring costs one round trip rather than one for every candidate.
    fn probe_all(&self, locations: &[String]) -> Vec<Probed> {
        let mut found: Vec<Probed> = std::thread::scope(|scope| {
            let asked: Vec<_> = locations
                .iter()
                .enumerate()
                .map(|(index, location)| {
                    scope.spawn(move || {
                        self.emit(EventPayload::SourceProbe {
                            source: SafeUrl::new(location),
                        });
                        let host = Host::of_location(location);
                        let credential = (self.credential)(host.as_str()).ok().flatten();
                        let answered = self.source.probe(location, credential.as_ref());
                        let refused = answered.as_ref().err().is_some_and(|failure| {
                            failure.kind() == ErrorKind::PolicyCredentialMissing
                        });
                        let metadata = answered.ok();
                        let measured = (self.measurement)(location);
                        Probed {
                            index,
                            location: location.clone(),
                            metadata,
                            throughput: measured.map(|found| found.throughput),
                            time_to_first_byte_ms: measured
                                .map(|found| found.time_to_first_byte_ms),
                            refused,
                            headroom: self.flights.headroom(host.as_str()),
                        }
                    })
                })
                .collect();
            asked
                .into_iter()
                .filter_map(|one| one.join().ok())
                .collect()
        });
        found.sort_by_key(|one| one.index);
        found
    }

    fn headroom(&self, location: &str) -> u32 {
        self.flights.headroom(Host::of_location(location).as_str())
    }

    fn attempt_until_spent(
        &self,
        expected: Option<ContentDigest>,
        prior: &dyn Fn(&str) -> Option<Prior>,
        location: &str,
        already: Option<SourceMetadata>,
    ) -> Result<Transferred, Error> {
        let controller = self
            .flights
            .controller(Host::of_location(location).as_str());
        let retry = Retry {
            limits: self.limits,
            pause: self.pause,
            observer: self.observer,
            sequence: self.sequence,
            controller: &controller,
            host: Host::of_location(location),
        };
        let mut buffer = vec![0u8; BUFFER];
        let probed = Mutex::new(already);
        retry.until_spent(|attempt| {
            let already = probed.lock().unwrap_or_else(PoisonError::into_inner).take();
            self.attempt_once(expected, prior, location, already, &mut buffer)
                .map(|mut done| {
                    done.attempts = attempt;
                    done
                })
        })
    }

    /// Makes a bounded metadata request carrying whatever credential this run
    /// resolved for the location's own host.
    fn probe(&self, location: &str) -> Result<SourceMetadata, Error> {
        let credential = self.credential_for(location)?;
        self.source.probe(location, credential.as_ref())
    }

    /// Returns the credential a request to a location's host may carry.
    fn credential_for(&self, location: &str) -> Result<Option<Credential>, Error> {
        (self.credential)(Host::of_location(location).as_str())
    }

    /// Takes the single-writer claim on a key, reporting a wait for another
    /// writer as `cache.wait`.
    fn claim(&self, key: PartialKey) -> Result<T::Lease, Error> {
        let lease = self.store.lease(key)?;
        if self.store.waited(&lease) {
            self.emit(EventPayload::CacheWait { digest: key.name() });
        }
        Ok(lease)
    }

    fn attempt_once(
        &self,
        expected: Option<ContentDigest>,
        prior: &dyn Fn(&str) -> Option<Prior>,
        location: &str,
        already: Option<SourceMetadata>,
        buffer: &mut [u8],
    ) -> Result<Transferred, Error> {
        if let Some(digest) = expected
            && self.store.contains(digest)?
        {
            return Ok(held(digest));
        }

        let arrived = match self.ask_whether_it_changed(expected, prior, location)? {
            Asked::Unchanged(digest) => return Ok(held(digest)),
            Asked::Changed(metadata, body) => Some((metadata, body)),
            Asked::Silence => None,
        };

        let metadata = match arrived.as_ref() {
            Some((metadata, _)) => metadata.clone(),
            None => match already {
                Some(found) => found,
                None => self.probe(location)?,
            },
        };
        let key = match expected {
            Some(digest) => PartialKey::of_content(digest),
            None => PartialKey::of_source(&metadata),
        };
        let (rung, keep) = self.where_it_starts(key, expected, &metadata, arrived.is_some())?;

        let lease = self.claim(key)?;
        if let Some(digest) = expected
            && self.store.contains(digest)?
        {
            return Ok(held(digest));
        }
        self.emit(EventPayload::TransferStart {
            source: metadata.location.clone(),
            host: metadata.host.clone(),
            expected_bytes: metadata.size,
        });
        if keep > 0 {
            self.emit(EventPayload::TransferResume {
                rung,
                bytes_kept: keep,
            });
        }

        let split = arrived
            .is_none()
            .then(|| self.split_into(&metadata, location))
            .flatten();
        let length = metadata.size.unwrap_or(0);
        self.store
            .record_source(key, &record_of(&metadata, rung, keep))?;
        let mut writer = self.store.resume(&lease, length, keep)?;

        let started = Span::start();
        let mut moved = 0u64;
        let (served, arrived) = if let Some(width) = split {
            let covered = spans(keep, length, width);
            (
                metadata.location.clone(),
                self.copy_split(location, &metadata, &covered, &mut writer, &mut moved),
            )
        } else {
            let (from, body) = self.body_from(arrived, location, &metadata, keep)?;
            (
                from,
                copy(
                    body,
                    &mut writer,
                    location,
                    &mut moved,
                    buffer,
                    &Backpressure {
                        meter: self.meter,
                        pause: self.pause,
                        controller: &self
                            .flights
                            .controller(Host::of_location(location).as_str()),
                    },
                ),
            )
        };
        self.store
            .record_source(key, &record_of(&metadata, rung, keep + moved))?;
        ended_early(&arrived, &metadata, location, keep + moved)?;
        let digests = self.store.commit(lease, writer)?;
        self.emit(EventPayload::TransferEnd {
            host: metadata.host.clone(),
            bytes: moved,
            duration_ms: started.elapsed_ms(),
        });

        Ok(Transferred {
            digest: digests.content,
            interop: Some(digests.interop),
            bytes_transferred: moved,
            bytes_kept: keep,
            rung,
            attempts: 0,
            validator: validator_for(&metadata),
            served,
            chosen: None,
        })
    }

    /// Returns the rung a transfer stands on and how many bytes already on disk
    /// it keeps, dropping a partial the source no longer identifies.
    ///
    /// # Errors
    ///
    /// Fails when the store cannot be read and when the source has served a
    /// different immutable identity for the same location.
    fn where_it_starts(
        &self,
        key: PartialKey,
        expected: Option<ContentDigest>,
        metadata: &SourceMetadata,
        revalidated: bool,
    ) -> Result<(ResumeRung, u64), Error> {
        let recorded = self.store.recorded_source(key)?;
        let on_disk = recorded.as_ref().map_or(0, |record| record.written);
        let verified = match expected {
            Some(digest) if !revalidated => self.store.verified_prefix(key, digest, on_disk)?,
            _ => 0,
        };
        let (rung, keep) = rung_for(recorded.as_ref(), metadata, on_disk, verified)?;
        if on_disk > keep {
            self.report_the_partial_dropped(recorded.as_ref(), metadata, rung);
            self.store.discard_partial(key)?;
        }
        Ok((rung, keep))
    }

    /// Records why the bytes already on disk were dropped rather than resumed
    /// from.
    fn report_the_partial_dropped(
        &self,
        recorded: Option<&SourceRecord>,
        metadata: &SourceMetadata,
        rung: ResumeRung,
    ) {
        if metadata.supports_ranges {
            self.degradations.record(
                format!(
                    "a resume on rung {}",
                    recorded.map_or(5, |record| record.rung.number())
                ),
                format!("rung {}, a transfer from zero", rung.number()),
                "the source no longer identifies the bytes the partial recorded, so appending to it would join two different objects",
            );
        } else {
            self.degradations.record(
                "a resume by range",
                "the whole object, transferred again from zero",
                "the source does not serve ranges",
            );
        }
    }

    /// Returns how many spans this run fetches one object as, recording a
    /// degradation when a split was wanted and a condition failed.
    ///
    /// A split is wanted once the object is long enough for the extra requests
    /// to pay for themselves; below that there is nothing to report, because
    /// nothing was given up.
    fn split_into(&self, metadata: &SourceMetadata, location: &str) -> Option<NonZeroU32> {
        let measured = (self.measurement)(location).map_or(0, |found| found.concurrency);
        let permitted = self
            .flights
            .controller(Host::of_location(location).as_str())
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .permitted();
        match parts_for(metadata, self.limits, measured.min(permitted)) {
            Ok(width) => Some(width),
            Err(Refused::NotLarge) => None,
            Err(refused) => {
                self.degradations.record(
                    "one object fetched as several ranges at once",
                    "the object fetched whole, in one stream",
                    refused.because(),
                );
                None
            }
        }
    }

    /// Fetches the missing bytes of one object as several spans at once and
    /// writes them in order, so that the digests taken as the bytes arrive are
    /// the digests of the object.
    fn copy_split(
        &self,
        location: &str,
        metadata: &SourceMetadata,
        covered: &[ByteRange],
        writer: &mut impl Write,
        moved: &mut u64,
    ) -> Result<(), Error> {
        let failure = Mutex::new(None);
        let controller = self
            .flights
            .controller(Host::of_location(location).as_str());
        let mut rate = WriteRate::default();
        std::thread::scope(|scope| {
            let mut arriving = Vec::with_capacity(covered.len());
            for span in covered {
                let (full, taken) = std::sync::mpsc::sync_channel::<Vec<u8>>(SPAN_BUFFERS);
                let (spent, reusable) = std::sync::mpsc::sync_channel::<Vec<u8>>(SPAN_BUFFERS);
                for _ in 0..SPAN_BUFFERS {
                    let _ = spent.send(vec![0u8; BUFFER]);
                }
                let held = &failure;
                scope.spawn(move || {
                    if let Err(reason) = self.fill(location, metadata, *span, &full, &reusable) {
                        let mut kept = held.lock().unwrap_or_else(PoisonError::into_inner);
                        if kept.is_none() {
                            *kept = Some(reason);
                        }
                    }
                });
                arriving.push((taken, spent));
            }
            for (taken, spent) in arriving {
                while let Ok(filled) = taken.recv() {
                    let length = filled.len() as u64;
                    let accepting = Instant::now();
                    writer.write_all(&filled).map_err(|reason| {
                        Error::new(
                            ErrorKind::CacheCorrupt,
                            format!(
                                "make room in the cache, because the bytes could not be written: {reason}"
                            ),
                        )
                    })?;
                    if rate.observed(length, accepting.elapsed()) {
                        controller
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .answered(Answer::Faltered);
                    }
                    *moved += length;
                    let _ = spent.send(filled);
                    if let Some(meter) = self.meter {
                        let owed = meter.moved(length);
                        if !owed.is_zero() {
                            self.pause.sleep(owed);
                        }
                    }
                }
            }
            Ok::<(), Error>(())
        })?;
        match failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// Streams one span into the channel the writer drains, reusing the buffers
    /// it gets back rather than allocating one for every chunk.
    fn fill(
        &self,
        location: &str,
        metadata: &SourceMetadata,
        span: ByteRange,
        full: &std::sync::mpsc::SyncSender<Vec<u8>>,
        reusable: &std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> Result<(), Error> {
        let credential = self.credential_for(location)?;
        let answered = self
            .source
            .fetch(location, Some(span), credential.as_ref())?;
        if answered.metadata.identity != metadata.identity {
            return Err(Error::new(
                ErrorKind::SourceIdentityChanged,
                "fetch this object whole, because the source served one span under a different \
                 identity than another and the two may not be the same object",
            )
            .with_source(location));
        }
        let mut body = answered.body;
        let mut left = span.length();
        while left > 0 {
            if crate::cancel::requested() {
                return Ok(());
            }
            let Ok(mut buffer) = reusable.recv() else {
                return Ok(());
            };
            buffer.resize(BUFFER, 0);
            let wanted = usize::try_from(left).unwrap_or(BUFFER).min(BUFFER);
            let filled = body
                .read(&mut buffer[..wanted])
                .map_err(|reason| body_failure(location, &reason))?;
            if filled == 0 {
                return Err(Error::new(
                    ErrorKind::IntegrityTruncated,
                    format!(
                        "fetch the span again, because the source ended it {left} bytes before the \
                         end it was asked for"
                    ),
                )
                .with_source(location)
                .with_retryable(true));
            }
            buffer.truncate(filled);
            left -= filled as u64;
            if full.send(buffer).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
    /// Returns the location that answered and the bytes it answered with.
    fn body_from(
        &self,
        arrived: Option<(SourceMetadata, S::Body)>,
        location: &str,
        metadata: &SourceMetadata,
        keep: u64,
    ) -> Result<(SafeUrl, Either<S::Body>), Error> {
        if let Some((from, body)) = arrived {
            return Ok((from.location, Either::Revalidated(body)));
        }
        let range = (keep > 0).then(|| ByteRange {
            start: keep,
            end: metadata.size.unwrap_or(u64::MAX),
        });
        let credential = self.credential_for(location)?;
        let answered = self.source.fetch(location, range, credential.as_ref())?;
        Ok((answered.metadata.location, Either::Fetched(answered.body)))
    }

    /// Asks a source in one request whether an object this run already holds is
    /// still what the reference names.
    fn ask_whether_it_changed(
        &self,
        expected: Option<ContentDigest>,
        prior: &dyn Fn(&str) -> Option<Prior>,
        location: &str,
    ) -> Result<Asked<S::Body>, Error> {
        if expected.is_some() {
            return Ok(Asked::Silence);
        }
        let Some(prior) = prior(location) else {
            return Ok(Asked::Silence);
        };
        if !prior.validator.can_be_asked_with() || !self.store.contains(prior.digest)? {
            return Ok(Asked::Silence);
        }
        let credential = self.credential_for(location)?;
        match self
            .source
            .revalidate(location, &prior.validator, credential.as_ref())?
        {
            Revalidated::Unchanged => Ok(Asked::Unchanged(prior.digest)),
            Revalidated::Changed(served) => {
                self.degradations.record(
                    "the object the cache already holds",
                    "a transfer of the whole object",
                    "the source answered that the bytes it serves are no longer the ones the recorded validator named",
                );
                Ok(Asked::Changed(served.metadata, served.body))
            }
        }
    }

    fn emit(&self, payload: EventPayload) {
        self.observer.emit(&Event::new(self.sequence, payload));
    }
}

/// What one conditional question produced.
enum Asked<B> {
    /// No question was asked.
    Silence,
    /// The source restated the validator it already gave.
    Unchanged(ContentDigest),
    /// The bytes changed, and the response carries them.
    Changed(SourceMetadata, B),
}

/// The bytes of a transfer, whichever request produced them.
enum Either<B> {
    /// A fetch, which may have carried a range.
    Fetched(B),
    /// The body a conditional request answered with, always from zero.
    Revalidated(B),
}

impl<B: Read> Read for Either<B> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Fetched(body) | Self::Revalidated(body) => body.read(buffer),
        }
    }
}

/// What a run reports for an object the cache already holds.
fn held(digest: ContentDigest) -> Transferred {
    Transferred {
        digest,
        interop: None,
        bytes_transferred: 0,
        bytes_kept: 0,
        rung: ResumeRung::Outboard,
        attempts: 0,
        validator: Validator::default(),
        served: SafeUrl::new(""),
        chosen: None,
    }
}

/// Returns what a response said identifies the bytes it served.
fn validator_for(metadata: &SourceMetadata) -> Validator {
    Validator {
        etag: validator_of(&metadata.identity),
        last_modified: metadata.last_modified.clone(),
    }
}

fn record_of(metadata: &SourceMetadata, rung: ResumeRung, written: u64) -> SourceRecord {
    SourceRecord {
        location: metadata.location.clone(),
        host: metadata.host.as_str().to_owned(),
        size: metadata.size,
        etag: validator_of(&metadata.identity),
        identity: metadata.identity.clone(),
        last_modified: metadata.last_modified.clone(),
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

/// The three things that decide how fast a copy loop is allowed to go: the
/// bandwidth ceiling, the wait it is served with, and the controller the volume
/// answers when it collapses.
struct Backpressure<'a, P> {
    meter: Option<&'a Meter>,
    pause: &'a P,
    controller: &'a Mutex<Controller>,
}

fn copy(
    mut body: impl Read,
    writer: &mut impl Write,
    location: &str,
    moved: &mut u64,
    buffer: &mut [u8],
    backpressure: &Backpressure<'_, impl Pause>,
) -> Result<(), Error> {
    let Backpressure {
        meter,
        pause,
        controller,
    } = backpressure;
    let mut rate = WriteRate::default();
    loop {
        if crate::cancel::requested() {
            return Ok(());
        }
        let filled = body
            .read(buffer)
            .map_err(|reason| body_failure(location, &reason))?;
        if filled == 0 {
            return Ok(());
        }
        let accepting = Instant::now();
        writer.write_all(&buffer[..filled]).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("make room in the cache, because the bytes could not be written: {reason}"),
            )
        })?;
        if rate.observed(filled as u64, accepting.elapsed()) {
            controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .answered(Answer::Faltered);
        }
        *moved += filled as u64;
        if let Some(meter) = meter {
            let owed = meter.moved(filled as u64);
            if !owed.is_zero() {
                pause.sleep(owed);
            }
        }
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
    /// How many transfers this host permits, moved as the host answers.
    pub controller: &'a Mutex<Controller>,
    /// The host being retried, which every retry event is filed under.
    pub host: crate::reference::Host,
}

impl<P: Pause> Retry<'_, P> {
    /// Runs an attempt until it succeeds, fails terminally, or runs out.
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
                Ok(done) => {
                    if attempt == 1 {
                        self.answered(Answer::Clean);
                    }
                    return Ok(done);
                }
                Err(failure) => failure,
            };
            self.answered(if failure.retry_after().is_some() {
                Answer::RateLimited
            } else {
                Answer::Faltered
            });
            if !failure.retryable() || attempt >= self.limits.retry_attempts {
                return Err(failure.with_attempts(attempt));
            }
            self.observer.emit(&Event::new(
                self.sequence,
                EventPayload::TransferRetry {
                    host: self.host.clone(),
                    attempt,
                    reason: failure.next_action().to_owned(),
                },
            ));
            let backing_off = backoff(self.limits, attempt, self.pause.fraction());
            let asked = failure
                .retry_after()
                .filter(|wait| honors(self.limits, *wait));
            self.pause.sleep(backing_off.max(asked.unwrap_or_default()));
        }
    }

    /// Moves the host's permitted count for what it answered.
    fn answered(&self, answer: Answer) {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .answered(answer);
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

/// Fails when a body stopped before the length the source stated, or when the
/// copy itself failed.
///
/// # Errors
///
/// Fails with `integrity.truncated` when the source ended a body early, and
/// with whatever ended the copy otherwise.
fn ended_early(
    arrived: &Result<(), Error>,
    metadata: &SourceMetadata,
    location: &str,
    have: u64,
) -> Result<(), Error> {
    let short = metadata.size.is_some_and(|expected| have < expected);
    let timed_out = arrived
        .as_ref()
        .err()
        .is_some_and(|failure| failure.kind() == ErrorKind::NetworkTimeout);
    if !short && arrived.is_ok() {
        return Ok(());
    }
    if short && !timed_out {
        return Err(Error::new(
            ErrorKind::IntegrityTruncated,
            format!(
                "fetch the rest, because the source ended the body after {have} of the {} bytes it said the object holds",
                metadata.size.unwrap_or_default()
            ),
        )
        .with_source(location)
        .with_retryable(true));
    }
    match arrived {
        Ok(()) => Ok(()),
        Err(failure) => Err(failure.clone()),
    }
}

/// Returns how long a length takes to move at a rate in bytes per second.
fn taking(length: u64, rate: u64) -> Duration {
    let nanos = u128::from(length) * 1_000_000_000 / u128::from(rate.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}
