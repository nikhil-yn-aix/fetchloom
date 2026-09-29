//! Error kinds: the stable names every error carries, and the exit code each maps to.

use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

macro_rules! kinds {
    ($($variant:ident = $name:literal => $code:literal, $doc:literal;)*) => {
        /// The kind of an error: a stable dotted name, shown in messages and JSON, and an exit code.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
        pub enum ErrorKind {
            $(
                #[doc = $doc]
                $variant,
            )*
        }

        impl ErrorKind {
            /// Every kind, in the order of the exit code table.
            pub const ALL: &[Self] = &[$(Self::$variant),*];

            /// The dotted name, such as `selection.empty`.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// The exit code a command ends with when this kind stops it.
            #[must_use]
            pub const fn exit_code(self) -> u8 {
                match self {
                    $(Self::$variant => $code,)*
                }
            }
        }
    };
}

kinds! {
    LockStale = "lock.stale" => 1, "`data.lock` no longer matches `data.toml`.";
    StatusDirty = "status.dirty" => 1, "Files under the data directory differ from the lock.";
    Usage = "usage" => 2, "The command line is wrong.";
    ReferenceUnresolved = "reference.unresolved" => 10, "A reference names nothing a source knows.";
    ReferenceInvalid = "reference.invalid" => 10, "A reference is not valid reference syntax.";
    ReferenceAmbiguous = "reference.ambiguous" => 11, "A name matched several records.";
    SelectionEmpty = "selection.empty" => 12, "A filter pattern matched no file.";
    OfflineMissing = "offline.missing" => 20, "Offline, and a needed object is in no local tier.";
    IntegrityDrift = "integrity.drift" => 30, "A source now serves other bytes than the lock records.";
    IntegrityMismatch = "integrity.mismatch" => 30, "Received bytes do not match their stated hash.";
    ArchiveUnsafePath = "archive.unsafe_path" => 30, "An archive member path leaves the dataset.";
    ArchiveLinkEscape = "archive.link_escape" => 30, "An archive link points outside the dataset.";
    ArchiveBomb = "archive.bomb" => 30, "An archive expands past its limits.";
    ArchiveCollision = "archive.collision" => 30, "Two archive members land on one path.";
    ArchiveInconsistent = "archive.inconsistent" => 30, "An archive contradicts itself.";
    ArchiveUnsupported = "archive.unsupported" => 30, "An archive format or feature is not supported.";
    AuthRequired = "auth.required" => 40, "A source needs credentials that are not configured.";
    AuthRejected = "auth.rejected" => 40, "A source refused the configured credentials.";
    TermsRequired = "terms.required" => 40, "A source needs its terms accepted first.";
    LocalUnrepresentable = "local.unrepresentable" => 50, "A path cannot exist on this filesystem.";
    ProjectInvalidManifest = "project.invalid_manifest" => 50, "`data.toml` cannot be read.";
    ProjectInvalidLock = "project.invalid_lock" => 50, "`data.lock` cannot be read.";
    StepFailed = "step.failed" => 60, "A step's command failed.";
    PluginCrashed = "plugin.crashed" => 70, "A plugin process exited unexpectedly.";
    PluginProtocol = "plugin.protocol" => 70, "A plugin broke the plugin protocol.";
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ErrorKind {
    type Err = UnknownKind;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.name() == text)
            .ok_or_else(|| UnknownKind(text.to_owned()))
    }
}

impl Serialize for ErrorKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// Text that names no error kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not an error kind")]
pub struct UnknownKind(String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Unmatched;
    use crate::lock::Lock;
    use crate::manifest::Manifest;
    use crate::reference::Reference;

    const TABLE: &[(&str, u8)] = &[
        ("lock.stale", 1),
        ("status.dirty", 1),
        ("usage", 2),
        ("reference.unresolved", 10),
        ("reference.invalid", 10),
        ("reference.ambiguous", 11),
        ("selection.empty", 12),
        ("offline.missing", 20),
        ("integrity.drift", 30),
        ("integrity.mismatch", 30),
        ("archive.unsafe_path", 30),
        ("archive.link_escape", 30),
        ("archive.bomb", 30),
        ("archive.collision", 30),
        ("archive.inconsistent", 30),
        ("archive.unsupported", 30),
        ("auth.required", 40),
        ("auth.rejected", 40),
        ("terms.required", 40),
        ("local.unrepresentable", 50),
        ("project.invalid_manifest", 50),
        ("project.invalid_lock", 50),
        ("step.failed", 60),
        ("plugin.crashed", 70),
        ("plugin.protocol", 70),
    ];

    #[test]
    fn every_kind_has_the_documented_name_and_exit_code() {
        let actual: Vec<(&str, u8)> = ErrorKind::ALL
            .iter()
            .map(|kind| (kind.name(), kind.exit_code()))
            .collect();
        assert_eq!(actual, TABLE);
    }

    #[test]
    fn names_parse_back_and_display() {
        for kind in ErrorKind::ALL {
            assert_eq!(kind.name().parse::<ErrorKind>(), Ok(*kind));
            assert_eq!(kind.to_string(), kind.name());
        }
        assert_eq!(
            "network".parse::<ErrorKind>().unwrap_err().to_string(),
            "`network` is not an error kind"
        );
    }

    #[test]
    fn model_errors_report_their_kind() {
        let reference = "".parse::<Reference>().unwrap_err();
        assert_eq!(reference.kind(), ErrorKind::ReferenceInvalid);
        let manifest = Manifest::from_toml("x").unwrap_err();
        assert_eq!(manifest.kind(), ErrorKind::ProjectInvalidManifest);
        let lock = Lock::from_toml("").unwrap_err();
        assert_eq!(lock.kind(), ErrorKind::ProjectInvalidLock);
        let unmatched = Unmatched {
            patterns: vec!["*.edf".to_owned()],
            considered: 0,
            examples: Vec::new(),
        };
        assert_eq!(unmatched.kind(), ErrorKind::SelectionEmpty);
    }

    #[test]
    fn serializes_as_its_name() {
        let json = serde_json::to_string(&ErrorKind::SelectionEmpty).unwrap();
        assert_eq!(json, "\"selection.empty\"");
    }
}
