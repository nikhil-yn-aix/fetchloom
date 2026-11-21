//! the project loop: sync, lock and status, and what spans several commands.

use std::fs;

use crate::support::{
    Result, Routes, Sandbox, Server, add_ok, delete, edit, file, json, ok, reply, sha256_hex, text,
    ways_left_out, zip_of,
};
#[cfg(unix)]
use crate::support::{Running, wait_until};

pub(crate) const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
pub(crate) const PATH: &str = "/files/iris.csv";

/// a project with iris added from `server`, whose first response serves it. later commands may
/// reach the server through data.toml and data.lock.
pub(crate) fn added(sandbox: &Sandbox, server: &Server) -> Result {
    add_ok(sandbox, &server.url)
}

/// copies data.toml and data.lock into a new folder, as a fresh clone of the project would.
pub(crate) fn clone(sandbox: &Sandbox, name: &str) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder(name)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), folder.join(file))?;
    }
    Ok(folder)
}

/// list tells apart two links that differ in a harmless query value, and hides a token.
#[test]
fn list_shows_a_harmless_query_and_hides_a_token() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.one]\nref = \"https://lab.example/x.csv?n=1\"\n\n[datasets.two]\nref = \"https://lab.example/x.csv?n=2\"\n\n[datasets.private]\nref = \"https://lab.example/y.csv?n=3&access_token=canary\"\n",
    )?;
    let output = sandbox.run(&["--json", "list"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let refs: Vec<&str> = document["result"]["datasets"]
        .as_array()
        .ok_or("no datasets")?
        .iter()
        .filter_map(|dataset| dataset["ref"].as_str())
        .collect();
    assert_eq!(
        refs,
        [
            "https://lab.example/x.csv?n=1",
            "https://lab.example/y.csv?n=3&access_token=***",
            "https://lab.example/x.csv?n=2"
        ]
    );
    assert!(!text(&sandbox.run(&["list"])?.stdout)?.contains("canary"));
    Ok(())
}

#[test]
fn list_shows_each_dataset_with_its_source_and_size() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let listed = sandbox.run(&["list"])?;
    assert_eq!(listed.status.code(), Some(0));
    let stdout = text(&listed.stdout)?;
    assert!(stdout.starts_with("iris  "), "{stdout}");
    assert!(stdout.contains("1 file · 32 B  data/iris"), "{stdout}");
    let document = json(&sandbox.run(&["--json", "list"])?)?;
    let dataset = &document["result"]["datasets"][0];
    assert_eq!(dataset["name"], "iris");
    assert_eq!(dataset["ref"], server.url.as_str());
    assert_eq!(dataset["files"], 1);
    assert_eq!(dataset["bytes"], IRIS.len());
    assert_eq!(dataset["folder"], "data/iris");
    assert_eq!(dataset["locked_files"][0]["path"], "iris.csv");
    assert_eq!(dataset["locked_files"][0]["size"], IRIS.len());
    assert_eq!(dataset["locked_files"][0]["sha256"], sha256_hex(IRIS));
    assert_eq!(dataset["locked_files"][0]["url"], server.url.as_str());
    assert!(dataset.get("title").is_some(), "{dataset}");
    assert_eq!(
        dataset["access"], "unknown",
        "a plain link says nothing of it"
    );
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        pinned.replace("\nlocked = ", "\naccess = \"terms\"\nlocked = "),
    )?;
    let document = json(&sandbox.run(&["--json", "list"])?)?;
    assert_eq!(document["result"]["datasets"][0]["access"], "terms");
    fs::write(&lock, pinned)?;
    let one = sandbox.run(&["list", "iris"])?;
    assert_eq!(one.status.code(), Some(0), "{}", text(&one.stderr)?);
    assert_eq!(text(&one.stdout)?, "iris.csv  32 B\n");
    let document = json(&sandbox.run(&["--json", "list", "iris"])?)?;
    assert_eq!(
        document["result"]["datasets"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(document["result"]["datasets"][0]["name"], "iris");
    let unknown = sandbox.run(&["list", "irs"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr)?.contains("fetchloom list iris"),
        "{}",
        text(&unknown.stderr)?
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn get_offers_to_pin_a_download_in_the_project_it_ran_in() -> Result {
    let sandbox = Sandbox::new()?;
    let responses = vec![
        file(IRIS, "\"v1\""),
        file(IRIS, "\"v1\""),
        file(IRIS, "\"v1\""),
    ];
    let server = Server::start(PATH, responses)?;
    let Some(outside) = sandbox.on_a_terminal(&["get", &server.url])? else {
        return Ok(());
    };
    assert!(
        outside.contains("next: to pin this dataset in a project, run `fetchloom add "),
        "{outside}"
    );
    assert!(sandbox.run(&["add", &server.url])?.status.success());

    let inside = sandbox
        .on_a_terminal(&["get", &server.url, "--dir", "again"])?
        .unwrap_or_default();

    assert!(
        inside.contains("next: to pin it in this project instead, run `fetchloom add "),
        "{inside}"
    );
    Ok(())
}

/// a pipe, `-q` and `--json` never get a `next:` line, so what scripts read stays the same.
#[test]
fn the_next_step_is_told_only_at_a_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    for args in [
        &["add", server.url.as_str()][..],
        &["lock"],
        &["sync"],
        &["remove", "iris"],
        &["get", server.url.as_str()],
    ] {
        let output = sandbox.run(args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("next:"), "{args:?}: {stderr}");
    }
    #[cfg(target_os = "linux")]
    {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
        if let Some(screen) = sandbox.on_a_terminal(&["add", &server.url])? {
            // the server is on this machine, so anyone who clones lists it in their settings
            let origin = server.url.trim_end_matches(PATH);
            assert!(
                screen.contains(&format!(
                    "\n  next: commit data.toml and data.lock; anyone who clones runs `fetchloom settings set allowed_origins {origin}`, with any origins they list already, then `fetchloom sync`\n"
                )),
                "{screen}"
            );
            for args in [
                &["-q", "remove", "iris"][..],
                &["--json", "add", &server.url],
            ] {
                let screen = sandbox.on_a_terminal(args)?.unwrap_or_default();
                assert!(!screen.contains("next:"), "{args:?}: {screen}");
            }
        }
    }
    Ok(())
}

#[test]
fn a_second_sync_does_nothing_at_all() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        text(&output.stderr)?,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    let quiet = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(quiet["work"]["requests"], 0);
    assert_eq!(quiet["result"]["lock_changed"], false);
    assert_eq!(
        fs::read(sandbox.work().join("data.lock"))?,
        lock,
        "data.lock is not rewritten"
    );
    Ok(())
}

/// adopting files that hold the pinned bytes is work, so it never ends with all files in place and
/// nothing to do.
#[test]
fn a_sync_that_adopts_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // a copy of the pinned bytes nothing records as placed, as one downloaded by hand
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, IRIS)?;
    delete(&sandbox.work().join(".fetchloom").join("state"))?;
    let adopted = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        adopted.contains("    Adopted data/iris · 1 file "),
        "{adopted}"
    );
    assert!(
        adopted.ends_with("     Synced 1 dataset into data\n"),
        "{adopted}"
    );
    Ok(())
}

#[test]
fn a_fresh_clone_gets_the_same_bytes_without_downloading_them_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let warm = clone(&sandbox, "warm")?;
    let document = json(&sandbox.command_in(&warm, &["--json", "sync"]).output()?)?;
    assert_eq!(document["status"], "ok");
    assert_eq!(
        document["work"]["requests"], 0,
        "the cache already holds iris"
    );
    let links = sandbox.links()?;
    assert_eq!(
        document["work"]["copies"],
        u64::from(!links.reflink && !links.hardlink),
        "a link costs no disk where the drive makes one"
    );
    assert_eq!(
        fs::read(warm.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(json(&output)?["work"]["requests"], 1);
    assert_eq!(
        fs::read(cold.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn hand_edits_survive_sync_until_forced() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // editors save by writing a new file in place of the old one
    delete(&placed)?;
    fs::write(&placed, "my own notes\n")?;
    let status = sandbox.run(&["status"])?;
    assert!(
        text(&status.stdout)?.contains("Edited data/iris/iris.csv"),
        "{}",
        text(&status.stdout)?
    );
    let kept = sandbox.run(&["sync"])?;
    assert_eq!(kept.status.code(), Some(0));
    assert!(
        text(&kept.stderr)?.contains("data/iris/iris.csv has changed, so it stays"),
        "{}",
        text(&kept.stderr)?
    );
    assert_eq!(fs::read_to_string(&placed)?, "my own notes\n");
    let forced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(
        server.requests().len(),
        1,
        "the cache still had the locked bytes"
    );
    Ok(())
}

/// the folder a suggested `fetchloom -C <folder> ...` names, unquoted, from the help `told`
/// gives before `then`.
fn named_folder(told: &str, then: &str) -> Option<std::path::PathBuf> {
    let (_, after) = told.split_once("`fetchloom -C ")?;
    let (quoted, _) = after.split_once(then)?;
    let unquoted = quoted
        .strip_prefix(['"', '\''])
        .and_then(|inner| inner.strip_suffix(['"', '\'']))
        .unwrap_or(quoted);
    Some(std::path::PathBuf::from(unquoted))
}

/// a plain `sync --force` pasted where fetchloom ran could force another project's files.
#[test]
fn a_force_after_a_run_with_c_names_its_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // editors save by writing a new file in place of the old one
    delete(&placed)?;
    fs::write(&placed, "my own notes\n")?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let work = sandbox.work();
    let project = fs::canonicalize(&work)?;
    let typed = work.to_string_lossy();
    for (args, then) in [
        (["-C", &typed, "sync"], " sync --force` discards your edit"),
        (["-C", &typed, "status"], " sync --force` to replace"),
    ] {
        let output = sandbox.command_in(&elsewhere, &args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let told = text(&output.stderr)?;
        let named = named_folder(&told, then).ok_or_else(|| format!("no -C in {told}"))?;
        assert_eq!(fs::canonicalize(named)?, project, "{told}");
    }
    // an edit is told of once, so the sync inside tells of a new one
    delete(&placed)?;
    fs::write(&placed, "my own notes, again\n")?;
    let inside = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        inside.contains("`fetchloom sync --force` discards your edit"),
        "{inside}"
    );
    assert_eq!(fs::read_to_string(&placed)?, "my own notes, again\n");
    Ok(())
}

/// status rows that name a sync, as its hints do, name the project after a run given `-C`.
#[test]
fn status_rows_after_c_name_its_project() -> Result {
    let sandbox = Sandbox::new()?;
    let mut servers = Vec::new();
    for path in ["/a.csv", "/b.csv", "/c.csv"] {
        let server = Server::start(path, vec![file(path.as_bytes(), "\"v\"")])?;
        added(&sandbox, &server)?;
        servers.push(server);
    }
    let work = sandbox.work();
    // b leaves data.toml, so its row and its file's row each name a sync
    fs::write(
        work.join("data.toml"),
        format!(
            "[datasets.a]\nref = \"{}\"\n\n[datasets.c]\nref = \"{}\"\n",
            servers[0].url, servers[2].url
        ),
    )?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let project = fs::canonicalize(&work)?;
    let typed = work.to_string_lossy();
    let output = sandbox
        .command_in(&elsewhere, &["-C", &typed, "status"])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let rows = text(&output.stdout)?;
    assert_eq!(rows.lines().count(), 2, "{rows}");
    for row in rows.lines() {
        let named = named_folder(row, " sync`").ok_or_else(|| format!("no -C in {row}"))?;
        assert_eq!(fs::canonicalize(named)?, project, "{row}");
    }
    Ok(())
}

/// two datasets imported from folders of this machine, birds and cows, each one file.
#[cfg(target_os = "linux")]
fn birds_and_cows(sandbox: &Sandbox) -> Result {
    for name in ["birds", "cows"] {
        let folder = sandbox.folder(name)?;
        fs::write(folder.join(format!("{name}.csv")), format!("{name}\n"))?;
        let output = sandbox.run(&["add", &format!("../{name}")])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    Ok(())
}

/// a hint must work as typed in the folder the person ran fetchloom from.
#[cfg(target_os = "linux")]
#[test]
fn hints_after_a_run_with_c_name_the_project_and_the_dataset_placed() -> Result {
    let sandbox = Sandbox::new()?;
    birds_and_cows(&sandbox)?;
    let work = sandbox.work();
    let typed = work.to_string_lossy();
    let cows = work.join("data").join("cows").join("cows.csv");
    delete(&cows)?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let on_a_terminal =
        |args: &[&str]| sandbox.on_a_terminal_running(&sandbox.command_in(&elsewhere, args));
    let Some(status) = on_a_terminal(&["-C", &typed, "status"])? else {
        return Ok(());
    };
    assert!(
        status.contains(&format!("hint: `fetchloom -C {typed} sync` places missing")),
        "{status}"
    );
    let alone = on_a_terminal(&["-C", &typed])?.unwrap_or_default();
    assert!(
        alone.contains(&format!(
            "hint: `fetchloom -C {typed} sync` puts the pinned files in data/; `fetchloom -C {typed} status` lists them"
        )),
        "{alone}"
    );
    let synced = on_a_terminal(&["-C", &typed, "sync"])?.unwrap_or_default();
    assert!(
        synced.contains(&format!(
            "next: `fetchloom -C {typed} path cows` tells your scripts where cows is"
        )),
        "{synced}"
    );
    Ok(())
}

/// after `-C`, a help names fetchloom's commands with the project's folder and another program's
/// paths whole, so each runs as written from the folder the person ran fetchloom in.
#[cfg(target_os = "linux")]
#[test]
fn helps_after_a_run_with_c_reach_the_project_from_where_it_ran() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let typed = work.to_string_lossy().into_owned();
    let elsewhere = sandbox.folder("elsewhere")?;
    let told = |args: &[&str]| -> Result<String> {
        let output = sandbox.command_in(&elsewhere, args).output()?;
        text(&output.stderr)
    };
    fs::write(work.join("data.toml"), "[datasets]\n")?;
    fs::write(work.join("data.lock"), "version = [\n")?;
    let broken = told(&["-C", &typed, "sync"])?;
    assert!(
        broken.contains(&format!("delete it and run `fetchloom -C {typed} lock`")),
        "{broken}"
    );
    fs::remove_file(work.join("data.lock"))?;
    let ours = told(&["-C", &typed, "lock", "--ours", "iris"])?;
    assert!(
        ours.contains(&format!("run `fetchloom -C {typed} lock` without them")),
        "{ours}"
    );
    let visits = work.join("data").join("visits");
    fs::create_dir_all(&visits)?;
    fs::write(visits.join("a.csv"), "a\n")?;
    let inside = told(&["-C", &typed, "add", "data/visits"])?;
    assert!(
        inside.contains(&format!(
            "run `fetchloom -C {typed} add ./data/visits` to pin it"
        )),
        "{inside}"
    );
    Ok(())
}

/// after `-C`, a git command a warning gives names the project's folder, so pasted as printed in
/// the folder the person ran fetchloom in it reaches the project's repository.
#[cfg(target_os = "linux")]
#[test]
fn git_commands_after_a_run_with_c_reach_the_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    let typed = work.to_string_lossy().into_owned();
    let elsewhere = sandbox.folder("elsewhere")?;
    let run = |args: &[&str]| -> Result<Vec<String>> {
        let mut command = sandbox.command_in(
            &elsewhere,
            &[&["-C", typed.as_str(), "--json"][..], args].concat(),
        );
        command.env("FETCHLOOM_TEST_INLINE", "0");
        for name in crate::merge::git_variables() {
            command.env_remove(name);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        Ok(crate::support::warned(&json(&output)?))
    };
    // the one warning that says to run `command`, which then runs as printed where fetchloom ran
    let paste = |warnings: &[String], command: &str| -> Result {
        let giving: Vec<&String> = warnings
            .iter()
            .filter(|warning| warning.contains(&format!("run `{command}`")))
            .collect();
        assert_eq!(giving.len(), 1, "{command} in {warnings:?}");
        let words: Vec<&str> = command.split(' ').skip(1).collect();
        let output = crate::merge::git(&sandbox, &elsewhere, &words)?.ok_or("no git")?;
        assert!(
            output.status.success(),
            "{command}: {}",
            text(&output.stderr)?
        );
        Ok(())
    };
    let added = run(&["add", &server.url])?;
    paste(&added, &format!("git -C {typed} add data.lock.d"))?;
    assert_eq!(
        git_warnings(&json(&sandbox.run(&["--json", "status"])?)?),
        Vec::<String>::new()
    );
    crate::merge::ok(&sandbox, &["commit", "-qam", "added"])?;
    let stale = "data.lock.d/iris.0123456789ab.tsv.zst";
    fs::write(work.join(stale), b"a list of another data.lock")?;
    crate::merge::ok(&sandbox, &["add", stale])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "stale"])?;
    paste(&run(&["status"])?, &format!("git -C {typed} rm {stale}"))?;
    assert!(!work.join(stale).exists(), "git rm deletes it");
    fs::write(work.join("tracked.csv"), b"length\n1.4\n")?;
    crate::merge::ok(&sandbox, &["add", "tracked.csv"])?;
    paste(
        &run(&["add", "./tracked.csv"])?,
        &format!("git -C {typed} rm -r --cached tracked.csv"),
    )?;
    let listed =
        crate::merge::git(&sandbox, &work, &["ls-files", "tracked.csv"])?.ok_or("no git")?;
    assert_eq!(text(&listed.stdout)?, "", "git no longer tracks it");
    Ok(())
}

/// the folder the last `fetchloom -C <folder> ...` before `then` in `told` names, unquoted.
fn folder_before(told: &str, then: &str) -> Option<std::path::PathBuf> {
    let (before, _) = told.split_once(then)?;
    let (_, quoted) = before.rsplit_once("`fetchloom -C ")?;
    let unquoted = quoted
        .strip_prefix(['"', '\''])
        .and_then(|inner| inner.strip_suffix(['"', '\'']))
        .unwrap_or(quoted);
    Some(std::path::PathBuf::from(unquoted))
}

/// a run of [`every_command_a_run_with_c_suggests_names_the_project`]: text to add to the end of
/// a project file first, the command line after `-C <folder>`, and what follows the folder in
/// the command it suggests.
type WithC<'a> = (Option<(&'a str, &'a str)>, Vec<&'a str>, &'a str);

/// a help or an error names the project after -C as a hint does, whichever command gave it.
#[test]
fn every_command_a_run_with_c_suggests_names_the_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        (
            "/files/wine.csv".to_owned(),
            file(b"alcohol\n14.2\n", "\"w1\""),
        ),
    ])?;
    let iris = format!("{}{PATH}", server.url);
    let wine = format!("{}/files/wine.csv", server.url);
    let new = format!("{}/files/new.csv", server.url);
    let zeros = "0".repeat(64);
    let work = sandbox.work();
    let project = fs::canonicalize(&work)?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let more = format!("\n[datasets.wine]\nref = \"{wine}\"\n");
    let cases: [WithC<'_>; 8] = [
        (None, vec!["add", &iris], " path iris`"),
        (
            None,
            vec!["add", &wine, "--as", "iris"],
            " remove iris` first",
        ),
        (None, vec!["exclude", "data/iris/iris.csv"], " remove iris`"),
        (None, vec!["path", "/iris"], " path iris` or"),
        (
            None,
            vec!["get", &iris, &wine, "--sha256", &zeros],
            " get --sha256` for each",
        ),
        (
            None,
            vec!["--offline", "add", &new],
            " get <link> --offline` reuses",
        ),
        (
            Some(("data.toml", &more)),
            vec!["export"],
            " lock` and commit data.lock, then export again",
        ),
        (
            Some(("data.lock", "<<<<<<< ours\n=======\n>>>>>>> theirs\n")),
            vec!["status"],
            " lock`, which merges both sides",
        ),
    ];
    let typed = work.to_string_lossy();
    for (appended, args, then) in cases {
        if let Some((name, text)) = appended {
            let mut written = fs::OpenOptions::new().append(true).open(work.join(name))?;
            std::io::Write::write_all(&mut written, text.as_bytes())?;
        }
        let line = [vec!["-C", &typed], args.clone()].concat();
        let told = text(&sandbox.command_in(&elsewhere, &line).output()?.stderr)?;
        let named = folder_before(&told, then).ok_or_else(|| format!("{args:?}: {told}"))?;
        assert_eq!(fs::canonicalize(named)?, project, "{args:?}: {told}");
    }
    Ok(())
}

/// a hardlink is the cache's own copy, shared by every project placing the file, so an edit
/// through it damages that copy. a reflink or a copy is the project's own, and its edit stays.
#[test]
fn an_edit_in_place_through_a_shared_link_is_put_back_and_kept_beside_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"changed in place\n")?;
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    let preview = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    let placement = &synced["result"]["placement"];
    if added["work"]["hardlinks"] == 1 {
        assert!(
            status.contains(
                "Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"
            ),
            "{status}"
        );
        assert!(
            preview.contains(
                "data/iris/iris.csv would be put back as pinned: it is linked to the cache's copy, \
                 which was changed. the changed copy would be kept as data/iris/iris.csv.changed"
            ),
            "{preview}"
        );
        assert_eq!(
            placement["restored"],
            serde_json::json!(["data/iris/iris.csv"])
        );
        assert_eq!(
            placement["changed_copies"],
            serde_json::json!([
                {"path": "data/iris/iris.csv", "kept_as": "data/iris/iris.csv.changed"}
            ])
        );
        assert_eq!(fs::read(&placed)?, IRIS);
        assert_eq!(
            fs::read(placed.with_file_name("iris.csv.changed"))?,
            b"changed in place\n",
            "the edit is kept, not dropped"
        );
        assert_eq!(
            server.requests().len(),
            2,
            "the changed copy is downloaded again"
        );
    } else {
        assert_eq!(
            placement["kept"][0]["reason"], "changed",
            "a reflink or a copy is this project's own"
        );
        assert_eq!(fs::read(&placed)?, b"changed in place\n");
    }
    Ok(())
}

