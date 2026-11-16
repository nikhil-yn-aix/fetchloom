//! folders of github and gitlab repositories, pinned at the commit their branch names, through a
//! server on this machine that stands in for each host: the apis and the hosts that serve files.
#![cfg(debug_assertions)]

use std::fs;

use crate::support::{Part, Result, Routes, Sandbox, file, json, ok, reply, sha256_hex, text};

const FIRST: &str = "0bda0ce801be0fa2f464ff845a9d5ceae99aad7d";
const SECOND: &str = "71e2436a092d714350de0fc409ca8a8714e7e78f";
const ROWS: &[u8] = b"site,count\na,1\nb,2\n";
const NOTES: &[u8] = b"collected in 2020\n";
const GITHUB_API: &str = "/api.github.com/repos/lab/survey";

/// what git holds at a path of a repository.
#[derive(Clone, Copy)]
enum Held<'a> {
    /// a file git keeps itself.
    File(&'a [u8]),
    /// a file git-lfs keeps apart: git holds its pointer, and the file's bytes sit elsewhere.
    Lfs(&'a [u8]),
    /// a symbolic link to another path.
    Link(&'a str),
    /// another repository at one of its commits.
    Submodule,
}

/// the pointer git-lfs commits in place of `bytes`.
fn pointer(bytes: &[u8]) -> Vec<u8> {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        sha256_hex(bytes),
        bytes.len()
    )
    .into_bytes()
}

/// `path` as a request names it.
fn encoded(path: &str) -> String {
    path.replace(' ', "%20")
}

/// the folders above each of `paths`, once each.
fn folders(paths: &[&str]) -> Vec<String> {
    let mut folders: Vec<String> = Vec::new();
    for path in paths {
        let mut above = String::new();
        for part in path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map_or(&[][..], |(_, rest)| rest)
        {
            if !above.is_empty() {
                above.push('/');
            }
            above.push_str(part);
            if !folders.contains(&above) {
                folders.push(above.clone());
            }
        }
    }
    folders
}

fn answer(value: &serde_json::Value) -> Result<Vec<Part>> {
    Ok(reply(
        "200 OK",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(value)?,
    ))
}

/// a redirect to `location`.
fn moved(location: &str) -> Vec<Part> {
    reply(
        "302 Found",
        &[("Location", location), ("Content-Length", "0")],
        b"",
    )
}

/// github's answers for lab/survey with `branch` at `commit`, whose tree holds `held`: the
/// commit the branch names, its whole tree in one answer, and each file where github serves
/// it. raw.githubusercontent.com serves what git holds, a pointer for a file git-lfs keeps;
/// github.com sends a file on to it, or one git-lfs keeps to media.githubusercontent.com.
fn github(branch: &str, commit: &str, held: &[(&str, Held)]) -> Result<Vec<(String, Vec<Part>)>> {
    let mut tree: Vec<serde_json::Value> = folders(&held.iter().map(|(path, _)| *path).collect::<Vec<_>>())
        .into_iter()
        .map(|folder| serde_json::json!({"path": folder, "mode": "040000", "type": "tree", "sha": "4b825dc642cb6eb9a060e54bf8d69288fbee4904"}))
        .collect();
    let mut routes = Vec::new();
    for (path, kind) in held {
        let raw = format!(
            "/raw.githubusercontent.com/lab/survey/{commit}/{}",
            encoded(path)
        );
        let page = format!("/github.com/lab/survey/raw/{commit}/{}", encoded(path));
        let stored = match kind {
            Held::File(bytes) => {
                routes.push((
                    page,
                    moved(&format!(
                        "https://raw.githubusercontent.com/lab/survey/{commit}/{}",
                        encoded(path)
                    )),
                ));
                bytes.to_vec()
            }
            Held::Lfs(bytes) => {
                let media = format!(
                    "/media.githubusercontent.com/media/lab/survey/{commit}/{}",
                    encoded(path)
                );
                routes.push((page, moved(&format!("https://{}", &media[1..]))));
                routes.push((media, file(bytes, "\"lfs\"")));
                pointer(bytes)
            }
            Held::Link(target) => target.as_bytes().to_vec(),
            Held::Submodule => {
                tree.push(serde_json::json!({"path": path, "mode": "160000", "type": "commit", "sha": SECOND}));
                continue;
            }
        };
        let mode = if matches!(kind, Held::Link(_)) {
            "120000"
        } else {
            "100644"
        };
        tree.push(serde_json::json!({
            "path": path,
            "mode": mode,
            "type": "blob",
            "sha": "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
            "size": stored.len(),
        }));
        routes.push((raw, file(&stored, "\"git\"")));
    }
    routes.push((
        format!("{GITHUB_API}/commits/{branch}"),
        reply(
            "200 OK",
            &[("Content-Type", "application/vnd.github.sha; charset=utf-8")],
            commit.as_bytes(),
        ),
    ));
    routes.push((
        format!("{GITHUB_API}/git/trees/{commit}?recursive=1"),
        answer(&serde_json::json!({
            "sha": commit,
            "url": format!("https://api.github.com/repos/lab/survey/git/trees/{commit}"),
            "tree": tree,
            "truncated": false,
        }))?,
    ));
    Ok(routes)
}

/// a command whose requests to every host go to `routes`.
fn run(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Result<std::process::Output> {
    Ok(sandbox
        .command(args)
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?)
}

/// a teammate's clone of this project, synced from data.lock alone with a cache of its own: the
/// folder, and what ran.
fn teammate_sync(
    sandbox: &Sandbox,
    routes: &Routes,
) -> Result<(std::path::PathBuf, std::process::Output)> {
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let synced = sandbox
        .command_in(&teammate, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    Ok((teammate, synced))
}

/// the paths of the files under `folder`, with `/` between folders, sorted.
fn placed(folder: &std::path::Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        for entry in fs::read_dir(&next)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let inside = path.strip_prefix(folder)?;
                let parts: Vec<String> = inside
                    .iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect();
                found.push(parts.join("/"));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// a 2 KiB file, which github's tree states the size of, since no git-lfs pointer is that large.
fn large() -> Vec<u8> {
    (0..2048u32).map(|n| (n % 251) as u8).collect()
}

/// a pasted folder link at a branch pins the folder's every file, subfolders included, at the
/// commit the branch names, with two requests to github's api; a teammate's sync asks no api.
#[test]
fn a_github_folder_at_a_branch_pins_every_file_at_its_commit() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("README.md", Held::File(b"# survey\n")),
            ("data/rows.csv", Held::File(ROWS)),
            ("data/2020/field notes.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
            ("other/rows.csv", Held::File(NOTES)),
        ],
    )?)?;
    let link = "https://github.com/lab/survey/tree/main/data";
    let output = run(&sandbox, &routes, &["add", link, "--as", "survey"])?;
    ok(&output)?;
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains(&format!("[datasets.survey]\nref = \"{link}\"\n")),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "resolved = \"https://github.com/lab/survey/tree/{FIRST}/data\""
        )),
        "{lock}"
    );
    for (path, bytes, url) in [
        (
            "rows.csv",
            ROWS,
            format!("https://github.com/lab/survey/raw/{FIRST}/data/rows.csv"),
        ),
        (
            "2020/field notes.txt",
            NOTES,
            format!("https://github.com/lab/survey/raw/{FIRST}/data/2020/field%20notes.txt"),
        ),
        (
            "big.bin",
            &big,
            format!("https://raw.githubusercontent.com/lab/survey/{FIRST}/data/big.bin"),
        ),
    ] {
        assert!(
            lock.contains(&format!(
                "{{ path = \"{path}\", size = {}, sha256 = \"{}\", url = \"{url}\" }}",
                bytes.len(),
                sha256_hex(bytes)
            )),
            "{path}: {lock}"
        );
        assert_eq!(
            fs::read(work.join("data/survey").join(path))?,
            bytes,
            "{path}"
        );
    }
    assert_eq!(
        placed(&work.join("data/survey"))?,
        ["2020/field notes.txt", "big.bin", "rows.csv"],
        "the folder's files alone, at their paths inside it"
    );
    let asked: Vec<String> = routes
        .targets()
        .into_iter()
        .filter(|target| target.starts_with("/api.github.com"))
        .collect();
    assert_eq!(
        asked,
        [
            format!("{GITHUB_API}/commits/main"),
            format!("{GITHUB_API}/git/trees/{FIRST}?recursive=1"),
        ]
    );

    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    assert_eq!(
        fs::read(teammate.join("data/survey/2020/field notes.txt"))?,
        NOTES
    );
    assert_eq!(fs::read(teammate.join("data/survey/big.bin"))?, big);
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| !target.starts_with("/api.github.com")),
        "the lock alone says what to download: {:?}",
        &routes.targets()[listed..]
    );
    Ok(())
}

