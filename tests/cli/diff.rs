//! `fetchloom diff`: what data.lock and its lists in data.lock.d changed between git commits, for
//! a project in a folder of its repository, with the git this machine has.

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::json;

use crate::merge::{git_variables, ok};
use crate::support::{Fetchloom, Result, Routes, Sandbox, file, json, sha256_hex, text};

/// the draws a source holds first, and those a later version of it holds.
const FIRST: &[u8] = b"278\n231\n753\n673\n545\n";
const SECOND: &[u8] = b"244\n312\n12\n307\n454\n";

/// a server holding each `(name, bytes)` below `/files/`, which every later command may reach.
fn source(sandbox: &Sandbox, files: &[(&str, &[u8])]) -> Result<Routes> {
    let routes = Routes::start(
        files
            .iter()
            .map(|(name, bytes)| (format!("/files/{name}"), served(bytes)))
            .collect(),
    )?;
    sandbox.allow(&routes.url);
    Ok(routes)
}

/// `bytes` as a server gives them, tagged by what they hold.
fn served(bytes: &[u8]) -> Vec<crate::support::Part> {
    file(bytes, &format!("\"{}\"", &sha256_hex(bytes)[..12]))
}

/// fetchloom with `args` in `folder`, without the git variables of whatever runs the tests.
fn command(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Fetchloom {
    let mut command = sandbox.command_in(folder, args);
    for name in git_variables() {
        command.env_remove(name);
    }
    command
}

fn run(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Output> {
    Ok(command(sandbox, folder, args).output()?)
}

/// [`run`], which must succeed.
fn done(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result {
    let output = run(sandbox, folder, args)?;
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: {}",
        text(&output.stderr)?
    );
    Ok(())
}

/// under `LANG=C` a diff once wrote `M  sim/x->y.csv  20 B -> 19 B`, the file's own arrow turned
/// into ascii with the one between its sizes: only the arrow fetchloom writes between them is.
#[test]
fn an_ascii_locale_keeps_the_arrow_a_file_name_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    let named = "x%E2%86%92y.csv";
    let routes = source(&sandbox, &[(named, FIRST)])?;
    let url = format!("{}/files/{named}", routes.url);
    done(&sandbox, &study, &["add", &url, "--as", "sim"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
    routes.change(vec![(format!("/files/{named}"), served(SECOND))]);
    done(&sandbox, &study, &["lock", "--upgrade", "sim"])?;
    let ascii = command(&sandbox, &study, &["diff"]).in_ascii().output()?;
    let ascii = text(&ascii.stdout)?;
    assert!(
        ascii.ends_with("M  sim/x\u{2192}y.csv  20 B -> 19 B\n"),
        "{ascii}"
    );
    Ok(())
}

/// a project in `study` below the repository's top: a source's file changes, one dataset goes and
/// another comes, each file a line as `git diff --name-status` gives one, then the same between
/// two commits as `--json` gives it.
#[test]
fn diff_names_each_dataset_and_file_a_change_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    let routes = source(
        &sandbox,
        &[
            ("draws.csv", FIRST),
            ("notes.txt", b"hello"),
            ("extra.csv", b"x"),
        ],
    )?;
    let url = |name: &str| format!("{}/files/{name}", routes.url);
    done(&sandbox, &study, &["add", &url("draws.csv"), "--as", "sim"])?;
    done(
        &sandbox,
        &study,
        &["add", &url("notes.txt"), "--as", "notes"],
    )?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
    let same = run(&sandbox, &study, &["diff"])?;
    assert_eq!(same.status.code(), Some(0), "{}", text(&same.stderr)?);
    assert_eq!(text(&same.stdout)?, "");
    assert!(
        text(&same.stderr)?
            .contains("Ok nothing differs in data.lock between HEAD and the project folder\n"),
        "{}",
        text(&same.stderr)?
    );

    routes.change(vec![("/files/draws.csv".to_owned(), served(SECOND))]);
    done(&sandbox, &study, &["lock", "--upgrade", "sim"])?;
    done(&sandbox, &study, &["remove", "notes"])?;
    done(
        &sandbox,
        &study,
        &["add", &url("extra.csv"), "--as", "extra"],
    )?;
    let output = run(&sandbox, &study, &["diff"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let told = text(&output.stdout)?;
    assert_eq!(
        told,
        format!(
            "extra added: {}; 1 file added, 1 B\n\
             A  extra/extra.csv  1 B\n\
             notes removed: {}; 1 file removed, 5 B\n\
             D  notes/notes.txt  5 B\n\
             sim changed: 1 file changed\n\
             M  sim/draws.csv  20 B \u{2192} 19 B\n",
            url("extra.csv"),
            url("notes.txt")
        )
    );
    let quiet = text(&run(&sandbox, &study, &["-q", "diff"])?.stdout)?;
    assert_eq!(
        quiet.lines().collect::<Vec<_>>(),
        told.lines()
            .filter(|line| !line.starts_with(['A', 'D', 'M']))
            .collect::<Vec<_>>(),
        "-q keeps a line for each dataset alone"
    );
    let ascii = command(&sandbox, &study, &["diff"]).in_ascii().output()?;
    let ascii = text(&ascii.stdout)?;
    assert!(ascii.ends_with("20 B -> 19 B\n"), "{ascii}");

    ok(&sandbox, &["add", "-A"])?;
    ok(&sandbox, &["commit", "-qm", "second"])?;
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
            "before": {"size": FIRST.len(), "sha256": sha256_hex(FIRST)},
            "after": {"size": SECOND.len(), "sha256": sha256_hex(SECOND)},
        }])
    );
    Ok(())
}

