//! What a materialization is given, and what it reports when it is done.

use super::adapters::Tuning;
use fetchloom_cache::Cache;
use fetchloom_engine::adapters::Adapters;
use fetchloom_engine::digest::{ContentDigest, TreeDigest};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::tree::TreeEntry;
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::work::{Work, WorkCounter};
use fetchloom_platform::NativePlatform;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedArtifact {
    pub(crate) id: String,
    pub(crate) digest: ContentDigest,
    pub(crate) interop: Option<fetchloom_engine::digest::InteropDigest>,
    pub(crate) size: u64,
    pub(crate) source: SafeUrl,
    pub(crate) prior: Option<ContentDigest>,
    pub(crate) observed: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub(crate) dataset: String,
    pub tree: TreeDigest,
    pub(crate) destination: PathBuf,
    pub entries: u64,
    pub bytes: u64,
    pub(crate) work: Work,
    pub(crate) trust: TrustClass,
    #[serde(skip)]
    pub(crate) recorded: Vec<TreeEntry>,
    #[serde(skip)]
    pub(crate) conflicts: Vec<String>,
    #[serde(skip)]
    pub(crate) upstream: Vec<TreeEntry>,
    #[serde(skip)]
    pub(crate) artifact: Option<RecordedArtifact>,
}

#[derive(Clone, Copy)]
pub struct Materialization<'a> {
    pub processor: &'a Processor,
    pub platform: &'a NativePlatform,
    pub durability: DurabilityTier,
    pub cache: Option<&'a Cache<NativePlatform>>,
    pub work: &'a Arc<WorkCounter>,
    pub extract: bool,
    pub digester: &'a std::sync::Mutex<fetchloom_engine::hashing::Digester>,
    pub verify: fetchloom_engine::verification::VerificationPolicy,
    pub tuning: &'a Tuning,
    pub policy: &'a dyn Policy,
    pub adapters: &'a Adapters,
}

pub struct Moved {
    pub digest: ContentDigest,
    pub interop: fetchloom_engine::digest::InteropDigest,
    pub size: u64,
    pub observed: Option<String>,
    pub chosen: Option<fetchloom_engine::transfer::Chosen>,
}
