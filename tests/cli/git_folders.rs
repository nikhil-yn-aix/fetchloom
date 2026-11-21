//! folders of github and gitlab repositories, pinned at the commit their branch names, through a
//! server on this machine that stands in for each host: the apis and the hosts that serve files.
#![cfg(debug_assertions)]

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

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
/// `held`, each blob by its id: the commit the branch names, its whole tree and each folder's own
/// in one answer, and each file where github serves it. raw.githubusercontent.com serves what git holds, a pointer
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
    // each folder's own tree, as `git/trees/<commit>:<folder>` names it, its entries inside it
    let paths: Vec<&str> = held.iter().map(|(path, _)| *path).collect();
    for folder in folders(&paths) {
        let inside: Vec<serde_json::Value> = tree
            .iter()
            .filter_map(|entry| {
                let path = entry["path"]
                    .as_str()?
                    .strip_prefix(&format!("{folder}/"))?;
                let mut entry = entry.clone();
                entry["path"] = path.into();
                Some(entry)
            })
            .collect();
        routes.push((
            format!("{api}/git/trees/{commit}:{}?recursive=1", encoded(&folder)),
            answer(&serde_json::json!({"sha": folder, "tree": inside, "truncated": false}))?,
        ));
    }
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

/// `target` with a folder's own tree, which a listing of a folder below the top asks for, named
/// as the whole tree it stands for: either is a listing's one tree request.
fn one_tree(target: &str) -> String {
    let Some((api, rest)) = target.split_once("/git/trees/") else {
        return target.to_owned();
    };
    match (rest.split_once(':'), rest.split_once('?')) {
        (Some((commit, _)), Some((_, query))) => format!("{api}/git/trees/{commit}?{query}"),
        _ => target.to_owned(),
    }
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

/// a dry run states the size of the files the tree states one for, beside the count of those it
/// states none for, whose size a git-lfs pointer's would misstate.
#[test]
fn a_dry_run_states_the_sizes_the_tree_states() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = Routes::start(github(
        "main",
        FIRST,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/notes.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
        ],
    )?)?;
    let link = "https://github.com/lab/survey/tree/main/data";
    let output = run(
        &sandbox,
        &routes,
        &["add", link, "--as", "survey", "--dry-run"],
    )?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Would pin survey, new in data.lock: https://github.com/lab/survey/tree/{}/data · 3 files · 2.0 KiB and 2 of unknown size\n",
            &FIRST[..7]
        )),
        "{stderr}"
    );
    Ok(())
}

