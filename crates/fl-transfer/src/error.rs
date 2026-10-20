//! Transfer errors: what failed, where, and the kind a command reports.

use std::io;
use std::time::Duration;

use fl_model::error::ErrorKind;
use fl_model::units;

use crate::credentials::CredentialError;
use crate::hash::Mismatch;
use crate::url::SafeUrl;

/// A failure of a transfer. Locations appear only in their redacted form.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The host could not be reached, or the connection broke.
    #[error("could not reach {host}: {reason}")]
    Unreachable {
        /// The host.
        host: String,
        /// What went wrong, as the network stack said.
        reason: String,
    },
    /// The connection stayed silent for longer than allowed.
    #[error("{host} went silent for longer than {} while {during}", units::duration(*.limit))]
    Timeout {
        /// The host.
        host: String,
        /// Connecting, waiting for a response, or receiving.
        during: &'static str,
        /// The silence allowed.
        limit: Duration,
    },
    /// The server answered with an error status.
    #[error("{url} answered {status}")]
    Status {
        /// The location asked for.
        url: SafeUrl,
        /// The status.
        status: u16,
    },
    /// The host asked to wait longer than is waited out.
    #[error("{host} asked to wait {}, longer than fetchloom waits", units::duration(*.wait))]
    RateLimited {
        /// The host.
        host: String,
        /// How long it asked for.
        wait: Duration,
    },
    /// A redirect led to an address that is never contacted.
    #[error("{url} leads to a private or reserved address, which is not followed")]
    Refused {
        /// Where the redirect led.
        url: SafeUrl,
    },
    /// Plain http where only https is allowed.
    #[error("{url}: {reason}")]
    Insecure {
        /// The location.
        url: SafeUrl,
        /// Why it is refused.
        reason: &'static str,
    },
    /// The server broke the rules of HTTP.
    #[error("{url}: {reason}")]
    Protocol {
        /// The location.
        url: SafeUrl,
        /// What it did.
        reason: String,
    },
    /// The server wants credentials and none are configured.
    #[error("{url} answered {status} and no credential is configured for it")]
    AuthRequired {
        /// The location.
        url: SafeUrl,
        /// 401 or 403.
        status: u16,
    },
    /// The server refused the credentials sent.
    #[error("{url} refused the configured credential with {status}")]
    AuthRejected {
        /// The location.
        url: SafeUrl,
        /// 401 or 403.
        status: u16,
    },
    /// A credentials file cannot be used.
    #[error(transparent)]
    Credentials(#[from] CredentialError),
    /// Offline, so nothing is fetched.
    #[error("offline, so {url} is not fetched")]
    Offline {
        /// The location.
        url: SafeUrl,
    },
    /// The bytes do not match a stated digest.
    #[error("{url}: {source}")]
    Mismatch {
        /// The location.
        url: SafeUrl,
        /// Which digest failed.
        #[source]
        source: Mismatch,
    },
    /// Another number of bytes arrived than the source stated.
    #[error("{url}: received {received} bytes where {expected} were stated")]
    Length {
        /// The location.
        url: SafeUrl,
        /// The bytes stated.
        expected: u64,
        /// The bytes received.
        received: u64,
    },
    /// Writing or reading the destination failed.
    #[error("could not write the download of {url}: {source}")]
    Sink {
        /// The location.
        url: SafeUrl,
        /// What failed.
        #[source]
        source: io::Error,
    },
    /// The client could not be built.
    #[error("could not set up the HTTP client: {0}")]
    Setup(String),
}

impl TransferError {
    /// The kind a command reports for this error.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::Unreachable { .. } => ErrorKind::NetworkUnreachable,
            Self::Timeout { .. } => ErrorKind::NetworkTimeout,
            Self::Status { .. } => ErrorKind::NetworkStatus,
            Self::RateLimited { .. } => ErrorKind::NetworkRateLimited,
            Self::Refused { .. } => ErrorKind::NetworkRefused,
            Self::Insecure { .. } => ErrorKind::NetworkInsecure,
            Self::Protocol { .. } => ErrorKind::NetworkProtocol,
            Self::AuthRequired { .. } => ErrorKind::AuthRequired,
            Self::AuthRejected { .. } | Self::Credentials(_) => ErrorKind::AuthRejected,
            Self::Offline { .. } => ErrorKind::OfflineMissing,
            Self::Mismatch { .. } | Self::Length { .. } => ErrorKind::IntegrityMismatch,
            Self::Sink { .. } | Self::Setup(_) => ErrorKind::LocalIo,
        }
    }
}
