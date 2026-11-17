//! lineage: `add --from` records the datasets one was made from, with each one's files as they
//! were, and status, `lock --check` and sync say when those files changed since. it is a record:
//! nothing runs a pipeline or checks that the files came from those inputs.

use std::fs;
use std::path::PathBuf;

use crate::support::{
    Result, Routes, Sandbox, delete, file, json, kinds, ok, sha256_hex, text, warning, zip_of,
};

const PATH: &str = "/files/iris.csv";
const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const NEWER: &[u8] = b"sepal_length,species\n6.3,virginica\n";
const COUNTS: &[u8] = b"species,count\nsetosa,1\n";

/// the tree hash of `files`, as data.lock gives an archive's `unpacks_to`, worked out here apart
/// from fetchloom's code: the SHA-256 of a line per file, by path, of its path, size and SHA-256.
fn tree(files: &[(&str, &[u8])]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .map(|(path, bytes)| format!("{path}\t{}\t{}\n", bytes.len(), sha256_hex(bytes)))
        .collect();
    lines.sort();
    format!("sha256:{}", sha256_hex(lines.concat().as_bytes()))
}

/// a folder beside the project holding `counts.csv`, the output a pipeline made.
fn output(sandbox: &Sandbox) -> Result<PathBuf> {
    let folder = sandbox.folder("processed")?;
    fs::write(folder.join("counts.csv"), COUNTS)?;
    Ok(folder)
}

/// a project whose iris comes from a server, with `processed` added from a folder as made from it.
fn made_from_iris(sandbox: &Sandbox) -> Result<Routes> {
    made(sandbox, &["--from", "iris"])
}

/// a project whose iris comes from a server, with `processed` added from a folder, `from` the
/// last words of its add.
fn made(sandbox: &Sandbox, from: &[&str]) -> Result<Routes> {
    let routes = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}{PATH}", routes.url)])?)?;
    output(sandbox)?;
    let mut add = vec!["add", "../processed", "--as", "processed"];
    add.extend(from);
    ok(&sandbox.run(&add)?)?;
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

/// the stale warning a person reads for `processed` made from iris.
const STALE: &str = "warning: processed may be stale: iris's files changed since processed was pinned from them\n  help: rerun your pipeline, then `fetchloom add <folder> --as processed`\n";

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

#[test]
fn status_lock_check_and_sync_each_say_once_that_an_output_may_be_stale() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    for args in [&["status"][..], &["lock", "--check"], &["sync"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("stale"), "{args:?}: {stderr}");
    }
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        warning(&document, "derived.stale"),
        &serde_json::Value::Null
    );
    iris_changed(&sandbox, &routes)?;
    for args in [
        &["status"][..],
        &["lock", "--check"],
        &["sync"],
        &["status"],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(stderr.matches(STALE).count(), 1, "{args:?}: {stderr}");
        assert_eq!(stderr.matches("stale").count(), 1, "{args:?}: {stderr}");
    }
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let stale = warning(&document, "derived.stale");
    assert_eq!(
        stale,
        &serde_json::json!({
            "kind": "derived.stale",
            "message": "processed may be stale: iris's files changed since processed was pinned from them",
            "details": [],
            "help": "rerun your pipeline, then `fetchloom add <folder> --as processed`",
            "dataset": "processed",
            "input": "iris",
            "recorded": tree(&[("iris.csv", IRIS)]),
            "current": tree(&[("iris.csv", NEWER)]),
            "reason": "changed",
        })
    );
    Ok(())
}

/// what each way of syncing says, once iris changed since `processed` was pinned from it in a
/// project [`made`] with `from`, with how many times it read data.toml and data.lock and found a
/// dataset's tree.
fn synced_each_way(sandbox: &Sandbox, from: &[&str]) -> Result<Vec<(String, [usize; 3])>> {
    let routes = made(sandbox, from)?;
    iris_changed(sandbox, &routes)?;
    let placed = sandbox
        .work()
        .join("data")
        .join("processed")
        .join("counts.csv");
    let traces = sandbox.folder("traces")?;
    let mut said = Vec::new();
    for (run, args) in [
        &["sync"][..],
        &["sync"],
        &["sync", "processed"],
        &["sync", "iris"],
        &["sync", "--locked"],
        &["sync", "processed"],
        &["sync"],
    ]
    .into_iter()
    .enumerate()
    {
        // the last two find a file to place again
        if run == 5 {
            delete(&placed)?;
        }
        let trace = traces.join(format!("{run}.json"));
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
        let reads = ["read_manifest", "read_lock", "tree"].map(|name| {
            events
                .iter()
                .filter(|event| event["ph"] == "B" && event["name"] == name)
                .count()
        });
        said.push((format!("{args:?}: {stderr}"), reads));
    }
    Ok(said)
}

