//! single compressed files: unpacked only when asked, by `--decompress` or `decompress = true`, to
//! their name without the ending, pinned beside what they unpack to, and refused within the
//! archive limits the settings give, before a bomb fills a disk.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::support::{
    Result, Routes, Sandbox, Server, file, json, ok, reply, sha256_hex, tar_of, text,
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

/// the tree data.lock pins for a file that decompresses to `name` holding `bytes`: the SHA-256
/// of its line, path, size and SHA-256 apart by tabs, as for any archive's files.
fn tree(name: &str, bytes: &[u8]) -> String {
    let line = format!("{name}\t{}\t{}\n", bytes.len(), sha256_hex(bytes));
    sha256_hex(line.as_bytes())
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
        tree("iris.csv", CSV)
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
            tree("iris.csv", CSV)
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
    let flowers = sandbox.work().join("flowers");
    fs::create_dir_all(flowers.join("sub"))?;
    fs::write(flowers.join("sub").join("iris.csv.gz"), gz(CSV)?)?;
    let inner = gz(&tar_of(&[("iris.csv", CSV)])?)?;
    fs::write(flowers.join("inner.gz"), &inner)?;
    fs::write(flowers.join("notes.gz"), b"plain notes\n")?;
    let output = sandbox.run(&["--json", "add", "./flowers", "--decompress"])?;
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
