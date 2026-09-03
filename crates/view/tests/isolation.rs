//! The proof that the live view reads the event stream and nothing else.
//!
//! Not by inspection. The two tests here read the crate's own manifest and its
//! own source, so the property fails loudly the moment someone adds a way in.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the file that could not be read is the message"
)]

use fetchloom_engine as _;

/// Every dependency this crate is permitted to have.
const PERMITTED: [&str; 1] = ["fetchloom-engine"];

/// Every way into the world that must not appear anywhere in this crate.
const FORBIDDEN: [&str; 8] = [
    "std::fs",
    "std::net",
    "std::process",
    "std::env",
    "File::open",
    "File::create",
    "TcpStream",
    "include_str!",
];

fn manifest() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap()
}

fn sources() -> Vec<(String, String)> {
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                found.push((path.display().to_string(), text));
            }
        }
    }
    assert!(!found.is_empty(), "the crate has no source to check");
    found
}

#[test]
fn the_view_depends_on_the_event_types_and_on_nothing_else() {
    let manifest = manifest();
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap_or_default()
        .split("\n[")
        .next()
        .unwrap_or_default();

    for line in dependencies.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split(['=', ' ']).next().unwrap_or_default();
        assert!(
            PERMITTED.contains(&name),
            "the live view gained the dependency {name}, which is a second input to a type \
             contracts says has only one. If this is deliberate, the contract in contracts.md \
             under Display modes has to change first."
        );
    }
}

#[test]
fn the_view_reaches_no_filesystem_no_network_and_no_process() {
    for (path, text) in sources() {
        for forbidden in FORBIDDEN {
            assert!(
                !text.contains(forbidden),
                "{path} names {forbidden}, so the live view can reach something the event stream \
                 did not give it"
            );
        }
    }
}

#[test]
fn the_view_takes_events_and_returns_text() {
    let sequence = fetchloom_engine::event::Sequence::new();
    let mut view = fetchloom_view::LiveView::new();
    view.observe(&fetchloom_engine::event::Event::new(
        &sequence,
        fetchloom_engine::event::EventPayload::RunStart,
    ));
    assert!(!view.render().is_empty());
}
