//! archives: unpacked into projects and folders, checked against data.lock, kept out of the cache
//! once their files are in, and refused whole when unsafe.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Output;

use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};

use crate::support::{Result, Sandbox, Server, delete, edit, file, json, reply, text};

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
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1);
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

#[test]
fn a_fresh_clone_unpacks_what_the_lock_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\""), file(&zip, "\"z1\"")])?;
    sandbox.allow(&server.url);
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
    sandbox.allow(&server.url);
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
    let told = text(&first.stderr)?;
    for line in [
        "README.md  17 B",
        "data/      3 files · 16 B",
        "long/      1 file · 10 B",
    ] {
        assert!(told.contains(line), "the top level is listed: {told}");
    }
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
fn a_select_on_the_links_own_name_holds_when_the_server_names_the_file() -> Result {
    let sandbox = Sandbox::new()?;
    let named = reply(
        "200 OK",
        &[
            ("Content-Length", "4"),
            ("ETag", "\"n1\""),
            ("Content-Disposition", "attachment; filename=\"iris.csv\""),
        ],
        b"a,b\n",
    );
    let server = Server::start("/download.csv", vec![named])?;
    let output = sandbox.run(&["add", &server.url, "--select", "download.csv"])?;
    ok(&output)?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains("path = \"iris.csv\""), "{lock}");
    Ok(())
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

/// the files python.zip holds, under the same top folder, in one solid 7z block.
fn seven_zip() -> Result<Vec<u8>> {
    let mut entries = Vec::new();
    let mut readers = Vec::new();
    for (path, bytes) in FILES {
        let mut entry = ArchiveEntry::new_file(&format!("dataset/{path}"));
        entry.has_stream = !bytes.is_empty();
        entries.push(entry);
        readers.push(SourceReader::from(bytes));
    }
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()))?;
    writer.push_archive_entries(entries, readers)?;
    Ok(writer.finish()?.into_inner())
}

#[test]
fn a_7z_unpacks_and_no_extract_keeps_it_packed() -> Result {
    let seven = seven_zip()?;
    let server = Server::start(
        "/files/dataset.7z",
        vec![file(&seven, "\"s1\""), file(&seven, "\"s1\"")],
    )?;
    let unpacked = Sandbox::new()?;
    ok(&unpacked.run(&["add", &server.url])?)?;
    holds_the_archive(&unpacked.work().join("data").join("dataset"))?;
    let lock = fs::read_to_string(unpacked.work().join("data.lock"))?;
    assert!(
        lock.contains(&format!("unpacks_to = \"sha256:{TREE}\"")),
        "{lock}"
    );
    let packed = Sandbox::new()?;
    ok(&packed.run(&["add", "--no-extract", &server.url])?)?;
    let folder = packed.work().join("data").join("dataset");
    assert_eq!(fs::read(folder.join("dataset.7z"))?, seven);
    assert_eq!(fs::read_dir(&folder)?.count(), 1, "only the 7z");
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
        .command(&["remove", "notes"])
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
    ok(&sandbox.run(&["get", &server.url, "--dir", "fresh"])?)?;
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
    let offline = sandbox.run(&["--offline", "get", &bare.url, "--dir", "fresh"])?;
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
        stderr.contains("Added empty, its archives contain no regular files"),
        "{stderr}"
    );
    assert!(!stderr.contains("fetchloom path"), "{stderr}");
    assert!(!sandbox.work().join("data/empty").exists());
    let again = sandbox.run(&["add", &server.url])?;
    ok(&again)?;
    let stderr = text(&again.stderr)?;
    assert!(
        stderr.contains("empty is already in data.toml, its archives contain no regular files"),
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
    sandbox.allow(&server.url);
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

/// python-v7.tar holds dataset/README.md and dataset/data/train.csv, and train.csv's bytes start
/// at byte 1536.
const V7_SHA256: &str = "9a2f3adae7925e1f1db798d92b4c394d2883f42aeb04ee00363e6e851ab56fdd";
const V7_TREE: &str = "aac41d8ac40805957f5260c833a02f89a62665783d5a02ef907b532a205ff4fd";

/// a tar holds no checksum over its files, so a byte changed in its cached copy unpacks to other
/// files without any error. that copy is fetched again, and what it unpacked to is never
/// recorded, so neither this sync nor an upgrade pins the damage.
#[test]
fn a_cached_archive_whose_bytes_changed_is_fetched_again() -> Result {
    let sandbox = Sandbox::new()?;
    let tar = fixture("python-v7.tar")?;
    let server = Server::start(
        "/files/set.tar",
        (0..4).map(|_| file(&tar, "\"t1\"")).collect(),
    )?;
    sandbox.allow(&server.url);
    // a project that keeps the tar packed keeps it in the cache
    let packed = sandbox.folder("packed")?;
    ok(&sandbox
        .command_in(&packed, &["add", "--no-extract", &server.url])
        .output()?)?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let cache = sandbox.cache();
    let object = |hex: &str| cache.join("objects").join(&hex[..2]).join(&hex[2..]);
    let record = cache.join("unpacked").join(V7_SHA256);
    let pinned = fs::read_to_string(&record)?;
    let mut damaged = tar.clone();
    damaged[1536] = b'X';
    edit(&object(V7_SHA256), &damaged)?;
    // as gc would once README.md's object is a day old, so the tar must be unpacked again
    let readme = pinned
        .lines()
        .find_map(|line| line.strip_prefix("dataset/README.md\t"))
        .and_then(|line| line.rsplit('\t').next())
        .ok_or("README.md is in the record")?;
    delete(&object(readme))?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }

    let synced = sandbox.command_in(&clone, &["sync"]).output()?;

    assert_eq!(fs::read_to_string(&record)?, pinned, "the record changed");
    ok(&synced)?;
    let set = clone.join("data").join("set");
    assert_eq!(
        fs::read(set.join("data").join("train.csv"))?,
        b"x,y\n1,2\n3,4\n"
    );
    assert_eq!(fs::read(object(V7_SHA256))?, tar);
    ok(&sandbox
        .command_in(&clone, &["lock", "--upgrade"])
        .output()?)?;
    let lock = fs::read_to_string(clone.join("data.lock"))?;
    assert!(
        lock.contains(&format!("unpacks_to = \"sha256:{V7_TREE}\"")),
        "{lock}"
    );
    Ok(())
}

