//! lineage: `add --from` records the datasets one was made from, with each one's files as they
//! were, and status, `lock --check` and sync say when those files changed since. it is a record:
//! nothing runs a pipeline or checks that the files came from those inputs.

use std::fs;

use crate::support::{Result, Routes, Sandbox, file, json, ok, sha256_hex, text};

const PATH: &str = "/files/iris.csv";
const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const NEWER: &[u8] = b"sepal_length,species\n6.3,virginica\n";
const COUNTS: &[u8] = b"species,count\nsetosa,1\n";

/// the identity version 1 gives a tree of `files`, worked out here apart from fetchloom's code:
/// the SHA-256 of `fetchloom tree 1` and a line per file, by path, of its path, size and SHA-256.
fn tree(files: &[(&str, &[u8])]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .map(|(path, bytes)| format!("{path}\t{}\t{}\n", bytes.len(), sha256_hex(bytes)))
        .collect();
    lines.sort();
    let text = format!("fetchloom tree 1\n{}", lines.concat());
    format!("sha256:{}", sha256_hex(text.as_bytes()))
}

fn read(sandbox: &Sandbox, name: &str) -> Result<String> {
    Ok(fs::read_to_string(sandbox.work().join(name))?)
}

/// the `from` lines data.lock writes for one input.
fn recorded(name: &str, tree: &str) -> String {
    format!("from = [\n  {{ name = \"{name}\", tree = \"{tree}\" }},\n]\n")
}

/// writes data.toml, two datasets whose `from` lists name each other.
fn circular(sandbox: &Sandbox) -> Result {
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.raw]\nref = \"https://example.org/raw.csv\"\nfrom = [\"clean\"]\n\n[datasets.clean]\nref = \"https://example.org/clean.csv\"\nfrom = [\"raw\"]\n",
    )?;
    Ok(())
}

#[test]
fn a_from_that_goes_round_in_a_circle_stops_every_command() -> Result {
    let sandbox = Sandbox::new()?;
    circular(&sandbox)?;
    let output = sandbox.run(&["--json", "status"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        "data.toml says clean was made from raw and raw from clean"
    );
    assert_eq!(
        error["details"],
        serde_json::json!([
            "data.toml:7   from = [\"raw\"]",
            "data.toml:3   from = [\"clean\"]"
        ])
    );
    Ok(())
}

/// iris and processed pinned from a server, with data.toml and data.lock written by hand to say
/// processed was made from iris's files when they were `tree`.
fn recorded_by_hand(sandbox: &Sandbox, tree: &str) -> Result<Routes> {
    let routes = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/processed.csv".to_owned(), file(COUNTS, "\"p1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for path in [PATH, "/files/processed.csv"] {
        ok(&sandbox.run(&["add", &format!("{}{path}", routes.url)])?)?;
    }
    let toml = read(sandbox, "data.toml")?.replace(
        "[datasets.processed]\n",
        "[datasets.processed]\nfrom = [\"iris\"]\n",
    );
    fs::write(sandbox.work().join("data.toml"), toml)?;
    let lock = read(sandbox, "data.lock")?;
    let at = lock
        .find("name = \"processed\"")
        .ok_or("no processed in data.lock")?;
    let files = at + lock[at..].find("files = [").ok_or("no files")?;
    let lock = format!(
        "{}{}{}",
        &lock[..files],
        recorded("iris", tree),
        &lock[files..]
    );
    fs::write(sandbox.work().join("data.lock"), lock)?;
    Ok(routes)
}

#[test]
fn ordinary_lock_and_sync_never_change_what_was_recorded() -> Result {
    let sandbox = Sandbox::new()?;
    let then = tree(&[("iris.csv", IRIS)]);
    let routes = recorded_by_hand(&sandbox, &then)?;
    let line = recorded("iris", &then);
    for args in [&["lock"][..], &["sync"], &["status"]] {
        ok(&sandbox.run(args)?)?;
        assert!(read(&sandbox, "data.lock")?.contains(&line), "{args:?}");
    }
    // both change at their source, and an upgrade pins each anew
    routes.change(vec![
        (PATH.to_owned(), file(NEWER, "\"v2\"")),
        (
            "/files/processed.csv".to_owned(),
            file(b"species,count\nvirginica,1\n", "\"p2\""),
        ),
    ]);
    ok(&sandbox.run(&["lock", "--upgrade"])?)?;
    let lock = read(&sandbox, "data.lock")?;
    assert!(lock.contains(&sha256_hex(NEWER)), "{lock}");
    assert!(
        lock.contains(&sha256_hex(b"species,count\nvirginica,1\n")),
        "{lock}"
    );
    assert!(
        lock.contains(&line),
        "only an add records what processed was made from: {lock}"
    );
    ok(&sandbox.run(&["sync"])?)?;
    assert!(read(&sandbox, "data.lock")?.contains(&line));
    Ok(())
}
