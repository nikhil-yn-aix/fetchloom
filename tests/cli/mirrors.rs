//! mirrors: folders and web servers that hold pinned files by their SHA-256, asked before the
//! sources data.lock names.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::add::sha256_hex as sha256;
use crate::caps::shown;
use crate::support::{Result, Routes, Sandbox, edit, file, json, reply, text};

const IRIS: &[u8] = b"sepal_length,sepal_width,species\n5.1,3.5,setosa\n";

/// every file tests/fixtures/archives/python.zip holds, by its path under the zip's top folder.
const MEMBERS: [(&str, &[u8]); 5] = [
    ("README.md", b"# sample dataset\n"),
    ("data/caf\u{e9}.csv", b"a,b\n"),
    ("data/empty.txt", b""),
    ("data/train.csv", b"x,y\n1,2\n3,4\n"),
    (
        "long/a-file-name-longer-than-one-hundred-bytes-so-old-tar-headers-cannot-hold-it-and-pax-or-gnu-records-must.txt",
        b"long path\n",
    ),
];

fn ok(output: &Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// a source serving one csv file and one zip, and a project in the sandbox's work folder that
/// pins both, synced once there so the sandbox's cache holds them.
fn project(sandbox: &Sandbox) -> Result<Routes> {
    let zip = fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archives/python.zip"),
    )?;
    let source = Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/files/dataset.zip".to_owned(), file(&zip, "\"z1\"")),
    ])?;
    sandbox.allow(&source.url);
    for path in ["/files/iris.csv", "/files/dataset.zip"] {
        ok(&sandbox.run(&["add", &format!("{}{path}", source.url)])?)?;
    }
    Ok(source)
}

/// a folder mirror at `root` holding every object the sandbox's cache holds and the tree of every
/// archive it unpacked, as pushing fills one.
fn mirror_of(sandbox: &Sandbox, root: &Path) -> Result {
    fs::create_dir_all(root)?;
    fs::write(root.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
    for fan in fs::read_dir(sandbox.cache().join("objects"))? {
        let fan = fan?;
        for object in fs::read_dir(fan.path())? {
            let object = object?;
            let to = root.join("objects").join(fan.file_name());
            fs::create_dir_all(&to)?;
            fs::copy(object.path(), to.join(object.file_name()))?;
        }
    }
    for record in fs::read_dir(sandbox.cache().join("unpacked"))? {
        let bytes = fs::read(record?.path())?;
        let hex = sha256(&bytes);
        let to = root.join("trees").join(&hex[..2]);
        fs::create_dir_all(&to)?;
        fs::write(to.join(&hex[2..]), bytes)?;
    }
    Ok(())
}

/// a web mirror under `/m` holding what [`mirror_of`] puts in a folder mirror.
fn web_mirror_of(sandbox: &Sandbox) -> Result<Routes> {
    let root = sandbox.folder("web-mirror")?;
    mirror_of(sandbox, &root)?;
    let mut routes = Vec::new();
    let mut folders = vec![root.clone()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
                continue;
            }
            let below = path
                .strip_prefix(&root)?
                .to_string_lossy()
                .replace('\\', "/");
            routes.push((format!("/m/{below}"), file(&fs::read(&path)?, "\"m\"")));
        }
    }
    Routes::start(routes)
}

/// a fresh clone of the project in the sandbox's work folder, with a cache of its own.
fn cold(sandbox: &Sandbox, name: &str) -> Result<(PathBuf, PathBuf)> {
    let folder = sandbox.folder(name)?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(file), folder.join(file))?;
    }
    Ok((folder, sandbox.folder(&format!("{name}-cache"))?))
}

/// the clone at `folder` holds the csv file and every file of the zip.
fn holds_the_project(folder: &Path) -> Result {
    assert_eq!(fs::read(folder.join("data/iris/iris.csv"))?, IRIS);
    for (path, bytes) in MEMBERS {
        assert_eq!(
            fs::read(folder.join("data/dataset").join(path))?,
            bytes,
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn the_settings_list_each_mirror_and_refuse_what_is_no_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let settings = sandbox.work().join("settings.toml");
    std::fs::write(
        &settings,
        "mirrors = [\"lab, shared\", \"https://data.lab.example/fetchloom\"]\nmirrors_only = true\n",
    )?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SETTINGS", &settings)
            .output()?,
    )?;
    let mirrors = shown(&document, "mirrors");
    // a relative folder is taken from the settings file's folder, comma and all
    let folder = sandbox.work().join("lab, shared").display().to_string();
    assert_eq!(
        mirrors["value"],
        serde_json::json!([folder, "https://data.lab.example/fetchloom"])
    );
    assert_eq!(mirrors["origin"], "file");
    assert_eq!(mirrors["variable"], "FETCHLOOM_MIRRORS");
    assert_eq!(shown(&document, "mirrors_only")["value"], true);
    let output = sandbox
        .command(&["--json", "settings"])
        .env("FETCHLOOM_MIRRORS", "ftp://lab.example/fetchloom")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        "FETCHLOOM_MIRRORS holds \"ftp://lab.example/fetchloom\", which is no mirror: a mirror is a folder or an http or https address, not ftp"
    );
    Ok(())
}

