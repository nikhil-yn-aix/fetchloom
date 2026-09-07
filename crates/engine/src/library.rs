//! One directory holding materialized datasets, and the path identity alone decides.

use std::path::{Path, PathBuf};

use crate::digest::{LIBRARY_KEY_CONTEXT, ManifestDigest};
use crate::selection::Selection;

const IDENTITY_HEX: usize = 32;

const DIGITS: &[u8; 16] = b"0123456789abcdef";

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[must_use]
pub fn sanitized(name: &str) -> String {
    let trimmed = name.trim_end_matches(['.', ' ']);
    let mut built = String::with_capacity(trimmed.len());
    for character in trimmed.chars() {
        let allowed = character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.');
        built.push(if allowed { character } else { '_' });
    }
    let stem = built
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        built.insert(0, '_');
    }
    if built.is_empty() {
        built.push_str("dataset");
    }
    built
}

#[must_use]
pub fn identity(manifest: ManifestDigest, release: Option<&str>, selection: &Selection) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(LIBRARY_KEY_CONTEXT);
    hasher.update(manifest.bytes());
    hasher.update(&[u8::from(release.is_some())]);
    if let Some(release) = release {
        hasher.update(&(release.len() as u64).to_le_bytes());
        hasher.update(release.as_bytes());
    }
    hasher.update(&(selection.include.len() as u64).to_le_bytes());
    for glob in &selection.include {
        hasher.update(&(glob.as_str().len() as u64).to_le_bytes());
        hasher.update(glob.as_str().as_bytes());
    }
    hasher.update(&(selection.exclude.len() as u64).to_le_bytes());
    for glob in &selection.exclude {
        hasher.update(&(glob.as_str().len() as u64).to_le_bytes());
        hasher.update(glob.as_str().as_bytes());
    }
    hasher.update(selection.layout.to_string().as_bytes());
    let mut hex = String::with_capacity(IDENTITY_HEX);
    for byte in hasher.finalize().as_bytes().iter().take(IDENTITY_HEX / 2) {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
    }
    hex
}

#[must_use]
pub fn entry_path(
    library: &Path,
    dataset: &str,
    manifest: ManifestDigest,
    release: Option<&str>,
    selection: &Selection,
) -> PathBuf {
    library
        .join(sanitized(dataset))
        .join(identity(manifest, release, selection))
}

#[cfg(test)]
mod tests {
    use super::{entry_path, identity, sanitized};
    use crate::digest::ManifestDigest;
    use crate::selection::{Glob, Layout, Selection};
    use std::path::Path;

    fn a_digest(seed: u8) -> ManifestDigest {
        ManifestDigest::from_bytes([seed; 32])
    }

    fn nothing_selected() -> Selection {
        Selection {
            include: Vec::new(),
            exclude: Vec::new(),
            layout: Layout::Keep,
        }
    }

    #[test]
    fn every_windows_device_name_is_moved_out_of_the_way() {
        for reserved in [
            "CON",
            "PRN",
            "AUX",
            "NUL",
            "COM1",
            "COM9",
            "LPT1",
            "LPT9",
            "con",
            "nul.txt",
            "LPT3.tar.gz",
        ] {
            let safe = sanitized(reserved);
            assert!(
                safe.starts_with('_'),
                "{reserved} sanitized to {safe}, which names a device on Windows"
            );
        }
    }

    #[test]
    fn a_separator_a_colon_and_a_trailing_space_never_survive() {
        assert_eq!(sanitized("org/name"), "org_name");
        assert_eq!(sanitized("zenodo:1234"), "zenodo_1234");
        assert_eq!(sanitized("trailing "), "trailing");
        assert_eq!(sanitized("trailing."), "trailing");
        assert_eq!(sanitized("a\\b"), "a_b");
        assert_eq!(sanitized("..."), "dataset");
    }

    #[test]
    fn two_versions_of_one_dataset_are_two_directories() {
        let selection = nothing_selected();
        let first = entry_path(Path::new("/lib"), "corpus", a_digest(1), None, &selection);
        let second = entry_path(Path::new("/lib"), "corpus", a_digest(2), None, &selection);
        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
    }

    #[test]
    fn a_selection_is_part_of_the_path_because_it_is_part_of_identity() {
        let taken_whole = identity(a_digest(1), None, &nothing_selected());
        let taken_in_part = identity(
            a_digest(1),
            None,
            &Selection {
                include: vec![Glob::new("train/*".to_owned())],
                exclude: Vec::new(),
                layout: Layout::Keep,
            },
        );
        assert_ne!(taken_whole, taken_in_part);
    }

    #[test]
    fn a_release_is_part_of_the_path() {
        assert_ne!(
            identity(a_digest(1), None, &nothing_selected()),
            identity(a_digest(1), Some("2012"), &nothing_selected())
        );
    }

    #[test]
    fn one_identity_is_the_same_answer_every_time() {
        assert_eq!(
            identity(a_digest(7), Some("v1"), &nothing_selected()),
            identity(a_digest(7), Some("v1"), &nothing_selected())
        );
    }

    #[test]
    fn a_name_that_sanitizes_the_same_stays_separate_by_identity() {
        let selection = nothing_selected();
        let first = entry_path(Path::new("/lib"), "org/name", a_digest(1), None, &selection);
        let second = entry_path(Path::new("/lib"), "org:name", a_digest(2), None, &selection);
        assert_eq!(first.parent(), second.parent());
        assert_ne!(first, second);
    }
}
