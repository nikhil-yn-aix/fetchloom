//! `fetchloom diff`: what data.lock and its lists in data.lock.d changed between git commits, for
//! a project in a folder of its repository, with the git this machine has.

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::json;

use crate::build::building_in;
use crate::merge::ok;
use crate::recipes::{DRAWS_7, DRAWS_8};
use crate::support::{Result, Sandbox, json, sha256_hex, text};

/// data.toml of recipes that read no dataset, each a name and the words of its step after the
/// program: `seeded` draws from `SEED`, `write` writes one file.
fn recipes(folder: &Path, steps: &[(&str, &str, &str)]) -> Result {
    let tables: Vec<String> = steps
        .iter()
        .map(|(name, words, seed)| {
            format!(
                "[datasets.{name}]\nrun = [\"recipe-step\", {words}, \"{{out}}\"]\nenv = {{ SEED = \"{seed}\" }}\n"
            )
        })
        .collect();
    fs::write(folder.join("data.toml"), tables.join("\n"))?;
    Ok(())
}

/// fetchloom with `args` in `folder`, without the git variables of whatever runs the tests.
fn run(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Output> {
    let mut command = sandbox.command_in(folder, args);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    Ok(command.output()?)
}

fn built(sandbox: &Sandbox, folder: &Path) -> Result {
    let output = building_in(sandbox, folder, &["build"])?.output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// a project in `study` below the repository's top: a seeded draw changes, one recipe goes and
/// another comes, each file a line as `git diff --name-status` gives one, then the same between
/// two commits as `--json` gives it.
#[test]
fn diff_names_each_dataset_and_file_a_change_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    let draws = ("sim", "\"seeded\", \"draws.csv\"", "7");
    recipes(
        &study,
        &[draws, ("notes", "\"write\", \"notes.txt\", \"hello\"", "1")],
    )?;
    built(&sandbox, &study)?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "seven"])?;
    let same = run(&sandbox, &study, &["diff"])?;
    assert_eq!(same.status.code(), Some(0), "{}", text(&same.stderr)?);
    assert_eq!(text(&same.stdout)?, "");
    assert!(
        text(&same.stderr)?
            .contains("Ok nothing differs in data.lock between HEAD and the project folder\n"),
        "{}",
        text(&same.stderr)?
    );

    let eight = ("sim", "\"seeded\", \"draws.csv\"", "8");
    recipes(
        &study,
        &[eight, ("extra", "\"write\", \"extra.csv\", \"x\"", "1")],
    )?;
    built(&sandbox, &study)?;
    let output = run(&sandbox, &study, &["diff"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let told = text(&output.stdout)?;
    assert_eq!(
        told,
        "extra added: made by a recipe; 1 file added, 1 B\n\
         A  extra/extra.csv  1 B\n\
         notes removed: made by a recipe; 1 file removed, 5 B\n\
         D  notes/notes.txt  5 B\n\
         sim changed: env SEED changed; 1 file changed\n\
         M  sim/draws.csv  20 B \u{2192} 19 B\n"
    );
    let quiet = text(&run(&sandbox, &study, &["-q", "diff"])?.stdout)?;
    assert_eq!(
        quiet.lines().collect::<Vec<_>>(),
        told.lines()
            .filter(|line| !line.starts_with(['A', 'D', 'M']))
            .collect::<Vec<_>>(),
        "-q keeps a line for each dataset alone"
    );

    ok(&sandbox, &["add", "-A"])?;
    ok(&sandbox, &["commit", "-qm", "eight"])?;
    let output = run(&sandbox, &study, &["--json", "diff", "HEAD~1", "HEAD"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let result = &document["result"];
    assert_eq!(result["from"]["revision"], "HEAD~1");
    assert_eq!(result["to"]["revision"], "HEAD");
    let changes: Vec<(&str, &str)> = result["datasets"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|dataset| {
            (
                dataset["name"].as_str().unwrap_or_default(),
                dataset["change"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [("extra", "added"), ("notes", "removed"), ("sim", "changed")]
    );
    assert_eq!(
        result["datasets"][2]["files"],
        json!([{
            "path": "draws.csv",
            "change": "changed",
            "before": {"size": DRAWS_7.len(), "sha256": sha256_hex(DRAWS_7)},
            "after": {"size": DRAWS_8.len(), "sha256": sha256_hex(DRAWS_8)},
        }])
    );
    Ok(())
}

/// a revision is never read as an option, whatever it starts with, and a project outside any
/// repository is told it has no commits to compare.
#[test]
fn diff_reads_a_revision_as_one_and_needs_a_repository() -> Result {
    let sandbox = Sandbox::new()?;
    recipes(
        &sandbox.work(),
        &[("sim", "\"seeded\", \"draws.csv\"", "7")],
    )?;
    built(&sandbox, &sandbox.work())?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "seven"])?;
    let output = run(
        &sandbox,
        &sandbox.work(),
        &["--json", "diff", "--", "--output=planted"],
    )?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "\"--output=planted\" names no commit in this repository"
    );
    assert!(!sandbox.work().join("planted").exists());

    let alone = sandbox.folder("alone")?;
    fs::copy(sandbox.work().join("data.toml"), alone.join("data.toml"))?;
    let outside = sandbox
        .command_in(&alone, &["diff"])
        .env("GIT_CEILING_DIRECTORIES", sandbox.folder("")?)
        .output()?;
    assert_eq!(outside.status.code(), Some(2));
    let stderr = text(&outside.stderr)?;
    assert!(
        stderr.contains("is not in a git repository, and diff compares its commits"),
        "{stderr}"
    );
    Ok(())
}

/// a commit made with `git commit -a` lacks the new list of a recipe it pins: diff says so for
/// that dataset and goes on. a side with no data.lock and a dataset data.toml names that
/// data.lock does not pin are each said.
#[test]
fn diff_says_what_a_commit_lacks_and_goes_on() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    recipes(&study, &[("sim", "\"seeded\", \"draws.csv\"", "7")])?;
    built(&sandbox, &study)?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "seven"])?;
    recipes(&study, &[("sim", "\"seeded\", \"draws.csv\"", "8")])?;
    built(&sandbox, &study)?;
    ok(
        &sandbox,
        &["commit", "-qam", "eight, its new list left out"],
    )?;
    let output = run(&sandbox, &study, &["diff", "HEAD~1", "HEAD"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        text(&output.stdout)?,
        "sim changed: env SEED changed; its file list is not in HEAD, so its files were not compared\n"
    );
    assert!(stderr.contains("`git add data.lock.d`"), "{stderr}");
    let document = json(&run(
        &sandbox,
        &study,
        &["--json", "diff", "HEAD~1", "HEAD"],
    )?)?;
    assert_eq!(document["result"]["datasets"][0]["unlisted"], json!(["to"]));

    let mut tables = fs::read_to_string(study.join("data.toml"))?;
    tables.push_str(
        "\n[datasets.more]\nrun = [\"recipe-step\", \"write\", \"more.txt\", \"m\", \"{out}\"]\n",
    );
    fs::write(study.join("data.toml"), tables)?;
    fs::remove_file(study.join("data.lock"))?;
    let output = run(&sandbox, &study, &["diff"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("the project folder has no data.lock, so every dataset counts as removed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("data.toml names 2 datasets that data.lock does not pin yet, so diff cannot show them: `fetchloom build` pins them"),
        "{stderr}"
    );
    Ok(())
}

/// a range, an empty name, a commit whose data.lock cannot be read and a shallow clone are each
/// told plainly, with what to do instead.
#[test]
fn diff_tells_a_revision_it_cannot_compare() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    recipes(&study, &[("sim", "\"seeded\", \"draws.csv\"", "7")])?;
    built(&sandbox, &study)?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "seven"])?;
    for (revision, said, help) in [
        (
            "HEAD~1..HEAD",
            "\"HEAD~1..HEAD\" names no commit in this repository",
            "diff takes two commits, not a range: `fetchloom diff HEAD~1 HEAD`",
        ),
        (
            "",
            "\"\" names no commit in this repository",
            "name a commit, a branch or a tag",
        ),
    ] {
        let document = json(&run(&sandbox, &study, &["--json", "diff", revision])?)?;
        let error = &document["errors"][0];
        assert_eq!(error["message"], said, "{document}");
        assert!(
            error["help"].as_str().unwrap_or_default().starts_with(help),
            "{document}"
        );
    }

    let kept = fs::read_to_string(study.join("data.lock"))?;
    fs::write(study.join("data.lock"), "version = \"one\"\n")?;
    ok(&sandbox, &["commit", "-qam", "a broken data.lock"])?;
    fs::write(study.join("data.lock"), &kept)?;
    let document = json(&run(&sandbox, &study, &["--json", "diff", "HEAD"])?)?;
    let error = &document["errors"][0];
    assert_eq!(
        error["details"],
        json!(["in data.lock as HEAD has it"]),
        "{document}"
    );
    assert_eq!(
        error["help"],
        "that commit's data.lock stays as it was committed, so compare other commits"
    );

    let origin = sandbox.work().display().to_string().replace('\\', "/");
    let shallow = sandbox.folder("shallow")?;
    let into = shallow.join("clone").display().to_string();
    ok(
        &sandbox,
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{origin}"),
            &into,
        ],
    )?;
    let document = json(&run(
        &sandbox,
        &shallow.join("clone").join("study"),
        &["--json", "diff", "HEAD~1"],
    )?)?;
    assert!(
        document["errors"][0]["help"]
            .as_str()
            .unwrap_or_default()
            .starts_with("this clone is shallow"),
        "{document}"
    );
    Ok(())
}