#[test]
fn a_kept_changed_copy_never_replaces_a_file_already_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    if added["work"]["hardlinks"] != 1 {
        return crate::support::skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    fs::write(placed.with_file_name("iris.csv.changed"), "my notes\n")?;
    edit(&placed, b"changed in place\n")?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("the changed copy is kept as data/iris/iris.csv.changed.2\n  help: if no one meant to change the file, delete data/iris/iris.csv.changed.2; to edit a dataset's files, set copy = true for it in data.toml\n"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(placed.with_file_name("iris.csv.changed"))?,
        "my notes\n"
    );
    assert_eq!(
        fs::read(placed.with_file_name("iris.csv.changed.2"))?,
        b"changed in place\n"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    Ok(())
}

#[test]
fn an_edit_to_a_placed_file_never_spreads_to_other_projects() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = IRIS.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    let output = sandbox.run(&["sync", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS, "the locked bytes are back");
    // only a hardlink is the cached object itself, so only an edit through one damages the
    // object, which the next sync then downloads again
    let links = sandbox.links()?;
    let hardlinked = links.hardlinks_read_only();
    assert_eq!(server.requests().len(), if hardlinked { 2 } else { 1 });
    let other = clone(&sandbox, "other")?;
    sandbox.command_in(&other, &["sync"]).output()?;
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// only a person may choose the new bytes, so JSON gives no fix a script would run.
#[test]
fn changed_bytes_upstream_stop_sync_with_both_ways_on() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed(), changed()])?;
    added(&sandbox, &server)?;
    let cold = clone(&sandbox, "cold")?;
    let other_cache = sandbox.folder("other-cache")?;
    let output = sandbox
        .command_in(&cold, &["sync"])
        .env("FETCHLOOM_CACHE", &other_cache)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    assert!(
        !cold.join("data").join("iris").join("iris.csv").exists(),
        "nothing unverified is placed"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));

    let output = sandbox
        .command_in(&cold, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", &other_cache)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch");
    assert_eq!(error["fix"], serde_json::Value::Null, "{error}");
    let help = error["help"].as_str().unwrap_or_default();
    for way in [
        "`fetchloom push --to <folder>`",
        "`--mirror <folder>`",
        "`fetchloom lock --upgrade iris`",
    ] {
        assert!(help.contains(way), "{help}");
    }
    Ok(())
}

/// an upgrade of a link whose bytes changed names the link once, since the ref did not move.
#[test]
fn an_upgrade_of_changed_bytes_names_its_link_once() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed])?;
    added(&sandbox, &server)?;
    let upgraded = text(&sandbox.run(&["lock", "--upgrade"])?.stderr)?;
    assert!(
        upgraded.contains(&format!("Upgraded iris · {} · 1 file changed", server.url)),
        "{upgraded}"
    );
    assert!(!upgraded.contains('→'), "{upgraded}");
    Ok(())
}

#[test]
fn changed_bytes_from_a_source_that_states_md5_get_the_same_fix() -> Result {
    let sandbox = Sandbox::new()?;
    let same_size = b"sepal_length,species\n4.9,setosa\n";
    assert_eq!(same_size.len(), IRIS.len());
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(same_size, "\"v2\"")])?;
    added(&sandbox, &server)?;
    // zenodo, figshare and dataverse state an MD5, which data.lock keeps beside the SHA-256
    let lock = sandbox.work().join("data.lock");
    let text_before = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        text_before.replace(
            ", url = ",
            ", md5 = \"0773746ec982652e1c31c2c7bbaacc9c\", url = ",
        ),
    )?;
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(30));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("is not the file data.lock records"),
        "{stderr}"
    );
    assert!(stderr.contains("fetchloom lock --upgrade iris"), "{stderr}");
    Ok(())
}

#[test]
fn offline_uses_the_cache_or_says_what_is_missing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let warm = clone(&sandbox, "warm")?;
    let output = sandbox.command_in(&warm, &["--offline", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    assert_eq!(output.status.code(), Some(20));
    assert_eq!(
        server.requests().len(),
        1,
        "offline never touches the network"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn lock_check_fails_until_the_lock_follows_data_toml() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let text_before = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text_before}select = \"*.csv\"\n"))?;
    let stale = sandbox.run(&["lock", "--check"])?;
    assert_eq!(stale.status.code(), Some(1));
    assert!(
        text(&stale.stderr)?.contains("iris changed in data.toml"),
        "{}",
        text(&stale.stderr)?
    );
    let lock_before = fs::read(sandbox.work().join("data.lock"))?;
    let locked = sandbox.run(&["sync", "--locked"])?;
    assert_eq!(locked.status.code(), Some(1));
    assert!(
        text(&locked.stderr)?.contains("or sync without --locked"),
        "{}",
        text(&locked.stderr)?
    );
    assert_eq!(
        fs::read(sandbox.work().join("data.lock"))?,
        lock_before,
        "--locked writes nothing"
    );
    let relocked = sandbox.run(&["lock"])?;
    assert_eq!(
        relocked.status.code(),
        Some(0),
        "{}",
        text(&relocked.stderr)?
    );
    assert_eq!(
        server.requests().len(),
        1,
        "a new filter on the same version downloads nothing"
    );
    // a check that passes says so, as lock does, and -q keeps it quiet
    let checked = sandbox.run(&["lock", "--check"])?;
    assert_eq!(checked.status.code(), Some(0));
    assert_eq!(
        text(&checked.stderr)?,
        "         Ok data.lock matches data.toml\n"
    );
    assert_eq!(text(&sandbox.run(&["-q", "lock", "--check"])?.stderr)?, "");
    assert_eq!(sandbox.run(&["sync", "--locked"])?.status.code(), Some(0));
    let empty = sandbox.folder("empty")?;
    fs::write(empty.join("data.toml"), "")?;
    let refused = sandbox.command_in(&empty, &["sync", "--locked"]).output()?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    assert!(
        !empty.join("data.lock").exists(),
        "--locked never writes data.lock"
    );
    Ok(())
}

/// sync and status must not call data.toml changed when the owner never committed data.lock.
#[test]
fn a_clone_without_data_lock_says_nothing_pins_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = sandbox.folder("clone")?;
    fs::copy(sandbox.work().join("data.toml"), clone.join("data.toml"))?;

    let locked = sandbox.command_in(&clone, &["sync", "--locked"]).output()?;
    assert_eq!(locked.status.code(), Some(1));
    let status = sandbox.command_in(&clone, &["status"]).output()?;
    assert_eq!(status.status.code(), Some(0));
    let streams = format!(
        "{}status stdout:\n{}status stderr:\n{}",
        text(&locked.stderr)?,
        text(&status.stdout)?,
        text(&status.stderr)?
    );
    sandbox.filters().bind(|| insta::assert_snapshot!(streams));
    assert!(
        !clone.join("data.lock").exists(),
        "neither writes data.lock"
    );
    Ok(())
}

/// the parts of the lists in the work folder's data.lock.d, by path from the work folder.
fn parts(sandbox: &Sandbox) -> Result<Vec<String>> {
    let mut parts: Vec<String> = fs::read_dir(sandbox.work().join("data.lock.d"))?
        .map(|entry| {
            Ok(format!(
                "data.lock.d/{}",
                entry?.file_name().to_string_lossy()
            ))
        })
        .collect::<Result<_>>()?;
    parts.sort();
    Ok(parts)
}

/// the warnings of the json document `value` about lists git does not have, without those a
/// disk adds, such as fat's copies.
fn git_warnings(value: &serde_json::Value) -> Vec<String> {
    crate::support::warned(value)
        .into_iter()
        .filter(|warning| warning.contains("git does not have"))
        .collect()
}

/// committing data.toml and data.lock alone, as `git commit -a` would, leaves the new list out.
#[test]
fn status_names_lists_git_does_not_have() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let listed = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_INLINE", "0")
            .output()
    };
    let added = listed(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    crate::merge::ok(&sandbox, &["add", "data.toml", "data.lock"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "iris"])?;
    let [part] = parts(&sandbox)?.try_into().map_err(|_| "one list")?;

    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        git_warnings(&status),
        [format!(
            "data.lock names {part}, which git does not have; run `git add data.lock.d`, then commit it with data.lock, since a clone without it cannot read data.lock"
        )]
    );
    crate::merge::ok(&sandbox, &["add", "data.lock.d"])?;
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(git_warnings(&status), Vec::<String>::new());
    Ok(())
}

/// a script is told by a warning and a person at a terminal by the `next:` line.
#[test]
fn a_new_list_git_does_not_have_is_named_at_the_end() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, (0..3).map(|_| file(IRIS, "\"v1\"")).collect())?;
    sandbox.allow(&server.url);
    let listed = |args: &[&str]| {
        let mut command = sandbox.command(args);
        command.env("FETCHLOOM_TEST_INLINE", "0");
        command
    };
    let ran = |args: &[&str]| -> Result<serde_json::Value> {
        let output = listed(args).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        json(&output)
    };
    let outside = ran(&["--json", "add", &server.url, "--as", "first"])?;
    assert_eq!(
        git_warnings(&outside),
        Vec::<String>::new(),
        "no repository"
    );
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    crate::merge::ok(&sandbox, &["add", "."])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "first"])?;
    let before = parts(&sandbox)?;

    let second = ran(&["--json", "add", &server.url, "--as", "second"])?;
    let part = parts(&sandbox)?
        .into_iter()
        .find(|part| !before.contains(part))
        .ok_or("no new list")?;
    assert_eq!(
        git_warnings(&second),
        [format!(
            "git does not have {part}, which data.lock names now; run `git add data.lock.d` before you commit, since `git commit -a` leaves new files out"
        )]
    );
    let again = ran(&["--json", "sync"])?;
    assert_eq!(
        git_warnings(&again),
        Vec::<String>::new(),
        "nothing new written"
    );
    #[cfg(target_os = "linux")]
    {
        crate::merge::ok(&sandbox, &["add", "."])?;
        crate::merge::ok(&sandbox, &["commit", "-qm", "second"])?;
        let Some(told) =
            sandbox.on_a_terminal_running(&listed(&["add", &server.url, "--as", "third"]))?
        else {
            return Ok(());
        };
        let next = told.lines().last().unwrap_or_default();
        assert!(
            next.starts_with("  next: run `git add data.lock.d` before you commit: git does not have data.lock.d/third."),
            "{told}"
        );
    }
    Ok(())
}

/// a removal that deletes the last list git has names data.lock.d among the files to commit,
/// since a commit of data.toml and data.lock alone leaves the deleted list in the repository.
#[cfg(target_os = "linux")]
#[test]
fn a_removal_that_deletes_a_list_git_has_says_to_commit_data_lock_d() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let added = sandbox
        .command(&["add", &server.url, "--as", "first"])
        .env("FETCHLOOM_TEST_INLINE", "0")
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    crate::merge::ok(&sandbox, &["add", "."])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "first"])?;
    let Some(told) = sandbox.on_a_terminal_running(&sandbox.command(&["remove", "first"]))? else {
        return Ok(());
    };
    assert!(!sandbox.work().join("data.lock.d").exists(), "{told}");
    let next = told.lines().last().unwrap_or_default();
    assert!(
        next.starts_with("  next: commit data.toml, data.lock and data.lock.d;"),
        "{told}"
    );
    Ok(())
}

/// the source serves other bytes now, which a relock that asked it would pin. a sync of one name,
/// as `path` runs, changes no other entry, so it copies the pin; the next lock drops the old one.
#[test]
fn a_renamed_dataset_keeps_its_pin_without_asking_the_source() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed(), changed()])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let rename = |from: &str, to: &str| -> Result {
        let text = fs::read_to_string(&manifest)?;
        let table = format!("[datasets.{from}]\n");
        assert!(text.contains(&table), "{text}");
        fs::write(
            &manifest,
            text.replace(&table, &format!("[datasets.{to}]\n")),
        )?;
        Ok(())
    };
    let pinned = |name: &str| -> Result<bool> {
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        Ok(lock.contains(&format!("name = \"{name}\"")) && lock.contains(&sha256_hex(IRIS)))
    };

    rename("iris", "flowers")?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Renamed iris to flowers in data.lock · its pin stays\n"),
        "{stderr}"
    );
    assert!(pinned("flowers")? && !pinned("iris")?);
    assert_eq!(
        fs::read(sandbox.work().join("data/flowers/iris.csv"))?,
        IRIS
    );
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));

    rename("flowers", "petals")?;
    let found = sandbox.run(&["path", "petals"])?;
    assert_eq!(found.status.code(), Some(0), "{}", text(&found.stderr)?);
    assert!(pinned("petals")? && pinned("flowers")?);
    let locked = sandbox.run(&["lock"])?;
    assert!(
        text(&locked.stderr)?.contains("Removed flowers from data.lock\n"),
        "{}",
        text(&locked.stderr)?
    );
    assert!(pinned("petals")? && !pinned("flowers")?);
    assert_eq!(server.requests().len(), 1, "only the add asked the source");
    Ok(())
}

/// a script running the `fix` would undo `--locked`, so only the help names the ways on.
#[test]
fn a_locked_run_never_offers_a_fix_that_changes_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = || file(b"species\nsetosa\n", "\"v2\"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), changed()])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    let (manifest, pins) = (work.join("data.toml"), work.join("data.lock"));
    let (asked, pinned) = (fs::read_to_string(&manifest)?, fs::read_to_string(&pins)?);
    let stops = |args: &[&str], kind: &str| -> Result {
        let output = sandbox.run(args)?;
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], kind, "{args:?}: {error}");
        assert_eq!(error["fix"], serde_json::Value::Null, "{args:?}: {error}");
        assert!(error["help"].is_string(), "{args:?}: {error}");
        Ok(())
    };

    fs::write(
        &manifest,
        asked.replacen(
            "[datasets.iris]\n",
            "[datasets.iris]\nselect = \"*.csv\"\n",
            1,
        ),
    )?;
    for args in [
        &["--json", "sync", "--locked"][..],
        &["--json", "path", "--locked", "iris"],
    ] {
        stops(args, "lock.outdated")?;
    }
    fs::write(
        &manifest,
        format!("<<<<<<< HEAD\n{asked}=======\n{asked}>>>>>>> theirs\n"),
    )?;
    stops(&["--json", "sync", "--locked"], "project.invalid")?;
    fs::write(&manifest, &asked)?;
    delete(&pins)?;
    stops(&["--json", "sync", "--locked"], "lock.outdated")?;
    stops(&["--json", "path", "--locked", "iris"], "lock.outdated")?;
    fs::write(&pins, &pinned)?;
    let cold = clone(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&cold, &["--json", "sync", "--locked", "iris"])
        .env("FETCHLOOM_CACHE", sandbox.folder("other-cache")?)
        .output()?;
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch", "{error}");
    assert_eq!(error["fix"], serde_json::Value::Null, "{error}");
    Ok(())
}

#[test]
fn upgrading_to_the_same_data_leaves_the_lock_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    added(&sandbox, &server)?;
    let path = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&path)?;
    let start = pinned.find("locked = \"").ok_or("no locked line")? + "locked = \"".len();
    let dated = format!(
        "{}2001-02-03T04:05:06Z{}",
        &pinned[..start],
        &pinned[start + 20..]
    );
    fs::write(&path, &dated)?;
    let output = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr,
        "         Ok data.lock is already up to date, nothing to do\n"
    );
    assert_eq!(
        fs::read_to_string(&path)?,
        dated,
        "a pin dates from when its data last changed"
    );
    let asked = &server.requests()[1];
    assert!(
        asked.contains("if-none-match: \"v1\""),
        "an upgrade asks whether an unpinned file changed: {asked}"
    );
    Ok(())
}

#[test]
fn upgrading_a_link_whose_bytes_changed_pins_the_new_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let changed = b"sepal_length,species\n9.9,new\n";
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(changed, "\"v2\"")])?;
    added(&sandbox, &server)?;
    let before = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["lock", "--upgrade"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let after = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_ne!(before, after);
    assert!(
        after.contains(&format!("size = {}", changed.len())),
        "{after}"
    );
    Ok(())
}

/// a record in .fetchloom/owner that no fetchloom writes, as a clone can carry, keeps nobody out:
/// the command removes it, says so once, and goes on.
#[test]
fn an_owner_record_no_fetchloom_writes_is_removed_with_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let owner = sandbox.work().join(".fetchloom").join("owner");
    fs::write(&owner, "{\"os\": \"plan9\"}")?;
    let output = sandbox.run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(
        crate::support::warned(&document),
        [format!(
            "removed {}, which no fetchloom wrote for this folder, as a cloned or copied project can hold, and went on",
            owner.display()
        )],
        "{document}"
    );
    assert_eq!(crate::support::kinds(&document), ["project.owner_removed"]);
    assert!(!owner.exists());
    Ok(())
}

/// a project begun outside git, by an add or by a sync of a hand-written data.toml, keeps each
/// dataset's folder out of git from its start, so a `git init` and `git add -A` right after it
/// commit no data file, and no .gitignore beside data.toml is written. the line that says so
/// waits for `-v` outside git.
#[test]
fn a_project_started_outside_git_keeps_its_data_out_of_a_later_commit() -> Result {
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    for (command, data_dir) in [("add", "data"), ("sync", "inputs")] {
        let sandbox = Sandbox::new()?;
        sandbox.allow(&server.url);
        let work = sandbox.work();
        let output = if command == "sync" {
            fs::write(
                work.join("data.toml"),
                format!(
                    "[project]\ndata_dir = \"{data_dir}\"\n\n[datasets.iris]\nref = \"{}\"\n",
                    server.url
                ),
            )?;
            sandbox.run(&["sync"])?
        } else {
            sandbox.run(&["add", &server.url])?
        };
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{command}: {stderr}");
        assert!(!stderr.contains("Ignored"), "{command}: {stderr}");
        assert!(!work.join(".gitignore").exists(), "{command}");
        assert_eq!(
            crate::support::marker(&work.join(data_dir).join("iris").join(".gitignore"))?,
            b"*\n",
            "{command}"
        );
        if crate::merge::ok(&sandbox, &["init", "-q"])?.is_some() {
            crate::merge::ok(&sandbox, &["add", "-A"])?;
            let staged = crate::merge::git(&sandbox, &work, &["diff", "--cached", "--name-only"])?
                .ok_or("git went away")?;
            assert_eq!(text(&staged.stdout)?, "data.lock\ndata.toml\n", "{command}");
        }
    }
    Ok(())
}

/// a .gitignore that is a link, as a cloned repository may hold one to a file of keys, is never
/// read or edited while fetchloom looks for a line hiding the data folder: the link and the file
/// it names stay as they were, and nothing is said of either.
#[cfg(unix)]
#[test]
fn a_linked_gitignore_is_never_read_or_edited() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir(work.join(".git"))?;
    let secret = sandbox.folder("outside")?.join("tokens");
    fs::write(&secret, "/data/\n")?;
    std::os::unix::fs::symlink(&secret, work.join(".gitignore"))?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let output = sandbox.run(&["--json", "add", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(fs::read_to_string(&secret)?, "/data/\n");
    let link = fs::symlink_metadata(work.join(".gitignore"))?;
    assert!(link.is_symlink(), "the link was replaced");
    assert_eq!(fs::read_link(work.join(".gitignore"))?, secret);
    let document = json(&output)?;
    assert_eq!(
        crate::support::kinds(&document),
        Vec::<&str>::new(),
        "{document}"
    );
    assert_eq!(
        crate::support::marker(&work.join("data/iris/.gitignore"))?,
        b"*\n"
    );
    Ok(())
}

/// data.toml or data.lock as a link, as a cloned repository may hold one to a file of keys that
/// reads as TOML, is refused before a byte of what it names is read, by every command that reads
/// the project and in `--json` too: a refused line of data.toml, which an error shows, never
/// shows that file's text. `lock`, which reads both files before it merges, stops at once.
#[cfg(unix)]
#[test]
fn a_linked_data_toml_or_data_lock_is_refused_unread() -> Result {
    const SECRET: &str = "SEKRIT_A1";
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let keys = sandbox.folder("outside")?.join("secrets.toml");
    fs::write(&keys, format!("token = \"{SECRET}\"\n"))?;
    let elsewhere = sandbox.folder("elsewhere")?;
    fs::write(elsewhere.join("a.csv"), "a\n")?;
    let mirror = sandbox.folder("mirror")?;
    let commands: [&[&str]; 13] = [
        &["status"],
        &["sync"],
        &["lock"],
        &["lock", "--check"],
        &["list"],
        &["path", "x"],
        &["export"],
        &["audit"],
        &["remove", "x"],
        &["drop", "x"],
        &["exclude", "data/x/a.csv"],
        &["push", "--to", &mirror.to_string_lossy()],
        &["add", &elsewhere.to_string_lossy(), "--as", "y"],
    ];
    for name in ["data.toml", "data.lock"] {
        fs::write(
            work.join("data.toml"),
            "[datasets.x]\nref = \"https://example.org/x.csv\"\n",
        )?;
        let link = work.join(name);
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&keys, &link)?;
        let refused = format!(
            "{} is a link, and fetchloom does not follow it",
            link.display()
        );
        for command in commands {
            for scripted in [false, true] {
                let mut args = command.to_vec();
                if scripted {
                    args.insert(0, "--json");
                }
                let output = sandbox.run(&args)?;
                let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
                assert!(!said.contains(SECRET), "{name}, {args:?}: {said}");
                assert_eq!(output.status.code(), Some(50), "{name}, {args:?}: {said}");
                if scripted {
                    let document = json(&output)?;
                    assert_eq!(
                        document["errors"][0]["message"],
                        refused.as_str(),
                        "{document}"
                    );
                    assert_eq!(document["errors"][0]["kind"], "project.invalid");
                } else {
                    assert!(said.contains(&refused), "{name}, {args:?}: {said}");
                    assert!(
                        said.contains(&format!(
                            "help: replace {name} with the file it names before running this command"
                        )),
                        "{said}"
                    );
                }
            }
        }
        assert_eq!(fs::read_link(&link)?, keys, "{name} stays the link it was");
        assert!(!work.join("data").exists(), "{name}");
        fs::remove_file(&link)?;
    }
    assert_eq!(
        fs::read_to_string(&keys)?,
        format!("token = \"{SECRET}\"\n")
    );
    Ok(())
}

/// each command that places files makes the .gitignore of a dataset folder that lost it, and
/// says so inside git: `add` of a dataset data.toml has, `sync`, whose dry run says so first, and
/// `path`, which places the files it answers with first.
#[test]
fn every_command_that_places_files_keeps_the_dataset_folder_out_of_git() -> Result {
    for (command, data_dir) in [("add", "data"), ("sync", "data"), ("path", "inputs/raw")] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
        let work = sandbox.work();
        if data_dir != "data" {
            fs::write(
                work.join("data.toml"),
                format!("[project]\ndata_dir = \"{data_dir}\"\n"),
            )?;
        }
        added(&sandbox, &server)?;
        let folder = data_dir
            .split('/')
            .fold(work.clone(), |folder, part| folder.join(part))
            .join("iris");
        fs::remove_file(folder.join(".gitignore"))?;
        fs::create_dir(work.join(".git"))?;
        let line = format!(
            "Ignored {data_dir}/iris in git with {data_dir}/iris/.gitignore, which also hides files you put there\n"
        );
        let args = match command {
            "add" => vec!["add", server.url.as_str()],
            "sync" => {
                let previewed = text(&sandbox.run(&["sync", "--dry-run"])?.stderr)?;
                assert!(
                    previewed.contains(&format!("Would ignore {data_dir}/iris")),
                    "{previewed}"
                );
                assert!(!folder.join(".gitignore").exists());
                vec!["sync"]
            }
            _ => {
                delete(&folder.join("iris.csv"))?;
                vec!["path", "iris"]
            }
        };
        let output = sandbox.run(&args)?;
        let stderr = text(&output.stderr)?;
        assert!(output.status.success(), "{command}: {stderr}");
        assert!(stderr.contains(&line), "{command}: {stderr}");
        assert_eq!(
            crate::support::marker(&folder.join(".gitignore"))?,
            b"*\n",
            "{command}"
        );
        assert!(!work.join(".gitignore").exists(), "{command}");
        match command {
            "add" => assert!(stderr.contains("iris is already in data.toml"), "{stderr}"),
            "path" => assert_eq!(text(&output.stdout)?, format!("{}\n", folder.display())),
            _ => {}
        }
    }
    Ok(())
}