/// a repository's own link leaves out git's own files, which hold no data: `.gitignore` and
/// `.gitattributes` at any depth, and at its top `.gitmodules`, `.github/`, `.gitlab/` and
/// `.gitlab-ci.yml`, which a folder inside it holds as data; the dry run names what it leaves out,
/// and a select pattern keeps one.
#[test]
fn a_repository_link_leaves_out_gits_own_files() -> Result {
    let sandbox = Sandbox::new()?;
    let held = [
        (".github/workflows/scrape.yml", Held::File(b"on: push\n")),
        (".gitignore", Held::File(b"*.tmp\n")),
        (".gitlab-ci.yml", Held::File(b"test:\n")),
        (".gitmodules", Held::File(b"[submodule]\n")),
        ("data/.gitignore", Held::File(b"*\n")),
        ("data/.github/notes.yml", Held::File(NOTES)),
        ("data/rows.csv", Held::File(ROWS)),
        ("requirements.txt", Held::File(b"pandas\n")),
    ];
    let routes = Routes::start(github("HEAD", FIRST, &held)?)?;
    let link = "https://github.com/lab/survey";
    let dry = run(&sandbox, &routes, &["add", link, "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "hint: git's own files hold no data, so these are left out unless a select pattern keeps them: .github/workflows/scrape.yml, .gitignore, .gitlab-ci.yml, .gitmodules and data/.gitignore\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains(" · 3 of 8 files"), "{stderr}");
    ok(&run(&sandbox, &routes, &["add", link])?)?;
    assert_eq!(
        placed(&sandbox.work().join("data/survey"))?,
        [
            ".gitignore",
            "data/.github/notes.yml",
            "data/rows.csv",
            "requirements.txt"
        ]
    );
    let kept = sandbox.folder("kept")?;
    let output = sandbox
        .command_in(&kept, &["add", link, "--select", ".gitignore"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    ok(&output)?;
    assert_eq!(placed(&kept.join("data/survey"))?, [".gitignore"]);
    Ok(())
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
        [".gitignore", "2020/field notes.txt", "big.bin", "rows.csv"],
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
            format!("{GITHUB_API}/git/trees/{FIRST}:data?recursive=1"),
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

const SECRET_API: &str = "/api.github.com/repos/lab/secret";
/// a github key, which reads lab/secret.
const KEY: &str = "ghp_test_key_that_is_never_printed";

/// the line of a request's head, lowercased, that carries [`KEY`].
fn key_line() -> String {
    format!("authorization: bearer {}", KEY.to_ascii_lowercase())
}

/// whether the request `head`, lowercased, holds the header line `line`.
fn holds(head: &str, line: &str) -> bool {
    head.lines().any(|held| held.trim() == line)
}

/// the target of the request `head`.
fn target(head: &str) -> &str {
    head.split(' ').nth(1).unwrap_or_default()
}

/// github's answers for the private repository lab/secret with `main` at the first commit, whose
/// tree holds `held`, all at its api, which answers them only to a request with [`KEY`]: the
/// repository, which says it is private, the commit, the tree, and each file's blob, which git
/// holds, a pointer for a file git-lfs keeps. github.com and its storage serve nothing of it.
fn secret(held: &[(&str, Held)]) -> Result<Routes> {
    let routes = Routes::start(secret_at(FIRST, held)?)?;
    routes.keyed(SECRET_API, &key_line());
    Ok(routes)
}

/// [`secret`]'s answers with `main` at `commit`.
fn secret_at(commit: &str, held: &[(&str, Held)]) -> Result<Vec<(String, Vec<Part>)>> {
    let paths: Vec<&str> = held.iter().map(|(path, _)| *path).collect();
    let mut tree: Vec<serde_json::Value> = folders(&paths)
        .into_iter()
        .map(|folder| serde_json::json!({"path": folder, "mode": "040000", "type": "tree", "sha": "4b825dc642cb6eb9a060e54bf8d69288fbee4904"}))
        .collect();
    let mut routes = Vec::new();
    for (path, kind) in held {
        let stored = match kind {
            Held::File(bytes) => bytes.to_vec(),
            Held::Lfs(bytes) => pointer(bytes),
            Held::Link(_) | Held::Submodule => continue,
        };
        let id = blob_id(&stored);
        tree.push(serde_json::json!({"path": path, "mode": "100644", "type": "blob", "sha": id, "size": stored.len()}));
        routes.push((
            format!("{SECRET_API}/git/blobs/{id}"),
            file(&stored, "W/\"blob\""),
        ));
    }
    routes.push((
        SECRET_API.to_owned(),
        answer(
            &serde_json::json!({"full_name": "lab/secret", "private": true, "visibility": "private"}),
        )?,
    ));
    routes.push((
        format!("{SECRET_API}/commits/main"),
        reply(
            "200 OK",
            &[("Content-Type", "application/vnd.github.sha; charset=utf-8")],
            commit.as_bytes(),
        ),
    ));
    routes.push((
        format!("{SECRET_API}/git/trees/{commit}?recursive=1"),
        answer(&serde_json::json!({"sha": commit, "tree": tree, "truncated": false}))?,
    ));
    Ok(routes)
}

/// the files of lab/secret's folder `data`, at their paths inside it.
fn secret_files(big: &[u8]) -> [(&'static str, Vec<u8>); 3] {
    [
        ("rows.csv", ROWS.to_vec()),
        ("2020/field notes.txt", NOTES.to_vec()),
        ("big.bin", big.to_vec()),
    ]
}

/// lab/secret, whose folder `data` holds [`secret_files`], added in the sandbox's work folder as
/// `secret` with [`KEY`].
fn added_secret(sandbox: &Sandbox, big: &[u8]) -> Result<Routes> {
    let files = secret_files(big);
    let held: Vec<(String, Held)> = files
        .iter()
        .map(|(path, bytes)| (format!("data/{path}"), Held::File(bytes)))
        .collect();
    let held: Vec<(&str, Held)> = held
        .iter()
        .map(|(path, kind)| (path.as_str(), *kind))
        .collect();
    let routes = secret(&held)?;
    let output = sandbox
        .command(&[
            "add",
            "https://github.com/lab/secret/tree/main/data",
            "--as",
            "secret",
        ])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", KEY)
        .output()?;
    ok(&output)?;
    let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
    assert!(!said.contains(KEY), "{said}");
    Ok(routes)
}

/// a key that reads a private repository pins its folder's files at github's api, each by its
/// blob and in its raw bytes, with the access github states, and the key goes to the api alone; a
/// key that cannot read the repository is told so.
#[test]
fn a_private_github_folder_pins_at_the_api_and_the_key_goes_there_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = added_secret(&sandbox, &big)?;
    let work = sandbox.work();
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("access = \"login\"\n"), "{lock}");
    for (path, bytes) in secret_files(&big) {
        assert!(
            lock.contains(&format!(
                "{{ path = \"{path}\", size = {}, sha256 = \"{}\", url = \"https://api.github.com/repos/lab/secret/git/blobs/{}\" }}",
                bytes.len(),
                sha256_hex(&bytes),
                blob_id(&bytes)
            )),
            "{path}: {lock}"
        );
        assert_eq!(
            fs::read(work.join("data/secret").join(path))?,
            bytes,
            "{path}"
        );
    }
    let heads = routes.requests();
    for head in &heads {
        assert!(target(head).starts_with(SECRET_API), "{head}");
        assert!(holds(head, &key_line()), "{head}");
    }
    let blobs: Vec<&String> = heads
        .iter()
        .filter(|head| target(head).contains("/git/blobs/"))
        .collect();
    assert_eq!(blobs.len(), 3, "{heads:?}");
    for head in blobs {
        assert!(
            holds(head, "accept: application/vnd.github.raw+json"),
            "{head}"
        );
    }

    let other = sandbox.folder("other")?;
    let output = sandbox
        .command_in(
            &other,
            &[
                "--json",
                "add",
                "https://github.com/lab/secret/tree/main/data",
            ],
        )
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", "ghp_another_key_that_cannot_read_it")
        .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    assert_eq!(
        json(&output)?["errors"][0]["message"],
        "github has no repository lab/secret, or this key cannot see it"
    );

    let saved = sandbox.folder("saved")?;
    let login = &["auth", "login", "github", "--with-token", "--offline"];
    ok(&crate::auth::with_stdin(
        sandbox.command_in(&saved, login),
        "ghp_another_key_that_cannot_read_it\n",
    )?)?;
    let output = sandbox
        .command_in(
            &saved,
            &[
                "--json",
                "add",
                "https://github.com/lab/secret/tree/main/data",
            ],
        )
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["help"],
        "check the name at https://github.com/lab/secret. if it is right, the key has no access to it: `fetchloom auth` shows where it comes from and whose it is"
    );
    assert_eq!(error["fix"], "fetchloom auth");
    Ok(())
}

/// a teammate without a github key asks github's api for a private repository's files without
/// one, and github's 404 is told as the key they take, with nothing asked again; one whose key
/// cannot read the repository is told that; with the key, the files come from github's api.
#[test]
fn a_teammate_without_the_key_is_told_which_when_github_refuses() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = added_secret(&sandbox, &big)?;
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let sync = |cache: &str, key: Option<&str>| -> Result<std::process::Output> {
        let mut command = sandbox.command_in(&teammate, &["sync", "--locked"]);
        command
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", sandbox.folder(cache)?);
        if let Some(key) = key {
            command.env("GH_TOKEN", key);
        }
        Ok(command.output()?)
    };
    let asked = routes.requests().len();
    let refused = sync("keyless-cache", None)?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(40), "{stderr}");
    assert!(
        stderr.contains(
            "error: github serves the files of the private repository lab/secret only to a key that can read it\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: set GH_TOKEN or GITHUB_TOKEN to a github token that can read it, or sign in with `fetchloom auth login github`"),
        "{stderr}"
    );
    assert!(!stderr.contains("404"), "{stderr}");
    let refusing = &routes.requests()[asked..];
    assert!(!refusing.is_empty(), "github answers for itself");
    let mut targets: Vec<&str> = refusing.iter().map(|head| target(head)).collect();
    targets.sort_unstable();
    targets.dedup();
    assert_eq!(targets.len(), refusing.len(), "{refusing:?}");
    for head in refusing {
        assert!(
            target(head).starts_with(&format!("{SECRET_API}/git/blobs/")),
            "{head}"
        );
        assert!(!head.contains("authorization:"), "{head}");
    }

    let unread = sync("unread-cache", Some("ghp_another_key_that_cannot_read_it"))?;
    let stderr = text(&unread.stderr)?;
    assert_eq!(unread.status.code(), Some(40), "{stderr}");
    assert!(
        stderr.contains("error: github has no repository lab/secret, or this key cannot see it\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("the key in GH_TOKEN went with the request\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: check the name at https://github.com/lab/secret. a private repository's files need a key that can read it: if the key in GH_TOKEN is old or wrong, set GH_TOKEN to a current one from https://github.com/settings/tokens\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("if it moved"), "{stderr}");

    let asked = routes.requests().len();
    ok(&sync("teammate-cache", Some(KEY))?)?;
    assert_eq!(fs::read(teammate.join("data/secret/big.bin"))?, big);
    let synced = &routes.requests()[asked..];
    assert_eq!(synced.len(), 3, "{synced:?}");
    for head in synced {
        assert!(
            target(head).starts_with(&format!("{SECRET_API}/git/blobs/")),
            "{head}"
        );
        assert!(holds(head, &key_line()), "{head}");
    }
    Ok(())
}

/// a repository answers alike for all its files, so a teammate without a key asks github for one
/// file of a private repository's twelve, and its refusal stands for the rest, unasked: each
/// request without a key spends one of the 60 an hour github allows an address.
#[test]
fn a_teammate_without_the_key_asks_github_once_a_repository() -> Result {
    let sandbox = Sandbox::new()?;
    let files: Vec<(String, Vec<u8>)> = (0..12)
        .map(|part| {
            (
                format!("data/part-{part:02}.csv"),
                format!("part,{part}\n").into_bytes(),
            )
        })
        .collect();
    let held: Vec<(&str, Held)> = files
        .iter()
        .map(|(path, bytes)| (path.as_str(), Held::File(bytes)))
        .collect();
    let routes = secret(&held)?;
    let link = "https://github.com/lab/secret/tree/main/data";
    ok(&sandbox
        .command(&["add", link, "--as", "secret"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", KEY)
        .output()?)?;
    let asked = routes.requests().len();
    let (_, synced) = teammate_sync(&sandbox, &routes)?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(40), "{stderr}");
    assert_eq!(stderr.matches("error: ").count(), 1, "{stderr}");
    assert!(
        stderr.contains(
            "error: github serves the files of the private repository lab/secret only to a key that can read it\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("the same for all 12 files\n"), "{stderr}");
    let refusing = &routes.requests()[asked..];
    assert_eq!(refusing.len(), 1, "one request a repository: {refusing:?}");
    assert!(
        target(&refusing[0]).starts_with(&format!("{SECRET_API}/git/blobs/")),
        "{refusing:?}"
    );
    assert!(!refusing[0].contains("authorization:"), "{refusing:?}");
    Ok(())
}

/// a pin made while its repository was private stays good once github shows the repository to
/// anyone: a run without a key asks github's api for each file without one, and github's limit on
/// such requests, 60 an hour, is told as the key that lifts it.
#[test]
fn a_private_pin_downloads_without_a_key_once_its_repository_is_public() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = added_secret(&sandbox, &big)?;
    routes.unkeyed(SECRET_API);
    let asked = routes.requests().len();
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    for (path, bytes) in secret_files(&big) {
        assert_eq!(
            fs::read(teammate.join("data/secret").join(path))?,
            bytes,
            "{path}"
        );
    }
    let public = &routes.requests()[asked..];
    assert_eq!(public.len(), 3, "{public:?}");
    for head in public {
        assert!(
            target(head).starts_with(&format!("{SECRET_API}/git/blobs/")),
            "{head}"
        );
        assert!(!head.contains("authorization:"), "{head}");
    }

    let reset = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 3600;
    let used_up = || {
        reply(
            "403 Forbidden",
            &[
                ("Content-Type", "application/json"),
                ("x-ratelimit-limit", "60"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &reset.to_string()),
            ],
            br#"{"message":"API rate limit exceeded for 203.0.113.7."}"#,
        )
    };
    routes.change(
        secret_files(&big)
            .iter()
            .map(|(_, bytes)| {
                (
                    format!("{SECRET_API}/git/blobs/{}", blob_id(bytes)),
                    used_up(),
                )
            })
            .collect(),
    );
    let reader = sandbox.folder("reader")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), reader.join(name))?;
    }
    let asked = routes.requests().len();
    let limited = sandbox
        .command_in(&reader, &["sync", "--locked"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("FETCHLOOM_CACHE", sandbox.folder("reader-cache")?)
        .output()?;
    let stderr = text(&limited.stderr)?;
    assert_eq!(limited.status.code(), Some(20), "{stderr}");
    assert!(
        stderr.contains(
            "error: github allows 60 requests an hour without a key, and they are used up\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("help: set GH_TOKEN or GITHUB_TOKEN to a github token, or sign in with `fetchloom auth login github`, which raises the limit to 5,000 an hour"),
        "{stderr}"
    );
    for head in &routes.requests()[asked..] {
        assert!(!head.contains("authorization:"), "{head}");
    }
    Ok(())
}

/// a private repository's release pins its assets at github's api by their ids; the api sends a
/// download on to github's storage, and the key never goes along that redirect.
#[test]
fn a_private_release_downloads_through_the_api_and_its_redirect_carries_no_key() -> Result {
    let sandbox = Sandbox::new()?;
    let storage = "/release-assets.githubusercontent.com/github-production-release-asset/42?sig=signed&se=2099";
    let routes = Routes::start(vec![
        (
            SECRET_API.to_owned(),
            answer(&serde_json::json!({"full_name": "lab/secret", "private": true}))?,
        ),
        (
            format!("{SECRET_API}/releases/tags/v1"),
            answer(&serde_json::json!({
                "tag_name": "v1",
                "name": "v1",
                "assets": [{
                    "id": 42,
                    "name": "rows.csv",
                    "size": ROWS.len(),
                    "state": "uploaded",
                    "digest": format!("sha256:{}", sha256_hex(ROWS)),
                    "browser_download_url": "https://github.com/lab/secret/releases/download/v1/rows.csv",
                }],
            }))?,
        ),
        (
            format!("{SECRET_API}/releases/assets/42"),
            moved(&format!("https:/{storage}")),
        ),
        (storage.to_owned(), file(ROWS, "\"asset\"")),
    ])?;
    routes.keyed(SECRET_API, &key_line());
    let output = sandbox
        .command(&["add", "github:lab/secret@v1", "--as", "release"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", KEY)
        .output()?;
    ok(&output)?;
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/release/rows.csv"))?, ROWS);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://api.github.com/repos/lab/secret/releases/assets/42\""),
        "{lock}"
    );
    assert!(lock.contains("access = \"login\"\n"), "{lock}");
    assert!(
        !lock.contains("signed") && !lock.contains("release-assets"),
        "{lock}"
    );
    let heads = routes.requests();
    let asset = heads
        .iter()
        .find(|head| target(head) == format!("{SECRET_API}/releases/assets/42"))
        .ok_or("the asset was never asked for")?;
    assert!(holds(asset, &key_line()), "{asset}");
    assert!(holds(asset, "accept: application/octet-stream"), "{asset}");
    let stored = heads
        .iter()
        .find(|head| target(head) == storage)
        .ok_or("the storage was never asked")?;
    assert!(!stored.contains("authorization"), "{stored}");
    for head in &heads {
        assert_eq!(
            holds(head, &key_line()),
            target(head).starts_with("/api.github.com/"),
            "{head}"
        );
    }
    Ok(())
}

/// github serves the files git-lfs keeps for a private repository only through github.com, where
/// fetchloom sends no github key, so such a file is named with the reason and left out by its
/// pattern, and the folder's other files pin.
#[test]
fn a_git_lfs_file_of_a_private_repository_is_refused_by_name() -> Result {
    let sandbox = Sandbox::new()?;
    let model = large().repeat(3);
    let routes = secret(&[
        ("data/rows.csv", Held::File(ROWS)),
        ("data/model.bin", Held::Lfs(&model)),
    ])?;
    let link = "https://github.com/lab/secret/tree/main/data";
    let add = |extra: &[&str]| {
        let args = [&["--json", "add", link, "--as", "secret"][..], extra].concat();
        sandbox
            .command(&args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("GH_TOKEN", KEY)
            .output()
    };
    let output = add(&[])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["help"],
        "git-lfs keeps model.bin, and github serves the files git-lfs keeps for a private repository only through github.com, where fetchloom sends no github key. the other files are in the cache. to add secret without it, run the add again with --exclude \"model.bin\""
    );
    ok(&add(&["--exclude", "model.bin"])?)?;
    assert_eq!(fs::read(sandbox.work().join("data/secret/rows.csv"))?, ROWS);
    assert!(!sandbox.work().join("data/secret/model.bin").exists());
    assert!(
        routes
            .requests()
            .iter()
            .all(|head| target(head).starts_with(SECRET_API)),
        "{:?}",
        routes.requests()
    );
    Ok(())
}

/// an upgrade that meets a new file git-lfs keeps for a private repository says how to upgrade
/// without it, as an add does: the pattern to add to the dataset's exclude in data.toml, which an
/// upgrade then follows.
#[test]
fn an_upgrade_meeting_a_private_lfs_file_names_its_exclude() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = added_secret(&sandbox, &big)?;
    let model = large().repeat(3);
    let files = secret_files(&big);
    let mut held: Vec<(String, Held)> = files
        .iter()
        .map(|(path, bytes)| (format!("data/{path}"), Held::File(bytes)))
        .collect();
    held.push(("data/weights/model.bin".to_owned(), Held::Lfs(&model)));
    let held: Vec<(&str, Held)> = held
        .iter()
        .map(|(path, kind)| (path.as_str(), *kind))
        .collect();
    routes.change(secret_at(SECOND, &held)?);
    let upgrade = || {
        sandbox
            .command(&["lock", "--upgrade", "secret"])
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("GH_TOKEN", KEY)
            .output()
    };
    let refused = upgrade()?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("help: git-lfs keeps weights/model.bin, and github serves the files git-lfs keeps for a private repository only through github.com, where fetchloom sends no github key. to upgrade secret without it, add \"weights/model.bin\" to exclude in datasets.secret in data.toml, then run `fetchloom lock --upgrade secret` again\n"),
        "{stderr}"
    );

    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    let entry = "[datasets.secret]\n";
    assert!(written.contains(entry), "{written}");
    fs::write(
        &manifest,
        written.replace(
            entry,
            &format!("{entry}exclude = [\"weights/model.bin\"]\n"),
        ),
    )?;
    ok(&upgrade()?)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(SECOND), "{lock}");
    assert!(!lock.contains("model.bin"), "{lock}");
    Ok(())
}

/// a key that reads a public repository changes nothing about its files: they come from github's
/// storage, which takes no key, as without one, and the access is public, as github says.
#[test]
fn a_public_repository_keeps_its_raw_links_with_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let mut served = github(
        "main",
        FIRST,
        &[
            ("data/rows.csv", Held::File(ROWS)),
            ("data/big.bin", Held::File(&big)),
        ],
    )?;
    served.push((
        GITHUB_API.to_owned(),
        answer(&serde_json::json!({"full_name": "lab/survey", "private": false}))?,
    ));
    let routes = Routes::start(served)?;
    let output = sandbox
        .command(&[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GH_TOKEN", KEY)
        .output()?;
    ok(&output)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains("access = \"public\"\n"), "{lock}");
    assert!(
        lock.contains(&format!(
            "url = \"https://github.com/lab/survey/raw/{FIRST}/data/rows.csv\""
        )),
        "{lock}"
    );
    assert!(
        lock.contains(&format!(
            "url = \"https://raw.githubusercontent.com/lab/survey/{FIRST}/data/big.bin\""
        )),
        "{lock}"
    );
    assert!(!lock.contains("api.github.com"), "{lock}");
    for head in routes.requests() {
        assert_eq!(
            holds(&head, &key_line()),
            target(&head).starts_with("/api.github.com/"),
            "{head}"
        );
    }
    Ok(())
}

/// gitlab's answers for lab/group/survey on `host` with `main` at the first commit: its folder
/// `data` listed, and each of `files` served raw at the commit, as gitlab serves a file git-lfs
/// keeps too. a listing in pages, which a stand-in cannot link, is the unit tests' to show.
fn gitlab(host: &str, files: &[(&str, &[u8])]) -> Result<Vec<(String, Vec<Part>)>> {
    gitlab_at(host, FIRST, files)
}

/// [`gitlab`]'s answers with `main` at `commit`.
fn gitlab_at(
    host: &str,
    commit: &str,
    files: &[(&str, &[u8])],
) -> Result<Vec<(String, Vec<Part>)>> {
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
                &serde_json::json!({"id": commit, "short_id": &commit[..8], "title": "add the survey"}),
            )?,
        ),
        (
            format!(
                "{api}/repository/tree?path=data&ref={commit}&recursive=true&per_page=100&pagination=keyset"
            ),
            answer(&serde_json::Value::from(listed))?,
        ),
    ];
    for (path, bytes) in files {
        routes.push((
            format!("/{host}/lab/group/survey/-/raw/{commit}/{}", encoded(path)),
            file(bytes, "\"git\""),
        ));
    }
    Ok(routes)
}

