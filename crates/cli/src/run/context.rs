//! What a materialization is given, and what it reports when it is done.

use super::adapters::Tuning;
use fetchloom_cache::Cache;
use fetchloom_engine::digest::{ContentDigest, TreeDigest};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::work::{Work, WorkCounter};
use fetchloom_platform::NativePlatform;
use std::path::PathBuf;
use std::sync::Arc;

/// The one object a run resolved, when it resolved one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedArtifact {
    /// The name the artifact is recorded under.
    pub id: String,
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes, when the run learned it.
    pub interop: Option<fetchloom_engine::digest::InteropDigest>,
    /// The length of the object in bytes.
    pub size: u64,
    /// Where the bytes came from, redacted as it was recorded.
    pub source: SafeUrl,
    /// The digest supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
}

/// What a completed run reports.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunResult {
    /// What the run did.
    pub status: RunStatus,
    /// The dataset the run materialized.
    pub dataset: String,
    /// The tree the run produced.
    pub tree: TreeDigest,
    /// Where the tree was materialized.
    pub destination: PathBuf,
    /// How many entries the tree holds.
    pub entries: u64,
    /// How many bytes those entries hold.
    pub bytes: u64,
    /// What the run read, wrote, and asked for.
    pub work: Work,
    /// What the run may claim about the bytes it produced.
    pub trust: TrustClass,
    /// The entry paths the run materialized with the executable mode, in
    /// ascending order. Never part of the machine-readable result.
    #[serde(skip)]
    pub executable: Vec<String>,
    /// The object the run resolved, when it resolved one.
    #[serde(skip)]
    pub artifact: Option<RecordedArtifact>,
}

/// Everything a materialization runs against.
#[derive(Clone, Copy)]
pub struct Materialization<'a> {
    /// The pool the digests are computed on.
    pub processor: &'a Processor,
    /// The platform the filesystem work goes through.
    pub platform: &'a NativePlatform,
    /// How far a write is pushed before publication.
    pub durability: DurabilityTier,
    /// The cache to read and write, when the run has one.
    pub cache: Option<&'a Cache<NativePlatform>>,
    /// Where the run counts the work it did.
    pub work: &'a Arc<WorkCounter>,
    /// Whether a recognized archive is extracted or kept as a file.
    pub extract: bool,
    /// The one buffer every stream this run hashes is read through.
    pub digester: &'a std::sync::Mutex<fetchloom_engine::hashing::Digester>,
    /// What a destination entry and a cache hit are both checked against before
    /// they are reused.
    pub verify: fetchloom_engine::verification::VerificationPolicy,
    /// What bounds the run's transfers, and whether they may move.
    pub tuning: &'a Tuning,
    /// What this run is allowed to do, including which credential it may send.
    pub policy: &'a dyn Policy,
    /// Every adapter this run may dispatch a reference to.
    pub adapters: &'a Adapters,
}

/// What one transfer of an artifact produced.
pub struct Moved {
    /// The digest the bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes.
    pub interop: fetchloom_engine::digest::InteropDigest,
    /// The length of the object in bytes.
    pub size: u64,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
    /// The source this run took and why, when it selected one.
    pub chosen: Option<fetchloom_engine::transfer::Chosen>,
}