/// a file git-lfs keeps comes as its bytes, never as the pointer git holds in its place, at add
/// and at a teammate's sync.
#[test]
fn a_git_lfs_file_in_a_github_folder_comes_whole() -> Result {
    let sandbox = Sandbox::new()?;
    let model = large().repeat(3);
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("models/model.bin", Held::Lfs(&model)),
            ("models/rows.csv", Held::File(ROWS)),
        ],
    )?)?;
    let link = "https://github.com/lab/survey/tree/main/models";
    let output = run(&sandbox, &routes, &["add", link, "--as", "models"])?;
    ok(&output)?;
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/models/model.bin"))?, model);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "{{ path = \"model.bin\", size = {}, sha256 = \"{}\"",
            model.len(),
            sha256_hex(&model)
        )),
        "{lock}"
    );
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/models/model.bin"))?, model);
    Ok(())
}

/// git's links, a submodule and a symbolic link, are left out by name in one warning, and the
/// folder's files are pinned.
#[test]
fn a_submodule_and_a_symbolic_link_are_named_and_left_out() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/latest.csv", Held::Link("rows.csv")),
            ("data/vendor", Held::Submodule),
        ],
    )?)?;
    let link = "https://github.com/lab/survey/tree/main/data";
    let output = run(
        &sandbox,
        &routes,
        &["--json", "add", link, "--as", "survey"],
    )?;
    ok(&output)?;
    let warnings = crate::support::warned(&json(&output)?);
    let named: Vec<&String> = warnings
        .iter()
        .filter(|warning| warning.contains("left out"))
        .collect();
    assert_eq!(
        named,
        [&format!(
            "{link} holds 1 symbolic link and 1 submodule, which git keeps as links rather than files, so they are left out: latest.csv and vendor"
        )],
        "{warnings:?}"
    );
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/survey/rows.csv"))?, ROWS);
    for left in ["latest.csv", "vendor"] {
        assert!(!work.join("data/survey").join(left).exists(), "{left}");
    }
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        !lock.contains("latest.csv") && !lock.contains("vendor"),
        "{lock}"
    );
    Ok(())
}