/// a file a folder's tree lists may have no bytes on its server, as a git-lfs file whose object was
/// never uploaded: its own commit link cannot have moved, so the help says why and leaves it out
/// by its pattern to pin the rest, or says no other file is left to pin.
#[test]
fn a_listed_file_its_server_has_no_bytes_for_is_left_out_by_name() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(gitlab(
        "gitlab.com",
        &[("data/rows.csv", ROWS), ("data/lfs_object.iso", NOTES)],
    )?)?;
    let gone = format!("/gitlab.com/lab/group/survey/-/raw/{FIRST}/data/lfs_object.iso");
    routes.change(vec![(
        gone.clone(),
        reply("404 Not Found", &[("Content-Length", "0")], b""),
    )]);
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    let listed = format!(
        "https://gitlab.com/lab/group/survey/-/tree/{}/data lists lfs_object.iso, but its server has no bytes for it, as for a git-lfs file whose object was never uploaded",
        &FIRST[..7]
    );
    let output = run(
        &sandbox,
        &routes,
        &["--json", "add", link, "--as", "survey"],
    )?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        format!(
            "gitlab.com has nothing at https://gitlab.com/lab/group/survey/-/raw/{FIRST}/data/lfs_object.iso (404)"
        )
    );
    assert_eq!(
        error["help"],
        format!(
            "{listed}. the other files are in the cache. to add survey without it, run the add again with --exclude \"lfs_object.iso\""
        )
    );
    ok(&run(
        &sandbox,
        &routes,
        &["add", link, "--as", "survey", "--exclude", "lfs_object.iso"],
    )?)?;
    assert_eq!(fs::read(sandbox.work().join("data/survey/rows.csv"))?, ROWS);

    let alone = sandbox.folder("alone")?;
    let output = sandbox
        .command_in(&alone, &["add", link, "--as", "iso", "--select", "*.iso"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "help: {listed}, and no other file is left to pin\n"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("if it moved"), "{stderr}");
    Ok(())
}

