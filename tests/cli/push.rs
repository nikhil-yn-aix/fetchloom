//! `fetchloom push`: pinned files copied from this machine's cache into a folder mirror, laid out
//! as mirrors are read.

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::Value;

use crate::add::sha256_hex as sha256;
use crate::build::{CODE, pin_clean, with_recipe};
use crate::caps::shown;
use crate::mirrors::{IRIS, MEMBERS, cold, holds_the_project, project};
use crate::support::{Result, Sandbox, Server, edit, file, json, text};

fn ok(output: &Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// the names of the files below `folder` of the mirror at `root`, each checked to hash to its
/// name, as a reader checks it.
fn named(root: &Path, folder: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for fan in fs::read_dir(root.join(folder))? {
        let fan = fan?;
        for file in fs::read_dir(fan.path())? {
            let file = file?;
            let name = format!(
                "{}{}",
                fan.file_name().to_string_lossy(),
                file.file_name().to_string_lossy()
            );
            assert_eq!(sha256(&fs::read(file.path())?), name, "{name}");
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// the SHA-256 of every file the project in [`project`] places, which a push copies.
fn pinned() -> Vec<String> {
    let mut hashes: Vec<String> = std::iter::once(IRIS)
        .chain(MEMBERS.iter().map(|(_, bytes)| *bytes))
        .map(sha256)
        .collect();
    hashes.sort();
    hashes
}

fn bytes() -> usize {
    IRIS.len() + MEMBERS.iter().map(|(_, bytes)| bytes.len()).sum::<usize>()
}

/// data.lock of the project in the sandbox's work folder, with `access` as given for each
/// dataset, as its source would have said.
fn access(sandbox: &Sandbox, given: &[(&str, &str)]) -> Result {
    let path = sandbox.work().join("data.lock");
    let mut lock = fs::read_to_string(&path)?;
    for (dataset, access) in given {
        let name = format!("name = \"{dataset}\"\n");
        assert!(lock.contains(&name), "{lock}");
        lock = lock.replace(&name, &format!("{name}access = \"{access}\"\n"));
    }
    fs::write(path, lock)?;
    Ok(())
}

/// the `push_to` setting names the folder push writes to: a relative one is taken from the
/// settings file's folder, as mirrors are, and `fetchloom settings` shows it with where it came
/// from.
#[test]
fn push_to_is_shown_with_where_it_came_from() -> Result {
    let sandbox = Sandbox::new()?;
    let settings = sandbox.work().join("settings.toml");
    fs::write(&settings, "push_to = \"lab, shared\"\n")?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SETTINGS", &settings)
            .output()?,
    )?;
    let push_to = shown(&document, "push_to");
    assert_eq!(
        push_to["value"],
        sandbox.work().join("lab, shared").display().to_string()
    );
    assert_eq!(push_to["origin"], "file");
    assert_eq!(push_to["variable"], "FETCHLOOM_PUSH_TO");
    let document = json(&sandbox.run(&["--json", "settings"])?)?;
    assert_eq!(shown(&document, "push_to")["value"], Value::Null);
    assert_eq!(shown(&document, "push_to")["origin"], "default");
    Ok(())
}

/// the closing `next:` line says where the settings file that names the mirror is.
#[cfg(target_os = "linux")]
#[test]
fn a_push_says_where_the_settings_file_is() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let root = sandbox.folder("lab")?.join("mirror");
    let push = sandbox.command(&["--yes", "push", "--to", &root.to_string_lossy()]);
    let Some(screen) = sandbox.on_a_terminal_running(&push)? else {
        return Ok(());
    };
    assert!(
        screen.contains(&format!(
            "next: teammates add mirrors = [\"{}\"] to their settings file, here ~/",
            root.display()
        )) && screen.contains("settings.toml; `fetchloom settings` shows where\n"),
        "{screen}"
    );
    Ok(())
}

/// a push starts the mirror, writes each pinned file and the archive's list of files under their
/// SHA-256, never the archive, and asks no server anything. the next push finds them after one
/// look each, and replaces a copy someone damaged.
#[test]
fn a_push_writes_each_file_once_and_skips_it_next_time() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let asked = source.requests().len();
    let root = sandbox.folder("lab")?.join("mirror");
    let output = sandbox
        .command(&["--yes", "push", "--to"])
        .arg(&root)
        .output()?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    assert_eq!(
        fs::read_to_string(root.join("fetchloom-mirror"))?,
        "fetchloom mirror 1\n"
    );
    assert_eq!(named(&root, "objects")?, pinned());
    assert_eq!(named(&root, "trees")?.len(), 1);
    assert_eq!(fs::read_dir(root.join("tmp"))?.count(), 0);
    assert_eq!(source.requests().len(), asked, "no server is asked");
    let hex = sha256(IRIS);
    edit(
        &root.join("objects").join(&hex[..2]).join(&hex[2..]),
        b"damaged",
    )?;
    let again = sandbox
        .command(&["--json", "--yes", "push", "--to"])
        .arg(&root)
        .output()?;
    ok(&again)?;
    let document = json(&again)?;
    let result = &document["result"];
    assert_eq!(result["target"], root.display().to_string());
    assert_eq!(result["started"], false);
    assert_eq!(result["datasets"], serde_json::json!(["dataset", "iris"]));
    assert_eq!(result["objects"], 6);
    assert_eq!(result["bytes"], bytes());
    assert_eq!(result["copied"], 1);
    assert_eq!(result["copied_bytes"], IRIS.len());
    assert_eq!(result["replaced"], 1);
    assert_eq!(result["present"], 5);
    assert_eq!(result["present_bytes"], bytes() - IRIS.len());
    assert_eq!(result["trees"], 0);
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(named(&root, "objects")?, pinned());
    Ok(())
}

/// a member a members table leaves out is not pushed: no clone places it, and gc may already
/// have freed its bytes, which would make the push refuse for want of them.
#[test]
fn a_member_left_out_of_a_dataset_is_not_pushed() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    ok(&sandbox.run(&["drop", "data/dataset/data/train.csv"])?)?;
    ok(&sandbox.run(&["cache", "gc", "--now"])?)?;
    let root = sandbox.folder("lab")?.join("mirror");
    let output = sandbox
        .command(&["--yes", "push", "--to"])
        .arg(&root)
        .output()?;
    ok(&output)?;
    let dropped = sha256(b"x,y\n1,2\n3,4\n");
    let mut kept = pinned();
    kept.retain(|hash| *hash != dropped);
    assert_eq!(named(&root, "objects")?, kept);
    Ok(())
}

