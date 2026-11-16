//! folders of github and gitlab repositories, pinned at the commit their branch names, through a
//! server on this machine that stands in for each host: the apis and the hosts that serve files.
#![cfg(debug_assertions)]

use std::fs;

use crate::support::{Part, Result, Routes, Sandbox, file, hex, json, ok, reply, sha256_hex, text};

const FIRST: &str = "0bda0ce801be0fa2f464ff845a9d5ceae99aad7d";
const SECOND: &str = "71e2436a092d714350de0fc409ca8a8714e7e78f";
const THIRD: &str = "c0ffee5a1d2e3f405162738495a6b7c8d9e0f1a2";
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

/// the id git gives a file of `bytes`: the SHA-1 of `blob`, its length, a zero byte and the bytes.
fn blob_id(bytes: &[u8]) -> String {
    use sha1::Digest as _;
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hex(&hasher.finalize())
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

/// github's answers for lab/survey with `branch` at `commit`, whose tree holds `held`.
fn github(branch: &str, commit: &str, held: &[(&str, Held)]) -> Result<Vec<(String, Vec<Part>)>> {
    on_github("lab/survey", branch, commit, held)
}

/// github's answers for the repository `name` with `branch` at `commit`, whose tree holds
/// `held`, each blob by its id: the commit the branch names, its whole tree in one answer, and
/// each file where github serves it. raw.githubusercontent.com serves what git holds, a pointer
/// for a file git-lfs keeps; github.com sends a file on to it, or one git-lfs keeps to
/// media.githubusercontent.com.
fn on_github(
    name: &str,
    branch: &str,
    commit: &str,
    held: &[(&str, Held)],
) -> Result<Vec<(String, Vec<Part>)>> {
    let mut tree: Vec<serde_json::Value> = folders(&held.iter().map(|(path, _)| *path).collect::<Vec<_>>())
        .into_iter()
        .map(|folder| serde_json::json!({"path": folder, "mode": "040000", "type": "tree", "sha": "4b825dc642cb6eb9a060e54bf8d69288fbee4904"}))
        .collect();
    let mut routes = Vec::new();
    for (path, kind) in held {
        let raw = format!(
            "/raw.githubusercontent.com/{name}/{commit}/{}",
            encoded(path)
        );
        let page = format!("/github.com/{name}/raw/{commit}/{}", encoded(path));
        let stored = match kind {
            Held::File(bytes) => {
                routes.push((
                    page,
                    moved(&format!(
                        "https://raw.githubusercontent.com/{name}/{commit}/{}",
                        encoded(path)
                    )),
                ));
                bytes.to_vec()
            }
            Held::Lfs(bytes) => {
                let media = format!(
                    "/media.githubusercontent.com/media/{name}/{commit}/{}",
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
            "sha": blob_id(&stored),
            "size": stored.len(),
        }));
        routes.push((raw, file(&stored, "\"git\"")));
    }
    let api = format!("/api.github.com/repos/{name}");
    routes.push((
        format!("{api}/commits/{branch}"),
        reply(
            "200 OK",
            &[("Content-Type", "application/vnd.github.sha; charset=utf-8")],
            commit.as_bytes(),
        ),
    ));
    routes.push((
        format!("{api}/git/trees/{commit}?recursive=1"),
        answer(&serde_json::json!({
            "sha": commit,
            "url": format!("https://api.github.com/repos/{name}/git/trees/{commit}"),
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
    let entry = |path: &str, bytes: &[u8]| serde_json::json!({"id": blob_id(bytes), "name": path.rsplit('/').next(), "type": "blob", "path": path, "mode": "100644"});
    let mut listed = vec![
        serde_json::json!({"id": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "name": "2020", "type": "tree", "path": "data/2020", "mode": "040000"}),
    ];
    listed.extend(files.iter().map(|(path, bytes)| entry(path, bytes)));
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

/// the requests since the first `before`, sorted, since a run downloads its files at once.
fn asked_since(routes: &Routes, before: usize) -> Vec<String> {
    let mut asked = routes.targets()[before..].to_vec();
    asked.sort();
    asked
}

/// the requests github serves a file of the folder `data` of lab/survey at `commit` with: a
/// small one is sent on from github.com, where git-lfs might keep it, a large one asked of
/// raw.githubusercontent.com alone.
fn served_from(commit: &str, path: &str, small: bool) -> Vec<String> {
    let raw = format!(
        "/raw.githubusercontent.com/lab/survey/{commit}/data/{}",
        encoded(path)
    );
    if small {
        vec![
            format!("/github.com/lab/survey/raw/{commit}/data/{}", encoded(path)),
            raw,
        ]
    } else {
        vec![raw]
    }
}

/// what data.lock pins of the dataset `name`: each file's path, size and SHA-256, sorted.
fn pins(work: &std::path::Path, name: &str) -> Result<Vec<(String, i64, String)>> {
    let lock: toml::Value = toml::from_str(&fs::read_to_string(work.join("data.lock"))?)?;
    let (_, files) = pinned(&lock, name)?;
    let mut pins = Vec::new();
    for file in files.as_array().ok_or("no files")? {
        pins.push((
            file["path"].as_str().ok_or("no path")?.to_owned(),
            file["size"].as_integer().ok_or("no size")?,
            file["sha256"].as_str().ok_or("no sha256")?.to_owned(),
        ));
    }
    pins.sort();
    Ok(pins)
}

/// the pins of `files`, each a path and its bytes, as [`pins`] gives them.
fn pinned_as(files: &[(&str, &[u8])]) -> Vec<(String, i64, String)> {
    let mut pins: Vec<(String, i64, String)> = files
        .iter()
        .map(|(path, bytes)| {
            (
                (*path).to_owned(),
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                sha256_hex(bytes),
            )
        })
        .collect();
    pins.sort();
    pins
}

/// `lock --upgrade` to a commit that changed one file of a folder downloads that file alone: the
/// others, moved or kept by git-lfs too, are blobs this cache downloaded before, so they come
/// from it and are pinned at the new commit, and a sync then places every new file unasked.
#[test]
fn an_upgrade_downloads_only_the_file_its_new_commit_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let changed: Vec<u8> = big.iter().rev().copied().collect();
    let model = large().repeat(3);
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/2020/field notes.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
            ("data/model.bin", Held::Lfs(&model)),
        ],
    )?)?;
    let link = "https://github.com/lab/survey/tree/main/data";
    ok(&run(&sandbox, &routes, &["add", link, "--as", "survey"])?)?;

    routes.change(github(
        "main",
        SECOND,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/2021/field notes.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&changed)),
            ("data/model.bin", Held::Lfs(&model)),
        ],
    )?);
    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["lock", "--upgrade"])?)?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
    ];
    expected.extend(served_from(SECOND, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    let work = sandbox.work();
    assert_eq!(
        pins(&work, "survey")?,
        pinned_as(&[
            ("rows.csv", ROWS),
            ("2021/field notes.txt", NOTES),
            ("big.bin", &changed),
            ("model.bin", &model),
        ])
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!(
            "url = \"https://github.com/lab/survey/raw/{SECOND}/data/2021/field%20notes.txt\""
        )),
        "a file from the cache is pinned at its new commit: {lock}"
    );

    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["sync"])?)?;
    assert_eq!(asked_since(&routes, before), Vec::<String>::new());
    assert_eq!(fs::read(work.join("data/survey/big.bin"))?, changed);
    assert_eq!(
        fs::read(work.join("data/survey/2021/field notes.txt"))?,
        NOTES
    );
    assert_eq!(fs::read(work.join("data/survey/model.bin"))?, model);
    Ok(())
}