#[test]
fn each_way_of_syncing_says_once_that_an_output_may_be_stale() -> Result {
    let sandbox = Sandbox::new()?;
    for (said, _) in synced_each_way(&sandbox, &["--from", "iris"])? {
        assert_eq!(said.matches(STALE).count(), 1, "{said}");
    }
    Ok(())
}

#[test]
fn a_from_costs_a_sync_no_second_read_and_no_tree_to_find() -> Result {
    let made_from = Sandbox::new()?;
    let without = Sandbox::new()?;
    let made_from = synced_each_way(&made_from, &["--from", "iris"])?;
    let without = synced_each_way(&without, &[])?;
    let reads = |said: &[(String, [usize; 3])], which: usize| -> Vec<usize> {
        said.iter().map(|(_, reads)| reads[which]).collect()
    };
    // a data.toml with a `from` is always parsed, where a settled sync may read a plain one
    // without its parser
    assert_eq!(reads(&made_from, 0), [1; 7]);
    assert_eq!(reads(&made_from, 1), reads(&without, 1));
    // iris's tree is the line data.lock gives it, and its plain files keep it while kept
    assert_eq!(reads(&made_from, 2), [0; 7]);
    Ok(())
}

#[test]
fn a_rerun_that_makes_the_same_files_ends_the_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    iris_changed(&sandbox, &routes)?;
    assert!(text(&sandbox.run(&["status"])?.stderr)?.contains(STALE));
    ok(&sandbox.run(&["add", "../processed", "--as", "processed"])?)?;
    let stderr = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(!stderr.contains("stale"), "{stderr}");
    Ok(())
}

#[test]
fn only_the_files_an_input_gives_count_not_its_link_or_its_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    routes.change(vec![(
        "/elsewhere/iris.csv".to_owned(),
        file(IRIS, "\"v9\""),
    )]);
    let toml = read(&sandbox, "data.toml")?.replace(
        &format!("ref = \"{}{PATH}\"\n", routes.url),
        &format!(
            "ref = \"{}/elsewhere/iris.csv\"\npath = \"flowers\"\ncopy = true\n",
            routes.url
        ),
    );
    assert!(toml.contains("elsewhere"), "{toml}");
    fs::write(sandbox.work().join("data.toml"), toml)?;
    ok(&sandbox.run(&["sync"])?)?;
    for args in [&["status"][..], &["lock", "--check"], &["sync"]] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("stale"), "{args:?}: {stderr}");
    }
    Ok(())
}

