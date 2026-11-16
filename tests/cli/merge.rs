//! merges: `fetchloom lock` after a git merge left both sides in data.toml or data.lock, run with
//! the git this machine has.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use crate::support::{Result, Routes, Sandbox, Server, file, json, sha256_hex, skipped, text};

/// git in `folder`, as a person with a name runs it, without the machine's git settings. none
/// when git is not installed.
pub(crate) fn git(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Option<Output>> {
    let home = sandbox.folder("home")?;
    // git refuses a repository on a disk that records no owner, as FAT does, unless the person
    // trusts it; fetchloom's git, run with this home too, then reads the base as theirs does
    let config = home.join(".gitconfig");
    if !config.exists() {
        fs::write(&config, "[safe]\n\tdirectory = *\n")?;
    }
    let mut command = Command::new("git");
    for name in git_variables() {
        command.env_remove(name);
    }
    let identity = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.org",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "init.defaultBranch=main",
    ];
    let output = command
        .args(identity)
        .args(args)
        .current_dir(folder)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output();
    match output {
        Ok(output) => Ok(Some(output)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            skipped("git", "git is not installed")?;
            Ok(None)
        }
        Err(err) => Err(err.into()),
    }
}

/// git in the work folder, which must succeed. none when git is not installed.
pub(crate) fn ok(sandbox: &Sandbox, args: &[&str]) -> Result<Option<()>> {
    let Some(output) = git(sandbox, &sandbox.work(), args)? else {
        return Ok(None);
    };
    assert!(
        output.status.success(),
        "{args:?}: {}",
        text(&output.stderr)?
    );
    Ok(Some(()))
}

/// the variables of whatever runs the tests that set up git, such as `GIT_DIR`, which git and
/// fetchloom run without here, so git reads only what a test gives it.
pub(crate) fn git_variables() -> impl Iterator<Item = std::ffi::OsString> {
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_string_lossy().starts_with("GIT_"))
}

/// fetchloom in the work folder, where git was given no settings of the machine's.
fn run(sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
    let mut command = sandbox.command(args);
    for name in git_variables() {
        command.env_remove(name);
    }
    Ok(command.output()?)
}

