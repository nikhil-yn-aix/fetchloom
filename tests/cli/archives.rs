//! archives: unpacked into projects and folders, checked against data.lock, kept out of the cache
//! once their files are in, and refused whole when unsafe.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

use crate::support::{Result, Sandbox, Server, delete, file, reply, text};

const PATH: &str = "/files/dataset.zip";
/// the tree python computed over the files in tests/fixtures/archives/python.zip.
const TREE: &str = "8c79dd9ba52babe269e8d1d2183c1c2201f677df8175749fe496f4d37bd0f2c1";
const ZIP_SHA256: &str = "0f9bec8699653333da43d8c5ff9092ad403447f8ec072040b22b8902fb34a8de";
const LONG: &str = "long/a-file-name-longer-than-one-hundred-bytes-so-old-tar-headers-cannot-hold-it-and-pax-or-gnu-records-must.txt";

/// every file python.zip holds, by its path under the zip's one top folder.
const FILES: [(&str, &[u8]); 5] = [
    ("README.md", b"# sample dataset\n"),
    ("data/caf\u{e9}.csv", b"a,b\n"),
    ("data/empty.txt", b""),
    ("data/train.csv", b"x,y\n1,2\n3,4\n"),
    (LONG, b"long path\n"),
];

fn fixture(name: &str) -> Result<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archives")
        .join(name);
    Ok(fs::read(path)?)
}

fn json(output: &Output) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn ok(output: &Output) -> Result {
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// the folder holds exactly the archive's files, and nothing else.
fn holds_the_archive(folder: &Path) -> Result {
    for (path, bytes) in FILES {
        assert_eq!(fs::read(folder.join(path))?, bytes, "{path}");
    }
    let mut found = 0;
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        for entry in fs::read_dir(next)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                folders.push(entry.path());
            } else {
                found += 1;
            }
        }
    }
    assert_eq!(found, FILES.len(), "no other file in {}", folder.display());
    Ok(())
}

fn zip_object(cache: &Path) -> PathBuf {
    cache
        .join("objects")
        .join(&ZIP_SHA256[..2])
        .join(&ZIP_SHA256[2..])
}

#[test]
fn a_dataset_that_is_one_zip_lands_unpacked_and_leaves_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    holds_the_archive(&sandbox.work().join("data").join("dataset"))?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains(&format!("unpacks_to = \"sha256:{TREE}\"")),
        "{lock}"
    );
    assert!(
        !zip_object(&sandbox.cache()).exists(),
        "the zip's own bytes leave the cache once its files are in"
    );
    let again = sandbox.run(&["sync"])?;
    ok(&again)?;
    assert_eq!(
        text(&again.stderr)?,
        "✓ everything already matches data.lock. nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1);
    let status = sandbox.run(&["status"])?;
    assert_eq!(text(&status.stderr)?, "✓ everything matches data.lock\n");
    Ok(())
}

#[test]
fn a_fresh_clone_unpacks_what_the_lock_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\""), file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.folder("cold")?;
    let synced = sandbox
        .command_in(&clone, &["sync"])
        .env("FETCHLOOM_CACHE", &cold)
        .output()?;
    ok(&synced)?;
    holds_the_archive(&clone.join("data").join("dataset"))?;
    assert!(!zip_object(&cold).exists());
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

#[test]
fn extract_false_keeps_the_zip_as_it_is() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\""), file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let manifest = sandbox.work().join("data.toml");
    let text_before = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text_before}extract = false\n"))?;
    ok(&sandbox.run(&["sync"])?)?;
    let folder = sandbox.work().join("data").join("dataset");
    assert_eq!(fs::read(folder.join("dataset.zip"))?, zip);
    assert!(!folder.join("README.md").exists(), "unpacked files go");
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(!lock.contains("unpacks_to"), "{lock}");
    Ok(())
}

#[test]
fn an_unsafe_archive_is_refused_whole() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python-escape.zip")?, "\"e1\"")])?;
    let output = sandbox.run(&["add", &server.url])?;
    assert_eq!(output.status.code(), Some(10));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("climbs out of its folder"), "{stderr}");
    assert!(stderr.contains("extract = false"), "{stderr}");
    assert!(
        !sandbox.work().join("data.toml").exists(),
        "add leaves no trace"
    );
    assert!(!sandbox.work().join("data").join("dataset").exists());
    Ok(())
}