/// what status, `lock --check` and sync say of `processed` once `change` has run on a project
/// made from iris: the warning `--json status` gives, and whether `lock --check` says it too.
fn unchecked(
    change: impl FnOnce(&Sandbox, &Routes) -> Result,
    check: bool,
) -> Result<serde_json::Value> {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    change(&sandbox, &routes)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let found = warning(&document, "derived.unchecked").clone();
    let said = format!(
        "warning: {}\n  help: {}\n",
        found["message"].as_str().unwrap_or_default(),
        found["help"].as_str().unwrap_or_default()
    );
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert_eq!(status.matches(&said).count(), 1, "{status}");
    if check {
        let output = sandbox.run(&["lock", "--check"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert_eq!(stderr.matches(&said).count(), 1, "{stderr}");
    }
    // sync says only what it knows changed, never what it cannot tell
    let synced = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(!synced.contains("cannot tell"), "{synced}");
    assert!(!synced.contains("stale"), "{synced}");
    Ok(found)
}

#[test]
fn an_input_data_toml_no_longer_has_is_missing() -> Result {
    let found = unchecked(|sandbox, _| ok(&sandbox.run(&["remove", "iris"])?), true)?;
    assert_eq!(
        found,
        serde_json::json!({
            "kind": "derived.unchecked",
            "message": "cannot tell whether processed is stale: data.toml has no dataset iris",
            "details": [],
            "help": "add iris back, or take it out of processed's from in data.toml",
            "dataset": "processed",
            "input": "iris",
            "recorded": tree(&[("iris.csv", IRIS)]),
            "current": null,
            "reason": "missing",
        })
    );
    Ok(())
}

#[test]
fn an_input_renamed_in_data_toml_is_found_by_its_files() -> Result {
    let renamed = |sandbox: &Sandbox, _: &Routes| {
        let toml = read(sandbox, "data.toml")?.replace("[datasets.iris]", "[datasets.flowers]");
        fs::write(sandbox.work().join("data.toml"), toml)?;
        ok(&sandbox.run(&["lock"])?)
    };
    let found = unchecked(renamed, true)?;
    let then = tree(&[("iris.csv", IRIS)]);
    assert_eq!(
        found,
        serde_json::json!({
            "kind": "derived.unchecked",
            "message": "cannot tell whether processed is stale: data.toml has no dataset iris, though flowers has the files iris had",
            "details": ["flowers"],
            "help": "record the new name with `fetchloom add <folder> --as processed --from flowers`",
            "dataset": "processed",
            "input": "iris",
            "recorded": then,
            "current": then,
            "reason": "renamed",
        })
    );
    Ok(())
}

#[test]
fn an_input_data_lock_does_not_pin_yet_is_unlocked() -> Result {
    let unpinned = |sandbox: &Sandbox, routes: &Routes| {
        routes.change(vec![(
            "/files/extra.csv".to_owned(),
            file(b"x\n", "\"e1\""),
        )]);
        let toml = read(sandbox, "data.toml")?
            .replace("from = [\"iris\"]", "from = [\"iris\", \"extra\"]");
        let toml = format!(
            "{toml}\n[datasets.extra]\nref = \"{}/files/extra.csv\"\n",
            routes.url
        );
        fs::write(sandbox.work().join("data.toml"), toml)?;
        Ok(())
    };
    let found = unchecked(unpinned, false)?;
    assert_eq!(
        found,
        serde_json::json!({
            "kind": "derived.unchecked",
            "message": "cannot tell whether processed is stale: data.lock does not pin extra's files yet",
            "details": [],
            "help": "`fetchloom sync` pins and places them",
            "dataset": "processed",
            "input": "extra",
            "recorded": null,
            "current": null,
            "reason": "unlocked",
        })
    );
    Ok(())
}

#[test]
fn an_input_without_a_record_is_unresolved() -> Result {
    let unrecorded = |sandbox: &Sandbox, _: &Routes| {
        let penguins = sandbox.folder("penguins")?;
        fs::write(penguins.join("penguins.csv"), b"species\nadelie\n")?;
        ok(&sandbox.run(&["add", "../penguins"])?)?;
        let toml = read(sandbox, "data.toml")?
            .replace("from = [\"iris\"]", "from = [\"iris\", \"penguins\"]");
        fs::write(sandbox.work().join("data.toml"), toml)?;
        Ok(())
    };
    let found = unchecked(unrecorded, true)?;
    assert_eq!(
        found,
        serde_json::json!({
            "kind": "derived.unchecked",
            "message": "cannot tell whether processed is stale: data.lock has no record of penguins's files from when processed was pinned",
            "details": [],
            "help": "if processed is made from penguins's files as they are now, `fetchloom add <folder> --as processed` records them",
            "dataset": "processed",
            "input": "penguins",
            "recorded": null,
            "current": tree(&[("penguins.csv", b"species\nadelie\n")]),
            "reason": "unresolved",
        })
    );
    Ok(())
}

#[test]
fn an_input_whose_files_were_not_known_is_unresolved() -> Result {
    let unknown = |sandbox: &Sandbox, _: &Routes| {
        let then = tree(&[("iris.csv", IRIS)]);
        let lock = read(sandbox, "data.lock")?
            .replace(&recorded("iris", &then), &recorded("iris", "unknown"));
        fs::write(sandbox.work().join("data.lock"), lock)?;
        Ok(())
    };
    let found = unchecked(unknown, true)?;
    assert_eq!(
        found["message"],
        "cannot tell whether processed is stale: iris's files were not known on the machine that pinned processed"
    );
    assert_eq!(found["recorded"], "unknown");
    assert_eq!(found["current"], tree(&[("iris.csv", IRIS)]));
    assert_eq!(found["reason"], "unresolved");
    Ok(())
}

/// a project whose iris is a zip of two files from a server, with `processed` made from it.
fn made_from_a_zip(sandbox: &Sandbox) -> Result<Routes> {
    let zip = zip_of(&[("iris/a.csv", b"a\n"), ("iris/b.csv", b"b\n")])?;
    let routes = Routes::start(vec![("/files/iris.zip".to_owned(), file(&zip, "\"z1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.zip", routes.url)])?)?;
    output(sandbox)?;
    ok(&sandbox.run(&["add", "../processed", "--as", "processed", "--from", "iris"])?)?;
    Ok(routes)
}

#[test]
fn a_members_table_that_keeps_other_files_changes_what_a_pipeline_saw() -> Result {
    let sandbox = Sandbox::new()?;
    let _routes = made_from_a_zip(&sandbox)?;
    let both = tree(&[("a.csv", b"a\n"), ("b.csv", b"b\n")]);
    assert!(read(&sandbox, "data.lock")?.contains(&recorded("iris", &both)));
    let toml = format!(
        "{}\n[datasets.iris.members.\"iris.zip\"]\nselect = [\"iris/a.csv\"]\n",
        read(&sandbox, "data.toml")?
    );
    fs::write(sandbox.work().join("data.toml"), toml)?;
    // the tree data.lock gives iris is the whole archive's until data.lock is written again
    let output = sandbox.run(&["--json", "lock", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["details"],
        serde_json::json!(["iris's files are not the tree data.lock gives them"])
    );
    ok(&sandbox.run(&["lock"])?)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let stale = warning(&document, "derived.stale");
    assert_eq!(stale["recorded"], both.as_str());
    assert_eq!(stale["current"], tree(&[("a.csv", b"a\n")]).as_str());
    Ok(())
}

#[test]
fn an_archive_never_unpacked_on_this_machine_leaves_its_input_unresolved() -> Result {
    let sandbox = Sandbox::new()?;
    let _routes = made_from_a_zip(&sandbox)?;
    // as a machine that never unpacked iris leaves its pin
    let lock = with_tree(&read(&sandbox, "data.lock")?, "iris", None)?;
    fs::write(sandbox.work().join("data.lock"), lock)?;
    let output = sandbox
        .command(&["--json", "status"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    let document = json(&output)?;
    let found = warning(&document, "derived.unchecked");
    assert_eq!(
        found["message"],
        "cannot tell whether processed is stale: this machine does not know iris's files"
    );
    assert_eq!(found["help"], "`fetchloom sync iris` brings them");
    assert_eq!(found["current"], "unknown");
    assert_eq!(found["reason"], "unresolved");
    assert_eq!(
        warning(&document, "derived.stale"),
        &serde_json::Value::Null
    );
    Ok(())
}

#[test]
fn staleness_is_direct_only() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    let summary = sandbox.folder("summary")?;
    fs::write(summary.join("summary.txt"), b"1 species\n")?;
    let added = sandbox.run(&[
        "add",
        "../summary",
        "--as",
        "summary",
        "--from",
        "processed",
    ])?;
    ok(&added)?;
    iris_changed(&sandbox, &routes)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let stale: Vec<&str> = document["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|warning| warning["kind"] == "derived.stale")
        .filter_map(|warning| warning["dataset"].as_str())
        .collect();
    assert_eq!(stale, ["processed"], "summary's own input did not change");
    Ok(())
}

#[test]
fn inputs_that_changed_together_make_one_line_and_a_warning_each() -> Result {
    let sandbox = Sandbox::new()?;
    let penguins = "/files/penguins.csv";
    let routes = Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        (penguins.to_owned(), file(b"species\nadelie\n", "\"p1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for path in [PATH, penguins] {
        ok(&sandbox.run(&["add", &format!("{}{path}", routes.url)])?)?;
    }
    output(&sandbox)?;
    let args = [
        "add",
        "../processed",
        "--as",
        "processed",
        "--from",
        "penguins",
        "--from",
        "iris",
    ];
    ok(&sandbox.run(&args)?)?;
    assert!(read(&sandbox, "data.toml")?.contains("from = [\"penguins\", \"iris\"]\n"));
    routes.change(vec![
        (PATH.to_owned(), file(NEWER, "\"v2\"")),
        (penguins.to_owned(), file(b"species\ngentoo\n", "\"p2\"")),
    ]);
    ok(&sandbox.run(&["lock", "--upgrade"])?)?;
    let stderr = text(&sandbox.run(&["status"])?.stderr)?;
    assert_eq!(
        stderr.matches("warning: processed may be stale: penguins's and iris's files changed since processed was pinned from them\n").count(),
        1,
        "{stderr}"
    );
    assert_eq!(stderr.matches("stale").count(), 1, "{stderr}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let inputs: Vec<&str> = document["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|warning| warning["kind"] == "derived.stale")
        .filter_map(|warning| warning["input"].as_str())
        .collect();
    assert_eq!(inputs, ["penguins", "iris"], "in the order from names them");
    Ok(())
}

#[test]
fn strict_makes_lock_check_fail_on_a_dataset_that_may_be_stale() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    ok(&sandbox.run(&["lock", "--check", "--strict"])?)?;
    iris_changed(&sandbox, &routes)?;
    ok(&sandbox.run(&["lock", "--check"])?)?;
    let output = sandbox.run(&["lock", "--check", "--strict"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("error: processed may be stale: iris's files changed since processed was pinned from them\n"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("warning:"),
        "the error says it once: {stderr}"
    );
    let output = sandbox.run(&["--json", "lock", "--check", "--strict"])?;
    assert_eq!(output.status.code(), Some(1));
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "derived.stale");
    assert_eq!(
        error["message"],
        "processed may be stale: iris's files changed since processed was pinned from them"
    );
    assert_eq!(
        error["help"],
        "rerun your pipeline, then `fetchloom add <folder> --as processed`"
    );
    assert!(!kinds(&document).contains(&"derived.stale"), "{document}");
    // a data.lock behind data.toml is what a check says first
    let toml = format!(
        "{}\n[datasets.extra]\nref = \"{}/files/extra.csv\"\n",
        read(&sandbox, "data.toml")?,
        routes.url
    );
    fs::write(sandbox.work().join("data.toml"), toml)?;
    let output = sandbox.run(&["--json", "lock", "--check", "--strict"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)?["errors"][0]["kind"], "lock.outdated");
    let output = sandbox.run(&["lock", "--strict"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    assert_eq!(
        text(&output.stderr)?,
        "error: --strict needs --check\n  usage: fetchloom lock [options]\n  help: add --check, or leave out --strict\n"
    );
    Ok(())
}

#[test]
fn an_input_no_one_can_check_never_fails_the_check() -> Result {
    let sandbox = Sandbox::new()?;
    made_from_iris(&sandbox)?;
    ok(&sandbox.run(&["remove", "iris"])?)?;
    let output = sandbox.run(&["--json", "lock", "--check", "--strict"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(kinds(&json(&output)?), ["derived.unchecked"]);
    Ok(())
}

/// copies of data.toml and data.lock in a folder of their own, as a ci runner's clone, with the
/// path of a cache folder that does not exist yet.
fn clone_without_cache(sandbox: &Sandbox) -> Result<(PathBuf, PathBuf)> {
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    Ok((clone, sandbox.folder("runner")?.join("cache")))
}

/// `lock` with `dataset`'s `tree` line set to `tree`, in place of the one it has, or after its
/// `locked` line; with no `tree`, the line taken out.
fn with_tree(lock: &str, dataset: &str, tree: Option<&str>) -> Result<String> {
    let start = lock
        .find(&format!("name = \"{dataset}\""))
        .ok_or("no such dataset")?;
    let end = lock[start..]
        .find("\n[[dataset]]")
        .map_or(lock.len(), |end| start + end);
    let mut lines: Vec<String> = lock[start..end].lines().map(str::to_owned).collect();
    lines.retain(|line| !line.starts_with("tree = "));
    if let Some(tree) = tree {
        let at = lines
            .iter()
            .position(|line| line.starts_with("locked = "))
            .ok_or("no locked line")?;
        lines.insert(at + 1, format!("tree = \"{tree}\""));
    }
    Ok(format!(
        "{}{}\n{}",
        &lock[..start],
        lines.join("\n"),
        &lock[end..]
    ))
}

/// the kinds of the lineage warnings of a `--json` document.
fn derived(document: &serde_json::Value) -> Vec<&str> {
    kinds(document)
        .into_iter()
        .filter(|kind| kind.starts_with("derived."))
        .collect()
}

#[test]
fn a_clone_without_a_cache_tells_a_changed_archive_input_from_data_lock_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_a_zip(&sandbox)?;
    let newer = zip_of(&[("iris/a.csv", b"A\n"), ("iris/b.csv", b"b\n")])?;
    routes.change(vec![("/files/iris.zip".to_owned(), file(&newer, "\"z2\""))]);
    ok(&sandbox.run(&["lock", "--upgrade", "iris"])?)?;
    let asked = routes.requests().len();
    let (clone, cache) = clone_without_cache(&sandbox)?;
    let output = sandbox
        .command_in(&clone, &["--json", "lock", "--check", "--strict"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "derived.stale");
    assert_eq!(
        error["message"],
        "processed may be stale: iris's files changed since processed was pinned from them"
    );
    assert_eq!(routes.requests().len(), asked, "a check asks no source");
    assert!(!cache.exists(), "a check makes no cache");
    Ok(())
}

#[test]
fn a_repin_to_the_same_files_raises_nothing_even_without_a_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_a_zip(&sandbox)?;
    // the same files packed again in another order: other archive bytes, the same files
    let repacked = zip_of(&[("iris/b.csv", b"b\n"), ("iris/a.csv", b"a\n")])?;
    let before = read(&sandbox, "data.lock")?;
    routes.change(vec![(
        "/files/iris.zip".to_owned(),
        file(&repacked, "\"z2\""),
    )]);
    ok(&sandbox.run(&["lock", "--upgrade", "iris"])?)?;
    assert_ne!(
        read(&sandbox, "data.lock")?,
        before,
        "the archive's pin changed"
    );
    for args in [
        &["--json", "status"][..],
        &["--json", "lock", "--check"],
        &["--json", "sync"],
    ] {
        let document = json(&sandbox.run(args)?)?;
        assert_eq!(
            derived(&document),
            Vec::<&str>::new(),
            "{args:?}: {document}"
        );
    }
    let (clone, cache) = clone_without_cache(&sandbox)?;
    for args in [
        &["--json", "lock", "--check", "--strict"][..],
        &["--json", "status"],
    ] {
        let output = sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        let document = json(&output)?;
        assert_eq!(
            derived(&document),
            Vec::<&str>::new(),
            "{args:?}: {document}"
        );
    }
    Ok(())
}

#[test]
fn a_wrong_tree_in_data_lock_fails_lock_check_where_its_files_are_known() -> Result {
    let sandbox = Sandbox::new()?;
    made_from_iris(&sandbox)?;
    let right = tree(&[("iris.csv", IRIS)]);
    let wrong = tree(&[("iris.csv", NEWER)]);
    let lock = with_tree(&read(&sandbox, "data.lock")?, "iris", Some(&wrong))?;
    fs::write(sandbox.work().join("data.lock"), lock)?;
    let output = sandbox.run(&["--json", "lock", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "lock.outdated");
    assert_eq!(
        error["message"],
        "data.lock is out of date with the files it pins"
    );
    assert_eq!(
        error["details"],
        serde_json::json!(["iris's files are not the tree data.lock gives them"])
    );
    assert_eq!(error["help"], "run `fetchloom lock` and commit data.lock");
    ok(&sandbox.run(&["lock"])?)?;
    let lock = read(&sandbox, "data.lock")?;
    assert!(lock.contains(&format!("tree = \"{right}\"")), "{lock}");
    assert!(!lock.contains(&wrong), "{lock}");
    ok(&sandbox.run(&["lock", "--check", "--strict"])?)
}

#[test]
fn an_input_with_no_tree_line_nor_cache_stays_unresolved_and_never_fails() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_a_zip(&sandbox)?;
    let newer = zip_of(&[("iris/a.csv", b"A\n"), ("iris/b.csv", b"b\n")])?;
    routes.change(vec![("/files/iris.zip".to_owned(), file(&newer, "\"z2\""))]);
    ok(&sandbox.run(&["lock", "--upgrade", "iris"])?)?;
    // as a machine that never unpacked iris leaves its pin
    let lock = with_tree(&read(&sandbox, "data.lock")?, "iris", None)?;
    fs::write(sandbox.work().join("data.lock"), lock)?;
    let (clone, cache) = clone_without_cache(&sandbox)?;
    let output = sandbox
        .command_in(&clone, &["--json", "lock", "--check", "--strict"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(derived(&document), ["derived.unchecked"]);
    let unchecked = warning(&document, "derived.unchecked");
    assert_eq!(unchecked["reason"], "unresolved");
    assert_eq!(unchecked["current"], "unknown");
    Ok(())
}

#[test]
fn a_dataset_of_an_archive_whose_files_land_as_they_are_has_the_archives_tree() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("a.csv", b"a\n"), ("b.csv", b"b\n")])?;
    let routes = Routes::start(vec![("/files/iris.zip".to_owned(), file(&zip, "\"z1\""))])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.zip", routes.url)])?)?;
    let lock = read(&sandbox, "data.lock")?;
    let both = tree(&[("a.csv", b"a\n"), ("b.csv", b"b\n")]);
    assert!(lock.contains(&format!("tree = \"{both}\"")), "{lock}");
    assert!(lock.contains(&format!("unpacks_to = \"{both}\"")), "{lock}");
    Ok(())
}

#[test]
fn data_lock_gives_each_pin_the_tree_of_the_files_a_pipeline_sees() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = zip_of(&[("iris/a.csv", b"a\n"), ("iris/b.csv", b"b\n")])?;
    let reads = crate::decompress::gz(b"read\n")?;
    let routes = Routes::start(vec![
        ("/files/iris.zip".to_owned(), file(&zip, "\"z1\"")),
        ("/files/reads.fastq.gz".to_owned(), file(&reads, "\"g1\"")),
    ])?;
    sandbox.allow(&routes.url);
    ok(&sandbox.run(&["add", &format!("{}/files/iris.zip", routes.url)])?)?;
    let gzipped = format!("{}/files/reads.fastq.gz", routes.url);
    ok(&sandbox.run(&["add", &gzipped, "--as", "reads", "--decompress"])?)?;
    let lines = |sandbox: &Sandbox, name: &str| -> Result<String> {
        let lock = read(sandbox, "data.lock")?;
        let at = lock
            .find(&format!("name = \"{name}\""))
            .ok_or("no dataset")?;
        let line = lock[at..]
            .lines()
            .find(|line| line.starts_with("tree = ") || line.starts_with("files"))
            .unwrap_or_default();
        Ok(line.to_owned())
    };
    let both = tree(&[("a.csv", b"a\n"), ("b.csv", b"b\n")]);
    assert_eq!(lines(&sandbox, "iris")?, format!("tree = \"{both}\""));
    let unpacked = tree(&[("reads.fastq", b"read\n")]);
    assert_eq!(lines(&sandbox, "reads")?, format!("tree = \"{unpacked}\""));
    // a members table needs no new pin, so sync --locked leaves data.lock; lock writes the tree
    let plain = read(&sandbox, "data.toml")?;
    let chosen =
        format!("{plain}\n[datasets.iris.members.\"iris.zip\"]\nselect = [\"iris/a.csv\"]\n");
    fs::write(sandbox.work().join("data.toml"), &chosen)?;
    let before = read(&sandbox, "data.lock")?;
    ok(&sandbox.run(&["sync", "--locked"])?)?;
    assert_eq!(read(&sandbox, "data.lock")?, before);
    ok(&sandbox.run(&["lock"])?)?;
    let only_a = tree(&[("a.csv", b"a\n")]);
    assert_eq!(lines(&sandbox, "iris")?, format!("tree = \"{only_a}\""));
    fs::write(sandbox.work().join("data.toml"), &plain)?;
    ok(&sandbox.run(&["lock"])?)?;
    assert_eq!(lines(&sandbox, "iris")?, format!("tree = \"{both}\""));
    // packed, the archive is the file a pipeline sees
    let packed = plain.replace("[datasets.iris]\n", "[datasets.iris]\nextract = false\n");
    fs::write(sandbox.work().join("data.toml"), packed)?;
    ok(&sandbox.run(&["sync"])?)?;
    let whole = tree(&[("iris.zip", &zip)]);
    assert_eq!(lines(&sandbox, "iris")?, format!("tree = \"{whole}\""));
    Ok(())
}
