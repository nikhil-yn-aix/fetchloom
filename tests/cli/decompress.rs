//! single compressed files: unpacked only when asked, by `--decompress` or `decompress = true`, to
//! their name without the ending, pinned beside what they unpack to, and refused within the
//! archive limits the settings give, before a bomb fills a disk.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::support::{
    Result, Routes, Sandbox, Server, file, json, ok, reply, sha256_hex, tar_of, text, tree_of,
};

const CSV: &[u8] = b"sepal,petal\n5.1,1.4\n4.9,1.4\n";

const KEEP_PACKED: &str = "keep the file packed with decompress = false on its dataset in data.toml, or add it without --decompress";

pub(crate) fn gz(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

pub(crate) fn bz2(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

fn xz(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

fn zst(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(zstd::encode_all(bytes, 3)?)
}

/// 1 GiB of zeros, through `bzip2 -9`: 785 bytes.
fn bomb() -> Result<Vec<u8>> {
    let path = crate::support::workspace().join("tests/fixtures/archives/zeros-1gib.bz2");
    Ok(fs::read(path)?)
}

/// the bytes of every file the cache holds as an object, in all.
fn object_bytes(cache: &Path) -> Result<u64> {
    let mut total = 0;
    let mut folders = vec![cache.join("objects")];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                folders.push(entry.path());
            } else {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// `add --decompress` places what a gzip holds under the name without `.gz`, writes
/// `decompress = true`, and pins both: the packed file, and the tree it unpacks to. a second run
/// has nothing to do, and the cache keeps no packed copy once the file is in.
#[test]
fn add_with_decompress_places_the_unpacked_file_and_pins_both() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let server = Server::start("/files/iris.csv.gz", vec![file(&packed, "\"g1\"")])?;
    let added = sandbox.run(&["add", &server.url, "--decompress"])?;
    ok(&added)?;
    let folder = sandbox.work().join("data").join("iris");
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    assert!(!folder.join("iris.csv.gz").exists());
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(manifest.contains("\ndecompress = true\n"), "{manifest}");
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let pinned = format!(
        "{{ path = \"iris.csv.gz\", size = {}, sha256 = \"{}\", url = \"{}\", unpacks_to = \"sha256:{}\" }}",
        packed.len(),
        sha256_hex(&packed),
        server.url,
        sha256_hex(tree_of(&[("iris.csv", CSV)]).as_bytes())
    );
    assert!(lock.contains(&pinned), "{lock}");
    assert!(lock.contains("\ndecompress = true\n"), "{lock}");
    assert!(
        !sandbox
            .cache()
            .join("objects")
            .join(&sha256_hex(&packed)[..2])
            .join(&sha256_hex(&packed)[2..])
            .exists(),
        "the packed bytes leave the cache once the file is in"
    );
    let again = sandbox.run(&["sync"])?;
    ok(&again)?;
    assert_eq!(
        text(&again.stderr)?,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// each of gzip, bzip2, xz and zstd decompresses to the name without its ending.
#[test]
fn every_single_file_compression_decompresses() -> Result {
    let sandbox = Sandbox::new()?;
    let made = [
        ("a.csv.gz", gz(CSV)?),
        ("b.csv.bz2", bz2(CSV)?),
        ("c.csv.xz", xz(CSV)?),
        ("d.csv.zst", zst(CSV)?),
    ];
    let routes = Routes::start(
        made.iter()
            .map(|(name, bytes)| (format!("/files/{name}"), file(bytes, "\"c1\"")))
            .collect(),
    )?;
    sandbox.allow(&routes.url);
    let links: Vec<String> = made
        .iter()
        .map(|(name, _)| format!("{}/files/{name}", routes.url))
        .collect();
    let mut args = vec!["add", "--decompress"];
    args.extend(links.iter().map(String::as_str));
    ok(&sandbox.run(&args)?)?;
    for (name, _) in &made {
        let stem = &name[..1];
        let folder = sandbox.work().join("data").join(stem);
        assert_eq!(fs::read(folder.join(format!("{stem}.csv")))?, CSV, "{name}");
        assert!(!folder.join(name).exists(), "{name}");
    }
    Ok(())
}

/// a teammate's clone with a cold cache places the decompressed file data.lock pins.
#[test]
fn a_fresh_clone_decompresses_what_the_lock_pins() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = xz(CSV)?;
    let server = Server::start(
        "/files/iris.csv.xz",
        vec![file(&packed, "\"x1\""), file(&packed, "\"x1\"")],
    )?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url, "--decompress"])?)?;
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
    let folder = clone.join("data").join("iris");
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    assert!(!folder.join("iris.csv.xz").exists());
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

/// `decompress = true` written into data.toml by hand unpacks on the next sync, and taking it out
/// again places the packed file: a switch keeps the pinned file and only what it unpacks to
/// changes.
#[test]
fn decompress_in_data_toml_switches_without_a_new_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let server = Server::start(
        "/files/iris.csv.gz",
        (0..3).map(|_| file(&packed, "\"g1\"")).collect(),
    )?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    let folder = sandbox.work().join("data").join("iris");
    assert_eq!(
        fs::read(folder.join("iris.csv.gz"))?,
        packed,
        "off by default"
    );
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    let lock_path = sandbox.work().join("data.lock");
    let first_lock = fs::read_to_string(&lock_path)?;
    assert!(!first_lock.contains("unpacks_to"), "{first_lock}");
    // the same link added again asking for the other setting needs a person's choice
    let again = sandbox.run(&["--json", "add", &server.url, "--decompress"])?;
    assert_eq!(again.status.code(), Some(2), "{}", text(&again.stderr)?);
    let error = &json(&again)?["errors"][0];
    assert_eq!(
        error["message"],
        format!(
            "{} is already in data.toml as iris, with another decompress setting",
            server.url
        )
    );
    assert_eq!(
        error["help"],
        "change decompress on iris in data.toml, or pass --as <name> to add it a second time"
    );

    fs::write(&manifest, format!("{plain}decompress = true\n"))?;
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    assert!(!folder.join("iris.csv.gz").exists());
    let switched = fs::read_to_string(&lock_path)?;
    assert!(
        switched.contains(&format!(
            "unpacks_to = \"sha256:{}\"",
            sha256_hex(tree_of(&[("iris.csv", CSV)]).as_bytes())
        )),
        "{switched}"
    );
    let pin = format!("sha256 = \"{}\"", sha256_hex(&packed));
    assert!(first_lock.contains(&pin), "{first_lock}");
    assert!(
        switched.contains(&pin),
        "the packed file's pin stands: {switched}"
    );

    fs::write(&manifest, &plain)?;
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(fs::read(folder.join("iris.csv.gz"))?, packed);
    assert!(!folder.join("iris.csv").exists());
    let back = fs::read_to_string(&lock_path)?;
    assert!(!back.contains("unpacks_to"), "{back}");
    assert!(!back.contains("decompress"), "{back}");
    Ok(())
}

/// diff names a change of `decompress` as it names every field data.lock records, in words and
/// in --json, the packed file's pin standing.
#[test]
fn diff_names_a_change_of_decompress() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let packed = gz(CSV)?;
    let server = Server::start(
        "/files/iris.csv.gz",
        (0..3).map(|_| file(&packed, "\"g1\"")).collect(),
    )?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "packed"])?;
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{plain}decompress = true\n"))?;
    ok(&sandbox.run(&["sync"])?)?;
    let diff = text(&sandbox.run(&["diff"])?.stdout)?;
    assert!(
        diff.contains("iris changed: decompress false \u{2192} true"),
        "{diff}"
    );
    let document = json(&sandbox.run(&["--json", "diff"])?)?;
    assert_eq!(
        document["result"]["datasets"][0]["fields"],
        serde_json::json!([{ "name": "decompress", "before": "false", "after": "true" }]),
        "{document}"
    );
    Ok(())
}