#[test]
fn get_unpacks_a_zip_into_its_folder_and_a_second_run_moves_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"z1\"")], b"");
    let server = Server::start(
        PATH,
        vec![file(&fixture("python.zip")?, "\"z1\""), unchanged],
    )?;
    let first = sandbox.run(&["get", &server.url])?;
    ok(&first)?;
    let folder = sandbox.work().join("dataset");
    assert_eq!(text(&first.stdout)?, format!("{}\n", folder.display()));
    holds_the_archive(&folder)?;
    assert!(!zip_object(&sandbox.cache()).exists());
    let second = sandbox.run(&["--json", "get", &server.url])?;
    ok(&second)?;
    let document = json(&second)?;
    assert_eq!(document["work"]["bytes_down"], 0);
    assert_eq!(
        document["result"]["files"].as_array().map(Vec::len),
        Some(FILES.len())
    );
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"z1\""), "{asked}");
    holds_the_archive(&folder)?;
    Ok(())
}

#[test]
fn the_cache_counts_an_archive_by_the_files_it_unpacked_to() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    assert_eq!(
        added["work"]["objects_written"],
        FILES.len() + 1,
        "the zip and each file it holds"
    );
    let info = json(&sandbox.run(&["--json", "cache"])?)?;
    let bytes: usize = FILES.iter().map(|(_, bytes)| bytes.len()).sum();
    assert_eq!(info["result"]["needed"], bytes);
    assert_eq!(info["result"]["bytes"], bytes);
    Ok(())
}

#[test]
fn get_unpacks_a_tar_gz_into_a_folder_named_without_tar() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/dataset.tar.gz",
        vec![file(&fixture("python.tar.gz")?, "\"t1\"")],
    )?;
    ok(&sandbox.run(&["get", &server.url])?)?;
    holds_the_archive(&sandbox.work().join("dataset"))
}

#[test]
fn an_archive_only_the_server_names_moves_nothing_the_second_time() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let length = zip.len().to_string();
    let named = reply(
        "200 OK",
        &[
            ("Content-Length", &length),
            ("ETag", "\"z1\""),
            (
                "Content-Disposition",
                "attachment; filename=\"dataset.zip\"",
            ),
        ],
        &zip,
    );
    let unchanged = reply("304 Not Modified", &[("ETag", "\"z1\"")], b"");
    let server = Server::start("/download/5", vec![named, unchanged])?;
    ok(&sandbox.run(&["get", &server.url])?)?;
    let folder = sandbox.work().join("dataset");
    holds_the_archive(&folder)?;
    let second = sandbox.run(&["--json", "get", &server.url])?;
    ok(&second)?;
    assert_eq!(json(&second)?["work"]["bytes_down"], 0);
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"z1\""), "{asked}");
    holds_the_archive(&folder)
}

#[test]
fn get_with_no_extract_keeps_the_zip_packed() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["get", "--no-extract", &server.url])?)?;
    let folder = sandbox.work().join("dataset");
    assert_eq!(fs::read(folder.join("dataset.zip"))?, zip);
    assert_eq!(fs::read_dir(&folder)?.count(), 1, "only the zip");
    Ok(())
}

#[test]
fn add_with_no_extract_writes_extract_false() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["add", "--no-extract", &server.url])?)?;
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(manifest.contains("extract = false"), "{manifest}");
    let folder = sandbox.work().join("data").join("dataset");
    assert_eq!(fs::read(folder.join("dataset.zip"))?, zip);
    Ok(())
}

#[test]
fn removing_a_dataset_keeps_files_of_an_archive_the_cache_never_saw() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    let notes = Server::start("/files/notes.csv", vec![file(b"a,b\n", "\"n1\"")])?;
    ok(&sandbox.run(&["add", &zip.url])?)?;
    ok(&sandbox.run(&["add", &notes.url])?)?;
    let removed = sandbox
        .command_in(&sandbox.work(), &["remove", "notes"])
        .env("FETCHLOOM_CACHE", sandbox.folder("cold")?)
        .output()?;
    ok(&removed)?;
    assert!(!sandbox.work().join("data").join("notes").exists());
    holds_the_archive(&sandbox.work().join("data").join("dataset"))
}

#[test]
fn an_archive_another_project_places_as_a_file_stays_in_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    // python-nested.zip holds python.zip, byte for byte, under nested/
    let nested = Server::start(
        "/files/nested.zip",
        vec![file(&fixture("python-nested.zip")?, "\"n1\"")],
    )?;
    ok(&sandbox.run(&["add", &nested.url])?)?;
    let other = sandbox.folder("other")?;
    let zip = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    ok(&sandbox.command_in(&other, &["add", &zip.url]).output()?)?;
    holds_the_archive(&other.join("data").join("dataset"))?;
    assert!(zip_object(&sandbox.cache()).exists());
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    ok(&sandbox
        .command_in(&clone, &["sync", "--offline"])
        .output()?)?;
    assert_eq!(
        fs::read(clone.join("data").join("nested").join("python.zip"))?,
        fixture("python.zip")?
    );
    Ok(())
}

