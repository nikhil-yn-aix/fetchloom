//! research repositories, listed and downloaded through a server on this machine that stands in
//! for their hosts, API and files alike.
#![cfg(debug_assertions)]

use std::fs;
use std::process::Command;

use crate::auth::with_stdin;
use crate::buckets::md5;
use crate::support::{Part, Result, Routes, Sandbox, file, json, ok, reply, sha256_hex, text};

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

const MENDELEY: &str = "/data.mendeley.com";

/// mendeley's answers for rscbjbr9sj at version 2, its top folder holding `(name, bytes, sha256)`
/// files with the SHA-256 stated, and each file's public download link sending it on to the
/// storage that serves its bytes.
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

/// a pasted version link is pinned with the SHA-256 mendeley states for each file, its download
/// link followed on to its storage, and a teammate's empty cache gets the same bytes from the lock.
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

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = sandbox
        .command_in(&teammate, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
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

/// the SHA-256 mendeley states checks the bytes its storage sends: other bytes are never placed.
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
    let listed: Vec<_> = assets
        .iter()
        .map(|(name, body, sha256)| {
            serde_json::json!({
                "name": name,
                "size": body.len(),
                "state": "uploaded",
                "digest": format!("sha256:{sha256}"),
                "browser_download_url": format!("https://github.com/jqlang/jq/releases/download/jq-1.8.2/{name}"),
            })
        })
        .collect();
    let release = serde_json::json!({"tag_name": "jq-1.8.2", "name": "jq 1.8.2", "assets": listed});
    let mut routes = vec![(
        "/api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2".to_owned(),
        answer(&release)?,
    )];
    for (name, body, _) in assets {
        let stored = format!("/storage/{name}");
        routes.push((
            format!("/github.com/jqlang/jq/releases/download/jq-1.8.2/{name}"),
            reply("302 Found", &[("Location", &stored)], b""),
        ));
        routes.push((stored, file(body, &format!("\"{name}\""))));
    }
    Routes::start(routes)
}

/// a pasted release page is pinned at its tag with the SHA-256 github states for each file, and
/// a teammate's empty cache gets the same bytes from the lock alone.
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

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = sandbox
        .command_in(&teammate, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
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

/// the SHA-256 github states checks the bytes its storage sends: other bytes are never placed.
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

/// gitlab's answer for gitlab-runner's release v19.3.6 holding `(path, bytes, stored)` links,
/// each at its own address: in an S3 bucket, or on gitlab.com in the project's packages when
/// `stored` says so. as gitlab.com does, a link's download address there answers a program with a
/// page that warns of leaving gitlab rather than sending it on to another host.
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

/// `get` takes a gitlab release with `--select`, its links keep the paths the release gives them,
/// and each file comes from the link's own address, in a bucket or on gitlab.com, never from the
/// page gitlab.com shows in its place.
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
    .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let folder = sandbox.work().join("runner");
    assert_eq!(fs::read(folder.join("binaries/rows.csv"))?, ROWS);
    assert_eq!(fs::read(folder.join("binaries/notes.txt"))?, NOTES);
    assert!(!folder.join("release.sha256").exists());
    Ok(())
}

const ENA_API: &str = "/www.ebi.ac.uk/ena/portal/api";

/// a file a stand-in serves: its name, its bytes and the hash its source states.
type Served<'a> = (&'a str, &'a [u8], &'a str);
const ENA_FIELDS: &str = "run_accession,fastq_ftp,fastq_md5,fastq_bytes,submitted_ftp,submitted_md5,submitted_bytes,submitted_format,sra_ftp,sra_md5,sra_bytes,study_title";

/// ENA's answers for `accession`: the count of its runs, and its report of each run's FASTQ
/// files `(name, bytes, md5)` at their addresses on ftp.sra.ebi.ac.uk, with the bytes there. a
/// run with no files is under controlled access, as ENA reports a run dbGaP holds.
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