/// each row goes to stdout, where a pipe gets it, and the closing line and hints to stderr, so
/// `fetchloom status | grep Missing` finds what is missing. a file of the person's own in a
/// dataset's folder is no difference, and a plain status says nothing of it; json names it.
#[test]
fn status_lists_what_differs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let clean = sandbox.run(&["status"])?;
    assert_eq!(
        text(&clean.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    assert!(clean.stdout.is_empty(), "{}", text(&clean.stdout)?);
    // editors save by writing a new file in place of the old one
    delete(&folder.join("iris.csv"))?;
    fs::write(folder.join("iris.csv"), "mine")?;
    fs::write(folder.join("notes.txt"), "mine")?;
    let edited = sandbox.run(&["status"])?;
    assert_eq!(text(&edited.stdout)?, "     Edited data/iris/iris.csv\n");
    let stderr = text(&edited.stderr)?;
    assert!(
        stderr.starts_with("      Found 1 difference from data.lock\n  hint: "),
        "{stderr}"
    );
    assert!(!stderr.contains("data/iris/iris.csv"), "{stderr}");
    delete(&folder.join("iris.csv"))?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0));
    let streams = format!(
        "stdout:\n{}stderr:\n{}",
        text(&output.stdout)?,
        text(&output.stderr)?
    );
    sandbox.filters().bind(|| insta::assert_snapshot!(streams));
    // the rows are the answer -q keeps; the closing line and hints are what it drops
    let quiet = sandbox.run(&["-q", "status"])?;
    assert_eq!(quiet.status.code(), Some(0));
    assert_eq!(text(&quiet.stdout)?, "    Missing data/iris/iris.csv\n");
    assert_eq!(
        crate::support::without_copy_warning(&text(&quiet.stderr)?),
        ""
    );
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(document["result"]["missing"][0], "data/iris/iris.csv");
    assert_eq!(document["result"]["yours"][0], "data/iris/notes.txt");
    Ok(())
}

#[test]
fn status_creates_no_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let unmade = sandbox.work().join("unmade-cache");
    let output = sandbox
        .command(&["status"])
        .env("FETCHLOOM_CACHE", &unmade)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!unmade.exists());
    Ok(())
}

#[test]
fn fetchloom_alone_in_a_project_sums_up_its_status() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let inner = sandbox.work().join("data").join("iris");
    let calm = sandbox.command_in(&inner, &[]).output()?;
    assert_eq!(calm.status.code(), Some(0));
    assert!(calm.stdout.is_empty(), "{}", text(&calm.stdout)?);
    assert_eq!(
        text(&calm.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    delete(&inner.join("iris.csv"))?;
    let missing = sandbox.run(&[])?;
    assert_eq!(
        text(&missing.stderr)?,
        "      Found 1 difference from data.lock\n  hint: `fetchloom sync` puts the pinned files in data/; `fetchloom status` lists them\n"
    );
    let document = json(&sandbox.run(&["--json"])?)?;
    assert_eq!(document["command"], "status");
    assert_eq!(document["result"]["missing"][0], "data/iris/iris.csv");
    Ok(())
}

#[test]
fn project_commands_outside_a_project_say_how_to_start_one() -> Result {
    let output = Sandbox::new()?.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("there is no data.toml in this folder or any folder above it"),
        "{stderr}"
    );
    assert!(stderr.contains("`fetchloom add <link>`"), "{stderr}");
    Ok(())
}

/// data.toml belongs to the repository that commits it, so the outer data.toml is left alone.
#[test]
fn a_git_repository_inside_a_project_starts_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let outer = fs::read(sandbox.work().join("data.toml"))?;
    let inner = sandbox.work().join("experiment");
    fs::create_dir_all(inner.join(".git"))?;
    let output = sandbox.command_in(&inner, &["add", &server.url]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("Started a project in {}", inner.display())),
        "{stderr}"
    );
    assert!(!stderr.contains("using the project in"), "{stderr}");
    assert_eq!(
        fs::read(inner.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    assert!(inner.join("data.toml").is_file());
    assert_eq!(fs::read(sandbox.work().join("data.toml"))?, outer);
    Ok(())
}

/// at the root of a repository whose project is in a folder below, the help names that folder
/// and the command to run there, rather than starting a second project.
#[test]
fn a_project_below_the_repository_root_is_named() -> Result {
    let sandbox = Sandbox::new()?;
    let root = sandbox.work().join("mono");
    fs::create_dir_all(root.join(".git"))?;
    fs::create_dir_all(root.join("analysis"))?;
    fs::write(root.join("analysis").join("data.toml"), "")?;
    fs::create_dir_all(root.join(".hidden"))?;
    fs::write(root.join(".hidden").join("data.toml"), "")?;
    let refused = sandbox.command_in(&root, &["--json", "status"]).output()?;
    assert_eq!(refused.status.code(), Some(50));
    assert_eq!(
        json(&refused)?["errors"][0]["help"],
        "there is one in analysis/: run `fetchloom -C analysis --json status`"
    );
    fs::create_dir_all(root.join("paper").join("draft"))?;
    fs::write(root.join("paper").join("data.toml"), "")?;
    let refused = sandbox.command_in(&root, &["--json", "status"]).output()?;
    assert_eq!(
        json(&refused)?["errors"][0]["help"],
        "there is one in each of analysis/ and paper/: give -C its folder, as in `fetchloom -C analysis --json status`"
    );
    Ok(())
}

/// the search for data.toml stops at the root of the git repository and says so.
#[test]
fn a_project_outside_the_git_repository_needs_its_folder_named() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let inner = sandbox.work().join("experiment");
    fs::create_dir_all(inner.join(".git"))?;
    fs::create_dir_all(inner.join("notebooks"))?;
    let refused = sandbox
        .command_in(&inner.join("notebooks"), &["--json", "path", "iris"])
        .output()?;
    assert_eq!(refused.status.code(), Some(50));
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        format!(
            "there is no data.toml in this folder or any folder above it inside the git repository at {}",
            inner.display()
        )
    );
    assert_eq!(
        error["help"],
        "run `fetchloom add <link>` to start a project here, or give -C the folder of a project outside this repository, as in `fetchloom -C <folder> <command>`"
    );
    assert_eq!(error["fix"], serde_json::Value::Null);
    let used = sandbox
        .command_in(&inner, &["-C", "..", "path", "iris"])
        .output()?;
    assert_eq!(used.status.code(), Some(0), "{}", text(&used.stderr)?);
    assert_eq!(
        text(&used.stdout)?,
        format!("{}\n", sandbox.work().join("data").join("iris").display())
    );
    Ok(())
}

/// the fix, `fetchloom lock`, merges both sides and never picks one for a person.
#[test]
fn an_unfinished_merge_says_how_to_finish_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    for name in ["data.toml", "data.lock"] {
        let path = sandbox.work().join(name);
        let merged = fs::read_to_string(&path)?;
        fs::write(
            &path,
            format!("<<<<<<< HEAD\n{merged}=======\n{merged}>>>>>>> theirs\n"),
        )?;
        let output = sandbox.run(&["--json", "sync"])?;
        assert_eq!(output.status.code(), Some(50), "{name}");
        let report = json(&output)?;
        let error = &report["errors"][0];
        assert_eq!(error["kind"], "project.invalid");
        assert_eq!(
            error["message"],
            format!("{name} has an unfinished git merge conflict")
        );
        assert_eq!(error["details"], serde_json::json!([format!("{name}:1")]));
        assert!(
            error["help"]
                .as_str()
                .is_some_and(|help| help.contains("`fetchloom lock`")),
            "{name}: {error}"
        );
        // a lock may download a file to pin it, so it is advice, no fix
        assert_eq!(error["fix"], serde_json::Value::Null, "{name}");
        assert_eq!(report["work"]["requests"], 0);
        fs::write(&path, merged)?;
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_script_never_gets_the_cache_reminder_and_nothing_is_freed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let removed = sandbox.run(&["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    // the reminder only runs for a person at a terminal, and it never deletes
    assert!(!sandbox.cache().join("reminded").exists());
    let info = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(info["result"]["objects"], 1);
    Ok(())
}
#[test]
fn the_cache_knows_its_projects_and_checks_its_files() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let info = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(info["result"]["objects"], 1);
    assert_eq!(info["result"]["needed"], IRIS.len());
    assert_eq!(
        info["result"]["projects"][0],
        sandbox.work().display().to_string()
    );
    let gc = sandbox.run(&["cache", "gc"])?;
    assert_eq!(text(&gc.stderr)?, "         Ok nothing to free\n");
    let verify = sandbox.run(&["cache", "verify"])?;
    assert_eq!(verify.status.code(), Some(0));
    assert_eq!(
        text(&verify.stderr)?,
        "   Verified 1 file · 32 B, every one is intact\n"
    );
    fs::remove_file(sandbox.work().join("data.toml"))?;
    let forgotten = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(
        forgotten["result"]["projects"].as_array().map(Vec::len),
        Some(0),
        "a project without data.toml is forgotten"
    );
    let young = sandbox.run(&["cache", "gc"])?;
    assert_eq!(
        text(&young.stderr)?,
        format!(
            "     Forgot {} · it has no data.toml anymore\n         Ok nothing to free yet\n  hint: 32 B no project needs is kept for a day in case you want it back; `fetchloom cache gc --now` frees it now\n  hint: if a project moved, run `fetchloom sync` in its new folder, so gc keeps its files\n",
            sandbox.work().display()
        )
    );
    let document = json(&sandbox.run(&["--json", "cache", "gc"])?)?;
    assert_eq!(document["result"]["kept_objects"], 1);
    assert_eq!(document["result"]["kept_bytes"], IRIS.len());
    let now = json(&sandbox.run(&["--json", "cache", "gc", "--now"])?)?;
    assert_eq!(now["result"]["removed_objects"], 1);
    assert_eq!(now["result"]["kept_objects"], 0);
    Ok(())
}

#[test]
fn a_changed_object_fails_verify_and_waits_for_replacement() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let objects = sandbox.cache().join("objects");
    let fan = fs::read_dir(&objects)?
        .next()
        .ok_or("no fan-out folder")??;
    let object = fs::read_dir(fan.path())?.next().ok_or("no object")??.path();
    edit(&object, b"damaged on disk")?;
    let verify = sandbox.run(&["cache", "verify"])?;
    assert_eq!(verify.status.code(), Some(30));
    let stderr = text(&verify.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: {} has changed, so fetchloom downloads it again the next time it is needed",
            object.display()
        )),
        "{stderr}"
    );
    assert_eq!(fs::read(&object)?, b"damaged on disk");
    // a reflink or copy in the project still holds the locked bytes, so only a placement that
    // needs the object again brings the replacement
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    if fs::read(&placed)? == IRIS {
        delete(&placed)?;
    }
    let synced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&object)?, IRIS);
    assert_eq!(fs::read(&placed)?, IRIS);
    Ok(())
}

/// a clone of the project in a folder whose path is `length` characters long.
#[cfg(windows)]
fn deep_clone(sandbox: &Sandbox, length: usize) -> Result<std::path::PathBuf> {
    let mut deep = sandbox.folder("deep")?;
    while length - deep.as_os_str().len() > 40 {
        deep.push("a_project_folder_name");
    }
    let rest = length - deep.as_os_str().len() - 1;
    deep.push("p".repeat(rest));
    fs::create_dir_all(&deep)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), deep.join(file))?;
    }
    Ok(deep)
}

#[cfg(windows)]
#[test]
fn a_project_whose_metadata_passes_260_characters_syncs_and_answers_path() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // as deep as the folder in the report, which windows can still work in
    let deep = deep_clone(&sandbox, 240)?;
    let output = sandbox.command_in(&deep, &["--json", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert!(deep.join(".fetchloom").join("state").is_file());
    let placed = deep.join("data").join("iris").join("iris.csv");
    assert_eq!(fs::read(&placed)?, IRIS);
    delete(&placed)?;
    let output = sandbox
        .command_in(&deep, &["path", "iris/iris.csv"])
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(text(&output.stdout)?.trim())?, IRIS);
    let status = sandbox.command_in(&deep, &["--json", "status"]).output()?;
    assert_eq!(json(&status)?["status"], "ok");
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_folder_too_long_to_work_in_is_named_with_its_length_before_anything_is_written() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let deep = deep_clone(&sandbox, 300)?;
    let named = deep.display().to_string();
    let output = sandbox.command(&["-C", &named, "sync"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(&named) && stderr.contains("300 characters"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "move the project to a folder with a shorter path, then run `fetchloom sync` there"
        ),
        "{stderr}"
    );
    for (arguments, expected) in [
        (
            vec![format!("-C{named}"), "sync".to_owned()],
            "fetchloom sync",
        ),
        (
            vec![format!("-C={named}"), "sync".to_owned()],
            "fetchloom sync",
        ),
        (
            vec![
                format!("-vC{named}"),
                "sync".to_owned(),
                "--offline".to_owned(),
            ],
            "fetchloom -v sync --offline",
        ),
    ] {
        let arguments: Vec<_> = arguments.iter().map(String::as_str).collect();
        let output = sandbox.run(&arguments)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.contains(&format!("then run `{expected}` there")),
            "{stderr}"
        );
    }
    assert!(!deep.join(".fetchloom").exists());
    assert!(!deep.join("data").exists());
    Ok(())
}

#[test]
fn status_and_sync_explain_untracked_conflicts() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    fs::remove_file(sandbox.work().join(".fetchloom").join("state"))?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    fs::write(&placed, b"my work")?;
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("fetchloom sync --force"), "{stderr}");
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(stderr.contains("1 file left unchanged"), "{stderr}");
    assert!(
        !stderr.contains("all pinned files are in place"),
        "{stderr}"
    );
    assert_eq!(fs::read(placed)?, b"my work");
    Ok(())
}

#[test]
fn status_reports_datasets_removed_from_the_manifest() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = clone(&sandbox, "removed")?;
    fs::write(clone.join("data.toml"), "[datasets]\n")?;
    let before = fs::read(clone.join("data.lock"))?;
    let output = sandbox.command_in(&clone, &["status"]).output()?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert!(
        stdout.contains(
            "    Removed iris · not in data.toml, so `fetchloom sync` takes it out of data.lock\n"
        ),
        "{stdout}"
    );
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("everything matches"), "{stderr}");
    let output = sandbox.command_in(&clone, &["--json", "status"]).output()?;
    let report = json(&output)?;
    assert_eq!(report["result"]["removed"], serde_json::json!(["iris"]));
    assert_eq!(report["result"]["unlocked"], serde_json::json!([]));
    assert_eq!(report["work"]["requests"], 0);
    let brief = sandbox.command_in(&clone, &[]).output()?;
    assert!(!text(&brief.stderr)?.contains("everything matches"));
    assert_eq!(fs::read(clone.join("data.lock"))?, before);
    Ok(())
}

/// a project whose data.lock pins 25 files in data/set/a, one each in data/set/b and c and 25 in
/// data/set/d, with only b's in place and a person's own files at d's paths, for status to list.
fn many_differences(sandbox: &Sandbox) -> Result {
    use std::fmt::Write as _;

    let mut lock = String::from(
        "version = 1\n\n[[dataset]]\nname = 'set'\nref = 'zenodo:2'\nspec = ''\nresolved = 'zenodo:2'\nlocked = '2026-01-01T00:00:00Z'\n",
    );
    for (path, sha256) in (0..25)
        .map(|index| (format!("a/file{index:02}.csv"), "0".repeat(64)))
        .chain(std::iter::once((
            "b/kept.csv".to_owned(),
            sha256_hex(b"kept"),
        )))
        .chain(std::iter::once(("c/only.csv".to_owned(), "1".repeat(64))))
        .chain((0..25).map(|index| (format!("d/other{index:02}.csv"), "2".repeat(64))))
    {
        writeln!(
            lock,
            "\n[[dataset.files]]\npath = '{path}'\nsize = 4\nsha256 = '{sha256}'\nurl = 'https://example.org/{path}'"
        )?;
    }
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.set]\nref = 'zenodo:2'\n",
    )?;
    fs::write(sandbox.work().join("data.lock"), lock)?;
    let data = sandbox.work().join("data");
    fs::create_dir_all(data.join("set").join("b"))?;
    fs::write(data.join("set").join("b").join("kept.csv"), b"kept")?;
    fs::create_dir_all(data.join("set").join("d"))?;
    for index in 0..25 {
        fs::write(
            data.join("set")
                .join("d")
                .join(format!("other{index:02}.csv")),
            b"keep",
        )?;
    }
    Ok(())
}

/// a pipe or a file gets every row, as `git status` gives them, so grep finds each path.
#[test]
fn status_lists_every_row_for_a_script() -> Result {
    let sandbox = Sandbox::new()?;
    many_differences(&sandbox)?;
    let output = sandbox.run(&["--offline", "status"])?;
    let stdout = text(&output.stdout)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(stdout.matches("    Missing data/set/a/file").count(), 25);
    assert_eq!(stdout.matches("  Untracked data/set/d/other").count(), 25);
    assert!(!stdout.contains(" more"), "{stdout}");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn status_at_a_terminal_names_a_gone_folder_once_and_caps_each_kind() -> Result {
    let sandbox = Sandbox::new()?;
    many_differences(&sandbox)?;
    let Some(screen) = sandbox.on_a_terminal(&["--offline", "status"])? else {
        return Ok(());
    };
    assert!(
        screen.contains("    Missing data/set/a/ · 25 files\n"),
        "{screen}"
    );
    assert!(!screen.contains("file00.csv"), "{screen}");
    // a folder that held one file is named by that file
    assert!(
        screen.contains("    Missing data/set/c/only.csv\n"),
        "{screen}"
    );
    assert!(
        screen.contains(
            "  Untracked data/set/d/other19.csv · holds other bytes than data.lock pins\n"
        ),
        "{screen}"
    );
    assert!(!screen.contains("other20.csv"), "{screen}");
    assert!(
        screen.contains("            and 5 more · `fetchloom status | less` shows all\n"),
        "{screen}"
    );
    // b/kept.csv holds the pinned bytes, which is no difference
    assert!(
        screen.contains(
            "   In place 1 file already holds the pinned bytes; `fetchloom sync` records it as placed\n"
        ),
        "{screen}"
    );
    assert!(screen.contains("Found 52 differences"), "{screen}");
    Ok(())
}

#[test]
fn status_keeps_all_differences_in_json() -> Result {
    use std::fmt::Write as _;

    let sandbox = Sandbox::new()?;
    let manifest = "[datasets.new]\nref = 'zenodo:1'\n[datasets.set]\nref = 'zenodo:2'\n";
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    let mut lock = String::from("version = 1\n");
    for (name, reference) in [("old", "zenodo:1"), ("set", "zenodo:2")] {
        writeln!(
            lock,
            "\n[[dataset]]\nname = '{name}'\nref = '{reference}'\nspec = ''\nresolved = '{reference}'\nlocked = '2026-01-01T00:00:00Z'"
        )?;
        if name == "set" {
            for index in 0..25 {
                writeln!(
                    lock,
                    "\n[[dataset.files]]\npath = 'file{index:02}.csv'\nsize = 1\nsha256 = '{}'\nurl = 'https://example.org/{index}.csv'",
                    "0".repeat(64)
                )?;
                writeln!(
                    lock,
                    "\n[[dataset.files]]\npath = 'other{index:02}.csv'\nsize = 4\nsha256 = '{}'\nurl = 'https://example.org/other{index}.csv'",
                    "2".repeat(64)
                )?;
            }
        } else {
            writeln!(lock, "files = []")?;
        }
    }
    fs::write(sandbox.work().join("data.lock"), &lock)?;
    let set = sandbox.work().join("data").join("set");
    fs::create_dir_all(&set)?;
    for index in 0..25 {
        fs::write(set.join(format!("other{index:02}.csv")), b"keep")?;
    }
    let output = sandbox.run(&["--offline", "status"])?;
    let stdout = text(&output.stdout)?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("Unlocked new"), "{stdout}");
    assert!(
        stdout.contains("Removed old · not in data.toml"),
        "{stdout}"
    );
    assert!(stdout.contains("Missing data/set/file24.csv"), "{stdout}");
    assert!(stderr.contains("53 differences from data.lock"), "{stderr}");
    let output = sandbox.run(&["--offline", "--json", "status"])?;
    assert_eq!(output.status.code(), Some(0));
    let document = json(&output)?;
    assert_eq!(
        document["result"]["missing"]
            .as_array()
            .ok_or("missing is not an array")?
            .len(),
        25
    );
    assert_eq!(document["result"]["missing"][24], "data/set/file24.csv");
    assert_eq!(
        document["result"]["untracked"]
            .as_array()
            .ok_or("untracked is not an array")?
            .len(),
        25
    );
    assert_eq!(document["result"]["removed"], serde_json::json!(["old"]));
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);
    assert!(!sandbox.work().join(".fetchloom").exists());
    assert_eq!(fs::read(set.join("other24.csv"))?, b"keep");
    Ok(())
}

#[test]
fn manifest_citations_do_not_echo_reference_secrets() -> Result {
    let sandbox = Sandbox::new()?;
    for manifest in [
        r#"datasets = { sample = { ref = "https://example.org/a?token=EM_CANARY", extract = "yes" } }"#,
        r#"datasets = { sample = { ref = "https://user:EM_CANARY@example.org/a", extract = "yes" } }"#,
        r#"datasets = { sample = { "r\u0065f" = "ht\u0074ps://example.org/a?token=EM_CANARY", extract = "yes" } }"#,
        r#"datasets = { sample = { ref = "https://example.org/a?token=EM_CANARY", extract = } }"#,
    ] {
        fs::write(sandbox.work().join("data.toml"), manifest)?;
        for args in [&["status"][..], &["status", "--json"]] {
            let output = sandbox.run(args)?;
            assert_eq!(output.status.code(), Some(50));
            let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
            assert!(shown.contains("data.toml:1"), "{shown}");
            assert!(!shown.contains("EM_CANARY"), "{shown}");
        }
    }
    Ok(())
}