/// a link at a commit never moves, so its 404 never says it may have: not in a get, nor in an
/// upgrade, which say why its server may have no bytes for the file, nor in a sync of a dataset of
/// that one file, which says how to keep the pinned bytes.
#[test]
fn a_commit_links_404_never_says_it_moved() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(gitlab(
        "gitlab.com",
        &[("data/rows.csv", ROWS), ("data/lfs_object.iso", NOTES)],
    )?)?;
    let api = "/gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey";
    let gone = |commit: &str, path: &str| {
        (
            format!("/gitlab.com/lab/group/survey/-/raw/{commit}/data/{path}"),
            reply("404 Not Found", &[("Content-Length", "0")], b""),
        )
    };
    routes.change(vec![
        (
            format!("{api}/repository/commits/{FIRST}"),
            answer(&serde_json::json!({"id": FIRST, "short_id": &FIRST[..8], "title": "add the survey"}))?,
        ),
        gone(FIRST, "lfs_object.iso"),
    ]);
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    let reason = |commit: &str, path: &str| {
        format!(
            "help: a link at a commit never moves, so gitlab.com has no bytes for data/{path} at {}: git-lfs may keep it with its object missing, or the commit may not hold it\n",
            &commit[..7]
        )
    };

    let other = sandbox.folder("other")?;
    let got = sandbox
        .command_in(&other, &["get", link])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    let stderr = text(&got.stderr)?;
    assert_eq!(got.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains(&reason(FIRST, "lfs_object.iso")),
        "{stderr}"
    );
    assert!(!stderr.contains("moved"), "{stderr}");

    ok(&run(
        &sandbox,
        &routes,
        &["add", link, "--as", "survey", "--select", "rows.csv"],
    )?)?;
    routes.change(vec![gone(FIRST, "rows.csv")]);
    let (_, synced) = teammate_sync(&sandbox, &routes)?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("help: to keep the pinned bytes"),
        "{stderr}"
    );
    assert!(!stderr.contains("moved"), "{stderr}");

    routes.change(gitlab_at(
        "gitlab.com",
        SECOND,
        &[("data/rows.csv", NOTES), ("data/lfs_object.iso", NOTES)],
    )?);
    routes.change(vec![gone(SECOND, "rows.csv")]);
    let upgraded = run(&sandbox, &routes, &["lock", "--upgrade", "survey"])?;
    let stderr = text(&upgraded.stderr)?;
    assert_eq!(upgraded.status.code(), Some(10), "{stderr}");
    assert!(stderr.contains(&reason(SECOND, "rows.csv")), "{stderr}");
    assert!(!stderr.contains("moved"), "{stderr}");
    Ok(())
}