/// a push to the branch moves nothing until `lock --upgrade`, which pins the newer commit; a
/// folder pinned at a full commit stays there.
#[test]
fn an_upgrade_moves_a_branch_folder_to_its_newer_commit_and_a_commit_stays() -> Result {
    let sandbox = Sandbox::new()?;
    let mut served = github("main", FIRST, &[("data/rows.csv", Held::File(ROWS))])?;
    // the same commit named by its full id, as a person pins one
    served.push((
        format!("{GITHUB_API}/commits/{FIRST}"),
        reply("200 OK", &[], FIRST.as_bytes()),
    ));
    let routes = Routes::start(served)?;
    let branch = "https://github.com/lab/survey/tree/main/data";
    let commit = format!("https://github.com/lab/survey/tree/{FIRST}/data");
    ok(&run(&sandbox, &routes, &["add", branch, "--as", "survey"])?)?;
    ok(&run(
        &sandbox,
        &routes,
        &["add", &commit, "--as", "pinned"],
    )?)?;
    let lock = sandbox.work().join("data.lock");
    let resolved =
        |at: &str| format!("resolved = \"https://github.com/lab/survey/tree/{at}/data\"");

    routes.change(github(
        "main",
        SECOND,
        &[
            ("data/rows.csv", Held::File(NOTES)),
            ("data/2021.csv", Held::File(ROWS)),
        ],
    )?);
    for args in [&["lock"][..], &["sync"]] {
        let kept = run(&sandbox, &routes, args)?;
        ok(&kept)?;
        assert!(!text(&kept.stderr)?.contains("Upgraded"), "{args:?}");
    }
    let pinned = fs::read_to_string(&lock)?;
    assert!(!pinned.contains(SECOND), "{pinned}");
    assert_eq!(fs::read(sandbox.work().join("data/survey/rows.csv"))?, ROWS);

    let upgraded = run(&sandbox, &routes, &["lock", "--upgrade"])?;
    ok(&upgraded)?;
    let said = text(&upgraded.stderr)?;
    assert!(
        said.contains(&format!(
            "Upgraded survey · https://github.com/lab/survey/tree/{FIRST}/data → https://github.com/lab/survey/tree/{SECOND}/data · 1 file changed · 1 file added"
        )),
        "{said}"
    );
    assert!(!said.contains("Upgraded pinned"), "{said}");
    let pinned = fs::read_to_string(&lock)?;
    assert!(pinned.contains(&resolved(SECOND)), "{pinned}");
    assert!(pinned.contains(&resolved(FIRST)), "{pinned}");
    ok(&run(&sandbox, &routes, &["sync"])?)?;
    assert_eq!(
        fs::read(sandbox.work().join("data/survey/rows.csv"))?,
        NOTES
    );
    assert_eq!(fs::read(sandbox.work().join("data/pinned/rows.csv"))?, ROWS);
    Ok(())
}

