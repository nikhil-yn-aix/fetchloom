//! What a user names on the command line.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The shape of a reference, which decides how it resolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceForm {
    /// A bare dataset name.
    BareName,
    /// A namespaced dataset name with a release.
    NamespacedRelease,
    /// A path to a manifest on this machine.
    LocalManifest,
    /// A location of a manifest served over the network.
    RemoteManifest,
    /// A location of one file served over the network.
    DirectFile,
    /// A path to a file or a directory on this machine.
    LocalPath,
    /// A prefix in an object store.
    ObjectStore,
    /// A provider's own identifier for a record or a repository.
    Provider,
    /// A location of a metadata document describing a dataset.
    MetadataDocument,
    /// A content address.
    ContentAddress,
}

/// The text a user gave, kept exactly as it was written.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Reference(String);

impl Reference {
    /// Keeps a reference exactly as the user wrote it.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Returns the reference text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name of a host a source is reached at.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Host(String);

impl Host {
    /// Keeps a host name exactly as it appeared.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Returns the host name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
