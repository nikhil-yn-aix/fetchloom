//! The one error type, its kinds, and the layer each kind belongs to.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::redact::SafeUrl;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Resolve,
    Transfer,
    Verify,
    Extract,
    Materialize,
    Cache,
    Policy,
    Resource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "&'static str")]
pub enum ErrorKind {
    ReferenceUnresolved,
    ManifestInvalid,
    AliasUnstable,
    NetworkTimeout,
    NetworkRefused,
    NetworkStatus,
    NetworkTls,
    SourceUnsupportedRange,
    SourceIdentityChanged,
    IntegrityMismatch,
    IntegrityTruncated,
    IntegrityRangeMismatch,
    ArchiveUnsafePath,
    ArchiveLinkEscape,
    ArchiveCollision,
    ArchiveBomb,
    ArchiveUnsupported,
    DestinationConflict,
    DestinationModified,
    DestinationForeign,
    DestinationUnrepresentable,
    DestinationCrossVolume,
    CacheLocked,
    CacheCorrupt,
    CacheFormatMismatch,
    CacheCrossVolume,
    CacheLockingUnsupported,
    PolicyOffline,
    PolicyCredentialMissing,
    PolicyCredentialInvalid,
    PolicyTermsRequired,
    PolicyTrustRefused,
    PolicyAddressRefused,
    ResourceDisk,
    ResourceLimit,
}

impl ErrorKind {
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
            Self::DestinationConflict => "destination.conflict",
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
            Self::PolicyAddressRefused => "policy.address_refused",
            Self::ResourceDisk => "resource.disk",
            Self::ResourceLimit => "resource.limit",
        }
    }

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
            Self::DestinationConflict
            | Self::DestinationModified
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
            | Self::PolicyTrustRefused
            | Self::PolicyAddressRefused => Layer::Policy,
            Self::ResourceDisk | Self::ResourceLimit => Layer::Resource,
        }
    }

    pub const ALL: [Self; 35] = [
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
        Self::DestinationConflict,
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
        Self::PolicyAddressRefused,
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
    pub fn with_member(mut self, member: &str) -> Self {
        self.member = Some(member.into());
        self
    }

    #[must_use]
    pub fn member(&self) -> Option<&str> {
        self.member.as_deref()
    }

    #[must_use]
    pub fn with_source(mut self, location: &str) -> Self {
        self.source = Some(SafeUrl::new(location));
        self
    }

    #[must_use]
    pub fn with_attempts(mut self, attempts: u32) -> Self {
        self.attempts = attempts;
        self
    }

    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_retry_after(mut self, wait: std::time::Duration) -> Self {
        let seconds = u32::try_from(wait.as_secs()).unwrap_or(u32::MAX);
        self.retry_after_seconds =
            Some(std::num::NonZeroU32::new(seconds).unwrap_or(std::num::NonZeroU32::MIN));
        self
    }

    #[must_use]
    pub fn rate_limited(mut self) -> Self {
        self.retry_after_seconds = self.retry_after_seconds.or(Some(std::num::NonZeroU32::MIN));
        self
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        self.retry_after_seconds
            .map(|seconds| std::time::Duration::from_secs(u64::from(seconds.get())))
    }

    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn layer(&self) -> Layer {
        self.layer
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
    pub fn source_location(&self) -> Option<&SafeUrl> {
        self.source.as_ref()
    }

    #[must_use]
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    #[must_use]
    pub fn retryable(&self) -> bool {
        self.retryable
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Cache,
    Destination,
    Source,
}

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