/// a fork holds the very blobs of its repository, by the same ids, yet this cache downloaded
/// them from the fork alone: the repository's own folder downloads each of its files, since one
/// repository's ids never vouch for another's bytes, and then its later commit downloads none.
#[test]
fn blobs_of_another_repository_never_stand_for_a_folders_files() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let held = [
        ("data/rows.csv", Held::File(ROWS)),
        ("data/big.bin", Held::File(&big)),
    ];
    let mut served = on_github("lab/fork", "main", FIRST, &held)?;
    served.extend(github("main", FIRST, &held)?);
    served.extend(github(SECOND, SECOND, &held)?);
    let routes = Routes::start(served)?;
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/fork/tree/main/data",
            "--as",
            "fork",
        ],
    )?)?;

    let before = routes.targets().len();
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ],
    )?)?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{FIRST}?recursive=1"),
    ];
    expected.extend(served_from(FIRST, "rows.csv", true));
    expected.extend(served_from(FIRST, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);

    let before = routes.targets().len();
    let later = format!("https://github.com/lab/survey/tree/{SECOND}/data");
    ok(&run(&sandbox, &routes, &["add", &later, "--as", "later"])?)?;
    assert_eq!(
        asked_since(&routes, before),
        [
            format!("{GITHUB_API}/commits/{SECOND}"),
            format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
        ],
        "the repository's own blobs come from the cache"
    );
    assert_eq!(
        pins(&sandbox.work(), "later")?,
        pinned_as(&[("rows.csv", ROWS), ("big.bin", &big)])
    );
    Ok(())
}