#[test]
fn path_checks_known_archive_members_before_syncing() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\""), file(&zip, "\"z1\"")])?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    for known in [true, false] {
        let clone = sandbox.folder(if known {
            "known-clone"
        } else {
            "unknown-clone"
        })?;
        for name in ["data.toml", "data.lock"] {
            fs::copy(sandbox.work().join(name), clone.join(name))?;
        }
        let mut command = sandbox.command_in(&clone, &["--json", "path", "dataset/no.csv"]);
        if !known {
            command.env("FETCHLOOM_CACHE", sandbox.folder("cold-cache")?);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
        assert_eq!(clone.join("data").exists(), !known);
        if !known {
            holds_the_archive(&clone.join("data").join("dataset"))?;
        }
    }
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

#[test]
fn a_cached_archive_with_crc_damage_is_fetched_again() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, (0..4).map(|_| file(&zip, "\"z1\"")).collect())?;
    sandbox.allow(&server.url);
    let packed = sandbox.folder("packed")?;
    ok(&sandbox
        .command_in(&packed, &["add", "--no-extract", &server.url])
        .output()?)?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let cache = sandbox.cache();
    let record = cache.join("unpacked").join(ZIP_SHA256);
    let pinned = fs::read_to_string(&record)?;
    let mut damaged = zip.clone();
    let central = damaged
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .ok_or("zip has a central directory")?;
    damaged[central + 16] ^= 1;
    let permissions = fs::metadata(zip_object(&cache))?.permissions();
    edit(&zip_object(&cache), &damaged)?;
    fs::set_permissions(zip_object(&cache), permissions)?;
    let readme = pinned
        .lines()
        .find_map(|line| line.strip_prefix("dataset/README.md\t"))
        .and_then(|line| line.rsplit('\t').next())
        .ok_or("README.md is in the record")?;
    delete(&cache.join("objects").join(&readme[..2]).join(&readme[2..]))?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let requests = server.requests().len();
    let synced = sandbox.command_in(&clone, &["sync"]).output()?;
    ok(&synced)?;
    holds_the_archive(&clone.join("data").join("dataset"))?;
    assert_eq!(fs::read_to_string(&record)?, pinned);
    assert_eq!(fs::read(zip_object(&cache))?, zip);
    assert_eq!(server.requests().len(), requests + 1);
    Ok(())
}

#[test]
fn sync_drops_an_archive_extracted_by_a_failed_get() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, (0..3).map(|_| file(&zip, "\"z1\"")).collect())?;
    let taken = sandbox.work().join("dataset");
    fs::create_dir(&taken)?;
    fs::write(taken.join("notes.txt"), b"mine")?;
    let refused = sandbox.run(&["get", &server.url])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    assert!(zip_object(&sandbox.cache()).exists());
    assert!(sandbox.cache().join("unpacked").join(ZIP_SHA256).is_file());
    let added = sandbox.run(&["add", &server.url])?;
    ok(&added)?;
    holds_the_archive(&sandbox.work().join("data").join("dataset"))?;
    assert!(
        !zip_object(&sandbox.cache()).exists(),
        "placed cached members left their packed archive behind"
    );
    Ok(())
}

