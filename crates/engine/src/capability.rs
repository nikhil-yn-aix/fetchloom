//! What a volume and a processor can actually do, detected and never assumed.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseFolding {
    Sensitive,
    Folding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    Sensitive,
    InsensitivePreserving,
    Normalizing,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backing {
    Local,
    Network,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scanner {
    Absent,
    Present { name: String, cost_ratio: f64 },
    Unknown { cost_ratio: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent capabilities the contract requires reported separately"
)]
pub struct VolumeCapabilities {
    pub case_folding: CaseFolding,
    pub normalization: Normalization,
    pub clone: bool,
    pub sparse: bool,
    pub symlink: bool,
    pub hard_link: bool,
    pub max_component_length: u32,
    pub max_path_length: u32,
    pub backing: Backing,
    pub scanner: Scanner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorLevel {
    Portable,
    Sse41,
    Avx2,
    Avx512,
    Neon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteropAcceleration {
    Usable,
    Absent,
    Undetectable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessorCapabilities {
    pub budget: crate::threads::ThreadBudget,
    pub vector_level: VectorLevel,
    pub interop_acceleration: InteropAcceleration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMechanism {
    Clone,
    Copy,
}