#[test]
fn a_cold_clone_syncs_from_a_folder_mirror_without_asking_the_source() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let asked = source.requests().len();
    let (folder, cache) = cold(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&folder, &["--json", "sync", "--locked", "--mirror"])
        .arg(&root)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    assert_eq!(source.requests().len(), asked, "the source is never asked");
    let document = json(&output)?;
    assert_eq!(document["work"]["requests"], 0);
    // the csv file and the zip's five files, one by one: the zip itself is on no mirror
    assert_eq!(document["work"]["mirror_objects"], 6);
    assert_eq!(
        document["work"]["mirror_bytes"],
        IRIS.len() + MEMBERS.iter().map(|(_, bytes)| bytes.len()).sum::<usize>()
    );
    Ok(())
}

#[test]
fn offline_sync_reads_a_folder_mirror_from_the_settings() -> Result {
    let sandbox = Sandbox::new()?;
    project(&sandbox)?;
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let (folder, cache) = cold(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&folder, &["--offline", "sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_MIRRORS", &root)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    let stderr = text(&output.stderr)?;
    sandbox
        .placed_filters(&stderr)?
        .bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn a_damaged_mirror_copy_is_told_and_the_source_gives_the_file() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let hex = sha256(IRIS);
    let copy = root.join("objects").join(&hex[..2]).join(&hex[2..]);
    edit(&copy, &IRIS.to_ascii_uppercase())?;
    let asked = source.requests().len();
    let (folder, cache) = cold(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&folder, &["sync", "--mirror"])
        .arg(&root)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!(
            "warning: the mirror {} holds a damaged copy of data/iris/iris.csv, so it was not used",
            root.display()
        )),
        "{stderr}"
    );
    assert_eq!(
        source.targets()[asked..],
        ["/files/iris.csv"],
        "only the damaged file comes from its source"
    );
    Ok(())
}

#[test]
fn a_mirror_that_is_not_there_is_skipped_with_one_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let asked = source.requests().len();
    let (folder, cache) = cold(&sandbox, "cold")?;
    let gone = sandbox.work().join("unmounted");
    let output = sandbox
        .command_in(&folder, &["sync", "--mirror"])
        .arg(&gone)
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    let stderr = text(&output.stderr)?;
    let warning = format!(
        "warning: the mirror {} is not there, so this run skips it",
        gone.display()
    );
    assert_eq!(stderr.matches(&warning).count(), 1, "{stderr}");
    assert_eq!(source.requests().len(), asked + 2);
    Ok(())
}

#[test]
fn mirror_only_mode_needs_a_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["--json", "settings"])
        .env("FETCHLOOM_MIRRORS_ONLY", "1")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["message"],
        "FETCHLOOM_MIRRORS_ONLY asks for mirrors alone, but no mirror is set"
    );
    let output = sandbox
        .command(&["settings", "--mirror", "elsewhere"])
        .env("FETCHLOOM_MIRRORS_ONLY", "1")
        .output()?;
    ok(&output)
}

#[test]
fn a_reviewer_restores_from_a_web_mirror_when_the_source_is_gone() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let mirror = web_mirror_of(&sandbox)?;
    drop(source);
    let base = format!("{}/m", mirror.url);
    let (folder, cache) = cold(&sandbox, "cold")?;
    // offline asks no web mirror
    let offline = sandbox
        .command_in(&folder, &["--offline", "sync", "--mirror", &base])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(
        offline.status.code(),
        Some(20),
        "{}",
        text(&offline.stderr)?
    );
    assert_eq!(mirror.targets(), Vec::<String>::new());
    let output = sandbox
        .command_in(&folder, &["--json", "sync", "--locked", "--mirror", &base])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    let document = json(&output)?;
    assert_eq!(document["work"]["mirror_objects"], 6);
    let targets = mirror.targets();
    // one look at the marker, the zip's tree, then each file once
    assert_eq!(targets[0], "/m/fetchloom-mirror");
    assert_eq!(targets.len(), 8, "{targets:?}");
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.starts_with("/m/trees/"))
            .count(),
        1
    );
    Ok(())
}

/// a web mirror that does not answer is asked once, then skipped for the rest of the run. one on
/// this machine, named in the settings, is reached as a typed link is.
#[test]
fn a_dead_web_mirror_is_asked_once_then_skipped() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let asked = source.requests().len();
    let mirror = Routes::start(vec![(
        "/m/fetchloom-mirror".to_owned(),
        reply("503 Service Unavailable", &[("Content-Length", "0")], b""),
    )])?;
    let (folder, cache) = cold(&sandbox, "cold")?;
    let output = sandbox
        .command_in(&folder, &["sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_MIRRORS", format!("{}/m", mirror.url))
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    ok(&output)?;
    holds_the_project(&folder)?;
    assert_eq!(mirror.targets(), ["/m/fetchloom-mirror"]);
    let stderr = text(&output.stderr)?;
    assert_eq!(
        stderr.matches("so this run skips it").count(),
        1,
        "{stderr}"
    );
    assert_eq!(source.requests().len(), asked + 2);
    Ok(())
}