fn succeeded(output: &Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// a repository whose main branch added alpha and whose branch theirs added beta, both beside
/// core, merged in `style` until git stopped at the conflicts. none without git.
fn two_additions(sandbox: &Sandbox, routes: &Routes, style: &str) -> Result<Option<()>> {
    let add = |name: &str| -> Result {
        succeeded(&run(
            sandbox,
            &["add", &format!("{}/{name}.csv", routes.url)],
        )?)
    };
    if ok(sandbox, &["init", "-q"])?.is_none() {
        return Ok(None);
    }
    add("core")?;
    ok(sandbox, &["add", "."])?;
    ok(sandbox, &["commit", "-qm", "core"])?;
    ok(sandbox, &["checkout", "-qb", "theirs"])?;
    add("beta")?;
    ok(sandbox, &["commit", "-qam", "beta"])?;
    ok(sandbox, &["checkout", "-q", "main"])?;
    add("alpha")?;
    ok(sandbox, &["commit", "-qam", "alpha"])?;
    let merged = git(
        sandbox,
        &sandbox.work(),
        &[
            "-c",
            &format!("merge.conflictStyle={style}"),
            "merge",
            "-q",
            "theirs",
        ],
    )?
    .ok_or("git went away")?;
    assert!(
        !merged.status.success(),
        "git merged both additions by itself"
    );
    for name in ["data.toml", "data.lock"] {
        let conflicted = fs::read_to_string(sandbox.work().join(name))?;
        assert!(conflicted.contains("<<<<<<< HEAD"), "{conflicted}");
    }
    Ok(Some(()))
}

fn three_files() -> Result<Routes> {
    Routes::start(
        ["alpha", "beta", "core"]
            .map(|name| (format!("/{name}.csv"), file(name.as_bytes(), "\"1\"")))
            .into(),
    )
}

/// what one branch's data.lock says of `name`: its table, from its name to the next.
fn table(lock: &str, name: &str) -> Result<String> {
    let start = lock
        .find(&format!("name = \"{name}\""))
        .ok_or("no such dataset")?;
    let end = lock[start..]
        .find("\n[[dataset]]")
        .map_or(lock.len(), |at| start + at);
    Ok(lock[start..end].to_owned())
}

#[test]
fn lock_merges_independent_additions_offline() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    if two_additions(&sandbox, &routes, "zdiff3")?.is_none() {
        return Ok(());
    }
    let ours =
        git(&sandbox, &sandbox.work(), &["show", "HEAD:data.lock"])?.ok_or("git went away")?;
    let theirs =
        git(&sandbox, &sandbox.work(), &["show", "theirs:data.lock"])?.ok_or("git went away")?;
    let asked = routes.requests().len();
    let conflicted = fs::read_to_string(sandbox.work().join("data.toml"))?;
    let previewed = run(&sandbox, &["lock", "--dry-run"])?;
    succeeded(&previewed)?;
    assert!(
        text(&previewed.stderr)?.contains(
            "Would merge data.toml and data.lock · alpha from ours, beta from theirs, 1 the same"
        ),
        "{}",
        text(&previewed.stderr)?
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        conflicted,
        "a dry run writes nothing"
    );
    let output = run(&sandbox, &["--json", "lock"])?;
    succeeded(&output)?;
    let merge = &json(&output)?["result"]["merge"];
    assert_eq!(merge["from_ours"], serde_json::json!(["alpha"]));
    assert_eq!(merge["from_theirs"], serde_json::json!(["beta"]));
    assert_eq!(merge["same"], serde_json::json!(["core"]));
    assert_eq!(merge["unpinned"], serde_json::json!([]));
    assert_eq!(routes.requests().len(), asked, "a merge asks no server");
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    let refs = |name: &str| format!("[datasets.{name}]\nref = \"{}/{name}.csv\"\n", routes.url);
    assert_eq!(
        manifest,
        format!("{}\n{}\n{}", refs("alpha"), refs("beta"), refs("core"))
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(
        table(&lock, "alpha")?,
        table(&text(&ours.stdout)?, "alpha")?
    );
    assert_eq!(
        table(&lock, "beta")?,
        table(&text(&theirs.stdout)?, "beta")?
    );
    succeeded(&run(&sandbox, &["lock", "--check"])?)?;
    succeeded(&run(&sandbox, &["--offline", "sync"])?)?;
    assert_eq!(
        fs::read(sandbox.work().join("data/beta/beta.csv"))?,
        b"beta"
    );
    Ok(())
}

/// git's usual conflict lines show no merge base, so fetchloom reads it from git's index, as any
/// git version gives it; without git, choices for the datasets one side added give the same lock.
#[test]
fn lock_reads_the_merge_base_from_git_or_asks_for_choices() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    if two_additions(&sandbox, &routes, "merge")?.is_none() {
        return Ok(());
    }
    let conflicted = metadata(&sandbox)?;
    let nowhere = sandbox.folder("no-git")?;
    let without_git = |args: &[&str]| -> Result<Output> {
        let mut command = sandbox.command(args);
        for name in git_variables() {
            command.env_remove(name);
        }
        Ok(command.env("PATH", &nowhere).output()?)
    };
    let output = without_git(&["--json", "lock"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert!(
        error["details"]
            .as_array()
            .and_then(|details| details.last())
            .and_then(serde_json::Value::as_str)
            .is_some_and(|detail| detail.starts_with("no merge base was found")),
        "{error}"
    );
    assert_eq!(metadata(&sandbox)?, conflicted, "nothing is written");
    succeeded(&without_git(&[
        "lock", "--ours", "alpha", "--theirs", "beta",
    ])?)?;
    let chosen = metadata(&sandbox)?;
    for name in ["alpha", "beta", "core"] {
        assert!(
            chosen.1.contains(&format!("name = \"{name}\"")),
            "{}",
            chosen.1
        );
    }
    abandon(&sandbox)?;
    stopped(&sandbox, &["merge", "-q", "theirs"])?;
    succeeded(&run(&sandbox, &["lock"])?)?;
    assert_eq!(metadata(&sandbox)?, chosen);
    Ok(())
}

/// no merge can tell which pin counts, so --yes never picks one, and nothing is downloaded.
#[test]
fn lock_stops_on_competing_upgrades_and_takes_the_side_a_person_names() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/version.csv",
        vec![
            file(b"1,base\n", "\"v1\""),
            file(b"3,right\n", "\"v3\""),
            file(b"2,left\n", "\"v2\""),
        ],
    )?;
    sandbox.allow(&server.url);
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    succeeded(&run(&sandbox, &["add", &server.url])?)?;
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "version"])?;
    ok(&sandbox, &["checkout", "-qb", "theirs"])?;
    succeeded(&run(&sandbox, &["lock", "--upgrade"])?)?;
    ok(&sandbox, &["commit", "-qam", "right"])?;
    ok(&sandbox, &["checkout", "-q", "main"])?;
    succeeded(&run(&sandbox, &["lock", "--upgrade"])?)?;
    ok(&sandbox, &["commit", "-qam", "left"])?;
    let merged = git(&sandbox, &sandbox.work(), &["merge", "-q", "theirs"])?.ok_or("no git")?;
    assert!(!merged.status.success());
    let conflicted = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let asked = server.requests().len();

    let output = run(&sandbox, &["--json", "--yes", "lock"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed");
    assert_eq!(error["message"], "the merge needs a choice for version");
    assert!(
        error["details"][0]
            .as_str()
            .is_some_and(|detail| detail.starts_with("version: ")),
        "{error}"
    );
    assert_eq!(error["fix"], serde_json::Value::Null, "only a person picks");
    assert_eq!(
        document["result"]["merge"]["conflicts"],
        serde_json::json!(["version"])
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.lock"))?,
        conflicted,
        "nothing is written"
    );
    // the refusal shows what each side pins, so the choice is not blind, and chooses nothing
    let refused = run(&sandbox, &["lock"])?;
    assert_eq!(refused.status.code(), Some(1));
    let told = text(&refused.stderr)?;
    for (side, bytes) in [("ours", b"2,left\n".as_slice()), ("theirs", b"3,right\n")] {
        let pinned = format!(
            "version on {side}: 1 file · {} B; version.csv · {} B · sha256 {}",
            bytes.len(),
            bytes.len(),
            &sha256_hex(bytes)[..12]
        );
        assert!(told.contains(&pinned), "{told}");
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.lock"))?,
        conflicted
    );

    succeeded(&run(&sandbox, &["lock", "--theirs", "version"])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&sha256_hex(b"3,right\n")), "{lock}");
    assert_eq!(server.requests().len(), asked, "a merge asks no server");
    Ok(())
}