/// gitlab lists a folder in pages, each naming the next in a `Link` header: a page on the host
/// that answered is asked, and the folder pins whole; one on any other origin, by its scheme, its
/// host or its port, stops the add with both named, and is never asked, rather than pin part of
/// the folder.
#[test]
fn a_listings_next_page_on_another_origin_stops_the_add() -> Result {
    let sandbox = Sandbox::new()?;
    let api = "gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey";
    let query = format!("path=data&ref={FIRST}&recursive=true&per_page=100&pagination=keyset");
    let first = format!("/{api}/repository/tree?{query}");
    let second = format!("/{api}/repository/tree?page_token=b2Zm&{query}");
    let entry = |path: &str, bytes: &[u8]| serde_json::json!({"id": blob_id(bytes), "name": path.rsplit('/').next(), "type": "blob", "path": path, "mode": "100644"});
    let paged = |origin: &str| -> Result<Vec<(String, Vec<Part>)>> {
        let link = format!(
            "<{origin}{}>; rel=\"next\"",
            &second[1 + "gitlab.com".len()..]
        );
        Ok(vec![(
            first.clone(),
            reply(
                "200 OK",
                &[("Content-Type", "application/json"), ("Link", &link)],
                &serde_json::to_vec(&serde_json::json!([
                    {"id": "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "name": "2020", "type": "tree", "path": "data/2020", "mode": "040000"},
                    entry("data/rows.csv", ROWS)
                ]))?,
            ),
        )])
    };
    let mut served = vec![
        (
            format!("/{api}/repository/commits/main"),
            answer(&serde_json::json!({"id": FIRST, "short_id": &FIRST[..8]}))?,
        ),
        (
            second.clone(),
            answer(&serde_json::json!([entry("data/2020/notes.txt", NOTES)]))?,
        ),
    ];
    for (path, bytes) in [("data/rows.csv", ROWS), ("data/2020/notes.txt", NOTES)] {
        served.push((
            format!("/gitlab.com/lab/group/survey/-/raw/{FIRST}/{path}"),
            file(bytes, "\"git\""),
        ));
    }
    let routes = Routes::start(served)?;
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    for origin in [
        "http://gitlab.com",
        "https://gitlab.com:8443",
        "https://elsewhere.example",
    ] {
        routes.change(paged(origin)?);
        let asked = routes.targets().len();
        let folder = sandbox.folder(&format!("other-{}", origin.len()))?;
        let output = sandbox
            .command_in(&folder, &["add", link, "--as", "survey"])
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(10), "{origin}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "error: https://gitlab.com named its next page on {origin}; fetchloom follows a listing only on the host that answered, so it stops rather than pin part of it\n"
            )),
            "{origin}: {stderr}"
        );
        assert!(!folder.join("data.toml").exists(), "{origin}");
        assert!(
            routes.targets()[asked..]
                .iter()
                .all(|target| !target.contains("page_token")),
            "{origin}: {:?}",
            &routes.targets()[asked..]
        );
    }
    routes.change(paged("https://gitlab.com")?);
    ok(&run(&sandbox, &routes, &["add", link, "--as", "survey"])?)?;
    assert_eq!(
        placed(&sandbox.work().join("data/survey"))?,
        [".gitignore", "2020/notes.txt", "rows.csv"]
    );
    Ok(())
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

