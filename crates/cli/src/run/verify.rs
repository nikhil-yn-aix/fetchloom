//! Checking a tree that already exists against what the run resolved.

use super::context::Materialization;
use crate::materialize;
use fetchloom_engine::canonical;
use fetchloom_engine::digest::TreeDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::event::EventPayload;
use fetchloom_engine::hashing;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::receipt::{Receipt, RecordedFingerprint};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::tree::TreeEntry;
use fetchloom_engine::work::WorkCounter;
use std::io::Read;
use std::path::Path;

pub fn verify_tree(
    path: &Path,
    receipt: Option<&Receipt>,
    budget: ThreadBudget,
    emit: &dyn Fn(EventPayload),
) -> Result<(TreeDigest, u64), Error> {
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        ));
    }
    let processor = Processor::new(budget).map_err(|reason| {
        Error::new(
            ErrorKind::ResourceLimit,
            format!("the processor pool could not be built: {reason}"),
        )
    })?;
    let work = WorkCounter::new();
    let digester = std::sync::Mutex::new(hashing::Digester::new());
    let walked = materialize::walk(path)?;
    let mut entries = walked.entries.clone();
    entries.extend(hash_files(
        &digester,
        &walked.root,
        &walked.files,
        &processor,
        &work,
    )?);
    let Some(receipt) = receipt else {
        report_unread_modes(
            entries
                .iter()
                .any(|entry| matches!(entry, TreeEntry::File { .. })),
            emit,
        );
        return Ok((canonical::tree_digest(&entries), entries.len() as u64));
    };
    let entries = with_receipt_modes(receipt, entries);
    Ok((canonical::tree_digest(&entries), entries.len() as u64))
}

pub(super) fn with_receipt_modes(receipt: &Receipt, found: Vec<TreeEntry>) -> Vec<TreeEntry> {
    found
        .into_iter()
        .map(|entry| match entry {
            TreeEntry::File {
                path,
                mode: _,
                size,
                content,
            } => {
                let mode = receipt.mode_of(path.as_str());
                TreeEntry::File {
                    path,
                    mode,
                    size,
                    content,
                }
            }
            other => other,
        })
        .collect()
}

pub(super) fn report_unread_modes(found_a_file: bool, emit: &dyn Fn(EventPayload)) {
    if !found_a_file {
        return;
    }
    emit(EventPayload::Degrade {
        requested: "the mode each file carries".to_owned(),
        used: format!("{:04o} for every file", u32::from(materialize::WALKED_MODE)),
        reason:
            "a filesystem tree states no mode, and reading one back from a volume that carries an \
             executable bit would digest the same tree differently than a volume that does not"
                .to_owned(),
    });
}

pub(super) fn hash_files(
    digester: &std::sync::Mutex<hashing::Digester>,
    root: &Path,
    files: &[materialize::SourceFile],
    processor: &Processor,
    work: &WorkCounter,
) -> Result<Vec<TreeEntry>, Error> {
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let full = root.join(&file.relative);
        let handle = std::fs::File::open(&full)
            .map_err(|reason| filesystem_failure(Surface::Source, &full, &reason))?;
        let size = handle
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Source, &full, &reason))?
            .len();
        let counted = CountedRead {
            inner: handle,
            work,
        };
        let digests = digester
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hash(processor, counted)
            .map_err(|reason| filesystem_failure(Surface::Source, &full, &reason))?;
        entries.push(TreeEntry::File {
            path: file.entry.clone(),
            mode: file.mode,
            size,
            content: digests.content,
        });
    }
    Ok(entries)
}

pub(super) struct CountedRead<'a, R> {
    inner: R,
    work: &'a WorkCounter,
}

impl<R: Read> Read for CountedRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.work.read_bytes(read as u64);
        }
        Ok(read)
    }
}

pub(super) fn destination_entries(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
) -> Result<Vec<TreeEntry>, Error> {
    let walked = materialize::walk(destination)?;
    let mut entries = walked.entries.clone();
    let recorded = recorded_fingerprints(with, destination, resolved);
    let mut to_hash = Vec::with_capacity(walked.files.len());
    for file in &walked.files {
        match unchanged_by_fingerprint(with, &walked.root, file, &recorded, resolved) {
            Some(entry) => entries.push(entry),
            None => to_hash.push(file.clone()),
        }
    }
    entries.extend(hash_files(
        with.digester,
        &walked.root,
        &to_hash,
        with.processor,
        with.work,
    )?);
    Ok(entries)
}

pub(super) fn recorded_fingerprints(
    with: &Materialization<'_>,
    destination: &Path,
    resolved: &[TreeEntry],
) -> std::collections::BTreeMap<String, RecordedFingerprint> {
    let wanted = canonical::tree_digest(resolved);
    with.cache
        .and_then(|cache| cache.read_receipt(destination).ok().flatten())
        .filter(|receipt| receipt.tree == Some(wanted))
        .map(|receipt| receipt.fingerprints)
        .unwrap_or_default()
}

pub(super) fn unchanged_by_fingerprint(
    with: &Materialization<'_>,
    root: &Path,
    file: &materialize::SourceFile,
    recorded: &std::collections::BTreeMap<String, RecordedFingerprint>,
    resolved: &[TreeEntry],
) -> Option<TreeEntry> {
    use fetchloom_engine::verification::VerificationPolicy;

    if with.verify == VerificationPolicy::Always {
        return None;
    }
    let held = recorded.get(file.entry.as_str())?;
    let now = with.platform.fingerprint(&root.join(&file.relative)).ok()?;
    if !held.matches(now) {
        return None;
    }
    let entry = resolved
        .iter()
        .find(|entry| entry.path() == &file.entry)?
        .clone();
    match entry {
        TreeEntry::File {
            path,
            size,
            content,
            ..
        } => Some(TreeEntry::File {
            path,
            mode: file.mode,
            size,
            content,
        }),
        _ => None,
    }
}
