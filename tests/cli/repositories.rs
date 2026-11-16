//! research repositories, listed and downloaded through a server on this machine that stands in
//! for their hosts, API and files alike.
#![cfg(debug_assertions)]

use std::fs;

use crate::auth::with_stdin;
use crate::support::md5_hex;
use crate::support::{
    Fetchloom, Part, Result, Routes, Sandbox, file, json, ok, reply, sha256_hex, text,
};

const ROWS: &[u8] = b"site,count\na,1\nb,2\n";
const NOTES: &[u8] = b"collected in 2020\n";

/// a command whose requests to the repositories' hosts go to `routes`.
fn command(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Fetchloom {
    let mut command = sandbox.command(args);
    command.env("FETCHLOOM_TEST_HOSTS", &routes.url);
    command
}

/// a teammate's clone of this project: data.toml and data.lock copied to a folder of its own,
/// then `args` run there against `routes`, with a cache of its own. the folder, and what ran.
fn teammate_sync(
    sandbox: &Sandbox,
    routes: &Routes,
    args: &[&str],
) -> Result<(std::path::PathBuf, std::process::Output)> {
    teammate_sync_with(sandbox, routes, args, &[])
}

/// [`teammate_sync`] with the variables `envs` set too.
fn teammate_sync_with(
    sandbox: &Sandbox,
    routes: &Routes,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(std::path::PathBuf, std::process::Output)> {
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let synced = sandbox
        .command_in(&teammate, args)
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .envs(envs.iter().copied())
        .output()?;
    Ok((teammate, synced))
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
    dryad_stating("md5", files)
}

/// [`dryad`] stating each file's `(path, bytes, digest)` digest as `kind`, as `md5` or `sha-256`.
fn dryad_stating(kind: &str, files: &[(&str, &[u8], &str)]) -> Result<Routes> {
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
                "digestType": kind,
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

/// a pasted page is pinned at dryad's newest version, with the MD5 dryad states for each file.
#[test]
fn a_dryad_dataset_is_pinned_at_its_version_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (md5_hex(ROWS), md5_hex(NOTES));
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

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["--json", "sync", "--locked"])?;
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

/// a source that lists files it does not serve fails the add, and since the other files are
/// fine and nothing moved, the help leaves the missing ones out by their exact patterns.
#[test]
fn an_add_missing_some_listed_files_offers_to_leave_them_out() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (md5_hex(ROWS), md5_hex(NOTES));
    let routes = dryad(&[
        ("rows.csv", ROWS, &rows),
        ("notes.txt", NOTES, &notes),
        ("raw/[draft] a.csv", ROWS, &rows),
        ("raw/b.csv", ROWS, &rows),
    ])?;
    let gone = || reply("404 Not Found", &[("Content-Length", "0")], b"");
    routes.change(vec![
        (format!("{DRYAD}/files/103/download"), gone()),
        (format!("{DRYAD}/files/104/download"), gone()),
    ]);
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let add = |extra: &[&str]| {
        let args = [&["--json", "add", page, "--as", "archiving"][..], extra].concat();
        command(&sandbox, &routes, &args).output()
    };
    let output = add(&[])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["help"],
        "the other files are in the cache. to add archiving without them, run the add again with --exclude \"raw/[[]draft] a.csv\" --exclude \"raw/b.csv\""
    );
    let served = file(ROWS, "\"b\"");
    routes.change(vec![(format!("{DRYAD}/files/104/download"), served)]);
    assert_eq!(
        json(&add(&[])?)?["errors"][0]["help"],
        "the other files are in the cache. to add archiving without it, run the add again with --exclude \"raw/[[]draft] a.csv\""
    );
    let output = add(&["--exclude", "raw/[[]draft] a.csv"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// `get` takes a dryad id as `add` does, into a folder of its own.
#[test]
fn get_downloads_a_dryad_version_into_a_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let rows = md5_hex(ROWS);
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

/// refused bytes are never placed.
#[test]
fn bytes_that_differ_from_dryads_md5_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = md5_hex(NOTES);
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
    let rows = md5_hex(ROWS);
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

/// dryad's test endpoint names the key's owner, a refused key is not saved, and none is printed.
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

const MENDELEY: &str = "/data.mendeley.com";

/// mendeley's answers for rscbjbr9sj at version 2, whose top folder holds `(name, bytes, sha256)`
/// files, each public download link sending on to the storage that serves the bytes.
fn mendeley(files: &[(&str, &[u8], &str)]) -> Result<Routes> {
    let api = format!("{MENDELEY}/public-api/datasets/rscbjbr9sj");
    let listed: Vec<_> = files
        .iter()
        .enumerate()
        .map(|(index, (name, body, sha256))| {
            let id = format!("00000000-0000-0000-0000-{:012}", index + 1);
            serde_json::json!({
                "filename": name,
                "id": id,
                "size": body.len(),
                "status": "COMPLETED",
                "content_details": {
                    "sha256_hash": sha256,
                    "size": body.len(),
                    "download_url": format!("https://data.mendeley.com/public-files/datasets/rscbjbr9sj/files/{id}/file_downloaded"),
                },
            })
        })
        .collect();
    let range = format!("items 0-{}/{}", files.len() - 1, files.len());
    let mut routes = vec![
        (
            format!("{api}?version=2"),
            answer(&fixture("mendeley/dataset.json")?)?,
        ),
        (format!("{api}/folders/2"), answer(&serde_json::json!([]))?),
        (
            format!("{api}/files?folder_id=root&version=2&$start=0"),
            reply(
                "200 OK",
                &[
                    ("Content-Type", "application/json"),
                    ("Content-Range", &range),
                ],
                &serde_json::to_vec(&listed)?,
            ),
        ),
    ];
    for (index, (_, body, _)) in files.iter().enumerate() {
        let id = format!("00000000-0000-0000-0000-{:012}", index + 1);
        let stored = format!("/storage/{id}");
        routes.push((
            format!("{MENDELEY}/public-files/datasets/rscbjbr9sj/files/{id}/file_downloaded"),
            reply("302 Found", &[("Location", &stored)], b""),
        ));
        routes.push((stored, file(body, &format!("\"{id}\""))));
    }
    Routes::start(routes)
}

/// a pasted version link is pinned with the SHA-256 mendeley states for each file, and each
/// download link is followed on to its storage.
#[test]
fn a_mendeley_version_is_pinned_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (sha256_hex(ROWS), sha256_hex(NOTES));
    let routes = mendeley(&[("rows.csv", ROWS, &rows), ("notes.txt", NOTES, &notes)])?;
    let page = "https://data.mendeley.com/datasets/rscbjbr9sj/2";
    let output = command(&sandbox, &routes, &["--json", "add", page, "--as", "oct"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.oct]\nref = \"mendeley:rscbjbr9sj@2\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for expected in [
        "resolved = \"mendeley:rscbjbr9sj@2\"",
        "license = \"CC BY 4.0\"",
        "doi = \"10.17632/rscbjbr9sj.2\"",
        "access = \"public\"",
        "url = \"https://data.mendeley.com/public-files/datasets/rscbjbr9sj/files/00000000-0000-0000-0000-000000000001/file_downloaded\"",
    ] {
        assert!(lock.contains(expected), "{expected}\n{lock}");
    }
    assert_eq!(fs::read(work.join("data/oct/rows.csv"))?, ROWS);

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/oct/notes.txt"))?, NOTES);
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| !target.contains("public-api")),
        "the lock alone says what to download"
    );
    Ok(())
}

/// `get` takes a mendeley id as `add` does.
#[test]
fn get_downloads_a_mendeley_version_into_a_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let rows = sha256_hex(ROWS);
    let routes = mendeley(&[("rows.csv", ROWS, &rows)])?;
    let output = command(
        &sandbox,
        &routes,
        &["get", "mendeley:rscbjbr9sj@2", "--dir", "oct"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(sandbox.work().join("oct/rows.csv"))?, ROWS);
    Ok(())
}

/// the bytes come from mendeley's storage, and refused ones are never placed.
#[test]
fn bytes_that_differ_from_mendeleys_sha256_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = sha256_hex(NOTES);
    let routes = mendeley(&[("rows.csv", ROWS, &stated)])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "mendeley:rscbjbr9sj@2", "--as", "oct"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["kind"], "integrity.mismatch",
        "{document}"
    );
    assert!(!sandbox.work().join("data/oct/rows.csv").exists());
    Ok(())
}

/// github's answer for jq's release jq-1.8.2 holding `(name, bytes, sha256)` assets with the
/// digest stated, and each asset's link on github.com sending its download on to storage.
fn github(assets: &[(&str, &[u8], &str)]) -> Result<Routes> {
    Routes::start(release("jq-1.8.2", false, assets)?)
}

/// github's answers for jq's release `tag`, the latest one when `latest`, holding `(name, bytes,
/// sha256)` assets with the digest stated, and each asset's link on github.com sending its
/// download on to storage.
fn release(
    tag: &str,
    latest: bool,
    assets: &[(&str, &[u8], &str)],
) -> Result<Vec<(String, Vec<Part>)>> {
    let listed: Vec<_> = assets
        .iter()
        .map(|(name, body, sha256)| {
            serde_json::json!({
                "name": name,
                "size": body.len(),
                "state": "uploaded",
                "digest": format!("sha256:{sha256}"),
                "browser_download_url": format!("https://github.com/jqlang/jq/releases/download/{tag}/{name}"),
            })
        })
        .collect();
    let release =
        serde_json::json!({"tag_name": tag, "name": format!("jq {tag}"), "assets": listed});
    let mut routes = vec![(
        format!("/api.github.com/repos/jqlang/jq/releases/tags/{tag}"),
        answer(&release)?,
    )];
    if latest {
        routes.push((
            "/api.github.com/repos/jqlang/jq/releases/latest".to_owned(),
            answer(&release)?,
        ));
    }
    for (name, body, _) in assets {
        let stored = format!("/storage/{tag}/{name}");
        routes.push((
            format!("/github.com/jqlang/jq/releases/download/{tag}/{name}"),
            reply("302 Found", &[("Location", &stored)], b""),
        ));
        routes.push((stored, file(body, &format!("\"{name}\""))));
    }
    Ok(routes)
}

/// a pasted release page is pinned at its tag with the SHA-256 github states for each file.
#[test]
fn a_github_release_is_pinned_at_its_tag_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (sha256_hex(ROWS), sha256_hex(NOTES));
    let routes = github(&[("rows.csv", ROWS, &rows), ("notes.txt", NOTES, &notes)])?;
    let page = "https://github.com/jqlang/jq/releases/tag/jq-1.8.2";
    let output = command(&sandbox, &routes, &["add", page, "--as", "jq"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.jq]\nref = \"github:jqlang/jq@jq-1.8.2\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://github.com/jqlang/jq/releases/download/jq-1.8.2/rows.csv\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/jq/rows.csv"))?, ROWS);

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/jq/notes.txt"))?, NOTES);
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| !target.starts_with("/api.github.com")),
        "the lock alone says what to download"
    );
    Ok(())
}