/// a compressed tar is extract's: with `--decompress` it unpacks to its files, never to a bare
/// tar, and with `--no-extract` too it stays packed as it came.
#[test]
fn a_compressed_tar_stays_extracts_job() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(&tar_of(&[("iris.csv", CSV)])?)?;
    let routes = Routes::start(
        ["/files/flowers.tar.gz", "/kept/flowers.tar.gz"]
            .into_iter()
            .map(|path| (path.to_owned(), file(&packed, "\"t1\"")))
            .collect(),
    )?;
    sandbox.allow(&routes.url);
    let link = format!("{}/files/flowers.tar.gz", routes.url);
    ok(&sandbox.run(&["add", &link, "--decompress"])?)?;
    let folder = sandbox.work().join("data").join("flowers");
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    assert!(!folder.join("flowers.tar").exists());
    let kept_link = format!("{}/kept/flowers.tar.gz", routes.url);
    ok(&sandbox.run(&[
        "add",
        &kept_link,
        "--decompress",
        "--no-extract",
        "--as",
        "packed",
    ])?)?;
    let kept = sandbox.work().join("data").join("packed");
    assert_eq!(fs::read(kept.join("flowers.tar.gz"))?, packed);
    assert!(!kept.join("flowers.tar").exists());
    Ok(())
}

/// a folder on this machine added with `--decompress` places its single compressed files
/// decompressed, and leaves as they came, warning of each, a compressed tar whose name does not say
/// tar, which only extract unpacks, and a file whose bytes are not compressed as its name says.
#[test]
fn a_folder_decompresses_and_leaves_what_is_no_single_compressed_file() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::create_dir_all(flowers.join("sub"))?;
    fs::write(flowers.join("sub").join("iris.csv.gz"), gz(CSV)?)?;
    let inner = gz(&tar_of(&[("iris.csv", CSV)])?)?;
    fs::write(flowers.join("inner.gz"), &inner)?;
    fs::write(flowers.join("notes.gz"), b"plain notes\n")?;
    let output = sandbox.run(&["--json", "add", "../flowers", "--decompress"])?;
    ok(&output)?;
    let document = json(&output)?;
    let warned = |kind: &str, message: &str| {
        document["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|warning| warning["kind"] == kind && warning["message"] == message)
    };
    assert!(
        warned(
            "archive.packed",
            "flowers/inner.gz stays packed: it is a compressed tar, which only extract unpacks, and only by a name that says so, as .tar.gz does"
        ),
        "{document}"
    );
    assert!(
        warned(
            "archive.unreadable",
            "flowers/notes.gz is not compressed as its name says, so it stays as it is"
        ),
        "{document}"
    );
    let placed = sandbox.work().join("data").join("flowers");
    assert_eq!(fs::read(placed.join("sub").join("iris.csv"))?, CSV);
    assert!(!placed.join("sub").join("iris.csv.gz").exists());
    assert_eq!(fs::read(placed.join("inner.gz"))?, inner);
    assert_eq!(fs::read(placed.join("notes.gz"))?, b"plain notes\n");
    Ok(())
}

