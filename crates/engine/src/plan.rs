//! A resolved run, written to a file and executable offline.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::digest::ContentDigest;
use crate::redact::SafeUrl;
use crate::reference::Host;
use crate::trust::TrustClass;

/// What a plan needs from the network.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanNetwork {
    /// The hosts the plan will reach.
    pub hosts: Vec<Host>,
    /// Whether the plan can be executed without reaching any of them.
    pub required: bool,
}

/// One artifact as a plan resolves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanArtifact {
    /// The name the manifest gave this artifact.
    pub id: String,
    /// The content digest the bytes must have.
    pub digest: ContentDigest,
    /// The length of the artifact in bytes.
    pub size: u64,
    /// The length the artifact expands to, when the source states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded: Option<u64>,
    /// Whether the cache already holds the bytes.
    pub cached: bool,
    /// The source the plan chose, redacted when it was recorded.
    pub source: SafeUrl,
    /// The member paths the plan covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<crate::selection::Glob>,
    /// How member paths are rewritten.
    #[serde(default)]
    pub layout: crate::selection::Layout,
    /// What the source said about who is billed for these bytes, absent when it
    /// said nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<crate::seam::source::Cost>,
}

/// How much space one requirement needs, and on which volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeRequirement {
    /// The volume the requirement lands on.
    pub volume: String,
    /// The bytes required on that volume, absent when no source stated what
    /// the requirement depends on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The four space requirements, each attributed to its volume.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanDisk {
    /// Space for in-progress transfers.
    pub partial: VolumeRequirement,
    /// Space for completed cache objects.
    pub cache: VolumeRequirement,
    /// Space for extraction trees not yet published.
    pub staging: VolumeRequirement,
    /// Space for the destination itself.
    pub destination: VolumeRequirement,
}

/// A resolved run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// The dataset the plan resolves.
    pub dataset: String,
    /// The release the plan resolved at, when the manifest names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// What the plan needs from the network.
    pub network: PlanNetwork,
    /// The artifacts, in manifest order.
    pub artifacts: Vec<PlanArtifact>,
    /// What will be known about the bytes.
    pub trust: TrustClass,
    /// The providers whose credentials the plan involves.
    pub credentials: Vec<String>,
    /// The terms the plan requires acceptance of.
    pub terms: Vec<String>,
    /// The four space requirements.
    pub disk: PlanDisk,
    /// Where the plan will materialize.
    pub destination: PathBuf,
    /// The destination entries that stand in the way.
    pub conflicts: Vec<String>,
    /// Every field the source could not supply.
    pub unknown: Vec<String>,
}

impl Plan {
    /// Renders this plan as the canonical text it is written in.
    ///
    /// # Errors
    ///
    /// Fails when the model cannot be written.
    pub fn render(&self) -> Result<String, crate::error::Error> {
        crate::document::render_model(self)
    }

    /// Reads a plan from a document written in any accepted syntax.
    ///
    /// # Errors
    ///
    /// Fails with `manifest.invalid` when the document does not parse or holds
    /// a key this build does not read.
    pub fn parse(
        bytes: &[u8],
        syntax: crate::document::Syntax,
        limits: &crate::limits::Limits,
    ) -> Result<Self, crate::error::Error> {
        crate::document::read_model_in(bytes, syntax, "plan", limits)
    }
}
