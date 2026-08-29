//! The event stream every display mode and every log is drawn from.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::digest::ContentDigest;
use crate::error::Error;
use crate::reconcile::ReconcileOutcome;
use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::timestamp::Timestamp;

/// Every event name, in the order the contract lists them.
pub const EVENT_NAMES: [&str; 34] = [
    "run.start",
    "run.end",
    "resolve.start",
    "resolve.alias",
    "resolve.end",
    "plan.ready",
    "cache.hit",
    "cache.miss",
    "cache.wait",
    "credential.required",
    "credential.offer",
    "credential.declined",
    "listing.start",
    "listing.skipped",
    "listing.end",
    "source.probe",
    "source.selected",
    "source.failover",
    "transfer.start",
    "transfer.progress",
    "transfer.retry",
    "transfer.resume",
    "transfer.end",
    "verify.start",
    "verify.range",
    "verify.mismatch",
    "verify.end",
    "extract.start",
    "extract.reject",
    "extract.end",
    "publish.commit",
    "reconcile.outcome",
    "degrade",
    "error",
];

/// What one event says.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "event")]
pub enum EventPayload {
    /// A run began.
    #[serde(rename = "run.start")]
    RunStart,
    /// A run finished.
    #[serde(rename = "run.end")]
    RunEnd {
        /// How long the run took, in milliseconds.
        duration_ms: u64,
    },
    /// Resolution began.
    #[serde(rename = "resolve.start")]
    ResolveStart,
    /// A reference resolved through an alias.
    #[serde(rename = "resolve.alias")]
    ResolveAlias {
        /// The alias that was followed.
        from: String,
        /// What it resolved to.
        to: String,
    },
    /// Resolution finished.
    #[serde(rename = "resolve.end")]
    ResolveEnd {
        /// How long resolution took, in milliseconds.
        duration_ms: u64,
    },
    /// A plan is resolved and can be reported or executed.
    #[serde(rename = "plan.ready")]
    PlanReady,
    /// The cache already holds an object.
    #[serde(rename = "cache.hit")]
    CacheHit {
        /// The object the cache holds.
        digest: ContentDigest,
    },
    /// The cache does not hold an object.
    #[serde(rename = "cache.miss")]
    CacheMiss {
        /// The object the cache does not hold.
        digest: ContentDigest,
    },
    /// Another writer holds an object, so this run waits and reuses it.
    #[serde(rename = "cache.wait")]
    CacheWait {
        /// The object being waited for.
        digest: ContentDigest,
    },
    /// A credential is required before the run can continue.
    #[serde(rename = "credential.required")]
    CredentialRequired {
        /// The provider that requires it.
        provider: String,
    },
    /// A credential would improve the run and is offered.
    #[serde(rename = "credential.offer")]
    CredentialOffer {
        /// The provider that would use it.
        provider: String,
    },
    /// An offered credential was declined for the whole run.
    #[serde(rename = "credential.declined")]
    CredentialDeclined {
        /// The provider whose offer was declined.
        provider: String,
    },
    /// A container is being listed.
    #[serde(rename = "listing.start")]
    ListingStart {
        /// The container being listed.
        source: SafeUrl,
    },
    /// Entries pointing outside the prefix were ignored.
    #[serde(rename = "listing.skipped")]
    ListingSkipped {
        /// How many entries were ignored.
        count: u64,
    },
    /// A listing finished.
    #[serde(rename = "listing.end")]
    ListingEnd {
        /// How many entries the listing returned.
        entries: u64,
        /// How long the listing took, in milliseconds.
        duration_ms: u64,
    },
    /// A candidate source was probed.
    #[serde(rename = "source.probe")]
    SourceProbe {
        /// The candidate probed.
        source: SafeUrl,
    },
    /// A source was chosen.
    #[serde(rename = "source.selected")]
    SourceSelected {
        /// The source chosen.
        source: SafeUrl,
        /// Why it was chosen.
        reason: String,
    },
    /// A transfer moved to another source.
    #[serde(rename = "source.failover")]
    SourceFailover {
        /// The source moved away from.
        from: SafeUrl,
        /// The source moved to.
        to: SafeUrl,
        /// Why the move happened.
        reason: String,
    },
    /// A transfer began.
    #[serde(rename = "transfer.start")]
    TransferStart {
        /// The source the bytes come from.
        source: SafeUrl,
        /// How many bytes are expected, when the source states it.
        expected_bytes: Option<u64>,
    },
    /// A transfer advanced.
    #[serde(rename = "transfer.progress")]
    TransferProgress {
        /// How many bytes have arrived so far.
        bytes: u64,
    },
    /// A transient failure is being retried.
    #[serde(rename = "transfer.retry")]
    TransferRetry {
        /// Which attempt this is.
        attempt: u32,
        /// Why the previous attempt failed.
        reason: String,
    },
    /// A transfer resumed rather than restarting.
    #[serde(rename = "transfer.resume")]
    TransferResume {
        /// The rung of the resume ladder used.
        rung: ResumeRung,
        /// How many bytes already on disk were kept.
        bytes_kept: u64,
    },
    /// A transfer finished.
    #[serde(rename = "transfer.end")]
    TransferEnd {
        /// How many bytes arrived.
        bytes: u64,
        /// How long the transfer took, in milliseconds.
        duration_ms: u64,
    },
    /// Verification began.
    #[serde(rename = "verify.start")]
    VerifyStart,
    /// One byte range was verified against the outboard tree.
    #[serde(rename = "verify.range")]
    VerifyRange {
        /// The first byte of the range.
        start: u64,
        /// One past the last byte of the range.
        end: u64,
    },
    /// Verification found bytes that do not match.
    #[serde(rename = "verify.mismatch")]
    VerifyMismatch {
        /// What did not match.
        error: Error,
    },
    /// Verification finished.
    #[serde(rename = "verify.end")]
    VerifyEnd {
        /// How many bytes were verified.
        bytes: u64,
        /// How long verification took, in milliseconds.
        duration_ms: u64,
    },
    /// Extraction began.
    #[serde(rename = "extract.start")]
    ExtractStart,
    /// One archive entry was rejected.
    #[serde(rename = "extract.reject")]
    ExtractReject {
        /// The entry path, exactly as the archive wrote it.
        path: String,
        /// Why the entry was rejected.
        error: Error,
    },
    /// Extraction finished.
    #[serde(rename = "extract.end")]
    ExtractEnd {
        /// How many entries were written to staging.
        entries: u64,
        /// How many bytes were written to staging.
        bytes: u64,
        /// How long extraction took, in milliseconds.
        duration_ms: u64,
    },
    /// Staging was published.
    #[serde(rename = "publish.commit")]
    PublishCommit,
    /// One destination entry was reconciled.
    #[serde(rename = "reconcile.outcome")]
    ReconcileOutcomeReached {
        /// The entry reconciled.
        path: String,
        /// What was found.
        outcome: ReconcileOutcome,
    },
    /// Something was lower than requested.
    #[serde(rename = "degrade")]
    Degrade {
        /// What was asked for.
        requested: String,
        /// What was used instead.
        used: String,
        /// Why the substitution happened.
        reason: String,
    },
    /// A failure occurred.
    #[serde(rename = "error")]
    Failure {
        /// The failure.
        error: Error,
    },
}