/// a pasted page of ENA's browser is pinned with the MD5 ENA states for each of the run's files,
/// and a teammate's empty cache gets the same bytes from the lock alone.
#[test]
fn an_ena_run_is_pinned_with_enas_md5_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let reads = md5(ROWS);
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

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = sandbox
        .command_in(&teammate, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
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

/// a study's runs under controlled access are left out, and the add says so, as a warning a
/// script can match.
#[test]
fn an_ena_study_leaves_out_runs_under_controlled_access_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (md5(ROWS), md5(NOTES));
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
    let warning = &document["warnings"][0];
    assert_eq!(warning["kind"], "source.partial", "{document}");
    assert_eq!(
        warning["message"],
        "1 of the 2 runs is under controlled access, so its files are left out"
    );
    let folder = sandbox.work().join("data/nhl/SRR1236466");
    assert_eq!(fs::read(folder.join("SRR1236466_1.fastq.gz"))?, ROWS);
    assert!(!sandbox.work().join("data/nhl/SRR036297").exists());
    Ok(())
}

/// `get` takes an experiment's runs, and `--select` picks one run by its accession.
#[test]
fn get_selects_one_run_of_an_ena_experiment_by_its_accession() -> Result {
    let sandbox = Sandbox::new()?;
    let (one, two) = (md5(ROWS), md5(NOTES));
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

/// the MD5 ENA states checks the bytes its file server sends: other bytes are never placed.
#[test]
fn bytes_that_differ_from_enas_md5_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = md5(NOTES);
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

/// NCBI's file server for GSE1000: its folder, and the folders in it holding `(path, bytes)`
/// files, as `matrix/GSE1000_series_matrix.txt.gz`, each index rounding sizes as NCBI's do.
fn geo(files: &[(&str, &[u8])]) -> Result<Routes> {
    let index = |entries: &[String]| -> Vec<Part> {
        let links = entries
            .iter()
            .map(|entry| format!("<a href=\"{entry}\">{entry}</a>  2026-10-05 16:24  1.3M\n"))
            .collect::<Vec<_>>()
            .concat();
        reply(
            "200 OK",
            &[("Content-Type", "text/html;charset=UTF-8")],
            format!("<pre><a href=\"/geo/series/GSE1nnn/\">Parent Directory</a>\n{links}</pre>")
                .as_bytes(),
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
        routes.push((format!("{GEO_SERIES}/{folder}/"), index(&names)));
    }
    for (path, body) in files {
        routes.push((format!("{GEO_SERIES}/{path}"), file(body, "\"v1\"")));
    }
    Routes::start(routes)
}

/// a pasted page of GEO's site is pinned by each file's SHA-256 and the size its download gave,
/// not the size an index rounds, and a teammate's empty cache gets the same bytes from the lock
/// alone.
#[test]
fn a_geo_series_is_pinned_by_its_bytes_and_synced_from_the_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = geo(&[
        ("matrix/GSE1000_series_matrix.txt.gz", ROWS),
        ("suppl/filelist.txt", NOTES),
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
        NOTES
    );

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = sandbox
        .command_in(&teammate, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
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

/// a pasted link to one of a series' folders on NCBI's file server takes that folder alone.
#[test]
fn get_takes_the_folder_a_pasted_geo_link_names() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = geo(&[
        ("matrix/GSE1000_series_matrix.txt.gz", ROWS),
        ("suppl/filelist.txt", NOTES),
    ])?;
    let folder = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/";
    let output = command(&sandbox, &routes, &["get", folder, "--dir", "gse1000"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let got = sandbox.work().join("gse1000");
    assert_eq!(fs::read(got.join("suppl/filelist.txt"))?, NOTES);
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
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let synced = sandbox
        .command_in(&teammate, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    assert_eq!(synced.status.code(), Some(30), "{}", text(&synced.stderr)?);
    assert_eq!(json(&synced)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(
        !teammate
            .join("data/gse1000/matrix/GSE1000_series_matrix.txt.gz")
            .exists()
    );
    Ok(())
}