/// each branch's new parts in data.lock.d survive the merge, and the merged lock names them.
#[test]
fn lock_merges_datasets_listed_in_parts() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    let listed = |args: &[&str]| -> Result<Output> {
        let mut command = sandbox.command(args);
        for name in git_variables() {
            command.env_remove(name);
        }
        Ok(command.env("FETCHLOOM_TEST_INLINE", "0").output()?)
    };
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let add = |name: &str| succeeded(&listed(&["add", &format!("{}/{name}.csv", routes.url)])?);
    add("core")?;
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "core"])?;
    ok(&sandbox, &["checkout", "-qb", "theirs"])?;
    add("beta")?;
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "beta"])?;
    ok(&sandbox, &["checkout", "-q", "main"])?;
    add("alpha")?;
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "alpha"])?;
    let merged = git(
        &sandbox,
        &sandbox.work(),
        &["-c", "merge.conflictStyle=zdiff3", "merge", "-q", "theirs"],
    )?
    .ok_or("no git")?;
    assert!(!merged.status.success());
    succeeded(&listed(&["lock"])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(lock.matches("files_in = [").count(), 3, "{lock}");
    succeeded(&listed(&["lock", "--check"])?)?;
    succeeded(&listed(&["--offline", "sync"])?)?;
    assert_eq!(
        fs::read(sandbox.work().join("data/beta/beta.csv"))?,
        b"beta"
    );
    Ok(())
}

#[test]
fn choices_need_a_merge_and_conflict_lines_in_git_order() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    succeeded(&run(
        &sandbox,
        &["add", &format!("{}/core.csv", routes.url)],
    )?)?;
    let chosen = run(&sandbox, &["lock", "--ours", "core"])?;
    assert_eq!(chosen.status.code(), Some(2), "{}", text(&chosen.stderr)?);
    assert!(
        text(&chosen.stderr)?
            .contains("neither data.toml nor data.lock holds git's conflict lines"),
        "{}",
        text(&chosen.stderr)?
    );
    let manifest = sandbox.work().join("data.toml");
    let before = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{before}=======\n"))?;
    let output = run(&sandbox, &["lock"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("data.toml has git's conflict lines out of order"),
        "{stderr}"
    );
    Ok(())
}

/// zdiff3 moves lines both sides share, such as a table's first lines, out of each conflict, so
/// the base is rebuilt around them; only labels, added on both sides, needs a choice.
#[test]
fn lock_reads_the_base_around_lines_zdiff3_moved_out_of_a_conflict() -> Result {
    let sandbox = Sandbox::new()?;
    let folders = [
        ("kappa", "x.txt", "base"),
        ("ours-labels", "l.txt", "cat"),
        ("their-labels", "l.txt", "wolf"),
        ("alpha", "a.txt", "a"),
        ("beta", "b.txt", "b"),
    ];
    for (folder, name, bytes) in folders {
        fs::write(sandbox.folder(folder)?.join(name), bytes)?;
    }
    let add = |folder: &str, name: &str| -> Result {
        succeeded(&run(
            &sandbox,
            &["add", &format!("../{folder}"), "--as", name],
        )?)
    };
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    add("kappa", "kappa")?;
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "kappa"])?;
    ok(&sandbox, &["checkout", "-qb", "theirs"])?;
    add("beta", "beta")?;
    add("their-labels", "labels")?;
    ok(&sandbox, &["commit", "-qam", "beta"])?;
    ok(&sandbox, &["checkout", "-q", "main"])?;
    add("alpha", "alpha")?;
    add("ours-labels", "labels")?;
    ok(&sandbox, &["commit", "-qam", "alpha"])?;
    let merged = git(
        &sandbox,
        &sandbox.work(),
        &["-c", "merge.conflictStyle=zdiff3", "merge", "-q", "theirs"],
    )?
    .ok_or("no git")?;
    assert!(!merged.status.success());

    let output = run(&sandbox, &["--json", "lock"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(
        error["details"][0], "labels: each side added it its own way",
        "{error}"
    );
    let merge = &document["result"]["merge"];
    assert_eq!(merge["conflicts"], serde_json::json!(["labels"]));
    assert_eq!(merge["from_ours"], serde_json::json!(["alpha"]));
    assert_eq!(merge["from_theirs"], serde_json::json!(["beta"]));
    succeeded(&run(&sandbox, &["lock", "--theirs", "labels"])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&sha256_hex(b"wolf")), "{lock}");
    Ok(())
}

/// a git merge or rebase in the work folder, as `args` asks, which must stop at a conflict.
fn stopped(sandbox: &Sandbox, args: &[&str]) -> Result {
    let output = git(sandbox, &sandbox.work(), args)?.ok_or("git went away")?;
    assert!(!output.status.success(), "{args:?} finished by itself");
    Ok(())
}

/// gives up the merge or rebase in the work folder, leaving the branch as it was committed.
fn abandon(sandbox: &Sandbox) -> Result {
    for args in [["merge", "--abort"], ["rebase", "--abort"]] {
        // only one of them is under way
        git(sandbox, &sandbox.work(), &args)?;
    }
    ok(sandbox, &["reset", "-q", "--hard"])?;
    ok(sandbox, &["clean", "-qfd", "--", "data.lock.d"])?;
    Ok(())
}

