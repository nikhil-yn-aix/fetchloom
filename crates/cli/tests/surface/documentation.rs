//! The proof that every flag, command, event and error kind reference.md names
//! above its Not built section is one this binary actually has, and that no flag
//! below that section is one it offers.
//!
//! Three audits found false sentences in the documentation by hand and each
//! recorded that nothing enforced the convention. This is the enforcement.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the thing that could not be read is the message"
)]

use crate::support;

use std::collections::BTreeSet;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::event::EVENT_NAMES;

const EXTENSIONS: [&str; 8] = ["yaml", "yml", "toml", "json", "ndjson", "md", "lock", "txt"];

const UNBUILT: &str = "
## Not built";

fn reference() -> String {
    let whole = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference.md"
    ))
    .unwrap();
    match whole.split_once(UNBUILT) {
        Some((built, _)) => built.to_owned(),
        None => whole,
    }
}

fn help(arguments: &[&str]) -> String {
    let output = support::fetchloom().args(arguments).arg("--help").output();
    let output = output.unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn subcommands(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Some(listing) = text.split("Commands:").nth(1) else {
        return found;
    };
    for line in listing.lines() {
        if !line.starts_with("  ") || line.starts_with("      ") {
            continue;
        }
        let Some(word) = line.split_whitespace().next() else {
            continue;
        };
        if word.chars().all(|letter| letter.is_ascii_lowercase()) {
            found.push(word.to_owned());
        }
    }
    found
}

fn commands() -> Vec<String> {
    let found = subcommands(&help(&[]));
    assert!(!found.is_empty(), "the top-level help lists no commands");
    found
}

fn flags() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pages = vec![help(&[])];
    for command in commands() {
        let page = help(&[&command]);
        for word in subcommands(&page) {
            pages.push(help(&[&command, &word]));
        }
        pages.push(page);
    }
    for page in &pages {
        found.extend(long_flags(page));
    }
    assert!(!found.is_empty(), "the binary offers no long flag");
    found
}

fn long_flags(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == '-' && bytes[index + 1] == '-' && bytes[index + 2].is_ascii_lowercase() {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end] == '-') {
                end += 1;
            }
            found.push(bytes[index..end].iter().collect());
            index = end;
            continue;
        }
        index += 1;
    }
    found
}

fn claimed(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for paragraph in text.split("\n\n") {
        let mut parts = paragraph.split('`');
        parts.next();
        while let Some(token) = parts.next() {
            if parts.next().is_none() {
                break;
            }
            found.insert(token.to_owned());
        }
    }
    assert!(!found.is_empty(), "reference.md quotes nothing");
    found
}

#[test]
fn every_flag_the_reference_names_is_one_the_binary_offers() {
    let offered = flags();
    let mut missing = Vec::new();
    for token in claimed(&reference()) {
        let Some(flag) = token.split_whitespace().next() else {
            continue;
        };
        if !flag.starts_with("--") {
            continue;
        }
        if !offered.contains(flag) {
            missing.push(token.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "reference.md names {missing:?} as flags this build has, and the binary's own help offers \
         none of them. Either the flag is unbuilt, in which case the sentence needs its Not built. \
         marker, or the name in the document is wrong."
    );
}

#[test]
fn every_dotted_name_the_reference_uses_is_an_event_or_an_error_kind() {
    let events: BTreeSet<&str> = EVENT_NAMES.into_iter().collect();
    let kinds: BTreeSet<&str> = ErrorKind::ALL.into_iter().map(ErrorKind::label).collect();
    let mut missing = Vec::new();
    for token in claimed(&reference()) {
        if !token.contains('.') || token.contains(' ') || token.contains('/') {
            continue;
        }
        if !token
            .chars()
            .all(|letter| letter.is_ascii_lowercase() || letter == '.' || letter == '_')
        {
            continue;
        }
        let last = token.rsplit('.').next().unwrap_or_default();
        if EXTENSIONS.contains(&last) {
            continue;
        }
        if !events.contains(token.as_str()) && !kinds.contains(token.as_str()) {
            missing.push(token.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "reference.md names {missing:?}, which is neither an event in EVENT_NAMES nor an error kind \
         in ErrorKind::ALL. A dotted lowercase name in that document is one or the other."
    );
}

fn unbuilt() -> String {
    let whole = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/reference.md"
    ))
    .unwrap();
    whole
        .split_once(UNBUILT)
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or_default()
}

#[test]
fn no_flag_the_not_built_table_names_is_one_the_binary_offers() {
    let offered = flags();
    let text = unbuilt();
    assert!(
        !text.trim().is_empty(),
        "reference.md has no Not built section, so the marker this test splits on has moved"
    );
    let mut built = Vec::new();
    for token in claimed(&text) {
        let Some(word) = token.split_whitespace().next() else {
            continue;
        };
        if word.starts_with("--") && offered.contains(word) {
            built.push(token.clone());
        }
    }
    assert!(
        built.is_empty(),
        "reference.md lists {built:?} under Not built and the binary offers them. A flag that \
         graduated has to leave that table in the same change, or the document says the opposite \
         of what runs."
    );
}
