//! lists of files as one dataset: a `SHA256SUMS`, an `MD5SUMS`, a pooch registry or a list of
//! links, at a link or in the project, read by one reader.

use std::fs;

use crate::support::{Result, Routes, Sandbox, file, json, md5_hex, sha256_hex, text};

const IRIS: &[u8] = b"sepal,petal\n5.1,1.4\n";
const TIPS: &[u8] = b"bill,tip\n16.99,1.01\n";

/// a server with iris.csv and sub/tips.csv in its `/data` folder, beside `lists`, each a list
/// file's path below `/data` and its text.
fn served(lists: &[(&str, String)]) -> Result<Routes> {
    let mut routes = vec![
        ("/data/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/data/sub/tips.csv".to_owned(), file(TIPS, "\"t1\"")),
    ];
    for (name, text) in lists {
        routes.push((format!("/data/{name}"), file(text.as_bytes(), "\"l1\"")));
    }
    Routes::start(routes)
}

fn gnu() -> String {
    format!(
        "{}  iris.csv\n{}  sub/tips.csv\n",
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    )
}

/// the files data.lock pins for `name`: each file's path, SHA-256 and link, as written.
fn pinned(lock: &str, name: &str) -> Vec<String> {
    let entry = lock
        .split("[[dataset]]")
        .find(|entry| entry.contains(&format!("name = \"{name}\"")))
        .unwrap_or_default();
    entry
        .lines()
        .filter(|line| line.trim_start().starts_with("{ path"))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// `add` of a link to a SHA256SUMS pins every file it lists at the list's folder plus its path,
/// as one dataset; a sync on another machine gets the same bytes from data.lock alone, and
/// `--select` narrows the list as it narrows any dataset.
#[test]
fn a_checksum_list_at_a_link_pins_every_file_it_lists() -> Result {
    let sandbox = Sandbox::new()?;
    let server = served(&[("SHA256SUMS", gnu())])?;
    let list = format!("{}/data/SHA256SUMS", server.url);
    let output = sandbox.run(&["--json", "add", &list])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        json(&output)?["result"]["datasets"],
        serde_json::json!(["data"])
    );
    let work = sandbox.work();
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        format!("[datasets.data]\nref = \"list:{list}\"\n")
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains(&format!("resolved = \"list:{list}\"")),
        "{lock}"
    );
    assert_eq!(
        pinned(&lock, "data"),
        [
            format!(
                "{{ path = \"iris.csv\", size = {}, sha256 = \"{}\", url = \"{}/data/iris.csv\" }},",
                IRIS.len(),
                sha256_hex(IRIS),
                server.url
            ),
            format!(
                "{{ path = \"sub/tips.csv\", size = {}, sha256 = \"{}\", url = \"{}/data/sub/tips.csv\" }},",
                TIPS.len(),
                sha256_hex(TIPS),
                server.url
            ),
        ]
    );
    assert_eq!(fs::read(work.join("data/data/iris.csv"))?, IRIS);
    assert_eq!(fs::read(work.join("data/data/sub/tips.csv"))?, TIPS);

    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    sandbox.allow(&server.url);
    let asked = server.targets().len();
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(clone.join("data/data/iris.csv"))?, IRIS);
    assert_eq!(fs::read(clone.join("data/data/sub/tips.csv"))?, TIPS);
    assert!(
        !server.targets()[asked..].contains(&"/data/SHA256SUMS".to_owned()),
        "a sync of a pinned list needs only data.lock: {:?}",
        server.targets()
    );

    let narrowed = sandbox.run(&["add", &list, "--as", "tips", "--select", "sub/*"])?;
    assert_eq!(
        narrowed.status.code(),
        Some(0),
        "{}",
        text(&narrowed.stderr)?
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    let tips = pinned(&lock, "tips");
    assert_eq!(tips.len(), 1, "{lock}");
    assert!(tips[0].starts_with("{ path = \"sub/tips.csv\""), "{lock}");
    Ok(())
}

/// an MD5SUMS line checks the first download: other bytes are refused, naming the file, and the
/// project stays as it was.
#[test]
fn a_file_whose_md5_differs_from_its_list_line_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let sums = format!(
        "{}  iris.csv\n{}  sub/tips.csv\n",
        md5_hex(IRIS),
        md5_hex(IRIS)
    );
    let server = served(&[("MD5SUMS", sums)])?;
    let output = sandbox.run(&["--json", "add", &format!("{}/data/MD5SUMS", server.url)])?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch", "{document}");
    assert!(
        error.to_string().contains("tips.csv"),
        "the error names the file: {document}"
    );
    let work = sandbox.work();
    assert!(!work.join("data.toml").exists());
    assert!(!work.join("data.lock").exists());
    assert!(!work.join("data/data/sub/tips.csv").exists());
    Ok(())
}

/// a line that climbs out of the list's folder, or starts at the root of a disk, stops the add
/// before any file is asked for.
#[test]
fn a_list_line_that_leaves_its_folder_is_refused_before_any_download() -> Result {
    for (line, reason) in [
        ("../secret.csv", "it climbs out of its folder"),
        ("/etc/passwd", "it starts at the root of the disk"),
    ] {
        let sandbox = Sandbox::new()?;
        let sums = format!(
            "{}  iris.csv\n{}  {line}\n",
            sha256_hex(IRIS),
            sha256_hex(TIPS)
        );
        let server = served(&[("SHA256SUMS", sums)])?;
        let list = format!("{}/data/SHA256SUMS", server.url);
        let output = sandbox.run(&["--json", "add", &list])?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(
            document["errors"][0]["message"],
            format!("line 2 of {list} names \"{line}\", which fetchloom refuses because {reason}")
        );
        assert_eq!(server.targets(), ["/data/SHA256SUMS"], "{line}");
        assert!(!sandbox.work().join("data.toml").exists());
    }
    Ok(())
}

/// a pooch registry, a BSD list and the GNU form of the same files pin the same files.
#[test]
fn every_list_form_pins_the_same_files() -> Result {
    let sandbox = Sandbox::new()?;
    let bsd = format!(
        "SHA256 (iris.csv) = {}\nSHA256 (sub/tips.csv) = {}\n",
        sha256_hex(IRIS),
        sha256_hex(TIPS)
    );
    let pooch = format!(
        "# pooch registry\niris.csv sha256:{}\nsub/tips.csv md5:{}\n",
        sha256_hex(IRIS),
        md5_hex(TIPS)
    );
    let server = served(&[
        ("SHA256SUMS", gnu()),
        ("CHECKSUMS.bsd", bsd),
        ("registry.txt", pooch),
    ])?;
    for (list, name) in [
        (format!("{}/data/SHA256SUMS", server.url), "gnu"),
        (format!("list:{}/data/CHECKSUMS.bsd", server.url), "bsd"),
        (format!("list:{}/data/registry.txt", server.url), "pooch"),
    ] {
        let output = sandbox.run(&["add", &list, "--as", name])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let gnu = pinned(&lock, "gnu");
    assert_eq!(gnu.len(), 2, "{lock}");
    assert_eq!(pinned(&lock, "bsd"), gnu, "{lock}");
    // the registry states tips.csv's MD5, which data.lock keeps beside the SHA-256 it learned
    let pooch: Vec<String> = pinned(&lock, "pooch")
        .into_iter()
        .map(|line| line.replace(&format!(" md5 = \"{}\",", md5_hex(TIPS)), ""))
        .collect();
    assert_eq!(pooch, gnu, "{lock}");
    Ok(())
}