/// a blob's record stands for nothing once gc frees its bytes: with the folder removed and the
/// cache swept, the same blobs at a later commit download again.
#[test]
fn a_blob_whose_bytes_gc_freed_downloads_again() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let held = [
        ("data/rows.csv", Held::File(ROWS)),
        ("data/big.bin", Held::File(&big)),
    ];
    let mut served = github("main", FIRST, &held)?;
    served.extend(github(SECOND, SECOND, &held)?);
    let routes = Routes::start(served)?;
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ],
    )?)?;
    let later = format!("https://github.com/lab/survey/tree/{SECOND}/data");
    let listed = vec![
        format!("{GITHUB_API}/commits/{SECOND}"),
        format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
    ];
    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["add", &later, "--as", "later"])?)?;
    assert_eq!(
        asked_since(&routes, before),
        listed,
        "held, they come from the cache"
    );

    for name in ["survey", "later"] {
        ok(&run(&sandbox, &routes, &["remove", name])?)?;
    }
    ok(&run(&sandbox, &routes, &["cache", "gc", "--now"])?)?;
    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["add", &later, "--as", "again"])?)?;
    let mut expected = listed;
    expected.extend(served_from(SECOND, "rows.csv", true));
    expected.extend(served_from(SECOND, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    assert_eq!(fs::read(sandbox.work().join("data/again/big.bin"))?, big);
    Ok(())
}

/// a teammate's cache never downloaded the commit data.lock pins, so their first upgrade
/// downloads each file of the new commit once, and the next one only what that commit changed.
#[test]
fn a_cache_that_never_held_the_old_commit_downloads_each_file_once() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let changed: Vec<u8> = big.iter().rev().copied().collect();
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/big.bin", Held::File(&big)),
        ],
    )?)?;
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ],
    )?)?;
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let upgrade = || -> Result<()> {
        ok(&sandbox
            .command_in(&teammate, &["lock", "--upgrade"])
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?)
    };

    routes.change(github(
        "main",
        SECOND,
        &[
            ("data/rows.csv", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
        ],
    )?);
    let before = routes.targets().len();
    upgrade()?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
    ];
    expected.extend(served_from(SECOND, "rows.csv", true));
    expected.extend(served_from(SECOND, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);

    routes.change(github(
        "main",
        THIRD,
        &[
            ("data/rows.csv", Held::File(NOTES)),
            ("data/big.bin", Held::File(&changed)),
        ],
    )?);
    let before = routes.targets().len();
    upgrade()?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{THIRD}?recursive=1"),
    ];
    expected.extend(served_from(THIRD, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    assert_eq!(
        pins(&teammate, "survey")?,
        pinned_as(&[("rows.csv", NOTES), ("big.bin", &changed)])
    );
    Ok(())
}

/// rewrites each record in the cache at `cache` that names the bytes `bytes` as `rewrite` makes
/// its json, keeping its check, so the record is damaged, or making it again when `checked`, as
/// someone else sharing the cache could write a record of their own.
fn rewrite_records(
    cache: &std::path::Path,
    bytes: &[u8],
    rewrite: impl Fn(&str) -> String,
    checked: bool,
) -> Result {
    let named = format!("\"sha256\":\"{}\"", sha256_hex(bytes));
    let mut rewritten = 0;
    for entry in fs::read_dir(cache.join("urls"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "log") {
            continue;
        }
        let log = fs::read_to_string(&path)?;
        let mut written = String::with_capacity(log.len());
        for line in log.split_inclusive('\n') {
            match line.split_once(' ') {
                Some((check, json)) if json.contains(&named) => {
                    let json = rewrite(json);
                    let check = if checked {
                        sha256_hex(json.trim_end().as_bytes())[..16].to_owned()
                    } else {
                        check.to_owned()
                    };
                    written.push_str(&check);
                    written.push(' ');
                    written.push_str(&json);
                    rewritten += 1;
                }
                _ => written.push_str(line),
            }
        }
        fs::write(&path, written)?;
    }
    assert!(rewritten > 0, "no record names those bytes");
    Ok(())
}

/// a record that names other bytes than its blob's never places them: a damaged one is no
/// record, and the bytes a foreign one names, which the cache holds, do not have the blob's id,
/// so each of their files downloads and is pinned by its own bytes, while the rest come from
/// the cache.
#[test]
fn a_damaged_or_foreign_record_never_places_other_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let other: Vec<u8> = big.iter().map(|byte| byte ^ 1).collect();
    let held = [
        ("data/rows.csv", Held::File(ROWS)),
        ("data/notes.txt", Held::File(NOTES)),
        ("data/big.bin", Held::File(&big)),
        ("data/other.bin", Held::File(&other)),
    ];
    let routes = Routes::start(github("main", FIRST, &held)?)?;
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ],
    )?)?;
    let cache = sandbox.cache();
    let (big_hex, other_hex) = (sha256_hex(&big), sha256_hex(&other));
    rewrite_records(
        &cache,
        &big,
        |json| json.replace(&big_hex, &other_hex),
        true,
    )?;
    let notes_hex = sha256_hex(NOTES);
    let flipped = format!(
        "{}{}",
        &notes_hex[..63],
        if notes_hex.ends_with('0') { '1' } else { '0' }
    );
    rewrite_records(
        &cache,
        NOTES,
        |json| json.replace(&notes_hex, &flipped),
        false,
    )?;

    routes.change(github("main", SECOND, &held)?);
    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["lock", "--upgrade"])?)?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
    ];
    expected.extend(served_from(SECOND, "notes.txt", true));
    expected.extend(served_from(SECOND, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    let work = sandbox.work();
    assert_eq!(
        pins(&work, "survey")?,
        pinned_as(&[
            ("rows.csv", ROWS),
            ("notes.txt", NOTES),
            ("big.bin", &big),
            ("other.bin", &other),
        ])
    );
    ok(&run(&sandbox, &routes, &["sync"])?)?;
    assert_eq!(fs::read(work.join("data/survey/big.bin"))?, big);
    assert_eq!(fs::read(work.join("data/survey/other.bin"))?, other);
    Ok(())
}
