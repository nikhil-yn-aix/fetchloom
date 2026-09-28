//! Checks that dependencies between the workspace crates only point in the allowed direction.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

type TestResult = Result<(), Box<dyn Error>>;

const CRATES: [&str; 7] = [
    "fl-archive",
    "fl-cli",
    "fl-model",
    "fl-sources",
    "fl-steps",
    "fl-store",
    "fl-transfer",
];

struct Edge {
    from: String,
    to: String,
    kind: String,
}

struct Workspace {
    crates: BTreeSet<String>,
    edges: Vec<Edge>,
}

fn allowed(from: &str, to: &str) -> bool {
    to == "fl-model"
        || from == "fl-cli"
        || (from, to) == ("fl-sources", "fl-transfer")
        || (from, to) == ("fl-steps", "fl-store")
}

fn workspace() -> Result<Workspace, Box<dyn Error>> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .arg("--manifest-path")
        .arg(manifest)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    let metadata: Value = serde_json::from_slice(&out.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata listed no packages")?;
    let crates: BTreeSet<String> = packages
        .iter()
        .filter_map(|package| package["name"].as_str())
        .map(String::from)
        .collect();
    let mut edges = Vec::new();
    for package in packages {
        let from = package["name"].as_str().ok_or("a package has no name")?;
        let dependencies = package["dependencies"]
            .as_array()
            .ok_or("a package has no dependency list")?;
        for dependency in dependencies {
            let to = dependency["name"]
                .as_str()
                .ok_or("a dependency has no name")?;
            if crates.contains(to) {
                edges.push(Edge {
                    from: from.to_owned(),
                    to: to.to_owned(),
                    kind: dependency["kind"].as_str().unwrap_or("normal").to_owned(),
                });
            }
        }
    }
    Ok(Workspace { crates, edges })
}

#[test]
fn metadata_lists_the_seven_crates() -> TestResult {
    let crates = workspace()?.crates;
    assert_eq!(
        crates,
        CRATES.iter().map(|name| (*name).to_owned()).collect()
    );
    Ok(())
}

#[test]
fn workspace_edges_point_in_the_allowed_direction() -> TestResult {
    let forbidden: Vec<String> = workspace()?
        .edges
        .iter()
        .filter(|edge| !allowed(&edge.from, &edge.to))
        .map(|edge| format!("{} -> {} ({})", edge.from, edge.to, edge.kind))
        .collect();
    assert!(forbidden.is_empty(), "forbidden edges: {forbidden:?}");
    Ok(())
}

#[test]
fn rules_reject_edges_against_the_direction() {
    for (from, to) in [
        ("fl-store", "fl-cli"),
        ("fl-model", "fl-store"),
        ("fl-transfer", "fl-store"),
        ("fl-archive", "fl-transfer"),
        ("fl-store", "fl-steps"),
        ("fl-sources", "fl-store"),
        ("fl-steps", "fl-transfer"),
    ] {
        assert!(!allowed(from, to), "{from} -> {to} was allowed");
    }
}

#[test]
fn rules_accept_every_documented_edge() {
    for name in CRATES.iter().filter(|name| **name != "fl-model") {
        assert!(allowed(name, "fl-model"), "{name} -> fl-model was refused");
    }
    for name in CRATES.iter().filter(|name| **name != "fl-cli") {
        assert!(allowed("fl-cli", name), "fl-cli -> {name} was refused");
    }
    assert!(allowed("fl-sources", "fl-transfer"));
    assert!(allowed("fl-steps", "fl-store"));
}
