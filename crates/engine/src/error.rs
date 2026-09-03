//! The one error type, its kinds, and the layer each kind belongs to.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::redact::SafeUrl;

/// The stage of a run an error belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Turning a reference into a manifest and its artifacts.
    Resolve,
    /// Moving bytes from a source.
    Transfer,
    /// Comparing bytes against a digest.
    Verify,
    /// Reading an archive into staging.
    Extract,
    /// Publishing staging onto a destination.
    Materialize,
    /// Reading from or writing to the cache.
    Cache,
    /// Refusing a run on policy.
    Policy,
    /// Running out of disk or exceeding a limit.
    Resource,
}

/// Every failure Fetchloom can report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "&'static str")]
pub enum ErrorKind {
    /// A reference named nothing Fetchloom could resolve.
    ReferenceUnresolved,
    /// A manifest did not parse or broke a manifest rule.
    ManifestInvalid,
    /// An alias moved when a run required it to be stable.
    AliasUnstable,
    /// A connection went idle past the timeout.
    NetworkTimeout,
    /// A connection was refused.
    NetworkRefused,
    /// A source answered with a status that ends the attempt.
    NetworkStatus,
    /// A transport layer security handshake or check failed.
    NetworkTls,
    /// A source cannot serve the ranges a resume needs.
    SourceUnsupportedRange,
    /// A source's identity for an object changed during a transfer.
    SourceIdentityChanged,
    /// Bytes did not match the digest they were expected to have.
    IntegrityMismatch,
    /// A source delivered fewer bytes than the object has.
    IntegrityTruncated,
    /// A byte range did not match the outboard tree.
    IntegrityRangeMismatch,
    /// An archive entry named a path outside the destination.
    ArchiveUnsafePath,
    /// An archive link resolved outside the destination.
    ArchiveLinkEscape,
    /// Two entries collide under the target filesystem's own folding rules.
    ArchiveCollision,
    /// An archive expanded past a limit.
    ArchiveBomb,
    /// An archive format or feature Fetchloom does not read.
    ArchiveUnsupported,
    /// A destination entry differs from the receipt.
    DestinationModified,
    /// A destination entry is present and not in the receipt.
    DestinationForeign,
    /// An entry cannot be named on the target filesystem.
    DestinationUnrepresentable,
    /// Staging and the destination are on different volumes.
    DestinationCrossVolume,
    /// Another writer holds the lock for this digest.
    CacheLocked,
    /// A cache file does not match what the cache recorded about it.
    CacheCorrupt,
    /// The cache format fingerprint is not the running build's.
    CacheFormatMismatch,
    /// Two cache directories are on different volumes.
    CacheCrossVolume,
    /// A volume cannot express the advisory locking a shared cache needs.
    CacheLockingUnsupported,
    /// A run needed the network and the network is forbidden.
    PolicyOffline,
    /// No reachable source can serve the request without a credential.
    PolicyCredentialMissing,
    /// A credential is expired, revoked, or too narrowly scoped.
    PolicyCredentialInvalid,
    /// A manifest requires acceptance that has not been asserted.
    PolicyTermsRequired,
    /// A trust class was weaker than the run allows.
    PolicyTrustRefused,
    /// A volume does not have room for what the run requires.
    ResourceDisk,
    /// A run exceeded a configured limit.
    ResourceLimit,
}