#[test]
fn get_drops_an_archive_it_unpacked_on_an_earlier_run() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"z1\"")], b"");
    let server = Server::start(
        PATH,
        vec![file(&fixture("python.zip")?, "\"z1\""), unchanged],
    )?;
    let taken = sandbox.work().join("dataset");
    fs::create_dir(&taken)?;
    fs::write(taken.join("notes.txt"), "mine")?;
    let refused = sandbox.run(&["get", &server.url])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    assert!(
        zip_object(&sandbox.cache()).exists(),
        "refused before placing"
    );
    ok(&sandbox.run(&["get", &server.url, "-o", "fresh"])?)?;
    holds_the_archive(&sandbox.work().join("fresh"))?;
    assert!(
        !zip_object(&sandbox.cache()).exists(),
        "its files are in, so its own bytes go"
    );
    Ok(())
}

#[test]
fn a_file_the_server_never_named_an_archive_needs_its_own_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let named = Server::start(PATH, vec![file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["get", &named.url])?)?;
    let bare = Server::start("/download/12345", vec![file(&zip, "\"z1\"")])?;
    ok(&sandbox.run(&["get", &bare.url])?)?;
    assert_eq!(fs::read(sandbox.work().join("12345").join("12345"))?, zip);
    // as cache gc would, once the object is a day old
    delete(&zip_object(&sandbox.cache()))?;
    let offline = sandbox.run(&["--offline", "get", &bare.url, "-o", "fresh"])?;
    let stderr = text(&offline.stderr)?;
    assert_eq!(offline.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains("is not in the cache"), "{stderr}");
    Ok(())
}

#[test]
fn an_archive_that_holds_nothing_still_leaves_its_folder() -> Result {
    let sandbox = Sandbox::new()?;
    // the end of a zip's central directory, and nothing before it
    let mut empty = b"PK\x05\x06".to_vec();
    empty.extend([0; 18]);
    let server = Server::start("/files/empty.zip", vec![file(&empty, "\"e1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    ok(&output)?;
    let folder = text(&output.stdout)?;
    assert!(Path::new(folder.trim()).is_dir(), "{folder}");
    Ok(())
}

fn directory_only_archive() -> Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive.add_directory("empty/", zip::write::SimpleFileOptions::default())?;
    Ok(archive.finish()?.into_inner())
}

#[test]
fn add_of_an_empty_archive_does_not_offer_a_path() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = directory_only_archive()?;
    let server = Server::start("/empty.zip", vec![file(&zip, "\"empty\"")])?;
    let output = sandbox.run(&["add", &server.url, "--as", "empty"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("added empty. its archives contain no regular files"),
        "{stderr}"
    );
    assert!(!stderr.contains("fetchloom path"), "{stderr}");
    assert!(!sandbox.work().join("data/empty").exists());
    let again = sandbox.run(&["add", &server.url])?;
    ok(&again)?;
    let stderr = text(&again.stderr)?;
    assert!(
        stderr.contains("empty is already in data.toml. its archives contain no regular files"),
        "{stderr}"
    );
    assert!(!stderr.contains("its files are in"), "{stderr}");
    Ok(())
}

#[test]
fn path_of_an_empty_archive_explains_why_it_has_no_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = directory_only_archive()?;
    let server = Server::start(
        "/empty.zip",
        vec![file(&zip, "\"empty\""), file(&zip, "\"empty\"")],
    )?;
    ok(&sandbox.run(&["add", &server.url, "--as", "empty"])?)?;
    for target in ["empty", "empty/missing"] {
        let output = sandbox.run(&["path", target])?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        assert!(output.stdout.is_empty(), "{}", text(&output.stdout)?);
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.contains("empty has no regular files after extraction"),
            "{stderr}"
        );
        assert!(stderr.contains("extract = false"), "{stderr}");
    }
    fs::remove_dir_all(sandbox.cache().join("unpacked"))?;
    let output = sandbox.run(&["path", "empty", "--json"])?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        "empty has no regular files after extraction"
    );
    assert!(document["result"].is_null());
    assert!(!sandbox.work().join("data/empty").exists());
    assert_eq!(
        server.requests().len(),
        2,
        "path retried after rebuilding the missing archive record"
    );
    Ok(())
}