/// a version taken out of data.toml keeps the release data.lock pins, as uv keeps a locked
/// version, and only an upgrade takes the latest one.
#[test]
fn a_version_taken_out_stays_locked_until_an_upgrade() -> Result {
    let sandbox = Sandbox::new()?;
    let mut served = release("jq-1.8.2", false, &[("rows.csv", ROWS, &sha256_hex(ROWS))])?;
    served.extend(release(
        "jq-1.8.3",
        true,
        &[("rows.csv", NOTES, &sha256_hex(NOTES))],
    )?);
    let routes = Routes::start(served)?;
    let run = |args: &[&str]| command(&sandbox, &routes, args).output();
    ok(&run(&["add", "github:jqlang/jq@jq-1.8.2", "--as", "jq"])?)?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("github:jqlang/jq@jq-1.8.2", "github:jqlang/jq"),
    )?;
    let lock = sandbox.work().join("data.lock");
    let pinned = |tag: &str| -> Result<bool> {
        Ok(fs::read_to_string(&lock)?.contains(&format!("resolved = \"github:jqlang/jq@{tag}\"")))
    };

    // each command that pins data.toml anew keeps the version, and says so once
    fs::write(sandbox.folder("more")?.join("more.csv"), "more")?;
    let taken_out = (fs::read(&manifest)?, fs::read(&lock)?);
    // an add pins only what it adds and leaves jq's entry to the lock and sync after it
    for args in [
        &["add", "../more", "--as", "more"][..],
        &["lock"],
        &["sync"],
    ] {
        fs::write(&manifest, &taken_out.0)?;
        fs::write(&lock, &taken_out.1)?;
        let kept = run(args)?;
        ok(&kept)?;
        let said = text(&kept.stderr)?;
        assert!(!said.contains("Upgraded"), "{args:?}: {said}");
        assert!(
            said.contains("hint: data.toml names no version of jq, so it stays at github:jqlang/jq@jq-1.8.2, as data.lock pins it; `fetchloom lock --upgrade jq` takes the newest"),
            "{args:?}: {said}"
        );
        assert!(
            pinned("jq-1.8.2")?,
            "{args:?}: {}",
            fs::read_to_string(&lock)?
        );
    }
    ok(&run(&["lock", "--check"])?)?;

    let upgraded = run(&["lock", "--upgrade", "jq"])?;
    ok(&upgraded)?;
    let said = text(&upgraded.stderr)?;
    assert!(
        said.contains("Upgraded jq · github:jqlang/jq@jq-1.8.2 → github:jqlang/jq@jq-1.8.3"),
        "{said}"
    );
    assert!(pinned("jq-1.8.3")?, "{}", fs::read_to_string(&lock)?);

    // data.toml naming an older version moves the pin back there, which is no upgrade
    let newest = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        newest.replace("github:jqlang/jq\"", "github:jqlang/jq@jq-1.8.2\""),
    )?;
    let moved = run(&["lock", "--upgrade", "jq"])?;
    ok(&moved)?;
    let said = text(&moved.stderr)?;
    assert!(
        said.contains(
            "Moved jq · github:jqlang/jq@jq-1.8.3 → github:jqlang/jq@jq-1.8.2 · 1 file changed"
        ),
        "{said}"
    );
    assert!(!said.contains("Upgraded"), "{said}");
    Ok(())
}

/// the bytes come from github's storage, and refused ones are never placed.
#[test]
fn bytes_that_differ_from_githubs_sha256_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = sha256_hex(NOTES);
    let routes = github(&[("rows.csv", ROWS, &stated)])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "github:jqlang/jq@jq-1.8.2", "--as", "jq"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(!sandbox.work().join("data/jq/rows.csv").exists());
    Ok(())
}

/// without a key github allows 60 requests an hour. once they are used up, the error says so in
/// plain words and how a key lifts the limit; with a key, it says the key's limit is used up.
#[test]
fn githubs_rate_limit_says_plainly_what_a_key_allows() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        "/api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2".to_owned(),
        reply(
            "403 Forbidden",
            &[
                ("Content-Type", "application/json"),
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "0"),
            ],
            br#"{"message":"API rate limit exceeded for 203.0.113.7. (But here's the good news: Authenticated requests get a higher rate limit. Check out the documentation for more details.)"}"#,
        ),
    )])?;
    let added = ["add", "github:jqlang/jq@jq-1.8.2"];
    let output = command(&sandbox, &routes, &added).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    assert!(
        stderr.contains("github allows 60 requests an hour without a key, and they are used up"),
        "{stderr}"
    );
    assert!(stderr.contains("GH_TOKEN or GITHUB_TOKEN"), "{stderr}");

    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "github:jqlang/jq@jq-1.8.2"],
    )
    .env("GH_TOKEN", "ghp_test_key_that_is_never_printed")
    .output()?;
    let (stdout, stderr) = (text(&output.stdout)?, text(&output.stderr)?);
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "network.rate_limited", "{error}");
    assert_eq!(
        error["message"],
        "github's rate limit for this key is used up"
    );
    assert!(!stdout.contains("never_printed") && !stderr.contains("never_printed"));
    Ok(())
}

/// gitlab's answer for gitlab-runner's release v19.3.6 holding `(path, bytes, stored)` links, at
/// an S3 bucket or, when `stored`, in the project's packages on gitlab.com. as there, a link's
/// download address answers a program with a page warning of leaving gitlab, not a redirect.
fn gitlab(links: &[(&str, &[u8], bool)]) -> Result<Routes> {
    let release = "/gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6";
    let packages =
        "gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/packages/generic/runner/v19.3.6";
    let bucket = "gitlab-runner-downloads.s3.amazonaws.com/v19.3.6";
    let address = |path: &str, stored: bool| {
        if stored {
            format!("{packages}/{path}")
        } else {
            format!("{bucket}/{path}")
        }
    };
    let listed: Vec<_> = links
        .iter()
        .map(|(path, _, stored)| {
            serde_json::json!({
                "name": path,
                "url": format!("https://{}", address(path, *stored)),
                "direct_asset_url": format!("https://gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6/downloads/{path}"),
                "link_type": "other",
            })
        })
        .collect();
    let answer_body = serde_json::json!({
        "name": "v19.3.6",
        "tag_name": "v19.3.6",
        "assets": {"count": links.len(), "sources": [], "links": listed},
    });
    let mut routes = vec![(
        "/gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/releases/v19.3.6".to_owned(),
        answer(&answer_body)?,
    )];
    let warning = b"<div class=\"tree-holder\">\n<h2>You are being redirected away from GitLab</h2>\n</div>\n";
    for (path, body, stored) in links {
        routes.push((
            format!("{release}/downloads/{path}"),
            reply(
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8")],
                warning,
            ),
        ));
        routes.push((format!("/{}", address(path, *stored)), file(body, "\"v1\"")));
    }
    Routes::start(routes)
}

/// links keep the paths the release gives them, and each file comes from the link's own address,
/// never from the page gitlab.com shows in its place.
#[test]
fn get_downloads_a_gitlab_release_link_at_its_path() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = gitlab(&[
        ("release.sha256", NOTES, false),
        ("binaries/rows.csv", ROWS, false),
        ("binaries/notes.txt", NOTES, true),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "get",
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            "--select",
            "binaries/*",
            "--dir",
            "runner",
        ],
    )
    .env("FETCHLOOM_TEST_HOSTS", &routes.url)
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let folder = sandbox.work().join("runner");
    assert_eq!(fs::read(folder.join("binaries/rows.csv"))?, ROWS);
    assert_eq!(fs::read(folder.join("binaries/notes.txt"))?, NOTES);
    assert!(!folder.join("release.sha256").exists());
    Ok(())
}

/// a release on another gitlab server, pasted as its page, is pinned from that server's api, its
/// link named by the address it ends with, and a teammate's sync reads the lock alone.
#[test]
fn a_release_on_another_gitlab_server_pins_and_syncs() -> Result {
    let sandbox = Sandbox::new()?;
    let mut release = fixture("gitlab/dav1d-release.json")?;
    let link = "https://downloads.videolan.org/videolan/dav1d/1.5.3/dav1d-1.5.3.sha256";
    release["assets"]["links"][0]["url"] = link.into();
    release["assets"]["links"][0]["direct_asset_url"] = link.into();
    let routes = Routes::start(vec![
        (
            "/code.videolan.org/api/v4/projects/videolan%2Fdav1d/releases/1.5.3".to_owned(),
            answer(&release)?,
        ),
        (
            "/downloads.videolan.org/videolan/dav1d/1.5.3/dav1d-1.5.3.sha256".to_owned(),
            file(NOTES, "\"v1\""),
        ),
    ])?;
    let page = "https://code.videolan.org/videolan/dav1d/-/releases/1.5.3";
    let output = command(&sandbox, &routes, &["add", page, "--as", "dav1d"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains(
            "[datasets.dav1d]\nref = \"gitlab://code.videolan.org/videolan/dav1d@1.5.3\"\n"
        ),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"dav1d-1.5.3.sha256\", size = {}, sha256 = \"{}\", url = \"{link}\" }}",
            NOTES.len(),
            sha256_hex(NOTES)
        )),
        "{lock}"
    );
    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(
        fs::read(teammate.join("data/dav1d/dav1d-1.5.3.sha256"))?,
        NOTES
    );
    assert_eq!(
        routes.targets()[listed..],
        ["/downloads.videolan.org/videolan/dav1d/1.5.3/dav1d-1.5.3.sha256"],
        "the lock alone says what to download"
    );
    Ok(())
}

const ENA_API: &str = "/www.ebi.ac.uk/ena/portal/api";

/// a file a stand-in serves: its name, its bytes and the hash its source states.
type Served<'a> = (&'a str, &'a [u8], &'a str);
const ENA_FIELDS: &str = "run_accession,fastq_ftp,fastq_md5,fastq_bytes,submitted_ftp,submitted_md5,submitted_bytes,submitted_format,sra_ftp,sra_md5,sra_bytes,study_title";

/// ENA's answers for `accession`: its run count, each run's FASTQ files `(name, bytes, md5)` and
/// their bytes on ftp.sra.ebi.ac.uk. a run without files stands for one dbGaP controls.
fn ena(accession: &str, runs: &[(&str, &[Served])]) -> Result<Routes> {
    let mut routes = Vec::new();
    let rows: Vec<_> = runs
        .iter()
        .map(|(run, files)| {
            let at = |name: &str| format!("ftp.sra.ebi.ac.uk/vol1/fastq/{run}/{name}");
            let joined = |part: &dyn Fn(&Served) -> String| {
                files.iter().map(part).collect::<Vec<_>>().join(";")
            };
            let protected = files.is_empty();
            serde_json::json!({
                "run_accession": run,
                "fastq_ftp": joined(&|(name, _, _)| at(name)),
                "fastq_md5": joined(&|(_, _, md5)| (*md5).to_owned()),
                "fastq_bytes": joined(&|(_, body, _)| body.len().to_string()),
                "submitted_ftp": if protected { "Protected file(s). Go to dbGap" } else { "" },
                "submitted_format": if protected { "PROTECTED" } else { "" },
                "study_title": "a small study",
            })
        })
        .collect();
    routes.push((
        format!("{ENA_API}/filereportcount?accession={accession}&result=read_run&format=json"),
        answer(&serde_json::json!({"count": runs.len().to_string()}))?,
    ));
    routes.push((
        format!(
            "{ENA_API}/filereport?accession={accession}&result=read_run&fields={ENA_FIELDS}&format=json&limit={}",
            runs.len()
        ),
        answer(&serde_json::Value::from(rows))?,
    ));
    for (run, files) in runs {
        for (name, body, md5) in *files {
            routes.push((
                format!("/ftp.sra.ebi.ac.uk/vol1/fastq/{run}/{name}"),
                file(body, &format!("\"{md5}\"")),
            ));
        }
    }
    Routes::start(routes)
}