/// github shows a private repository to a key that can read it, but serves its files only where
/// fetchloom sends no key, so the folder is refused before anything is pinned; without a key,
/// github shows no such repository at all.
#[test]
fn a_private_github_repository_is_refused_and_says_why() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (
            "/api.github.com/repos/lab/secret".to_owned(),
            answer(
                &serde_json::json!({"full_name": "lab/secret", "private": true, "visibility": "private"}),
            )?,
        ),
        (
            "/api.github.com/repos/lab/secret/commits/main".to_owned(),
            reply(
                "404 Not Found",
                &[("Content-Type", "application/json")],
                br#"{"message":"Not Found","status":"404"}"#,
            ),
        ),
    ])?;
    let link = "https://github.com/lab/secret/tree/main/data";
    let output = sandbox
        .command(&["--json", "add", link])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", "ghp_test_key_that_is_never_printed")
        .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "auth.required", "{error}");
    assert_eq!(
        error["message"],
        "github keeps lab/secret private, and fetchloom downloads the files of public repositories alone"
    );
    assert_eq!(
        error["help"],
        "github serves a private repository's files only with a key, and fetchloom sends a github key to api.github.com alone. clone the repository with git, then pin the folder from your clone with `fetchloom add <folder> --as <name>`"
    );
    assert_eq!(routes.targets(), ["/api.github.com/repos/lab/secret"]);
    assert!(!sandbox.work().join("data.toml").exists());

    let output = run(&sandbox, &routes, &["--json", "add", link])?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "github has no repository lab/secret, or it is private"
    );
    assert_eq!(
        error["help"],
        "check the name at https://github.com/lab/secret. fetchloom downloads the folders of public repositories alone, since it sends a github key to api.github.com and nowhere else"
    );
    Ok(())
}

/// gitlab's answers for lab/group/survey on `host` with `main` at the first commit: its folder
/// `data` listed, and each of `files` served raw at the commit, as gitlab serves a file git-lfs
/// keeps too. a listing in pages, which a stand-in cannot link, is the unit tests' to show.
fn gitlab(host: &str, files: &[(&str, &[u8])]) -> Result<Vec<(String, Vec<Part>)>> {
    let api = format!("/{host}/api/v4/projects/lab%2Fgroup%2Fsurvey");
    let entry = |path: &str| serde_json::json!({"id": "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391", "name": path.rsplit('/').next(), "type": "blob", "path": path, "mode": "100644"});
    let mut listed = vec![
        serde_json::json!({"id": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "name": "2020", "type": "tree", "path": "data/2020", "mode": "040000"}),
    ];
    listed.extend(files.iter().map(|(path, _)| entry(path)));
    let mut routes = vec![
        (
            format!("{api}/repository/commits/main"),
            answer(
                &serde_json::json!({"id": FIRST, "short_id": &FIRST[..8], "title": "add the survey"}),
            )?,
        ),
        (
            format!(
                "{api}/repository/tree?path=data&ref={FIRST}&recursive=true&per_page=100&pagination=keyset"
            ),
            answer(&serde_json::Value::from(listed))?,
        ),
    ];
    for (path, bytes) in files {
        routes.push((
            format!("/{host}/lab/group/survey/-/raw/{FIRST}/{}", encoded(path)),
            file(bytes, "\"git\""),
        ));
    }
    Ok(routes)
}

