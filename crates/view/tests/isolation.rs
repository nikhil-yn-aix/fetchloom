//! The proof that the live view reads the event stream and nothing else.
//!
//! Not by inspection. The two tests here read the crate's own manifest and its
//! own source, so the property fails loudly the moment someone adds a way in.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the file that could not be read is the message"
)]

use fetchloom_engine as _;

const PERMITTED: [&str; 1] = ["fetchloom-engine"];

const FORBIDDEN: [&str; 12] = [
    "fs",
    "net",
    "process",
    "env",
    "File",
    "OpenOptions",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "Command",
    "include_str",
    "include_bytes",
];

fn code_only(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut held = text.chars().peekable();
    let mut in_string = false;
    while let Some(character) = held.next() {
        if in_string {
            if character == '\\' {
                held.next();
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '/' if held.peek() == Some(&'/') => {
                for skipped in held.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
                kept.push('\n');
            }
            '/' if held.peek() == Some(&'*') => {
                held.next();
                let mut last = ' ';
                for skipped in held.by_ref() {
                    if last == '*' && skipped == '/' {
                        break;
                    }
                    last = skipped;
                }
                kept.push(' ');
            }
            _ => kept.push(character),
        }
    }
    kept
}

fn words(code: &str) -> Vec<String> {
    code.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

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
        let spelled = words(&code_only(&text));
        for forbidden in FORBIDDEN {
            assert!(
                !spelled.iter().any(|word| word == forbidden),
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