/// a pasted page of ENA's browser is pinned with the MD5 ENA states for each of the run's files.
#[test]
fn an_ena_run_is_pinned_with_enas_md5_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let reads = md5_hex(ROWS);
    let routes = ena(
        "ERR1160846",
        &[("ERR1160846", &[("ERR1160846.fastq.gz", ROWS, &reads)])],
    )?;
    let page = "https://www.ebi.ac.uk/ena/browser/view/ERR1160846";
    let output = command(&sandbox, &routes, &["add", page, "--as", "blank"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.blank]\nref = \"ena:ERR1160846\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "md5 = \"{reads}\", url = \"https://ftp.sra.ebi.ac.uk/vol1/fastq/ERR1160846/ERR1160846.fastq.gz\""
        )),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/blank/ERR1160846.fastq.gz"))?, ROWS);

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(
        fs::read(teammate.join("data/blank/ERR1160846.fastq.gz"))?,
        ROWS
    );
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| target.starts_with("/ftp.sra.ebi.ac.uk/")),
        "the lock alone says what to download"
    );
    Ok(())
}

/// the warning has a kind a script can match.
#[test]
fn an_ena_study_leaves_out_runs_under_controlled_access_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (md5_hex(ROWS), md5_hex(NOTES));
    let routes = ena(
        "PRJNA172563",
        &[
            ("SRR036297", &[]),
            (
                "SRR1236466",
                &[
                    ("SRR1236466_1.fastq.gz", ROWS, &one),
                    ("SRR1236466_2.fastq.gz", NOTES, &two),
                ],
            ),
        ],
    )?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "ena:PRJNA172563", "--as", "nhl"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        crate::support::warning(&document, "source.partial")["message"],
        "1 of the 2 runs is under controlled access, so its files are left out",
        "{document}"
    );
    let folder = sandbox.work().join("data/nhl/SRR1236466");
    assert_eq!(fs::read(folder.join("SRR1236466_1.fastq.gz"))?, ROWS);
    assert!(!sandbox.work().join("data/nhl/SRR036297").exists());
    Ok(())
}

#[test]
fn get_selects_one_run_of_an_ena_experiment_by_its_accession() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (md5_hex(ROWS), md5_hex(NOTES));
    let routes = ena(
        "SRX079566",
        &[
            ("SRR292241", &[("SRR292241_1.fastq.gz", ROWS, &one)]),
            ("SRR390728", &[("SRR390728_1.fastq.gz", NOTES, &two)]),
        ],
    )?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "get",
            "ena:SRX079566",
            "--select",
            "SRR390728/*",
            "--dir",
            "runs",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let folder = sandbox.work().join("runs");
    assert_eq!(
        fs::read(folder.join("SRR390728/SRR390728_1.fastq.gz"))?,
        NOTES
    );
    assert!(!folder.join("SRR292241").exists());
    Ok(())
}

/// the bytes come from ENA's file server, and refused ones are never placed.
#[test]
fn bytes_that_differ_from_enas_md5_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = md5_hex(NOTES);
    let routes = ena(
        "ERR1160846",
        &[("ERR1160846", &[("ERR1160846.fastq.gz", ROWS, &stated)])],
    )?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "ena:ERR1160846", "--as", "blank"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(
        !sandbox
            .work()
            .join("data/blank/ERR1160846.fastq.gz")
            .exists()
    );
    Ok(())
}

const GEO_SERIES: &str = "/ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000";

/// a filelist in the form GEO writes, naming no tar.
const GEO_FILELIST: &[u8] = b"#Archive/File\tName\tTime\tSize\tType\n";

/// NCBI's file server for GSE1000: its folder, and the folders in it holding `(path, bytes)`
/// files, as `matrix/GSE1000_series_matrix.txt.gz`, each index rounding sizes as NCBI's do.
fn geo(files: &[(&str, &[u8])]) -> Result<Routes> {
    let index = |folder: &str, parent: &str, entries: &[String]| -> Vec<Part> {
        let links = entries
            .iter()
            .map(|entry| format!("<a href=\"{entry}\">{entry}</a>  2026-10-05 16:24  1.3M\n"))
            .collect::<Vec<_>>()
            .concat();
        reply(
            "200 OK",
            &[("Content-Type", "text/html;charset=UTF-8")],
            format!("<html><head><title>Index of {folder}</title></head><body><pre><a href=\"{parent}\">Parent Directory</a>\n{links}</pre>").as_bytes(),
        )
    };
    let mut folders: Vec<String> = Vec::new();
    for (path, _) in files {
        let (folder, _) = path.split_once('/').ok_or("a file outside a folder")?;
        if !folders.iter().any(|known| known == folder) {
            folders.push(folder.to_owned());
        }
    }
    let mut routes = vec![(
        format!("{GEO_SERIES}/"),
        index(
            "/geo/series/GSE1nnn/GSE1000",
            "/geo/series/GSE1nnn/",
            &folders
                .iter()
                .map(|folder| format!("{folder}/"))
                .collect::<Vec<_>>(),
        ),
    )];
    for folder in &folders {
        let names: Vec<String> = files
            .iter()
            .filter_map(|(path, _)| path.strip_prefix(&format!("{folder}/")).map(str::to_owned))
            .collect();
        routes.push((
            format!("{GEO_SERIES}/{folder}/"),
            index(
                &format!("/geo/series/GSE1nnn/GSE1000/{folder}"),
                "/geo/series/GSE1nnn/GSE1000/",
                &names,
            ),
        ));
    }
    for (path, body) in files {
        routes.push((format!("{GEO_SERIES}/{path}"), file(body, "\"v1\"")));
    }
    Routes::start(routes)
}

/// a pasted page of GEO's site is pinned by each file's SHA-256 and the size its download gave,
/// not the size an index rounds.
#[test]
fn a_geo_series_is_pinned_by_its_bytes_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = geo(&[
        ("matrix/GSE1000_series_matrix.txt.gz", ROWS),
        ("suppl/filelist.txt", GEO_FILELIST),
    ])?;
    let page = "https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSE1000";
    let output = command(&sandbox, &routes, &["add", page, "--as", "gse1000"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.gse1000]\nref = \"geo:GSE1000\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"matrix/GSE1000_series_matrix.txt.gz\", size = {}, sha256 = \"{}\", url = \"https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/matrix/GSE1000_series_matrix.txt.gz\" }}",
            ROWS.len(),
            sha256_hex(ROWS)
        )),
        "{lock}"
    );
    assert_eq!(
        fs::read(work.join("data/gse1000/suppl/filelist.txt"))?,
        GEO_FILELIST
    );

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(
        fs::read(teammate.join("data/gse1000/matrix/GSE1000_series_matrix.txt.gz"))?,
        ROWS
    );
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| !target.ends_with('/')),
        "the lock alone says what to download, so no index is read"
    );
    Ok(())
}

#[test]
fn get_takes_the_folder_a_pasted_geo_link_names() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = geo(&[
        ("matrix/GSE1000_series_matrix.txt.gz", ROWS),
        ("suppl/filelist.txt", GEO_FILELIST),
    ])?;
    let folder = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/";
    let output = command(&sandbox, &routes, &["get", folder, "--dir", "gse1000"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let got = sandbox.work().join("gse1000");
    assert_eq!(fs::read(got.join("suppl/filelist.txt"))?, GEO_FILELIST);
    assert!(!got.join("matrix").exists());
    Ok(())
}

/// GEO remakes a series' files from time to time, and states no hash for them, so the SHA-256
/// the lock pinned is what tells remade bytes apart: a sync that receives them refuses them.
#[test]
fn bytes_geo_remade_under_the_same_name_are_refused_by_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = geo(&[("matrix/GSE1000_series_matrix.txt.gz", ROWS)])?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "geo:GSE1000", "--as", "gse1000"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    routes.change(vec![(
        format!("{GEO_SERIES}/matrix/GSE1000_series_matrix.txt.gz"),
        file(NOTES, "\"v2\""),
    )]);
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["--json", "sync", "--locked"])?;
    assert_eq!(synced.status.code(), Some(30), "{}", text(&synced.stderr)?);
    assert_eq!(json(&synced)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(
        !teammate
            .join("data/gse1000/matrix/GSE1000_series_matrix.txt.gz")
            .exists()
    );
    Ok(())
}

/// NCBI refuses a program the listing of its folders at times while it serves their files: a
/// series is still pinned from the names GEO gives every series and the tar its filelist names,
/// at the size the filelist states, and the add says what such a listing leaves out rather than
/// calling the series private.
#[test]
fn a_geo_series_whose_listing_ncbi_refuses_is_pinned_by_name() -> Result {
    let sandbox = Sandbox::new()?;
    let filelist = format!(
        "#Archive/File\tName\tTime\tSize\tType\nArchive\tGSE1000_RAW.tar\t01/17/2013 13:28:57\t{}\tTAR\nFile\tGSM15785.cel.gz\t01/28/2004 08:49:28\t3460434\tCEL\n",
        NOTES.len()
    );
    let routes = Routes::start(vec![
        (
            format!("{GEO_SERIES}/"),
            reply(
                "403 Forbidden",
                &[("Content-Type", "text/html")],
                b"<h1>Access forbidden</h1>",
            ),
        ),
        (
            format!("{GEO_SERIES}/matrix/GSE1000_series_matrix.txt.gz"),
            file(ROWS, "\"m1\""),
        ),
        (
            format!("{GEO_SERIES}/suppl/filelist.txt"),
            file(filelist.as_bytes(), "\"f1\""),
        ),
        (
            format!("{GEO_SERIES}/suppl/GSE1000_RAW.tar"),
            file(NOTES, "\"t1\""),
        ),
    ])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "geo:GSE1000", "--as", "gse1000"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let warnings = crate::support::warned(&json(&output)?);
    assert!(
        warnings.iter().any(|warning| warning.starts_with(
            "NCBI refused to list the folders of geo:GSE1000 for now, so it holds the files GEO names alike for every series"
        )),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().all(|warning| !warning.contains("owner")),
        "{warnings:?}"
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"suppl/GSE1000_RAW.tar\", size = {}, sha256 = \"{}\", url = \"https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/GSE1000_RAW.tar\" }}",
            NOTES.len(),
            sha256_hex(NOTES)
        )),
        "{lock}"
    );
    assert!(lock.contains("path = \"suppl/filelist.txt\""), "{lock}");
    assert!(
        !lock.contains("soft/") && !lock.contains("miniml/"),
        "names the file server does not serve are left out: {lock}"
    );
    assert_eq!(
        fs::read(
            sandbox
                .work()
                .join("data/gse1000/matrix/GSE1000_series_matrix.txt.gz")
        )?,
        ROWS
    );
    Ok(())
}