/// what `git show` prints for `what`, such as `main:data.lock`.
fn shown(sandbox: &Sandbox, what: &str) -> Result<String> {
    let output = git(sandbox, &sandbox.work(), &["show", what])?.ok_or("git went away")?;
    assert!(output.status.success(), "{what}: {}", text(&output.stderr)?);
    text(&output.stdout)
}

/// data.toml and data.lock in the work folder.
fn metadata(sandbox: &Sandbox) -> Result<(String, String)> {
    Ok((
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        fs::read_to_string(sandbox.work().join("data.lock"))?,
    ))
}

/// branch kept changes only the pin of `name`, through `change`, and branch removed removes it,
/// so git stops in data.lock alone. at each of `meetings`, `fetchloom lock` must ask, then write
/// the kept side whole or neither, as a person names. none without git.
fn removal_meets_a_new_pin(
    sandbox: &Sandbox,
    name: &str,
    change: impl Fn() -> Result,
    meetings: &[(&str, &[&str], &str)],
) -> Result<Option<()>> {
    if git(sandbox, &sandbox.work(), &["init", "-q"])?.is_none() {
        return Ok(None);
    }
    ok(sandbox, &["add", "."])?;
    ok(sandbox, &["commit", "-qm", "base"])?;
    ok(sandbox, &["checkout", "-qb", "removed"])?;
    succeeded(&run(sandbox, &["remove", name, "--yes"])?)?;
    ok(sandbox, &["commit", "-qam", "remove it"])?;
    ok(sandbox, &["checkout", "-qb", "kept", "main"])?;
    change()?;
    ok(sandbox, &["commit", "-qam", "pin other files for it"])?;
    let kept = (
        shown(sandbox, "kept:data.toml")?,
        shown(sandbox, "kept:data.lock")?,
    );
    let removed = (
        shown(sandbox, "removed:data.toml")?,
        shown(sandbox, "removed:data.lock")?,
    );
    for (on, meeting, kept_side) in meetings {
        let removal_side = if *kept_side == "ours" {
            "theirs"
        } else {
            "ours"
        };
        ok(sandbox, &["checkout", "-q", on])?;
        stopped(sandbox, meeting)?;
        let conflicted = metadata(sandbox)?;
        assert!(
            !conflicted.0.contains("<<<<<<<"),
            "{meeting:?}: {}",
            conflicted.0
        );
        assert!(
            conflicted.1.contains("<<<<<<<"),
            "{meeting:?}: {}",
            conflicted.1
        );

        let asked = run(sandbox, &["--json", "lock"])?;
        assert_eq!(
            asked.status.code(),
            Some(1),
            "{meeting:?}: {}",
            text(&asked.stderr)?
        );
        let document = json(&asked)?;
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "unconfirmed", "{meeting:?}: {error}");
        let details = error["details"].as_array().ok_or("no details")?;
        let why = format!("{name}: one side changed it and the other removed it");
        let gone = format!("{name} on {removal_side}: not in data.toml");
        assert!(
            details[0] == why && details.contains(&gone.into()),
            "{meeting:?}: {error}"
        );
        assert_eq!(error["fix"], serde_json::Value::Null, "only a person picks");
        let merge = &document["result"]["merge"];
        assert_eq!(merge["conflicts"], serde_json::json!([name]), "{meeting:?}");
        assert_eq!(merge["same"], serde_json::json!(["kappa"]), "{meeting:?}");
        for array in ["from_ours", "from_theirs", "removed", "unpinned"] {
            assert_eq!(merge[array], serde_json::json!([]), "{meeting:?} {array}");
        }
        assert_eq!(
            metadata(sandbox)?,
            conflicted,
            "{meeting:?}: nothing is written"
        );

        let chosen = run(
            sandbox,
            &["--json", "lock", &format!("--{kept_side}"), name],
        )?;
        succeeded(&chosen)?;
        let merge = &json(&chosen)?["result"]["merge"];
        assert_eq!(
            merge[format!("from_{kept_side}")],
            serde_json::json!([name])
        );
        assert_eq!(merge["removed"], serde_json::json!([]), "{meeting:?}");
        assert_eq!(
            metadata(sandbox)?,
            kept,
            "{meeting:?}: the kept side's entry, all of it, and its pin"
        );
        succeeded(&run(sandbox, &["lock", "--check"])?)?;
        abandon(sandbox)?;

        stopped(sandbox, meeting)?;
        let chosen = run(
            sandbox,
            &["--json", "lock", &format!("--{removal_side}"), name],
        )?;
        succeeded(&chosen)?;
        let merge = &json(&chosen)?["result"]["merge"];
        assert_eq!(merge["removed"], serde_json::json!([name]), "{meeting:?}");
        assert_eq!(metadata(sandbox)?, removed, "{meeting:?}");
        abandon(sandbox)?;
    }
    Ok(Some(()))
}