/// the name a file decompresses to is its own without the ending, and one that would be empty,
/// leave its folder, name repository metadata or not be stored by every system is refused, saying
/// which file and why: before any download where the source names its files, and once the file
/// came where its server may name it, before it decompresses.
#[test]
fn a_name_that_cannot_hold_the_unpacked_file_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let refused = |output: &std::process::Output, why: &str| -> Result {
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
        let document = json(output)?;
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "source.invalid", "{document}");
        assert_eq!(error["message"], why);
        assert_eq!(
            error["help"],
            "leave it out with --exclude, or add it without --decompress"
        );
        assert!(
            !sandbox.work().join("data.toml").exists(),
            "add leaves no trace"
        );
        Ok(())
    };
    for (name, why) in [
        (".gz", "\".gz\" would decompress to a file with no name"),
        (
            "...gz",
            "\"...gz\" would decompress to \"..\", which climbs out of its folder",
        ),
        (
            ".git.gz",
            "\".git.gz\" would decompress to \".git\", which names repository metadata",
        ),
        (
            "a..gz",
            "\"a..gz\" would decompress to a name not every system can store: \"a.\" ends with a dot or a space, which windows drops",
        ),
    ] {
        let server = Server::start(&format!("/files/{name}"), vec![file(&gz(CSV)?, "\"n1\"")])?;
        sandbox.allow(&server.url);
        fs::write(
            sandbox.work().join("links.txt"),
            format!("{}\n", server.url),
        )?;
        let output = sandbox.run(&[
            "--json",
            "add",
            "-i",
            "links.txt",
            "--decompress",
            "--as",
            "named",
        ])?;
        refused(&output, why)?;
        assert!(
            server.requests().is_empty(),
            "{name}: refused before downloading"
        );
    }
    // a plain link's server may name the file, so its name is known once it came
    let server = Server::start("/files/a..gz", vec![file(&gz(CSV)?, "\"n1\"")])?;
    sandbox.allow(&server.url);
    let output = sandbox.run(&[
        "--json",
        "add",
        &server.url,
        "--decompress",
        "--as",
        "named",
    ])?;
    refused(
        &output,
        "\"a..gz\" would decompress to a name not every system can store: \"a.\" ends with a dot or a space, which windows drops",
    )?;
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        fs::read_dir(sandbox.cache().join("unpacked"))
            .map(Iterator::count)
            .unwrap_or_default(),
        0,
        "nothing was decompressed"
    );
    Ok(())
}

/// a bzip2 of 785 bytes that holds 1 GiB is refused once it grows past the ratio the settings
/// allow, long before the disk fills, and add leaves no trace.
#[test]
fn a_bomb_is_refused_before_the_disk_fills() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = bomb()?;
    assert!(packed.len() < 1024);
    let server = Server::start("/files/zeros.csv.bz2", vec![file(&packed, "\"b1\"")])?;
    let output = sandbox.run(&["--json", "add", &server.url, "--decompress"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "source.invalid", "{document}");
    assert_eq!(
        error["message"],
        "\"zeros.csv\" grew past 1000 times its compressed size as it decompressed, which is how compression bombs fill disks"
    );
    assert_eq!(
        error["help"],
        format!(
            "if you trust the file, raise FETCHLOOM_ARCHIVE_RATIO, or archive_ratio in the settings file. otherwise {KEEP_PACKED}"
        )
    );
    assert!(
        object_bytes(&sandbox.cache())? < 1 << 20,
        "nothing of the bomb's output stays in the cache"
    );
    let scratch = fs::read_dir(sandbox.cache().join("tmp"))?.count();
    assert_eq!(scratch, 0, "no file is left half written");
    assert!(
        !sandbox.work().join("data.toml").exists(),
        "add leaves no trace"
    );
    Ok(())
}

/// the limits are the archive limits a person sets for this machine: data.toml cannot raise
/// them, and `FETCHLOOM_ARCHIVE_SIZE` lowers the most a file may decompress to.
#[test]
fn the_limits_come_from_this_machine_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let large = vec![b'x'; 4096];
    let packed = gz(&large)?;
    let server = Server::start("/files/big.csv.gz", vec![file(&packed, "\"l1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.big]\nref = \"{}\"\ndecompress = true\narchive_size = \"1T\"\n",
            server.url
        ),
    )?;
    let refused = sandbox.run(&["sync"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    assert!(
        text(&refused.stderr)?.contains(
            "data.toml has a key fetchloom does not know: \"archive_size\" in datasets.big"
        ),
        "{}",
        text(&refused.stderr)?
    );
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.big]\nref = \"{}\"\ndecompress = true\n",
            server.url
        ),
    )?;
    let output = sandbox
        .command(&["--json", "sync"])
        .env("FETCHLOOM_ARCHIVE_SIZE", "1K")
        .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(
        error["message"],
        "\"big.csv\" decompresses to more than 1.0 KiB, the most fetchloom unpacks from one file"
    );
    assert_eq!(
        error["help"],
        format!(
            "if you trust the file, raise FETCHLOOM_ARCHIVE_SIZE, or archive_size in the settings file. otherwise {KEEP_PACKED}"
        )
    );
    assert!(
        !sandbox
            .work()
            .join("data")
            .join("big")
            .join("big.csv")
            .exists()
    );
    Ok(())
}

