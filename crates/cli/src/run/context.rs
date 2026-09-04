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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedArtifact {
    pub id: String,
    pub digest: ContentDigest,
    pub interop: Option<fetchloom_engine::digest::InteropDigest>,
    pub size: u64,
    pub source: SafeUrl,
    pub prior: Option<ContentDigest>,
    pub observed: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RunResult {
    pub status: RunStatus,
    pub dataset: String,
    pub tree: TreeDigest,
    pub destination: PathBuf,
    pub entries: u64,
    pub bytes: u64,
    pub work: Work,
    pub trust: TrustClass,
    #[serde(skip)]
    pub executable: Vec<String>,
    #[serde(skip)]
    pub artifact: Option<RecordedArtifact>,
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