/// the re-import is remove then add, as add's docs say, which leaves data.toml as it was; the new
/// pin must not vanish unasked from either branch or a rebase, where the replayed one is theirs.
#[test]
fn lock_asks_before_a_removal_drops_a_reimported_pin() -> Result {
    let sandbox = Sandbox::new()?;
    for (folder, bytes) in [("base", "base"), ("cats", "cat"), ("wolves", "wolf")] {
        fs::write(sandbox.folder(folder)?.join("l.txt"), bytes)?;
    }
    let add = |folder: &str, name: &str| -> Result {
        succeeded(&run(
            &sandbox,
            &["add", &format!("../{folder}"), "--as", name],
        )?)
    };
    add("base", "kappa")?;
    add("cats", "labels")?;
    let reimport = || -> Result {
        succeeded(&run(&sandbox, &["remove", "labels", "--yes"])?)?;
        add("wolves", "labels")
    };
    let zdiff3 = ["-c", "merge.conflictStyle=zdiff3", "merge", "-q", "kept"];
    let meetings: [(&str, &[&str], &str); 3] = [
        ("removed", &zdiff3, "theirs"),
        ("kept", &["merge", "-q", "removed"], "ours"),
        ("kept", &["rebase", "-q", "removed"], "theirs"),
    ];
    if removal_meets_a_new_pin(&sandbox, "labels", reimport, &meetings)?.is_none() {
        return Ok(());
    }
    // the bytes the kept pin names are the ones a sync places
    ok(&sandbox, &["checkout", "-q", "removed"])?;
    stopped(&sandbox, &zdiff3)?;
    succeeded(&run(&sandbox, &["lock", "--theirs", "labels"])?)?;
    succeeded(&run(&sandbox, &["--offline", "sync"])?)?;
    assert_eq!(fs::read(sandbox.work().join("data/labels/l.txt"))?, b"wolf");
    Ok(())
}

/// the entry holds more than the link, a note and the folder its files go to, and the kept side
/// brings all of it back without asking the server.
#[test]
fn lock_asks_before_a_removal_drops_an_upgraded_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/u.csv",
        vec![file(b"v1\n", "\"1\""), file(b"v2 changed\n", "\"2\"")],
    )?;
    sandbox.allow(&server.url);
    fs::write(sandbox.folder("base")?.join("k.csv"), "k")?;
    succeeded(&run(&sandbox, &["add", "../base", "--as", "kappa"])?)?;
    succeeded(&run(&sandbox, &["add", &server.url, "--as", "series"])?)?;
    let manifest = sandbox.work().join("data.toml");
    let text = fs::read_to_string(&manifest)?.replace(
        "[datasets.series]\n",
        "# the series the paper reads\n[datasets.series]\n",
    );
    fs::write(&manifest, format!("{text}path = \"tables\"\n"))?;
    let upgrade = || succeeded(&run(&sandbox, &["lock", "--upgrade", "series"])?);
    let diff3 = ["-c", "merge.conflictStyle=diff3", "merge", "-q", "removed"];
    let meetings: [(&str, &[&str], &str); 2] = [
        ("removed", &["merge", "-q", "kept"], "theirs"),
        ("kept", &diff3, "ours"),
    ];
    let pinned = server.requests().len();
    if removal_meets_a_new_pin(&sandbox, "series", upgrade, &meetings)?.is_none() {
        return Ok(());
    }
    assert_eq!(
        server.requests().len(),
        pinned + 1,
        "only the upgrade asks the server"
    );
    Ok(())
}

/// [`run`] with every file list beside data.lock, as a big dataset keeps it.
fn run_listed(sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
    let mut command = sandbox.command(args);
    for name in git_variables() {
        command.env_remove(name);
    }
    Ok(command.env("FETCHLOOM_TEST_INLINE", "0").output()?)
}

/// a project pinning in.txt from `server` as a-src, its list beside data.lock, committed on main.
/// branch b upgrades a-src to what the server serves next; branch a starts from main. none
/// without git.
fn upgraded_on_b(sandbox: &Sandbox, server: &Server) -> Result<Option<()>> {
    if git(sandbox, &sandbox.work(), &["init", "-q"])?.is_none() {
        return Ok(None);
    }
    succeeded(&run_listed(
        sandbox,
        &["add", &server.url, "--as", "a-src"],
    )?)?;
    ok(sandbox, &["add", "."])?;
    ok(sandbox, &["commit", "-qm", "a-src"])?;
    ok(sandbox, &["checkout", "-qb", "b"])?;
    succeeded(&run_listed(sandbox, &["lock", "--upgrade", "a-src"])?)?;
    ok(sandbox, &["add", "-A"])?;
    ok(sandbox, &["commit", "-qm", "upgrade a-src"])?;
    ok(sandbox, &["checkout", "-qb", "a", "main"])?;
    Ok(Some(()))
}

fn upstream() -> Result<Server> {
    Server::start(
        "/in.txt",
        vec![
            file(b"one\n", "\"1\""),
            file(b"one, changed upstream\n", "\"2\""),
        ],
    )
}