impl EventPayload {
    /// Returns the name this payload is written under.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::RunStart => "run.start",
            Self::RunEnd { .. } => "run.end",
            Self::ResolveStart => "resolve.start",
            Self::ResolveAlias { .. } => "resolve.alias",
            Self::ResolveEnd { .. } => "resolve.end",
            Self::PlanReady => "plan.ready",
            Self::CacheHit { .. } => "cache.hit",
            Self::CacheMiss { .. } => "cache.miss",
            Self::CacheWait { .. } => "cache.wait",
            Self::CredentialRequired { .. } => "credential.required",
            Self::CredentialOffer { .. } => "credential.offer",
            Self::CredentialDeclined { .. } => "credential.declined",
            Self::ListingStart { .. } => "listing.start",
            Self::ListingSkipped { .. } => "listing.skipped",
            Self::ListingEnd { .. } => "listing.end",
            Self::SourceProbe { .. } => "source.probe",
            Self::SourceSelected { .. } => "source.selected",
            Self::SourceFailover { .. } => "source.failover",
            Self::TransferStart { .. } => "transfer.start",
            Self::TransferProgress { .. } => "transfer.progress",
            Self::TransferRetry { .. } => "transfer.retry",
            Self::TransferResume { .. } => "transfer.resume",
            Self::TransferEnd { .. } => "transfer.end",
            Self::VerifyStart => "verify.start",
            Self::VerifyRange { .. } => "verify.range",
            Self::VerifyMismatch { .. } => "verify.mismatch",
            Self::VerifyEnd { .. } => "verify.end",
            Self::ExtractStart => "extract.start",
            Self::ExtractReject { .. } => "extract.reject",
            Self::ExtractEnd { .. } => "extract.end",
            Self::PublishCommit => "publish.commit",
            Self::ReconcileOutcomeReached { .. } => "reconcile.outcome",
            Self::Degrade { .. } => "degrade",
            Self::Failure { .. } => "error",
        }
    }
}

/// The source of the monotonic sequence number every event carries.
#[derive(Debug, Default)]
pub struct Sequence(AtomicU64);

impl Sequence {
    /// Starts a sequence at zero.
    #[must_use]
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Takes the next number in the sequence.
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

/// One line of the event stream.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    seq: u64,
    timestamp: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    dataset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
    #[serde(flatten)]
    payload: EventPayload,
}

impl Event {
    /// Stamps a payload with its place in the sequence and the wall clock.
    #[must_use]
    pub fn new(sequence: &Sequence, payload: EventPayload) -> Self {
        Self {
            seq: sequence.next(),
            timestamp: Timestamp::now(),
            dataset: None,
            artifact: None,
            payload,
        }
    }

    /// Records the dataset this event belongs to.
    #[must_use]
    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = Some(dataset.into());
        self
    }

    /// Records the artifact this event belongs to.
    #[must_use]
    pub fn with_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.artifact = Some(artifact.into());
        self
    }

    /// Returns this event's place in the sequence.
    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Returns when this event was stamped.
    #[must_use]
    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    /// Returns the dataset this event belongs to, when one is known.
    #[must_use]
    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    /// Returns the artifact this event belongs to, when one is known.
    #[must_use]
    pub fn artifact(&self) -> Option<&str> {
        self.artifact.as_deref()
    }

    /// Returns what this event says.
    #[must_use]
    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    /// Returns the name this event is written under.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.payload.name()
    }
}