#[test]
fn equal_untracked_read_only_files_get_the_requested_writable_copy() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return crate::support::skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = {:?}\ncopy = true\n", server.url),
    )?;
    let locked = sandbox.run(&["lock"])?;
    assert!(locked.status.success(), "{}", text(&locked.stderr)?);
    let target = sandbox.work().join("data/iris/iris.csv");
    fs::create_dir_all(target.parent().ok_or("target has no parent")?)?;
    let foreign = sandbox.work().join("original.csv");
    fs::write(&foreign, IRIS)?;
    let mut permissions = fs::metadata(&foreign)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&foreign, permissions)?;
    fs::hard_link(&foreign, &target)?;

    let synced = sandbox.run(&["--json", "--offline", "sync"])?;
    assert!(synced.status.success(), "{}", text(&synced.stdout)?);
    assert_eq!(fs::read(&target)?, IRIS);
    assert!(!fs::metadata(&target)?.permissions().readonly());
    assert!(!same_file::is_same_file(&target, &foreign)?);
    assert_eq!(fs::read(&foreign)?, IRIS);
    assert!(fs::metadata(&foreign)?.permissions().readonly());
    let status = sandbox.run(&["--json", "status"])?;
    assert!(status.status.success(), "{}", text(&status.stdout)?);
    assert_eq!(json(&status)?["result"]["outdated"], serde_json::json!([]));
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn sync_reuses_its_validated_plan() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "plan-once")?;
    let trace = sandbox.work().join("sync.trace.json");
    let output = sandbox
        .command_in(&project, &["--json", "--offline", "sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
    let plans = events
        .iter()
        .filter(|event| event["ph"] == "B" && event["name"] == "plan")
        .count();
    assert_eq!(plans, 1);
    assert_eq!(fs::read(project.join("data/iris/iris.csv"))?, IRIS);
    assert_eq!(json(&output)?["work"]["requests"], 0);
    Ok(())
}

#[test]
fn prepared_downloads_keep_pins_and_shared_object_claims() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "prepared")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let second = row.replacen("iris.csv", "z.csv", 1);
    fs::write(&path, lock.replace(row, &format!("{row}\n{second}")))?;
    let parsed: toml::Value = toml::from_str(&lock)?;
    let digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("no digest")?;
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]),
    )?;
    let output = sandbox.command_in(&project, &["--json", "sync"]).output()?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["work"]["requests"], 1);
    assert_eq!(document["work"]["bytes_down"], IRIS.len());
    assert_eq!(document["result"]["placement"]["placed"], 2);
    for file in ["iris.csv", "z.csv"] {
        assert_eq!(fs::read(project.join("data/iris").join(file))?, IRIS);
    }
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

#[test]
fn prepared_downloads_validate_late_links_before_any_transfer() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "invalid-prepared")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let parsed: toml::Value = toml::from_str(&lock)?;
    let digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("no digest")?;
    let invalid = row
        .replacen("iris.csv", "z.csv", 1)
        .replace(&server.url, "not a URL")
        .replace(digest, &"0".repeat(64));
    fs::write(&path, lock.replace(row, &format!("{row}\n{invalid}")))?;
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..]),
    )?;
    let output = sandbox.command_in(&project, &["--json", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "project.invalid");
    assert_eq!(json(&output)?["work"]["requests"], 0);
    assert!(!project.join("data").exists());
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn path_and_status_refuse_case_collisions_before_reporting_locations() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let project = clone(&sandbox, "case-clash")?;
    let path = project.join("data.lock");
    let lock = fs::read_to_string(&path)?;
    let row = lock
        .lines()
        .find(|line| line.starts_with("  { path = "))
        .ok_or("no locked file")?;
    let other = row.replacen("iris.csv", "IRIS.csv", 1);
    fs::write(&path, lock.replace(row, &format!("{other}\n{row}")))?;
    for args in [
        &["status"][..],
        &["path", "iris"],
        &["path", "iris/iris.csv"],
    ] {
        let output = sandbox
            .command_in(&project, &["--json", "--offline"])
            .args(args)
            .output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], "project.invalid");
        assert_eq!(document["work"]["requests"], 0);
        assert!(!project.join("data").exists());
        assert!(!project.join(".fetchloom").exists());
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// `data_dir` cannot lead out of the project, so the help never suggests setting it: a link
/// to a folder no `data_links` entry holds gets the command that lists the folder.
#[cfg(unix)]
#[test]
fn a_linked_data_folder_is_refused_with_advice_that_works() -> Result {
    let sandbox = Sandbox::new()?;
    let elsewhere = sandbox.folder("elsewhere")?;
    std::os::unix::fs::symlink(&elsewhere, sandbox.work().join("data"))?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--json", "add", &server.url])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(!help.contains("set data_dir"), "{help}");
    assert!(
        help.ends_with(&format!(
            "list that folder with `fetchloom settings set data_links {}`",
            elsewhere.display()
        )),
        "{help}"
    );
    assert_eq!(fs::read_dir(&elsewhere)?.count(), 0);
    Ok(())
}

/// the survey of a data folder reached through a link stops before it reads anything there, so
/// status names no file of the folder the link leads to.
#[cfg(unix)]
#[test]
fn status_reads_nothing_through_a_linked_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let data = sandbox.work().join("data");
    crate::support::delete_folder(&data)?;
    let elsewhere = sandbox.folder("elsewhere")?;
    fs::create_dir_all(elsewhere.join("iris"))?;
    fs::write(elsewhere.join("iris").join("secret.txt"), "not yours\n")?;
    std::os::unix::fs::symlink(&elsewhere, &data)?;
    let output = sandbox.run(&["--json", "status"])?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(50), "{document}");
    assert_eq!(
        document["errors"][0]["kind"], "project.invalid",
        "{document}"
    );
    assert!(!document.to_string().contains("secret.txt"), "{document}");
    Ok(())
}

/// a state written for another folder, as a copy or a clone brings it along, is not used, so its
/// records cannot make sync, remove or exclude delete a file of this folder's own with the bytes
/// they name; in its own folder the same state still lets remove delete the files it placed.
#[test]
fn a_state_from_another_folder_cannot_make_a_command_delete_a_file() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("shared")?;
    for name in ["a.csv", "b.csv"] {
        fs::write(source.join(name), name)?;
    }
    let added = sandbox.run(&["add", "../shared"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let state = fs::read(sandbox.work().join(".fetchloom").join("state"))?;
    for (name, args) in [
        ("synced", &["--offline", "sync"][..]),
        ("removed", &["--offline", "--yes", "remove", "shared"]),
        ("excluded", &["--offline", "exclude", "data/shared/a.csv"]),
    ] {
        let folder = clone(&sandbox, name)?;
        if name == "synced" {
            fs::write(folder.join("data.toml"), "")?;
        }
        fs::create_dir_all(folder.join(".fetchloom"))?;
        fs::write(folder.join(".fetchloom").join("state"), &state)?;
        let own = folder.join("data").join("shared");
        fs::create_dir_all(&own)?;
        for file in ["a.csv", "b.csv"] {
            fs::write(own.join(file), file)?;
        }
        let output = sandbox.command_in(&folder, args).output()?;
        let stderr = text(&output.stderr)?;
        for file in ["a.csv", "b.csv"] {
            assert_eq!(
                fs::read(own.join(file))?,
                file.as_bytes(),
                "{name}: {stderr}"
            );
        }
    }
    let removed = sandbox.run(&["--offline", "--yes", "remove", "shared"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!sandbox.work().join("data/shared/a.csv").exists());
    Ok(())
}

/// a copy not yet synced has no state of its own, so remove cannot tell which files of the
/// dataset's folder it placed: it deletes none and says so in one line, as its dry run does. the
/// project the copy came from deletes them and says nothing of the kind.
#[test]
fn remove_in_a_copy_says_what_it_keeps_and_why() -> Result {
    let sandbox = Sandbox::new()?;
    let source = sandbox.folder("shared")?;
    for name in ["a.csv", "b.csv"] {
        fs::write(source.join(name), name)?;
    }
    let added = sandbox.run(&["add", "../shared"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let copy = clone(&sandbox, "copy")?;
    fs::create_dir_all(copy.join(".fetchloom"))?;
    let state = sandbox.work().join(".fetchloom").join("state");
    fs::copy(state, copy.join(".fetchloom").join("state"))?;
    let own = copy.join("data").join("shared");
    fs::create_dir_all(&own)?;
    for name in ["a.csv", "b.csv"] {
        fs::write(own.join(name), name)?;
    }
    let why =
        "its 2 files, since fetchloom cannot tell which files it placed in this project folder";
    let dry = sandbox
        .command_in(&copy, &["remove", "--dry-run", "shared"])
        .output()?;
    let stderr = text(&dry.stderr)?;
    assert!(
        stderr.contains(&format!("warning: data/shared would keep {why}\n")),
        "{stderr}"
    );
    let removed = sandbox
        .command_in(&copy, &["--yes", "remove", "shared"])
        .output()?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!("warning: data/shared keeps {why}\n")),
        "{stderr}"
    );
    assert_eq!(fs::read(own.join("a.csv"))?, b"a.csv");
    let here = sandbox.run(&["--yes", "remove", "shared"])?;
    let stderr = text(&here.stderr)?;
    assert!(!stderr.contains("cannot tell"), "{stderr}");
    assert!(!sandbox.work().join("data/shared/a.csv").exists());
    Ok(())
}

/// iris placed in `sandbox`, its state then recording the folder as a move on fat or exfat leaves
/// it: another folder number but the folder's own creation time, or on windows, which takes no
/// number there, the time alone. false on a disk that keeps no creation time, or keeps it to the
/// whole second, as hfs+ does, where fetchloom records none.
fn moved_on_a_disk_without_lasting_numbers(sandbox: &Sandbox, server: &Server) -> Result<bool> {
    let Ok(created) = fs::metadata(sandbox.work())?.created() else {
        return Ok(false);
    };
    let created = created.duration_since(std::time::UNIX_EPOCH)?;
    if created.subsec_nanos() == 0 {
        return Ok(false);
    }
    added(sandbox, server)?;
    let path = sandbox.work().join(".fetchloom").join("state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let inode = state["folder"]["inode"].as_u64().unwrap_or_default();
    let created = u64::try_from(created.as_nanos())?;
    let folder = state["folder"]
        .as_object_mut()
        .ok_or("the state names its folder")?;
    folder.insert("created".to_owned(), created.into());
    if cfg!(windows) {
        folder.remove("inode");
    } else {
        folder.insert("inode".to_owned(), inode.wrapping_add(1).into());
    }
    fs::write(&path, serde_json::to_vec(&state)?)?;
    Ok(true)
}

/// fat and exfat keep no lasting file numbers, so a move gives the folder another while its
/// creation time stays: a state that records that time is still the folder's, and remove deletes
/// the file fetchloom placed.
#[test]
fn a_state_whose_folder_kept_its_creation_time_is_still_used() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    if !moved_on_a_disk_without_lasting_numbers(&sandbox, &server)? {
        return crate::support::skipped("creation-time", "this disk keeps no fine creation time");
    }
    let mut remove = sandbox.command(&["--offline", "remove", "iris"]);
    // windows takes a folder's number everywhere but on fat or exfat, which this disk then must be
    if cfg!(windows) {
        remove.env("FETCHLOOM_TEST_FILESYSTEM", "exfat");
    }
    let removed = remove.output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!sandbox.work().join("data/iris/iris.csv").exists());
    Ok(())
}

/// hfs+ keeps creation times to the whole second, which a copy made in the same second shares, so
/// there a matching time vouches for no folder: the state is not used, and remove leaves the file
/// it cannot tell fetchloom placed.
#[test]
fn a_creation_time_on_hfs_vouches_for_no_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    if !moved_on_a_disk_without_lasting_numbers(&sandbox, &server)? {
        return crate::support::skipped("creation-time", "this disk keeps no fine creation time");
    }
    let removed = sandbox
        .command(&["--offline", "remove", "iris"])
        .env("FETCHLOOM_TEST_FILESYSTEM", "hfs")
        .output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(sandbox.work().join("data/iris/iris.csv").exists());
    Ok(())
}

/// a renamed folder is the same folder, so its state is still used: remove deletes the file
/// fetchloom placed, with no sync first, which would write the state again.
#[test]
fn a_renamed_project_folder_keeps_its_state() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let renamed = sandbox.folder("moved")?.join("work");
    fs::rename(sandbox.work(), &renamed)?;
    let removed = sandbox
        .command_in(&renamed, &["--offline", "remove", "iris"])
        .output()?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!renamed.join("data/iris/iris.csv").exists());
    Ok(())
}

/// each machine numbers a network disk its own way, so a state that records another device for
/// the same folder is still this folder's: remove deletes the file fetchloom placed.
#[cfg(unix)]
#[test]
fn a_state_that_records_another_device_is_still_used() -> Result {
    use std::os::unix::fs::MetadataExt as _;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let path = sandbox.work().join(".fetchloom").join("state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let device = fs::metadata(sandbox.work())?.dev();
    state["folder"]["device"] = serde_json::json!(device.wrapping_add(1));
    fs::write(&path, serde_json::to_vec(&state)?)?;
    let removed = sandbox.run(&["--offline", "remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!sandbox.work().join("data/iris/iris.csv").exists());
    Ok(())
}

/// an owner file far larger than any fetchloom writes, as a cloned project can hold, is removed
/// with a warning in plain words, and the command goes on.
#[test]
fn an_owner_record_too_large_to_be_one_is_removed_and_told_plainly() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let owner = sandbox.work().join(".fetchloom").join("owner");
    fs::write(&owner, vec![b' '; 1 << 20])?;
    delete(&sandbox.work().join("data/iris/iris.csv"))?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.starts_with(&format!(
            "warning: removed {}, which no fetchloom wrote for this folder, as a cloned or copied project can hold, and went on\n",
            owner.display()
        )),
        "{stderr}"
    );
    for internal in ["claim", "record"] {
        assert!(!stderr.contains(internal), "{internal}: {stderr}");
    }
    assert!(!owner.exists());
    Ok(())
}

/// a folder this test cannot write stands in for a full disk.
#[cfg(unix)]
#[test]
fn a_sync_that_cannot_save_what_it_placed_names_the_file() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    delete(&sandbox.work().join("data/iris/iris.csv"))?;
    let own = sandbox.work().join(".fetchloom");
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555))?;
    // a user who may write anywhere, such as root, cannot show this
    if fs::write(own.join("probe"), b"").is_ok() {
        fs::remove_file(own.join("probe"))?;
        fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let output = sandbox.run(&["sync"]);
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
    let output = output?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "error: could not save the list of placed files in {}: permission was denied",
            own.join("state").display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// the owner record only tells a waiting command who holds the project, and the state here would
/// only note files hashed as unchanged. an unwritable folder stands in for a full disk.
#[cfg(unix)]
#[test]
fn a_sync_that_cannot_write_the_owner_record_still_works() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let own = sandbox.work().join(".fetchloom");
    // the state as new as the file it records, as a coarse clock leaves them, so sync hashes the
    // file and would settle it
    let placed = fs::metadata(sandbox.work().join("data/iris/iris.csv"))?.modified()?;
    fs::File::options()
        .write(true)
        .open(own.join("state"))?
        .set_times(fs::FileTimes::new().set_modified(placed))?;
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555))?;
    // a user who may write anywhere, such as root, cannot show this
    if fs::write(own.join("probe"), b"").is_ok() {
        fs::remove_file(own.join("probe"))?;
        fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let output = sandbox.run(&["sync"]);
    let blocked = sandbox.run(&["--json", "sync"]);
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
    let (output, blocked) = (output?, blocked?);
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert_eq!(blocked.status.code(), Some(0), "{}", text(&blocked.stdout)?);
    let left: Vec<String> = fs::read_dir(&own)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<_>>()?;
    assert!(
        !left.iter().any(|name| name.starts_with("owner")),
        "{left:?}"
    );
    Ok(())
}

/// naming what was pinned makes a typo in data.toml that pins an unrelated record stand out.
#[test]
fn a_first_pin_by_lock_or_sync_gets_a_line_of_its_own() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    for (command, name) in [("lock", "cows"), ("sync", "hens")] {
        let other = Server::start(
            &format!("/files/{name}.csv"),
            vec![file(b"cow\nbess\n", "\"c1\"")],
        )?;
        sandbox.allow(&other.url);
        let updated = format!(
            "{}\n[datasets.{name}]\nref = \"{}\"\n",
            fs::read_to_string(&manifest)?,
            other.url
        );
        fs::write(&manifest, updated)?;
        let output = sandbox.run(&[command])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let line = format!(
            "     Pinned {name}, new in data.lock: {} · 1 file · 9 B\n",
            other.url
        );
        assert!(stderr.contains(&line), "{command}: {stderr}");
        assert!(
            !stderr.contains(&format!("Locked {name}")),
            "{command}: {stderr}"
        );
    }
    Ok(())
}

/// a pin counts as changed when it is new, pins other data or is gone.
#[test]
fn json_names_the_datasets_whose_pins_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let first = fs::read_to_string(&manifest)?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    sandbox.allow(&cows.url);
    let hens = Server::start("/files/hens.csv", vec![file(b"hen\n", "\"h\"")])?;
    sandbox.allow(&hens.url);
    let both = format!(
        "{first}\n[datasets.cows]\nref = \"{}\"\n\n[datasets.hens]\nref = \"{}\"\n",
        cows.url, hens.url
    );
    fs::write(&manifest, &both)?;
    let previewed = json(&sandbox.run(&["--json", "sync", "--dry-run"])?)?;
    assert_eq!(
        previewed["result"]["datasets"],
        serde_json::json!(["cows", "hens"])
    );
    let synced = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(synced["result"]["lock_changed"], true, "{synced}");
    assert_eq!(
        synced["result"]["datasets"],
        serde_json::json!(["cows", "hens"])
    );
    fs::write(
        &manifest,
        both.replace("[datasets.iris]", "[datasets.iris-renamed]"),
    )?;
    let locked = json(&sandbox.run(&["--json", "lock"])?)?;
    assert_eq!(
        locked["result"]["datasets"],
        serde_json::json!(["iris", "iris-renamed"]),
        "{locked}"
    );
    let unchanged = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(unchanged["result"]["datasets"], serde_json::json!([]));
    Ok(())
}

/// a link no checksum pins is a file to fetch, not a change, since only its bytes can tell.
#[test]
fn lock_dry_run_tells_what_would_change_and_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    sandbox.allow(&cows.url);
    let manifest = sandbox.work().join("data.toml");
    let updated = format!(
        "{}\n[datasets.cows]\nref = \"{}\"\n",
        fs::read_to_string(&manifest)?,
        cows.url
    );
    fs::write(&manifest, updated)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["lock", "--upgrade", "--dry-run"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  Would pin cows, new in data.lock:"),
        "{stderr}"
    );
    assert!(stderr.contains("Would fetch iris/iris.csv"), "{stderr}");
    assert!(!stderr.contains("upgrade iris"), "{stderr}");
    assert!(stderr.contains("dry run, nothing was changed"), "{stderr}");
    let document = json(&sandbox.run(&["--json", "lock", "--upgrade", "-n"])?)?;
    let result = &document["result"];
    assert_eq!(result["dry_run"], true, "{document}");
    assert_eq!(result["lock_changed"], true);
    assert_eq!(result["datasets"], serde_json::json!(["cows"]));
    assert_eq!(result["placement"], serde_json::Value::Null);
    assert_eq!(
        result["changes"]["download"],
        serde_json::json!([
            { "path": "cows/cows.csv", "size": null },
            { "path": "iris/iris.csv", "size": null },
        ])
    );
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert_eq!(server.requests().len(), 1, "only the first add downloads");
    assert_eq!(cows.requests().len(), 0);
    Ok(())
}

/// how many bytes the partial downloads in `cache` hold.
#[cfg(unix)]
pub(crate) fn partial_bytes(cache: &std::path::Path) -> u64 {
    crate::support::partial_files(cache)
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "part")
        })
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// the line under `stopped` in `stderr`, which says what a stopped download kept, once stderr
/// shows a stop as a person asked for it: a note that a second ctrl-c quits at once, then
/// `stopped` and that line last, and no `error:`.
#[cfg(unix)]
pub(crate) fn told_a_stop<'s>(stderr: &'s str, stopped: &str) -> Result<&'s str> {
    told_a_stop_kept_or_not(stderr, stopped)?
        .ok_or_else(|| format!("stderr says nothing was kept: {stderr}").into())
}

/// [`told_a_stop`] for a stop that may land before a download's first write reaches the disk,
/// when there is nothing kept to tell, so `stopped` may be the last line.
#[cfg(unix)]
pub(crate) fn told_a_stop_kept_or_not<'s>(
    stderr: &'s str,
    stopped: &str,
) -> Result<Option<&'s str>> {
    let told: Vec<&str> = stderr.lines().collect();
    assert!(
        told.contains(&"   Stopping (press ctrl-c again to quit at once)"),
        "{stderr}"
    );
    assert!(!stderr.contains("error:"), "{stderr}");
    match told.as_slice() {
        [.., last] if *last == stopped => Ok(None),
        [.., last, kept] if *last == stopped => {
            assert!(
                kept.ends_with(" downloaded so far is kept, and the next run continues from there"),
                "{stderr}"
            );
            Ok(Some(*kept))
        }
        _ => Err(format!("stderr does not end with the stop's lines: {stderr}").into()),
    }
}

/// the sync here writes neither data.lock nor data/ until every file is in.
#[cfg(unix)]
#[test]
fn a_stopped_sync_says_so_in_a_plain_line() -> Result {
    let sandbox = Sandbox::new()?;
    let slow = Server::held_file("/files/cows.csv")?;
    sandbox.allow(&slow.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.cows]\nref = \"{}\"\n", slow.url),
    )?;
    let running = Running::start(&mut sandbox.command(&["sync"]))?;
    wait_until(
        "bytes to arrive",
        || Ok(partial_bytes(&sandbox.cache()) > 0),
    )?;
    running.interrupt()?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let kept = told_a_stop(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    assert!(kept.starts_with("            the "), "{stderr}");
    assert!(!sandbox.work().join("data.lock").exists(), "{stderr}");
    Ok(())
}