/// c-new lands right after a-src in data.lock, so git merges part of a-src's entry and stops at
/// the rest; read whole with the base, a-src changed on b alone, so the merge needs no choice.
#[test]
fn lock_merges_an_upgrade_beside_a_new_dataset_by_itself() -> Result {
    let sandbox = Sandbox::new()?;
    let input = upstream()?;
    let extra = Server::start("/extra.txt", vec![file(b"extra\n", "\"e\"")])?;
    sandbox.allow(&input.url);
    sandbox.allow(&extra.url);
    if upgraded_on_b(&sandbox, &input)?.is_none() {
        return Ok(());
    }
    succeeded(&run_listed(
        &sandbox,
        &["add", &extra.url, "--as", "c-new"],
    )?)?;
    ok(&sandbox, &["add", "-A"])?;
    ok(&sandbox, &["commit", "-qm", "add c-new"])?;
    let (a, b) = (
        shown(&sandbox, "a:data.lock")?,
        shown(&sandbox, "b:data.lock")?,
    );
    let asked = (input.requests().len(), extra.requests().len());
    let zdiff3 = ["-c", "merge.conflictStyle=zdiff3", "merge", "-q", "a"];
    let meetings: [(&str, &[&str], &str); 3] = [
        ("a", &["merge", "-q", "b"], "ours"),
        ("b", &zdiff3, "theirs"),
        ("b", &["rebase", "-q", "a"], "ours"),
    ];
    for (on, meeting, a_side) in meetings {
        let b_side = if a_side == "ours" { "theirs" } else { "ours" };
        ok(&sandbox, &["checkout", "-q", on])?;
        stopped(&sandbox, meeting)?;
        let output = run_listed(&sandbox, &["--json", "lock"])?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{meeting:?}: {}",
            text(&output.stderr)?
        );
        let merge = &json(&output)?["result"]["merge"];
        assert_eq!(
            merge[format!("from_{a_side}")],
            serde_json::json!(["c-new"])
        );
        assert_eq!(
            merge[format!("from_{b_side}")],
            serde_json::json!(["a-src"]),
            "{meeting:?}"
        );
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        assert_eq!(table(&lock, "a-src")?, table(&b, "a-src")?, "{meeting:?}");
        assert_eq!(table(&lock, "c-new")?, table(&a, "c-new")?, "{meeting:?}");
        succeeded(&run_listed(&sandbox, &["lock", "--check"])?)?;
        abandon(&sandbox)?;
    }
    assert_eq!(
        (input.requests().len(), extra.requests().len()),
        asked,
        "a merge asks no server"
    );
    Ok(())
}

/// a project importing kappa, committed on main, then placed in tables-tom on branch tom and in
/// tables-priya on main, each in a commit of its own. none without git.
fn placed_apart(sandbox: &Sandbox) -> Result<Option<()>> {
    fs::write(sandbox.folder("base")?.join("k.csv"), "k")?;
    if git(sandbox, &sandbox.work(), &["init", "-q"])?.is_none() {
        return Ok(None);
    }
    succeeded(&run(sandbox, &["add", "../base", "--as", "kappa"])?)?;
    ok(sandbox, &["add", "."])?;
    ok(sandbox, &["commit", "-qm", "kappa"])?;
    let manifest = sandbox.work().join("data.toml");
    let place = |folder: &str, why: &str| -> Result {
        let text = fs::read_to_string(&manifest)?;
        fs::write(&manifest, format!("{text}path = \"{folder}\"\n"))?;
        ok(sandbox, &["commit", "-qam", why])?;
        Ok(())
    };
    ok(sandbox, &["checkout", "-qb", "tom"])?;
    place("tables-tom", "tom keeps the tables apart")?;
    ok(sandbox, &["checkout", "-q", "main"])?;
    place("tables-priya", "priya keeps the tables apart")?;
    Ok(Some(()))
}

/// `what`'s commit as `git log --oneline` shows it, such as `main`'s.
fn oneline(sandbox: &Sandbox, what: &str) -> Result<String> {
    let output = git(
        sandbox,
        &sandbox.work(),
        &["log", "-1", "--format=%h %s", what],
    )?
    .ok_or("git went away")?;
    assert!(output.status.success(), "{what}: {}", text(&output.stderr)?);
    Ok(text(&output.stdout)?.trim().to_owned())
}

/// a dataset each side changed its own way is shown as each side holds it, beside whose each side
/// is and its commit, in the refusal and in `--json`: a rebase turns git's ours and theirs around.
#[test]
fn a_needed_choice_shows_what_each_side_holds_and_whose_it_is() -> Result {
    let sandbox = Sandbox::new()?;
    if placed_apart(&sandbox)?.is_none() {
        return Ok(());
    }
    let (priya, tom) = (oneline(&sandbox, "main")?, oneline(&sandbox, "tom")?);
    let (priyas, toms) = ("path = \"tables-priya\"", "path = \"tables-tom\"");
    let meetings = [
        (
            "merge",
            [
                ("the branch you are on", &priya, priyas),
                ("the branch you are merging", &tom, toms),
            ],
        ),
        (
            "rebase",
            [
                ("the branch you are rebasing onto", &tom, toms),
                ("your commit being replayed", &priya, priyas),
            ],
        ),
    ];
    for (
        meeting,
        [
            (our_role, our_commit, ours),
            (their_role, their_commit, theirs),
        ],
    ) in meetings
    {
        stopped(&sandbox, &[meeting, "-q", "tom"])?;
        let output = run(&sandbox, &["--json", "lock"])?;
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
        let document = json(&output)?;
        assert_eq!(
            document["errors"][0]["details"],
            serde_json::json!([
                "kappa: each side changed it its own way",
                format!("kappa on ours: {ours}"),
                format!("kappa on theirs: {theirs}"),
                format!("ours is {our_role}: {our_commit}"),
                format!("theirs is {their_role}: {their_commit}"),
            ]),
            "{meeting}"
        );
        assert_eq!(
            document["result"]["merge"]["sides"]["kappa"],
            serde_json::json!({ "ours": [ours], "theirs": [theirs] }),
            "{meeting}"
        );
        let refused = text(&run(&sandbox, &["lock"])?.stderr)?;
        assert!(
            refused.contains(&format!("theirs is {their_role}: {their_commit}")),
            "{refused}"
        );
        abandon(&sandbox)?;
        ok(&sandbox, &["checkout", "-q", "main"])?;
    }
    Ok(())
}