/// a split archive goes to the mirror as its files and the list of them, never as its parts, and
/// a clone with an empty cache places it from there.
#[test]
fn a_split_archive_is_pushed_as_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fs::read(crate::support::workspace().join("tests/fixtures/archives/python.zip"))?;
    let scans = sandbox.folder("scans")?;
    let (first, second) = zip.split_at(zip.len() / 2);
    fs::write(scans.join("dataset.zip.001"), first)?;
    fs::write(scans.join("dataset.zip.002"), second)?;
    ok(&sandbox.run(&["add", "../scans", "--as", "dataset"])?)?;
    let root = sandbox.folder("lab")?.join("mirror");
    ok(&sandbox
        .command(&["--yes", "push", "--to"])
        .arg(&root)
        .output()?)?;
    let mut members: Vec<String> = MEMBERS.iter().map(|(_, bytes)| sha256(bytes)).collect();
    members.sort();
    assert_eq!(named(&root, "objects")?, members);
    assert_eq!(named(&root, "trees")?.len(), 1);
    let (folder, cache) = cold(&sandbox, "cold")?;
    ok(&sandbox
        .command_in(&folder, &["sync", "--mirror"])
        .arg(&root)
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_MIRRORS_ONLY", "1")
        .output()?)?;
    for (path, bytes) in MEMBERS {
        assert_eq!(
            fs::read(folder.join("data/dataset").join(path))?,
            bytes,
            "{path}"
        );
    }
    Ok(())
}

