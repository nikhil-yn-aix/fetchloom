//! research repositories, listed and downloaded through a server on this machine that stands in
//! for their hosts, API and files alike.
#![cfg(debug_assertions)]

use std::fs;
use std::process::Command;

use crate::auth::with_stdin;
use crate::buckets::md5;
use crate::support::{Part, Result, Routes, Sandbox, file, json, ok, reply, text};

const ROWS: &[u8] = b"site,count\na,1\nb,2\n";
const NOTES: &[u8] = b"collected in 2020\n";

/// a command whose requests to the repositories' hosts go to `routes`.
fn command(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Command {
    let mut command = sandbox.command(args);
    command.env("FETCHLOOM_TEST_SOURCES", &routes.url);
    command
}

fn fixture(name: &str) -> Result<serde_json::Value> {
    let path = crate::support::workspace()
        .join("tests/fixtures/sources")
        .join(name);
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn answer(value: &serde_json::Value) -> Result<Vec<Part>> {
    Ok(reply(
        "200 OK",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(value)?,
    ))
}

const DRYAD: &str = "/datadryad.org/api/v2";

/// dryad's answers for doi:10.5061/dryad.j1fd7, its newest version listing `(path, bytes, md5)`
/// files with the MD5 stated, and the bytes behind each file's download link in its API.
fn dryad(files: &[(&str, &[u8], &str)]) -> Result<Routes> {
    let listed: Vec<_> = files
        .iter()
        .enumerate()
        .map(|(index, (path, body, md5))| {
            let id = 101 + index;
            serde_json::json!({
                "_links": {
                    "self": {"href": format!("/api/v2/files/{id}")},
                    "stash:download": {"href": format!("/api/v2/files/{id}/download")},
                },
                "path": path,
                "size": body.len(),
                "status": "created",
                "digest": md5,
                "digestType": "md5",
            })
        })
        .collect();
    let page = serde_json::json!({
        "count": listed.len(),
        "total": listed.len(),
        "_embedded": {"stash:files": listed},
    });
    let mut routes = vec![
        (
            format!("{DRYAD}/datasets/doi:10.5061%2Fdryad.j1fd7"),
            answer(&fixture("dryad/dataset.json")?)?,
        ),
        (
            format!("{DRYAD}/versions/26724/files?page=1&per_page=100"),
            answer(&page)?,
        ),
    ];
    for (index, (_, body, md5)) in files.iter().enumerate() {
        routes.push((
            format!("{DRYAD}/files/{}/download", 101 + index),
            file(body, &format!("\"{md5}\"")),
        ));
    }
    Routes::start(routes)
}

/// a pasted dataset page is added at dryad's newest version, which data.lock pins with the MD5
/// dryad states for each file, and a teammate's empty cache gets the same bytes from the lock
/// alone.
#[test]
fn a_dryad_dataset_is_pinned_at_its_version_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (md5(ROWS), md5(NOTES));
    let routes = dryad(&[("rows.csv", ROWS, &rows), ("notes.txt", NOTES, &notes)])?;
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let output = command(&sandbox, &routes, &["add", page, "--as", "archiving"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.archiving]\nref = \"dryad:10.5061/dryad.j1fd7\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for expected in [
        "resolved = \"dryad:10.5061/dryad.j1fd7@1\"".to_owned(),
        "license = \"CC0-1.0\"".to_owned(),
        "access = \"public\"".to_owned(),
        format!("md5 = \"{rows}\", url = \"https://datadryad.org/api/v2/files/101/download\""),
    ] {
        assert!(lock.contains(&expected), "{expected}\n{lock}");
    }
    assert_eq!(fs::read(work.join("data/archiving/rows.csv"))?, ROWS);
    assert_eq!(fs::read(work.join("data/archiving/notes.txt"))?, NOTES);

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = sandbox
        .command_in(&teammate, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    ok(&synced)?;
    assert_eq!(json(&synced)?["errors"], serde_json::json!([]));
    assert_eq!(fs::read(teammate.join("data/archiving/rows.csv"))?, ROWS);
    let mut fetched = routes.targets().split_off(listed);
    fetched.sort();
    assert_eq!(
        fetched,
        [
            format!("{DRYAD}/files/101/download"),
            format!("{DRYAD}/files/102/download"),
        ],
        "the lock alone says what to download"
    );
    Ok(())
}

/// `get` takes a dryad id as `add` does, into a folder of its own.
#[test]
fn get_downloads_a_dryad_version_into_a_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let rows = md5(ROWS);
    let routes = dryad(&[("rows.csv", ROWS, &rows)])?;
    let output = command(
        &sandbox,
        &routes,
        &["get", "dryad:10.5061/dryad.j1fd7@1", "--dir", "archiving"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let folder = sandbox.work().join("archiving");
    assert_eq!(text(&output.stdout)?, format!("{}\n", folder.display()));
    assert_eq!(fs::read(folder.join("rows.csv"))?, ROWS);
    Ok(())
}

/// the MD5 dryad states checks the bytes it sends: other bytes are never placed.
#[test]
fn bytes_that_differ_from_dryads_md5_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = md5(NOTES);
    let routes = dryad(&[("rows.csv", ROWS, &stated)])?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "--json",
            "add",
            "dryad:10.5061/dryad.j1fd7",
            "--as",
            "archiving",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["kind"], "integrity.mismatch",
        "{document}"
    );
    assert!(!sandbox.work().join("data/archiving/rows.csv").exists());
    Ok(())
}

/// dryad lists datasets to anyone but sends files only to an API account. a download it refuses
/// says so, and how to get a key, rather than a bare 401.
#[test]
fn a_dryad_download_refused_without_a_key_says_how_to_get_one() -> Result {
    let sandbox = Sandbox::new()?;
    let rows = md5(ROWS);
    let routes = dryad(&[("rows.csv", ROWS, &rows)])?;
    routes.change(vec![(
        format!("{DRYAD}/files/101/download"),
        reply(
            "401 Unauthorized",
            &[("Content-Type", "application/json")],
            br#"{"error":"Unauthorized, must have current bearer token"}"#,
        ),
    )]);
    let added = ["add", "dryad:10.5061/dryad.j1fd7", "--as", "archiving"];
    let output = command(&sandbox, &routes, &added).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(
        stderr.contains("dryad lets only a signed-in API account download files"),
        "{stderr}"
    );
    assert!(stderr.contains("DRYAD_TOKEN"), "{stderr}");

    let mut json_added = vec!["--json"];
    json_added.extend(added);
    let output = command(&sandbox, &routes, &json_added).output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{document}");
    assert_eq!(
        error["help"],
        "get a key on your datadryad.org account page and sign in with `fetchloom auth login dryad`, or set DRYAD_TOKEN; a key dryad gives lasts 10 hours"
    );
    Ok(())
}

/// a dryad key is checked with dryad's test endpoint, which names the key's owner, before it is
/// saved. a key dryad refuses is not saved, and no key is ever printed.
#[test]
fn a_dryad_key_is_checked_before_it_is_saved() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        format!("{DRYAD}/test"),
        reply("401 Unauthorized", &[], b""),
    )])?;
    let login = ["auth", "login", "dryad", "--with-token", "--json"];
    let refused = with_stdin(command(&sandbox, &routes, &login), "dryad-wrong-key")?;
    assert_eq!(
        refused.status.code(),
        Some(40),
        "{}",
        text(&refused.stderr)?
    );
    assert_eq!(json(&refused)?["errors"][0]["kind"], "auth.required");

    routes.change(vec![(
        format!("{DRYAD}/test"),
        reply(
            "200 OK",
            &[("Content-Type", "application/json")],
            br#"{"message":"Welcome application owner Ada Lovelace","user_id":7}"#,
        ),
    )]);
    let accepted = with_stdin(command(&sandbox, &routes, &login), "dryad-right-key")?;
    let (stdout, stderr) = (text(&accepted.stdout)?, text(&accepted.stderr)?);
    assert_eq!(accepted.status.code(), Some(0), "{stderr}");
    assert_eq!(json(&accepted)?["result"]["user"], "Ada Lovelace");
    assert!(!stdout.contains("right-key") && !stderr.contains("right-key"));

    let status = json(&command(&sandbox, &routes, &["auth", "--json", "--offline"]).output()?)?;
    let accounts = status["result"]["accounts"]
        .as_array()
        .ok_or("no accounts")?;
    let dryad = accounts
        .iter()
        .find(|account| account["account"] == "dryad")
        .ok_or("no dryad account")?;
    assert_eq!(dryad["signed_in"], true, "{dryad}");
    assert!(!status.to_string().contains("right-key"));
    Ok(())
}