/// an archive past a limit says which setting lifts it, for a person who trusts the archive.
#[test]
fn an_archive_past_a_limit_names_the_setting_that_lifts_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_ARCHIVE_FILES", "3")
        .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["message"],
        "the archive holds more than 3 entries, the most fetchloom unpacks from one archive"
    );
    assert_eq!(
        document["errors"][0]["help"],
        "if you trust the archive, raise FETCHLOOM_ARCHIVE_FILES, or archive_files in the settings file. otherwise keep the archive packed with extract = false on its dataset in data.toml, or with --no-extract"
    );
    Ok(())
}

/// the start of a rar5 archive. fetchloom unpacks no rar, so the name alone keeps it packed and
/// nothing reads further.
const RAR: &[u8] = b"Rar!\x1a\x07\x01\x00no decoder reads this";

/// the help a warning about a rar left packed gives on this system: a command that unpacks the
/// rar at `path` into sample-unpacked, then what stops the warning.
fn next_step(path: &str, stop: &str) -> String {
    next_step_into(path, "sample-unpacked", stop)
}

/// [`next_step`] for a rar that unpacks into `folder`.
fn next_step_into(path: &str, folder: &str, stop: &str) -> String {
    if cfg!(windows) {
        format!(
            r#"unpack it with 7-Zip: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x {} -o{folder}`. {stop} stops this warning"#,
            path.replace('/', "\\")
        )
    } else if cfg!(target_os = "macos") {
        format!(
            "unpack it with unar: `brew install unar`, then `unar -D -o {folder} {path}`. {stop} stops this warning"
        )
    } else {
        format!(
            "unpack it with unrar from your package manager: `unrar x {path} {folder}/`. {stop} stops this warning"
        )
    }
}

/// a rar stays packed, and its warning names a command for this system that unpacks it into a
/// folder outside the data folder, which status leaves alone.
#[test]
fn rar_stays_packed_with_a_next_step_for_this_system() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/sample.rar", vec![file(RAR, "\"r1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    ok(&added)?;
    let stderr = text(&added.stderr)?;
    let warned = format!(
        "warning: sample/sample.rar stays packed, since fetchloom does not unpack rar\n  help: {}\n",
        next_step("data/sample/sample.rar", "extract = false")
    );
    assert!(stderr.contains(&warned), "{stderr}");
    assert_eq!(
        fs::read(sandbox.work().join("data/sample/sample.rar"))?,
        RAR
    );
    // what the command leaves behind
    let unpacked = sandbox.folder("work/sample-unpacked")?;
    fs::write(unpacked.join("clip.avi"), b"frames")?;
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

#[test]
fn get_says_how_to_unpack_a_rar_it_keeps_packed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/sample.rar", vec![file(RAR, "\"r1\"")])?;
    let got = sandbox.run(&["--json", "get", &server.url])?;
    ok(&got)?;
    let warning = format!(
        "sample.rar stays packed, since fetchloom does not unpack rar; {}",
        next_step("sample/sample.rar", "--no-extract")
    );
    assert_eq!(json(&got)?["warnings"], serde_json::json!([warning]));
    assert_eq!(fs::read(sandbox.work().join("sample/sample.rar"))?, RAR);
    Ok(())
}

/// webdataset shards: many numbered tars, which loaders read packed. a server stands in for S3,
/// which only debug builds can be pointed at.
#[cfg(debug_assertions)]
mod shards {
    use super::{Output, Result, Sandbox, file, fs, ok, text};
    use crate::buckets::{md5, s3_listing, s3_page};
    use crate::support::Routes;

    const LINK: &str = "s3://shard-bucket/shards/";

    /// the hint's start for the 8 shards of [`bucket`] in a dataset named shards.
    const HINT: &str = "shards holds 8 numbered .tar shards, which webdataset loaders read packed. keep them packed with";