const IA_ITEM: &str = "/archive.org/metadata/iris_20240626";

/// archive.org's answer about `iris_20240626` listing `(name, bytes, md5, private)` files beside its
/// own list of files, and the bytes behind each public file's download link.
fn internet_archive(files: &[(&str, &[u8], &str, bool)]) -> Result<Routes> {
    let mut listed: Vec<serde_json::Value> = files
        .iter()
        .map(|(name, body, md5, private)| {
            let mut file = serde_json::json!({
                "name": name,
                "source": "original",
                "size": body.len().to_string(),
                "md5": md5,
                "format": "Comma-Separated Values",
            });
            if *private {
                file["private"] = "true".into();
            }
            file
        })
        .collect();
    listed.push(serde_json::json!({
        "name": "iris_20240626_files.xml",
        "source": "original",
        "format": "Metadata",
        "md5": "3b9b9a3bb15c4a09c24d8a4f1c8bad64",
        "summation": "md5",
    }));
    let mut item = fixture("ia/iris.json")?;
    item["files"] = listed.into();
    let mut routes = vec![(IA_ITEM.to_owned(), answer(&item)?)];
    for (name, body, _, private) in files {
        if !private {
            routes.push((
                format!("/archive.org/download/iris_20240626/{name}"),
                file(body, "\"v1\""),
            ));
        }
    }
    Routes::start(routes)
}

/// an item pins its public files by bytes their stated MD5 checked, leaves a file it keeps
/// private out with a reason, and a teammate's sync reads the lock alone.
#[test]
fn an_internet_archive_item_pins_md5_checked_files_and_syncs() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = internet_archive(&[
        ("iris.csv", ROWS, &md5_hex(ROWS), false),
        ("notes.txt", NOTES, &md5_hex(NOTES), true),
    ])?;
    let page = "https://archive.org/details/iris_20240626";
    let output = command(&sandbox, &routes, &["--json", "add", page, "--as", "iris"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let warnings = crate::support::warned(&json(&output)?);
    assert!(
        warnings.contains(
            &"archive.org keeps 1 file of ia:iris_20240626 private, so it is left out: notes.txt"
                .to_owned()
        ),
        "{warnings:?}"
    );
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.iris]\nref = \"ia:iris_20240626\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"iris.csv\", size = {}, sha256 = \"{}\", md5 = \"{}\", url = \"https://archive.org/download/iris_20240626/iris.csv\" }}",
            ROWS.len(),
            sha256_hex(ROWS),
            md5_hex(ROWS)
        )),
        "{lock}"
    );
    assert!(
        !lock.contains("notes.txt") && !lock.contains("_files.xml"),
        "{lock}"
    );

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/iris/iris.csv"))?, ROWS);
    assert_eq!(
        routes.targets()[listed..],
        ["/archive.org/download/iris_20240626/iris.csv"],
        "the lock alone says what to download"
    );
    Ok(())
}

/// bytes unlike the MD5 archive.org states are refused, and never placed.
#[test]
fn an_internet_archive_file_unlike_its_md5_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = internet_archive(&[("iris.csv", ROWS, &md5_hex(NOTES), false)])?;
    let output = command(
        &sandbox,
        &routes,
        &["--json", "add", "ia:iris_20240626", "--as", "iris"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(!sandbox.work().join("data/iris/iris.csv").exists());
    Ok(())
}

/// zenodo's answer for record 18149369, in zenodo's own form, holding `rows.csv` with its MD5,
/// and the bytes behind its link.
fn zenodo_record() -> Result<Routes> {
    let mut record = fixture("zenodo/cc0.json")?;
    record["files"] = serde_json::json!([{
        "key": "rows.csv",
        "size": ROWS.len(),
        "checksum": format!("md5:{}", md5_hex(ROWS)),
    }]);
    Routes::start(vec![
        (
            "/zenodo.org/api/records/18149369".to_owned(),
            answer(&record)?,
        ),
        (
            "/zenodo.org/api/records/18149369/files/rows.csv/content".to_owned(),
            file(ROWS, "\"z1\""),
        ),
    ])
}

/// zenodo answers in its own form, so its records pin as they always have: the same resolved
/// reference, license id, DOI, access and links in data.lock, CC0 as zenodo's `cc-zero`.
#[test]
fn a_zenodo_record_pins_as_it_always_has() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = zenodo_record()?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "zenodo:18149369", "--as", "dao"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?
        .lines()
        .filter(|line| !line.starts_with("locked = "))
        .fold(String::new(), |lock, line| lock + line + "\n");
    assert_eq!(
        lock,
        format!(
            "version = 1\n\n[[dataset]]\nname = \"dao\"\nref = \"zenodo:18149369\"\nspec = \"sha256:711472e85f75f358a75c8c40edeb8f54232359c3d09fe95c26c31d7cf620f8bc\"\nresolved = \"zenodo:18149369\"\ntitle = \"DAO Analyzer dataset\"\nlicense = \"cc-zero\"\ndoi = \"10.5281/zenodo.18149369\"\naccess = \"public\"\nfiles = [\n  {{ path = \"rows.csv\", size = {}, sha256 = \"{}\", md5 = \"{}\", url = \"https://zenodo.org/api/records/18149369/files/rows.csv/content\" }},\n]\n",
            ROWS.len(),
            sha256_hex(ROWS),
            md5_hex(ROWS)
        )
    );
    Ok(())
}

/// a file in a hugging face bucket, which its owner may change at any time and which states no
/// SHA-256, is a plain link pinned by the SHA-256 of its bytes, fetched from wherever hugging face
/// sends the request; a link to the bucket's own page names the form of a file's link.
#[test]
fn a_hugging_face_bucket_file_is_a_plain_link_pinned_by_its_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let resolve = "/huggingface.co/buckets/lab/plots/resolve/runs/rows.csv";
    let stored = "/cas-bridge.xethub.hf.co/xet-bridge-us/rows";
    let routes = Routes::start(vec![
        (
            resolve.to_owned(),
            reply(
                "302 Found",
                &[
                    ("Location", &format!("https:/{stored}")),
                    ("Content-Length", "0"),
                ],
                b"",
            ),
        ),
        (stored.to_owned(), file(ROWS, "\"b1\"")),
    ])?;
    let link = format!("https:/{resolve}");
    let output = command(&sandbox, &routes, &["add", &link, "--as", "rows"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&format!("resolved = \"{link}\"")), "{lock}");
    assert!(
        lock.contains(&format!(
            "{{ path = \"rows.csv\", size = {}, sha256 = \"{}\", url = \"{link}\" }}",
            ROWS.len(),
            sha256_hex(ROWS)
        )),
        "{lock}"
    );
    assert_eq!(fs::read(sandbox.work().join("data/rows/rows.csv"))?, ROWS);
    assert_eq!(routes.targets(), [resolve, stored], "no hub api is asked");

    let page = "https://huggingface.co/buckets/lab/plots";
    let output = command(&sandbox, &routes, &["--json", "add", page]).output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = json(&output)?["errors"][0].clone();
    assert_eq!(
        error["message"],
        format!(
            "{page} is a page of a hugging face bucket, and fetchloom takes a bucket's files by their own links"
        ),
        "{error}"
    );
    assert_eq!(
        error["help"],
        "paste the link of each file you want, as https://huggingface.co/buckets/lab/plots/resolve/<path>",
        "{error}"
    );
    assert_eq!(routes.targets().len(), 2, "a page is refused unasked");
    Ok(())
}

const RDM_RECORD: &str = "/rdm.example.org/api/records/abcde-12345";
const RDM_CONCEPT: &str = "/rdm.example.org/api/records/fghij-67890";

/// an invenioRDM install at rdm.example.org holding record `abcde-12345`, registered as DOI
/// `10.1234/abcde-12345`, whose one file `rows.csv` has bytes `body` and the MD5 `md5`. it is the
/// newest version of `fghij-67890`, whose page the install shows and whose answer in its api
/// redirects to it, as invenioRDM does.
fn invenio(body: &[u8], md5: &str) -> Result<Routes> {
    let mut record = fixture("invenio/caltech-record.json")?;
    record["id"] = "abcde-12345".into();
    record["parent"]["id"] = "fghij-67890".into();
    record["pids"]["doi"]["identifier"] = "10.1234/abcde-12345".into();
    record["files"] = serde_json::json!({
        "enabled": true,
        "count": 1,
        "entries": {
            "rows.csv": {"key": "rows.csv", "size": body.len(), "checksum": format!("md5:{md5}")},
        },
    });
    let handle = r#"{"responseCode":1,"handle":"10.1234/abcde-12345","values":[{"index":1,"type":"URL","data":{"format":"string","value":"https://rdm.example.org/doi/10.1234/abcde-12345"}}]}"#;
    Routes::start(vec![
        (RDM_RECORD.to_owned(), answer(&record)?),
        (
            "/rdm.example.org/records/fghij-67890".to_owned(),
            reply(
                "200 OK",
                &[("Content-Type", "text/html")],
                b"<html><title>rows</title></html>",
            ),
        ),
        (
            RDM_CONCEPT.to_owned(),
            reply(
                "302 Found",
                &[
                    ("Location", &format!("https:/{RDM_RECORD}")),
                    ("Content-Length", "0"),
                ],
                b"",
            ),
        ),
        (
            format!("{RDM_RECORD}/files/rows.csv/content"),
            file(body, "\"r1\""),
        ),
        (
            "/doi.org/api/handles/10.1234%2Fabcde-12345".to_owned(),
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                handle.as_bytes(),
            ),
        ),
    ])
}

/// a record on another invenioRDM install pins with its MD5-checked bytes, and a teammate's sync
/// reads the lock alone.
#[test]
fn a_record_on_another_invenio_install_pins_and_syncs() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = invenio(ROWS, &md5_hex(ROWS))?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "invenio:rdm.example.org/abcde-12345", "--as", "rows"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"rows.csv\", size = {}, sha256 = \"{}\", md5 = \"{}\", url = \"https://rdm.example.org/api/records/abcde-12345/files/rows.csv/content\" }}",
            ROWS.len(),
            sha256_hex(ROWS),
            md5_hex(ROWS)
        )),
        "{lock}"
    );
    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/rows/rows.csv"))?, ROWS);
    assert_eq!(
        routes.targets()[listed..],
        [format!("{RDM_RECORD}/files/rows.csv/content")],
        "the lock alone says what to download"
    );
    Ok(())
}