/// a scheduler sends a SIGTERM to a job it ends, and `--json` still gets its one document.
#[cfg(unix)]
#[test]
fn a_sigterm_stops_a_sync_as_a_first_ctrl_c_does() -> Result {
    for args in [&["sync"][..], &["--json", "sync"]] {
        let sandbox = Sandbox::new()?;
        let slow = Server::held_file("/files/cows.csv")?;
        sandbox.allow(&slow.url);
        fs::write(
            sandbox.work().join("data.toml"),
            format!("[datasets.cows]\nref = \"{}\"\n", slow.url),
        )?;
        let running = Running::start(&mut sandbox.command(args))?;
        wait_until(
            "bytes to arrive",
            || Ok(partial_bytes(&sandbox.cache()) > 0),
        )?;
        running.signal("-TERM")?;
        let output = running.finish()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(130), "{args:?}: {stderr}");
        if args[0] == "--json" {
            let document = json(&output)?;
            assert_eq!(document["errors"][0]["kind"], "interrupted", "{document}");
            assert_eq!(
                document["errors"][0]["message"],
                "stopped by a SIGTERM while talking to 127.0.0.1, so nothing changed",
                "{document}"
            );
        } else {
            let told: Vec<&str> = stderr.lines().collect();
            for line in [
                "   Stopping (a second SIGTERM quits at once)",
                "    Stopped by a SIGTERM while talking to 127.0.0.1, so nothing changed",
            ] {
                assert!(told.contains(&line), "{line:?} in\n{stderr}");
            }
            assert!(!stderr.contains("error:"), "{stderr}");
        }
        assert!(!sandbox.work().join("data.lock").exists(), "{args:?}");
    }
    Ok(())
}

/// the next sync goes on from all of them, so the stop names their total.
#[cfg(unix)]
#[test]
fn a_stopped_download_of_several_files_says_what_they_all_keep() -> Result {
    let sandbox = Sandbox::new()?;
    let cows = Server::held_file("/files/cows.csv")?;
    sandbox.allow(&cows.url);
    let pigs = Server::held_file("/files/pigs.csv")?;
    sandbox.allow(&pigs.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.cows]\nref = \"{}\"\n\n[datasets.pigs]\nref = \"{}\"\n",
            cows.url, pigs.url
        ),
    )?;
    let running = Running::start(&mut sandbox.command(&["sync"]))?;
    wait_until("bytes of both files to arrive", || {
        let started = crate::support::partial_files(&sandbox.cache())
            .iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "part")
            })
            .filter(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0))
            .count();
        Ok(started == 2)
    })?;
    running.interrupt()?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let kept = told_a_stop(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    let kept = kept
        .trim_start()
        .strip_prefix("the ")
        .and_then(|rest| rest.split_once(" downloaded so far"))
        .map(|(size, _)| size)
        .ok_or_else(|| format!("no size in {kept}"))?;
    let rerun = sandbox.run(&["sync"])?;
    let told = text(&rerun.stderr)?;
    assert!(rerun.status.success(), "{told}");
    let earlier = told
        .lines()
        .find_map(|line| {
            line.strip_suffix(" from an earlier run")?
                .rsplit(" \u{b7} ")
                .next()
        })
        .ok_or_else(|| format!("the second sync continued nothing: {told}"))?;
    assert_eq!(kept, earlier, "{stderr}\n{told}");
    Ok(())
}

/// ctrl-c at the question stops downloads already running rather than waiting for them.
#[cfg(target_os = "linux")]
#[test]
fn a_cancel_at_a_question_stops_the_other_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let large = Server::start("/huge.bin", vec![huge(), huge()])?;
    sandbox.allow(&large.url);
    let slow = Server::held_file("/files/slow.csv")?;
    sandbox.allow(&slow.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.huge]\nref = \"{}\"\n\n[datasets.slow]\nref = \"{}\"\n",
            large.url, slow.url
        ),
    )?;
    let mut sync = sandbox.command(&["sync"]);
    sync.env("FETCHLOOM_ASK_ABOVE", "1G");
    // -isig lets ctrl-c reach the question as the key it reads
    let Some((exit, screen)) = crate::support::typed(
        &sandbox.work(),
        &sync,
        "-isig",
        "download huge/huge.bin (2.0 GiB)?",
        "\u{3}",
    )?
    else {
        return Ok(());
    };
    assert_eq!(exit, Some(130), "{screen}");
    assert!(!screen.contains("error:"), "{screen}");
    let held = u64::try_from(crate::support::held_body().len())?;
    let mut folders = vec![sandbox.cache().join("objects")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).into_iter().flatten().flatten() {
            let found = entry.metadata()?;
            assert!(found.is_dir() || found.len() != held, "{screen}");
            if found.is_dir() {
                folders.push(entry.path());
            }
        }
    }
    Ok(())
}

/// a sync places files only once all are in the cache, so a fresh clone's stop changes nothing.
#[cfg(unix)]
#[test]
fn a_sync_stopped_while_it_downloads_pinned_files_says_nothing_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let site = crate::support::Ranged::start("/files/cows.csv", body, 0)?;
    sandbox.allow(&site.url);
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        format!("[datasets.cows]\nref = \"{}\"\n", site.url),
    )?;
    let locked = sandbox.run(&["lock"])?;
    assert!(locked.status.success(), "{}", text(&locked.stderr)?);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    site.pace(64 * 1024);
    let asked = site.requests().len();
    // a cache without the file, as on another machine
    let cache = sandbox.folder("another cache")?;
    let mut sync = sandbox.command(&["sync"]);
    sync.env("FETCHLOOM_CACHE", &cache);
    let running = Running::start(&mut sync)?;
    // the paced answer takes 16 s, so a stop once the sync asks for it lands mid-download; its
    // first bytes reach the disk only with a whole write batch, seconds in
    wait_until("the sync to ask for the file", || {
        Ok(site.requests().len() > asked)
    })?;
    running.interrupt()?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    told_a_stop_kept_or_not(
        &stderr,
        "    Stopped while talking to 127.0.0.1, so nothing changed",
    )?;
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    assert!(!work.join("data/cows").exists(), "{stderr}");
    Ok(())
}

/// the sync asks only after it brings data.lock in line with data.toml, which may write it.
#[cfg(target_os = "linux")]
#[test]
fn a_cancelled_download_of_a_sync_says_nothing_changed_only_when_so() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let large = Server::start("/files/huge.bin", vec![file(b"eleven byte", "\"h\"")])?;
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\n", "\"c\"")])?;
    for (server, name) in [(&large, "huge"), (&cows, "cows")] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url, "--as", name])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    // data.lock now pins 2 GiB of other bytes for the file, which the cache does not hold, so
    // placing it again asks first
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    let grown = pinned
        .replace("size = 11,", "size = 2147483648,")
        .replace(&sha256_hex(b"eleven byte"), &sha256_hex(b"two gibibytes"));
    assert_eq!(grown.matches("2147483648").count(), 1, "{grown}");
    assert!(grown.contains(&sha256_hex(b"two gibibytes")), "{grown}");
    fs::write(work.join("data.lock"), &grown)?;
    delete(&work.join("data/huge/huge.bin"))?;
    let mut sync = sandbox.command(&["sync"]);
    sync.env("FETCHLOOM_ASK_ABOVE", "1G");
    let question = "download 1 file (2.0 GiB)?";
    let Some((exit, screen)) = crate::support::typed(&work, &sync, "", question, "n\r")? else {
        return Ok(());
    };
    assert_eq!(exit, Some(1), "{screen}");
    assert!(
        screen.contains("  Cancelled the download of 1 file (2.0 GiB), so nothing changed\n"),
        "{screen}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, grown);
    // with cows gone from data.toml, the sync writes data.lock before it asks
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    let kept: Vec<&str> = manifest
        .split("\n\n")
        .filter(|table| !table.starts_with("[datasets.cows]"))
        .collect();
    fs::write(work.join("data.toml"), kept.join("\n\n"))?;
    let Some((exit, screen)) = crate::support::typed(&work, &sync, "", question, "n\r")? else {
        return Ok(());
    };
    assert_eq!(exit, Some(1), "{screen}");
    assert!(
        screen.contains("  Cancelled the download of 1 file (2.0 GiB)\n"),
        "{screen}"
    );
    assert!(!screen.contains("nothing changed"), "{screen}");
    assert!(
        !fs::read_to_string(work.join("data.lock"))?.contains("cows"),
        "{screen}"
    );
    Ok(())
}

/// sets every folder and file under `root` read-only, or writable again, as a checkout on a
/// read-only mount or owned by another user is.
#[cfg(unix)]
pub(crate) fn writable(root: &std::path::Path, yes: bool) -> Result {
    use std::os::unix::fs::PermissionsExt;
    let mut paths = vec![root.to_path_buf()];
    let mut at = 0;
    while at < paths.len() {
        if paths[at].is_dir() {
            for entry in fs::read_dir(&paths[at])? {
                paths.push(entry?.path());
            }
        }
        at += 1;
    }
    for path in &paths {
        let mode = match (path.is_dir(), yes) {
            (true, true) => 0o755,
            (true, false) => 0o555,
            (false, true) => 0o644,
            (false, false) => 0o444,
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// a folder where fetchloom keeps a file of its own, the cache's lock or the project's list of
/// placed files, is named with what to do about it.
#[test]
fn a_folder_in_place_of_a_file_fetchloom_keeps_is_named_with_the_fix() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = sandbox.cache().join("gc.lock");
    delete(&lock)?;
    fs::create_dir(&lock)?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "{} is a folder where fetchloom keeps its lock file: remove that folder",
            lock.display()
        )),
        "{stderr}"
    );
    // moving the cache elsewhere is no fix for a folder in it
    assert!(!stderr.contains("FETCHLOOM_CACHE"), "{stderr}");
    fs::remove_dir(&lock)?;
    let state = sandbox.work().join(".fetchloom").join("state");
    delete(&state)?;
    fs::create_dir(&state)?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "help: remove the folder {}, where fetchloom keeps that list",
            state.display()
        )),
        "{stderr}"
    );
    Ok(())
}

/// a job often syncs before it reads, on a checkout it may not write.
#[cfg(unix)]
#[test]
fn a_read_only_project_with_nothing_to_do_syncs() -> Result {
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    // a sync right after placing, as a fast machine or an image build runs it, finds file times
    // no older than its state's, so it hashes them before trusting them
    fs::File::options()
        .write(true)
        .open(sandbox.work().join(".fetchloom/state"))?
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_hours(24))?;
    let fresh = clone(&sandbox, "fresh")?;
    writable(&sandbox.work(), false)?;
    writable(&fresh, false)?;
    let settled = sandbox.run(&["--json", "sync"]);
    let locked = sandbox.run(&["sync", "--locked"]);
    let unsynced = sandbox.command_in(&fresh, &["--json", "sync"]).output();
    // a container's image may hold the cache read-only as well
    writable(&sandbox.cache(), false)?;
    let sealed = sandbox.run(&["sync"]);
    writable(&sandbox.cache(), true)?;
    writable(&sandbox.work(), true)?;
    writable(&fresh, true)?;
    let settled = settled?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let sealed = sealed?;
    assert_eq!(sealed.status.code(), Some(0), "{}", text(&sealed.stderr)?);
    let document = json(&settled)?;
    assert_eq!(document["result"]["lock_changed"], false);
    assert_eq!(document["result"]["placement"]["unchanged"], 1);
    let locked = locked?;
    assert_eq!(locked.status.code(), Some(0), "{}", text(&locked.stderr)?);
    assert!(
        text(&locked.stderr)?.contains("all pinned files are in place"),
        "{}",
        text(&locked.stderr)?
    );
    // a clone never synced would place files, so it needs the hold it cannot take
    let unsynced = unsynced?;
    assert_eq!(
        unsynced.status.code(),
        Some(50),
        "{}",
        text(&unsynced.stderr)?
    );
    assert_eq!(
        json(&unsynced)?["errors"][0]["message"],
        format!(
            "could not create {}: the project folder {} is read-only",
            fresh.join(".fetchloom").display(),
            fresh.display()
        )
    );
    Ok(())
}

/// a synced checkout that must place a file again, on a mount it may not write, names the folder
/// it cannot write, as one never synced does.
#[cfg(unix)]
#[test]
fn a_read_only_synced_project_that_must_write_names_its_folder() -> Result {
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let work = sandbox.work();
    delete(&work.join("data").join("iris").join("iris.csv"))?;
    writable(&work, false)?;
    let output = sandbox.run(&["--json", "sync"]);
    writable(&work, true)?;
    let output = output?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        format!(
            "could not lock the project with {}: the project folder {} is read-only",
            work.join(".fetchloom").join("lock").display(),
            work.display()
        ),
        "{error}"
    );
    assert_eq!(
        error["help"],
        "make that folder writable for you, then run `fetchloom --json sync` again"
    );
    Ok(())
}

/// an add places what it adds alone, and a project settled before it stays settled, so a sync
/// on a checkout it may not write has nothing to do.
#[cfg(unix)]
#[test]
fn an_add_leaves_a_settled_project_settled() -> Result {
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let tips = Server::start("/files/tips.csv", vec![file(b"tip\n1\n", "\"t1\"")])?;
    sandbox.allow(&tips.url);
    let output = sandbox.run(&["add", &tips.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    writable(&sandbox.work(), false)?;
    let settled = sandbox.run(&["sync"]);
    writable(&sandbox.work(), true)?;
    let settled = settled?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    Ok(())
}

/// each download fits on its own, so only their sum shows the shortfall.
#[test]
fn a_sync_the_cache_disk_cannot_hold_asks_once_before_downloading() -> Result {
    let sandbox = Sandbox::new()?;
    // two different files, as the cache keeps identical ones once
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\""), file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\""), file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let empty = sandbox.folder("empty-cache")?;
    let output = sandbox
        .command_in(&fresh, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", &empty)
        .env("FETCHLOOM_TEST_FREE", "1500")
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "this sync needs at least 2.0 KiB on the disk holding the cache at {}, which has 1.5 KiB free",
            empty.display()
        )
    );
    assert_eq!(document["work"]["bytes_down"], 0);
    assert!(!fresh.join("data").join("one").exists());
    Ok(())
}

/// `--yes` still checks the room, and says so when it may run short, but goes on: free space is
/// only an estimate on network disks and under quotas.
#[test]
fn a_sync_told_yes_warns_of_too_little_room_and_goes_on() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\""), file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\""), file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let empty = sandbox.folder("empty-cache")?;
    let output = sandbox
        .command_in(&fresh, &["--json", "sync", "--yes"])
        .env("FETCHLOOM_CACHE", &empty)
        .env("FETCHLOOM_TEST_FREE", "1500")
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        crate::support::warning(&document, "space.short")["message"],
        format!(
            "this sync needs at least 2.0 KiB on the disk holding the cache at {}, which has 1.5 KiB free",
            empty.display()
        ),
        "{document}"
    );
    assert!(fresh.join("data").join("one").exists());
    Ok(())
}

/// a sync whose downloads take the cache past `cache_budget` is stopped like one short of room
/// when no one can answer, saying what frees room, and goes on with a warning under `--yes`.
/// `fetchloom cache` holds the cache's files against the budget.
#[test]
fn a_sync_past_the_cache_budget_stops_without_yes_and_warns_with_it() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\""), file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\""), file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let cache = sandbox.folder("budget-cache")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&fresh, args)
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_CACHE_BUDGET", "1.5K")
            .output()
    };
    let output = run(&["sync", "one"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);

    let output = run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{document}");
    // the cache holds one as its disk stores it, in whole blocks, and two adds its bytes
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("this sync would bring the cache to ")
            && message.ends_with(", past the 1.5 KiB cache_budget"),
        "{document}"
    );
    assert_eq!(
        error["details"],
        serde_json::json!([
            "`fetchloom cache gc` would free nothing now",
            "this project's largest datasets are one (1.0 KiB) and two (1.0 KiB)"
        ])
    );
    assert_eq!(document["work"]["bytes_down"], 0);
    assert!(!fresh.join("data").join("two").exists());
    // the variable wins over the settings file, so a budget it sets is raised there
    let help = error["help"].as_str().unwrap_or_default();
    assert!(
        help.contains(", or raise FETCHLOOM_CACHE_BUDGET, then "),
        "{help}"
    );
    let set = sandbox.run(&["settings", "set", "cache_budget", "1.5K"])?;
    assert_eq!(set.status.code(), Some(0), "{}", text(&set.stderr)?);
    let output = sandbox
        .command_in(&fresh, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    let document = json(&output)?;
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(
        help.contains(", or raise the budget with `fetchloom settings set cache_budget <size>`"),
        "{document}"
    );

    let output = run(&["--json", "sync", "--yes"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let warned = crate::support::warning(&document, "cache.over_budget")["message"].to_string();
    assert!(
        warned.contains("this sync brings the cache to ")
            && warned.contains(", past the 1.5 KiB cache_budget"),
        "{document}"
    );
    assert!(fresh.join("data").join("two").exists());

    let info = json(&run(&["--json", "cache"])?)?;
    assert_eq!(info["result"]["budget"], 1536, "{info}");
    assert_eq!(info["result"]["bytes"], 2048, "{info}");
    // a disk stores each file in whole blocks, so the files take at least their bytes there
    if cfg!(unix) {
        assert!(info["result"]["on_disk"].as_u64() >= Some(2048), "{info}");
    }
    let told = text(&run(&["cache"])?.stderr)?;
    assert!(
        told.contains(" of 1.5 KiB used \u{b7} ")
            && told.contains(" past it, so a sync that downloads more asks first\n"),
        "{told}"
    );
    Ok(())
}

/// an add of one link whose size only its server states is held to `cache_budget` before the
/// download writes a byte: with no one to answer it stops and changes nothing, and --yes goes on
/// with a warning.
#[test]
fn an_add_of_one_link_is_held_to_the_cache_budget() -> Result {
    let sandbox = Sandbox::new()?;
    let big = vec![b'z'; 3000];
    let server = Server::start(
        "/files/big.bin",
        vec![file(&big, "\"z\""), file(&big, "\"z\"")],
    )?;
    sandbox.allow(&server.url);
    let add = |yes: &str| {
        let args: Vec<&str> = [yes, "--json", "add", &server.url]
            .into_iter()
            .filter(|arg| !arg.is_empty())
            .collect();
        sandbox
            .command_in(&sandbox.work(), &args)
            .env("FETCHLOOM_CACHE_BUDGET", "1K")
            .output()
    };
    let refused = add("")?;
    let document = json(&refused)?;
    assert_eq!(refused.status.code(), Some(1), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{document}");
    assert_eq!(
        error["message"],
        "downloading big.bin would bring the cache to 2.9 KiB, past the 1.0 KiB cache_budget",
        "{document}"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    let added = add("--yes")?;
    let document = json(&added)?;
    assert_eq!(added.status.code(), Some(0), "{document}");
    let warned = crate::support::warning(&document, "cache.over_budget")["message"].to_string();
    assert!(
        warned.contains(
            "downloading big.bin brings the cache to 2.9 KiB, past the 1.0 KiB cache_budget"
        ),
        "{document}"
    );
    Ok(())
}

/// the budget holds the cache's files as the disk stores them, as `fetchloom cache` counts them,
/// so a link whose size only its answer states finds no room under a budget the cache fills on
/// disk, though its files' bytes alone would leave room.
#[test]
fn a_link_is_held_to_the_budget_as_the_disk_stores_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &first)?;
    let report = json(&sandbox.run(&["--json", "cache"])?)?;
    let on_disk = report["result"]["on_disk"].as_u64().unwrap_or_default();
    if on_disk < IRIS.len() as u64 + 100 {
        return crate::support::skipped(
            "disk-blocks",
            "this disk keeps a small file in no block of its own",
        );
    }
    let second = Server::start("/files/small.bin", vec![file(&[b'z'; 100], "\"z\"")])?;
    sandbox.allow(&second.url);
    let output = sandbox
        .command_in(&sandbox.work(), &["--json", "add", &second.url])
        .env("FETCHLOOM_CACHE_BUDGET", format!("{on_disk}B"))
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(1), "{document}");
    assert_eq!(document["errors"][0]["kind"], "unconfirmed", "{document}");
    Ok(())
}

/// the budget's question names the command that would pass it. an add touches only what it adds,
/// so in a fresh clone it is the pinned two, of a size data.lock knows, that passes the budget.
#[test]
fn an_add_past_the_cache_budget_is_named_as_an_add() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let empty = sandbox.folder("empty-cache")?;
    let output = sandbox
        .command_in(&fresh, &["--json", "add", &second.url])
        .env("FETCHLOOM_CACHE", &empty)
        .env("FETCHLOOM_CACHE_BUDGET", "512B")
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(1), "{document}");
    let message = document["errors"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        message, "this add would bring the cache to 1.0 KiB, past the 512 B cache_budget",
        "{document}"
    );
    // two is what the add brings, so only one is offered for removal
    assert_eq!(
        document["errors"][0]["details"][1], "this project's largest dataset is one (1.0 KiB)",
        "{document}"
    );
    Ok(())
}

/// a sync that must ask first says what it would download in the question alone: no line says
/// the files are downloading before anyone said yes, and the line after a yes counts the same.
#[test]
fn a_download_that_needs_a_yes_is_told_only_once_it_starts() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\""), file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\""), file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let fresh = clone(&sandbox, "fresh")?;
    let empty = sandbox.folder("empty-cache")?;
    let sync = |args: &[&str]| {
        sandbox
            .command_in(&fresh, args)
            .env("FETCHLOOM_CACHE", &empty)
            .env("FETCHLOOM_ASK_ABOVE", "1K")
            .output()
    };
    let refused = sync(&["sync"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("Downloading"), "{stderr}");
    assert!(
        stderr.contains("downloading 2 files needs 2.0 KiB, and fetchloom cannot ask you here"),
        "{stderr}"
    );
    let agreed = sync(&["sync", "--yes"])?;
    let stderr = text(&agreed.stderr)?;
    assert_eq!(agreed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Downloading 2 files \u{b7} 2.0 KiB"),
        "{stderr}"
    );
    Ok(())
}

/// a sync that cannot get one dataset's files still places the datasets whose files are all on
/// this machine, of all or of those named, and its error names the one that waits; `--json` gives
/// both in its result too.
#[test]
fn a_sync_that_lacks_some_files_places_the_datasets_it_has_whole() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let hex = sha256_hex(&two);
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]),
    )?;
    for (name, args) in [
        ("fresh", &["--json", "sync", "--offline"][..]),
        ("named", &["--json", "sync", "one", "two", "--offline"]),
    ] {
        let fresh = clone(&sandbox, name)?;
        let output = sandbox.command_in(&fresh, args).output()?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(20), "{document}");
        assert_eq!(
            document["errors"][0]["kind"], "offline.missing",
            "{document}"
        );
        assert_eq!(fs::read(fresh.join("data/one/one.bin"))?, one, "{document}");
        assert!(!fresh.join("data/two").exists(), "{document}");
        let details = document["errors"][0]["details"].to_string();
        assert!(
            details.contains("placed one, whose files are all on this machine")
                && details.contains("two waits for files this run could not get"),
            "{document}"
        );
        let result = &document["result"];
        assert_eq!(result["datasets"], serde_json::json!(["one"]), "{document}");
        assert_eq!(result["waiting"], serde_json::json!(["two"]), "{document}");
        assert_eq!(result["placement"]["placed"], 1, "{document}");
    }
    Ok(())
}

