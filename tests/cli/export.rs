//! exports: the checksums of the files a project places and the links of the files it downloads,
//! for other tools, read from data.lock and the cache without asking any server.

use std::fs;
use std::process::Command;

use crate::support::{Result, Routes, Sandbox, file, json, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const A: &[u8] = b"a\n";
const C: &[u8] = b"c\n";

/// a zip of `pack/a.csv` and `pack/b/c.txt`.
fn pack() -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in [("pack/a.csv", A), ("pack/b/c.txt", C)] {
        archive.start_file(path, zip::write::SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut archive, bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

/// a server on this machine for iris.csv and pack.zip, with the zip's bytes.
fn server() -> Result<(Routes, Vec<u8>)> {
    let zip = pack()?;
    let routes = Routes::start(vec![
        ("/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/pack.zip".to_owned(), file(&zip, "\"p1\"")),
    ])?;
    Ok((routes, zip))
}

/// adds each of `files` from `routes` to the project in the sandbox's work folder.
fn added(sandbox: &Sandbox, routes: &Routes, files: &[&str]) -> Result {
    for name in files {
        let added = sandbox.run(&["add", &format!("{}/{name}", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[test]
fn sha256_lists_each_placed_file_with_archive_members_for_sha256sum() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let asked = routes.requests().len();
    let output = sandbox.run(&["export"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let sums = text(&output.stdout)?;
    assert_eq!(
        sums,
        format!(
            "{}  data/iris/iris.csv\n{}  data/pack/a.csv\n{}  data/pack/b/c.txt\n",
            sha256(IRIS),
            sha256(A),
            sha256(C)
        )
    );
    assert_eq!(text(&output.stderr)?, "");
    assert_eq!(routes.requests().len(), asked, "export asks no server");
    // coreutils checks the list from the project's folder, where linux has it
    fs::write(sandbox.work().join("sums"), &sums)?;
    if let Ok(checked) = Command::new("sha256sum")
        .args(["--check", "--strict", "sums"])
        .current_dir(sandbox.work())
        .output()
    {
        assert!(checked.status.success(), "{}", text(&checked.stderr)?);
        assert_eq!(text(&checked.stdout)?.lines().count(), 3);
    }
    Ok(())
}

#[test]
fn links_and_aria2_name_each_downloaded_file_and_never_an_archive_member() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let links = sandbox.run(&["export", "--format", "links"])?;
    assert_eq!(links.status.code(), Some(0), "{}", text(&links.stderr)?);
    let url = &routes.url;
    assert_eq!(
        text(&links.stdout)?,
        format!(
            "{url}/iris.csv {}\n{url}/pack.zip {}\n",
            sha256(IRIS),
            sha256(&zip)
        )
    );
    let aria2 = sandbox.run(&["export", "--format", "aria2", "pack"])?;
    assert_eq!(aria2.status.code(), Some(0), "{}", text(&aria2.stderr)?);
    assert_eq!(
        text(&aria2.stdout)?,
        format!(
            "{url}/pack.zip\n  out=pack/pack.zip\n  checksum=sha-256={}\n",
            sha256(&zip)
        )
    );
    Ok(())
}

#[test]
fn json_lists_placed_files_and_downloaded_ones_with_their_hashes() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let output = sandbox.run(&["--json", "export", "pack"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let result = &json(&output)?["result"];
    assert_eq!(
        result["placed"],
        serde_json::json!([
            {"dataset": "pack", "path": "data/pack/a.csv", "size": A.len(), "sha256": sha256(A), "unpacked_from": "pack.zip"},
            {"dataset": "pack", "path": "data/pack/b/c.txt", "size": C.len(), "sha256": sha256(C), "unpacked_from": "pack.zip"},
        ])
    );
    assert_eq!(
        result["sources"],
        serde_json::json!([
            {"dataset": "pack", "path": "pack.zip", "size": zip.len(), "sha256": sha256(&zip), "url": format!("{}/pack.zip", routes.url)},
        ])
    );
    for gap in ["unlocked", "not_unpacked", "no_link"] {
        assert_eq!(result[gap], serde_json::json!([]), "{gap}");
    }
    assert_eq!(result["file"], serde_json::Value::Null);
    let unknown = sandbox.run(&["export", "pak"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr)?.contains("fetchloom export pack"),
        "{}",
        text(&unknown.stderr)?
    );
    Ok(())
}

/// a clone whose cache never unpacked an archive cannot list its files. text refuses rather than
/// leave them out, json names the archive, and links need no unpacking.
#[test]
fn an_archive_never_unpacked_here_stops_text_and_is_named_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["pack.zip"])?;
    let cold = sandbox.folder("cold")?;
    let run = |args: &[&str]| sandbox.command(args).env("FETCHLOOM_CACHE", &cold).output();
    let refused = run(&["export"])?;
    assert_eq!(
        refused.status.code(),
        Some(20),
        "{}",
        text(&refused.stderr)?
    );
    assert_eq!(text(&refused.stdout)?, "", "no partial list");
    let stderr = text(&refused.stderr)?;
    assert!(
        stderr.contains("error: the files inside data/pack/pack.zip are not known on this machine"),
        "{stderr}"
    );
    assert!(stderr.contains("`fetchloom sync`"), "{stderr}");
    let listed = run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let result = &json(&listed)?["result"];
    assert_eq!(
        result["not_unpacked"],
        serde_json::json!(["data/pack/pack.zip"])
    );
    assert_eq!(result["placed"], serde_json::json!([]));
    assert_eq!(result["sources"][0]["sha256"], sha256(&zip));
    let links = run(&["export", "--format", "links"])?;
    assert_eq!(links.status.code(), Some(0), "{}", text(&links.stderr)?);
    assert_eq!(
        text(&links.stdout)?,
        format!("{}/pack.zip {}\n", routes.url, sha256(&zip))
    );
    assert!(!cold.join("objects").exists(), "export makes no cache");
    Ok(())
}

#[test]
fn a_dataset_not_pinned_yet_stops_text_and_is_named_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let manifest = sandbox.work().join("data.toml");
    let before = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{before}\n[datasets.later]\nref = \"{}/pack.zip\"\n",
            routes.url
        ),
    )?;
    let refused = sandbox.run(&["export", "--format", "links"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    assert_eq!(text(&refused.stdout)?, "");
    let stderr = text(&refused.stderr)?;
    assert!(stderr.contains("later is not in data.lock yet"), "{stderr}");
    let listed = sandbox.run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let result = &json(&listed)?["result"];
    assert_eq!(result["unlocked"], serde_json::json!(["later"]));
    assert_eq!(result["placed"][0]["path"], "data/iris/iris.csv");
    Ok(())
}

/// fetchloom never pins a link with a login or a secret, so one in data.lock was written by hand
/// or by someone else. export writes no such link, and never shows its secret.
#[test]
fn a_link_with_a_secret_is_never_written() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let link = format!("{}/iris.csv", routes.url);
    fs::write(
        &lock,
        pinned.replace(
            &format!("url = \"{link}\""),
            &format!("url = \"{link}?token=export-canary\""),
        ),
    )?;
    let refused = sandbox.run(&["export", "--format", "aria2"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    assert_eq!(text(&refused.stdout)?, "");
    let stderr = text(&refused.stderr)?;
    assert!(
        stderr.contains("the link data.lock gives iris/iris.csv holds a secret"),
        "{stderr}"
    );
    let listed = sandbox.run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let document = json(&listed)?;
    assert_eq!(
        document["result"]["sources"][0]["url"],
        serde_json::Value::Null
    );
    assert_eq!(
        document["result"]["no_link"],
        serde_json::json!(["iris/iris.csv"])
    );
    for output in [&refused, &listed] {
        for stream in [&output.stdout, &output.stderr] {
            assert!(
                !text(stream)?.contains("export-canary"),
                "{}",
                text(stream)?
            );
        }
    }
    let sums = sandbox.run(&["export"])?;
    assert_eq!(sums.status.code(), Some(0), "checksums need no link");
    Ok(())
}

#[test]
fn output_writes_the_file_whole_and_replaces_one_only_with_force() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let expected = format!("{}  data/iris/iris.csv\n", sha256(IRIS));
    let sums = sandbox.work().join("sums.txt");
    let written = sandbox.run(&["export", "-o", "sums.txt"])?;
    assert_eq!(written.status.code(), Some(0), "{}", text(&written.stderr)?);
    assert_eq!(text(&written.stdout)?, "");
    assert_eq!(text(&written.stderr)?, "   Exported 1 file to sums.txt\n");
    assert_eq!(fs::read_to_string(&sums)?, expected);
    fs::write(&sums, "mine\n")?;
    let kept = sandbox.run(&["--json", "export", "-o", "sums.txt"])?;
    assert_eq!(kept.status.code(), Some(50));
    let error = &json(&kept)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert!(
        error["help"]
            .as_str()
            .unwrap_or_default()
            .contains("--force"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&sums)?, "mine\n");
    let forced = sandbox.run(&["--json", "export", "-o", "sums.txt", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(json(&forced)?["result"]["file"], sums.display().to_string());
    assert_eq!(fs::read_to_string(&sums)?, expected);
    let mut names: Vec<String> = fs::read_dir(sandbox.work())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    assert_eq!(
        names,
        [".fetchloom", "data", "data.lock", "data.toml", "sums.txt"],
        "nothing is left beside it"
    );
    let folder = sandbox.run(&["export", "-o", "data"])?;
    assert_eq!(folder.status.code(), Some(2), "{}", text(&folder.stderr)?);
    let force_alone = sandbox.run(&["export", "--force"])?;
    assert_eq!(force_alone.status.code(), Some(2));
    Ok(())
}
