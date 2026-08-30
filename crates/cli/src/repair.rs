//! Refetching the damaged ranges of a cached object.

use std::sync::Arc;

use fetchloom_cache::Cache;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::repair::{RepairPlan, WholeReason, plan_repair};
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::source::{ByteRange, Source};
use fetchloom_engine::work::{Work, WorkCounter};
use fetchloom_platform::NativePlatform;
use fetchloom_sources::HttpSource;

/// What one repair did.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RepairResult {
    /// What the repair did: `repaired` when bytes moved, `unchanged` when
    /// nothing was damaged.
    pub status: &'static str,
    /// The object that was repaired.
    pub digest: String,
    /// How many separate ranges were fetched.
    pub ranges: u64,
    /// How many bytes those ranges carried.
    pub bytes: u64,
    /// What the run read, wrote, and asked for.
    pub work: Work,
}

/// Everything one repair runs against.
pub struct Repair<'a> {
    /// The cache holding the damaged object.
    pub cache: &'a Cache<NativePlatform>,
    /// Where the object came from.
    pub location: &'a str,
    /// Where the run counts what it moves.
    pub work: &'a Arc<WorkCounter>,
    /// Where the event stream is written.
    pub observer: &'a dyn Observer,
    /// The numbers the events are ordered by.
    pub sequence: &'a Sequence,
}

impl Repair<'_> {
    /// Repairs one object, fetching only the ranges that are wrong.
    ///
    /// Takes the digest the object is named by. Returns what moved.
    ///
    /// # Errors
    ///
    /// Fails with `integrity.mismatch` when the repaired bytes still do not
    /// hash to the digest, and with whatever the source failed with.
    pub fn run(&self, digest: ContentDigest) -> Result<RepairResult, Error> {
        let emit = |payload: EventPayload| self.observer.emit(&Event::new(self.sequence, payload));
        emit(EventPayload::VerifyStart);

        let localized = self.cache.localize(digest)?;
        let source = HttpSource::new(Limits::default(), Arc::clone(self.work));
        let metadata = source.probe(self.location, None)?;

        let whole = whole_of(&metadata, &localized);
        let limits = Limits::default();
        let plan = if localized.not_localized.is_some() {
            Self::report_unlocalized(&localized, whole, &emit);
            RepairPlan::Whole(WholeReason::NoRanges)
        } else {
            plan_repair(
                &localized.damaged,
                localized.object_len,
                metadata.supports_ranges,
                &limits,
            )
        };

        let spans = match plan {
            RepairPlan::Nothing => {
                emit(EventPayload::VerifyEnd {
                    bytes: 0,
                    duration_ms: 0,
                });
                return Ok(RepairResult {
                    status: "unchanged",
                    digest: digest.to_string(),
                    ranges: 0,
                    bytes: 0,
                    work: self.work.taken(),
                });
            }
            RepairPlan::Spans(spans) => spans,
            RepairPlan::Whole(reason) => {
                let (requested, used, why) = reason.degradation(whole);
                emit(EventPayload::Degrade {
                    requested,
                    used,
                    reason: why,
                });
                vec![ByteRange {
                    start: 0,
                    end: whole,
                }]
            }
        };

        let mut writer = self.cache.begin_repair(digest)?;
        let mut moved = 0u64;
        for span in &spans {
            emit(EventPayload::VerifyRange {
                start: span.start,
                end: span.end,
            });
            let range = (spans.len() > 1 || span.start > 0).then_some(*span);
            let body = source.fetch(self.location, range, None)?;
            moved += self.cache.patch(&mut writer, span.start..span.end, body)?;
        }
        for entry in source.take_degradations() {
            emit(EventPayload::Degrade {
                requested: entry.requested,
                used: entry.used,
                reason: entry.reason,
            });
        }

        match self.cache.finish_repair(writer, whole) {
            Ok(_) => {
                emit(EventPayload::VerifyEnd {
                    bytes: moved,
                    duration_ms: 0,
                });
                emit(EventPayload::PublishCommit);
                Ok(RepairResult {
                    status: "repaired",
                    digest: digest.to_string(),
                    ranges: spans.len() as u64,
                    bytes: moved,
                    work: self.work.taken(),
                })
            }
            Err(refused) => {
                emit(EventPayload::VerifyMismatch {
                    error: refused.clone(),
                });
                let recorded = self.cache.recorded_source_of(digest);
                self.cache.quarantine(digest, recorded.0, recorded.1)?;
                Err(refused)
            }
        }
    }

    /// Says why the damage could not be narrowed, before the whole object is
    /// fetched instead.
    fn report_unlocalized(
        localized: &fetchloom_cache::repair::Localized,
        whole: u64,
        emit: &dyn Fn(EventPayload),
    ) {
        use fetchloom_cache::diagnosis::NotLocalized;

        let reason = match localized.not_localized {
            Some(NotLocalized::NoTreeStored) => {
                "no chunk tree was stored for this object, so there is nothing to walk and no way to say which of its bytes are wrong"
            }
            Some(NotLocalized::TreeDoesNotCheckOut) => {
                "the stored chunk tree does not check out against the digest, so it says nothing about which of the object's bytes are wrong"
            }
            Some(NotLocalized::ObjectUnreadable) | None => {
                "the object's own bytes could not be read, so nothing could be compared against the tree"
            }
        };
        emit(EventPayload::Degrade {
            requested: "a repair fetching only the damaged ranges".to_owned(),
            used: format!("a fetch of all {whole} bytes"),
            reason: reason.to_owned(),
        });
    }
}

/// Returns how long the whole object is.
///
/// The source is asked rather than the file on disk.
fn whole_of(
    metadata: &fetchloom_engine::seam::source::SourceMetadata,
    localized: &fetchloom_cache::repair::Localized,
) -> u64 {
    metadata.size.unwrap_or(localized.object_len)
}

/// Returns the digest a repair works on, from what the run states or from what
/// the cache last resolved the reference to.
///
/// # Errors
///
/// Fails with `reference.unresolved` when neither states one.
pub fn digest_for(
    cache: &Cache<NativePlatform>,
    reference: &str,
    pinned: Option<ContentDigest>,
) -> Result<ContentDigest, Error> {
    if let Some(digest) = pinned {
        return Ok(digest);
    }
    if let Some(found) = cache.resolution(reference)? {
        return Ok(found.digest);
    }
    if let Some(digest) = parse_digest(reference)
        && cache.locate(digest).is_some()
    {
        return Ok(digest);
    }
    Err(Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "run get on {reference} first, because repair puts right an object this cache already holds and this one has never been fetched here"
        ),
    ))
}

fn parse_digest(reference: &str) -> Option<ContentDigest> {
    let parsed = reference.parse::<fetchloom_engine::digest::Digest>().ok()?;
    ContentDigest::try_from(parsed).ok()
}

/// Reports what a repair did.
#[must_use]
pub fn report(outcome: &Result<RepairResult, Error>, json: bool) -> ExitCode {
    match outcome {
        Ok(result) => {
            if json {
                match serde_json::to_string(result) {
                    Ok(rendered) => println!("{rendered}"),
                    Err(reason) => eprintln!("the result could not be written: {reason}"),
                }
            } else if result.ranges == 0 {
                println!("{}  nothing damaged", result.digest);
            } else {
                println!(
                    "{}  {} ranges  {} bytes",
                    result.digest, result.ranges, result.bytes
                );
            }
            ExitCode::Success
        }
        Err(refused) => crate::report(refused, json),
    }
}