/// after a wrong choice, data.toml and data.lock hold no conflict lines, so a second choice is
/// refused with how to bring them back while the rebase is under way; outside one, plain lock.
#[test]
fn a_side_chosen_wrong_is_chosen_again_after_git_checkout_m() -> Result {
    let sandbox = Sandbox::new()?;
    if placed_apart(&sandbox)?.is_none() {
        return Ok(());
    }
    stopped(&sandbox, &["rebase", "-q", "tom"])?;
    succeeded(&run(&sandbox, &["lock", "--ours", "kappa"])?)?;
    let again = run(&sandbox, &["--json", "lock", "--theirs", "kappa"])?;
    assert_eq!(again.status.code(), Some(2), "{}", text(&again.stderr)?);
    let error = &json(&again)?["errors"][0];
    assert_eq!(
        error["details"],
        serde_json::json!([
            "the git rebase is still under way, so a side chosen before can be chosen again"
        ]),
        "{error}"
    );
    assert_eq!(
        error["help"],
        "bring back git's conflict lines with `git checkout -m data.toml data.lock`, then run `fetchloom --json lock --theirs kappa` again",
        "{error}"
    );
    assert_eq!(error["fix"], serde_json::Value::Null, "git goes first");
    ok(&sandbox, &["checkout", "-m", "data.toml", "data.lock"])?;
    succeeded(&run(&sandbox, &["lock", "--theirs", "kappa"])?)?;
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(
        manifest.ends_with("path = \"tables-priya\"\n"),
        "{manifest}"
    );
    abandon(&sandbox)?;

    let outside = run(&sandbox, &["lock", "--theirs", "kappa"])?;
    assert_eq!(outside.status.code(), Some(2), "{}", text(&outside.stderr)?);
    assert!(
        text(&outside.stderr)?.contains("help: run `fetchloom lock` without them"),
        "{}",
        text(&outside.stderr)?
    );
    Ok(())
}

/// git leaves `REBASE_HEAD` behind once a rebase that stopped is done, and a later conflict from
/// `git stash pop` is no rebase: the choice names the stash's sides, and once a side is chosen,
/// the way back is git's, since git still holds both sides.
#[test]
fn a_conflict_after_a_finished_rebase_is_named_for_what_made_it() -> Result {
    let sandbox = Sandbox::new()?;
    if placed_apart(&sandbox)?.is_none() {
        return Ok(());
    }
    stopped(&sandbox, &["rebase", "-q", "tom"])?;
    succeeded(&run(&sandbox, &["lock", "--theirs", "kappa"])?)?;
    ok(&sandbox, &["add", "-A"])?;
    ok(
        &sandbox,
        &["-c", "core.editor=true", "rebase", "--continue"],
    )?;
    assert!(
        sandbox.work().join(".git/REBASE_HEAD").exists(),
        "git keeps it"
    );
    let manifest = sandbox.work().join("data.toml");
    let place = |folder: &str| -> Result {
        let text = fs::read_to_string(&manifest)?;
        let kept = text.rsplit_once("path = ").ok_or("no path")?.0;
        fs::write(&manifest, format!("{kept}path = \"{folder}\"\n"))?;
        Ok(())
    };
    place("tables-stashed")?;
    ok(&sandbox, &["stash", "-q"])?;
    place("tables-kept")?;
    ok(&sandbox, &["commit", "-qam", "keep the tables"])?;
    stopped(&sandbox, &["stash", "pop", "-q"])?;
    let head = oneline(&sandbox, "HEAD")?;

    let output = run(&sandbox, &["--json", "lock"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["details"],
        serde_json::json!([
            "kappa: each side changed it its own way",
            "kappa on ours: path = \"tables-kept\"",
            "kappa on theirs: path = \"tables-stashed\"",
            format!("ours is the branch you are on: {head}"),
            "theirs is your stashed changes",
        ])
    );
    succeeded(&run(&sandbox, &["lock", "--ours", "kappa"])?)?;
    let again = run(&sandbox, &["--json", "lock", "--theirs", "kappa"])?;
    assert_eq!(again.status.code(), Some(2), "{}", text(&again.stderr)?);
    assert_eq!(
        json(&again)?["errors"][0]["details"],
        serde_json::json!([
            "git still holds both sides of the conflict, so a side chosen before can be chosen again"
        ])
    );
    Ok(())
}

/// a settled merge leaves git's files unmerged, so the hint names `git add` and the step that
/// finishes what git is in; a dataset no pin answers says that sync pins it.
#[test]
fn a_settled_merge_says_how_to_finish_it() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    if two_additions(&sandbox, &routes, "merge")?.is_none() {
        return Ok(());
    }
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{written}select = [\"core.csv\"]\n"))?;
    let merged = run(&sandbox, &["lock"])?;
    succeeded(&merged)?;
    let told = text(&merged.stderr)?;
    for line in [
        "warning: core is not pinned, as neither side of data.lock pins it as data.toml now asks\n  help: run `fetchloom sync`, which pins it",
        "hint: run `fetchloom sync` to update data/, then `git add data.toml data.lock` and `git commit` to finish the merge",
    ] {
        assert!(told.contains(line), "{told}");
    }

    let sandbox = Sandbox::new()?;
    placed_apart(&sandbox)?.ok_or("git went away")?;
    stopped(&sandbox, &["rebase", "-q", "tom"])?;
    let chosen = text(&run(&sandbox, &["lock", "--theirs", "kappa"])?.stderr)?;
    assert!(
        chosen.contains(
            "then `git add data.toml data.lock` and `git rebase --continue` to finish the rebase"
        ),
        "{chosen}"
    );
    Ok(())
}