/// a partial sync whose placing of the datasets the cache holds whole fails too says so beside the
/// error that stopped the run, so the second failure is not a surprise of the next run.
#[cfg(unix)]
#[test]
fn a_partial_sync_names_a_dataset_it_could_not_place_whole() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return crate::support::skipped("unprivileged", "root ignores folder permissions");
    }
    let sandbox = Sandbox::new()?;
    let (one, two) = (vec![b'x'; 1024], vec![b'y'; 1024]);
    let first = Server::start("/one.bin", vec![file(&one, "\"a\"")])?;
    let second = Server::start("/two.bin", vec![file(&two, "\"b\"")])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let hex = sha256_hex(&two);
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]),
    )?;
    let fresh = clone(&sandbox, "fresh")?;
    let refusing = fresh.join("data").join("one");
    fs::create_dir_all(&refusing)?;
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o555))?;
    let output = sandbox
        .command_in(&fresh, &["--json", "sync", "--offline"])
        .output();
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o755))?;
    let document = json(&output?)?;
    assert_eq!(
        document["errors"][0]["kind"], "offline.missing",
        "{document}"
    );
    let details = document["errors"][0]["details"].to_string();
    assert!(
        details.contains("could not place the datasets this machine has whole either")
            && details.contains("one.bin"),
        "{document}"
    );
    Ok(())
}

/// a `--json` sync of a fresh clone of the work folder's project, with an empty cache of its own.
fn synced_fresh(sandbox: &Sandbox) -> Result<(std::path::PathBuf, std::process::Output)> {
    let fresh = clone(sandbox, "fresh")?;
    let output = sandbox
        .command_in(&fresh, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("empty-cache")?)
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    Ok((fresh, output))
}

/// the datasets a partial sync places whole come from the data folder as `--verify` checked it,
/// so a copy whose bytes changed under the same size and time is put back there too.
#[test]
fn a_partial_sync_puts_back_what_verify_found_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let one = Server::start("/one.csv", vec![file(b"one\n", "\"o\"")])?;
    let two = Server::start("/two.csv", vec![file(b"two\n", "\"t\"")])?;
    for server in [&one, &two] {
        sandbox.allow(&server.url);
    }
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.one]\nref = \"{}\"\ncopy = true\n\n[datasets.two]\nref = \"{}\"\ncopy = true\n",
            one.url, two.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    let placed = sandbox.work().join("data/one/one.csv");
    let time = fs::metadata(&placed)?.modified()?;
    // fat and exfat can give the state the time of the files written just before it. a sync then
    // hashes them, and calls a change it finds an edit, which stays; a sync a tick later settles
    // them, writing the state again, after which only --verify reads them
    fs::File::options()
        .write(true)
        .open(&state)?
        .set_modified(time)?;
    sandbox.wait_for_a_later_time(time)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    fs::write(&placed, b"ONE\n")?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(time)?;
    // two waits: neither its file nor its bytes are on this machine
    delete(&sandbox.work().join("data/two/two.csv"))?;
    let hex = sha256_hex(b"two\n");
    delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]),
    )?;

    let output = sandbox.run(&["--json", "sync", "--verify", "--offline"])?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(20), "{document}");
    assert_eq!(
        document["result"]["waiting"],
        serde_json::json!(["two"]),
        "{document}"
    );
    assert_eq!(fs::read(&placed)?, b"one\n", "{document}");
    assert_eq!(
        document["result"]["placement"]["restored"],
        serde_json::json!(["data/one/one.csv"]),
        "{document}"
    );
    Ok(())
}

/// an archive no source serves now fails before the other files download, and they still do.
#[test]
fn an_archive_that_cannot_be_got_stops_no_other_download() -> Result {
    let sandbox = Sandbox::new()?;
    let zipped = zip_of(&[("a.csv", b"a\n"), ("b.csv", b"b\n")])?;
    let one = Server::start(
        "/one.csv",
        vec![file(b"one\n", "\"o\""), file(b"one\n", "\"o\"")],
    )?;
    let gone = reply("404 Not Found", &[("Content-Length", "0")], b"");
    let archive = Server::start("/arch.zip", vec![file(&zipped, "\"z\""), gone])?;
    for server in [&one, &archive] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }

    let (fresh, output) = synced_fresh(&sandbox)?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(10), "{document}");
    let placed = fresh.join("data/one/one.csv");
    assert!(placed.exists(), "{document}");
    assert_eq!(fs::read(placed)?, b"one\n", "{document}");
    let result = &document["result"];
    assert_eq!(result["datasets"], serde_json::json!(["one"]), "{document}");
    assert_eq!(result["waiting"], serde_json::json!(["arch"]), "{document}");
    Ok(())
}

/// one archive that cannot be got keeps no other archive of the same run packed: the one that
/// came is unpacked and its dataset placed, and only the missing one's dataset waits.
#[test]
fn an_archive_that_cannot_be_got_keeps_no_other_archive_packed() -> Result {
    let sandbox = Sandbox::new()?;
    let kept = zip_of(&[("a.csv", b"a\n"), ("b.csv", b"b\n")])?;
    let lost = zip_of(&[("c.csv", b"c\n"), ("d.csv", b"d\n")])?;
    let gone = reply("404 Not Found", &[("Content-Length", "0")], b"");
    let first = Server::start(
        "/first.zip",
        vec![file(&kept, "\"k\""), file(&kept, "\"k\"")],
    )?;
    let second = Server::start("/second.zip", vec![file(&lost, "\"l\""), gone])?;
    for server in [&first, &second] {
        sandbox.allow(&server.url);
        let output = sandbox.run(&["add", &server.url])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }

    let (fresh, output) = synced_fresh(&sandbox)?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(10), "{document}");
    assert_eq!(
        fs::read(fresh.join("data/first/a.csv"))?,
        b"a\n",
        "{document}"
    );
    let result = &document["result"];
    assert_eq!(
        result["datasets"],
        serde_json::json!(["first"]),
        "{document}"
    );
    assert_eq!(
        result["waiting"],
        serde_json::json!(["second"]),
        "{document}"
    );
    Ok(())
}

/// an import this machine lacks waits with its own error, and the other files still download.
#[test]
fn an_import_the_cache_lacks_stops_no_other_download() -> Result {
    let sandbox = Sandbox::new()?;
    let one = Server::start(
        "/one.csv",
        vec![file(b"one\n", "\"o\""), file(b"one\n", "\"o\"")],
    )?;
    sandbox.allow(&one.url);
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    for reference in [one.url.as_str(), "../flowers"] {
        let output = sandbox.run(&["add", reference])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }

    let (fresh, output) = synced_fresh(&sandbox)?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(10), "{document}");
    let placed = fresh.join("data/one/one.csv");
    assert!(placed.exists(), "{document}");
    assert_eq!(fs::read(placed)?, b"one\n", "{document}");
    let error = &document["errors"][0];
    assert_eq!(
        error["message"],
        "flowers was imported from files, and this machine's cache does not hold them",
        "{document}"
    );
    assert!(
        !error["help"].to_string().contains("sync one"),
        "{document}"
    );
    let result = &document["result"];
    assert_eq!(result["datasets"], serde_json::json!(["one"]), "{document}");
    assert_eq!(
        result["waiting"],
        serde_json::json!(["flowers"]),
        "{document}"
    );
    Ok(())
}

/// freeing space cannot help, so the refusal comes before the question about room.
#[test]
fn a_pinned_file_fat32_cannot_hold_is_refused_before_anything_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let fresh = clone(&sandbox, "fresh")?;
    let lock = fresh.join("data.lock");
    let text = fs::read_to_string(&lock)?;
    let pinned = format!("size = {}", IRIS.len());
    assert!(text.contains(&pinned), "{text}");
    let largest: u64 = (1 << 32) - 1;
    for (size, filesystem, kind) in [
        (largest + 1, "fat32", "local.io"),
        (largest, "fat32", "unconfirmed"),
        (largest + 1, "exfat", "unconfirmed"),
    ] {
        fs::write(&lock, text.replace(&pinned, &format!("size = {size}")))?;
        let cache = sandbox.folder(&format!("cache-{size}-{filesystem}"))?;
        let output = sandbox
            .command_in(&fresh, &["--json", "sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_TEST_FILESYSTEM", filesystem)
            .env("FETCHLOOM_TEST_FREE", "1024")
            .output()?;
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(error["kind"], kind, "{size} on {filesystem}: {error}");
        assert_eq!(document["work"]["requests"], 0, "{size} on {filesystem}");
        if kind == "local.io" {
            assert_eq!(
                error["message"],
                format!(
                    "the cache at {} is on a FAT32 disk, which holds no file of 4 GiB or more, and iris/iris.csv is 4.0 GiB",
                    cache.display()
                )
            );
        }
    }
    assert_eq!(server.requests().len(), 1, "the add alone downloaded");
    Ok(())
}

/// kaggle serves `train.csv` beside `train.csv.zip`, a zip of it. excluding data/kag/train.csv
/// would leave out the zip's copy, the wanted file, so no run may offer one.
#[cfg(debug_assertions)]
#[test]
fn an_exclude_that_left_out_a_listed_file_is_never_called_one_that_missed() -> Result {
    use crate::buckets::{s3_listing, s3_page};
    use crate::support::Routes;
    use crate::support::md5_hex;

    let sandbox = Sandbox::new()?;
    let zipped = zip_of(&[("train.csv", b"id,y\n1,0\n")])?;
    let objects: [(&str, &[u8]); 3] = [
        ("kag/test.csv", b"id\n3\n"),
        ("kag/train.csv", b"id,y\n9,9\n"),
        ("kag/train.csv.zip", &zipped),
    ];
    let base = "/kag-bucket.s3.amazonaws.com/";
    let mut routes = vec![(s3_listing(base, "kag/", None), s3_page(&objects, None))];
    for (key, body) in objects {
        let etag = format!("\"{}\"", md5_hex(body));
        routes.push((format!("{base}{key}"), file(body, &etag)));
    }
    let routes = Routes::start(routes)?;
    let run = |args: &[&str]| -> Result<(Option<i32>, String)> {
        let output = sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()?;
        let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        Ok((output.status.code(), said))
    };
    let (code, said) = run(&["add", "s3://kag-bucket/kag/"])?;
    assert_eq!(code, Some(50), "{said}");
    assert!(said.contains("--exclude \"train.csv\""), "{said}");
    let train = sandbox.work().join("data/kag/train.csv");
    let test = sandbox.work().join("data/kag/test.csv");
    let (code, said) = run(&["add", "s3://kag-bucket/kag/", "--exclude", "train.csv"])?;
    assert_eq!(code, Some(0), "{said}");
    assert!(!said.contains("does not reach"), "{said}");
    assert_eq!(fs::read(&train)?, b"id,y\n1,0\n", "the zip's train.csv");
    // a missing file makes status and sync plan, rather than find the project settled
    delete(&test)?;
    for args in [&["status"][..], &["sync"], &["status"]] {
        let (code, said) = run(args)?;
        assert_eq!(code, Some(0), "{args:?}: {said}");
        assert!(
            !said.contains("does not reach") && !said.contains("exclude data/kag/train.csv"),
            "{args:?}: {said}"
        );
    }
    assert_eq!(fs::read(&train)?, b"id,y\n1,0\n");
    assert_eq!(fs::read(&test)?, b"id\n3\n", "sync put it back");
    Ok(())
}

/// `lock --check` names the first pin, in data.lock's order, whose size is not that of its bytes:
/// past an archive's members, beside a folder whose name starts the same, and with data.lock's
/// rows swapped by hand.
#[test]
fn lock_check_names_the_first_wrong_size_in_lock_order() -> Result {
    let sandbox = Sandbox::new()?;
    for name in ["a", "a-b", "b"] {
        let folder = sandbox.folder(name)?;
        for row in 1..=3 {
            fs::write(
                folder.join(format!("f{row}.csv")),
                format!("{name} {row}\n"),
            )?;
        }
    }
    let members: Vec<(String, Vec<u8>)> = (0..40)
        .map(|member| {
            (
                format!("m{member:02}.txt"),
                format!("{member}\n").into_bytes(),
            )
        })
        .collect();
    let members: Vec<(&str, &[u8])> = members
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let folder = sandbox.folder("a")?;
    fs::write(folder.join("pack.zip"), zip_of(&members)?)?;
    fs::write(folder.join("q.csv"), "past the members\n")?;
    for name in ["a", "a-b", "b"] {
        ok(&sandbox.run(&["add", &format!("../{name}")])?)?;
    }
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let row = |dataset: &str, file: &str| -> Result<String> {
        let at = pinned
            .find(&format!("name = \"{dataset}\"\n"))
            .ok_or("no dataset")?;
        let line = pinned[at..]
            .lines()
            .find(|line| line.contains(&format!("path = \"{file}\"")))
            .ok_or("no file")?;
        Ok(line.to_owned())
    };
    let sized = |line: &str, size: usize, to: usize| {
        line.replace(&format!("size = {size},"), &format!("size = {to},"))
    };
    let check = |edited: &str| -> Result<(Option<i32>, String)> {
        fs::write(&lock, edited)?;
        let output = sandbox.run(&["lock", "--check"])?;
        Ok((output.status.code(), text(&output.stderr)?))
    };
    let (q, a_b, b) = (
        row("a", "q.csv")?,
        row("a-b", "f3.csv")?,
        row("b", "f2.csv")?,
    );
    let wrong = pinned
        .replace(&q, &sized(&q, 17, 18))
        .replace(&a_b, &sized(&a_b, 6, 5))
        .replace(&b, &sized(&b, 4, 5));
    let (code, said) = check(&wrong)?;
    assert_eq!(code, Some(50), "{said}");
    assert!(
        said.contains("data.lock pins a/q.csv at 18 bytes, but its SHA-256 is that of 17 bytes"),
        "{said}"
    );
    let (code, said) = check(&wrong.replace(&sized(&q, 17, 18), &q))?;
    assert_eq!(code, Some(50), "{said}");
    assert!(
        said.contains("data.lock pins a-b/f3.csv at 5 bytes, but its SHA-256 is that of 6 bytes"),
        "{said}"
    );
    let b1 = row("b", "f1.csv")?;
    let swapped = pinned
        .replace(&b1, "\u{0}")
        .replace(&b, &b1)
        .replace('\u{0}', &b)
        .replace(&b1, &sized(&b1, 4, 5));
    let (code, said) = check(&swapped)?;
    assert_eq!(code, Some(50), "{said}");
    assert!(
        said.contains("data.lock pins b/f1.csv at 5 bytes"),
        "{said}"
    );
    let (code, said) = check(&pinned)?;
    assert_eq!(code, Some(0), "{said}");
    Ok(())
}

/// the edited size and the old SHA-256 fit no bytes a source or mirror could send, so data.lock
/// itself is wrong rather than the files in place.
#[test]
fn a_size_the_pinned_bytes_do_not_have_stops_until_data_lock_is_restored() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let size = format!("size = {}", IRIS.len());
    assert!(pinned.contains(&size), "{pinned}");
    fs::write(
        &lock,
        pinned.replace(&size, &format!("size = {}", IRIS.len() + 1)),
    )?;
    let said = format!(
        "data.lock pins iris/iris.csv at {} bytes, but its SHA-256 is that of {} bytes on this machine",
        IRIS.len() + 1,
        IRIS.len()
    );
    for args in [
        &["--json", "status"][..],
        &["--json", "lock", "--check"],
        &["--json", "sync"],
        &["--json", "sync", "--force"],
    ] {
        let output = sandbox.run(args)?;
        let document = json(&output)?;
        let error = &document["errors"][0];
        assert_eq!(output.status.code(), Some(50), "{args:?}: {document}");
        assert_eq!(error["kind"], "project.invalid", "{args:?}");
        assert_eq!(error["message"], said.as_str(), "{args:?}");
        assert!(error["fix"].is_null(), "{args:?}: {error}");
    }
    assert_eq!(fs::read(sandbox.work().join("data/iris/iris.csv"))?, IRIS);
    fs::write(&lock, &pinned)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    Ok(())
}

/// a folder named data.toml still marks its project, so no command there suggests starting one.
#[test]
fn a_folder_named_data_toml_is_told_as_such() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("data.toml"))?;
    let sub = work.join("sub");
    fs::create_dir_all(&sub)?;
    for folder in [&work, &sub] {
        let output = sandbox.command_in(folder, &["status"]).output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.contains("data.toml is a folder, not a file"),
            "{stderr}"
        );
        assert!(!stderr.contains("fetchloom add"), "{stderr}");
    }
    let mut left: Vec<String> = fs::read_dir(&work)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    left.sort();
    assert_eq!(left, ["data.toml", "sub"]);
    Ok(())
}

/// sync carries the pin over to the name data.toml has and keeps the files, so status tells no
/// name as removed from data.toml and no kept file as one sync removes.
#[test]
fn status_forecasts_a_rename_as_sync_carries_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    fs::write(
        &lock,
        pinned.replacen("name = \"iris\"", "name = \"irisx\"", 1),
    )?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let result = &document["result"];
    assert_eq!(result["unlocked"], serde_json::json!(["iris"]), "{result}");
    assert_eq!(result["removed"], serde_json::json!([]), "{result}");
    assert_eq!(
        (&result["extra"], &result["unneeded"]),
        (&serde_json::json!([]), &serde_json::json!([])),
        "{result}"
    );
    let told = text(&sandbox.run(&["status"])?.stdout)?;
    assert_eq!(
        told,
        "   Unlocked iris · data.lock pins it as irisx, which data.toml does not have, and `fetchloom sync` keeps that pin under this name without asking the source\n"
    );
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    #[cfg(unix)]
    let inode = {
        use std::os::unix::fs::MetadataExt as _;
        fs::metadata(&placed)?.ino()
    };
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read_to_string(&lock)?, pinned);
    assert_eq!(fs::read(&placed)?, IRIS);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(fs::metadata(&placed)?.ino(), inode, "sync keeps the file");
    }
    Ok(())
}

/// committing what the suggestion names leaves nothing untracked: each dataset's folder keeps
/// itself out of git, so no .gitignore beside data.toml is named or written.
#[cfg(target_os = "linux")]
#[test]
fn the_commit_a_git_project_is_told_leaves_nothing_untracked() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    sandbox.allow(&server.url);
    let Some(screen) = sandbox.on_a_terminal(&["add", &server.url])? else {
        return Ok(());
    };
    let origin = server.url.trim_end_matches(PATH);
    assert!(
        screen.contains(&format!(
            "\n  next: commit data.toml and data.lock; anyone who clones runs `fetchloom settings set allowed_origins {origin}`, with any origins they list already, then `fetchloom sync`\n"
        )),
        "{screen}"
    );
    crate::merge::ok(&sandbox, &["add", "data.toml", "data.lock"])?;
    let left = crate::merge::git(&sandbox, &sandbox.work(), &["status", "--porcelain"])?
        .ok_or("no git")?;
    assert_eq!(text(&left.stdout)?, "A  data.lock\nA  data.toml\n");
    Ok(())
}

/// a build before a `git checkout` of an older data.lock can leave a list nothing reads.
#[test]
fn status_names_a_list_data_lock_no_longer_names() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let in_git = crate::merge::ok(&sandbox, &["init", "-q"])?.is_some();
    let added = sandbox
        .command(&["add", &server.url])
        .env("FETCHLOOM_TEST_INLINE", "0")
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let stale = "data.lock.d/iris.0123456789ab.tsv.zst";
    fs::write(sandbox.work().join(stale), b"a list of another data.lock")?;
    let unnamed = || -> Result<Vec<String>> {
        let document = json(&sandbox.run(&["--json", "status"])?)?;
        Ok(crate::support::warned(&document)
            .into_iter()
            .filter(|warning| warning.contains("no longer names"))
            .collect())
    };
    assert_eq!(
        unnamed()?,
        [format!(
            "data.lock no longer names {stale}, so nothing reads it; delete {stale}"
        )]
    );
    if in_git {
        crate::merge::ok(&sandbox, &["add", "data.lock.d"])?;
        assert_eq!(
            unnamed()?,
            [format!(
                "data.lock no longer names {stale}, so nothing reads it; run `git rm {stale}`, then commit"
            )]
        );
    }
    assert!(
        sandbox.work().join(stale).is_file(),
        "status deletes nothing"
    );
    Ok(())
}

/// data.lock keeps a dataset's title as its source wrote it, control characters and all: an
/// unrelated add writes data.lock again with the same bytes for it, and what fetchloom prints
/// shows them escaped, never raw.
#[test]
fn a_title_stays_as_written_when_data_lock_is_written_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let resolved = pinned
        .lines()
        .find(|line| line.starts_with("resolved = "))
        .ok_or("no resolved line")?;
    let title = "title = \"Iris\\u001B]0;x\\u0007\"";
    fs::write(
        &lock,
        pinned.replace(resolved, &format!("{resolved}\n{title}")),
    )?;
    fs::write(sandbox.folder("other")?.join("o.csv"), "o")?;
    let other = sandbox.run(&["add", "../other", "--as", "other"])?;
    assert_eq!(other.status.code(), Some(0), "{}", text(&other.stderr)?);
    let written = fs::read_to_string(&lock)?;
    assert!(written.contains(&format!("{title}\n")), "{written}");

    let listed = sandbox.run(&["--json", "list"])?;
    assert!(!listed.stdout.contains(&0x1b), "json keeps it escaped");
    let document = json(&listed)?;
    let iris = document["result"]["datasets"]
        .as_array()
        .and_then(|datasets| datasets.iter().find(|dataset| dataset["name"] == "iris"))
        .ok_or("no iris")?;
    assert_eq!(iris["title"], "Iris\u{1b}]0;x\u{7}");
    for args in [&["list"][..], &["list", "iris"]] {
        let shown = sandbox.run(args)?;
        for stream in [&shown.stdout, &shown.stderr] {
            assert!(
                !stream.contains(&0x1b) && !stream.contains(&0x07),
                "{args:?}"
            );
        }
    }
    Ok(())
}

/// fetchloom with `args` in `folder`, writing every source's list beside data.lock.
fn listing_in(
    sandbox: &Sandbox,
    folder: &std::path::Path,
    args: &[&str],
) -> Result<std::process::Output> {
    Ok(sandbox
        .command_in(folder, args)
        .env("FETCHLOOM_TEST_INLINE", "0")
        .output()?)
}