impl ErrorKind {
    /// Returns the name this kind is written with.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ReferenceUnresolved => "reference.unresolved",
            Self::ManifestInvalid => "manifest.invalid",
            Self::AliasUnstable => "alias.unstable",
            Self::NetworkTimeout => "network.timeout",
            Self::NetworkRefused => "network.refused",
            Self::NetworkStatus => "network.status",
            Self::NetworkTls => "network.tls",
            Self::SourceUnsupportedRange => "source.unsupported_range",
            Self::SourceIdentityChanged => "source.identity_changed",
            Self::IntegrityMismatch => "integrity.mismatch",
            Self::IntegrityTruncated => "integrity.truncated",
            Self::IntegrityRangeMismatch => "integrity.range_mismatch",
            Self::ArchiveUnsafePath => "archive.unsafe_path",
            Self::ArchiveLinkEscape => "archive.link_escape",
            Self::ArchiveCollision => "archive.collision",
            Self::ArchiveBomb => "archive.bomb",
            Self::ArchiveUnsupported => "archive.unsupported",
            Self::DestinationModified => "destination.modified",
            Self::DestinationForeign => "destination.foreign",
            Self::DestinationUnrepresentable => "destination.unrepresentable",
            Self::DestinationCrossVolume => "destination.cross_volume",
            Self::CacheLocked => "cache.locked",
            Self::CacheCorrupt => "cache.corrupt",
            Self::CacheFormatMismatch => "cache.format_mismatch",
            Self::CacheCrossVolume => "cache.cross_volume",
            Self::CacheLockingUnsupported => "cache.locking_unsupported",
            Self::PolicyOffline => "policy.offline",
            Self::PolicyCredentialMissing => "policy.credential_missing",
            Self::PolicyCredentialInvalid => "policy.credential_invalid",
            Self::PolicyTermsRequired => "policy.terms_required",
            Self::PolicyTrustRefused => "policy.trust_refused",
            Self::ResourceDisk => "resource.disk",
            Self::ResourceLimit => "resource.limit",
        }
    }

    /// Returns the layer this kind belongs to.
    #[must_use]
    pub fn layer(self) -> Layer {
        match self {
            Self::ReferenceUnresolved | Self::ManifestInvalid | Self::AliasUnstable => {
                Layer::Resolve
            }
            Self::NetworkTimeout
            | Self::NetworkRefused
            | Self::NetworkStatus
            | Self::NetworkTls
            | Self::SourceUnsupportedRange
            | Self::SourceIdentityChanged => Layer::Transfer,
            Self::IntegrityMismatch | Self::IntegrityTruncated | Self::IntegrityRangeMismatch => {
                Layer::Verify
            }
            Self::ArchiveUnsafePath
            | Self::ArchiveLinkEscape
            | Self::ArchiveCollision
            | Self::ArchiveBomb
            | Self::ArchiveUnsupported => Layer::Extract,
            Self::DestinationModified
            | Self::DestinationForeign
            | Self::DestinationUnrepresentable
            | Self::DestinationCrossVolume => Layer::Materialize,
            Self::CacheLocked
            | Self::CacheCorrupt
            | Self::CacheFormatMismatch
            | Self::CacheCrossVolume
            | Self::CacheLockingUnsupported => Layer::Cache,
            Self::PolicyOffline
            | Self::PolicyCredentialMissing
            | Self::PolicyCredentialInvalid
            | Self::PolicyTermsRequired
            | Self::PolicyTrustRefused => Layer::Policy,
            Self::ResourceDisk | Self::ResourceLimit => Layer::Resource,
        }
    }

    /// Every kind, in the order the contract lists them.
    pub const ALL: [Self; 33] = [
        Self::ReferenceUnresolved,
        Self::ManifestInvalid,
        Self::AliasUnstable,
        Self::NetworkTimeout,
        Self::NetworkRefused,
        Self::NetworkStatus,
        Self::NetworkTls,
        Self::SourceUnsupportedRange,
        Self::SourceIdentityChanged,
        Self::IntegrityMismatch,
        Self::IntegrityTruncated,
        Self::IntegrityRangeMismatch,
        Self::ArchiveUnsafePath,
        Self::ArchiveLinkEscape,
        Self::ArchiveCollision,
        Self::ArchiveBomb,
        Self::ArchiveUnsupported,
        Self::DestinationModified,
        Self::DestinationForeign,
        Self::DestinationUnrepresentable,
        Self::DestinationCrossVolume,
        Self::CacheLocked,
        Self::CacheCorrupt,
        Self::CacheFormatMismatch,
        Self::CacheCrossVolume,
        Self::CacheLockingUnsupported,
        Self::PolicyOffline,
        Self::PolicyCredentialMissing,
        Self::PolicyCredentialInvalid,
        Self::PolicyTermsRequired,
        Self::PolicyTrustRefused,
        Self::ResourceDisk,
        Self::ResourceLimit,
    ];
}

impl From<ErrorKind> for &'static str {
    fn from(kind: ErrorKind) -> Self {
        kind.label()
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A failure, carrying every field the contract requires of one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    kind: ErrorKind,
    layer: Layer,
    dataset: Option<String>,
    artifact: Option<String>,
    source: Option<SafeUrl>,
    attempts: u32,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    member: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_seconds: Option<std::num::NonZeroU32>,
    next_action: Box<str>,
}

impl Error {
    /// Builds a failure of one kind with the action the user should take.
    #[must_use]
    pub fn new(kind: ErrorKind, next_action: impl Into<String>) -> Self {
        Self {
            kind,
            layer: kind.layer(),
            dataset: None,
            artifact: None,
            source: None,
            attempts: 0,
            retryable: false,
            member: None,
            retry_after_seconds: None,
            next_action: next_action.into().into_boxed_str(),
        }
    }

    /// Records the dataset this failure belongs to.
    #[must_use]
    pub fn with_dataset(mut self, dataset: impl Into<String>) -> Self {
        self.dataset = Some(dataset.into());
        self
    }

    /// Records the artifact this failure belongs to.
    #[must_use]
    pub fn with_artifact(mut self, artifact: impl Into<String>) -> Self {
        self.artifact = Some(artifact.into());
        self
    }

    /// Records the archive member this failure rejected.
    #[must_use]
    pub fn with_member(mut self, member: &str) -> Self {
        self.member = Some(member.into());
        self
    }