    /// a tar holding one file, `name`.
    fn tar_of(name: &str, body: &[u8]) -> Result<Vec<u8>> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, body)?;
        Ok(builder.into_inner()?)
    }

    /// a bucket whose `shards/` prefix holds 8 numbered tar shards of one file each.
    fn bucket() -> Result<Routes> {
        let base = "/shard-bucket.s3.amazonaws.com/";
        let mut objects = Vec::new();
        for number in 0..8 {
            let body = tar_of(&format!("sample-{number}.txt"), b"x")?;
            objects.push((format!("shards/train-{number:06}.tar"), body));
        }
        let listed: Vec<(&str, &[u8])> = objects
            .iter()
            .map(|(key, body)| (key.as_str(), body.as_slice()))
            .collect();
        let mut routes = vec![(s3_listing(base, "shards/", None), s3_page(&listed, None))];
        for (key, body) in &objects {
            routes.push((
                format!("{base}{key}"),
                file(body, &format!("\"{}\"", md5(body))),
            ));
        }
        Routes::start(routes)
    }

    fn run(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Result<Output> {
        Ok(sandbox
            .command(args)
            .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
            .output()?)
    }

    /// the hint comes after the listing and before anything downloads, while the choice is free.
    fn hinted_before_downloading(stderr: &str, hint: &str) {
        let hinted = stderr.find(hint);
        let downloaded = stderr.find("Downloaded");
        assert!(
            hinted.is_some() && hinted < downloaded,
            "{hint:?} before any download in:\n{stderr}"
        );
    }

    #[test]
    fn adding_shards_hints_no_extract_before_anything_downloads() -> Result {
        let sandbox = Sandbox::new()?;
        let routes = bucket()?;
        let added = run(&sandbox, &routes, &["add", LINK, "--as", "shards"])?;
        ok(&added)?;
        hinted_before_downloading(
            &text(&added.stderr)?,
            &format!("  hint: {HINT} --no-extract, or extract = false in data.toml\n"),
        );
        assert_eq!(
            fs::read(sandbox.work().join("data/shards/sample-7.txt"))?,
            b"x"
        );
        Ok(())
    }

    #[test]
    fn a_sync_hints_only_for_a_new_entry_of_shards() -> Result {
        let sandbox = Sandbox::new()?;
        let routes = bucket()?;
        let manifest = sandbox.work().join("data.toml");
        let entry = format!("[datasets.shards]\nref = \"{LINK}\"\n");
        fs::write(&manifest, &entry)?;
        let first = run(&sandbox, &routes, &["sync"])?;
        ok(&first)?;
        hinted_before_downloading(
            &text(&first.stderr)?,
            &format!("  hint: {HINT} extract = false in data.toml\n"),
        );
        let unhinted = |args: &[&str]| -> Result {
            let output = run(&sandbox, &routes, args)?;
            ok(&output)?;
            let stderr = text(&output.stderr)?;
            assert!(!stderr.contains("webdataset"), "{args:?}: {stderr}");
            Ok(())
        };
        // a later sync keeps the entry, an upgrade lists the entry already hinted, and a switch
        // of extract is a choice the person just made
        unhinted(&["sync"])?;
        unhinted(&["lock", "--upgrade"])?;
        fs::write(&manifest, format!("{entry}extract = false\n"))?;
        unhinted(&["sync"])?;
        fs::write(&manifest, &entry)?;
        unhinted(&["sync"])
    }

    #[test]
    fn get_hints_no_extract_for_shards() -> Result {
        let sandbox = Sandbox::new()?;
        let routes = bucket()?;
        let got = run(&sandbox, &routes, &["get", LINK])?;
        ok(&got)?;
        hinted_before_downloading(
            &text(&got.stderr)?,
            &format!(
                "  hint: {LINK} holds 8 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract\n"
            ),
        );
        Ok(())
    }

    #[test]
    fn no_hint_when_shards_stay_packed_or_the_run_is_quiet() -> Result {
        let routes = bucket()?;
        for args in [
            &["add", "--no-extract", LINK][..],
            &["get", "--no-extract", LINK],
            &["--json", "add", LINK],
            &["-q", "get", LINK],
        ] {
            let sandbox = Sandbox::new()?;
            let output = run(&sandbox, &routes, args)?;
            ok(&output)?;
            for told in [&output.stdout, &output.stderr] {
                let told = text(told)?;
                assert!(!told.contains("webdataset"), "{args:?}: {told}");
            }
        }
        Ok(())
    }
}

