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
use crate::seam::source::{ByteRange, Revalidated, Source, SourceMetadata, Validator};
use crate::seam::store::Store;
use crate::source_record::SourceRecord;
use crate::split::{Refused, parts_for, spans};
use crate::tuning::{Answer, HostMeasurement, Meter, WriteRate};

use crate::limits::STREAM_BUFFER_BYTES as BUFFER;

mod copy;
mod resume;
mod retry;

pub use resume::{Prior, rung_for};
pub use retry::{Retry, SleepingPause, backoff, body_failure, honors};

use copy::{Asked, Backpressure, Either, copy};
use resume::{held, record_of, validator_for};
use retry::{ended_early, taking};

const SPAN_BUFFERS: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transferred {
    pub digest: ContentDigest,
    pub interop: Option<crate::digest::InteropDigest>,
    pub bytes_transferred: u64,
    pub bytes_kept: u64,
    pub rung: ResumeRung,
    pub attempts: u32,
    pub validator: Validator,
    pub served: SafeUrl,
    pub chosen: Option<Chosen>,
}

pub trait Pause: Send + Sync {
    fn fraction(&self) -> f64;

    fn sleep(&self, duration: Duration);
}

pub struct Transfer<'a, S, T, P> {
    pub store: &'a T,
    pub source: &'a S,
    pub pause: &'a P,
    pub limits: &'a Limits,
    pub degradations: &'a DegradeQueue,
    pub measurement: &'a (dyn Fn(&str) -> Option<HostMeasurement> + Sync),
    pub observer: &'a dyn Observer,
    pub sequence: &'a Sequence,
    pub flights: &'a Flights<'a>,
    pub meter: Option<&'a Meter>,
    pub credential: &'a (dyn Fn(&str) -> Result<Option<Credential>, Error> + Sync),
    pub offer: &'a (dyn Fn(&str, Duration) + Sync),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chosen {
    pub source: SafeUrl,
    pub reason: String,
}

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
    /// # Errors
    /// Whatever the last candidate source reported once every source has been
    /// tried: the `network.*` kinds, `integrity.mismatch` when the bytes do
    /// not hash to what was expected, and the store's own kinds for a write
    /// that could not be completed.
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

    fn probe(&self, location: &str) -> Result<SourceMetadata, Error> {
        let credential = self.credential_for(location)?;
        self.source.probe(location, credential.as_ref())
    }

    fn credential_for(&self, location: &str) -> Result<Option<Credential>, Error> {
        (self.credential)(Host::of_location(location).as_str())
    }

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
        let digests = self.committed(lease, writer, keep + moved)?;
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

    fn committed(
        &self,
        lease: T::Lease,
        writer: T::Writer,
        bytes: u64,
    ) -> Result<crate::hashing::Digests, Error> {
        let checking = Span::start();
        self.emit(EventPayload::VerifyStart);
        match self.store.commit(lease, writer) {
            Ok(digests) => {
                self.emit(EventPayload::VerifyEnd {
                    bytes,
                    duration_ms: checking.elapsed_ms(),
                });
                Ok(digests)
            }
            Err(reason) => {
                if reason.kind() == ErrorKind::IntegrityMismatch {
                    self.emit(EventPayload::VerifyMismatch {
                        error: reason.clone(),
                    });
                }
                Err(reason)
            }
        }
    }

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
