//! What a volume and a processor can actually do, detected and never assumed.

use serde::Serialize;

/// Whether a volume folds names that differ only in case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseFolding {
    /// Two names differing only in case are two names.
    Sensitive,
    /// Two names differing only in case are one name.
    Folding,
}

/// How a volume treats two spellings of one Unicode string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    /// Two spellings are two names.
    Sensitive,
    /// Two spellings are one name, and the bytes written are the bytes stored.
    InsensitivePreserving,
    /// Two spellings are one name, and the bytes stored are a normalized form.
    Normalizing,
    /// The volume refused the name the probe measures with.
    Unknown,
}

/// What a volume's storage sits behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backing {
    /// The volume is local to this machine.
    Local,
    /// The volume is reached over a network protocol.
    Network,
    /// The volume's backing could not be determined.
    Unknown,
}

/// Whether an on-access scanner inspects writes, and what it costs.
///
/// Presence and absence are only ever reported where the platform can
/// enumerate what inspects a write. Where it cannot, the measured cost is
/// reported with the answer left unknown.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scanner {
    /// Nothing is inspecting writes on this volume.
    Absent,
    /// A named product is inspecting writes on this volume.
    Present {
        /// The product's name.
        name: String,
        /// How many times longer many small writes took than one large write.
        cost_ratio: f64,
    },
    /// The platform cannot say what inspects writes, so only the cost is known.
    Unknown {
        /// How many times longer many small writes took than one large write.
        cost_ratio: f64,
    },
}

/// Everything detected about one volume.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent capabilities the contract requires reported separately"
)]
pub struct VolumeCapabilities {
    /// Whether the volume folds case.
    pub case_folding: CaseFolding,
    /// How the volume treats Unicode spellings.
    pub normalization: Normalization,
    /// Whether copy-on-write cloning works on this volume.
    pub clone: bool,
    /// Whether sparse files work on this volume.
    pub sparse: bool,
    /// Whether this process may create a symbolic link on this volume.
    pub symlink: bool,
    /// Whether hard links work on this volume.
    pub hard_link: bool,
    /// The longest single path component the volume accepts.
    pub max_component_length: u32,
    /// The longest whole path the volume accepts.
    pub max_path_length: u32,
    /// What the volume's storage sits behind.
    pub backing: Backing,
    /// Whether an on-access scanner inspects writes.
    pub scanner: Scanner,
}

/// The vector instruction level selected at runtime for the content digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorLevel {
    /// No vector implementation was selected.
    Portable,
    /// The one hundred and twenty-eight bit implementation on this processor.
    Sse41,
    /// The two hundred and fifty-six bit implementation on this processor.
    Avx2,
    /// The five hundred and twelve bit implementation on this processor.
    Avx512,
    /// The vector implementation on this processor's architecture.
    Neon,
}

/// Whether the interop digest can use a hardware instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InteropAcceleration {
    /// The instruction is present and the implementation uses it.
    Usable,
    /// The processor does not have the instruction.
    Absent,
    /// The instruction may be present and cannot be detected on this target.
    Undetectable,
}

/// Everything detected about the processor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessorCapabilities {
    /// The threads processor work may use, after every limit.
    pub budget: crate::threads::ThreadBudget,
    /// The vector level selected for the content digest.
    pub vector_level: VectorLevel,
    /// Whether the interop digest can use a hardware instruction.
    pub interop_acceleration: InteropAcceleration,
}

/// How one file's bytes were placed at a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMechanism {
    /// The filesystem shared the blocks.
    Clone,
    /// The bytes were written again.
    Copy,
}