/// a dry run says what a push would write and what it would ask about, and writes nothing.
#[test]
fn a_dry_run_counts_what_it_would_write_and_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let root = sandbox.work().join("mirror");
    let output = sandbox
        .command(&["--json", "push", "--dry-run", "--to"])
        .arg(&root)
        .output()?;
    ok(&output)?;
    let result = &json(&output)?["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["started"], true);
    assert_eq!(result["planned"], 6);
    assert_eq!(result["copied"], 6);
    assert_eq!(result["copied_bytes"], bytes());
    assert_eq!(result["present"], 0);
    assert_eq!(result["trees"], 1);
    let mut copy: Vec<String> = result["changes"]["copy"]
        .as_array()
        .ok_or("no changes.copy")?
        .iter()
        .map(|file| file["sha256"].as_str().unwrap_or_default().to_owned())
        .collect();
    copy.sort();
    assert_eq!(copy, pinned());
    assert_eq!(
        result["asks"],
        serde_json::json!([
            {"dataset": "dataset", "access": "unknown"},
            {"dataset": "iris", "access": "unknown"}
        ])
    );
    assert!(!root.exists());
    Ok(())
}

/// a folder that holds files but no marker may be anything, such as the folder a network disk
/// mounts on, so push writes nothing there.
#[test]
fn a_folder_that_is_not_a_mirror_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let thesis = sandbox.folder("thesis")?;
    fs::write(thesis.join("draft.tex"), "chapter one")?;
    let output = sandbox
        .command(&["--json", "--yes", "push", "--to"])
        .arg(&thesis)
        .output()?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    let names: Vec<_> = fs::read_dir(&thesis)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(names, ["draft.tex"]);
    Ok(())
}

/// a `--to` with `..` in it is named by the folder it leads to, in the lines and in `--json`.
#[test]
fn a_target_is_named_without_dots() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let root = sandbox.folder("lab")?.join("mirror");
    let output = sandbox.run(&["--json", "push", "--to", "../lab/./mirror", "--dry-run"])?;
    ok(&output)?;
    assert_eq!(
        json(&output)?["result"]["target"],
        root.display().to_string()
    );
    let output = sandbox.run(&["push", "--to", "../lab/../lab/mirror", "--dry-run"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!("Would start a mirror in {}\n", root.display())),
        "{stderr}"
    );
    assert!(!stderr.contains(".."), "{stderr}");
    Ok(())
}

/// push downloads what the cache lacks, as a sync would, then pushes it: a clone that never
/// synced fills the mirror from the sources, and its project stays as it was.
#[test]
fn push_downloads_what_the_cache_lacks() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let asked = source.requests().len();
    let (folder, cache) = cold(&sandbox, "cold")?;
    let root = sandbox.work().join("mirror");
    let output = sandbox
        .command_in(&folder, &["--json", "--yes", "push", "--to"])
        .arg(&root)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(0), "{document}");
    assert_eq!(document["work"]["requests"], 2, "iris and the zip");
    assert_eq!(source.requests().len(), asked + 2);
    assert_eq!(named(&root, "objects")?, pinned());
    assert!(!folder.join("data").exists(), "push places nothing");
    Ok(())
}