/// a members table changes which files of an archive land, not what data.lock pins: editing it
/// asks no server, `sync --locked` takes it, and taking it out brings the file back from the
/// cache.
#[test]
fn a_member_filter_edit_makes_no_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    fs::write(
        &manifest,
        format!(
            "{plain}\n[datasets.dataset.members.\"dataset.zip\"]\nexclude = [\"dataset/data/train.csv\"]\n"
        ),
    )?;
    let filtered = sandbox.run(&["--json", "sync", "--locked"])?;
    ok(&filtered)?;
    let document = json(&filtered)?;
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(document["result"]["lock_changed"], false);
    assert_eq!(document["result"]["placement"]["removed"], 1);
    let folder = sandbox.work().join("data").join("dataset");
    assert!(!folder.join("data/train.csv").exists());
    assert_eq!(fs::read(folder.join("README.md"))?, FILES[0].1);
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );

    fs::write(&manifest, plain)?;
    let undone = sandbox.run(&["--json", "sync", "--locked"])?;
    ok(&undone)?;
    assert_eq!(json(&undone)?["work"]["requests"], 0);
    holds_the_archive(&folder)?;
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// gc frees a file inside an archive that only a members table leaves out, and keeps every file
/// the project places.
#[test]
fn gc_frees_a_file_a_members_table_leaves_out() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&fixture("python.zip")?, "\"z1\"")])?;
    ok(&sandbox.run(&["add", &server.url])?)?;
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{plain}\n[datasets.dataset.members.\"dataset.zip\"]\nexclude = [\"dataset/data/train.csv\"]\n"
        ),
    )?;
    ok(&sandbox.run(&["sync"])?)?;
    let object = |bytes: &[u8]| {
        let hex = crate::add::sha256_hex(bytes);
        sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
    };
    let (readme, train) = (FILES[0].1, FILES[3].1);
    assert!(object(train).exists());
    ok(&sandbox.run(&["cache", "gc", "--now"])?)?;
    assert!(!object(train).exists(), "no project places train.csv");
    assert!(object(readme).exists(), "the project places README.md");
    Ok(())
}

/// the walkthrough: an exclude written against a file inside the zip, as the data folder shows
/// it, matches no file the source lists, so the file stays. status hints and sync warns, each
/// naming the drop that leaves it out.
#[test]
fn an_exclude_naming_a_member_warns_and_points_at_drop() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = fixture("python.zip")?;
    let server = Server::start(PATH, vec![file(&zip, "\"z1\""), file(&zip, "\"z1\"")])?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!("{plain}exclude = [\"data/train.csv\"]\n"),
    )?;
    let said = "exclude \"data/train.csv\" in datasets.dataset names a file inside dataset.zip, which exclude does not reach";
    let help = "run `fetchloom drop data/dataset/data/train.csv`";
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains(&format!("  hint: {said}; {help}\n")),
        "{status}"
    );
    let synced = sandbox.run(&["sync"])?;
    ok(&synced)?;
    let stderr = text(&synced.stderr)?;
    assert!(
        stderr.contains(&format!("warning: {said}\n  help: {help}\n")),
        "{stderr}"
    );
    let train = sandbox.work().join("data/dataset/data/train.csv");
    assert!(train.exists(), "exclude does not reach it");
    ok(&sandbox.run(&["drop", "data/dataset/data/train.csv"])?)?;
    assert!(!train.exists());
    let stderr = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(!stderr.contains("warning"), "{stderr}");
    Ok(())
}

/// archives cut into parts, as `split` and 7-Zip cut them: `scans.zip.001` and on.
mod split {
    use std::path::{Path, PathBuf};

    use super::{RAR, Result, Sandbox, Server, file, fs, next_step_into, ok, text};

    /// five files under one top folder, as a dataset's authors zip them.
    const SCANS: [(&str, &[u8]); 5] = [
        ("scans/a.csv", b"a\n"),
        ("scans/b.csv", b"b\n"),
        ("scans/c/d.csv", b"d\n"),
        ("scans/e.txt", b"e\n"),
        ("scans/f.txt", b"f\n"),
    ];