/// streams joined end to end, as pigz and bgzip write them, decompress whole, and the limits
/// count them together: each part fits the most allowed, both together do not.
#[test]
fn joined_streams_decompress_whole_and_count_together() -> Result {
    let sandbox = Sandbox::new()?;
    let (first, second) = (vec![b'a'; 3000], vec![b'b'; 3000]);
    let mut joined = gz(&first)?;
    joined.extend(gz(&second)?);
    let server = Server::start(
        "/files/joined.txt.gz",
        vec![file(&joined, "\"j1\""), file(&joined, "\"j1\"")],
    )?;
    sandbox.allow(&server.url);
    let output = sandbox
        .command(&["--json", "add", &server.url, "--decompress"])
        .env("FETCHLOOM_ARCHIVE_SIZE", "4K")
        .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(
        document["errors"][0]["message"],
        "\"joined.txt\" decompresses to more than 4.0 KiB, the most fetchloom unpacks from one file"
    );
    ok(&sandbox.run(&["add", &server.url, "--decompress"])?)?;
    let mut whole = first;
    whole.extend(second);
    assert_eq!(
        fs::read(
            sandbox
                .work()
                .join("data")
                .join("joined")
                .join("joined.txt")
        )?,
        whole
    );
    Ok(())
}

/// what a file decompresses to counts against the room the cache's disk had free, and stops
/// before it would fill that disk.
#[cfg(debug_assertions)]
#[test]
fn decompressing_past_the_room_the_disk_had_free_stops_before_filling_it() -> Result {
    let sandbox = Sandbox::new()?;
    let large = vec![b'x'; 64 << 10];
    let packed = gz(&large)?;
    let server = Server::start("/files/wide.csv.gz", vec![file(&packed, "\"w1\"")])?;
    let cache = sandbox.folder("small-disk")?;
    let output = sandbox
        .command(&["--json", "add", &server.url, "--decompress"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_TEST_FREE", "16384")
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(50), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "local.io", "{document}");
    assert_eq!(
        error["message"],
        format!(
            "the file decompresses to more than the 16.0 KiB the disk holding the cache at {} had free",
            cache.display()
        )
    );
    assert_eq!(error["details"], serde_json::json!(["in wide/wide.csv.gz"]));
    Ok(())
}

/// a clone learns what its files decompress to only once it decompressed them, so the copies the
/// data folder takes of them are counted then, before any is placed: on exFAT, where every file
/// placed is a copy, the plain file's download and copy fit the disk, but not with a copy of the
/// decompressed file beside them.
#[cfg(debug_assertions)]
#[test]
fn a_clone_counts_the_copies_of_what_it_decompressed_before_placing_them() -> Result {
    let sandbox = Sandbox::new()?;
    let big = vec![7_u8; 20 << 10];
    let packed = gz(&vec![b'x'; 50 << 10])?;
    let routes = Routes::start(vec![
        ("/files/big.bin".to_owned(), file(&big, "\"b1\"")),
        ("/files/wide.csv.gz".to_owned(), file(&packed, "\"w1\"")),
    ])?;
    sandbox.allow(&routes.url);
    fs::write(
        sandbox.work().join("links.txt"),
        format!("{0}/files/big.bin\n{0}/files/wide.csv.gz\n", routes.url),
    )?;
    ok(&sandbox.run(&["add", "-i", "links.txt", "--decompress", "--as", "d"])?)?;
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock", "links.txt"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.folder("cold")?;
    let output = sandbox
        .command_in(&clone, &["--json", "sync"])
        .env("FETCHLOOM_CACHE", &cold)
        .env("FETCHLOOM_TEST_FILESYSTEM", "exfat")
        .env("FETCHLOOM_TEST_FREE", (64 << 10).to_string())
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(1), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{document}");
    assert_eq!(
        error["message"],
        format!(
            "this sync needs at least 90.0 KiB on the disk holding the data folder at {}, which has 64.0 KiB free",
            clone.join("data").display()
        )
    );
    assert!(!clone.join("data").join("d").join("wide.csv").exists());
    Ok(())
}

/// a run stopped while it decompresses leaves no file half written and no record of what the file
/// holds; the next run decompresses again from the bytes the first downloaded, as a stream that
/// stopped cannot go on where it stopped.
#[cfg(debug_assertions)]
#[test]
fn a_stopped_decompression_leaves_nothing_and_the_next_run_finishes() -> Result {
    let sandbox = Sandbox::new()?;
    let large: Vec<u8> = (0..(1 << 20)).map(|at: u32| (at % 251) as u8).collect();
    let packed = gz(&large)?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"s1\"")], b"");
    let server = Server::start(
        "/files/long.bin.gz",
        vec![file(&packed, "\"s1\""), unchanged],
    )?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.long]\nref = \"{}\"\ndecompress = true\n",
            server.url
        ),
    )?;
    let stopped = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TEST_STOP_UNPACKING", "1")
        .output()?;
    assert_eq!(
        stopped.status.code(),
        Some(130),
        "{}",
        text(&stopped.stderr)?
    );
    assert!(
        !sandbox
            .work()
            .join("data")
            .join("long")
            .join("long.bin")
            .exists()
    );
    assert_eq!(fs::read_dir(sandbox.cache().join("tmp"))?.count(), 0);
    let unpacked = sandbox.cache().join("unpacked");
    let records = fs::read_dir(&unpacked)
        .map(Iterator::count)
        .unwrap_or_default();
    assert_eq!(records, 0, "nothing records what the file holds");
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(
        fs::read(sandbox.work().join("data").join("long").join("long.bin"))?,
        large
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[1].contains("if-none-match: \"s1\""),
        "the next run only asks whether the file changed: {}",
        requests[1]
    );
    Ok(())
}

