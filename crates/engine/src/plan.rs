//! A resolved run, written to a file and executable offline.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::digest::ContentDigest;
use crate::redact::SafeUrl;
use crate::reference::Host;
use crate::trust::TrustClass;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanNetwork {
    pub hosts: Vec<Host>,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanArtifact {
    pub id: String,
    pub digest: ContentDigest,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<u64>,
    pub cached: bool,
    pub source: SafeUrl,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<crate::selection::Glob>,
    #[serde(default)]
    pub layout: crate::selection::Layout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<crate::seam::source::Cost>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeRequirement {
    pub volume: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDisk {
    pub partial: VolumeRequirement,
    pub cache: VolumeRequirement,
    pub staging: VolumeRequirement,
    pub destination: VolumeRequirement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub dataset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    pub network: PlanNetwork,
    pub artifacts: Vec<PlanArtifact>,
    pub trust: TrustClass,
    pub credentials: Vec<String>,
    pub terms: Vec<String>,
    pub disk: PlanDisk,
    pub destination: PathBuf,
    pub conflicts: Vec<String>,
    pub unknown: Vec<String>,
}

impl Plan {
    /// # Errors
    /// `manifest.invalid` when the plan cannot be written as a document.
    pub fn render(&self) -> Result<String, crate::error::Error> {
        crate::document::render_model(self)
    }

    /// # Errors
    /// `manifest.invalid` when the document does not parse as a plan, holds an
    /// unknown key, or is longer than the limit allows.
    pub fn parse(
        bytes: &[u8],
        syntax: crate::document::Syntax,
        limits: &crate::limits::Limits,
    ) -> Result<Self, crate::error::Error> {
        crate::document::read_model_in(
            bytes,
            syntax,
            "plan",
            limits,
            crate::document::Bound::Foreign,
        )
    }
}
