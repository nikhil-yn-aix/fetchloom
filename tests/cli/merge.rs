//! merges: `fetchloom lock` after a git merge left both sides in data.toml or data.lock, run with
//! the git this machine has.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use crate::export::sha256;
use crate::support::{Result, Routes, Sandbox, Server, file, json, text};

/// git in `folder`, as a person with a name runs it, without the machine's git settings. none
/// when git is not installed.
fn git(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Option<Output>> {
    let home = sandbox.folder("home")?;
    // git refuses a repository on a disk that records no owner, as FAT does, unless the person
    // trusts it; fetchloom's git, run with this home too, then reads the base as theirs does
    let config = home.join(".gitconfig");
    if !config.exists() {
        fs::write(&config, "[safe]\n\tdirectory = *\n")?;
    }
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
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
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// git in the work folder, which must succeed. none when git is not installed.
fn ok(sandbox: &Sandbox, args: &[&str]) -> Result<Option<()>> {
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

/// whether the git here reads a merge base for fetchloom: 2.45 or later.
fn reads_bases(sandbox: &Sandbox) -> Result<bool> {
    Ok(
        git(sandbox, &sandbox.work(), &["--no-lazy-fetch", "version"])?
            .is_some_and(|output| output.status.success()),
    )
}

/// fetchloom in the work folder, where git was given no settings of the machine's.
fn run(sandbox: &Sandbox, args: &[&str]) -> Result<Output> {
    let mut command = sandbox.command(args);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
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

/// git's usual conflict lines show no merge base. git 2.45 and later give fetchloom the base
/// from its index; with an older git each dataset only one side has needs a choice.
#[test]
fn lock_reads_the_merge_base_from_git_or_asks_for_choices() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    if two_additions(&sandbox, &routes, "merge")?.is_none() {
        return Ok(());
    }
    if reads_bases(&sandbox)? {
        succeeded(&run(&sandbox, &["lock"])?)?;
    } else {
        let output = run(&sandbox, &["--json", "lock"])?;
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "unconfirmed");
        assert!(
            error["details"][2]
                .as_str()
                .is_some_and(|detail| detail.starts_with("no merge base was found")),
            "{error}"
        );
        succeeded(&run(
            &sandbox,
            &["lock", "--ours", "alpha", "--theirs", "beta"],
        )?)?;
    }
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    for name in ["alpha", "beta", "core"] {
        assert!(lock.contains(&format!("name = \"{name}\"")), "{lock}");
    }
    assert!(!lock.contains("<<<<<<<"), "{lock}");
    Ok(())
}

/// both teammates upgraded the same link and each pinned other bytes. no merge can tell which
/// one counts, and --yes never picks one: a person names the side, and nothing is downloaded.
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

    succeeded(&run(&sandbox, &["lock", "--theirs", "version"])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&sha256(b"3,right\n")), "{lock}");
    assert_eq!(server.requests().len(), asked, "a merge asks no server");
    Ok(())
}

/// datasets whose files data.lock keeps in data.lock.d: each branch's new parts are there after
/// the merge, and the merged lock names them.
#[test]
fn lock_merges_datasets_listed_in_parts() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = three_files()?;
    sandbox.allow(&routes.url);
    let listed = |args: &[&str]| -> Result<Output> {
        let mut command = sandbox.command(args);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                command.env_remove(name);
            }
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

/// both branches add a dataset of the same name, labels, each with its own files, and one more
/// each. zdiff3 moves the lines both sides share, such as the table's first lines, out of each
/// conflict, so the base fetchloom reads is rebuilt around them: labels needs a choice, as each
/// side added it its own way, and alpha and beta merge by themselves, as with diff3.
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
        error["details"],
        serde_json::json!(["labels: each side added it its own way"]),
        "{error}"
    );
    let merge = &document["result"]["merge"];
    assert_eq!(merge["conflicts"], serde_json::json!(["labels"]));
    assert_eq!(merge["from_ours"], serde_json::json!(["alpha"]));
    assert_eq!(merge["from_theirs"], serde_json::json!(["beta"]));
    succeeded(&run(&sandbox, &["lock", "--theirs", "labels"])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&sha256(b"wolf")), "{lock}");
    Ok(())
}
