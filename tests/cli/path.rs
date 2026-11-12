//! `path` against servers on this machine: the answer for a dataset or a file in it, and the sync
//! it runs first when the files are not in place.

use std::fs;

use crate::project::{IRIS, PATH, added, clone};
use crate::support::{Result, Sandbox, Server, file, json, text};

#[test]
fn path_prints_where_a_dataset_or_file_is() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let output = sandbox.run(&["path", "iris"])?;
    assert_eq!(text(&output.stdout)?, format!("{}\n", folder.display()));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
    let file = sandbox.run(&["path", "iris/iris.csv"])?;
    assert_eq!(
        text(&file.stdout)?,
        format!("{}\n", folder.join("iris.csv").display())
    );
    let document = json(&sandbox.run(&["--json", "path", "iris/iris.csv"])?)?;
    assert_eq!(
        document["result"],
        serde_json::json!({ "path": folder.join("iris.csv") })
    );
    let typo = sandbox.run(&["path", "irs"])?;
    assert_eq!(typo.status.code(), Some(2));
    assert!(
        text(&typo.stderr)?.contains("did you mean `fetchloom path iris`?"),
        "{}",
        text(&typo.stderr)?
    );
    let swapped = sandbox.run(&["path", "irsi"])?;
    assert!(
        text(&swapped.stderr)?.contains("did you mean `fetchloom path iris`?"),
        "{}",
        text(&swapped.stderr)?
    );
    for (typed, meant) in [
        (
            &["path", "-q", "irs/iris.csv"][..],
            "did you mean `fetchloom path -q iris/iris.csv`?",
        ),
        (
            &["path", "iris/iris.cvs", "--locked"],
            "did you mean `fetchloom path iris/iris.csv --locked`?",
        ),
    ] {
        let said = text(&sandbox.run(typed)?.stderr)?;
        assert!(said.contains(meant), "{typed:?}: {said}");
    }
    Ok(())
}

#[test]
fn path_answers_a_file_a_dataset_lacks_with_paths_it_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = crate::support::workspace()
        .join("tests")
        .join("fixtures")
        .join("archives")
        .join("python.zip");
    let server = Server::start("/files/dataset.zip", vec![file(&fs::read(zip)?, "\"z1\"")])?;
    added(&sandbox, &server)?;
    let typo = sandbox.run(&["path", "dataset/data/trian.csv"])?;
    assert_eq!(typo.status.code(), Some(2));
    assert!(
        text(&typo.stderr)?.contains("did you mean `fetchloom path dataset/data/train.csv`?"),
        "{}",
        text(&typo.stderr)?
    );
    let unknown = sandbox.run(&["path", "dataset/not-a-file.csv"])?;
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = text(&unknown.stderr)?;
    assert!(!stderr.contains("fetchloom status"), "{stderr}");
    insta::assert_snapshot!(stderr);
    Ok(())
}

#[test]
fn path_syncs_first_when_the_files_are_not_there() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let clone = clone(&sandbox, "fresh")?;
    let output = sandbox.command_in(&clone, &["path", "iris"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(clone.join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// `path --locked` never writes data.lock: a dataset added to data.toml by hand fails as
/// `sync --locked` does, before any request, while a dataset already in place still answers.
#[test]
fn a_locked_path_fails_instead_of_pinning_a_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let weather = Server::start(
        "/files/weather.csv",
        vec![file(b"day,rain\n1,0\n", "\"w1\"")],
    )?;
    let manifest = sandbox.work().join("data.toml");
    let before = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{before}\n[datasets.weather]\nref = \"{}\"\n", weather.url),
    )?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read(&lock)?;
    let refused = sandbox.run(&["--json", "path", "weather", "--locked"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stdout)?);
    let report = json(&refused)?;
    let error = &report["errors"][0];
    assert_eq!(error["kind"], "lock.outdated");
    assert_eq!(
        error["details"],
        serde_json::json!(["weather is not in data.lock yet"])
    );
    assert_eq!(
        error["help"],
        "run `fetchloom lock` and commit data.lock, or run path without --locked"
    );
    assert_eq!(report["work"]["requests"], 0);
    assert_eq!(weather.requests().len(), 0);
    assert_eq!(fs::read(&lock)?, pinned);
    let ready = sandbox.run(&["path", "iris", "--locked"])?;
    assert_eq!(ready.status.code(), Some(0), "{}", text(&ready.stderr)?);
    assert_eq!(
        text(&ready.stdout)?,
        format!("{}\n", sandbox.work().join("data").join("iris").display())
    );
    assert_eq!(fs::read(&lock)?, pinned);
    Ok(())
}