/// [`clone`] with every list of data.lock.d but `left_out`'s, as `git commit -a` leaves a new list
/// out. gives the clone's folder and the part it lacks.
fn clone_without(sandbox: &Sandbox, left_out: &str) -> Result<(std::path::PathBuf, String)> {
    let folder = clone(sandbox, "clone")?;
    fs::create_dir(folder.join("data.lock.d"))?;
    let mut missing = None;
    for part in parts(sandbox)? {
        if part.starts_with(&format!("data.lock.d/{left_out}.")) {
            missing = Some(part);
        } else {
            fs::copy(sandbox.work().join(&part), folder.join(&part))?;
        }
    }
    Ok((folder, missing.ok_or("no list to leave out")?))
}

/// `lock --check` in `folder` says the list `part` of `name` is not there.
fn checked_missing(sandbox: &Sandbox, folder: &std::path::Path, name: &str, part: &str) -> Result {
    let output = listing_in(sandbox, folder, &["--json", "lock", "--check"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        format!("data.lock names {part}, the file list of {name}, which is not there")
    );
    Ok(())
}

/// a clone that lacks the list of big, imported from files: big waits alone, so every command
/// that pins or places the rest works and writes data.lock as it was.
#[test]
fn a_clone_missing_an_import_list_waits_for_it_alone() -> Result {
    let sandbox = Sandbox::new()?;
    for (name, files) in [("big", &["a.csv", "b.csv"][..]), ("small", &["c.csv"])] {
        let folder = sandbox.folder(name)?;
        for each in files {
            fs::write(folder.join(each), each)?;
        }
        let added = listing_in(
            &sandbox,
            &sandbox.work(),
            &["add", &folder.to_string_lossy(), "--as", name],
        )?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, part) = clone_without(&sandbox, "big")?;
    let lock = fs::read(clone.join("data.lock"))?;
    for args in [
        &["sync"][..],
        &["sync", "--dry-run"],
        &["lock"],
        &["lock", "--dry-run"],
    ] {
        let output = listing_in(&sandbox, &clone, args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(fs::read(clone.join("data.lock"))?, lock, "{args:?}");
    }
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert!(
        stderr.contains("the file list of big, which is not there, so big waits for it"),
        "{stderr}"
    );
    assert_eq!(fs::read(clone.join("data/small/c.csv"))?, b"c.csv");
    assert!(!clone.join("data/big").exists());
    checked_missing(&sandbox, &clone, "big", &part)
}

/// a clone that lacks iris's list never asks iris's source, which serves other bytes now: iris
/// waits, and the rest syncs and locks with data.lock as it was.
#[test]
fn a_clone_missing_a_fetched_list_never_asks_its_source() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for (name, path) in [("iris", PATH), ("b", "/files/b.csv")] {
        let link = format!("{}{path}", routes.url);
        let added = listing_in(&sandbox, &sandbox.work(), &["add", &link, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    routes.change(vec![(
        PATH.to_owned(),
        file(b"sepal_length\n9.9\n", "\"v2\""),
    )]);
    let (clone, part) = clone_without(&sandbox, "iris")?;
    let lock = fs::read(clone.join("data.lock"))?;
    let asked = routes.requests().len();
    for args in [
        &["sync"][..],
        &["sync", "--dry-run"],
        &["lock"],
        &["lock", "--dry-run"],
    ] {
        let output = listing_in(&sandbox, &clone, args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(fs::read(clone.join("data.lock"))?, lock, "{args:?}");
    }
    assert_eq!(routes.requests().len(), asked, "{:?}", routes.requests());
    assert_eq!(fs::read(clone.join("data/b/b.csv"))?, b"b\n");
    assert!(!clone.join("data/iris").exists());
    // an audit asks about b's file alone, and one of iris stops with the warning's words
    let audited = listing_in(&sandbox, &clone, &["--json", "audit"])?;
    assert_eq!(audited.status.code(), Some(0), "{}", text(&audited.stderr)?);
    assert_eq!(json(&audited)?["result"]["files"], 1);
    assert_eq!(
        routes.requests().len(),
        asked + 1,
        "{:?}",
        routes.requests()
    );
    let audited = listing_in(&sandbox, &clone, &["audit"])?;
    let stderr = text(&audited.stderr)?;
    assert!(
        stderr.contains("Ok the file in data.lock is still served at its pinned size, and its source lists it as data.lock pins it, but iris waits for its file list and was not asked"),
        "{stderr}"
    );
    let audited = listing_in(&sandbox, &clone, &["--json", "audit", "iris"])?;
    assert_eq!(
        audited.status.code(),
        Some(50),
        "{}",
        text(&audited.stderr)?
    );
    assert_eq!(
        json(&audited)?["errors"][0]["message"],
        format!("data.lock names {part}, the file list of iris, which is not there")
    );
    // a ci check of every dataset fails at the list it could not audit, after auditing the rest
    let checked = listing_in(&sandbox, &clone, &["--json", "audit", "--check"])?;
    let document = json(&checked)?;
    assert_eq!(checked.status.code(), Some(50), "{document}");
    assert_eq!(
        document["errors"][0]["message"],
        format!("data.lock names {part}, the file list of iris, which is not there")
    );
    assert_eq!(document["result"]["files"], 1, "{document}");
    checked_missing(&sandbox, &clone, "iris", &part)
}

/// a data.lock field this fetchloom does not know, as a newer fetchloom may write, stops each
/// reader with words that ask for that fetchloom, never with advice to delete data.lock and pin
/// anew, which would throw its pins away.
#[test]
fn a_lock_a_newer_fetchloom_wrote_asks_for_that_fetchloom() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("flowers")?;
    fs::write(folder.join("sepals.csv"), b"length\n5.1\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let path = sandbox.work().join("data.lock");
    let lock = fs::read_to_string(&path)?.replacen("locked = ", "citation = \"x\"\nlocked = ", 1);
    fs::write(&path, &lock)?;
    let line = lock[..lock.find("citation").unwrap_or_default()]
        .matches('\n')
        .count()
        + 1;
    for command in [&["lock", "--check"][..], &["status"], &["export"]] {
        let output = sandbox.run(&[&["--json"][..], command].concat())?;
        assert_eq!(output.status.code(), Some(50), "{command:?}");
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "project.invalid", "{command:?}");
        assert_eq!(
            error["message"],
            format!(
                "data.lock holds citation on line {line}, a field this fetchloom does not know, so a newer fetchloom likely wrote it"
            ),
            "{command:?}"
        );
        assert_eq!(
            error["help"],
            "install a newer fetchloom to read it; if someone edited data.lock by hand, restore it from git",
            "{command:?}"
        );
        assert_eq!(error["fix"], serde_json::Value::Null, "{command:?}");
    }
    Ok(())
}

/// a project whose data.toml asks for a newer fetchloom in `required_version` stops every
/// command that reads it with both versions and how to update, before any other key is read and
/// with nothing written; a version in its range works as before.
#[test]
fn a_project_needing_a_newer_fetchloom_stops_every_command() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("flowers")?;
    fs::write(folder.join("sepals.csv"), b"length\n5.1\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let path = sandbox.work().join("data.toml");
    let manifest = fs::read_to_string(&path)?;
    let running = env!("CARGO_PKG_VERSION");
    fs::write(
        &path,
        format!("required_version = \">={running}\"\n{manifest}"),
    )?;
    for command in [&["status", "--check"][..], &["sync"], &["lock", "--check"]] {
        let output = sandbox.run(command)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{command:?}: {}",
            text(&output.stderr)?
        );
    }
    let needing = format!(
        "required_version = \">=999.0\"\n{manifest}citation = \"a key a newer fetchloom reads\"\n"
    );
    fs::write(&path, &needing)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy();
    for command in [
        &["status"][..],
        &["sync"],
        &["lock"],
        &["list"],
        &["path", "flowers"],
        &["add", "../flowers", "--as", "petals"],
        &["remove", "flowers"],
        &["exclude", "data/flowers/sepals.csv"],
        &["drop", "flowers"],
        &["export"],
        &["push", "--to", &mirror],
    ] {
        let output = sandbox.run(&[&["--json"][..], command].concat())?;
        assert_eq!(output.status.code(), Some(50), "{command:?}");
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "project.invalid", "{command:?}");
        assert_eq!(
            error["message"],
            format!("this project needs fetchloom >=999.0, and this one is {running}"),
            "{command:?}"
        );
        let help = error["help"].as_str().unwrap_or_default();
        assert!(
            help.starts_with(
                "update fetchloom to a version that matches >=999.0, as https://nikhil-yn-aix.github.io/fetchloom/user/start/install/ shows, then run `fetchloom --json "
            ),
            "{command:?}: {help}"
        );
        assert_eq!(fs::read_to_string(&path)?, needing, "{command:?}");
        assert_eq!(
            fs::read(sandbox.work().join("data.lock"))?,
            lock,
            "{command:?}"
        );
    }
    let output = sandbox.run(&["status"])?;
    assert_eq!(
        text(&output.stderr)?,
        format!(
            "error: this project needs fetchloom >=999.0, and this one is {running}\n  data.toml:1   required_version = \">=999.0\"\n  help: update fetchloom to a version that matches >=999.0, as https://nikhil-yn-aix.github.io/fetchloom/user/start/install/ shows, then run `fetchloom status` again\n"
        )
    );
    Ok(())
}

/// big, imported from files, and small, each with its list in data.lock.d, then a clone that
/// lacks big's list: gives the clone's folder and the part it lacks.
fn clone_without_big(sandbox: &Sandbox) -> Result<(std::path::PathBuf, String)> {
    for (name, files) in [("big", &["a.csv", "b.csv"][..]), ("small", &["c.csv"])] {
        let folder = sandbox.folder(name)?;
        for each in files {
            fs::write(folder.join(each), each)?;
        }
        let added = listing_in(
            sandbox,
            &sandbox.work(),
            &["add", &folder.to_string_lossy(), "--as", name],
        )?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    clone_without(sandbox, "big")
}

/// the git command that brings back a list deleted in this clone, told to a run given -C from
/// another folder, names the clone, as every command a help names does after -C.
#[test]
fn a_missing_list_names_the_clone_in_its_git_command() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, _) = clone_without_big(&sandbox)?;
    let clone_folder = clone.display().to_string();
    let checked = listing_in(
        &sandbox,
        &sandbox.work(),
        &["--json", "-C", &clone_folder, "lock", "--check"],
    )?;
    let document = json(&checked)?;
    assert_eq!(checked.status.code(), Some(50), "{document}");
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(
        help.contains("`git -C ") && help.ends_with(" checkout -- data.lock.d`"),
        "{help}"
    );
    Ok(())
}

/// a sync while big waits leaves the project unsettled, so once big's list is back with data.lock
/// as it was, as pulling the commit that adds data.lock.d brings it, status finds big missing and
/// the next sync places it.
#[test]
fn a_list_that_comes_back_is_placed_by_the_next_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, part) = clone_without_big(&sandbox)?;
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let lock = fs::read(clone.join("data.lock"))?;
    fs::copy(sandbox.work().join(&part), clone.join(&part))?;
    let checked = listing_in(&sandbox, &clone, &["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked.stdout)?);
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(clone.join("data/big/a.csv"))?,
        b"a.csv",
        "{stderr}"
    );
    assert_eq!(fs::read(clone.join("data.lock"))?, lock);
    let checked = listing_in(&sandbox, &clone, &["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stdout)?);
    Ok(())
}

/// in a project whose list of big is gone, plain `export`, and `remove` or `exclude` of another
/// dataset work, export telling that big waits; `export big` and an exclude inside big's folder
/// say that its list is not there.
#[test]
fn commands_of_other_datasets_go_on_while_a_list_is_missing() -> Result {
    let sandbox = Sandbox::new()?;
    let (_, part) = clone_without_big(&sandbox)?;
    let pair = sandbox.folder("pair")?;
    for name in ["d.csv", "e.csv"] {
        fs::write(pair.join(name), name)?;
    }
    let pair = pair.to_string_lossy();
    let added = listing_in(&sandbox, &sandbox.work(), &["add", &pair, "--as", "pair"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    fs::remove_file(sandbox.work().join(&part))?;
    let exported = listing_in(&sandbox, &sandbox.work(), &["export"])?;
    let stderr = text(&exported.stderr)?;
    assert_eq!(exported.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("so big waits for it"), "{stderr}");
    let stdout = text(&exported.stdout)?;
    assert!(
        stdout.contains("data/small/c.csv") && !stdout.contains("data/big/"),
        "{stdout}"
    );
    for args in [
        &["remove", "small", "--dry-run"][..],
        &["exclude", "data/pair/d.csv", "--dry-run"],
    ] {
        let output = listing_in(&sandbox, &sandbox.work(), args)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
    }
    let said = format!("data.lock names {part}, the file list of big, which is not there");
    for args in [
        &["--json", "export", "big"][..],
        &["--json", "exclude", "data/big/a.csv", "--dry-run"],
    ] {
        let output = listing_in(&sandbox, &sandbox.work(), args)?;
        assert_eq!(output.status.code(), Some(50), "{args:?}");
        assert_eq!(json(&output)?["errors"][0]["message"], said, "{args:?}");
    }
    Ok(())
}

/// a waiting dataset whose table data.toml no longer has, deleted, renamed or changed, is no pin
/// data.toml asks for: `lock` takes its entry out and says so, pins a new or changed table from
/// its source, and `lock --check` passes after it, rather than `lock` saying all is up to date
/// while `--check` fails.
#[test]
fn a_waiting_dataset_data_toml_no_longer_asks_for_goes() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for (name, path) in [("iris", PATH), ("b", "/files/b.csv")] {
        let link = format!("{}{path}", routes.url);
        let added = listing_in(&sandbox, &sandbox.work(), &["add", &link, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, _) = clone_without(&sandbox, "iris")?;
    // what iris serves now gives its list another name than the one the clone lacks
    routes.change(vec![
        (PATH.to_owned(), file(b"sepal_length\n9.9\n", "\"v2\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ]);
    let manifest = clone.join("data.toml");
    let text_of = fs::read_to_string(&manifest)?;
    let table = format!("[datasets.iris]\nref = \"{}{PATH}\"\n", routes.url);
    assert!(text_of.contains(&table), "{text_of}");
    let lock = fs::read(clone.join("data.lock"))?;
    let selected = text_of.replace(&table, &format!("{table}select = \"*.csv\"\n"));
    for (edited, would, said, pinned) in [
        (
            text_of.replace(&table, ""),
            "Would remove iris from data.lock",
            "Removed iris from data.lock",
            None,
        ),
        (
            text_of.replace("[datasets.iris]", "[datasets.flowers]"),
            "Would remove iris from data.lock",
            "Removed iris from data.lock",
            Some("flowers"),
        ),
        (selected, "Would pin iris", "Pinned iris", Some("iris")),
    ] {
        fs::write(clone.join("data.lock"), &lock)?;
        fs::write(&manifest, &edited)?;
        let dry = listing_in(&sandbox, &clone, &["lock", "--dry-run"])?;
        let stderr = text(&dry.stderr)?;
        assert_eq!(dry.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(would), "{stderr}");
        let locked = listing_in(&sandbox, &clone, &["lock"])?;
        let stderr = text(&locked.stderr)?;
        assert_eq!(locked.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains(said), "{stderr}");
        let written = fs::read_to_string(clone.join("data.lock"))?;
        match pinned {
            Some(name) => assert!(written.contains(&format!("name = \"{name}\"")), "{written}"),
            None => assert!(!written.contains("name = \"iris\""), "{written}"),
        }
        let checked = listing_in(&sandbox, &clone, &["lock", "--check"])?;
        assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    }
    Ok(())
}

/// a sync that cannot get one dataset's files while big waits for its list places small alone:
/// its error never counts big as placed, and it warns that big waits for its list.
#[test]
fn a_partial_sync_never_counts_a_waiting_dataset_as_placed() -> Result {
    let sandbox = Sandbox::new()?;
    let two = vec![b'y'; 1024];
    let server = Server::start("/two.bin", vec![file(&two, "\"b\"")])?;
    sandbox.allow(&server.url);
    let added = listing_in(&sandbox, &sandbox.work(), &["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let (clone, _) = clone_without_big(&sandbox)?;
    delete(&crate::support::object(&sandbox.cache(), &two))?;
    let output = listing_in(&sandbox, &clone, &["--json", "sync", "--offline"])?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(20), "{document}");
    let details = document["errors"][0]["details"].to_string();
    assert!(
        details.contains("placed small, whose files are all on this machine")
            && details.contains("two waits for files this run could not get"),
        "{document}"
    );
    let result = &document["result"];
    assert_eq!(
        result["datasets"],
        serde_json::json!(["small"]),
        "{document}"
    );
    assert_eq!(result["waiting"], serde_json::json!(["two"]), "{document}");
    assert!(
        crate::support::warned(&document)
            .iter()
            .any(|warning| warning
                .contains("the file list of big, which is not there, so big waits for it")),
        "{document}"
    );
    assert_eq!(fs::read(clone.join("data/small/c.csv"))?, b"c.csv");
    assert!(!clone.join("data/big").exists());
    Ok(())
}

/// a sync's closing line counts only the datasets it placed and names big, which waits for its
/// file list, and the next sync never calls every pinned file in place.
#[test]
fn a_sync_names_the_dataset_that_waits_in_its_closing_line() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, _) = clone_without_big(&sandbox)?;
    let first = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&first.stderr)?;
    assert_eq!(first.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Synced 1 dataset into data · big waits for its file list\n"),
        "{stderr}"
    );
    let again = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "Ok all pinned files but big's are in place by size and time, nothing to do\n"
        ),
        "{stderr}"
    );
    Ok(())
}

/// `list big` while big waits for its list says so, as status does, and never that data.lock has
/// no pin of it or that a sync would lock it; `--json` names the part it waits for.
#[test]
fn list_of_a_waiting_dataset_says_it_waits() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, part) = clone_without_big(&sandbox)?;
    let listed = listing_in(&sandbox, &clone, &["list", "big"])?;
    let shown = format!("{}{}", text(&listed.stdout)?, text(&listed.stderr)?);
    assert_eq!(listed.status.code(), Some(0), "{shown}");
    assert!(
        shown.contains(&format!("Waiting big · its file list {part} is not there")),
        "{shown}"
    );
    assert!(
        !shown.contains("Unlocked") && !shown.contains("to lock it"),
        "{shown}"
    );
    let listed = listing_in(&sandbox, &clone, &["--json", "list", "big"])?;
    let document = json(&listed)?;
    assert_eq!(
        document["result"]["datasets"][0]["waiting"],
        part.as_str(),
        "{document}"
    );
    let listed = listing_in(&sandbox, &clone, &["--json", "list", "small"])?;
    let document = json(&listed)?;
    assert!(
        document["result"]["datasets"][0].get("waiting").is_none(),
        "{document}"
    );
    Ok(())
}

/// `add <folder>` of the files of big, an import that waits for its list, supplies big as `add
/// <folder> --as big` does: its own files pin it again to the same list, which comes back, and
/// other files are refused with the list it waits for, never told that big pins other files with
/// help to remove it.
#[test]
fn adding_the_folder_of_a_waiting_import_supplies_it() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, part) = clone_without_big(&sandbox)?;
    let lock = fs::read_to_string(clone.join("data.lock"))?;
    let other = sandbox.folder("elsewhere")?.join("big");
    fs::create_dir(&other)?;
    fs::write(other.join("a.csv"), "other bytes")?;
    let own = sandbox.folder("big")?;
    for (folder, code) in [(&other, 50), (&own, 0)] {
        let folder = folder.to_string_lossy();
        for args in [
            &["--json", "add", &*folder][..],
            &["--json", "add", &*folder, "--as", "big"],
        ] {
            delete(&clone.join(&part)).ok();
            let added = listing_in(&sandbox, &clone, args)?;
            let document = json(&added)?;
            assert_eq!(added.status.code(), Some(code), "{args:?}: {document}");
            // a pin made again takes the time it was made
            let pins = |lock: &str| -> String {
                lock.lines()
                    .filter(|line| !line.starts_with("locked = "))
                    .collect()
            };
            let written = fs::read_to_string(clone.join("data.lock"))?;
            assert_eq!(pins(&written), pins(&lock), "{args:?}");
            if code == 0 {
                assert!(clone.join(&part).is_file(), "{args:?}: {document}");
                continue;
            }
            assert_eq!(written, lock, "{args:?}");
            let error = &document["errors"][0];
            assert_eq!(error["kind"], "project.invalid", "{args:?}: {document}");
            assert_eq!(
                error["message"],
                format!("data.lock names {part}, the file list of big, which is not there"),
                "{args:?}"
            );
        }
    }
    Ok(())
}

/// `add <link>` of iris, which data.toml has and which waits for its list, stops as `sync iris`
/// does, naming the list it waits for, never saying its archives hold no files.
#[test]
fn adding_the_link_of_a_waiting_dataset_says_it_waits() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ])?;
    sandbox.allow(&routes.url);
    let link = format!("{}{PATH}", routes.url);
    for (name, link) in [
        ("iris", link.clone()),
        ("b", format!("{}/files/b.csv", routes.url)),
    ] {
        let added = listing_in(&sandbox, &sandbox.work(), &["add", &link, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, part) = clone_without(&sandbox, "iris")?;
    let lock = fs::read(clone.join("data.lock"))?;
    for args in [
        &["--json", "add", &*link][..],
        &["--json", "add", &*link, "--as", "iris"],
    ] {
        let added = listing_in(&sandbox, &clone, args)?;
        let document = json(&added)?;
        assert_eq!(added.status.code(), Some(50), "{args:?}: {document}");
        assert_eq!(
            document["errors"][0]["message"],
            format!("data.lock names {part}, the file list of iris, which is not there"),
            "{args:?}"
        );
        assert_eq!(fs::read(clone.join("data.lock"))?, lock, "{args:?}");
    }
    Ok(())
}

/// `lock --upgrade` while iris waits for its list never says every pin is up to date: iris was
/// not asked, and its closing line says so.
#[test]
fn an_upgrade_names_a_waiting_dataset_it_did_not_ask() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for (name, path) in [("iris", PATH), ("b", "/files/b.csv")] {
        let link = format!("{}{path}", routes.url);
        let added = listing_in(&sandbox, &sandbox.work(), &["add", &link, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, _) = clone_without(&sandbox, "iris")?;
    let lock = fs::read(clone.join("data.lock"))?;
    let upgraded = listing_in(&sandbox, &clone, &["lock", "--upgrade"])?;
    let stderr = text(&upgraded.stderr)?;
    assert_eq!(upgraded.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "Ok data.lock is already up to date, but iris waits for its file list and was not asked"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("nothing to do"), "{stderr}");
    assert_eq!(fs::read(clone.join("data.lock"))?, lock);
    Ok(())
}

/// big, waiting for its list, taken out of data.toml by hand: status says the next sync takes it
/// out of data.lock, as for any dataset data.toml no longer has, and that sync does, so no later
/// run warns that it waits.
#[test]
fn a_waiting_dataset_taken_out_of_data_toml_goes_at_the_next_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, _) = clone_without_big(&sandbox)?;
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let manifest = clone.join("data.toml");
    let kept: String = fs::read_to_string(&manifest)?
        .split_inclusive("\n\n")
        .filter(|table| !table.contains("[datasets.big]"))
        .collect();
    assert!(kept.contains("[datasets.small]"), "{kept}");
    fs::write(&manifest, kept)?;
    let status = listing_in(&sandbox, &clone, &["--json", "status"])?;
    let document = json(&status)?;
    assert_eq!(
        document["result"]["removed"],
        serde_json::json!(["big"]),
        "{document}"
    );
    let status = listing_in(&sandbox, &clone, &["status"])?;
    let shown = text(&status.stdout)?;
    assert!(
        shown.contains(
            "Removed big · not in data.toml, so `fetchloom sync` takes it out of data.lock"
        ),
        "{shown}"
    );
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Removed big from data.lock"), "{stderr}");
    assert!(!fs::read_to_string(clone.join("data.lock"))?.contains("name = \"big\""));
    let again = listing_in(&sandbox, &clone, &["sync"])?;
    let stderr = text(&again.stderr)?;
    assert!(!stderr.contains("waits"), "{stderr}");
    let checked = listing_in(&sandbox, &clone, &["status", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stdout)?);
    Ok(())
}

