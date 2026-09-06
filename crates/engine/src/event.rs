//! The event stream every display mode and every log is drawn from.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::digest::ContentDigest;
use crate::error::Error;
use crate::reconcile::ReconcileOutcome;
use crate::redact::SafeUrl;
use crate::resume::ResumeRung;
use crate::timestamp::Timestamp;

pub const EVENT_NAMES: [&str; 35] = [
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
    "merge.resolution",
    "degrade",
    "error",
];

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
#[serde(tag = "event")]
pub enum EventPayload {
    #[serde(rename = "run.start")]
    RunStart,
    #[serde(rename = "run.end")]
    RunEnd { duration_ms: u64 },
    #[serde(rename = "resolve.start")]
    ResolveStart,
    #[serde(rename = "resolve.alias")]
    ResolveAlias { from: String, to: String },
    #[serde(rename = "resolve.end")]
    ResolveEnd { duration_ms: u64 },
    #[serde(rename = "plan.ready")]
    PlanReady,
    #[serde(rename = "cache.hit")]
    CacheHit { digest: ContentDigest },
    #[serde(rename = "cache.miss")]
    CacheMiss { digest: ContentDigest },
    #[serde(rename = "cache.wait")]
    CacheWait { digest: ContentDigest },
    #[serde(rename = "credential.required")]
    CredentialRequired { provider: String },
    #[serde(rename = "credential.offer")]
    CredentialOffer { provider: String },
    #[serde(rename = "credential.declined")]
    CredentialDeclined { provider: String },
    #[serde(rename = "listing.start")]
    ListingStart { source: SafeUrl },
    #[serde(rename = "listing.skipped")]
    ListingSkipped { count: u64 },
    #[serde(rename = "listing.end")]
    ListingEnd { entries: u64, duration_ms: u64 },
    #[serde(rename = "source.probe")]
    SourceProbe { source: SafeUrl },
    #[serde(rename = "source.selected")]
    SourceSelected { source: SafeUrl, reason: String },
    #[serde(rename = "source.failover")]
    SourceFailover {
        from: SafeUrl,
        to: SafeUrl,
        reason: String,
    },
    #[serde(rename = "transfer.start")]
    TransferStart {
        source: SafeUrl,
        host: crate::reference::Host,
        expected_bytes: Option<u64>,
    },
    #[serde(rename = "transfer.progress")]
    TransferProgress { bytes: u64 },
    #[serde(rename = "transfer.retry")]
    TransferRetry {
        host: crate::reference::Host,
        attempt: u32,
        reason: String,
    },
    #[serde(rename = "transfer.resume")]
    TransferResume { rung: ResumeRung, bytes_kept: u64 },
    #[serde(rename = "transfer.end")]
    TransferEnd {
        host: crate::reference::Host,
        bytes: u64,
        duration_ms: u64,
    },
    #[serde(rename = "verify.start")]
    VerifyStart,
    #[serde(rename = "verify.range")]
    VerifyRange { start: u64, end: u64 },
    #[serde(rename = "verify.mismatch")]
    VerifyMismatch { error: Error },
    #[serde(rename = "verify.end")]
    VerifyEnd { bytes: u64, duration_ms: u64 },
    #[serde(rename = "extract.start")]
    ExtractStart,
    #[serde(rename = "extract.reject")]
    ExtractReject { path: String, error: Error },
    #[serde(rename = "extract.end")]
    ExtractEnd {
        entries: u64,
        bytes: u64,
        duration_ms: u64,
    },
    #[serde(rename = "publish.commit")]
    PublishCommit,
    #[serde(rename = "reconcile.outcome")]
    ReconcileOutcomeReached {
        path: String,
        outcome: ReconcileOutcome,
    },
    #[serde(rename = "merge.resolution")]
    MergeResolutionReached {
        path: String,
        resolution: crate::merge::Resolution,
    },
    #[serde(rename = "degrade")]
    Degrade {
        requested: String,
        used: String,
        reason: String,
    },
    #[serde(rename = "error")]
    Failure { error: Error },
}

impl EventPayload {
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
            Self::MergeResolutionReached { .. } => "merge.resolution",
            Self::Degrade { .. } => "degrade",
            Self::Failure { .. } => "error",
        }
    }
}

#[derive(Debug, Default)]
pub struct Sequence(AtomicU64);

impl Sequence {
    #[must_use]
    pub fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Span(std::time::Instant);

impl Span {
    #[must_use]
    pub fn start() -> Self {
        Self(std::time::Instant::now())
    }

    #[must_use]
    pub fn elapsed_ms(self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Default for Span {
    fn default() -> Self {
        Self::start()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
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

    #[must_use]
    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = Some(dataset.into());
        self
    }

    #[must_use]
    pub fn with_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.artifact = Some(artifact.into());
        self
    }

    #[must_use]
    pub fn seq(&self) -> u64 {
        self.seq
    }

    #[must_use]
    pub fn timestamp(&self) -> Timestamp {
        self.timestamp
    }

    #[must_use]
    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    #[must_use]
    pub fn artifact(&self) -> Option<&str> {
        self.artifact.as_deref()
    }

    #[must_use]
    pub fn payload(&self) -> &EventPayload {
        &self.payload
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        self.payload.name()
    }
}
