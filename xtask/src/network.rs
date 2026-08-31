//! The lane that fetches real archives from real servers.

use std::path::Path;
use std::process::Command;

/// One archive this lane fetches, and what it must produce.
struct Subject {
    /// Where it lives.
    location: &'static str,
    /// What the destination is called under the scratch root.
    name: &'static str,
    /// How many entries the archive holds.
    entries: u64,
    /// The tree digest `get` reports, taken from the archive's own modes, and
    /// the one `verify` must reproduce from the run's receipt.
    fetched_tree: &'static str,
}

/// The archives this lane holds to a recorded answer.
const SUBJECTS: [Subject; 3] = [
    Subject {
        location: "https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz",
        name: "hello",
        entries: 462,
        fetched_tree: "blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec",
    },
    Subject {
        location: "https://files.pythonhosted.org/packages/source/s/six/six-1.16.0.tar.gz",
        name: "six",
        entries: 19,
        fetched_tree: "blake3:779c05c09bbcb5b8f7c38df8bafa46bb6d5a1403331e296bdce412c6d53887e6",
    },
    Subject {
        location: "https://ftp.gnu.org/gnu/gzip/gzip-1.12.tar.xz",
        name: "gzip",
        entries: 515,
        fetched_tree: "blake3:efed332979fab239c085f4492eda93c46af9364a1d11fbff9836e7400b793ba9",
    },
];

/// What one attempt at the lane produced.
pub enum Outcome {
    /// Every subject matched what was recorded.
    Passed,
    /// The lane could not reach a host and ran nothing.
    Skipped(String),
    /// A subject disagreed with what was recorded.
    Failed(String),
}

/// Fetches every recorded subject and checks it against its record.
pub fn run(workspace: &Path, binary: Option<&Path>) -> Outcome {
    let binary = binary.map_or_else(
        || {
            workspace
                .join("target")
                .join("debug")
                .join(if cfg!(windows) {
                    "fetchloom.exe"
                } else {
                    "fetchloom"
                })
        },
        Path::to_path_buf,
    );
    if !binary.is_file() {
        return Outcome::Failed(format!("{} was not built", binary.display()));
    }
    let root = std::env::temp_dir().join("fetchloom-network-lane");
    let _ = std::fs::remove_dir_all(&root);
    if let Err(reason) = std::fs::create_dir_all(&root) {
        return Outcome::Failed(format!("{} could not be created: {reason}", root.display()));
    }
    let cache = root.join("cache");

    for subject in &SUBJECTS {
        let destination = root.join(subject.name);
        let first = match fetch(&binary, subject.location, &destination, &cache) {
            Ok(body) => body,
            Err(reason) => {
                if reason.contains("\"network.") || reason.contains("policy.offline") {
                    return Outcome::Skipped(format!(
                        "{} could not be reached: {reason}",
                        subject.location
                    ));
                }
                return Outcome::Failed(format!("{}: {reason}", subject.location));
            }
        };
        if let Some(complaint) = disagrees(subject, &first) {
            return Outcome::Failed(complaint);
        }

        let untouched = state_of(&destination);
        let second = match fetch(&binary, subject.location, &destination, &cache) {
            Ok(body) => body,
            Err(reason) => return Outcome::Failed(format!("the second run: {reason}")),
        };
        if field(&second, "status") != "unchanged" {
            return Outcome::Failed(format!(
                "{} ran twice and did not report unchanged: {second}",
                subject.location
            ));
        }
        if untouched != state_of(&destination) {
            return Outcome::Failed(format!(
                "{} ran twice and the second run touched the destination",
                subject.location
            ));
        }

        let verified = match verify(&binary, &destination, &cache) {
            Ok(body) => body,
            Err(reason) => return Outcome::Failed(format!("verify: {reason}")),
        };
        if field(&verified, "tree") != subject.fetched_tree {
            return Outcome::Failed(format!(
                "verify of {} reported {} rather than the {} the fetch reported",
                subject.name,
                field(&verified, "tree"),
                subject.fetched_tree
            ));
        }
    }
    Outcome::Passed
}

fn disagrees(subject: &Subject, body: &str) -> Option<String> {
    if field(body, "tree") != subject.fetched_tree {
        return Some(format!(
            "{} fetched as {} rather than the recorded {}",
            subject.location,
            field(body, "tree"),
            subject.fetched_tree
        ));
    }
    if number(body, "entries") != Some(subject.entries) {
        return Some(format!(
            "{} holds {:?} entries rather than the recorded {}",
            subject.location,
            number(body, "entries"),
            subject.entries
        ));
    }
    None
}

fn fetch(
    binary: &Path,
    location: &str,
    destination: &Path,
    cache: &Path,
) -> Result<String, String> {
    let output = Command::new(binary)
        .arg("get")
        .arg(location)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--cache-dir")
        .arg(cache)
        .arg("--json")
        .output()
        .map_err(|reason| reason.to_string())?;
    let body = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        return Ok(body);
    }
    Err(body)
}

fn verify(binary: &Path, destination: &Path, cache: &Path) -> Result<String, String> {
    let output = Command::new(binary)
        .arg("verify")
        .arg(destination)
        .arg("--cache-dir")
        .arg(cache)
        .arg("--json")
        .output()
        .map_err(|reason| reason.to_string())?;
    let body = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() {
        return Ok(body);
    }
    Err(body)
}

/// Reads one string field out of a result body without parsing JSON.
fn field<'a>(body: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\":\"");
    let Some(at) = body.find(&needle) else {
        return "";
    };
    let rest = &body[at + needle.len()..];
    rest.find('"').map_or("", |end| &rest[..end])
}

fn number(body: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let at = body.find(&needle)?;
    let rest = &body[at + needle.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Describes every file under a destination well enough to tell whether a later
/// run touched it.
fn state_of(destination: &Path) -> Vec<(String, u64, Option<std::time::SystemTime>)> {
    let mut found = Vec::new();
    collect(destination, destination, &mut found);
    found.sort();
    found
}

fn collect(root: &Path, at: &Path, found: &mut Vec<(String, u64, Option<std::time::SystemTime>)>) {
    let Ok(entries) = std::fs::read_dir(at) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        found.push((name, metadata.len(), metadata.modified().ok()));
        if metadata.is_dir() {
            collect(root, &path, found);
        }
    }
}