/// the names in the work folder, sorted.
fn work_names(sandbox: &Sandbox) -> Result<Vec<String>> {
    let mut names = fs::read_dir(sandbox.work())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

/// `get --decompress` puts what a single compressed file holds in the folder, under its name
/// without the ending; the hint to pin it keeps `--decompress`, and a second run only asks whether
/// the file changed.
#[test]
fn get_decompresses_into_its_folder_and_a_rerun_moves_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let routes = Routes::start(vec![(
        "/files/iris.csv.gz".to_owned(),
        file(&packed, "\"g1\""),
    )])?;
    routes.unchanged("/files/iris.csv.gz", "\"g1\"");
    let link = format!("{}/files/iris.csv.gz", routes.url);
    ok(&sandbox.run(&["get", &link, "--decompress"])?)?;
    let folder = sandbox.work().join("iris");
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    assert!(!folder.join("iris.csv.gz").exists());
    let again = sandbox.run(&["--json", "get", &link, "--decompress"])?;
    ok(&again)?;
    let document = json(&again)?;
    assert_eq!(document["work"]["bytes_down"], 0, "{document}");
    let got = &document["result"]["files"][0];
    assert_eq!(got["path"], "iris.csv");
    assert_eq!(got["sha256"], sha256_hex(CSV));
    assert_eq!(got["unpacked_from"], "iris.csv.gz");
    let asked = routes.requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1].contains("if-none-match: \"g1\""), "{}", asked[1]);
    assert_eq!(fs::read(folder.join("iris.csv"))?, CSV);
    #[cfg(target_os = "linux")]
    if let Some(said) = sandbox.on_a_terminal(&["get", &link, "--decompress"])? {
        assert!(
            said.contains(&format!(
                "next: to pin this dataset in a project, run `fetchloom add {link} --decompress` there"
            )),
            "{said}"
        );
    }
    Ok(())
}

/// several links get a folder each, each file decompressed by the decoder its bytes name.
#[test]
fn get_decompresses_every_single_file_compression() -> Result {
    let sandbox = Sandbox::new()?;
    let made = [
        ("a.csv.gz", gz(CSV)?),
        ("b.csv.bz2", bz2(CSV)?),
        ("c.csv.xz", xz(CSV)?),
        ("d.csv.zst", zst(CSV)?),
    ];
    let routes = Routes::start(
        made.iter()
            .map(|(name, bytes)| (format!("/files/{name}"), file(bytes, "\"c1\"")))
            .collect(),
    )?;
    let links: Vec<String> = made
        .iter()
        .map(|(name, _)| format!("{}/files/{name}", routes.url))
        .collect();
    let mut args = vec!["get", "--decompress", "--dir", "out"];
    args.extend(links.iter().map(String::as_str));
    ok(&sandbox.run(&args)?)?;
    for (name, _) in &made {
        let stem = &name[..1];
        let folder = sandbox.work().join("out").join(stem);
        assert_eq!(fs::read(folder.join(format!("{stem}.csv")))?, CSV, "{name}");
        assert!(!folder.join(name).exists(), "{name}");
    }
    Ok(())
}

/// with `-o`, the file the link decompresses to is written at exactly that path, and nothing of
/// the run stays in the cache.
#[test]
fn get_output_with_decompress_writes_the_decompressed_file_there() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let server = Server::start("/files/iris.csv.gz", vec![file(&packed, "\"o1\"")])?;
    let output = sandbox.run(&[
        "--json",
        "get",
        &server.url,
        "--decompress",
        "-o",
        "flowers.csv",
    ])?;
    ok(&output)?;
    let written = sandbox.work().join("flowers.csv");
    assert_eq!(fs::read(&written)?, CSV);
    let document = json(&output)?;
    assert_eq!(
        document["result"]["file"],
        written.display().to_string(),
        "{document}"
    );
    let got = &document["result"]["files"][0];
    assert_eq!(got["path"], "flowers.csv");
    assert_eq!(got["size"], CSV.len());
    assert_eq!(got["sha256"], sha256_hex(CSV));
    assert_eq!(got["unpacked_from"], "iris.csv.gz");
    assert_eq!(
        object_bytes(&sandbox.cache())?,
        0,
        "nothing of this run stays in the cache"
    );
    assert_eq!(work_names(&sandbox)?, ["flowers.csv"]);
    Ok(())
}

/// `--sha256` checks the bytes as they were downloaded, before they decompress, as pooch's
/// `known_hash` does: the packed file's hash passes, the decompressed file's is refused, and the
/// help says so.
#[test]
fn sha256_checks_the_downloaded_bytes_before_they_decompress() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let server = Server::start(
        "/files/iris.csv.gz",
        (0..4).map(|_| file(&packed, "\"s1\"")).collect(),
    )?;
    let decompressed = sha256_hex(CSV);
    let wrong = sandbox.run(&[
        "--json",
        "get",
        &server.url,
        "--decompress",
        "-o",
        "iris.csv",
        "--sha256",
        &decompressed,
    ])?;
    assert_eq!(wrong.status.code(), Some(30), "{}", text(&wrong.stderr)?);
    let error = &json(&wrong)?["errors"][0];
    assert_eq!(error["message"], "iris.csv.gz does not match --sha256");
    assert_eq!(
        error["details"],
        serde_json::json!([format!(
            "expected sha256 {decompressed}, got {}",
            sha256_hex(&packed)
        )])
    );
    assert!(!sandbox.work().join("iris.csv").exists());
    let right = sandbox.run(&[
        "get",
        &server.url,
        "--decompress",
        "-o",
        "iris.csv",
        "--sha256",
        &sha256_hex(&packed),
    ])?;
    ok(&right)?;
    assert_eq!(fs::read(sandbox.work().join("iris.csv"))?, CSV);
    let into = |sha256: &str| {
        let args = ["--json", "get", &server.url, "--decompress", "--dir", "got"];
        sandbox.run(&[&args[..], &["--sha256", sha256]].concat())
    };
    let wrong = into(&decompressed)?;
    assert_eq!(wrong.status.code(), Some(30), "{}", text(&wrong.stderr)?);
    assert_eq!(json(&wrong)?["errors"][0]["kind"], "integrity.mismatch");
    assert!(!sandbox.work().join("got").exists());
    ok(&into(&sha256_hex(&packed))?)?;
    assert_eq!(fs::read(sandbox.work().join("got").join("iris.csv"))?, CSV);
    let help = sandbox.run(&["get", "--help"])?;
    let help = text(&help.stdout)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        help.contains(
            "check that the downloaded file, still compressed with --decompress, has this SHA-256"
        ),
        "{help}"
    );
    Ok(())
}

