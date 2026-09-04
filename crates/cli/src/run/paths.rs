//! Where a reference points on this filesystem, and what a run calls what it found there.

use super::adapters::unbuilt_form;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::tree::{Mode, TreeEntry};
use std::path::{Path, PathBuf};

#[must_use]
pub fn executable_paths(entries: &[TreeEntry]) -> Vec<String> {
    let mut paths: Vec<String> = entries
        .iter()
        .filter_map(|entry| match entry {
            TreeEntry::File {
                path,
                mode: Mode::Executable,
                ..
            } => Some(path.as_str().to_owned()),
            _ => None,
        })
        .collect();
    paths.sort();
    paths
}

pub fn local_path(reference: &str) -> Result<PathBuf, Error> {
    if let Some(rest) = reference.strip_prefix("file://") {
        let trimmed = rest.strip_prefix('/').unwrap_or(rest);
        let looks_like_windows_path = trimmed.as_bytes().get(1).is_some_and(|byte| *byte == b':');
        let path = if looks_like_windows_path {
            PathBuf::from(trimmed)
        } else {
            PathBuf::from(format!("/{trimmed}"))
        };
        return Ok(path);
    }
    if reference.contains("://") {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "this build resolves only a local path or a file: location, not {}",
                SafeUrl::new(reference)
            ),
        ));
    }
    if let Some(form) = unbuilt_form(reference) {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "this build resolves only a local path or a file: location, not {} which is {form}",
                SafeUrl::new(reference)
            ),
        ));
    }
    let path = PathBuf::from(reference);
    if path.exists() {
        Ok(path)
    } else {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "check that {} names a path that exists",
                SafeUrl::new(reference)
            ),
        ))
    }
}

#[must_use]
pub fn default_destination(source: &Path) -> PathBuf {
    let name = source
        .file_name()
        .map_or_else(|| PathBuf::from("dataset"), PathBuf::from);
    PathBuf::from(".").join(name)
}

pub fn resolve_path(path: &Path) -> Result<PathBuf, Error> {
    std::path::absolute(path).map_err(|reason| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!("{}: {reason}", path.display()),
        )
    })
}

pub(super) fn containing_directory(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub(super) fn temp_beside(to: &Path) -> PathBuf {
    let name = to.file_name().map_or_else(
        || "entry".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    to.with_file_name(format!(".{name}.fetchloom-restore"))
}

pub(super) fn staging_beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(".{name}.fetchloom-staging"))
}

pub(super) fn names_something_here(reference: &str) -> bool {
    if let Some(path) = reference.strip_prefix("file://") {
        return !path.is_empty();
    }
    if reference.contains("://") {
        return false;
    }
    std::path::Path::new(reference).exists()
}

pub(super) fn container_name(location: &str) -> String {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    let trimmed = after_scheme.trim_end_matches('/');
    let last = trimmed.rsplit('/').find(|part| !part.is_empty());
    last.map_or_else(|| "dataset".to_owned(), str::to_owned)
}

pub(super) fn entry_path_str(entry: &TreeEntry) -> &str {
    match entry {
        TreeEntry::File { path, .. }
        | TreeEntry::Directory { path }
        | TreeEntry::Symlink { path, .. } => path.as_str(),
    }
}

pub(super) fn object_name(location: &str) -> String {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    let path = after_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let last = path.rsplit('/').find(|part| !part.is_empty());
    last.map_or_else(|| "object".to_owned(), str::to_owned)
}

#[must_use]
pub fn remote_name(location: &str) -> String {
    object_name(location)
}

pub(super) fn resolve_source_path(base: &Path, source: &str) -> PathBuf {
    let stated = PathBuf::from(source.strip_prefix("file://").unwrap_or(source));
    if stated.is_absolute() {
        stated
    } else {
        base.join(stated)
    }
}