/// a merge whose result names no list, while git's merge brought one in, sweeps that part and
/// folder, and the `git add` it says to run still names data.lock.d, so following it stages the
/// deletion and the merge commit keeps no part nothing names.
#[test]
fn a_merge_that_sweeps_the_last_list_names_it_in_git_add() -> Result {
    let sandbox = Sandbox::new()?;
    for (folder, bytes) in [("cats", "cat"), ("wolves", "wolf")] {
        fs::write(sandbox.folder(folder)?.join("l.txt"), bytes)?;
    }
    // every list in data.lock.d, as a dataset past 50,000 files keeps it
    let listed = |args: &[&str]| -> Result {
        let mut command = sandbox.command(args);
        for name in git_variables() {
            command.env_remove(name);
        }
        succeeded(&command.env("FETCHLOOM_TEST_INLINE", "0").output()?)
    };
    listed(&["add", "../cats", "--as", "labels"])?;
    if git(&sandbox, &sandbox.work(), &["init", "-q"])?.is_none() {
        return Ok(());
    }
    ok(&sandbox, &["add", "."])?;
    ok(&sandbox, &["commit", "-qm", "base"])?;
    ok(&sandbox, &["checkout", "-qb", "removed"])?;
    listed(&["remove", "labels", "--yes"])?;
    ok(
        &sandbox,
        &["add", "-A", "data.toml", "data.lock", "data.lock.d"],
    )?;
    ok(&sandbox, &["commit", "-qm", "remove it"])?;
    ok(&sandbox, &["checkout", "-qb", "kept", "main"])?;
    listed(&["remove", "labels", "--yes"])?;
    listed(&["add", "../wolves", "--as", "labels"])?;
    ok(
        &sandbox,
        &["add", "-A", "data.toml", "data.lock", "data.lock.d"],
    )?;
    ok(&sandbox, &["commit", "-qm", "pin other files for it"])?;
    ok(&sandbox, &["checkout", "-q", "removed"])?;
    stopped(&sandbox, &["merge", "-q", "kept"])?;
    let mut command = sandbox.command(&["lock", "--ours", "labels"]);
    for name in git_variables() {
        command.env_remove(name);
    }
    let chosen = command.env("FETCHLOOM_TEST_INLINE", "0").output()?;
    succeeded(&chosen)?;
    assert!(!sandbox.work().join("data.lock.d").exists());
    let told = text(&chosen.stderr)?;
    let add = "`git add data.toml data.lock data.lock.d`";
    assert!(told.contains(add), "{told}");
    ok(&sandbox, &["add", "data.toml", "data.lock", "data.lock.d"])?;
    ok(&sandbox, &["commit", "-qm", "merge"])?;
    let parts =
        git(&sandbox, &sandbox.work(), &["ls-files", "data.lock.d"])?.ok_or("git went away")?;
    assert_eq!(text(&parts.stdout)?, "", "the merge commit keeps a part");
    Ok(())
}

/// in a clone whose data.lock.d lacks core's list, which no commit holds either, a merge that
/// conflicts over two new datasets leaves core waiting, its entry written back as it was.
#[test]
fn lock_merges_while_a_list_waits() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    if ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let link = |name: &str| format!("{}/{name}.csv", routes.url);
    succeeded(&run_listed(&sandbox, &["add", &link("core")])?)?;
    ok(&sandbox, &["add", "data.toml", "data.lock"])?;
    ok(&sandbox, &["commit", "-qm", "core, data.lock.d left out"])?;
    fs::remove_dir_all(sandbox.work().join("data.lock.d"))?;
    let core = table(
        &fs::read_to_string(sandbox.work().join("data.lock"))?,
        "core",
    )?;
    ok(&sandbox, &["checkout", "-qb", "theirs"])?;
    succeeded(&run(&sandbox, &["add", &link("beta")])?)?;
    ok(&sandbox, &["commit", "-qam", "beta"])?;
    ok(&sandbox, &["checkout", "-q", "main"])?;
    succeeded(&run(&sandbox, &["add", &link("alpha")])?)?;
    ok(&sandbox, &["commit", "-qam", "alpha"])?;
    stopped(&sandbox, &["merge", "-q", "theirs"])?;
    let merged = run(&sandbox, &["lock"])?;
    succeeded(&merged)?;
    let stderr = text(&merged.stderr)?;
    assert!(stderr.contains("so core waits for it"), "{stderr}");
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(table(&lock, "core")?, core);
    for name in ["alpha", "beta"] {
        assert!(lock.contains(&format!("name = \"{name}\"")), "{lock}");
    }
    let checked = run(&sandbox, &["--json", "lock", "--check"])?;
    assert_eq!(
        checked.status.code(),
        Some(50),
        "{}",
        text(&checked.stderr)?
    );
    assert!(
        json(&checked)?["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.ends_with("the file list of core, which is not there")),
        "{}",
        text(&checked.stdout)?
    );
    Ok(())
}