/// gitlab's answer to a request it limits: 429 with the wait, counted by address for a request
/// without a key, as gitlab.com says in `RateLimit-Name`.
fn limited() -> Vec<Part> {
    reply(
        "429 Too Many Requests",
        &[
            ("Retry-After", "1891"),
            ("RateLimit-Name", "unauthenticated_traffic_per_ip"),
            ("Content-Length", "0"),
        ],
        b"",
    )
}

/// gitlab limits the requests that come without a key by their address, which a whole lab may
/// share, so the error names the key that gives a limit of one's own: for an add and a teammate's
/// sync on gitlab.com, and for another server its own sign-in. a run whose key went is told only
/// to wait.
#[test]
fn gitlabs_rate_limit_without_a_key_names_the_key() -> Result {
    let sandbox = Sandbox::new()?;
    let key = "set GITLAB_TOKEN to a gitlab.com token, or sign in with `fetchloom auth login gitlab`, for a limit of your own";
    let commit = "/gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey/repository/commits/main";
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    let routes = Routes::start(vec![(commit.to_owned(), limited())])?;
    let output = run(&sandbox, &routes, &["add", link])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    assert!(
        stderr.contains("error: gitlab.com asked to wait 31 min 31 s before the next request"),
        "{stderr}"
    );
    assert!(stderr.contains(key), "{stderr}");
    let keyed = sandbox
        .command(&["add", link])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .env("GITLAB_TOKEN", "glpat-test-key-that-is-never-printed")
        .output()?;
    let stderr = text(&keyed.stderr)?;
    assert_eq!(keyed.status.code(), Some(20), "{stderr}");
    assert!(!stderr.contains("GITLAB_TOKEN"), "{stderr}");

    let routes = Routes::start(gitlab("gitlab.com", &[("data/rows.csv", ROWS)])?)?;
    ok(&run(&sandbox, &routes, &["add", link, "--as", "survey"])?)?;
    routes.change(vec![(
        format!("/gitlab.com/lab/group/survey/-/raw/{FIRST}/data/rows.csv"),
        limited(),
    )]);
    let (_, synced) = teammate_sync(&sandbox, &routes)?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains(key), "{stderr}");

    let elsewhere = Routes::start(vec![(
        "/code.example.org/api/v4/projects/lab%2Fgroup%2Fsurvey/repository/commits/main".to_owned(),
        limited(),
    )])?;
    let output = run(
        &sandbox,
        &elsewhere,
        &[
            "add",
            "https://code.example.org/lab/group/survey/-/tree/main/data",
        ],
    )?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    assert!(
        stderr.contains("save a key for it with `fetchloom auth login code.example.org --token`, for a limit of your own"),
        "{stderr}"
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

    routes.change(vec![(
        "/code.example.org/api/v4/projects/lab%2Fgroup%2Fhidden/repository/commits/main".to_owned(),
        reply(
            "404 Not Found",
            &[("Content-Type", "application/json")],
            br#"{"message":"404 Project Not Found"}"#,
        ),
    )]);
    let hidden = "https://code.example.org/lab/group/hidden/-/tree/main/data";
    let output = run(&sandbox, &routes, &["add", hidden, "--as", "hidden"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(40), "{stderr}");
    assert!(
        stderr.contains(
            "error: code.example.org has no project lab/group/hidden, or this key cannot see it\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("went with the request\n"), "{stderr}");
    assert!(
        stderr.contains("help: check the path at https://code.example.org/lab/group/hidden. a private project's files need a key that can read it: if the key saved for code.example.org is old or wrong, save a current one with `fetchloom auth login code.example.org --token`\n"),
        "{stderr}"
    );
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
    let mut asked: Vec<String> = routes.targets()[before..]
        .iter()
        .map(|target| one_tree(target))
        .collect();
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

/// a teammate's cache that synced data.lock holds the commit it pins, so their first upgrade to a
/// commit that changed one file downloads that file alone: the others are proven by their blob
/// ids, computed from the bytes the sync brought, and recorded, so a later commit that moves one
/// finds it by its blob alone.
#[test]
fn a_teammate_who_synced_downloads_only_what_the_new_commit_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
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
    let (teammate, synced) = teammate_sync(&sandbox, &routes)?;
    ok(&synced)?;
    let cache = sandbox.folder("teammate-cache")?;
    let upgrade = || -> Result<()> {
        ok(&sandbox
            .command_in(&teammate, &["lock", "--upgrade"])
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?)
    };
    let api = |commit: &str| {
        vec![
            format!("{GITHUB_API}/commits/main"),
            format!("{GITHUB_API}/git/trees/{commit}?recursive=1"),
        ]
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
    let mut expected = api(SECOND);
    expected.extend(served_from(SECOND, "rows.csv", true));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    assert_eq!(
        pins(&teammate, "survey")?,
        pinned_as(&[("rows.csv", NOTES), ("big.bin", &big)])
    );

    routes.change(github(
        "main",
        THIRD,
        &[
            ("data/rows.csv", Held::File(NOTES)),
            ("data/moved.bin", Held::File(&big)),
        ],
    )?);
    let before = routes.targets().len();
    upgrade()?;
    let mut expected = api(THIRD);
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
    assert_eq!(
        pins(&teammate, "survey")?,
        pinned_as(&[("rows.csv", NOTES), ("moved.bin", &big)])
    );
    Ok(())
}

/// a teammate who synced a private repository's folder upgrades it as one who synced a public
/// folder does: each file whose blob the new commit keeps is proven from the cache, though every
/// pin's link names a blob of the one repository, and only the changed file's blob is asked.
#[test]
fn a_teammate_who_synced_a_private_folder_downloads_only_what_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let routes = added_secret(&sandbox, &big)?;
    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), teammate.join(name))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let keyed = |args: &[&str]| -> Result<std::process::Output> {
        Ok(sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .env("FETCHLOOM_CACHE", &cache)
            .env("GH_TOKEN", KEY)
            .output()?)
    };
    ok(&keyed(&["sync", "--locked"])?)?;
    let changed: &[u8] = b"site,count\na,1\nb,2\nc,3\n";
    let held = [
        ("data/rows.csv", Held::File(changed)),
        ("data/2020/field notes.txt", Held::File(NOTES)),
        ("data/big.bin", Held::File(&big)),
    ];
    routes.change(secret_at(SECOND, &held)?);
    let before = routes.targets().len();
    ok(&keyed(&["lock", "--upgrade"])?)?;
    let blobs: Vec<String> = asked_since(&routes, before)
        .into_iter()
        .filter(|target| target.contains("/git/blobs/"))
        .collect();
    assert_eq!(
        blobs,
        [format!("{SECRET_API}/git/blobs/{}", blob_id(changed))]
    );
    let lock = fs::read_to_string(teammate.join("data.lock"))?;
    for (path, bytes) in [
        ("rows.csv", changed),
        ("2020/field notes.txt", NOTES),
        ("big.bin", &big),
    ] {
        assert!(lock.contains(&sha256_hex(bytes)), "{path}: {lock}");
    }
    Ok(())
}

/// data.lock's pins stand only for files of their own repository: a dataset whose link moves from
/// a fork to the repository downloads each file again, though the fork holds the very same blobs,
/// since one repository's ids never vouch for another's bytes.
#[test]
fn a_pin_of_a_fork_never_stands_for_a_file_of_the_repository() -> Result {
    let sandbox = Sandbox::new()?;
    let big = large();
    let held = [
        ("data/rows.csv", Held::File(ROWS)),
        ("data/big.bin", Held::File(&big)),
    ];
    let mut served = on_github("lab/fork", "main", FIRST, &held)?;
    served.extend(github("main", FIRST, &held)?);
    let routes = Routes::start(served)?;
    ok(&run(
        &sandbox,
        &routes,
        &[
            "add",
            "https://github.com/lab/fork/tree/main/data",
            "--as",
            "survey",
        ],
    )?)?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.survey]\nref = \"https://github.com/lab/survey/tree/main/data\"\n",
    )?;
    let before = routes.targets().len();
    ok(&run(&sandbox, &routes, &["lock"])?)?;
    let mut expected = vec![
        format!("{GITHUB_API}/commits/main"),
        format!("{GITHUB_API}/git/trees/{FIRST}?recursive=1"),
    ];
    expected.extend(served_from(FIRST, "rows.csv", true));
    expected.extend(served_from(FIRST, "big.bin", false));
    expected.sort();
    assert_eq!(asked_since(&routes, before), expected);
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

