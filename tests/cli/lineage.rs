//! lineage: `add --from` records the datasets one was made from, with each one's files as they
//! were, and status, `lock --check` and sync say when those files changed since. it is a record:
//! nothing runs a pipeline or checks that the files came from those inputs.

use std::fs;
use std::path::PathBuf;

use crate::support::{
    Result, Routes, Sandbox, file, json, kinds, ok, sha256_hex, text, warning, zip_of,
};

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
        let lock = read(sandbox, "data.lock")?.replace(&then, "unknown");
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
fn fail_on_stale_makes_lock_check_fail_with_a_code_of_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = made_from_iris(&sandbox)?;
    ok(&sandbox.run(&["lock", "--check", "--fail-on-stale"])?)?;
    iris_changed(&sandbox, &routes)?;
    ok(&sandbox.run(&["lock", "--check"])?)?;
    let output = sandbox.run(&["lock", "--check", "--fail-on-stale"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(60), "{stderr}");
    assert!(
        stderr.contains("error: processed may be stale: iris's files changed since processed was pinned from them\n"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("warning:"),
        "the error says it once: {stderr}"
    );
    let output = sandbox.run(&["--json", "lock", "--check", "--fail-on-stale"])?;
    assert_eq!(output.status.code(), Some(60));
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
    let output = sandbox.run(&["--json", "lock", "--check", "--fail-on-stale"])?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)?["errors"][0]["kind"], "lock.outdated");
    let output = sandbox.run(&["--json", "lock", "--fail-on-stale"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["message"], "--fail-on-stale only goes with --check");
    assert_eq!(
        error["help"],
        "run `fetchloom lock --check --fail-on-stale`"
    );
    Ok(())
}

#[test]
fn an_input_no_one_can_check_never_fails_the_check() -> Result {
    let sandbox = Sandbox::new()?;
    made_from_iris(&sandbox)?;
    ok(&sandbox.run(&["remove", "iris"])?)?;
    let output = sandbox.run(&["--json", "lock", "--check", "--fail-on-stale"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(kinds(&json(&output)?), ["derived.unchecked"]);
    Ok(())
}