/// an archive's ending stays extract's: get unpacks a `.tar.gz` into its files with
/// `--decompress` as without it, keeps it packed with `--no-extract`, and `-o` writes it as it
/// came.
#[test]
fn get_leaves_an_archive_to_extract() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(&tar_of(&[("iris.csv", CSV)])?)?;
    let server = Server::start(
        "/files/flowers.tar.gz",
        (0..3).map(|_| file(&packed, "\"t1\"")).collect(),
    )?;
    ok(&sandbox.run(&["get", &server.url, "--decompress", "--dir", "unpacked"])?)?;
    let unpacked = sandbox.work().join("unpacked");
    assert_eq!(fs::read(unpacked.join("iris.csv"))?, CSV);
    assert!(!unpacked.join("flowers.tar").exists());
    ok(&sandbox.run(&[
        "get",
        &server.url,
        "--decompress",
        "--no-extract",
        "--dir",
        "packed",
    ])?)?;
    assert_eq!(
        fs::read(sandbox.work().join("packed").join("flowers.tar.gz"))?,
        packed
    );
    assert!(!sandbox.work().join("packed").join("flowers.tar").exists());
    ok(&sandbox.run(&["get", &server.url, "--decompress", "-o", "kept.tar.gz"])?)?;
    assert_eq!(fs::read(sandbox.work().join("kept.tar.gz"))?, packed);
    Ok(())
}

/// decompressing is get's to ask for: without `--decompress` a single compressed file arrives as it
/// came, into a folder or at `-o`.
#[test]
fn get_leaves_a_compressed_file_packed_unless_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let routes = Routes::start(vec![(
        "/files/iris.csv.gz".to_owned(),
        file(&packed, "\"p1\""),
    )])?;
    let link = format!("{}/files/iris.csv.gz", routes.url);
    ok(&sandbox.run(&["get", &link, "--dir", "packed"])?)?;
    let folder = sandbox.work().join("packed");
    assert_eq!(fs::read(folder.join("iris.csv.gz"))?, packed);
    assert!(!folder.join("iris.csv").exists());
    ok(&sandbox.run(&["get", &link, "-o", "kept.csv.gz"])?)?;
    assert_eq!(fs::read(sandbox.work().join("kept.csv.gz"))?, packed);
    Ok(())
}

/// a dry run says what each file would decompress to, into a folder or at `-o`, and writes
/// nothing.
#[test]
fn a_dry_run_of_get_says_what_it_would_decompress_to() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let routes = Routes::start(vec![(
        "/files/iris.csv.gz".to_owned(),
        file(&packed, "\"d1\""),
    )])?;
    let link = format!("{}/files/iris.csv.gz", routes.url);
    let folder = sandbox.run(&["get", &link, "--decompress", "--dry-run"])?;
    ok(&folder)?;
    let said = text(&folder.stderr)?;
    assert!(
        said.contains("Would decompress iris.csv.gz to iris.csv"),
        "{said}"
    );
    assert!(said.contains("Would put 1 file in iris"), "{said}");
    let dry = [
        "get",
        &link,
        "--decompress",
        "--dry-run",
        "-o",
        "flowers.csv",
    ];
    let output = sandbox.run(&dry)?;
    ok(&output)?;
    let said = text(&output.stderr)?;
    assert!(
        said.contains("Would decompress iris.csv.gz to flowers.csv"),
        "{said}"
    );
    let output = sandbox.run(&[&["--json"][..], &dry].concat())?;
    ok(&output)?;
    assert_eq!(
        json(&output)?["result"]["changes"]["place"],
        serde_json::json!(["flowers.csv"])
    );
    assert_eq!(work_names(&sandbox)?, Vec::<String>::new());
    Ok(())
}

/// a file at `-o` that holds what the link decompresses to stays as it is: with `--sha256` naming
/// the downloaded bytes, with no request; without it, after a 304 that moves nothing. one holding
/// other bytes is refused unless `--force`.
#[test]
fn a_file_at_output_holding_the_decompressed_bytes_is_kept() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let routes = Routes::start(vec![(
        "/files/iris.csv.gz".to_owned(),
        file(&packed, "\"h1\""),
    )])?;
    routes.unchanged("/files/iris.csv.gz", "\"h1\"");
    let link = format!("{}/files/iris.csv.gz", routes.url);
    let get = |extra: &[&str]| {
        let mut args = vec!["get", link.as_str(), "--decompress", "-o", "iris.csv"];
        args.extend_from_slice(extra);
        sandbox.run(&args)
    };
    ok(&get(&[])?)?;
    let again = get(&["--json"])?;
    ok(&again)?;
    assert_eq!(json(&again)?["work"]["bytes_down"], 0);
    let asked = routes.requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1].contains("if-none-match: \"h1\""), "{}", asked[1]);
    let told = get(&[])?;
    ok(&told)?;
    let said = text(&told.stderr)?;
    assert!(
        said.contains("Ok iris.csv already holds what the link serves now, decompressed"),
        "{said}"
    );
    assert_eq!(routes.requests().len(), 3);
    let named = get(&["--sha256", &sha256_hex(&packed)])?;
    ok(&named)?;
    let said = text(&named.stderr)?;
    assert!(
        said.contains("Ok iris.csv already holds what --sha256 names, decompressed"),
        "{said}"
    );
    assert_eq!(routes.requests().len(), 3, "no request");
    let offline = get(&["--offline"])?;
    ok(&offline)?;
    let said = text(&offline.stderr)?;
    assert!(
        said.contains("Ok iris.csv already holds what the link served last time, decompressed"),
        "{said}"
    );
    assert_eq!(routes.requests().len(), 3, "no request offline");
    let placed = sandbox.work().join("iris.csv");
    fs::write(&placed, b"other\n")?;
    let refused = get(&["--json"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "iris.csv already exists and holds other bytes than the link serves now, decompressed"
    );
    assert_eq!(fs::read(&placed)?, b"other\n");
    ok(&get(&["--force"])?)?;
    assert_eq!(fs::read(&placed)?, CSV);
    Ok(())
}