/// a damaged record is no record, so the notes, moved where data.lock pins nothing, download and
/// are pinned by their own bytes. a record naming other bytes the cache holds stands for its blob
/// unread in a cache only this user writes, as a url record does there, so a relock pins those
/// bytes; in one others may write, where another member's add left the records, which a debug
/// build told it runs as another user stands for, those bytes do not have the blob's id, so the
/// file downloads and is pinned by its own. the rest come from the cache.
#[test]
fn a_foreign_record_stands_only_in_a_cache_this_user_alone_writes() -> Result {
    let big = large();
    let other: Vec<u8> = big.iter().map(|byte| byte ^ 1).collect();
    for shared in [false, true] {
        let sandbox = Sandbox::new()?;
        #[cfg(unix)]
        if shared && !crate::folders::shared_cache(&sandbox)? {
            continue;
        }
        #[cfg(not(unix))]
        if shared {
            continue;
        }
        let held = [
            ("data/rows.csv", Held::File(ROWS)),
            ("data/notes.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
            ("data/other.bin", Held::File(&other)),
        ];
        let routes = Routes::start(github("main", FIRST, &held)?)?;
        let mut adding = sandbox.command(&[
            "add",
            "https://github.com/lab/survey/tree/main/data",
            "--as",
            "survey",
        ]);
        adding.env("FETCHLOOM_TEST_HOSTS", &routes.url);
        if shared {
            adding.env("FETCHLOOM_TEST_EUID", "3999999999");
        }
        ok(&adding.output()?)?;
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

        // the notes move, so no pin of data.lock stands for them at the new commit
        let moved = [
            ("data/rows.csv", Held::File(ROWS)),
            ("data/moved.txt", Held::File(NOTES)),
            ("data/big.bin", Held::File(&big)),
            ("data/other.bin", Held::File(&other)),
        ];
        routes.change(github("main", SECOND, &moved)?);
        let before = routes.targets().len();
        ok(&run(&sandbox, &routes, &["lock", "--upgrade"])?)?;
        let mut expected = vec![
            format!("{GITHUB_API}/commits/main"),
            format!("{GITHUB_API}/git/trees/{SECOND}?recursive=1"),
        ];
        expected.extend(served_from(SECOND, "moved.txt", true));
        if shared {
            expected.extend(served_from(SECOND, "big.bin", false));
        }
        expected.sort();
        assert_eq!(asked_since(&routes, before), expected, "{shared}");
        let work = sandbox.work();
        let placed_as: &[u8] = if shared { &big } else { &other };
        assert_eq!(
            pins(&work, "survey")?,
            pinned_as(&[
                ("rows.csv", ROWS),
                ("moved.txt", NOTES),
                ("big.bin", placed_as),
                ("other.bin", &other),
            ]),
            "{shared}"
        );
        ok(&run(&sandbox, &routes, &["sync"])?)?;
        assert_eq!(fs::read(work.join("data/survey/big.bin"))?, placed_as);
        assert_eq!(fs::read(work.join("data/survey/other.bin"))?, other);
    }
    Ok(())
}

/// a folder below its repository's top pins as the whole tree pins it, its tree asked for in one
/// request of its own rather than the whole repository's.
#[test]
fn a_github_folder_asked_by_its_own_tree_pins_as_the_whole_tree_does() -> Result {
    let held = [
        ("README.md", Held::File(b"# survey\n")),
        ("data/rows.csv", Held::File(ROWS)),
        ("data/2020/field notes.txt", Held::File(NOTES)),
        ("other/rows.csv", Held::File(NOTES)),
    ];
    let routes = Routes::start(github("main", FIRST, &held)?)?;
    let sandbox = Sandbox::new()?;
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
    assert_eq!(
        pins(&sandbox.work(), "survey")?,
        pinned_as(&[("2020/field notes.txt", NOTES), ("rows.csv", ROWS)])
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
            format!("{GITHUB_API}/git/trees/{FIRST}:data?recursive=1")
        ]
    );
    Ok(())
}