/// a pasted record page and a DOI the install registered are each known as the record by one
/// request for it at the install, before its file downloads.
#[test]
fn an_invenio_record_page_and_doi_are_known_by_one_request() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = invenio(ROWS, &md5_hex(ROWS))?;
    for (reference, name, before) in [
        (
            "https://rdm.example.org/records/abcde-12345",
            "page",
            vec![],
        ),
        (
            "doi:10.1234/abcde-12345",
            "doi",
            vec!["/doi.org/api/handles/10.1234%2Fabcde-12345".to_owned()],
        ),
    ] {
        let asked = routes.targets().len();
        let output = command(&sandbox, &routes, &["add", reference, "--as", name]).output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        assert!(
            lock.contains("resolved = \"invenio:rdm.example.org/abcde-12345\""),
            "{lock}"
        );
        let mut wanted = before;
        wanted.push(RDM_RECORD.to_owned());
        let targets = routes.targets();
        let resolving: Vec<&String> = targets[asked..]
            .iter()
            .filter(|target| !target.ends_with("/content"))
            .collect();
        assert_eq!(resolving, wanted.iter().collect::<Vec<_>>(), "{reference}");
    }
    Ok(())
}

/// a pasted page of a record's every version, whose api answer redirects to the newest one, is
/// that newest record, and data.toml keeps the page, so an upgrade takes a newer one.
#[test]
fn an_invenio_page_of_every_version_pins_the_newest() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = invenio(ROWS, &md5_hex(ROWS))?;
    let page = "https://rdm.example.org/records/fghij-67890";
    let output = command(&sandbox, &routes, &["add", page, "--as", "rows"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        format!("[datasets.rows]\nref = \"{page}\"\n")
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains("resolved = \"invenio:rdm.example.org/abcde-12345\""),
        "{lock}"
    );
    assert_eq!(fs::read(sandbox.work().join("data/rows/rows.csv"))?, ROWS);
    assert_eq!(
        routes.targets(),
        [
            RDM_CONCEPT.to_owned(),
            RDM_RECORD.to_owned(),
            format!("{RDM_RECORD}/files/rows.csv/content")
        ]
    );
    Ok(())
}

/// bytes unlike the MD5 another install states are refused, and never placed.
#[test]
fn an_invenio_file_unlike_its_md5_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = invenio(ROWS, &md5_hex(NOTES))?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "--json",
            "add",
            "invenio:rdm.example.org/abcde-12345",
            "--as",
            "rows",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(!sandbox.work().join("data/rows/rows.csv").exists());
    Ok(())
}

const DANDI_API: &str = "/api.dandiarchive.org/api/dandisets/000027";
const DANDI_VERSION: &str = "0.210831.2033";

/// dandi's answers for dandiset 000027, `embargo` as its status, whose newest published version
/// holds `(path, bytes, stated SHA-256)` assets, and the bytes behind each one's blob on S3.
fn dandi(embargo: &str, assets: &[(&str, &[u8], &str)]) -> Result<Routes> {
    let version = format!("{DANDI_API}/versions/{DANDI_VERSION}");
    let mut routes = vec![
        (
            format!("{DANDI_API}/"),
            answer(&serde_json::json!({
                "identifier": "000027",
                "embargo_status": embargo,
                "most_recent_published_version": {"version": DANDI_VERSION, "name": "a test dandiset"},
                "draft_version": {"version": "draft", "name": "a test dandiset"},
            }))?,
        ),
        (
            format!("{version}/info/"),
            answer(&serde_json::json!({
                "version": DANDI_VERSION,
                "name": "a test dandiset",
                "dandiset": {"identifier": "000027", "embargo_status": embargo},
                "metadata": {"license": ["spdx:CC-BY-4.0"], "doi": "10.48324/dandi.000027/0.210831.2033"},
            }))?,
        ),
    ];
    let mut results = Vec::new();
    for (at, (path, body, sha256)) in assets.iter().enumerate() {
        let blob = format!("blobs/2db/af0/2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb382{at}");
        results.push(serde_json::json!({
            "asset_id": format!("1c095f5f-d1e2-45db-b807-fdcfea08c6d{at}"),
            "blob": format!("2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb382{at}"),
            "zarr": null,
            "path": path,
            "size": body.len(),
            "metadata": {
                "digest": {"dandi:dandi-etag": "6ec084ca9d3be17ec194a8f700d65344-1", "dandi:sha2-256": sha256},
                "contentUrl": [
                    format!("https://api.dandiarchive.org/api/assets/1c095f5f-d1e2-45db-b807-fdcfea08c6d{at}/download/"),
                    format!("https://dandiarchive.s3.amazonaws.com/{blob}"),
                ],
            },
        }));
        routes.push((
            format!("/dandiarchive.s3.amazonaws.com/{blob}"),
            file(body, &format!("\"{}\"", md5_hex(body))),
        ));
    }
    routes.push((
        format!("{version}/assets/?metadata=true&order=path&page_size=1000"),
        answer(&serde_json::json!({"count": assets.len(), "next": null, "previous": null, "results": results}))?,
    ));
    Routes::start(routes)
}

/// the newest published version is pinned, and a teammate's sync reads only data.lock's blobs.
#[test]
fn a_dandiset_is_pinned_with_dandis_sha256_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = sha256_hex(ROWS);
    let routes = dandi("OPEN", &[("sub-RAT123/sub-RAT123.nwb", ROWS, &stated)])?;
    let output = command(&sandbox, &routes, &["add", "dandi:000027", "--as", "rat"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("resolved = \"dandi:000027@0.210831.2033\"")
            && lock.contains(&format!("sha256 = \"{stated}\""))
            && lock.contains("url = \"https://dandiarchive.s3.amazonaws.com/blobs/2db/af0/2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb3820\""),
        "{lock}"
    );
    assert_eq!(
        fs::read(work.join("data/rat/sub-RAT123/sub-RAT123.nwb"))?,
        ROWS
    );
    let listed = routes.targets().len();
    let (_, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    assert_eq!(
        routes.targets()[listed..],
        ["/dandiarchive.s3.amazonaws.com/blobs/2db/af0/2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb3820"],
        "the lock alone says what to download"
    );
    Ok(())
}

/// the bytes come from dandi's blobs on S3, and refused ones are never placed.
#[test]
fn bytes_that_differ_from_dandis_sha256_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dandi(
        "OPEN",
        &[("sub-RAT123/sub-RAT123.nwb", ROWS, &sha256_hex(NOTES))],
    )?;
    let output = command(&sandbox, &routes, &["add", "dandi:000027", "--as", "rat"]).output()?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("expected {}", sha256_hex(NOTES))),
        "{stderr}"
    );
    assert!(
        !sandbox
            .work()
            .join("data/rat/sub-RAT123/sub-RAT123.nwb")
            .exists()
    );
    Ok(())
}

/// dandi shows an embargoed dandiset to no one without an account, and no data.toml is written.
#[test]
fn an_embargoed_dandiset_is_refused_and_says_why() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        format!("{DANDI_API}/"),
        reply(
            "401 Unauthorized",
            &[("Content-Type", "application/json")],
            br#"{"detail":"Authentication credentials were not provided."}"#,
        ),
    )])?;
    let output = command(&sandbox, &routes, &["--json", "add", "dandi:000027"]).output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let said = json(&output)?;
    assert_eq!(said["errors"][0]["kind"], "auth.required");
    let message = said["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("embargo"), "{message}");
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

#[test]
fn get_takes_the_folder_a_pasted_dandi_page_names() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dandi(
        "OPEN",
        &[
            ("sub-RAT123/sub-RAT123.nwb", ROWS, &sha256_hex(ROWS)),
            ("sub-RAT124/sub-RAT124.nwb", NOTES, &sha256_hex(NOTES)),
        ],
    )?;
    let page =
        "https://dandiarchive.org/dandiset/000027/0.210831.2033/files?location=sub-RAT124%2F";
    let output = command(&sandbox, &routes, &["get", page, "--dir", "rats"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let rats = sandbox.work().join("rats");
    assert_eq!(fs::read(rats.join("sub-RAT124/sub-RAT124.nwb"))?, NOTES);
    assert!(!rats.join("sub-RAT123").exists());
    Ok(())
}

const OPENNEURO: &str = "/openneuro.org/crn/graphql";
const OPENNEURO_TREE: &str = "db72baeb3665a309db35d6ef649e117eb45a8a87";
const OPENNEURO_README: &str = "b6aff27bdd11bd66b06f01339be7cd794c56f586";
const OPENNEURO_SCAN: &str = "/s3.amazonaws.com/openneuro.org/ds000001/sub-01/sub-01_T1w.nii.gz?versionId=SHOhSfza0LCFpgctRH3sMZ8iwAGSUwIy";

/// the link of a GraphQL query to openneuro's API at the stand-in, as fetchloom sends it.
fn graphql(query: &str) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("query", query)
        .finish();
    format!("{OPENNEURO}?{encoded}")
}

/// the query for snapshot 1.0.0 of ds000001: its description and its top folder.
fn openneuro_top() -> String {
    graphql(
        r#"query{snapshot(datasetId:"ds000001",tag:"1.0.0"){tag deprecated{reason} description{Name License DatasetDOI} files{id filename size directory annexed urls}}}"#,
    )
}

/// the query for one folder of snapshot 1.0.0 of ds000001.
fn openneuro_folder(tree: &str) -> String {
    graphql(&format!(
        r#"query{{snapshot(datasetId:"ds000001",tag:"1.0.0"){{files(tree:"{tree}"){{id filename size directory annexed urls}}}}}}"#
    ))
}

/// openneuro's answers for snapshot 1.0.0 of ds000001: a README kept in git, `DataLad`'s own
/// folder, and a folder holding one scan in git-annex under `key`, whose bytes are `scan`.
fn openneuro(key: &str, scan: &[u8]) -> Result<Routes> {
    let objects = format!(
        "https://openneuro.org/crn/datasets/ds000001/objects/{OPENNEURO_README}?filename=README"
    );
    let top = serde_json::json!({"data": {"snapshot": {
        "tag": "1.0.0",
        "deprecated": null,
        "description": {"Name": "Balloon Analog Risk-taking Task", "License": "CC0", "DatasetDOI": "10.18112/openneuro.ds000001.v1.0.0"},
        "files": [
            {"id": OPENNEURO_README, "filename": "README", "size": NOTES.len(), "directory": false, "annexed": false, "urls": [objects]},
            {"id": "37e92e55882570ffb064bcb64423481b98e68da3", "filename": ".datalad", "size": 0, "directory": true, "annexed": false, "urls": []},
            {"id": OPENNEURO_TREE, "filename": "sub-01", "size": 0, "directory": true, "annexed": false, "urls": []},
        ],
    }}});
    let folder = serde_json::json!({"data": {"snapshot": {"files": [
        {"id": key, "filename": "sub-01_T1w.nii.gz", "size": scan.len(), "directory": false, "annexed": true,
         "urls": [format!("https://s3.amazonaws.com{}", OPENNEURO_SCAN.trim_start_matches("/s3.amazonaws.com"))]},
    ]}}});
    Routes::start(vec![
        (openneuro_top(), answer(&top)?),
        (openneuro_folder(OPENNEURO_TREE), answer(&folder)?),
        (
            format!(
                "/openneuro.org/crn/datasets/ds000001/objects/{OPENNEURO_README}?filename=README"
            ),
            file(NOTES, &format!("\"{OPENNEURO_README}\"")),
        ),
        (
            OPENNEURO_SCAN.to_owned(),
            file(scan, &format!("\"{}\"", md5_hex(scan))),
        ),
    ])
}

/// the files come from the S3 links pinned to the snapshot, every query carries the header
/// openneuro's API asks of a GET, and `DataLad`'s own folder is never asked for.
#[test]
fn an_openneuro_snapshot_is_pinned_by_its_annex_keys_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let key = format!("SHA256E-s{}--{}.nii.gz", ROWS.len(), sha256_hex(ROWS));
    let routes = openneuro(&key, ROWS)?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "openneuro:ds000001@1.0.0", "--as", "bart"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = sandbox.work();
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("resolved = \"openneuro:ds000001@1.0.0\"")
            && lock.contains(&format!("sha256 = \"{}\"", sha256_hex(ROWS)))
            && lock.contains("url = \"https://s3.amazonaws.com/openneuro.org/ds000001/sub-01/sub-01_T1w.nii.gz?versionId=SHOhSfza0LCFpgctRH3sMZ8iwAGSUwIy\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/bart/README"))?, NOTES);
    assert_eq!(
        fs::read(work.join("data/bart/sub-01/sub-01_T1w.nii.gz"))?,
        ROWS
    );
    let queries: Vec<String> = routes
        .requests()
        .into_iter()
        .filter(|head| head.contains(OPENNEURO))
        .collect();
    assert_eq!(
        queries.len(),
        2,
        "the top folder and sub-01, not .datalad: {queries:?}"
    );
    assert!(
        queries.iter().all(|head| head
            .to_ascii_lowercase()
            .contains("apollo-require-preflight: true")),
        "{queries:?}"
    );
    let listed = routes.targets().len();
    let (_, synced) = teammate_sync(&sandbox, &routes, &["sync", "--locked"])?;
    ok(&synced)?;
    let mut fetched = routes.targets()[listed..].to_vec();
    fetched.sort();
    assert_eq!(
        fetched,
        [
            format!(
                "/openneuro.org/crn/datasets/ds000001/objects/{OPENNEURO_README}?filename=README"
            ),
            OPENNEURO_SCAN.to_owned(),
        ],
        "the lock alone says what to download"
    );
    Ok(())
}

