//! The cross-platform conformance corpus: pure data, no filesystem access.

use crate::digest::ContentDigest;
use crate::error::ErrorKind;
use crate::tree::{EntryPath, Mode, TreeEntry};

#[expect(
    clippy::panic,
    reason = "a corpus literal that fails EntryPath::new is a bug in this file, not an input to handle"
)]
fn path(text: &str) -> EntryPath {
    EntryPath::new(text).unwrap_or_else(|error| {
        panic!("conformance corpus path {text:?} is not a valid entry path: {error}")
    })
}

fn content_of(bytes: &[u8]) -> ContentDigest {
    ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes())
}

fn directory(text: &str) -> TreeEntry {
    TreeEntry::Directory { path: path(text) }
}

fn file(text: &str, mode: Mode, bytes: &[u8]) -> TreeEntry {
    TreeEntry::File {
        path: path(text),
        mode,
        size: bytes.len() as u64,
        content: content_of(bytes),
    }
}

fn symlink(text: &str, target: &str) -> TreeEntry {
    TreeEntry::Symlink {
        path: path(text),
        size: target.len() as u64,
        content: content_of(target.as_bytes()),
    }
}

/// The entries a conforming implementation must materialize identically, byte
/// for byte, on Windows and Linux.
#[must_use]
pub fn portable_core() -> Vec<TreeEntry> {
    vec![
        file("empty-file.bin", Mode::ReadWrite, b""),
        directory("dirs"),
        directory("dirs/empty"),
        directory("dirs/nested"),
        directory("dirs/nested/deep"),
        file("dirs/nested/deep/leaf.txt", Mode::ReadWrite, b"leaf"),
        directory("dirs/only-dirs"),
        directory("dirs/only-dirs/child"),
        directory("shared"),
        file("shared/a.bin", Mode::ReadWrite, b"identical content"),
        file("shared/b.bin", Mode::ReadWrite, b"identical content"),
        directory("modes"),
        file("modes/readme.txt", Mode::ReadWrite, b"read only bits"),
        file("modes/run.sh", Mode::Executable, b"#!/bin/sh\necho hi\n"),
        directory("links"),
        symlink("links/to-target", "shared/a.bin"),
        directory("unicode"),
        file("unicode/café.txt", Mode::ReadWrite, b"non-ascii utf-8 name"),
        directory("sort"),
        file("sort/Banana.txt", Mode::ReadWrite, b"uppercase first byte"),
        file("sort/apple.txt", Mode::ReadWrite, b"lowercase first byte"),
        directory("adjacent"),
        file(
            "adjacent/Item.txt",
            Mode::ReadWrite,
            b"distinct raw bytes, adjacent under case folding",
        ),
        file(
            "adjacent/item2.txt",
            Mode::ReadWrite,
            b"distinct raw bytes, adjacent under case folding",
        ),
    ]
}

/// One combination the target filesystem or platform cannot represent, with the
/// exact kind and reason a conforming implementation reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredFailure {
    /// What this case demonstrates.
    pub description: &'static str,
    /// The entries that together cannot be represented.
    pub entries: Vec<TreeEntry>,
    /// The error kind a conforming implementation reports.
    pub kind: ErrorKind,
    /// Why representation is impossible, named for the error's next action.
    pub reason: &'static str,
}

/// The entries a conforming implementation must reject, each with the exact
/// kind and reason.
#[must_use]
pub fn declared_failures() -> Vec<DeclaredFailure> {
    vec![
        DeclaredFailure {
            description: "two names differing only in case",
            entries: vec![
                directory("case-collision"),
                file("case-collision/File.txt", Mode::ReadWrite, b"first"),
                file("case-collision/file.txt", Mode::ReadWrite, b"second"),
            ],
            kind: ErrorKind::ArchiveCollision,
            reason: "case-collision/File.txt and case-collision/file.txt fold to the same name on a case-folding volume",
        },
        DeclaredFailure {
            description: "two names differing only in Unicode normalization",
            entries: vec![
                directory("unicode-collision"),
                file("unicode-collision/caf\u{e9}.txt", Mode::ReadWrite, b"nfc"),
                file("unicode-collision/cafe\u{301}.txt", Mode::ReadWrite, b"nfd"),
            ],
            kind: ErrorKind::ArchiveCollision,
            reason: "unicode-collision/café.txt in NFC and NFD fold to the same name on a normalization-folding volume",
        },
        DeclaredFailure {
            description: "a symlink where the platform forbids creating one",
            entries: vec![
                directory("forbidden-link"),
                symlink("forbidden-link/target", "shared/a.bin"),
            ],
            kind: ErrorKind::DestinationUnrepresentable,
            reason: "forbidden-link/target cannot be created without symlink privilege or Developer Mode",
        },
        DeclaredFailure {
            description: "a path longer than the target's maximum",
            entries: vec![
                directory("too-long"),
                file(&long_path(), Mode::ReadWrite, b"too long"),
            ],
            kind: ErrorKind::DestinationUnrepresentable,
            reason: "the path exceeds every target platform's maximum component or path length",
        },
    ]
}

fn long_path() -> String {
    let mut text = String::from("too-long/");
    text.push_str(&"x".repeat(5000));
    text.push_str(".bin");
    text
}