/// a get of a link only its source can list, as a zenodo record, remembers the bytes the link
/// served, still compressed, so an offline get finds what they decompressed to at `-o` and keeps
/// it without asking anyone.
#[cfg(debug_assertions)]
#[test]
fn an_offline_get_of_a_listed_link_keeps_the_file_it_decompressed() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let record = serde_json::json!({
        "id": 101,
        "conceptrecid": "100",
        "doi": "10.5281/zenodo.101",
        "metadata": {
            "title": "Iris",
            "license": { "id": "cc-by-4.0" },
            "access_right": "open",
        },
        "files": [{
            "key": "iris.csv.gz",
            "size": packed.len(),
            "checksum": format!("md5:{}", crate::support::md5_hex(&packed)),
        }],
    });
    let routes = Routes::start(vec![
        (
            "/zenodo.org/api/records/101".to_owned(),
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                &serde_json::to_vec(&record)?,
            ),
        ),
        (
            "/zenodo.org/api/records/101/files/iris.csv.gz/content".to_owned(),
            file(&packed, "\"z1\""),
        ),
    ])?;
    let get = |first: &[&str]| {
        let args = ["get", "zenodo:101", "--decompress", "-o", "iris.csv"];
        sandbox
            .command(&[first, &args[..]].concat())
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()
    };
    ok(&get(&[])?)?;
    assert_eq!(fs::read(sandbox.work().join("iris.csv"))?, CSV);
    let asked = routes.requests().len();
    let offline = get(&["--offline"])?;
    ok(&offline)?;
    let said = text(&offline.stderr)?;
    assert!(
        said.contains("Ok iris.csv already holds what the link served last time, decompressed"),
        "{said}"
    );
    assert_eq!(routes.requests().len(), asked, "no request offline");
    Ok(())
}

/// get decompresses through the same guard, with the limits of this machine: a bomb stops at
/// the ratio into a folder or at `-o` and leaves no file, with help in get's words, and
/// `FETCHLOOM_ARCHIVE_SIZE` lowers the most a file may decompress to.
#[test]
fn get_refuses_a_bomb_and_files_past_the_local_limits() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        ("/files/zeros.csv.bz2".to_owned(), file(&bomb()?, "\"b1\"")),
        (
            "/files/big.csv.gz".to_owned(),
            file(&gz(&vec![b'x'; 4096])?, "\"b2\""),
        ),
    ])?;
    let zeros = format!("{}/files/zeros.csv.bz2", routes.url);
    for args in [&["--decompress"][..], &["--decompress", "-o", "zeros.csv"]] {
        let output = sandbox.run(&[&["--json", "get", zeros.as_str()][..], args].concat())?;
        assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(
            error["message"],
            "\"zeros.csv\" grew past 1000 times its compressed size as it decompressed, which is how compression bombs fill disks",
            "{args:?}"
        );
        assert_eq!(
            error["help"],
            "if you trust the file, raise FETCHLOOM_ARCHIVE_RATIO, or archive_ratio in the settings file. otherwise leave out --decompress to keep the file packed",
            "{args:?}"
        );
        assert!(object_bytes(&sandbox.cache())? < 1 << 20, "{args:?}");
    }
    assert_eq!(work_names(&sandbox)?, Vec::<String>::new());
    let big = sandbox
        .command(&[
            "--json",
            "get",
            &format!("{}/files/big.csv.gz", routes.url),
            "--decompress",
            "-o",
            "big.csv",
        ])
        .env("FETCHLOOM_ARCHIVE_SIZE", "1K")
        .output()?;
    assert_eq!(big.status.code(), Some(10), "{}", text(&big.stderr)?);
    assert_eq!(
        json(&big)?["errors"][0]["message"],
        "\"big.csv\" decompresses to more than 1.0 KiB, the most fetchloom unpacks from one file"
    );
    assert!(!sandbox.work().join("big.csv").exists());
    Ok(())
}

/// a damaged file get would decompress is refused with get's own way out, leaving out
/// `--decompress`, in place of a project's.
#[test]
fn a_damaged_file_get_would_decompress_names_get_s_way_out() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let cut = &packed[..packed.len() - 8];
    let routes = Routes::start(vec![("/files/cut.csv.gz".to_owned(), file(cut, "\"c1\""))])?;
    let link = format!("{}/files/cut.csv.gz", routes.url);
    let output = sandbox.run(&["--json", "get", &link, "--decompress", "-o", "cut.csv"])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(
        error["message"],
        "the file is damaged: it ends before its stream does"
    );
    assert_eq!(
        error["help"],
        "ask the file's owner for a fixed copy, or leave out --decompress to keep the file packed"
    );
    assert_eq!(work_names(&sandbox)?, Vec::<String>::new());
    Ok(())
}