/// data whose source wanted an account or accepted terms, or did not say, is pushed only once a
/// person says yes: anyone who can read the mirror gets it without either. a script gets
/// `unconfirmed` with no command to run, since only a person may agree.
#[test]
fn gated_data_asks_first() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    access(&sandbox, &[("iris", "terms"), ("dataset", "public")])?;
    let root = sandbox.work().join("mirror");
    let output = sandbox
        .command(&["--json", "push", "--to"])
        .arg(&root)
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    // nothing written, what the push set out to write, and what it asks about
    assert_eq!(document["result"]["copied"], 0);
    assert_eq!(document["result"]["copied_bytes"], 0);
    assert_eq!(document["result"]["planned"], 6);
    assert_eq!(document["result"]["planned_bytes"], bytes());
    assert_eq!(
        document["result"]["asks"],
        serde_json::json!([{"dataset": "iris", "access": "terms"}])
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("iris"),
        "{error}"
    );
    assert_eq!(error["fix"], Value::Null);
    assert!(!root.exists());
    // public data alone asks nothing
    let public = sandbox
        .command(&["--json", "push", "dataset", "--to"])
        .arg(&root)
        .output()?;
    ok(&public)?;
    let result = &json(&public)?["result"];
    assert_eq!(result["datasets"], serde_json::json!(["dataset"]));
    assert_eq!(result["objects"], 5);
    let yes = sandbox
        .command(&["--json", "--yes", "push", "--to"])
        .arg(&root)
        .output()?;
    ok(&yes)?;
    assert_eq!(json(&yes)?["result"]["copied"], 1);
    Ok(())
}

/// a push stopped while it downloads what the cache lacks, before it writes, copied nothing: its
/// result says so, beside what it set out to write.
#[cfg(unix)]
#[test]
fn a_stopped_push_says_it_copied_nothing_of_its_plan() -> Result {
    use crate::support::{Part, reply};
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 1 << 20];
    let (release, wait) = std::sync::mpsc::channel();
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Length", &body.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        &body[..1 << 16],
    );
    held.push(Part::Wait(wait));
    let server = Server::start("/files/big.bin", vec![file(&body, "\"v1\""), held])?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    let (folder, cache) = cold(&sandbox, "cold")?;
    let root = sandbox.work().join("mirror");
    let child = sandbox
        .command_in(&folder, &["--json", "--yes", "push", "--to"])
        .arg(&root)
        .env("FETCHLOOM_CACHE", &cache)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    server.wait_for_requests(2)?;
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    assert!(sent.success());
    let output = child.wait_with_output()?;
    drop(release);
    assert_eq!(output.status.code(), Some(130), "{}", text(&output.stderr)?);
    let result = &json(&output)?["result"];
    assert_eq!(result["copied"], 0, "{result}");
    assert_eq!(result["planned"], 1);
    assert_eq!(result["planned_bytes"], body.len());
    assert!(!root.exists());
    Ok(())
}

/// at a terminal, the question names the data and who can read the mirror; no keeps everything
/// as it was.
#[cfg(target_os = "linux")]
#[test]
fn no_at_the_question_pushes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    access(&sandbox, &[("iris", "terms"), ("dataset", "public")])?;
    let root = sandbox.work().join("mirror");
    let mut command = sandbox.command(&["push", "--to"]);
    command.arg(&root);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &command, "", "push it anyway?", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(1), "{screen}");
    assert!(screen.contains("iris needs accepted terms"), "{screen}");
    assert!(screen.contains("Cancelled"), "{screen}");
    assert!(!root.exists());
    Ok(())
}

/// two pushes of one project into one new mirror at once leave every file once, each holding
/// the bytes its name says.
#[test]
fn two_pushes_at_once_leave_one_good_copy() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let root = sandbox.work().join("mirror");
    let push = || {
        sandbox
            .command(&["--yes", "push", "--to"])
            .arg(&root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    };
    let (first, second) = (push()?, push()?);
    for child in [first, second] {
        ok(&child.wait_with_output()?)?;
    }
    assert_eq!(named(&root, "objects")?, pinned());
    assert_eq!(named(&root, "trees")?.len(), 1);
    assert_eq!(fs::read_dir(root.join("tmp"))?.count(), 0);
    Ok(())
}

