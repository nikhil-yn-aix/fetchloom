//! What a lock pins, containing nothing local to one machine.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::digest::{ContentDigest, InteropDigest, ManifestDigest, TreeDigest};
use crate::error::{Error, ErrorKind};
use crate::selection::{Glob, Layout};

/// One artifact as a lock pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedArtifact {
    /// The content digest the bytes must have.
    pub digest: ContentDigest,
    /// The interop digest recorded alongside it.
    pub interop: InteropDigest,
    /// The length of the artifact in bytes.
    pub size: u64,
    /// The member paths the lock entry covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub select: Vec<Glob>,
    /// How member paths are rewritten.
    #[serde(default)]
    pub layout: Layout,
}

/// One dataset as a lock pins it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDataset {
    /// The manifest the entry was resolved from.
    pub manifest: ManifestDigest,
    /// The release the entry was resolved at, when the manifest names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// The artifacts, by the name the manifest gave each one.
    pub artifacts: BTreeMap<String, LockedArtifact>,
    /// The tree the artifacts materialized to, present only after a successful
    /// materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<TreeDigest>,
}

/// The portable record of what a run resolved to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    /// The datasets pinned, by name.
    pub datasets: BTreeMap<String, LockedDataset>,
}

impl LockedDataset {
    /// Checks what a run resolved against what the lock pins.
    ///
    /// Takes what the lock holds for this dataset and what the run resolved.
    /// A difference in the bytes is an integrity failure, because the lock
    /// stated a digest and the source did not serve it. A difference in what
    /// was asked for is a resolution failure, because the lock describes a
    /// different request than the one that was made, and refetching cannot fix
    /// it.
    ///
    /// # Errors
    ///
    /// Fails with `integrity.mismatch` naming the field, the value the lock
    /// pins, and the value the run resolved, and with `alias.unstable` for a
    /// difference in identity rather than in bytes.
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
    /// Checks the request a run is about to make against what the lock pins.
    ///
    /// Takes the digest of the manifest the run resolved from, the release it
    /// names, and the selection the run was given. Everything compared here is
    /// known before a byte moves, so a run the lock does not describe is
    /// refused before it publishes anything.
    ///
    /// # Errors
    ///
    /// Fails with `alias.unstable` naming the field, what the lock states, and
    /// what the run asks for.
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
    /// Checks one artifact a run resolved against what the lock pins.
    ///
    /// # Errors
    ///
    /// Fails with `integrity.mismatch` for a difference in the bytes and with
    /// `alias.unstable` for a difference in what was selected.
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
    /// Reads the lock at a path, returning an empty lock when there is none.
    ///
    /// # Errors
    ///
    /// Fails when the file is present and cannot be read or does not parse.
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
        crate::document::read_model(&bytes, "lock", limits)
    }

    /// Writes the lock at a path, in the one canonical form.
    ///
    /// Writes beside the lock and renames onto it, so a reader finds it whole
    /// or not at all.
    ///
    /// # Errors
    ///
    /// Fails when the lock cannot be written.
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
