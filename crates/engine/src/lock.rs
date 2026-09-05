//! What a lock pins, containing nothing local to one machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest, ManifestDigest, TreeDigest};
use crate::error::{Error, ErrorKind};
use crate::selection::{Glob, Layout};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedArtifact {
    pub digest: ContentDigest,
    pub interop: InteropDigest,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<Glob>,
    #[serde(default)]
    pub layout: Layout,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDataset {
    pub manifest: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    pub artifacts: BTreeMap<String, LockedArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeDigest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub datasets: BTreeMap<String, LockedDataset>,
}

impl LockedDataset {
    /// # Errors
    /// `alias.unstable` when the resolved dataset holds an artifact the lock
    /// pins nothing for, and whatever `LockedArtifact::check` gives for an
    /// artifact that moved.
    pub fn check(&self, resolved: &Self) -> Result<(), Error> {
        for (id, found) in &resolved.artifacts {
            let Some(pinned) = self.artifacts.get(id) else {
                return Err(Error::new(
                    ErrorKind::AliasUnstable,
                    format!(
                        "run without --locked to record it, because the lock pins no artifact \
                         named {id}"
                    ),
                )
                .with_artifact(id.clone()));
            };
            pinned.check(id, found)?;
        }
        for id in self.artifacts.keys() {
            if !resolved.artifacts.contains_key(id) {
                return Err(Error::new(
                    ErrorKind::AliasUnstable,
                    format!(
                        "run without --locked to record what it resolves to, because the lock \
                         pins an artifact named {id} that this run did not resolve"
                    ),
                )
                .with_artifact(id.clone()));
            }
        }
        match (self.tree, resolved.tree) {
            (Some(pinned), Some(found)) if pinned != found => Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "materialize somewhere this platform can represent every entry, because the \
                     tree is {found} where the lock pins {pinned}"
                ),
            )),
            _ => Ok(()),
        }
    }
}

impl LockedDataset {
    /// # Errors
    /// `alias.unstable` when the manifest digest, the release, or the
    /// selection the run asks for is not the one the lock pins.
    pub fn check_request(
        &self,
        manifest: ManifestDigest,
        release: Option<&str>,
        selection: &crate::selection::Selection,
    ) -> Result<(), Error> {
        if self.manifest != manifest {
            return Err(moved("manifest", &self.manifest, &manifest));
        }
        if self.release.as_deref() != release {
            return Err(moved(
                "release",
                &self.release.clone().unwrap_or_default(),
                &release.unwrap_or_default(),
            ));
        }
        for (id, pinned) in &self.artifacts {
            if pinned.select != selection.include {
                return Err(moved(
                    "select",
                    &render_globs(&pinned.select),
                    &render_globs(&selection.include),
                )
                .with_artifact(id.clone()));
            }
            if pinned.layout != selection.layout {
                return Err(
                    moved("layout", &pinned.layout, &selection.layout).with_artifact(id.clone())
                );
            }
        }
        Ok(())
    }
}

impl LockedArtifact {
    /// # Errors
    /// `integrity.mismatch` when the bytes hash to something other than what
    /// the lock pins, and `alias.unstable` when another pinned field moved.
    pub fn check(&self, id: &str, resolved: &Self) -> Result<(), Error> {
        if self.digest != resolved.digest {
            return Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "fetch it from a source that still serves what the lock pins, because the \
                     bytes hash to {} where the lock pins {}",
                    resolved.digest, self.digest
                ),
            )
            .with_artifact(id.to_owned()));
        }
        if self.interop != resolved.interop {
            return Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "fetch it again, because the interop digest is {} where the lock pins {}",
                    resolved.interop, self.interop
                ),
            )
            .with_artifact(id.to_owned()));
        }
        if self.size != resolved.size {
            return Err(Error::new(
                ErrorKind::IntegrityMismatch,
                format!(
                    "fetch it again, because the object is {} bytes where the lock pins {}",
                    resolved.size, self.size
                ),
            )
            .with_artifact(id.to_owned()));
        }
        Ok(())
    }
}

fn render_globs(globs: &[Glob]) -> String {
    globs
        .iter()
        .map(|glob| glob.as_str().to_owned())
        .collect::<Vec<String>>()
        .join(" ")
}

fn moved(field: &str, pinned: &impl std::fmt::Display, found: &impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::AliasUnstable,
        format!(
            "run without --locked to record what it resolves to now, because {field} is {found} \
             where the lock states {pinned}"
        ),
    )
}

impl Lock {
    /// # Errors
    /// `manifest.invalid` when the file exists and cannot be read or does not
    /// parse. A lock that is not there is an empty lock rather than an error.
    pub fn read(path: &Path, limits: &crate::limits::Limits) -> Result<Self, Error> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(reason) => {
                return Err(Error::new(
                    ErrorKind::ManifestInvalid,
                    format!("make {} readable: {reason}", path.display()),
                ));
            }
        };
        crate::document::read_model(&bytes, "lock", limits, crate::document::Bound::Foreign)
    }

    /// # Errors
    /// `manifest.invalid` when the lock cannot be rendered, written beside
    /// the destination, or renamed onto it.
    pub fn write(&self, path: &Path) -> Result<(), Error> {
        let rendered = crate::document::render_model(self)?;
        let mut beside = path.as_os_str().to_owned();
        beside.push(format!(".{}.writing", std::process::id()));
        let beside = PathBuf::from(beside);
        let failure = |reason: &std::io::Error| {
            Error::new(
                ErrorKind::ManifestInvalid,
                format!("make {} writable: {reason}", path.display()),
            )
        };
        std::fs::write(&beside, rendered.as_bytes()).map_err(|reason| failure(&reason))?;
        std::fs::rename(&beside, path).map_err(|reason| {
            let _ = std::fs::remove_file(&beside);
            failure(&reason)
        })
    }
}