/// a folder on gitlab.com, pasted with the `ref_type` gitlab's own links carry, is pinned at the
/// commit its branch names from a listing in pages; a teammate's sync asks no api.
#[test]
fn a_gitlab_folder_pins_every_file_at_its_commit() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(gitlab(
        "gitlab.com",
        &[
            ("data/rows.csv", ROWS),
            ("data/2020/field notes.txt", NOTES),
        ],
    )?)?;
    let output = run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://gitlab.com/lab/group/survey/-/tree/main/data?ref_type=heads",
            "--as",
            "survey",
        ],
    )?;
    ok(&output)?;
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("ref = \"https://gitlab.com/lab/group/survey/-/tree/main/data\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "resolved = \"https://gitlab.com/lab/group/survey/-/tree/{FIRST}/data\""
        )),
        "{lock}"
    );
    assert!(
        lock.contains(&format!(
            "url = \"https://gitlab.com/lab/group/survey/-/raw/{FIRST}/data/2020/field%20notes.txt\""
        )),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/survey/rows.csv"))?, ROWS);
    assert_eq!(
        fs::read(work.join("data/survey/2020/field notes.txt"))?,
        NOTES
    );
    let listed = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/survey/rows.csv"))?, ROWS);
    assert!(
        routes.targets()[listed..]
            .iter()
            .all(|target| !target.contains("/api/v4/")),
        "the lock alone says what to download: {:?}",
        &routes.targets()[listed..]
    );
    Ok(())
}

/// a folder on another gitlab server, with a key saved for that server, asks that server alone,
/// for its listing and its files alike.
#[test]
fn a_folder_on_another_gitlab_server_asks_that_server_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(gitlab(
        "code.example.org",
        &[
            ("data/rows.csv", ROWS),
            ("data/2020/field notes.txt", NOTES),
        ],
    )?)?;
    let mut login = sandbox.command(&[
        "auth",
        "login",
        "code.example.org",
        "--token",
        "--with-token",
    ]);
    login.env("FETCHLOOM_TEST_HOSTS", &routes.url);
    ok(&crate::auth::with_stdin(
        login,
        "glpat-test-key-that-is-never-printed\n",
    )?)?;
    let link = "https://code.example.org/lab/group/survey/-/tree/main/data";
    let output = run(&sandbox, &routes, &["add", link, "--as", "survey"])?;
    ok(&output)?;
    assert_eq!(fs::read(sandbox.work().join("data/survey/rows.csv"))?, ROWS);
    let targets = routes.targets();
    assert!(
        targets
            .iter()
            .all(|target| target.starts_with("/code.example.org/")),
        "{targets:?}"
    );
    let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
    assert!(!said.contains("never-printed"), "{said}");
    Ok(())
}

/// the `resolved` and `files` data.lock pins for the dataset `name`.
fn pinned(lock: &toml::Value, name: &str) -> Result<(toml::Value, toml::Value)> {
    let dataset = lock["dataset"]
        .as_array()
        .and_then(|datasets| {
            datasets
                .iter()
                .find(|dataset| dataset["name"].as_str() == Some(name))
        })
        .ok_or("no such dataset in data.lock")?;
    Ok((dataset["resolved"].clone(), dataset["files"].clone()))
}