/// a snapshot nobody can change lists the same bytes when an upgrade asks again, so neither the
/// dry run nor the upgrade fetches the README, which no checksum pins.
#[test]
fn an_upgrade_to_the_snapshot_data_lock_pins_fetches_no_file() -> Result {
    let sandbox = Sandbox::new()?;
    let key = format!("SHA256E-s{}--{}.nii.gz", ROWS.len(), sha256_hex(ROWS));
    let routes = openneuro(&key, ROWS)?;
    let add = &["add", "openneuro:ds000001@1.0.0", "--as", "bart"];
    ok(&command(&sandbox, &routes, add).output()?)?;
    let listed = routes.targets().len();
    let dry = &["--json", "lock", "--upgrade", "--dry-run"];
    let planned = command(&sandbox, &routes, dry).output()?;
    ok(&planned)?;
    assert_eq!(
        json(&planned)?["result"]["changes"]["download"],
        serde_json::json!([])
    );
    ok(&command(&sandbox, &routes, &["lock", "--upgrade"]).output()?)?;
    let fetched: Vec<String> = routes.targets()[listed..]
        .iter()
        .filter(|target| !target.starts_with(OPENNEURO))
        .cloned()
        .collect();
    assert_eq!(fetched, Vec::<String>::new());
    Ok(())
}

/// an old dataset's git-annex key states an MD5, and refused bytes are never placed.
#[test]
fn bytes_that_differ_from_an_openneuro_annex_key_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let key = format!("MD5E-s{}--{}.nii.gz", ROWS.len(), md5_hex(NOTES));
    let routes = openneuro(&key, ROWS)?;
    let output = command(
        &sandbox,
        &routes,
        &["add", "openneuro:ds000001@1.0.0", "--as", "bart"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("MD5"), "{stderr}");
    assert!(
        !sandbox
            .work()
            .join("data/bart/sub-01/sub-01_T1w.nii.gz")
            .exists()
    );
    Ok(())
}

/// the listing takes one query, and the resolved line says what it covers.
#[test]
fn a_top_level_pattern_lists_openneuros_top_folder_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let key = format!("SHA256E-s{}--{}.nii.gz", ROWS.len(), sha256_hex(ROWS));
    let routes = openneuro(&key, ROWS)?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "add",
            "openneuro:ds000001@1.0.0",
            "--select",
            "README",
            "--as",
            "bart",
        ],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Resolved openneuro:ds000001@1.0.0 · Balloon Analog Risk-taking Task · 1 file in the top folder"),
        "{stderr}"
    );
    assert_eq!(fs::read(sandbox.work().join("data/bart/README"))?, NOTES);
    let queries: Vec<String> = routes
        .targets()
        .into_iter()
        .filter(|target| target.starts_with(OPENNEURO))
        .collect();
    assert_eq!(queries, [openneuro_top()]);
    Ok(())
}

/// openneuro shows such a dataset to no one without an account, and no data.toml is written.
#[test]
fn a_private_openneuro_dataset_is_refused_and_says_why() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        graphql(r#"query{dataset(id:"ds004999"){latestSnapshot{tag}}}"#),
        answer(&serde_json::json!({
            "errors": [{"message": "You do not have access to read this dataset.", "path": ["dataset"]}],
            "data": {"dataset": null},
        }))?,
    )])?;
    let output = command(&sandbox, &routes, &["--json", "add", "openneuro:ds004999"]).output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let said = json(&output)?;
    assert_eq!(said["errors"][0]["kind"], "auth.required");
    let message = said["errors"][0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("openneuro:ds004999"), "{message}");
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// `add --no-sync` of dryad's `files`, stated `(path, bytes, md5)`, as `archiving`, downloading
/// nothing, and data.lock as it left it.
fn added_unsynced(sandbox: &Sandbox, routes: &Routes) -> Result<String> {
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let asked = routes.targets().len();
    let output = command(
        sandbox,
        routes,
        &["add", page, "--as", "archiving", "--no-sync"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let downloads: Vec<String> = routes
        .targets()
        .split_off(asked)
        .into_iter()
        .filter(|target| target.ends_with("/download"))
        .collect();
    assert_eq!(downloads, Vec::<String>::new(), "nothing downloads");
    assert!(!sandbox.work().join("data/archiving").exists());
    Ok(fs::read_to_string(sandbox.work().join("data.lock"))?)
}

/// a dataset pinned by dryad's MD5 alone is written as not downloaded, counts as locked, and a
/// clone's first sync downloads it, checks each MD5 and size, pins it whole and places it.
#[test]
fn a_pin_by_the_sources_md5_is_downloaded_and_completed_by_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let (rows, notes) = (md5_hex(ROWS), md5_hex(NOTES));
    let routes = dryad(&[("rows.csv", ROWS, &rows), ("notes.txt", NOTES, &notes)])?;
    let lock = added_unsynced(&sandbox, &routes)?;
    let expected = format!(
        "downloaded = false\nfiles = [\n  {{ path = \"notes.txt\", size = {}, md5 = \"{notes}\", url = \"https://datadryad.org/api/v2/files/102/download\" }},\n  {{ path = \"rows.csv\", size = {}, md5 = \"{rows}\", url = \"https://datadryad.org/api/v2/files/101/download\" }},\n]\n",
        NOTES.len(),
        ROWS.len()
    );
    assert!(lock.contains(&expected), "{lock}");
    assert!(!lock.contains("sha256 = "), "{lock}");
    let check = sandbox.run(&["--json", "lock", "--check"])?;
    ok(&check)?;
    // only a sync downloads it: any other relock carries the pin as it is
    ok(&command(&sandbox, &routes, &["lock"]).output()?)?;
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);

    let status = sandbox.run(&["status"])?;
    assert!(
        text(&status.stdout)?.contains(
            "To download archiving · pinned by its source's MD5, not downloaded yet; `fetchloom sync archiving` downloads it, checks its MD5 and records its SHA-256"
        ),
        "{}",
        text(&status.stdout)?
    );
    let listed = json(&sandbox.run(&["--json", "list", "archiving"])?)?;
    assert_eq!(
        listed["result"]["datasets"][0]["downloaded"], false,
        "{listed}"
    );
    assert_eq!(listed["result"]["datasets"][0]["files"], 2, "{listed}");

    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["--json", "sync"])?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/archiving/rows.csv"))?, ROWS);
    assert_eq!(fs::read(teammate.join("data/archiving/notes.txt"))?, NOTES);
    let filled = fs::read_to_string(teammate.join("data.lock"))?;
    assert!(!filled.contains("downloaded"), "{filled}");
    assert!(
        filled.contains(&format!(
            "{{ path = \"rows.csv\", size = {}, sha256 = \"{}\", md5 = \"{rows}\", url = \"https://datadryad.org/api/v2/files/101/download\" }},",
            ROWS.len(),
            sha256_hex(ROWS)
        )),
        "{filled}"
    );
    let date = |lock: &str| {
        lock.lines()
            .find(|line| line.starts_with("locked = "))
            .map(str::to_owned)
    };
    assert_eq!(date(&filled), date(&lock), "the pin keeps its date");
    let again = sandbox
        .command_in(&teammate, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    ok(&again)?;
    Ok(())
}

/// bytes other than the MD5 data.lock pins fail the sync as any pin does, saying the file is not
/// the one data.lock records, and data.lock and the data folder stay as they were.
#[test]
fn a_sync_that_finds_other_bytes_than_the_stated_md5_records_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(NOTES))])?;
    let lock = added_unsynced(&sandbox, &routes)?;
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["--json", "sync"])?;
    assert_eq!(synced.status.code(), Some(30), "{}", text(&synced.stderr)?);
    let error = &json(&synced)?["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch");
    assert_eq!(
        error["message"],
        "archiving/rows.csv is not the file data.lock records"
    );
    let help = error["help"].as_str().unwrap_or_default();
    assert!(
        help.ends_with("if the new bytes are intended, run `fetchloom lock --upgrade archiving`"),
        "{help}"
    );
    assert_eq!(fs::read_to_string(teammate.join("data.lock"))?, lock);
    assert!(!teammate.join("data/archiving/rows.csv").exists());
    Ok(())
}

/// offline, a pin data.lock keeps by its source's MD5 needs one download to learn its SHA-256,
/// so the help says to run the same sync online, not to get a new link.
#[test]
fn an_offline_sync_of_a_stated_pin_says_to_run_it_online() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    added_unsynced(&sandbox, &routes)?;
    let (_, synced) = teammate_sync(&sandbox, &routes, &["--json", "--offline", "sync"])?;
    assert_eq!(synced.status.code(), Some(20), "{}", text(&synced.stderr)?);
    let error = &json(&synced)?["errors"][0];
    assert_eq!(error["kind"], "offline.missing", "{error}");
    assert_eq!(
        error["message"],
        "offline, and a file pinned by what its source states, not by a SHA-256, must be downloaded once to learn it",
        "{error}"
    );
    assert_eq!(
        error["help"], "run it again without --offline to download it",
        "{error}"
    );
    Ok(())
}