/// `push` of every dataset while big waits for its list pushes the rest and warns that big waits,
/// as sync places the rest; `push big` stops at its missing list.
#[test]
fn a_push_of_every_dataset_leaves_a_waiting_one_out() -> Result {
    let sandbox = Sandbox::new()?;
    let (clone, part) = clone_without_big(&sandbox)?;
    let folder = sandbox.folder("mirror")?;
    let mirror = folder.to_string_lossy();
    for args in [
        &["--json", "push", "--to", &*mirror, "--dry-run"][..],
        &["--json", "push", "--to", &*mirror, "--yes"],
    ] {
        let pushed = listing_in(&sandbox, &clone, args)?;
        let document = json(&pushed)?;
        assert_eq!(pushed.status.code(), Some(0), "{args:?}: {document}");
        assert!(
            crate::support::warned(&document)
                .iter()
                .any(|warning| warning.contains("so big waits for it;")),
            "{args:?}: {document}"
        );
    }
    let object = crate::support::object(&folder, b"c.csv");
    assert!(object.is_file(), "small's file is on the mirror");
    let named = listing_in(
        &sandbox,
        &clone,
        &["--json", "push", "big", "--to", &*mirror],
    )?;
    let document = json(&named)?;
    assert_eq!(named.status.code(), Some(50), "{document}");
    assert_eq!(
        document["errors"][0]["message"],
        format!("data.lock names {part}, the file list of big, which is not there")
    );
    Ok(())
}

/// `add <ref> --as <name>`, with or without `--no-sync`, while that dataset waits for its list
/// stops at that list before asking its source, here zenodo for zen, rather than resolving it and
/// meeting the check every data.lock write makes; without --as the link is resolved first, to
/// find which dataset it is, and stops there too.
#[test]
fn adding_as_a_waiting_dataset_asks_no_source() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        (
            "/zenodo.org/api/records/101".to_owned(),
            crate::audit::record(101, "cc-by-4.0")?,
        ),
        (
            "/zenodo.org/api/records/101/files/iris.csv/content".to_owned(),
            file(crate::mirrors::IRIS, "\"z1\""),
        ),
    ])?;
    sandbox.allow(&routes.url);
    let run_in = |folder: &std::path::Path, args: &[&str]| {
        sandbox
            .command_in(folder, args)
            .env("FETCHLOOM_TEST_INLINE", "0")
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()
    };
    let link = format!("{}{PATH}", routes.url);
    for (reference, name) in [(link.as_str(), "iris"), ("zenodo:101", "zen")] {
        let added = run_in(&sandbox.work(), &["add", reference, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, iris) = clone_without(&sandbox, "iris")?;
    let zen = parts(&sandbox)?
        .into_iter()
        .find(|part| part.starts_with("data.lock.d/zen."))
        .ok_or("no list of zen")?;
    delete(&clone.join(&zen))?;
    let asked = routes.requests().len();
    for (args, name, part) in [
        (
            &["--json", "add", &*link, "--as", "iris", "--no-sync"][..],
            "iris",
            &iris,
        ),
        (&["--json", "add", &*link, "--as", "iris"], "iris", &iris),
        (
            &["--json", "add", "zenodo:101", "--as", "zen", "--no-sync"],
            "zen",
            &zen,
        ),
        (&["--json", "add", "zenodo:101", "--as", "zen"], "zen", &zen),
    ] {
        let added = run_in(&clone, args)?;
        let document = json(&added)?;
        assert_eq!(added.status.code(), Some(50), "{args:?}: {document}");
        assert_eq!(
            document["errors"][0]["message"],
            format!("data.lock names {part}, the file list of {name}, which is not there"),
            "{args:?}"
        );
        assert_eq!(
            routes.requests().len(),
            asked,
            "{args:?}: {:?}",
            routes.requests()
        );
    }
    // without --as the link is resolved first, to find which dataset it is
    let added = run_in(&clone, &["--json", "add", &link, "--no-sync"])?;
    let document = json(&added)?;
    assert_eq!(added.status.code(), Some(50), "{document}");
    assert_eq!(
        document["errors"][0]["message"],
        format!("data.lock names {iris}, the file list of iris, which is not there")
    );
    Ok(())
}

/// `lock --check`, which holds nothing, reading data.lock just before a writer replaces it and
/// sweeps the list it named, looks for that list again under the new data.lock rather than
/// calling iris waiting for a list that was there a moment ago.
#[cfg(debug_assertions)]
#[test]
fn a_reader_racing_a_writer_never_sees_a_false_wait() -> Result {
    use crate::support::{Running, wait_until};
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    sandbox.allow(&routes.url);
    let link = format!("{}{PATH}", routes.url);
    let added = listing_in(&sandbox, &sandbox.work(), &["add", &link])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let before = parts(&sandbox)?;
    routes.change(vec![(
        PATH.to_owned(),
        file(b"sepal_length\n9.9\n", "\"v2\""),
    )]);
    let release = sandbox.folder("hold")?.join("release");
    let mut command = sandbox.command(&["lock", "--check"]);
    command
        .env("FETCHLOOM_TEST_INLINE", "0")
        .env("FETCHLOOM_TEST_HOLD_LOCK_READ", &release);
    let reader = Running::start(&mut command)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    wait_until("the check to hold once it read data.lock", || {
        Ok(std::path::Path::new(&held).exists())
    })?;
    let upgraded = listing_in(&sandbox, &sandbox.work(), &["lock", "--upgrade"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    let after = parts(&sandbox)?;
    assert!(
        before.iter().all(|part| !after.contains(part)),
        "{before:?} {after:?}"
    );
    fs::write(&release, b"")?;
    let checked = reader.finish()?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("data.lock matches data.toml"), "{stderr}");
    Ok(())
}

/// the cache report, once only a clone where iris waits for its list could need iris's file,
/// counts that file as unneeded, as gc does, and says why, naming iris and the clone, so
/// "unneeded" does not read as "no project will ask for it".
#[test]
fn the_cache_report_names_a_waiting_dataset_it_cannot_count() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        (PATH.to_owned(), file(IRIS, "\"v1\"")),
        ("/files/b.csv".to_owned(), file(b"b\n", "\"b1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for (name, path) in [("iris", PATH), ("b", "/files/b.csv")] {
        let link = format!("{}{path}", routes.url);
        let added = listing_in(&sandbox, &sandbox.work(), &["add", &link, "--as", name])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let (clone, _) = clone_without(&sandbox, "iris")?;
    let synced = listing_in(&sandbox, &clone, &["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let removed = listing_in(&sandbox, &sandbox.work(), &["remove", "iris"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    let report = listing_in(&sandbox, &clone, &["cache"])?;
    let stderr = text(&report.stderr)?;
    assert_eq!(report.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Unneeded"), "{stderr}");
    assert!(
        stderr.contains("waits for its file list, so the cache counts none of its files as needed and gc may free them, which a sync there downloads again once the list is back"),
        "{stderr}"
    );
    let report = listing_in(&sandbox, &clone, &["--json", "cache"])?;
    let document = json(&report)?;
    let result = &document["result"];
    let waiting = &result["waiting"];
    assert_eq!(waiting.as_array().map(Vec::len), Some(1), "{document}");
    assert_eq!(waiting[0]["dataset"], "iris", "{document}");
    let projects = result["projects"].as_array().ok_or("no projects")?;
    assert!(projects.contains(&waiting[0]["project"]), "{document}");
    Ok(())
}

/// a server that sends iris.csv at [`PATH`] as often as it is asked, added to `sandbox`'s project.
fn served(sandbox: &Sandbox) -> Result<Routes> {
    let server = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    sandbox.allow(&server.url);
    let link = format!("{}{PATH}", server.url);
    let output = sandbox.run(&["add", &link])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(server)
}

/// a clone of `sandbox`'s project in `name` whose data.toml sets `data_dir`, kept as written.
fn clone_with_data_dir(
    sandbox: &Sandbox,
    name: &str,
    data_dir: &str,
) -> Result<std::path::PathBuf> {
    let folder = clone(sandbox, name)?;
    let toml = fs::read_to_string(folder.join("data.toml"))?;
    fs::write(
        folder.join("data.toml"),
        format!("[project]\ndata_dir = \"{data_dir}\"\n\n{toml}"),
    )?;
    Ok(folder)
}

/// runs `args` in `folder` with the cache at `cache`, refused as an overlap of the data folder
/// and the cache before anything is placed: the json error, and no file of iris in `placed`.
fn refused_for_the_cache(
    sandbox: &Sandbox,
    folder: &std::path::Path,
    cache: &std::path::Path,
    args: &[&str],
    placed: &std::path::Path,
) -> Result<serde_json::Value> {
    let output = sandbox
        .command_in(folder, &[&["--json"][..], args].concat())
        .env("FETCHLOOM_CACHE", cache)
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(50), "{args:?}: {document}");
    assert_eq!(
        document["errors"][0]["kind"], "project.invalid",
        "{document}"
    );
    assert!(
        document["errors"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("cache folder")),
        "{document}"
    );
    assert!(
        !placed.join("iris").exists(),
        "{args:?} placed {}",
        placed.display()
    );
    Ok(document)
}

/// a clone's data.toml whose `data_dir` lies in the cache kept in the project, as the ci guide
/// keeps it, is refused by sync, status and add before any of them writes: a dataset's files there would
/// pass for the cache's own records, as an unpacked archive's. the names are compared as a disk
/// that ignores case compares them.
#[test]
fn a_data_folder_inside_the_cache_is_refused_before_anything_is_written() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&sandbox)?;
    for (name, data_dir) in [
        ("unpacked", ".fetchloom-cache/unpacked"),
        ("itself", ".fetchloom-cache"),
        ("folded", ".FETCHLOOM-CACHE/unpacked"),
    ] {
        let folder = clone_with_data_dir(&sandbox, name, data_dir)?;
        let cache = folder.join(".fetchloom-cache");
        let toml = fs::read_to_string(folder.join("data.toml"))?;
        let placed = folder.join(data_dir);
        let link = format!("{}{PATH}", server.url);
        for args in [&["sync"][..], &["status"], &["add", &link, "--as", "other"]] {
            refused_for_the_cache(&sandbox, &folder, &cache, args, &placed)?;
        }
        assert_eq!(
            fs::read_to_string(folder.join("data.toml"))?,
            toml,
            "{name}"
        );
    }
    Ok(())
}

/// a cache kept in the data folder is refused too, so no dataset's folder can land in it or take
/// its place, as a dataset whose path names the cache's folder would.
#[test]
fn a_cache_inside_the_data_folder_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let _server = served(&sandbox)?;
    let folder = clone(&sandbox, "held")?;
    let data = folder.join("data");
    let document = refused_for_the_cache(&sandbox, &folder, &data.join("cache"), &["sync"], &data)?;
    let help = document["errors"][0]["help"].as_str().unwrap_or_default();
    assert!(help.contains("FETCHLOOM_CACHE"), "{help}");
    let toml = fs::read_to_string(folder.join("data.toml"))?;
    fs::write(
        folder.join("data.toml"),
        toml.replace(
            "[datasets.iris]\n",
            "[datasets.iris]\npath = \"cache/unpacked\"\n",
        ),
    )?;
    refused_for_the_cache(&sandbox, &folder, &data.join("cache"), &["sync"], &data)?;
    Ok(())
}

/// the cache named through a link in a folder above it is the same cache: a data folder inside
/// the folder the link leads to is refused as one named without the link.
#[cfg(unix)]
#[test]
fn a_cache_named_through_a_linked_folder_is_the_same_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let _server = served(&sandbox)?;
    let folder = clone_with_data_dir(&sandbox, "linked", ".fetchloom-cache/unpacked")?;
    fs::create_dir(folder.join(".fetchloom-cache"))?;
    let via = sandbox.folder("elsewhere")?.join("via");
    std::os::unix::fs::symlink(&folder, &via)?;
    let placed = folder.join(".fetchloom-cache").join("unpacked");
    refused_for_the_cache(
        &sandbox,
        &folder,
        &via.join(".fetchloom-cache"),
        &["sync"],
        &placed,
    )?;
    Ok(())
}

/// a change a test makes to the project in a folder.
type Change = fn(&std::path::Path) -> Result;

/// the one part of data.lock's file list in the project at `work`.
fn list_part(work: &std::path::Path) -> Result<std::path::PathBuf> {
    let parts: Vec<std::path::PathBuf> = fs::read_dir(work.join("data.lock.d"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(parts.len(), 1, "{parts:?}");
    Ok(parts[0].clone())
}

/// a settled project's sync, which tries the settled shortcut with data.lock's text before it
/// parses data.lock, finds every change, and ends as a sync that parsed data.lock first ends: its
/// exit code, what it tells and the files it leaves, for nothing changed, a placed file replaced
/// by the person's own or deleted, data.lock edited, its file list gone or changed, and fewer files
/// allowed than the list holds.
#[test]
fn a_settled_sync_finds_every_change_before_reading_data_lock() -> Result {
    const SEPALS: &[u8] = b"length,width\n5.1,3.5\n";
    const RED: &[u8] = b"length\n1.4\n";
    const BLUE: &[u8] = b"length\n1.9\n";
    const NOTHING: &str =
        "         Ok all pinned files are in place by size and time, nothing to do\n";
    let cases: [(&str, Change, &str, Option<i32>, &str); 7] = [
        ("nothing", |_| Ok(()), "", Some(0), NOTHING),
        (
            "an edit",
            |work| {
                let sepals = work.join("data/flowers/sepals.csv");
                delete(&sepals)?;
                Ok(fs::write(sepals, b"mine\n")?)
            },
            "",
            Some(0),
            "warning: data/flowers/sepals.csv has changed, so it stays\n  help: keep your version as a file or dataset of your own, or set copy = true for flowers in data.toml to edit its files in place; `fetchloom sync --force` discards your edit\n     Synced 1 dataset into data, 1 file left unchanged; see the warnings above\n",
        ),
        (
            "a deletion",
            |work| delete(&work.join("data/flowers/petals/red.csv")),
            "",
            Some(0),
            "    Placing 1 file\n     Placed data/flowers · 1 file\nwarning: put back data/flowers/petals/red.csv, which was deleted or moved, since data.lock pins it\n  help: there is nothing to do: each sync puts back the files data.lock pins; to leave a file out for good, run `fetchloom exclude data/flowers/petals/red.csv`\n     Synced 1 dataset into data\n",
        ),
        (
            "data.lock",
            |work| {
                let lock = fs::read_to_string(work.join("data.lock"))?;
                let edited = lock.replacen("locked = \"20", "locked = \"19", 1);
                Ok(fs::write(work.join("data.lock"), edited)?)
            },
            "",
            Some(0),
            NOTHING,
        ),
        (
            "a list gone",
            |work| delete(&list_part(work)?),
            "",
            Some(0),
            "warning: data.lock names data.lock.d/flowers.69f00dbc65e5.tsv.zst, the file list of flowers, which is not there, so flowers waits for it\n  help: where flowers was pinned, run `git add data.lock.d` and commit, then pull that commit here. a part deleted here comes back with `git checkout -- data.lock.d`\n         Ok all pinned files but flowers's are in place by size and time, nothing to do\n",
        ),
        (
            "a list changed",
            |work| {
                let part = list_part(work)?;
                delete(&part)?;
                let text = "path\tsize\tsha256\tmd5\turl\tunpacks_to\n";
                Ok(fs::write(part, zstd::encode_all(text.as_bytes(), 3)?)?)
            },
            "",
            Some(50),
            "error: the file list of flowers in data.lock.d is not the one data.lock records\n  help: restore data.lock and data.lock.d from git, or delete data.lock and run `fetchloom lock`, which resolves every dataset again and may pin newer data\n",
        ),
        (
            "fewer files allowed",
            |_| Ok(()),
            "2",
            Some(50),
            "error: data.lock lists 3 files in flowers, more than the 2 this run lists for one dataset\n  help: raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory\n",
        ),
    ];
    for (case, change, most, code, told) in cases {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("flowers")?;
        fs::create_dir_all(folder.join("petals"))?;
        fs::write(folder.join("sepals.csv"), SEPALS)?;
        fs::write(folder.join("petals/red.csv"), RED)?;
        fs::write(folder.join("petals/blue.csv"), BLUE)?;
        let added = sandbox
            .command(&["add", "../flowers"])
            .env("FETCHLOOM_TEST_INLINE", "1")
            .output()?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        let settled = sandbox.run(&["sync"])?;
        assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
        let work = sandbox.work();
        change(&work)?;
        let mut command = sandbox.command(&["sync"]);
        if !most.is_empty() {
            command.env("FETCHLOOM_MOST_FILES", most);
        }
        let output = command.output()?;
        let said =
            ways_left_out(&text(&output.stderr)?).replace(&work.display().to_string(), "<work>");
        assert_eq!(output.status.code(), code, "{case}: {said}");
        assert_eq!(said, told, "{case}");
        let placed: Vec<Vec<u8>> = ["sepals.csv", "petals/red.csv", "petals/blue.csv"]
            .iter()
            .map(|file| fs::read(work.join("data/flowers").join(file)).unwrap_or_default())
            .collect();
        let sepals: &[u8] = if case == "an edit" { b"mine\n" } else { SEPALS };
        assert_eq!(placed, [sepals, RED, BLUE], "{case}");
    }
    Ok(())
}

/// a settled project's sync with nothing changed builds none of its pins, so it answers in a
/// container whose memory would not hold them, as a status there would not; once a file changed,
/// the sync reads its pins and the container refuses it before it reads them, as before: files
/// listed in data.lock itself and in data.lock.d.
#[test]
fn a_settled_sync_answers_in_a_container_too_small_for_its_pins() -> Result {
    for inline in ["", "1"] {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("parts")?;
        for name in ["a.csv", "b.csv", "c.csv"] {
            fs::write(folder.join(name), name)?;
        }
        let mut add = sandbox.command(&["add", "../parts"]);
        if !inline.is_empty() {
            add.env("FETCHLOOM_TEST_INLINE", inline);
        }
        let added = add.output()?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        let settled = sandbox.run(&["sync"])?;
        assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
        let work = sandbox.work();
        assert_eq!(work.join("data.lock.d").exists(), !inline.is_empty());
        let small = |args: &[&str]| {
            sandbox
                .command(args)
                .env("FETCHLOOM_TEST_ROOM", (1_u64 << 20).to_string())
                .output()
        };
        let answered = small(&["sync"])?;
        let told = text(&answered.stderr)?;
        assert_eq!(answered.status.code(), Some(0), "{inline}: {told}");
        assert!(told.contains("nothing to do"), "{inline}: {told}");
        delete(&work.join("data/parts/b.csv"))?;
        let refused = small(&["--json", "sync"])?;
        let document = json(&refused)?;
        assert_eq!(refused.status.code(), Some(50), "{inline}: {document}");
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "local.io", "{inline}: {document}");
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("3 files") && message.contains("1.0 MiB this container leaves"),
            "{inline}: {message}"
        );
        assert!(!work.join("data/parts/b.csv").exists(), "{inline}");
    }
    Ok(())
}

/// `file` in the project at `work` rewritten by `edit`, and the state's record of the last
/// complete sync made to match it, as a newer fetchloom that synced the project leaves them.
fn rewritten_as_synced(
    work: &std::path::Path,
    file: &str,
    edit: impl Fn(&str) -> String,
) -> Result {
    let path = work.join(file);
    let before = fs::read_to_string(&path)?;
    let after = edit(&before);
    assert_ne!(after, before, "{file}");
    fs::write(&path, &after)?;
    let state = work.join(".fetchloom").join("state");
    let recorded = fs::read_to_string(&state)?;
    let (was, is) = (sha256_hex(before.as_bytes()), sha256_hex(after.as_bytes()));
    assert_eq!(recorded.matches(&was).count(), 1, "{recorded}");
    Ok(fs::write(&state, recorded.replace(&was, &is))?)
}

/// a project a newer fetchloom left settled, its data.lock in a newer format or its data.toml
/// asking for a newer fetchloom, is refused though the settled shortcut is tried before data.lock
/// is parsed: files listed in data.lock itself or in data.lock.d.
#[test]
fn a_newer_fetchloom_s_settled_project_is_refused() -> Result {
    let running = env!("CARGO_PKG_VERSION");
    let newer_lock = "error: data.lock is in format 2, which a newer fetchloom wrote, and this one reads format 1\n";
    let newer_toml =
        format!("error: this project needs fetchloom >=999.0, and this one is {running}\n");
    let cases = [
        ("data.lock", "", newer_lock.to_owned()),
        ("data.lock", "1", newer_lock.to_owned()),
        ("data.toml", "", newer_toml),
    ];
    for (file, inline, refusal) in cases {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("flowers")?;
        fs::write(folder.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
        fs::write(folder.join("petals.csv"), b"length\n1.4\n")?;
        let mut add = sandbox.command(&["add", "../flowers"]);
        if !inline.is_empty() {
            add.env("FETCHLOOM_TEST_INLINE", inline);
        }
        let added = add.output()?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        let settled = sandbox.run(&["sync"])?;
        assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
        let work = sandbox.work();
        assert_eq!(work.join("data.lock.d").exists(), !inline.is_empty());
        rewritten_as_synced(&work, file, |text| match file {
            "data.lock" => text.replacen("version = 1\n", "version = 2\n", 1),
            _ => format!("required_version = \">=999.0\"\n{text}"),
        })?;
        let output = sandbox.run(&["sync"])?;
        let told = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{file} {inline}: {told}");
        assert!(told.starts_with(&refusal), "{file} {inline}: {told}");
    }
    Ok(())
}