/// a link to a repository, as people share one, is its root at the default branch: pinned at
/// that branch's commit as its `tree/<branch>` link would be and named after the repository,
/// it moves on with the default branch at `lock --upgrade`; a teammate's sync asks no api.
#[test]
fn a_bare_github_link_pins_its_default_branch_and_follows_it() -> Result {
    let sandbox = Sandbox::new()?;
    let held = [
        ("README.md", Held::File(b"# survey\n")),
        ("data/rows.csv", Held::File(ROWS)),
    ];
    let mut served = github("HEAD", FIRST, &held)?;
    served.push((
        format!("{GITHUB_API}/commits/main"),
        reply("200 OK", &[], FIRST.as_bytes()),
    ));
    let routes = Routes::start(served)?;
    ok(&run(
        &sandbox,
        &routes,
        &["add", "https://github.com/lab/survey.git"],
    )?)?;
    let asked: Vec<String> = routes
        .targets()
        .into_iter()
        .filter(|target| target.starts_with("/api.github.com"))
        .collect();
    assert_eq!(
        asked,
        [
            format!("{GITHUB_API}/commits/HEAD"),
            format!("{GITHUB_API}/git/trees/{FIRST}?recursive=1"),
        ]
    );
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/survey/tree/main",
            "--as",
            "main",
        ],
    )?)?;
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.survey]\nref = \"https://github.com/lab/survey\"\n"),
        "{manifest}"
    );
    assert_eq!(fs::read(work.join("data/survey/data/rows.csv"))?, ROWS);
    let lock: toml::Value = toml::from_str(&fs::read_to_string(work.join("data.lock"))?)?;
    let (resolved, files) = pinned(&lock, "survey")?;
    assert_eq!(
        resolved.as_str(),
        Some(format!("https://github.com/lab/survey/tree/{FIRST}").as_str())
    );
    assert_eq!(
        (resolved, files),
        pinned(&lock, "main")?,
        "a bare link pins as its default branch's tree link does"
    );

    let asked = routes.targets().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    assert_eq!(fs::read(teammate.join("data/survey/data/rows.csv"))?, ROWS);
    assert!(
        routes.targets()[asked..]
            .iter()
            .all(|target| !target.starts_with("/api.github.com")),
        "the lock alone says what to download: {:?}",
        &routes.targets()[asked..]
    );

    routes.change(github(
        "HEAD",
        SECOND,
        &[
            ("README.md", Held::File(b"# survey\n")),
            ("data/rows.csv", Held::File(NOTES)),
        ],
    )?);
    let upgraded = run(&sandbox, &routes, &["lock", "--upgrade", "survey"])?;
    ok(&upgraded)?;
    let said = text(&upgraded.stderr)?;
    assert!(
        said.contains(&format!(
            "Upgraded survey · https://github.com/lab/survey/tree/{FIRST} → https://github.com/lab/survey/tree/{SECOND} · 1 file changed"
        )),
        "{said}"
    );
    Ok(())
}

/// a gitlab.com project's own link is its root at the default branch too, named after the
/// project.
#[test]
fn a_bare_gitlab_link_pins_its_default_branch() -> Result {
    let sandbox = Sandbox::new()?;
    let api = "/gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey";
    let entry = |path: &str| serde_json::json!({"id": "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391", "name": path, "type": "blob", "path": path, "mode": "100644"});
    let routes = Routes::start(vec![
        (
            format!("{api}/repository/commits/HEAD"),
            answer(&serde_json::json!({"id": FIRST, "short_id": &FIRST[..8]}))?,
        ),
        (
            format!(
                "{api}/repository/tree?ref={FIRST}&recursive=true&per_page=100&pagination=keyset"
            ),
            answer(&serde_json::json!([entry("rows.csv")]))?,
        ),
        (
            format!("/gitlab.com/lab/group/survey/-/raw/{FIRST}/rows.csv"),
            file(ROWS, "\"git\""),
        ),
    ])?;
    ok(&run(
        &sandbox,
        &routes,
        &["add", "https://gitlab.com/lab/group/survey.git/"],
    )?)?;
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.survey]\nref = \"https://gitlab.com/lab/group/survey\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "resolved = \"https://gitlab.com/lab/group/survey/-/tree/{FIRST}\""
        )),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/survey/rows.csv"))?, ROWS);
    Ok(())
}