/// a get stopped while it decompresses writes nothing, and the next get finishes from the bytes
/// the first downloaded, asking only whether they changed.
#[cfg(debug_assertions)]
#[test]
fn a_stopped_get_decompression_writes_nothing_and_the_next_get_finishes() -> Result {
    let sandbox = Sandbox::new()?;
    let large: Vec<u8> = (0..(1 << 20)).map(|at: u32| (at % 251) as u8).collect();
    let routes = Routes::start(vec![(
        "/files/long.bin.gz".to_owned(),
        file(&gz(&large)?, "\"l1\""),
    )])?;
    routes.unchanged("/files/long.bin.gz", "\"l1\"");
    let link = format!("{}/files/long.bin.gz", routes.url);
    let stopped = sandbox
        .command(&["get", &link, "--decompress", "-o", "long.bin"])
        .env("FETCHLOOM_TEST_STOP_UNPACKING", "1")
        .output()?;
    assert_eq!(
        stopped.status.code(),
        Some(130),
        "{}",
        text(&stopped.stderr)?
    );
    assert!(!sandbox.work().join("long.bin").exists());
    assert_eq!(fs::read_dir(sandbox.cache().join("tmp"))?.count(), 0);
    ok(&sandbox.run(&["get", &link, "--decompress", "-o", "long.bin"])?)?;
    assert_eq!(fs::read(sandbox.work().join("long.bin"))?, large);
    let asked = routes.requests();
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1].contains("if-none-match: \"l1\""), "{}", asked[1]);
    Ok(())
}

/// a name that would decompress to no file's name is refused: before any download where a
/// listing names the file, and once the file came where only its server names it.
#[test]
fn get_refuses_a_name_no_file_may_take() -> Result {
    let sandbox = Sandbox::new()?;
    let html = "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>Directory listing for /data/</title>\n</head>\n<body>\n<h1>Directory listing for /data/</h1>\n<hr>\n<ul>\n<li><a href=\"a..gz\">a..gz</a></li>\n<li><a href=\"iris.csv\">iris.csv</a></li>\n</ul>\n<hr>\n</body>\n</html>\n";
    let length = html.len().to_string();
    let listing = reply(
        "200 OK",
        &[
            ("Content-Type", "text/html; charset=utf-8"),
            ("Content-Length", &length),
        ],
        html.as_bytes(),
    );
    let routes = Routes::start(vec![
        ("/data/".to_owned(), listing),
        ("/data/a..gz".to_owned(), file(&gz(CSV)?, "\"n1\"")),
        ("/data/iris.csv".to_owned(), file(CSV, "\"n2\"")),
    ])?;
    let why = "\"a..gz\" would decompress to a name not every system can store: \"a.\" ends with a dot or a space, which windows drops";
    let folder = format!("{}/data/", routes.url);
    let listed = sandbox.run(&["--json", "get", &folder, "--decompress"])?;
    assert_eq!(listed.status.code(), Some(10), "{}", text(&listed.stderr)?);
    let error = &json(&listed)?["errors"][0];
    assert_eq!(error["message"], why);
    assert_eq!(
        error["help"],
        "leave it out with --exclude, or leave out --decompress to keep it packed"
    );
    let asked = routes.requests();
    assert_eq!(asked.len(), 1, "the listing alone: {asked:?}");
    let link = format!("{folder}a..gz");
    let plain = sandbox.run(&["--json", "get", &link, "--decompress"])?;
    assert_eq!(plain.status.code(), Some(10), "{}", text(&plain.stderr)?);
    let error = &json(&plain)?["errors"][0];
    assert_eq!(error["message"], why);
    assert_eq!(error["help"], "leave out --decompress to keep it packed");
    assert_eq!(routes.requests().len(), 2);
    assert_eq!(work_names(&sandbox)?, Vec::<String>::new());
    Ok(())
}

/// a change of `decompress` in data.toml is named as one, apart from `extract`, by lock --check and
/// by export, with the same kind and exit code a change of `extract` gets, and a change of both
/// names both.
#[test]
fn lock_check_names_a_change_of_decompress() -> Result {
    let sandbox = Sandbox::new()?;
    let packed = gz(CSV)?;
    let server = Server::start("/files/iris.csv.gz", vec![file(&packed, "\"g1\"")])?;
    sandbox.allow(&server.url);
    ok(&sandbox.run(&["add", &server.url])?)?;
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    let lock = "`fetchloom lock` needs no network for it while its";
    for (lines, keys, files) in [
        ("decompress = true\n", "decompress", "compressed files"),
        (
            "extract = false\ndecompress = true\n",
            "extract and decompress",
            "archives and compressed files",
        ),
    ] {
        fs::write(&manifest, format!("{plain}{lines}"))?;
        let checked = sandbox.run(&["--json", "lock", "--check"])?;
        assert_eq!(checked.status.code(), Some(1), "{}", text(&checked.stderr)?);
        let error = &json(&checked)?["errors"][0];
        assert_eq!(error["kind"], "lock.outdated", "{error}");
        assert_eq!(
            error["details"][0],
            format!("iris changed {keys} in data.toml; {lock} {files} are cached"),
            "{error}"
        );
        let exported = sandbox.run(&["export"])?;
        let stderr = text(&exported.stderr)?;
        assert_eq!(exported.status.code(), Some(1), "{stderr}");
        assert!(
            stderr.contains(&format!("  iris changed {keys} in data.toml\n")),
            "{stderr}"
        );
    }
    Ok(())
}