#[test]
fn path_rejects_an_impossible_locked_member_before_syncing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    for cold in [false, true] {
        let clone = clone(&sandbox, if cold { "cold-clone" } else { "warm-clone" })?;
        let mut command =
            sandbox.command_in(&clone, &["--json", "--offline", "path", "iris/no.csv"]);
        if cold {
            command.env("FETCHLOOM_CACHE", sandbox.folder("cold-cache")?);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        let report = json(&output)?;
        assert_eq!(report["errors"][0]["kind"], "usage");
        assert_eq!(report["work"]["requests"], 0);
        assert!(
            !clone.join("data").exists(),
            "an impossible member must not place data"
        );
        assert!(
            !clone.join(".fetchloom").exists(),
            "an impossible member must not sync"
        );
    }
    Ok(())
}

#[test]
fn ready_paths_read_and_plan_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let trace = sandbox.work().join("path.trace.json");
    for target in ["iris", "iris/iris.csv"] {
        let output = sandbox
            .command(&["-vv", "--offline", "path", target])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert!(output.status.success(), "{stderr}");
        let events: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(&trace)?)?;
        for name in ["read_manifest", "read_lock", "plan", "open_cache", "scan"] {
            let calls = events
                .iter()
                .filter(|event| event["ph"] == "B" && event["name"] == name)
                .count();
            assert_eq!(calls, 1, "{target}: {name}\n{stderr}");
        }
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// `path -v` tells where the bytes really are, in one plain line: a placed file is a link to its
/// file in the cache folder, or a copy of it, and `fetchloom cache` shows that folder. without
/// `-v` the answer stays alone.
#[test]
fn verbose_path_says_where_the_bytes_are() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let line = "a placed file is a link to its file in the cache folder, or a copy of it; `fetchloom cache` shows the cache";
    let plain = sandbox.run(&["path", "iris"])?;
    assert_eq!(plain.status.code(), Some(0), "{}", text(&plain.stderr)?);
    assert!(!text(&plain.stderr)?.contains(line));
    for target in ["iris", "iris/iris.csv"] {
        let verbose = sandbox.run(&["-v", "path", target])?;
        let stderr = text(&verbose.stderr)?;
        assert_eq!(verbose.status.code(), Some(0), "{stderr}");
        assert!(
            stderr.lines().any(|told| told.trim() == line),
            "{target}: {stderr}"
        );
    }
    Ok(())
}

/// an empty target and a path on this machine are refused as typed, with how a target names a
/// dataset and a file or folder in it, while a file in a dataset still answers.
#[test]
fn a_target_that_names_no_dataset_is_refused_as_typed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    for (target, message) in [
        ("", "error: \"\" names no dataset\n"),
        (
            "/etc/passwd",
            "error: \"/etc/passwd\" is a path on this machine, not a dataset\n",
        ),
    ] {
        let output = sandbox.run(&["path", "--", target])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(stderr.starts_with(message), "{stderr}");
        assert!(
            stderr.contains("help: name a dataset, with a file or folder in it after a / if you like, as in `fetchloom path iris` or `fetchloom path iris/<file>`"),
            "{stderr}"
        );
    }
    let member = sandbox.run(&["path", "iris/iris.csv"])?;
    assert_eq!(member.status.code(), Some(0), "{}", text(&member.stderr)?);
    Ok(())
}