/// offline, one refusal names every file a sync lacks: those pinned by what their source states,
/// which one download must learn the SHA-256 of, and those data.lock names that the cache lacks.
/// the dry run names them the same way, and nothing asks either server.
#[test]
fn an_offline_sync_names_every_file_it_lacks_in_one_refusal() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    added_unsynced(&sandbox, &routes)?;
    let files = Routes::start(vec![("/files/notes.txt".to_owned(), file(NOTES, "\"n1\""))])?;
    sandbox.allow(&files.url);
    ok(&sandbox.run(&["add", &format!("{}/files/notes.txt", files.url)])?)?;
    let asked = (routes.requests().len(), files.requests().len());
    for args in [
        &["--json", "--offline", "sync"][..],
        &["--json", "--offline", "sync", "--dry-run"],
    ] {
        let (_, synced) = teammate_sync(&sandbox, &routes, args)?;
        assert_eq!(synced.status.code(), Some(20), "{}", text(&synced.stderr)?);
        let document = json(&synced)?;
        let errors = document["errors"].as_array().cloned().unwrap_or_default();
        let [error] = errors.as_slice() else {
            return Err(format!("{args:?}: one error, not {document}").into());
        };
        assert_eq!(error["kind"], "offline.missing", "{args:?}: {error}");
        assert_eq!(
            error["message"],
            "offline, and 2 files are not in the cache: 1 data.lock names, and 1 pinned by what its source states, which must be downloaded once to learn its SHA-256",
            "{args:?}: {error}"
        );
        assert_eq!(
            error["details"],
            serde_json::json!(["archiving/rows.csv", "notes/notes.txt"]),
            "{args:?}"
        );
        assert_eq!(
            error["help"],
            "run it again without --offline to download them, or set FETCHLOOM_CACHE to a cache folder that already holds them",
            "{args:?}"
        );
    }
    assert_eq!(
        (routes.requests().len(), files.requests().len()),
        asked,
        "offline asks nothing"
    );
    Ok(())
}

/// offline, a pin by its source's MD5 that this machine cannot fill waits, as a dataset missing
/// files does: the sync still places the datasets it has whole, and says which pin waits.
#[test]
fn an_offline_sync_places_what_it_has_while_a_stated_pin_waits() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    let unsynced = added_unsynced(&sandbox, &routes)?;
    let files = Routes::start(vec![("/files/notes.txt".to_owned(), file(NOTES, "\"n1\""))])?;
    sandbox.allow(&files.url);
    ok(&sandbox.run(&["add", &format!("{}/files/notes.txt", files.url)])?)?;
    // a clone on this machine, whose cache holds notes but never downloaded archiving
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let synced = sandbox
        .command_in(&clone, &["--json", "--offline", "sync"])
        .output()?;
    assert_eq!(synced.status.code(), Some(20), "{}", text(&synced.stderr)?);
    let error = &json(&synced)?["errors"][0];
    assert_eq!(
        error["message"],
        "offline, and a file pinned by what its source states, not by a SHA-256, must be downloaded once to learn it",
        "{error}"
    );
    assert_eq!(
        error["details"],
        serde_json::json!([
            "archiving/rows.csv",
            "placed notes, whose files are all on this machine",
            "archiving waits for files this run could not get"
        ]),
        "{error}"
    );
    assert_eq!(fs::read(clone.join("data/notes/notes.txt"))?, NOTES);
    assert!(!clone.join("data/archiving").exists());
    let lock = fs::read_to_string(clone.join("data.lock"))?;
    let pin = &unsynced[unsynced.find("[[dataset]]").unwrap_or_default()..];
    assert!(lock.contains(pin), "the pin stays as it is:\n{lock}");
    Ok(())
}

/// a dry run promises nothing its run does not do: a pin by its source's MD5 that the run cannot
/// fill, offline, alone or beside a file data.lock names that the cache lacks, or with only a
/// folder mirror to ask, which keeps no file by its MD5, is not told as one it would lock, and the
/// dry run refuses with its run's words and exit code.
#[test]
fn a_dry_run_refuses_as_its_run_does_and_promises_no_lock() -> Result {
    for (beside, mirrors_only) in [(false, false), (true, false), (false, true)] {
        let sandbox = Sandbox::new()?;
        let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
        added_unsynced(&sandbox, &routes)?;
        if beside {
            let files =
                Routes::start(vec![("/files/notes.txt".to_owned(), file(NOTES, "\"n1\""))])?;
            sandbox.allow(&files.url);
            ok(&sandbox.run(&["add", &format!("{}/files/notes.txt", files.url)])?)?;
        }
        let root = sandbox.folder("mirror")?;
        fs::write(root.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
        let mirror = root.to_string_lossy().into_owned();
        let (args, envs): (&[&str], &[(&str, &str)]) = if mirrors_only {
            (
                &["--mirror", &mirror, "sync"],
                &[("FETCHLOOM_MIRRORS_ONLY", "1")],
            )
        } else {
            (&["--offline", "sync"], &[])
        };
        let case = format!("beside a missing file: {beside}, mirrors only: {mirrors_only}");
        let (teammate, run) = teammate_sync_with(&sandbox, &routes, args, envs)?;
        let refused = if mirrors_only { 10 } else { 20 };
        assert_eq!(
            run.status.code(),
            Some(refused),
            "{case}: {}",
            text(&run.stderr)?
        );
        assert_eq!(
            fs::read(teammate.join("data.lock"))?,
            fs::read(sandbox.work().join("data.lock"))?,
            "{case}: the run locks nothing"
        );
        let dry = [args, &["--dry-run"]].concat();
        let (_, dry) = teammate_sync_with(&sandbox, &routes, &dry, envs)?;
        assert_eq!(
            (dry.status.code(), text(&dry.stderr)?),
            (run.status.code(), text(&run.stderr)?),
            "{case}"
        );
    }
    Ok(())
}

/// offline, a pin by the SHA-256 its source states fills from a folder mirror that holds the file,
/// and its dry run, which only looks at the mirror, says it would lock the pin rather than refuse.
#[test]
fn an_offline_dry_run_counts_a_stated_file_a_mirror_holds_as_its_run_does() -> Result {
    let sandbox = Sandbox::new()?;
    let digest = sha256_hex(ROWS);
    let routes = dryad_stating("sha-256", &[("rows.csv", ROWS, &digest)])?;
    added_unsynced(&sandbox, &routes)?;
    let root = sandbox.folder("mirror")?;
    fs::write(root.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
    let fan = root.join("objects").join(&digest[..2]);
    fs::create_dir_all(&fan)?;
    fs::write(fan.join(&digest[2..]), ROWS)?;
    let mirror = root.to_string_lossy().into_owned();
    let (_, dry) = teammate_sync(
        &sandbox,
        &routes,
        &["--offline", "--mirror", &mirror, "sync", "--dry-run"],
    )?;
    let said = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{said}");
    assert!(said.contains("Would lock archiving"), "{said}");
    let (teammate, run) = teammate_sync(
        &sandbox,
        &routes,
        &["--offline", "--mirror", &mirror, "sync"],
    )?;
    assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr)?);
    assert_eq!(fs::read(teammate.join("data/archiving/rows.csv"))?, ROWS);
    Ok(())
}

/// offline, a pin by the SHA-256 its source states fills from a folder `reuse` lists that holds
/// the file under any name, and its dry run, which only looks there, says the same: the same exit
/// code, and the lock the run makes or the refusal it gives.
#[test]
fn an_offline_dry_run_counts_a_stated_file_a_reuse_folder_holds_as_its_run_does() -> Result {
    let sandbox = Sandbox::new()?;
    let digest = sha256_hex(ROWS);
    let routes = dryad_stating("sha-256", &[("rows.csv", ROWS, &digest)])?;
    added_unsynced(&sandbox, &routes)?;
    let folder = sandbox.folder("downloads")?;
    fs::write(folder.join("sites.csv"), ROWS)?;
    let reuse = folder.to_string_lossy().into_owned();
    let envs = [("FETCHLOOM_REUSE", reuse.as_str())];
    // the dry run first, while the teammate's cache lacks the file and only the folder holds it
    let (_, dry) = teammate_sync_with(
        &sandbox,
        &routes,
        &["--offline", "sync", "--dry-run"],
        &envs,
    )?;
    let (teammate, run) = teammate_sync_with(&sandbox, &routes, &["--offline", "sync"], &envs)?;
    let (dry_said, run_said) = (text(&dry.stderr)?, text(&run.stderr)?);
    // the lock line, by the run's verb, and every line of a refusal
    let told = |said: &str| -> Vec<String> {
        let refused = said.lines().skip_while(|line| !line.starts_with("error:"));
        said.lines()
            .map(str::trim)
            .filter(|line| line.ends_with(" in data.lock"))
            .map(|line| line.replacen("Would lock", "Locked", 1))
            .chain(refused.map(str::to_owned))
            .collect()
    };
    assert_eq!(
        (dry.status.code(), told(&dry_said)),
        (run.status.code(), told(&run_said)),
        "dry run:\n{dry_said}\nrun:\n{run_said}"
    );
    if run.status.success() {
        assert_eq!(fs::read(teammate.join("data/archiving/rows.csv"))?, ROWS);
    }
    Ok(())
}

/// `--locked` refuses a pin a sync would complete, since data.lock would change, saying which
/// sync completes it; the check without it counts the pin as locked.
#[test]
fn a_locked_sync_refuses_a_pin_not_downloaded_yet() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    let unsynced = added_unsynced(&sandbox, &routes)?;
    // another add rewrites data.lock and carries the pin as it is
    let files = Routes::start(vec![("/files/notes.txt".to_owned(), file(NOTES, "\"n1\""))])?;
    ok(&sandbox.run(&["add", &format!("{}/files/notes.txt", files.url)])?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let pin = &unsynced[unsynced.find("[[dataset]]").unwrap_or_default()..];
    assert!(
        lock.contains(pin) && lock.contains("name = \"notes\""),
        "{lock}"
    );
    for args in [
        &["--json", "sync", "--locked"][..],
        &["--json", "sync", "archiving", "--locked"],
    ] {
        let output = command(&sandbox, &routes, args).output()?;
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], "lock.outdated", "{args:?}");
        assert_eq!(
            error["message"],
            "data.lock would change: archiving is pinned by its source's MD5 until a sync downloads it"
        );
        assert_eq!(
            error["help"],
            "run `fetchloom sync archiving` where the data can download, then commit data.lock"
        );
    }
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);
    // data.toml changing the dataset makes the pin out of date, as any pin
    let manifest = sandbox.work().join("data.toml");
    let table = "[datasets.archiving]\nref = \"dryad:10.5061/dryad.j1fd7\"\n";
    let excluding = fs::read_to_string(&manifest)?
        .replace(table, &format!("{table}exclude = [\"notes.txt\"]\n"));
    fs::write(&manifest, excluding)?;
    let check = sandbox.run(&["--json", "lock", "--check"])?;
    assert_eq!(check.status.code(), Some(1));
    assert_eq!(
        json(&check)?["errors"][0]["details"][0],
        "archiving changed in data.toml"
    );
    Ok(())
}

