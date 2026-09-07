//! Narrowing a resolved tree to the members the request asked for.

use super::paths::names_something_here;
use crate::materialize;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::selection::{Candidate, Selection};
use fetchloom_engine::tree::TreeEntry;
use std::collections::HashMap;

pub(super) enum SelectionCandidate {
    Directory,
    Symlink {
        size: u64,
        content: ContentDigest,
        target: Vec<u8>,
    },
    File(materialize::SourceFile),
}

pub(super) fn offer(walked: &materialize::Walked) -> (Vec<String>, Vec<SelectionCandidate>) {
    let mut members: Vec<String> = Vec::new();
    let mut candidates: Vec<SelectionCandidate> = Vec::new();
    for entry in &walked.entries {
        match entry {
            TreeEntry::Directory { path } => {
                members.push(path.as_str().to_owned());
                candidates.push(SelectionCandidate::Directory);
            }
            TreeEntry::Symlink {
                path,
                size,
                content,
            } => {
                members.push(path.as_str().to_owned());
                let target = walked
                    .links
                    .iter()
                    .find(|link| link.entry == *path)
                    .map(|link| link.target.clone())
                    .unwrap_or_default();
                candidates.push(SelectionCandidate::Symlink {
                    size: *size,
                    content: *content,
                    target,
                });
            }
            TreeEntry::File { .. } => {}
        }
    }
    for file in &walked.files {
        members.push(file.entry.as_str().to_owned());
        candidates.push(SelectionCandidate::File(file.clone()));
    }
    (members, candidates)
}

pub(super) fn apply_selection(
    walked: materialize::Walked,
    selection: &Selection,
) -> Result<materialize::Walked, Error> {
    let (members, candidates) = offer(&walked);

    let offered: Vec<Candidate<'_>> = members
        .iter()
        .zip(&candidates)
        .map(|(path, candidate)| Candidate {
            path,
            directory: matches!(candidate, SelectionCandidate::Directory),
        })
        .collect();
    let applied = selection.apply(&offered)?;

    let mut new_entries: Vec<TreeEntry> = Vec::new();
    let mut new_files: Vec<materialize::SourceFile> = Vec::new();
    let mut new_links: Vec<materialize::SourceLink> = Vec::new();
    let mut claimed: HashMap<String, String> = HashMap::new();

    for member in &applied.members {
        let original = &members[member.index];
        if let Some(earlier) = claimed.insert(member.path.as_str().to_owned(), original.clone()) {
            return Err(Error::new(
                ErrorKind::ArchiveCollision,
                format!(
                    "rename {earlier} or {original} so --layout does not flatten both onto {}",
                    member.path
                ),
            ));
        }
        match &candidates[member.index] {
            SelectionCandidate::Directory => new_entries.push(TreeEntry::Directory {
                path: member.path.clone(),
            }),
            SelectionCandidate::Symlink {
                size,
                content,
                target,
            } => {
                new_entries.push(TreeEntry::Symlink {
                    path: member.path.clone(),
                    size: *size,
                    content: *content,
                });
                new_links.push(materialize::SourceLink {
                    entry: member.path.clone(),
                    target: target.clone(),
                });
            }
            SelectionCandidate::File(file) => {
                let mut rewritten = file.clone();
                rewritten.entry = member.path.clone();
                new_files.push(rewritten);
            }
        }
    }
    for directory in &applied.directories {
        new_entries.push(TreeEntry::Directory {
            path: directory.clone(),
        });
    }

    let mut bytes = 0u64;
    for file in &new_files {
        let full = walked.root.join(&file.relative);
        let metadata = std::fs::symlink_metadata(&full)
            .map_err(|reason| filesystem_failure(Surface::Source, &full, &reason))?;
        bytes += metadata.len();
    }

    Ok(materialize::Walked {
        entries: new_entries,
        files: new_files,
        links: new_links,
        bytes,
        root: walked.root,
    })
}

pub(crate) fn assert_terms(
    policy: &dyn Policy,
    license: Option<&fetchloom_engine::license::License>,
) -> Result<Option<fetchloom_engine::license::Acceptance>, Error> {
    use fetchloom_engine::license::Acceptance;

    let Some(license) = license else {
        return Ok(None);
    };
    if !license.requires_acceptance {
        return Ok(None);
    }
    match policy.terms(license)? {
        Acceptance::Asserted => Ok(Some(Acceptance::Asserted)),
        Acceptance::Withheld => Err(Error::new(
            ErrorKind::PolicyTermsRequired,
            "accept the recorded terms when asked, or run the command again with --yes, because this run declined them",
        )),
    }
}

/// Forbids the network when the run is offline, and says whether it did, so a
/// command that can answer from the cache decides for itself what to do next.
pub fn forbid_when_offline(policy: &dyn Policy) -> bool {
    if !policy.offline() {
        return false;
    }
    fetchloom_engine::network::forbid();
    true
}

/// # Errors
/// `policy.offline` when the run was told not to reach the network and the
/// reference names something that is not already here.
pub fn allowed_offline(reference: &str, policy: &dyn Policy) -> Result<(), Error> {
    if !forbid_when_offline(policy) {
        return Ok(());
    }
    if names_something_here(reference) {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::PolicyOffline,
        format!(
            "run the command again without --offline to reach {}",
            SafeUrl::new(reference)
        ),
    ))
}

pub(super) fn selected_entries(
    location: &str,
    listed: Vec<fetchloom_engine::seam::source::ListingEntry>,
    selection: &Selection,
) -> Result<Vec<fetchloom_engine::seam::source::ListingEntry>, Error> {
    let considered = listed.len();
    let kept: Vec<_> = listed
        .into_iter()
        .filter(|entry| selection.takes(&entry.path))
        .collect();
    if kept.is_empty() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "widen the selection, because {considered} entries were listed at {location} and none of them matched it"
            ),
        ));
    }
    Ok(kept)
}