/// a hand edit of data.lock that moves a file's link to another host and keeps its SHA-256, as a
/// pull request can, is told as that file at another link with both hosts, to a person and in
/// `--json`, since a clone then asks that host first. `lock --check` still passes: it holds
/// data.lock to what data.toml asks, which the edit left as it was.
#[test]
fn diff_names_a_link_a_hand_edit_moved() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let routes = source(&sandbox, &[("draws.csv", FIRST)])?;
    let link = format!("{}/files/draws.csv", routes.url);
    done(&sandbox, &work, &["add", &link, "--as", "sim"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
    let lock = work.join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let moved = "https://example.org/mirror/draws.csv";
    let edited = pinned.replace(&format!("url = \"{link}\""), &format!("url = \"{moved}\""));
    assert_ne!(edited, pinned, "{pinned}");
    fs::write(&lock, edited)?;

    let output = run(&sandbox, &work, &["diff"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?,
        "sim changed: 1 file at another link\n\
         M  sim/draws.csv  20 B, the same bytes, link 127.0.0.1 \u{2192} example.org\n"
    );
    let document = json(&run(&sandbox, &work, &["--json", "diff"])?)?;
    let sim = &document["result"]["datasets"][0];
    assert_eq!(sim["relinked"], 1, "{document}");
    assert_eq!(
        sim["files"],
        json!([{
            "path": "draws.csv",
            "change": "relinked",
            "before": {"size": FIRST.len(), "sha256": sha256_hex(FIRST)},
            "after": {"size": FIRST.len(), "sha256": sha256_hex(FIRST)},
            "link": {"before": link, "after": moved},
        }])
    );
    done(&sandbox, &work, &["lock", "--check"])?;
    Ok(())
}

/// a change of select or exclude names each side's patterns, as data.toml asks for them in the
/// project folder and in each commit.
#[test]
fn diff_names_the_patterns_a_dataset_changed() -> Result {
    let sandbox = Sandbox::new()?;
    // a listing as python's http.server writes it
    let listing = "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Directory listing for /files/</title>\n</head>\n<body>\n<h1>Directory listing for /files/</h1>\n<hr>\n<ul>\n<li><a href=\"a.csv\">a.csv</a></li>\n<li><a href=\"b.csv\">b.csv</a></li>\n</ul>\n<hr>\n</body>\n</html>\n";
    let length = listing.len().to_string();
    let html = [
        ("Content-Type", "text/html; charset=utf-8"),
        ("Content-Length", length.as_str()),
    ];
    let routes = Routes::start(vec![
        (
            "/files/".to_owned(),
            crate::support::reply("200 OK", &html, listing.as_bytes()),
        ),
        ("/files/a.csv".to_owned(), served(b"a\n")),
        ("/files/b.csv".to_owned(), served(b"b\n")),
    ])?;
    sandbox.allow(&routes.url);
    let work = sandbox.work();
    let folder = format!("{}/files/", routes.url);
    done(&sandbox, &work, &["add", &folder, "--as", "draws"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "both draws"])?;
    done(&sandbox, &work, &["exclude", "data/draws/b.csv"])?;
    let told =
        "draws changed: exclude none \u{2192} [\"b.csv\"]; 1 file removed\nD  draws/b.csv  2 B\n";
    let output = run(&sandbox, &work, &["diff"])?;
    assert_eq!(text(&output.stdout)?, told, "{}", text(&output.stderr)?);
    ok(&sandbox, &["commit", "-qam", "leave b out"])?;
    let output = run(&sandbox, &work, &["diff", "HEAD~1", "HEAD"])?;
    assert_eq!(text(&output.stdout)?, told, "{}", text(&output.stderr)?);
    Ok(())
}

/// a revision is never read as an option, whatever it starts with, and a project outside any
/// repository is told it has no commits to compare.
#[test]
fn diff_reads_a_revision_as_one_and_needs_a_repository() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = source(&sandbox, &[("draws.csv", FIRST)])?;
    let url = format!("{}/files/draws.csv", routes.url);
    done(&sandbox, &sandbox.work(), &["add", &url, "--as", "sim"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
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
    assert!(
        stderr.contains("if git keeps this one, `git status` says why git cannot read it"),
        "{stderr}"
    );
    Ok(())
}

/// a commit made with `git commit -a` lacks the new list of a dataset it pins: diff says so for
/// that dataset and goes on. a side with no data.lock and a dataset data.toml names that
/// data.lock does not pin are each said.
#[test]
fn diff_says_what_a_commit_lacks_and_goes_on() -> Result {
    let sandbox = Sandbox::new()?;
    let study = sandbox.work().join("study");
    fs::create_dir_all(&study)?;
    let routes = source(&sandbox, &[("draws.csv", FIRST)])?;
    // each file list goes to data.lock.d, as a big dataset's does
    let listed = |args: &[&str]| -> Result {
        let output = command(&sandbox, &study, args)
            .env("FETCHLOOM_TEST_INLINE", "0")
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        Ok(())
    };
    let url = format!("{}/files/draws.csv", routes.url);
    listed(&["add", &url, "--as", "sim"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
    routes.change(vec![("/files/draws.csv".to_owned(), served(SECOND))]);
    listed(&["lock", "--upgrade", "sim"])?;
    ok(
        &sandbox,
        &["commit", "-qam", "second, its new list left out"],
    )?;
    let output = run(&sandbox, &study, &["diff", "HEAD~1", "HEAD"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        text(&output.stdout)?,
        "sim changed: its file list is not in HEAD, so its files were not compared\n"
    );
    assert!(stderr.contains("`git add data.lock.d`"), "{stderr}");
    let document = json(&run(
        &sandbox,
        &study,
        &["--json", "diff", "HEAD~1", "HEAD"],
    )?)?;
    assert_eq!(document["result"]["datasets"][0]["unlisted"], json!(["to"]));

    let tables = fs::read_to_string(study.join("data.toml"))?;
    fs::write(
        study.join("data.toml"),
        format!(
            "{tables}\n[datasets.more]\nref = \"{}/files/more.txt\"\n",
            routes.url
        ),
    )?;
    fs::remove_file(study.join("data.lock"))?;
    let output = run(&sandbox, &study, &["diff"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("the project folder has no data.lock, so every dataset counts as removed"),
        "{stderr}"
    );
    assert!(
        stderr.contains("data.toml names 2 datasets that data.lock does not pin yet, so diff cannot show them: `fetchloom lock` pins them"),
        "{stderr}"
    );
    // run from the folder above, the command pins in this project
    let output = run(&sandbox, &sandbox.work(), &["-C", "study", "diff"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let hint = stderr
        .lines()
        .find(|line| line.contains("does not pin yet"))
        .unwrap_or_default();
    // windows quotes the folder, as in `fetchloom -C "C:\...\study" lock`
    assert!(
        hint.contains("`fetchloom -C ")
            && hint.contains("study")
            && hint.ends_with(" lock` pins them"),
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
    let routes = source(&sandbox, &[("draws.csv", FIRST)])?;
    let url = format!("{}/files/draws.csv", routes.url);
    done(&sandbox, &study, &["add", &url, "--as", "sim"])?;
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "first"])?;
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