/// push writes into a folder it is given, never to a web address: a server or a bucket gets a
/// folder mirror through a copying tool.
#[test]
fn a_push_needs_a_folder_and_never_a_web_address() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let output = sandbox.run(&["--json", "--yes", "push"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "usage");
    let help = error["help"].as_str().unwrap_or_default();
    assert!(help.contains("--to") && help.contains("push_to"), "{help}");
    let output = sandbox.run(&[
        "--json",
        "--yes",
        "push",
        "--to",
        "https://data.lab.example/fetchloom",
    ])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "usage");
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.contains("rclone")),
        "{error}"
    );
    Ok(())
}

/// push sends what data.lock pins, so a dataset data.toml has that data.lock does not pin yet
/// stops it before anything is written.
#[test]
fn a_dataset_not_pinned_yet_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let manifest = sandbox.work().join("data.toml");
    let text_now = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{text_now}\n[datasets.extra]\nref = \"https://example.org/extra.csv\"\n"),
    )?;
    let root = sandbox.work().join("mirror");
    let output = sandbox
        .command(&["--json", "--yes", "push", "--to"])
        .arg(&root)
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "lock.outdated");
    assert!(!root.exists());
    Ok(())
}

/// the lab's journey: alice pushes the files her cache holds into the folder `push_to` names; a
/// cold clone whose settings name that folder as its only mirror then syncs with no request to
/// anything else.
#[test]
fn a_cold_clone_syncs_from_what_a_push_left_with_no_other_request() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let lab = sandbox.folder("lab")?;
    let alice = lab.join("alice.toml");
    fs::write(&alice, "push_to = \"fetchloom\"\n")?;
    let pushed = sandbox
        .command(&["--yes", "push"])
        .env("FETCHLOOM_SETTINGS", &alice)
        .output()?;
    ok(&pushed)?;
    let asked = source.requests().len();
    let (folder, cache) = cold(&sandbox, "bob")?;
    let bob = lab.join("bob.toml");
    fs::write(&bob, "mirrors = [\"fetchloom\"]\nmirrors_only = true\n")?;
    let output = sandbox
        .command_in(&folder, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_SETTINGS", &bob)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    let document = json(&output)?;
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(document["work"]["mirror_objects"], 6);
    assert_eq!(source.requests().len(), asked, "the source is never asked");
    Ok(())
}

/// `--inputs` adds what a recipe is made from, so a rebuild elsewhere finds it on the mirror. a
/// result's files are pushed as data.lock pins them, and one the cache lacks names the build
/// that makes it, which only a person may run.
/// a plain push sends what a sync places, so a dataset with `place = false` stays off the mirror
/// unless a name or `--inputs` asks for it.
#[test]
fn a_plain_push_leaves_out_what_sync_does_not_place() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        crate::project::PATH,
        vec![file(crate::project::IRIS, "\"v1\"")],
    )?;
    with_recipe(&sandbox, &server)?;
    pin_clean(&sandbox, CODE, &[])?;
    let manifest = sandbox.work().join("data.toml");
    let text = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        text.replacen("[datasets.iris]\n", "[datasets.iris]\nplace = false\n", 1),
    )?;
    // the result as a build would leave it in the cache
    let done = sha256(b"done");
    let fan = sandbox.cache().join("objects").join(&done[..2]);
    fs::create_dir_all(&fan)?;
    fs::write(fan.join(&done[2..]), b"done")?;
    let pushed = |mirror: &str, args: &[&str]| -> Result<Vec<String>> {
        let root = sandbox.work().join(mirror);
        let mut command = sandbox.command(&["--yes", "push"]);
        ok(&command.args(args).arg("--to").arg(&root).output()?)?;
        named(&root, "objects")
    };
    let iris = sha256(crate::project::IRIS);
    assert_eq!(pushed("plain", &[])?, std::slice::from_ref(&done));
    let mut both = vec![iris.clone(), done];
    both.sort();
    assert_eq!(pushed("inputs", &["--inputs"])?, both);
    assert_eq!(pushed("named", &["iris"])?, [iris]);
    // with no name, --inputs still sends what every recipe is made from, placed or not
    let text = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        text.replacen("[datasets.clean]\n", "[datasets.clean]\nplace = false\n", 1),
    )?;
    assert_eq!(pushed("unplaced", &["--inputs"])?, both);
    Ok(())
}