/// an archive pinned by its source's MD5 gets the tree of what it unpacks to once a sync
/// downloads it, and its files are placed unpacked.
#[test]
fn an_archive_pinned_by_its_md5_gets_its_tree_from_the_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = crate::support::zip_of(&[("rows.csv", ROWS), ("notes.txt", NOTES)])?;
    let routes = dryad(&[("bundle.zip", &zip, &md5_hex(&zip))])?;
    let lock = added_unsynced(&sandbox, &routes)?;
    assert!(!lock.contains("unpacks_to"), "{lock}");
    let (teammate, synced) = teammate_sync(&sandbox, &routes, &["--json", "sync"])?;
    ok(&synced)?;
    let filled = fs::read_to_string(teammate.join("data.lock"))?;
    assert!(filled.contains(", unpacks_to = \"sha256:"), "{filled}");
    assert_eq!(fs::read(teammate.join("data/archiving/rows.csv"))?, ROWS);
    assert_eq!(fs::read(teammate.join("data/archiving/notes.txt"))?, NOTES);
    Ok(())
}

/// a source that states no hash of a file is refused before anything is written, naming it; so
/// is a path on this machine.
#[test]
fn no_sync_refuses_files_no_source_hash_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![("/files/rows.csv".to_owned(), file(ROWS, "\"r1\""))])?;
    let link = format!("{}/files/rows.csv", routes.url);
    let output = sandbox.run(&["--json", "add", &link, "--no-sync"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "usage");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.ends_with("states no size with a SHA-256 or MD5 for 1 file: rows.csv"),
        "{message}"
    );
    assert_eq!(
        error["help"],
        format!(
            "pinning them needs their bytes: run `fetchloom add {link}` without --no-sync where the data can download"
        )
    );
    assert_eq!(routes.targets(), Vec::<String>::new(), "nothing downloads");
    // a mistyped path is told as missing first, as an add without --no-sync tells it
    let missing = sandbox.run(&["--json", "add", "./flowres", "--no-sync"])?;
    assert_eq!(
        json(&missing)?["errors"][0]["message"],
        json(&sandbox.run(&["--json", "add", "./flowres"])?)?["errors"][0]["message"]
    );
    fs::create_dir(sandbox.work().join("flowers"))?;
    let output = sandbox.run(&["--json", "add", "./flowers", "--no-sync"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "--no-sync pins what a source states, and files on this machine have no source"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join("data.lock").exists());
    Ok(())
}

/// a clone's data.lock is untrusted: a pin not downloaded yet that would place a file outside its
/// folder is refused before anything downloads.
#[test]
fn a_hostile_pin_not_downloaded_from_a_clone_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    let lock = added_unsynced(&sandbox, &routes)?;
    fs::write(
        sandbox.work().join("data.lock"),
        lock.replace("path = \"rows.csv\"", "path = \"../../rows.csv\""),
    )?;
    let asked = routes.targets().len();
    let output = command(&sandbox, &routes, &["--json", "sync"]).output()?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "project.invalid");
    assert_eq!(routes.targets().len(), asked, "nothing was asked");
    Ok(())
}

/// an add after `add --no-sync` places everything it can, but leaves the pin to download, so the
/// next sync, plain or strict, still downloads it rather than finding the project settled.
#[test]
fn a_later_add_leaves_a_pin_not_downloaded_to_the_next_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    added_unsynced(&sandbox, &routes)?;
    let files = Routes::start(vec![("/files/notes.txt".to_owned(), file(NOTES, "\"n1\""))])?;
    ok(&sandbox.run(&["add", &format!("{}/files/notes.txt", files.url)])?)?;
    let synced = command(&sandbox, &routes, &["--json", "sync", "--strict"]).output()?;
    ok(&synced)?;
    assert_eq!(
        fs::read(sandbox.work().join("data/archiving/rows.csv"))?,
        ROWS
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(!lock.contains("downloaded = false"), "{lock}");
    Ok(())
}

/// `path` syncs a dataset not downloaded yet, as it syncs any dataset not in place: it downloads
/// it, checks each stated MD5, pins it whole and prints its folder; only `--locked` refuses.
#[test]
fn path_downloads_a_pin_not_downloaded_yet_and_answers() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    added_unsynced(&sandbox, &routes)?;
    let locked = command(
        &sandbox,
        &routes,
        &["--json", "path", "archiving", "--locked"],
    )
    .output()?;
    assert_eq!(locked.status.code(), Some(1), "{}", text(&locked.stderr)?);
    assert_eq!(json(&locked)?["errors"][0]["kind"], "lock.outdated");
    let output = command(&sandbox, &routes, &["path", "archiving/rows.csv"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let placed = sandbox
        .work()
        .join("data")
        .join("archiving")
        .join("rows.csv");
    assert_eq!(
        text(&output.stdout)?.trim_end(),
        placed.display().to_string()
    );
    assert_eq!(fs::read(&placed)?, ROWS);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(!lock.contains("downloaded = false"), "{lock}");
    Ok(())
}

/// `status --check` fails while a dataset is not downloaded yet, naming it and the sync that
/// downloads it, as it fails for a file not in place.
#[test]
fn status_check_fails_while_a_pin_is_not_downloaded_yet() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    added_unsynced(&sandbox, &routes)?;
    let output = sandbox.run(&["--json", "status", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "data.not_ready");
    assert_eq!(
        error["details"][0],
        "archiving is pinned by its source's MD5, not downloaded yet: `fetchloom sync archiving` downloads and checks it"
    );
    // the sync named downloads, which whoever runs fetchloom chooses, so it is advice, no fix
    assert_eq!(error["fix"], serde_json::Value::Null);
    ok(&command(&sandbox, &routes, &["sync"]).output()?)?;
    ok(&sandbox.run(&["status", "--check"])?)?;
    Ok(())
}

/// `add --no-sync` of a link data.toml has but data.lock does not pin, written by hand or changed
/// since its pin, pins it as a first add would and leaves data.toml as it is; once pinned, it
/// changes nothing.
#[test]
fn add_no_sync_pins_a_dataset_data_toml_has_without_a_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[
        ("rows.csv", ROWS, &md5_hex(ROWS)),
        ("notes.txt", NOTES, &md5_hex(NOTES)),
    ])?;
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let manifest = sandbox.work().join("data.toml");
    let table = "[datasets.archiving]\nref = \"dryad:10.5061/dryad.j1fd7\"\n";
    fs::write(&manifest, table)?;
    let lock = sandbox.work().join("data.lock");
    let pinned = |args: &[&str]| -> Result<String> {
        let output = command(&sandbox, &routes, args).output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains("Pinned archiving in data.lock"), "{stderr}");
        Ok(fs::read_to_string(&lock)?)
    };
    let first = pinned(&["add", page, "--no-sync"])?;
    assert!(
        first.contains("name = \"archiving\"") && first.contains("downloaded = false"),
        "{first}"
    );
    assert!(first.contains("path = \"notes.txt\""), "{first}");
    assert_eq!(fs::read_to_string(&manifest)?, table);
    // data.toml leaves notes.txt out now, so the pin is out of date
    let excluding = format!("{table}exclude = [\"notes.txt\"]\n");
    fs::write(&manifest, &excluding)?;
    let again = pinned(&["add", page, "--exclude", "notes.txt", "--no-sync"])?;
    assert!(!again.contains("path = \"notes.txt\""), "{again}");
    assert_eq!(fs::read_to_string(&manifest)?, excluding);
    ok(&sandbox.run(&["--json", "lock", "--check"])?)?;
    let output = command(
        &sandbox,
        &routes,
        &["add", page, "--exclude", "notes.txt", "--no-sync"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok archiving is already in data.toml"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(&lock)?, again);
    let downloads = routes
        .targets()
        .into_iter()
        .filter(|target| target.ends_with("/download"))
        .count();
    assert_eq!(downloads, 0, "nothing downloads");
    Ok(())
}

/// `add --no-sync` of the link of a dataset data.toml renamed keeps the pin data.lock holds under
/// the old name, which a sync carries, and never pins what the source states now: the next sync
/// keeps the pinned bytes under the new name.
#[test]
fn add_no_sync_keeps_the_pin_of_a_renamed_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let first = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    ok(&command(&sandbox, &first, &["add", page, "--as", "ds"]).output()?)?;
    let manifest = sandbox.work().join("data.toml");
    let renamed = fs::read_to_string(&manifest)?.replace("[datasets.ds]", "[datasets.renamed]");
    fs::write(&manifest, renamed)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    // what the source states now differs from what data.lock pins
    let now = dryad(&[("rows.csv", NOTES, &md5_hex(NOTES))])?;
    let output = command(&sandbox, &now, &["add", page, "--no-sync"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok renamed is already in data.toml, and data.lock pins it as ds"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(sandbox.work().join("data.lock"))?, lock);
    ok(&command(&sandbox, &now, &["sync"]).output()?)?;
    assert_eq!(
        fs::read(sandbox.work().join("data/renamed/rows.csv"))?,
        ROWS
    );
    Ok(())
}

/// a pin by MD5 alone is only as strong as MD5 until the first sync records each file's
/// SHA-256, and the add says so in one line; one by SHA-256 says nothing more.
#[test]
fn add_no_sync_says_the_first_sync_records_the_sha256() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    for args in [
        &["add", page, "--as", "archiving", "--no-sync", "--dry-run"][..],
        &["add", page, "--as", "archiving", "--no-sync"],
    ] {
        let output = command(&sandbox, &routes, args).output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(
            stderr.contains(
                "            the first sync checks these files by MD5 alone, then records their SHA-256 for every later sync\n"
            ),
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}

/// a pin `add --no-sync` replaces takes its file list in data.lock.d with it, as any write of
/// data.lock does, so `git add data.lock.d` never commits a list nothing names.
#[test]
fn add_no_sync_sweeps_the_list_of_a_pin_it_replaces() -> Result {
    let sandbox = Sandbox::new()?;
    let files = Routes::start(vec![("/files/rows.csv".to_owned(), file(ROWS, "\"r1\""))])?;
    let listed = sandbox
        .command(&[
            "add",
            &format!("{}/files/rows.csv", files.url),
            "--as",
            "archiving",
        ])
        .env("FETCHLOOM_TEST_INLINE", "0")
        .output()?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let lists = sandbox.work().join("data.lock.d");
    assert!(lists.is_dir());
    // data.toml loses the dataset by hand, so data.lock still pins it, with its list
    fs::write(sandbox.work().join("data.toml"), "")?;
    let routes = dryad(&[("rows.csv", ROWS, &md5_hex(ROWS))])?;
    let page = "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7";
    let output = command(
        &sandbox,
        &routes,
        &["add", page, "--as", "archiving", "--no-sync"],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains("downloaded = false"), "{lock}");
    assert!(!lists.exists(), "{:?}", fs::read_dir(&lists)?.count());
    Ok(())
}