    fn scans_zip() -> Result<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (path, bytes) in SCANS {
            zip.start_file(path, zip::write::SimpleFileOptions::default())?;
            std::io::Write::write_all(&mut zip, bytes)?;
        }
        Ok(zip.finish()?.into_inner())
    }

    /// `bytes` cut into as many parts as `names`, of nearly one size, each written to
    /// `folder` under its name.
    fn cut(folder: &Path, bytes: &[u8], names: &[&str]) -> Result<Vec<Vec<u8>>> {
        let size = bytes.len().div_ceil(names.len());
        let parts: Vec<Vec<u8>> = bytes.chunks(size).map(<[u8]>::to_vec).collect();
        for (name, part) in names.iter().zip(&parts) {
            fs::write(folder.join(name), part)?;
        }
        Ok(parts)
    }

    /// every file below `folder`, by its path from there.
    fn files_in(folder: &Path) -> Result<Vec<String>> {
        let mut found = Vec::new();
        let mut folders = vec![folder.to_path_buf()];
        while let Some(next) = folders.pop() {
            for entry in fs::read_dir(next)? {
                let path = entry?.path();
                if path.is_dir() {
                    folders.push(path);
                } else {
                    let below = path.strip_prefix(folder)?.to_string_lossy();
                    found.push(below.replace('\\', "/"));
                }
            }
        }
        found.sort();
        Ok(found)
    }

    fn object(cache: &Path, bytes: &[u8]) -> PathBuf {
        let digest = crate::add::sha256_hex(bytes);
        cache.join("objects").join(&digest[..2]).join(&digest[2..])
    }

    /// d4's acceptance on a folder: two halves of a zip of five files place those five files,
    /// pinned at the first part, and the halves leave the cache once the files are in.
    #[test]
    fn a_folder_of_parts_unpacks_as_one_archive() -> Result {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("scans")?;
        let zip = scans_zip()?;
        let halves = cut(&folder, &zip, &["scans.zip.001", "scans.zip.002"])?;
        ok(&sandbox.run(&["add", "../scans"])?)?;
        let data = sandbox.work().join("data/scans");
        assert_eq!(
            files_in(&data)?,
            ["a.csv", "b.csv", "c/d.csv", "e.txt", "f.txt"]
        );
        for (path, bytes) in SCANS {
            assert_eq!(fs::read(data.join(&path["scans/".len()..]))?, bytes);
        }
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        let line = |name: &str| {
            lock.lines()
                .find(|line| line.contains(name))
                .unwrap_or_default()
        };
        assert!(
            line("scans.zip.001").contains("unpacks_to = \"sha256:"),
            "{lock}"
        );
        assert!(!line("scans.zip.002").is_empty() && !line("scans.zip.002").contains("unpacks_to"));
        assert_eq!(lock.matches("unpacks_to").count(), 1, "{lock}");
        for half in &halves {
            assert!(
                !object(&sandbox.cache(), half).exists(),
                "a part left the cache"
            );
        }
        let status = sandbox.run(&["status"])?;
        assert_eq!(
            text(&status.stderr)?,
            "         Ok everything matches data.lock by size and time\n"
        );
        // another project on this machine places the files from what the parts unpacked to
        let clone = sandbox.folder("clone")?;
        for name in ["data.toml", "data.lock"] {
            fs::copy(sandbox.work().join(name), clone.join(name))?;
        }
        ok(&sandbox
            .command_in(&clone, &["--offline", "sync"])
            .output()?)?;
        assert_eq!(files_in(&clone.join("data/scans"))?.len(), 5);
        Ok(())
    }

    #[test]
    fn a_missing_part_is_named_and_the_others_stay_as_they_are() -> Result {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("scans")?;
        let names = ["scans.zip.001", "scans.zip.002", "scans.zip.003"];
        let parts = cut(&folder, &scans_zip()?, &names)?;
        fs::remove_file(folder.join("scans.zip.002"))?;
        let added = sandbox.run(&["add", "../scans"])?;
        ok(&added)?;
        assert!(
            text(&added.stderr)?.contains(
                "warning: scans/scans.zip.002 is missing, so the parts of scans.zip stay as they are\n  help: fetchloom joins a split archive only from every one of its parts: check that select and exclude keep them all, or ask the dataset's authors for the missing one\n"
            ),
            "{}",
            text(&added.stderr)?
        );
        let data = sandbox.work().join("data/scans");
        assert_eq!(files_in(&data)?, ["scans.zip.001", "scans.zip.003"]);
        assert_eq!(fs::read(data.join("scans.zip.003"))?, parts[2]);
        let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
        assert!(!lock.contains("unpacks_to"), "{lock}");
        Ok(())
    }

    /// the help for a zip spanned over `.z01` files, on this system.
    fn join_step(path: &str, name: &str) -> String {
        if cfg!(windows) {
            format!(
                r#"unpack it with 7-Zip, which reads its parts: `winget install 7zip.7zip`, then `& "C:\Program Files\7-Zip\7z.exe" x {} -o{name}-unpacked`. extract = false stops this warning"#,
                path.replace('/', "\\")
            )
        } else {
            let zip = if cfg!(target_os = "macos") {
                "the zip macos has"
            } else {
                "zip from your package manager"
            };
            format!(
                "join its parts with {zip}, then unpack them: `zip -s 0 {path} --out {name}-joined.zip`, then `unzip {name}-joined.zip -d {name}-unpacked`. extract = false stops this warning"
            )
        }
    }

    /// rar volumes and a zip spanned over `.z01` files stay packed, each told once, with what
    /// reads all of its volumes.
    #[test]
    fn rar_volumes_and_a_spanned_zip_stay_packed_with_one_next_step_each() -> Result {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("shots")?;
        for name in ["clips.part1.rar", "clips.part2.rar"] {
            fs::write(folder.join(name), RAR)?;
        }
        fs::write(folder.join("maps.z01"), b"PK\x07\x08the first volume")?;
        fs::write(
            folder.join("maps.zip"),
            b"the last volume, and its directory",
        )?;
        let added = sandbox.run(&["add", "../shots"])?;
        ok(&added)?;
        let stderr = text(&added.stderr)?;
        let rar = format!(
            "warning: shots/clips.part1.rar stays packed with 1 other volume of its rar, since fetchloom does not unpack rar\n  help: {}\n",
            next_step_into(
                "data/shots/clips.part1.rar",
                "clips.part1-unpacked",
                "extract = false"
            )
        );
        let zip = format!(
            "warning: shots/maps.zip stays packed with 1 other file it spans, since fetchloom does not unpack a zip spanned over several files\n  help: {}\n",
            join_step("data/shots/maps.zip", "maps")
        );
        assert!(stderr.contains(&rar), "{stderr}");
        assert!(stderr.contains(&zip), "{stderr}");
        assert_eq!(stderr.matches("warning:").count(), 2, "{stderr}");
        assert_eq!(
            files_in(&sandbox.work().join("data/shots"))?,
            ["clips.part1.rar", "clips.part2.rar", "maps.z01", "maps.zip"]
        );
        Ok(())
    }

    /// parts in a bucket prefix, which only debug builds can point at a server of their own.
    #[cfg(debug_assertions)]
    mod bucket {
        use super::super::{Output, ok, text};
        use super::{SCANS, Sandbox, cut, files_in, fs, scans_zip};
        use crate::buckets::{md5, s3_listing, s3_page};
        use crate::support::{Result, Routes, file};

        const LINK: &str = "s3://split-bucket/scans/";

        /// a bucket whose `scans/` prefix holds two halves of a zip of five files, and the halves.
        fn bucket() -> Result<(Routes, Vec<Vec<u8>>)> {
            let base = "/split-bucket.s3.amazonaws.com/";
            let folder = tempfile::tempdir()?;
            let halves = cut(folder.path(), &scans_zip()?, &["001", "002"])?;
            let keys = ["scans/scans.zip.001", "scans/scans.zip.002"];
            let listed: Vec<(&str, &[u8])> = keys
                .iter()
                .zip(&halves)
                .map(|(key, half)| (*key, half.as_slice()))
                .collect();
            let mut routes = vec![(s3_listing(base, "scans/", None), s3_page(&listed, None))];
            for (key, half) in keys.iter().zip(&halves) {
                for _ in 0..2 {
                    routes.push((
                        format!("{base}{key}"),
                        file(half, &format!("\"{}\"", md5(half))),
                    ));
                }
            }
            Ok((Routes::start(routes)?, halves))
        }

        fn run(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> Result<Output> {
            Ok(sandbox
                .command(args)
                .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
                .output()?)
        }

        /// d4's acceptance on a bucket prefix: the five files land and no part does, and a
        /// clone with an empty cache downloads both parts once to place them.
        #[test]
        fn a_bucket_prefix_of_parts_unpacks_as_one_archive_here_and_in_a_clone() -> Result {
            let sandbox = Sandbox::new()?;
            let (routes, _) = bucket()?;
            let added = run(&sandbox, &routes, &["add", LINK, "--as", "scans"])?;
            ok(&added)?;
            assert!(!text(&added.stderr)?.contains("webdataset"));
            let placed = ["a.csv", "b.csv", "c/d.csv", "e.txt", "f.txt"];
            assert_eq!(files_in(&sandbox.work().join("data/scans"))?, placed);
            let clone = sandbox.folder("clone")?;
            for name in ["data.toml", "data.lock"] {
                fs::copy(sandbox.work().join(name), clone.join(name))?;
            }
            let cold = sandbox.folder("cold")?;
            let before = routes.requests().len();
            let synced = sandbox
                .command_in(&clone, &["sync"])
                .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
                .env("FETCHLOOM_CACHE", &cold)
                .output()?;
            ok(&synced)?;
            assert_eq!(files_in(&clone.join("data/scans"))?, placed);
            let asked = routes.requests()[before..].to_vec();
            assert_eq!(asked.len(), 2, "each part once: {asked:?}");
            for (path, bytes) in SCANS {
                let placed = clone.join("data/scans").join(&path["scans/".len()..]);
                assert_eq!(fs::read(placed)?, bytes);
            }
            Ok(())
        }

        /// export names each file by the first part it came from, and lists every part as a
        /// download.
        #[test]
        fn export_names_files_by_the_first_part_and_lists_every_part() -> Result {
            let sandbox = Sandbox::new()?;
            let (routes, _) = bucket()?;
            ok(&run(&sandbox, &routes, &["add", LINK, "--as", "scans"])?)?;
            let exported = sandbox.run(&["--json", "export"])?;
            ok(&exported)?;
            let result = &crate::support::json(&exported)?["result"];
            let from: Vec<&str> = result["placed"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|placed| placed["unpacked_from"].as_str())
                .collect();
            assert_eq!(from, ["scans.zip.001"; 5]);
            let sources: Vec<&str> = result["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|source| source["path"].as_str())
                .collect();
            assert_eq!(sources, ["scans.zip.001", "scans.zip.002"]);
            Ok(())
        }

        /// a file inside a split archive is dropped through the members table of its first
        /// part, and leaving every file out leaves out every part.
        #[test]
        fn drop_names_a_split_archive_by_its_first_part() -> Result {
            let sandbox = Sandbox::new()?;
            let (routes, _) = bucket()?;
            ok(&run(&sandbox, &routes, &["add", LINK, "--as", "scans"])?)?;
            ok(&sandbox.run(&["drop", "data/scans/a.csv"])?)?;
            let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
            assert!(
                manifest.contains("[datasets.scans.members.\"scans.zip.001\"]"),
                "{manifest}"
            );
            assert!(!sandbox.work().join("data/scans/a.csv").exists());
            assert_eq!(files_in(&sandbox.work().join("data/scans"))?.len(), 4);
            Ok(())
        }

        /// with extract = false the parts are files like any other, and switching back unpacks
        /// them from the cache without asking the bucket again.
        #[test]
        fn kept_packed_the_parts_are_files_and_switching_back_asks_nothing() -> Result {
            let sandbox = Sandbox::new()?;
            let (routes, halves) = bucket()?;
            let added = run(
                &sandbox,
                &routes,
                &["add", LINK, "--as", "scans", "--no-extract"],
            )?;
            ok(&added)?;
            let data = sandbox.work().join("data/scans");
            assert_eq!(files_in(&data)?, ["scans.zip.001", "scans.zip.002"]);
            assert_eq!(fs::read(data.join("scans.zip.002"))?, halves[1]);
            let manifest = sandbox.work().join("data.toml");
            let text_before = fs::read_to_string(&manifest)?;
            fs::write(&manifest, text_before.replace("extract = false\n", ""))?;
            let before = routes.requests().len();
            let switched = sandbox.run(&["--offline", "sync"])?;
            ok(&switched)?;
            assert_eq!(routes.requests().len(), before, "no request");
            assert_eq!(files_in(&data)?.len(), 5, "{}", text(&switched.stderr)?);
            let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
            assert_eq!(lock.matches("unpacks_to").count(), 1, "{lock}");
            Ok(())
        }
    }

    /// parts added each as a dataset of its own never join: a part that could start a split
    /// archive stays packed, with a warning that says where parts join, as `split -d` numbers
    /// them from `00`, or from `01` with `--numeric-suffixes=1`.
    #[test]
    fn parts_added_one_link_each_say_they_join_only_inside_one_dataset() -> Result {
        let sandbox = Sandbox::new()?;
        let folder = sandbox.folder("parts")?;
        let names = ["scans.zip.00", "scans.zip.01", "scans.zip.02"];
        let parts = cut(&folder, &scans_zip()?, &names)?;
        let mut servers = Vec::new();
        for (name, part) in names.iter().zip(&parts) {
            let server = Server::start(&format!("/files/{name}"), vec![file(part, "\"p\"")])?;
            sandbox.allow(&server.url);
            servers.push(server);
        }
        let links: Vec<&str> = servers.iter().map(|server| server.url.as_str()).collect();
        fs::write(sandbox.work().join("links.txt"), links.join("\n"))?;
        let added = sandbox.run(&["add", "-i", "links.txt"])?;
        ok(&added)?;
        let stderr = text(&added.stderr)?;
        for start in ["scans.zip.00", "scans.zip.01"] {
            assert!(
                stderr.contains(&format!("/{start} stays packed, since it does not unpack on its own\n  help: it may be the first part of an archive split into several files, which fetchloom joins only inside one dataset, such as a folder or a record that lists every part\n")),
                "{stderr}"
            );
        }
        assert_eq!(
            stderr.matches("warning: ").count(),
            4,
            "two taken names, two parts: {stderr}"
        );
        assert!(!fs::read_to_string(sandbox.work().join("data.lock"))?.contains("unpacks_to"));
        Ok(())
    }
}