    /// Returns the archive member this failure rejected, when it names one.
    #[must_use]
    pub fn member(&self) -> Option<&str> {
        self.member.as_deref()
    }

    /// Records the source this failure came from, redacting it as it is stored.
    #[must_use]
    pub fn with_source(mut self, location: &str) -> Self {
        self.source = Some(SafeUrl::new(location));
        self
    }

    /// Records how many attempts were made before this failure.
    #[must_use]
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    /// Records whether another attempt could succeed.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Records that the source asked to be left alone, and for how long.
    #[must_use]
    pub fn with_retry_after(mut self, wait: std::time::Duration) -> Self {
        let seconds = u32::try_from(wait.as_secs()).unwrap_or(u32::MAX);
        self.retry_after_seconds =
            Some(std::num::NonZeroU32::new(seconds).unwrap_or(std::num::NonZeroU32::MIN));
        self
    }

    /// Records that the source asked to be left alone without saying for how
    /// long, which is the shortest wait a source can ask for.
    #[must_use]
    pub fn rate_limited(mut self) -> Self {
        self.retry_after_seconds = self.retry_after_seconds.or(Some(std::num::NonZeroU32::MIN));
        self
    }

    /// Returns how long the source asked to be left alone, when it asked.
    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        self.retry_after_seconds
            .map(|seconds| std::time::Duration::from_secs(u64::from(seconds.get())))
    }

    /// Returns the kind of this failure.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns the layer of this failure.
    #[must_use]
    pub fn layer(&self) -> Layer {
        self.layer
    }

    /// Returns the dataset this failure belongs to, when one is known.
    #[must_use]
    pub fn dataset(&self) -> Option<&str> {
        self.dataset.as_deref()
    }

    /// Returns the artifact this failure belongs to, when one is known.
    #[must_use]
    pub fn artifact(&self) -> Option<&str> {
        self.artifact.as_deref()
    }

    /// Returns the redacted source this failure came from, when one is known.
    #[must_use]
    pub fn source_location(&self) -> Option<&SafeUrl> {
        self.source.as_ref()
    }

    /// Returns how many attempts were made before this failure.
    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Returns whether another attempt could succeed.
    #[must_use]
    pub fn retryable(&self) -> bool {
        self.retryable
    }

    /// Returns the action the user should take.
    #[must_use]
    pub fn next_action(&self) -> &str {
        &self.next_action
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.next_action)
    }
}

impl std::error::Error for Error {}

/// Which of the two surfaces a run writes a path belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// A path under the cache root.
    Cache,
    /// A path under a destination the run materializes.
    Destination,
    /// A path a reference names, which the run reads and never writes.
    Source,
}

/// Turns a filesystem failure on one of the two surfaces into the kind that
/// names it.
#[must_use]
pub fn filesystem_failure(
    surface: Surface,
    path: &std::path::Path,
    reason: &std::io::Error,
) -> Error {
    if surface == Surface::Source && reason.kind() == std::io::ErrorKind::NotFound {
        return Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a path that exists, because nothing is at {}",
                path.display()
            ),
        );
    }
    let kind = filesystem_kind(surface, reason);
    let action = if surface == Surface::Source {
        format!("make {} readable: {reason}", path.display())
    } else {
        format!("{}: {reason}", path.display())
    };
    Error::new(kind, action)
}

/// Turns a failure to take an advisory lock into the kind that names it.
#[must_use]
pub fn lock_failure(path: &std::path::Path, reason: &std::io::Error) -> Error {
    if matches!(
        reason.kind(),
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
    ) {
        return filesystem_failure(Surface::Cache, path, reason);
    }
    Error::new(
        ErrorKind::CacheLockingUnsupported,
        format!(
            "put the cache on a volume that supports advisory locking, because {} cannot express one: {reason}",
            path.display()
        ),
    )
}

/// Decides which kind a filesystem failure on one surface is.
///
/// This is the rule, and `filesystem_failure` is the way to reach it with a
/// path. A caller holding an open handle and no path reaches it here rather
/// than deciding for itself.
#[must_use]
pub fn filesystem_kind(surface: Surface, reason: &std::io::Error) -> ErrorKind {
    match (reason.kind(), surface) {
        (std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded, _) => {
            ErrorKind::ResourceDisk
        }
        (std::io::ErrorKind::CrossesDevices, Surface::Cache) => ErrorKind::CacheCrossVolume,
        (std::io::ErrorKind::CrossesDevices, Surface::Destination | Surface::Source) => {
            ErrorKind::DestinationCrossVolume
        }
        (_, Surface::Cache) => ErrorKind::CacheCorrupt,
        (_, Surface::Destination) => ErrorKind::DestinationUnrepresentable,
        (_, Surface::Source) => ErrorKind::ReferenceUnresolved,
    }
}
