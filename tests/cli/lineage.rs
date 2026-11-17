//! lineage: `add --from` records the datasets one was made from, with each one's files as they
//! were, and status, `lock --check` and sync say when those files changed since. it is a record:
//! nothing runs a pipeline or checks that the files came from those inputs.

use std::fs;
use std::path::PathBuf;

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

/// a folder beside the project holding `counts.csv`, the output a pipeline made.
fn output(sandbox: &Sandbox) -> Result<PathBuf> {
    let folder = sandbox.folder("processed")?;
    fs::write(folder.join("counts.csv"), COUNTS)?;
    Ok(folder)
}

/// a project whose iris comes from a server, with `processed` added from a folder as made from it.
fn made_from_iris(sandbox: &Sandbox) -> Result<Routes> {
    let routes = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}{PATH}", routes.url)])?)?;
    output(sandbox)?;
    let added = sandbox.run(&["add", "../processed", "--as", "processed", "--from", "iris"])?;
    ok(&added)?;
    Ok(routes)
}

/// iris changed at its source, and data.lock pins its new bytes.
fn iris_changed(sandbox: &Sandbox, routes: &Routes) -> Result {
    routes.change(vec![(PATH.to_owned(), file(NEWER, "\"v2\""))]);
    ok(&sandbox.run(&["lock", "--upgrade", "iris"])?)
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

#[test]
fn add_from_records_each_input_with_its_files_as_they_are() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}{PATH}", routes.url)])?)?;
    output(&sandbox)?;
    let added = sandbox.run(&["add", "../processed", "--as", "processed", "--from", "iris"])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "   Recorded processed as made from iris; status says when iris's files change\n"
        ),
        "{stderr}"
    );
    let toml = read(&sandbox, "data.toml")?;
    assert!(
        toml.contains("[datasets.processed]\nref = \"local\"\nfrom = [\"iris\"]\n"),
        "{toml}"
    );
    let lock = read(&sandbox, "data.lock")?;
    let processed = &lock[lock.find("name = \"processed\"").ok_or("no processed")?..];
    assert!(
        processed.contains(&recorded("iris", &tree(&[("iris.csv", IRIS)]))),
        "{lock}"
    );
    Ok(())
}

#[test]
fn adding_an_output_again_keeps_its_from_and_records_its_inputs_anew() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    iris_changed(&sandbox, &routes)?;
    // the pipeline ran again on the new iris and made the same files
    let again = sandbox.run(&["add", "../processed", "--as", "processed"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("   Recorded processed as made from iris;"),
        "{stderr}"
    );
    assert!(read(&sandbox, "data.toml")?.contains("from = [\"iris\"]\n"));
    let lock = read(&sandbox, "data.lock")?;
    assert!(
        lock.contains(&recorded("iris", &tree(&[("iris.csv", NEWER)]))),
        "{lock}"
    );
    // nothing changed since, so the same add records nothing new and says nothing of it
    let same = sandbox.run(&["add", "../processed", "--as", "processed"])?;
    ok(&same)?;
    assert!(!text(&same.stderr)?.contains("Recorded"));
    assert_eq!(read(&sandbox, "data.lock")?, lock);
    Ok(())
}

#[test]
fn a_from_given_again_replaces_the_datasets_named_before() -> Result {
    let sandbox = Sandbox::new()?;
    made_from_iris(&sandbox)?;
    let penguins = sandbox.folder("penguins")?;
    fs::write(penguins.join("penguins.csv"), b"species\nadelie\n")?;
    ok(&sandbox.run(&["add", "../penguins"])?)?;
    let added = sandbox.run(&[
        "add",
        "../processed",
        "--as",
        "processed",
        "--from",
        "penguins",
    ])?;
    ok(&added)?;
    let toml = read(&sandbox, "data.toml")?;
    assert!(toml.contains("from = [\"penguins\"]\n"), "{toml}");
    assert!(!toml.contains("\"iris\""), "{toml}");
    let lock = read(&sandbox, "data.lock")?;
    let tree = tree(&[("penguins.csv", b"species\nadelie\n")]);
    assert!(lock.contains(&recorded("penguins", &tree)), "{lock}");
    assert!(!lock.contains("name = \"iris\", tree"), "{lock}");
    Ok(())
}

#[test]
fn from_names_other_datasets_data_toml_has() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    let link = format!("{}{PATH}", routes.url);
    let toml = read(&sandbox, "data.toml")?;
    let lock = read(&sandbox, "data.lock")?;
    for (args, message, help) in [
        (
            vec!["add", "../processed", "--as", "processed", "--from", "irs"],
            "--from names irs, which is not a dataset in data.toml",
            "did you mean iris?",
        ),
        (
            vec![
                "add",
                "../processed",
                "--as",
                "processed",
                "--from",
                "processed",
            ],
            "processed cannot be made from itself",
            "leave processed out of --from",
        ),
        (
            vec!["add", &link, "--as", "iris", "--from", "processed"],
            "data.toml would say iris was made from processed and processed from iris",
            "leave processed out of --from: a dataset cannot be made from itself, even by way of others",
        ),
        (
            vec!["add", &link, &link, "--from", "iris"],
            "--from records what one dataset was made from, and 2 links add 2",
            "add the link made from them on its own, as in `fetchloom add <link> --from <dataset>`",
        ),
    ] {
        let output = sandbox.run(&[&["--json"][..], &args].concat())?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "usage", "{args:?}");
        assert_eq!(error["message"], message, "{args:?}");
        assert_eq!(error["help"], help, "{args:?}");
        assert_eq!(read(&sandbox, "data.toml")?, toml);
        assert_eq!(read(&sandbox, "data.lock")?, lock);
    }
    let output = sandbox.run(&["add", &link, "--as", "other", "--no-sync", "--from", "iris"])?;
    assert_eq!(output.status.code(), Some(2));
    Ok(())
}