#[test]
fn inputs_push_what_a_recipe_is_made_from() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        crate::project::PATH,
        vec![file(crate::project::IRIS, "\"v1\"")],
    )?;
    with_recipe(&sandbox, &server)?;
    pin_clean(&sandbox, CODE, &[])?;
    let root = sandbox.work().join("mirror");
    let output = sandbox
        .command(&["--json", "--yes", "push", "--inputs", "clean", "--to"])
        .arg(&root)
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["result"]["selected"], serde_json::json!(["clean"]));
    assert_eq!(
        document["result"]["datasets"],
        serde_json::json!(["clean", "iris"])
    );
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "offline.missing");
    assert_eq!(error["details"], serde_json::json!(["clean/clean.csv"]));
    assert_eq!(error["help"], "`fetchloom build clean` makes clean's files");
    assert_eq!(error["fix"], Value::Null);
    assert!(!root.exists());
    // the result as a build would leave it in the cache
    let hex = sha256(b"done");
    let fan = sandbox.cache().join("objects").join(&hex[..2]);
    fs::create_dir_all(&fan)?;
    fs::write(fan.join(&hex[2..]), b"done")?;
    let pushed = sandbox
        .command(&["--json", "--yes", "push", "--inputs", "clean", "--to"])
        .arg(&root)
        .output()?;
    ok(&pushed)?;
    assert_eq!(json(&pushed)?["result"]["copied"], 2);
    let mut expected = vec![sha256(crate::project::IRIS), hex];
    expected.sort();
    assert_eq!(named(&root, "objects")?, expected);
    // a clone that never built is told to build the recipe before anything downloads, since no
    // download brings its result
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock", "clean.py"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let parts = clone.join("data.lock.d");
    fs::create_dir(&parts)?;
    for part in fs::read_dir(sandbox.work().join("data.lock.d"))? {
        let part = part?;
        fs::copy(part.path(), parts.join(part.file_name()))?;
    }
    let output = sandbox
        .command_in(
            &clone,
            &["--json", "--yes", "push", "--inputs", "clean", "--to"],
        )
        .arg(sandbox.folder("other")?.join("mirror"))
        .env("FETCHLOOM_CACHE", sandbox.folder("clone-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(
        error["message"],
        "this machine's cache lacks files the push needs, which only a build makes"
    );
    assert_eq!(error["details"], serde_json::json!(["clean/clean.csv"]));
    assert_eq!(error["help"], "`fetchloom build clean` makes clean's files");
    assert_eq!(error["fix"], Value::Null);
    assert_eq!(document["work"]["requests"], 0);
    Ok(())
}

/// a size in data.lock edited by hand: the cache holds the bytes its SHA-256 names at their
/// real size, so push says data.lock is wrong rather than send the person to a sync that would
/// find nothing to do.
#[test]
fn a_size_the_cached_bytes_do_not_have_sends_no_one_to_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let _source = project(&sandbox)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let size = format!("size = {}", IRIS.len());
    assert!(pinned.contains(&size), "{pinned}");
    fs::write(
        &lock,
        pinned.replace(&size, &format!("size = {}", IRIS.len() + 1)),
    )?;
    let mirror = sandbox.folder("mirror")?;
    let pushed = sandbox.run(&["--json", "push", "--to", &mirror.to_string_lossy(), "--yes"])?;
    let document = json(&pushed)?;
    let error = &document["errors"][0];
    assert_eq!(pushed.status.code(), Some(50), "{document}");
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        format!(
            "data.lock pins iris/iris.csv at {} bytes, but its SHA-256 is that of {} bytes on this machine",
            IRIS.len() + 1,
            IRIS.len()
        )
    );
    assert!(error["fix"].is_null(), "{error}");
    Ok(())
}
