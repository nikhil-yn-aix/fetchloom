//! `fetchloom get`: one link into one folder, or into the one file `-o` names, through the cache.

use std::fs;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crate::support::{
    Part, Result, Sandbox, Server, file, json, link_folder, partial_files, reply, text, zip_of,
};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";
const PATH: &str = "/records/3608990/files/iris.csv";

#[test]
fn a_link_lands_in_a_folder_named_after_its_file() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let folder = sandbox.work().join("iris");
    assert_eq!(text(&output.stdout)?, format!("{}\n", folder.display()));
    assert_eq!(fs::read(folder.join("iris.csv"))?, IRIS);
    sandbox
        .placed_filters(&stderr)?
        .bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn a_file_with_the_same_size_and_time_but_other_bytes_is_not_taken_for_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    assert_eq!(sandbox.run(&["get", &server.url])?.status.code(), Some(0));
    let placed = sandbox.work().join("iris").join("iris.csv");
    let time = fs::metadata(&placed)?.modified()?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "only a writable file can be removed on windows"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&placed, permissions)?;
    fs::remove_file(&placed)?;
    fs::write(&placed, vec![b'x'; IRIS.len()])?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(time)?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["errors"][0]["kind"], "local.not_empty");
    assert_eq!(
        fs::read(&placed)?,
        vec![b'x'; IRIS.len()],
        "nothing of yours is replaced"
    );
    Ok(())
}

#[test]
fn json_describes_the_run_and_nothing_else() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
    assert_eq!(
        text(&output.stdout)?.lines().count(),
        1,
        "one document on one line"
    );
    let mut document = json(&output)?;
    // what differs between machines is checked here, so the snapshot holds what never does
    let folder = std::mem::replace(&mut document["result"]["folder"], "[folder]".into());
    assert_eq!(folder, sandbox.work().join("iris").display().to_string());
    let links = sandbox.links()?;
    let (expected, counter) = if links.reflinks_read_only() {
        ("reflink", "reflinks")
    } else if links.hardlink {
        ("hardlink", "hardlinks")
    } else {
        ("copy", "copies")
    };
    let method = std::mem::replace(
        &mut document["result"]["files"][0]["method"],
        "[method]".into(),
    );
    assert_eq!(method, expected);
    let item = &mut document["result"]["items"][0];
    assert_eq!(
        std::mem::replace(&mut item["folder"], "[folder]".into()),
        folder
    );
    assert_eq!(
        std::mem::replace(&mut item["files"][0]["method"], "[method]".into()),
        expected
    );
    let work = &mut document["work"];
    for key in ["reflinks", "hardlinks", "copies"] {
        assert_eq!(work[key], u64::from(key == counter), "{key}");
        work[key] = "[varies]".into();
    }
    work["elapsed_ms"] = "[varies]".into();
    let pretty = serde_json::to_string_pretty(&document)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(pretty));
    Ok(())
}

#[test]
fn a_second_run_moves_zero_bytes() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    let first = json(&sandbox.run(&["--json", "get", &server.url])?)?;
    assert_eq!(first["work"]["requests"], 1);
    assert_eq!(first["work"]["bytes_down"], IRIS.len());
    assert_eq!(first["work"]["objects_written"], 1);

    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"v1\""), "{asked}");
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn files_placed_from_bytes_the_cache_held_are_counted() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    let first = json(&sandbox.run(&["--json", "get", &server.url])?)?;
    assert_eq!(first["work"]["placed_from_cache"], 0, "{first}");
    let again = json(&sandbox.run(&["--json", "get", &server.url, "--dir", "again"])?)?;
    assert_eq!(again["work"]["bytes_down"], 0, "{again}");
    assert_eq!(again["work"]["placed_from_cache"], 1, "{again}");
    Ok(())
}

#[test]
fn a_dropped_connection_resumes_where_it_stopped() -> Result {
    let body: Vec<u8> = (0..100_000u32).flat_map(u32::to_le_bytes).collect();
    let cut = 150_000;
    let length = body.len().to_string();
    let rest = (body.len() - cut).to_string();
    let range = format!("bytes {cut}-{}/{}", body.len() - 1, body.len());
    for (options, placed) in [(&[][..], "big/big.bin"), (&["-o", "big.bin"], "big.bin")] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(
            "/big.bin",
            vec![
                reply(
                    "200 OK",
                    &[("Content-Length", &length), ("ETag", "\"v1\"")],
                    &body[..cut],
                ),
                reply(
                    "206 Partial Content",
                    &[
                        ("Content-Length", &rest),
                        ("Content-Range", &range),
                        ("ETag", "\"v1\""),
                    ],
                    &body[cut..],
                ),
            ],
        )?;
        let output = sandbox.run(&[&["--json", "get", &server.url][..], options].concat())?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let work = &json(&output)?["work"];
        assert_eq!(work["requests"], 2, "{placed}");
        assert_eq!(work["retries"], 1, "{placed}");
        assert_eq!(work["resumes"], 1, "{placed}");
        assert_eq!(
            work["bytes_down"],
            body.len(),
            "{placed}: no byte is downloaded twice"
        );
        assert_eq!(fs::read(sandbox.work().join(placed))?, body, "{placed}");
        let resumed = &server.requests()[1];
        assert!(
            resumed.contains(&format!("range: bytes={cut}-")),
            "{resumed}"
        );
        assert!(resumed.contains("if-range: \"v1\""), "{resumed}");
    }
    Ok(())
}

/// how many files the cache's object store holds.
fn objects(sandbox: &Sandbox) -> usize {
    fs::read_dir(sandbox.cache().join("objects"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|fan| fs::read_dir(fan.path()).into_iter().flatten().flatten())
        .count()
}

/// the names in the work folder, sorted.
fn names(sandbox: &Sandbox) -> Result<Vec<String>> {
    let mut names = fs::read_dir(sandbox.work())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<String>>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn output_writes_exactly_that_file_writable_and_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let placed = sandbox.work().join("data.csv");
    assert_eq!(text(&output.stdout)?, format!("{}\n", placed.display()));
    assert_eq!(fs::read(&placed)?, IRIS);
    let metadata = fs::metadata(&placed)?;
    assert!(
        !metadata.permissions().readonly(),
        "the file is yours to edit"
    );
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::nlink(&metadata),
        1,
        "no other name shares its bytes"
    );
    assert_eq!(names(&sandbox)?, ["data.csv"]);
    assert_eq!(objects(&sandbox), 0, "a one-off keeps no copy in the cache");
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

/// moving writes the bytes once, however large, also into folders `-o` names that are not there
/// yet. windows copies, since fetchloom cannot count a file's names there.
#[test]
fn output_moves_a_one_off_out_of_the_cache() -> Result {
    for path in ["data.csv", "new/sub/data.csv"] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
        let output = sandbox.run(&["--json", "get", &server.url, "-o", path])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let document = json(&output)?;
        let method = &document["result"]["files"][0]["method"];
        if cfg!(unix) {
            assert_eq!(method, "move", "{path}");
            for key in ["reflinks", "hardlinks", "copies"] {
                assert_eq!(document["work"][key], 0, "{path}: {key}");
            }
        } else {
            assert_ne!(method, "move");
        }
        assert_eq!(fs::read(sandbox.work().join(path))?, IRIS);
        assert_eq!(objects(&sandbox), 0);
    }
    Ok(())
}

/// the line must not say the cache holds those bytes, which it no longer does.
#[test]
fn a_304_after_a_one_off_left_the_cache_says_what_the_server_serves() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    let first = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    assert_eq!(objects(&sandbox), 0, "the one-off moved out");
    let output = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(server.requests()[1].contains("if-none-match: \"v1\""));
    assert!(
        stderr.contains("Checked iris.csv · 127.0.0.1 still serves the bytes downloaded last time"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn output_copies_bytes_a_project_needs_and_keeps_them_cached() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    crate::project::added(&sandbox, &server)?;
    // the project's pin stays while the cache loses the bytes, so get downloads them again
    for shard in fs::read_dir(sandbox.cache().join("objects"))? {
        let shard = shard?.path();
        if shard.is_dir() {
            for object in fs::read_dir(&shard)? {
                crate::support::delete(&object?.path())?;
            }
        }
    }
    let output = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_ne!(json(&output)?["result"]["files"][0]["method"], "move");
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    assert_eq!(objects(&sandbox), 1, "the project's bytes stay cached");
    Ok(())
}

/// a registered project whose data.toml is gone needs nothing, as gc forgets it.
#[test]
fn output_moves_a_one_off_though_a_registered_project_is_gone() -> Result {
    let sandbox = Sandbox::new()?;
    let pinned = Server::start(PATH, vec![file(b"a,b\n", "\"p1\"")])?;
    let gone = sandbox.folder("gone")?;
    let added = sandbox.command_in(&gone, &["add", &pinned.url]).output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    fs::remove_file(gone.join("data.toml"))?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    if cfg!(unix) {
        assert_eq!(document["result"]["files"][0]["method"], "move");
        assert_eq!(document["work"]["copies"], 0);
    }
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    Ok(())
}

/// the error also offers another path, and comes before any request or write.
#[test]
fn output_beneath_a_file_names_the_file_in_the_way() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let data = sandbox.work().join("data");
    fs::create_dir(&data)?;
    fs::write(data.join("py.csv"), b"mine")?;
    let output = sandbox.run(&["--json", "get", &server.url, "-o", "data/py.csv/x.csv"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "local.io");
    assert_eq!(
        error["message"],
        "cannot write data/py.csv/x.csv: data/py.csv is a file, not a folder"
    );
    let separator = std::path::MAIN_SEPARATOR;
    assert_eq!(
        error["help"],
        format!(
            "choose another name with -o, such as `-o data{separator}x.csv`, or rename data/py.csv so a folder can take its name"
        )
    );
    assert!(server.requests().is_empty(), "nothing is downloaded");
    assert_eq!(fs::read(data.join("py.csv"))?, b"mine");
    assert_eq!(names(&sandbox)?, ["data"]);
    assert_eq!(objects(&sandbox), 0);
    Ok(())
}

#[test]
fn output_keeps_an_archive_packed() -> Result {
    let sandbox = Sandbox::new()?;
    let bytes = zip_of(&[("iris.csv", IRIS)])?;
    let server = Server::start("/iris.zip", vec![file(&bytes, "\"z1\"")])?;
    let output = sandbox.run(&["get", &server.url, "-o", "packed/iris.zip"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("packed").join("iris.zip"))?,
        bytes
    );
    assert_eq!(
        names(&sandbox)?,
        ["packed"],
        "-o makes the folders it names"
    );
    Ok(())
}

/// a file holding the named bytes is kept, and one holding other bytes is refused.
#[test]
fn output_with_a_known_sha256_decides_about_a_file_there_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    for (bytes, code) in [(IRIS, 0), (&b"mine"[..], 50)] {
        fs::write(sandbox.work().join("data.csv"), bytes)?;
        let output = sandbox.run(&[
            "--json",
            "get",
            &server.url,
            "-o",
            "data.csv",
            "--sha256",
            IRIS_SHA256,
        ])?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            text(&output.stdout)?
        );
        assert_eq!(fs::read(sandbox.work().join("data.csv"))?, bytes);
    }
    assert!(server.requests().is_empty(), "{:?}", server.requests());
    Ok(())
}

/// the help leads with checking the hash rather than with `--force`.
#[test]
fn output_with_another_sha256_than_its_file_says_either_may_be_wrong() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    fs::write(sandbox.work().join("data.csv"), IRIS)?;
    let wrong = "0".repeat(64);
    let output = sandbox.run(&[
        "--json",
        "get",
        &server.url,
        "-o",
        "data.csv",
        "--sha256",
        &wrong,
    ])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "data.csv already exists, and its bytes are not the ones --sha256 names: the hash or the file may be wrong"
    );
    assert_eq!(
        error["details"][0],
        format!("--sha256 names {wrong}, data.csv holds {IRIS_SHA256}")
    );
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.starts_with("check the hash")),
        "{error}"
    );
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    assert_eq!(server.requests(), Vec::<String>::new());
    Ok(())
}

#[test]
fn output_leaves_a_file_holding_the_same_bytes_as_it_is() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let first = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let placed = sandbox.work().join("data.csv");
    let before = fs::metadata(&placed)?;
    let output = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok data.csv already holds what the link serves"),
        "{stderr}"
    );
    let after = fs::metadata(&placed)?;
    assert_eq!(after.modified()?, before.modified()?);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&after),
        std::os::unix::fs::MetadataExt::ino(&before)
    );
    assert_eq!(
        objects(&sandbox),
        0,
        "the bytes it compared leave the cache"
    );
    Ok(())
}

/// the bytes left the cache for the file, yet the rerun still asks the server, and its 304 means
/// no download and no question about the size.
#[test]
fn output_asks_whether_the_bytes_its_file_holds_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    // every download of more than a byte asks first
    let get = |extra: &[&str]| {
        sandbox
            .command(&[&["--json", "get", &server.url, "-o", "data.csv"], extra].concat())
            .env("FETCHLOOM_ASK_ABOVE", "1")
            .output()
    };
    let first = get(&["--yes"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stdout)?);
    assert_eq!(objects(&sandbox), 0, "the bytes left the cache");
    let output = get(&[])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"v1\""), "{asked}");
    let document = json(&output)?;
    assert_eq!(document["work"]["bytes_down"], 0, "{document}");
    assert_eq!(document["result"]["files"][0]["origin"], "not_modified");
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    Ok(())
}

/// `-o` wrote the file, so its bytes left the cache, yet the folder's rerun still asks.
#[test]
fn a_folder_asks_whether_the_bytes_its_file_holds_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    let first = sandbox.run(&["get", &server.url, "-o", "data/iris.csv"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let output = sandbox.run(&["get", &server.url, "--dir", "data"])?;
    let stderr = text(&output.stderr)?.replace('\\', "/");
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let asked = &server.requests()[1];
    assert!(asked.contains("if-none-match: \"v1\""), "{asked}");
    assert!(
        stderr.contains("Ok data/iris.csv is already up to date"),
        "{stderr}"
    );
    Ok(())
}

/// a zip saved as `.csv` stays packed, as every `-o` file does, and the warning says how to
/// unpack it; a name that says zip needs none.
#[test]
fn an_archive_saved_under_another_kind_of_name_gets_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let zipped = b"PK\x03\x04 a zip as the server sent it";
    let server = Server::start(
        "/static/public/53/iris.zip",
        vec![file(zipped, "\"z1\""), file(zipped, "\"z1\"")],
    )?;
    let output = sandbox.run(&["--json", "get", &server.url, "-o", "iris_uci.csv"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(fs::read(sandbox.work().join("iris_uci.csv"))?, zipped);
    assert_eq!(
        crate::support::warned(&json(&output)?),
        [
            "iris_uci.csv holds the archive iris.zip, kept packed as it came; use `--dir iris` to unpack it into a folder"
        ]
    );
    let named = sandbox.run(&["get", &server.url, "-o", "iris.zip"])?;
    let stderr = text(&named.stderr)?;
    assert_eq!(named.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
    Ok(())
}

/// the bytes stay as sent with `Content-Encoding: gzip`, which a published hash matches, and one
/// warning says so; a name that says gzip needs none.
#[test]
fn bytes_a_server_compressed_on_their_way_are_kept_with_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let sent = b"\x1f\x8b\x08\x00 gzip as the server sent it";
    let compressed = || {
        reply(
            "200 OK",
            &[
                ("Content-Length", &sent.len().to_string()),
                ("Content-Encoding", "gzip"),
            ],
            sent,
        )
    };
    for (path, warned) in [("/iris.csv", true), ("/iris.csv.gz", false)] {
        let server = Server::start(path, vec![compressed()])?;
        let output = sandbox.run(&["get", &server.url, "-o", "kept"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert_eq!(fs::read(sandbox.work().join("kept"))?, sent, "{path}");
        assert_eq!(
            stderr.matches("warning:").count(),
            usize::from(warned),
            "{stderr}"
        );
        if warned {
            assert!(
                stderr.contains(
                    "warning: iris.csv came compressed with gzip, as the server sent it with Content-Encoding: gzip, and fetchloom keeps the bytes as sent\n  help: decompress it with `gzip -dc`, or read it as gzip, as pandas does with compression=\"gzip\""
                ),
                "{stderr}"
            );
        }
        fs::remove_file(sandbox.work().join("kept"))?;
    }
    Ok(())
}

/// a quoted `~` reaches fetchloom unexpanded, so get writes to a folder named ~ and warns, while
/// `./~` says that folder is meant.
#[test]
fn a_path_starting_with_a_folder_named_tilde_gets_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url, "-o", "~/data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(sandbox.work().join("~").join("data.csv"))?, IRIS);
    let home = sandbox.folder("home")?.join("data.csv");
    assert!(
        stderr.replace('\\', "/").contains(&format!(
            "warning: ~/data.csv starts with a folder named ~ here, not your home folder, since a shell turns ~ into it only outside quotes\n  help: write {}, or ./~/data.csv to keep a folder named ~",
            home.display().to_string().replace('\\', "/")
        )),
        "{stderr}"
    );
    let meant = sandbox.run(&["get", &server.url, "-o", "./~/other.csv"])?;
    let stderr = text(&meant.stderr)?;
    assert_eq!(meant.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
    Ok(())
}

/// offline, what a link served last time stands for what it serves.
#[test]
fn offline_output_keeps_a_file_holding_what_the_link_served_last() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let first = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let output = sandbox.run(&["--offline", "get", &server.url, "-o", "data.csv"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok data.csv already holds what the link served last time"),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// the file holds what the link served last, so the error says the source changed, not the file.
#[test]
fn output_refusing_a_file_the_source_changed_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let newer = b"sepal_length,species\n5.1,setosa\n";
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(newer, "\"v2\"")])?;
    let first = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let output = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "data.csv already exists and holds what the link served last time, and its source changed it since"
    );
    assert_eq!(
        error["help"],
        "add --force to replace it with what the link serves now, or choose another name with -o to keep both"
    );
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    Ok(())
}

/// fetchloom puts no file where the folder now stands, and leaves the folder as it was made.
#[test]
fn an_output_path_that_becomes_a_folder_while_downloading_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let target = sandbox.work().join("data.csv");
    let blocked = target.clone();
    let mut response = vec![Part::Action(Box::new(move || {
        fs::create_dir(&blocked).unwrap();
    }))];
    response.extend(file(IRIS, "\"v1\""));
    let server = Server::start(PATH, vec![response])?;
    let refused = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stdout)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "data.csv became a folder while downloading"
    );
    assert_eq!(error["help"], "choose another name with -o");
    assert_eq!(fs::read_dir(&target)?.count(), 0, "the folder stays empty");
    Ok(())
}

/// the link stays until `--force` replaces the link itself, never what it names.
#[cfg(unix)]
#[test]
fn output_names_a_link_to_nothing_in_its_way() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let placed = sandbox.work().join("data.csv");
    std::os::unix::fs::symlink("nowhere.csv", &placed)?;
    let refused = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stdout)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "data.csv already exists and is a link to nowhere.csv, which does not exist"
    );
    assert_eq!(
        error["help"],
        "add --force to replace the link, or choose another name with -o"
    );
    let forced = sandbox.run(&["get", &server.url, "-o", "data.csv", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS);
    assert!(
        !sandbox.work().join("nowhere.csv").exists(),
        "the link was replaced, not followed"
    );
    Ok(())
}

/// only the download can tell the file apart, which the run says first, not after a long wait.
#[test]
fn output_says_it_downloads_to_compare_a_file_in_its_way() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    fs::write(sandbox.work().join("data.csv"), "mine")?;
    let refused = sandbox.run(&["get", &server.url, "-o", "data.csv"])?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(50), "{stderr}");
    let told = stderr
        .find("   Checking data.csv · already there, so the link downloads first to compare\n");
    let refusal = stderr.find("error: data.csv already exists and holds other bytes");
    assert!(
        told.zip(refusal)
            .is_some_and(|(told, refusal)| told < refusal),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn output_refuses_a_file_holding_other_bytes_until_force() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), unchanged])?;
    let placed = sandbox.work().join("data.csv");
    fs::write(&placed, "mine")?;
    let refused = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stdout)?
    );
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert_eq!(
        error["message"],
        "data.csv already exists and holds other bytes than the link serves now"
    );
    assert_eq!(
        fs::read_to_string(&placed)?,
        "mine",
        "nothing of yours is replaced"
    );

    let forced = sandbox.run(&["--json", "get", &server.url, "-o", "data.csv", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stdout)?);
    let document = json(&forced)?;
    let result = &document["result"];
    assert_eq!(result["file"], placed.display().to_string());
    assert_eq!(result["folder"], sandbox.work().display().to_string());
    assert_eq!(result["files"][0]["path"], "data.csv");
    assert_eq!(
        document["work"]["bytes_down"], 0,
        "the bytes the refused run fetched are used"
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    assert!(!fs::metadata(&placed)?.permissions().readonly());
    assert_eq!(names(&sandbox)?, ["data.csv"], "nothing is left beside it");
    Ok(())
}

#[test]
fn a_killed_download_continues_with_a_range_from_its_last_second() -> Result {
    let body: Vec<u8> = (0..1_048_576u32).flat_map(u32::to_le_bytes).collect();
    let sandbox = Sandbox::new()?;
    // the first answer trickles at 640 KiB a second, so the kill lands inside the body
    let server = Server::slow_file("/big.bin", body.clone())?;
    let mut child = sandbox
        .command(&["--json", "get", &server.url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let on_disk = part_reaches(&sandbox, 1024 * 1024, &mut child)?;
    child.kill()?;
    child.wait()?;
    assert!(
        on_disk >= 1024 * 1024,
        "the first run ended before the kill"
    );

    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let work = &json(&output)?["work"];
    assert_eq!(work["resumes"], 1, "{work}");
    let continued = body.len() as u64 - work["bytes_down"].as_u64().unwrap_or_default();
    assert!(
        continued > 0 && on_disk.saturating_sub(continued) <= 2 * 640 * 1024,
        "{on_disk} bytes were on disk and the download continued from {continued}"
    );
    let asked = &server.requests()[1];
    assert!(
        asked.contains(&format!("range: bytes={continued}-")),
        "{asked}"
    );
    assert!(asked.contains("if-range: \"v1\""), "{asked}");
    assert_eq!(fs::read(sandbox.work().join("big").join("big.bin"))?, body);
    Ok(())
}

/// a stop the person asked for is no error: a note offers a second ctrl-c, then a `Stopped` line
/// says where and a dim line what the download kept, with exit code 130.
#[cfg(unix)]
#[test]
fn a_stopped_get_says_so_in_a_plain_line() -> Result {
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    let sandbox = Sandbox::new()?;
    let server = Server::held_file("/big.bin", body)?;
    let mut child = sandbox
        .command(&["get", &server.url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let arrived = part_reaches(&sandbox, 1, &mut child)?;
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    let output = child.wait_with_output()?;
    let stderr = text(&output.stderr)?;
    assert!(arrived > 0 && sent.success(), "{stderr}");
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let told: Vec<&str> = stderr.lines().collect();
    assert!(
        told.contains(&"   Stopping (press ctrl-c again to quit at once)"),
        "{stderr}"
    );
    assert!(!stderr.contains("error:"), "{stderr}");
    let [.., stopped, kept] = told.as_slice() else {
        return Err(format!("stderr ends before the stop's lines: {stderr}").into());
    };
    assert_eq!(
        *stopped, "    Stopped while talking to 127.0.0.1",
        "{stderr}"
    );
    assert!(
        kept.starts_with("            the ")
            && kept.ends_with(" downloaded so far is kept, and the next run continues from there"),
        "{stderr}"
    );
    Ok(())
}

/// a server like python's http.server, which names no version and sends whole files, cannot
/// continue, so the rerun starts over and says so once, though a broken connection restarts it.
#[cfg(unix)]
#[test]
fn a_stop_promises_only_what_the_server_allows() -> Result {
    // more than the writer gathers before it writes, so the bytes reach the disk
    let body: Vec<u8> = (0..4u32 << 20).map(|n| (n % 251) as u8).collect();
    let length = body.len().to_string();
    let head = [("Content-Length", length.as_str())];
    let (release, wait) = std::sync::mpsc::channel();
    let mut stopped = reply("200 OK", &head, &body[..2 << 20]);
    stopped.push(Part::Wait(wait));
    let broken = reply("200 OK", &head, &body[..1 << 20]);
    let server = Server::start(
        "/big.bin",
        vec![stopped, broken, reply("200 OK", &head, &body)],
    )?;
    let sandbox = Sandbox::new()?;
    let mut child = sandbox
        .command(&["get", &server.url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let arrived = part_reaches(&sandbox, 1, &mut child)?;
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    let output = child.wait_with_output()?;
    drop(release);
    let stderr = text(&output.stderr)?;
    assert!(arrived > 0 && sent.success(), "{stderr}");
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let kept = stderr.lines().last().unwrap_or_default();
    assert!(
        kept.starts_with("            the server cannot continue from the ")
            && kept.ends_with(" downloaded so far, so the next run starts over"),
        "{stderr}"
    );
    let rerun = sandbox.run(&["get", &server.url])?;
    let stderr = text(&rerun.stderr)?;
    assert_eq!(rerun.status.code(), Some(0), "{stderr}");
    let restarts: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("Restarting"))
        .collect();
    assert!(
        matches!(restarts.as_slice(), [line] if line.starts_with(" Restarting big.bin · the ")
            && line.ends_with(" an earlier run kept cannot be continued")),
        "{stderr}"
    );
    assert_eq!(server.requests().len(), 3, "{stderr}");
    assert_eq!(fs::read(sandbox.work().join("big").join("big.bin"))?, body);
    Ok(())
}

/// waits until an unfinished download in the cache holds `size` bytes, or the run ends, and says
/// how many it holds.
fn part_reaches(sandbox: &Sandbox, size: u64, run: &mut Child) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut on_disk = 0;
    while on_disk < size && run.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        // the size a folder listing shows lags behind a file another process still writes on
        // windows, so each file is asked directly
        on_disk = partial_files(&sandbox.cache())
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "part")
            })
            .filter_map(|path| fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .max()
            .unwrap_or_default();
    }
    Ok(on_disk)
}

#[test]
fn a_busy_server_is_tried_again() -> Result {
    let sandbox = Sandbox::new()?;
    let busy = reply(
        "503 Service Unavailable",
        &[("Content-Length", "0"), ("Retry-After", "0")],
        b"",
    );
    let server = Server::start(PATH, vec![busy, file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    sandbox
        .placed_filters(&stderr)?
        .bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

/// the file is tried again, and the warning is in plain words, never the http library's.
#[test]
fn a_connection_that_breaks_partway_through_a_file_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let length = IRIS.len().to_string();
    let cut = reply(
        "200 OK",
        &[("Content-Length", length.as_str())],
        &IRIS[..IRIS.len() / 2],
    );
    let server = Server::start(PATH, vec![cut, file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "warning: the connection to 127.0.0.1 broke partway through the file. trying again in "
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("decoding"), "{stderr}");
    Ok(())
}

#[test]
fn fetchloom_retries_sets_how_often_a_request_is_tried_again() -> Result {
    let sandbox = Sandbox::new()?;
    let busy = || {
        reply(
            "503 Service Unavailable",
            &[("Content-Length", "0"), ("Retry-After", "0")],
            b"",
        )
    };
    let server = Server::start(PATH, vec![busy(), busy(), file(IRIS, "\"v1\"")])?;
    let output = sandbox
        .command(&["get", &server.url])
        .env("FETCHLOOM_RETRIES", "1")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains("after 2 tries"), "{stderr}");
    assert!(stderr.contains("(try 2 of 2)"), "{stderr}");
    assert_eq!(server.requests().len(), 2);

    let output = sandbox
        .command(&["get", &server.url])
        .env("FETCHLOOM_RETRIES", "many")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("FETCHLOOM_RETRIES is \"many\", which is not a number from 0 to 20"),
        "{stderr}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "a bad setting stops before any request"
    );
    Ok(())
}

/// scripts match the json kind, which says whose side the trouble is on. neither answer says when
/// to come back, so each spends a try.
#[test]
fn giving_up_names_a_busy_or_limiting_server_by_its_kind() -> Result {
    for (status, kind, help) in [
        (
            "503 Service Unavailable",
            "network.status",
            "127.0.0.1 is having trouble",
        ),
        (
            "429 Too Many Requests",
            "network.rate_limited",
            "127.0.0.1 is limiting requests",
        ),
    ] {
        let sandbox = Sandbox::new()?;
        let refused = || reply(status, &[("Content-Length", "0")], b"");
        let server = Server::start(PATH, vec![refused(), refused()])?;
        let output = sandbox
            .command(&["--json", "get", &server.url])
            .env("FETCHLOOM_RETRIES", "1")
            .output()?;
        assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], kind);
        assert_eq!(
            error["message"],
            format!(
                "gave up on 127.0.0.1 after 2 tries: 127.0.0.1 answered {}",
                &status[..3]
            )
        );
        assert_eq!(
            error["help"],
            format!(
                "{help}; run `fetchloom --json get {}` again later",
                server.url
            )
        );
        assert_eq!(server.requests().len(), 2);
    }
    Ok(())
}

/// zenodo's answer once its limit is used up: a 429 that says when the limit resets, by the
/// host's clock in `Date`, which is fixed here so the machine's own clock decides nothing.
fn rate_limited(reset: u64, retry_after: u64) -> Vec<Part> {
    reply(
        "429 Too Many Requests",
        &[
            ("Content-Length", "0"),
            ("Date", "Fri, 30 Oct 2026 08:55:06 GMT"),
            ("X-RateLimit-Limit", "133"),
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", &reset.to_string()),
            ("Retry-After", &retry_after.to_string()),
        ],
        b"",
    )
}

/// more refusals than a request has tries still end in the file. whether a wait gets a line
/// depends on machine load, so `pace::tests` checks that with fixed times.
#[test]
fn a_rate_limit_is_waited_out_without_spending_tries() -> Result {
    let sandbox = Sandbox::new()?;
    // the second of the date above
    let second = 1_793_350_506;
    let mut responses = vec![rate_limited(second + 1, 1)];
    responses.extend((0..5).map(|_| rate_limited(second, 0)));
    responses.push(file(IRIS, "\"v1\""));
    let server = Server::start(PATH, responses)?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(server.requests().len(), 7);
    assert!(!stderr.contains("trying again"), "{stderr}");
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn a_missing_file_fails_with_the_reference_code() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![reply("404 Not Found", &[("Content-Length", "0")], b"")],
    )?;
    let output = sandbox.run(&["get", &server.url])?;
    assert_eq!(output.status.code(), Some(10));
    assert!(
        !sandbox.work().join("iris").exists(),
        "a failed get leaves no folder behind"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

/// get keeps no project, so its help names neither data.toml nor a pin.
#[test]
fn get_errors_speak_of_no_project() -> Result {
    let sandbox = Sandbox::new()?;
    let gone = Server::start(
        "/gone.csv",
        vec![reply("404 Not Found", &[("Content-Length", "0")], b"")],
    )?;
    let fresh = Server::start(PATH, Vec::new())?;
    fs::create_dir(sandbox.work().join("iris"))?;
    let cases = [
        (
            vec!["get", gone.url.as_str(), "-o", "x.csv"],
            "check the link in a browser. if it moved, run get with the new link",
        ),
        (
            vec!["--offline", "get", fresh.url.as_str(), "-o", "x.csv"],
            "run it again without --offline to download it once. after that, `fetchloom get <link> --offline` reuses it",
        ),
        (
            vec!["get", fresh.url.as_str(), "--select", "*.data"],
            "compare the pattern with the paths above, then fix --select",
        ),
        (vec!["get", "iris"], "run `fetchloom search iris` to search"),
    ];
    for (args, help) in cases {
        let document = json(&sandbox.run(&[&["--json"][..], &args].concat())?)?;
        let error = &document["errors"][0];
        assert_eq!(error["help"], help, "{args:?}: {document}");
        for failure in error["failures"].as_array().into_iter().flatten() {
            assert_eq!(failure["help"], help, "{args:?}: {document}");
        }
    }
    // a catalog on this machine stands in for zenodo, which debug builds alone allow
    #[cfg(debug_assertions)]
    {
        let nobody = Server::start(
            "/",
            vec![reply("404 Not Found", &[("Content-Length", "0")], b"")],
        )?;
        let output = sandbox
            .command(&["--json", "get", "zenodo:1"])
            .env("FETCHLOOM_TEST_ZENODO_API", &nobody.url)
            .output()?;
        assert_eq!(
            json(&output)?["errors"][0]["help"],
            "check the id in a browser, then run get with the right one. a dataset that was removed cannot be fetched"
        );
    }
    assert_eq!(fresh.requests(), Vec::<String>::new());
    Ok(())
}

#[test]
fn offline_uses_only_what_the_cache_holds() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let before = sandbox.run(&["--offline", "get", &server.url])?;
    assert_eq!(before.status.code(), Some(20));
    assert!(
        server.requests().is_empty(),
        "offline never touches the network"
    );
    let stderr = text(&before.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));

    sandbox.run(&["get", &server.url])?;
    let after = sandbox
        .command(&["--json", "get", &server.url, "--dir", "again"])
        .env("FETCHLOOM_OFFLINE", "1")
        .output()?;
    assert_eq!(after.status.code(), Some(0), "{}", text(&after.stdout)?);
    let document = json(&after)?;
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(document["result"]["files"][0]["origin"], "cache");
    assert_eq!(
        fs::read(sandbox.work().join("again").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn a_folder_holding_other_files_is_refused_before_downloading() -> Result {
    let sandbox = Sandbox::new()?;
    fs::create_dir(sandbox.work().join("iris"))?;
    fs::write(sandbox.work().join("iris").join("notes.txt"), "mine")?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(50));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "local.not_empty");
    assert_eq!(document["work"]["bytes_down"], 0);
    Ok(())
}

#[test]
fn a_file_of_your_own_is_never_overwritten() -> Result {
    let sandbox = Sandbox::new()?;
    fs::create_dir(sandbox.work().join("iris"))?;
    fs::write(sandbox.work().join("iris").join("iris.csv"), "my edits")?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    assert_eq!(output.status.code(), Some(50));
    assert_eq!(
        fs::read_to_string(sandbox.work().join("iris").join("iris.csv"))?,
        "my edits"
    );
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn a_changed_file_replaces_the_copy_fetched_before() -> Result {
    let sandbox = Sandbox::new()?;
    let newer = b"sepal_length,species\n5.1,setosa\n";
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(newer, "\"v2\"")])?;
    sandbox.run(&["get", &server.url])?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        newer
    );
    sandbox
        .placed_filters(&stderr)?
        .bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

#[test]
fn a_redirect_into_this_machine_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let inward = reply(
        "302 Found",
        &[
            ("Location", "http://localhost:9/admin"),
            ("Content-Length", "0"),
        ],
        b"",
    );
    let server = Server::start(PATH, vec![inward])?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(20));
    assert_eq!(json(&output)?["errors"][0]["kind"], "network.refused");
    Ok(())
}

#[test]
fn a_large_download_needs_a_yes_when_nobody_can_answer() -> Result {
    let sandbox = Sandbox::new()?;
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let server = Server::start("/huge.bin", vec![huge(), huge()])?;
    let output = sandbox.run(&["get", &server.url])?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    // the question comes before the body is read, so nothing was downloaded or kept, not even
    // an empty unfinished download that `fetchloom cache` would count
    let document = json(&sandbox.run(&["--json", "get", &server.url, "-o", "huge.bin"])?)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(document["work"]["bytes_down"], 0);
    assert_eq!(document["work"]["objects_written"], 0);
    assert!(!sandbox.work().join("huge").exists());
    assert!(!sandbox.work().join("huge.bin").exists());
    assert_eq!(
        partial_files(&sandbox.cache()),
        Vec::<std::path::PathBuf>::new()
    );
    Ok(())
}

/// no or ctrl-c at a terminal's question gets one line saying what was cancelled and that nothing
/// changed, and the exit codes stay 1 and 130.
#[cfg(target_os = "linux")]
#[test]
fn a_large_download_cancelled_at_its_question_is_no_error() -> Result {
    let sandbox = Sandbox::new()?;
    let huge = || reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let server = Server::start("/huge.bin", vec![huge(), huge()])?;
    let command = sandbox.command(&["get", &server.url]);
    for (keys, code) in [("n\r", 1), ("\u{3}", 130)] {
        // -isig lets ctrl-c reach the question as the key it reads, whenever it is typed
        let Some((exit, screen)) = crate::support::typed(
            &sandbox.work(),
            &command,
            "-isig",
            "download huge.bin (2.0 GiB)?",
            keys,
        )?
        else {
            return Ok(());
        };
        assert!(screen.contains("download huge.bin (2.0 GiB)?"), "{screen}");
        assert_eq!(exit, Some(code), "{keys:?}: {screen}");
        assert!(
            screen.contains("  Cancelled the download of huge.bin (2.0 GiB), so nothing changed\n"),
            "{keys:?}: {screen}"
        );
        assert!(!screen.contains("error:"), "{keys:?}: {screen}");
        assert!(!sandbox.work().join("huge").exists(), "{screen}");
        assert_eq!(
            partial_files(&sandbox.cache()),
            Vec::<std::path::PathBuf>::new(),
            "{keys:?}"
        );
    }
    Ok(())
}

/// with `FETCHLOOM_ACCESSIBLE=1`, even `TERM=dumb` gets the question, free of escape codes, and
/// yes downloads; CI, a coding agent and --json never ask.
#[cfg(target_os = "linux")]
#[test]
fn an_accessible_terminal_asks_on_plain_lines_and_ci_never_does() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, (0..8).map(|_| file(IRIS, "\"v1\"")).collect())?;
    let get = |args: &[&str], unattended: Option<(&str, &str)>| {
        let mut command =
            sandbox.command(&[&["get", &server.url, "-o", "iris.csv"], args].concat());
        // the plain line that tells the download begins comes as its size is known, as the
        // question does, and never inside it
        command
            .env("FETCHLOOM_ASK_ABOVE", "1")
            .env("FETCHLOOM_ACCESSIBLE", "1")
            .env("FETCHLOOM_TEST_TICKER", "on")
            .env("TERM", "dumb");
        if let Some((name, value)) = unattended {
            command.env(name, value);
        }
        command
    };
    for (args, unattended) in [
        (&[][..], Some(("CI", "true"))),
        (&[][..], Some(("CLAUDECODE", "1"))),
        (&["--json"][..], None),
    ] {
        let Some((code, screen)) =
            crate::support::typed(&sandbox.work(), &get(args, unattended), "", "", "")?
        else {
            return Ok(());
        };
        assert_eq!(code, Some(1), "{args:?} {unattended:?}: {screen}");
        assert!(
            !screen.contains("[y/N]"),
            "{args:?} {unattended:?}: {screen}"
        );
    }
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &get(&[], None), "", "[y/N] ", "y\r")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    let asked = format!("download iris.csv ({} B)? [y/N] y\n", IRIS.len());
    assert!(screen.contains(&asked), "{screen}");
    assert!(!screen.contains('\u{1b}'), "{screen:?}");
    assert_eq!(fs::read(sandbox.work().join("iris.csv"))?, IRIS);
    Ok(())
}

#[test]
fn the_server_can_name_the_file() -> Result {
    let sandbox = Sandbox::new()?;
    let named = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            (
                "Content-Disposition",
                "attachment; filename=\"measurements.csv\"",
            ),
        ],
        IRIS,
    );
    let server = Server::start("/download?id=5", vec![named])?;
    let output = sandbox.run(&["get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("measurements").join("measurements.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn a_synced_folder_gets_a_reflink_or_a_copy() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox
        .command(&["get", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // sync clients keep a reflink like any file of its own but break a hardlink, so a filesystem
    // without reflinks gets a copy, and the copy is explained
    let name = if sandbox.links()?.reflink {
        "a_synced_folder_gets_a_reflink"
    } else {
        "a_synced_folder_gets_a_copy_and_says_why"
    };
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!(name, stderr));
    Ok(())
}

#[test]
fn two_runs_of_one_link_download_it_once() -> Result {
    let sandbox = Sandbox::new()?;
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, held) = std::sync::mpsc::channel();
    slow.push(Part::Wait(held));
    slow.push(Part::Bytes(IRIS.to_vec()));
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![slow, unchanged])?;
    let first = crate::support::Running::start(&mut sandbox.command(&[
        "get",
        &server.url,
        "--dir",
        "first",
    ]))?;
    server.wait_for_requests(1)?;
    let second = crate::support::Running::start(
        sandbox
            .command(&["get", &server.url, "--dir", "second"])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    let waiting = second.wait_for("Waiting iris.csv · another fetchloom is downloading it");
    drop(release);
    waiting?;
    let second = second.finish()?;
    let first = first.finish()?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr)?);
    assert!(
        !text(&second.stderr)?.contains("Downloading"),
        "{}",
        text(&second.stderr)?
    );
    let downloads = server
        .requests()
        .iter()
        .filter(|head| !head.contains("if-none-match"))
        .count();
    assert_eq!(
        downloads, 1,
        "however the two runs overlap, the file is downloaded once"
    );
    assert_eq!(
        fs::read(sandbox.work().join("second").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// the second run waits, then keeps the file as a later run would, though its bytes left the
/// cache: the server answers unchanged, and with `--sha256` nobody asks.
#[test]
fn a_second_output_of_one_link_keeps_the_file_the_first_wrote() -> Result {
    for extra in [&[][..], &["--sha256", IRIS_SHA256][..]] {
        let sandbox = Sandbox::new()?;
        let mut slow = reply(
            "200 OK",
            &[
                ("Content-Length", &IRIS.len().to_string()),
                ("ETag", "\"v1\""),
            ],
            b"",
        );
        let (release, held) = std::sync::mpsc::channel();
        slow.push(Part::Wait(held));
        slow.push(Part::Bytes(IRIS.to_vec()));
        let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
        let server = Server::start(PATH, vec![slow, unchanged])?;
        let args = [&["get", &server.url, "-o", "out/data.csv"][..], extra].concat();
        let first = crate::support::Running::start(&mut sandbox.command(&args))?;
        server.wait_for_requests(1)?;
        let second = crate::support::Running::start(
            sandbox.command(&args).env("FETCHLOOM_TEST_TICKER", "on"),
        )?;
        let waiting = second.wait_for("Waiting iris.csv · another fetchloom is downloading it");
        drop(release);
        waiting?;
        let second = second.finish()?;
        let first = first.finish()?;
        assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
        assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr)?);
        let downloads = server
            .requests()
            .iter()
            .filter(|head| !head.contains("if-none-match"))
            .count();
        assert_eq!(downloads, 1, "{extra:?}: the second run downloads nothing");
        assert_eq!(fs::read(sandbox.work().join("out").join("data.csv"))?, IRIS);
    }
    Ok(())
}

/// a folder named through a link is common where a drive is mounted into a home folder.
#[test]
fn a_folder_named_through_a_link_gets_the_files_and_their_updates() -> Result {
    let sandbox = Sandbox::new()?;
    let newer = b"sepal_length,species\n5.1,setosa\n";
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(newer, "\"v2\"")])?;
    let real = sandbox.folder("real")?;
    if !link_folder(&real, &sandbox.work().join("linked")) {
        return Ok(());
    }
    for (bytes, run) in [(IRIS, "first"), (newer, "second")] {
        let output = sandbox.run(&["get", &server.url, "--dir", "linked"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{run} run: {stderr}");
        assert_eq!(fs::read(real.join("iris.csv"))?, bytes, "{run} run");
    }
    Ok(())
}

/// the count says files written to the cache, never objects, the cache's own word for them.
#[test]
fn verbose_counters_name_new_files_in_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["-v", "get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(" · 1 file written to the cache · "),
        "{stderr}"
    );
    assert!(!stderr.contains("new object"), "{stderr}");
    Ok(())
}

#[test]
fn get_reports_zero_byte_copies_in_a_synced_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/empty.csv", vec![file(b"", "\"empty\"")])?;
    let output = sandbox
        .command(&["get", &server.url, "-v"])
        .env("OneDrive", sandbox.work())
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    if sandbox.links()?.reflink {
        assert!(stderr.contains("linked to the cache"), "{stderr}");
    } else {
        assert!(stderr.contains("copied, 0 B"), "{stderr}");
        assert!(stderr.contains("1 copy"), "{stderr}");
        assert!(stderr.contains("is inside onedrive"), "{stderr}");
    }
    assert_eq!(
        fs::metadata(sandbox.work().join("empty/empty.csv"))?.len(),
        0
    );
    Ok(())
}

#[test]
fn data_file_links_warn_when_the_server_returns_html() -> Result {
    for (path, warns) in [
        ("/iris.csv", true),
        ("/iris.CSV", true),
        ("/index.html", false),
        ("/index.HTM", false),
        ("/index.xhtml", false),
    ] {
        let sandbox = Sandbox::new()?;
        let body = b"<html>not the data</html>";
        let server = Server::start(
            path,
            vec![reply(
                "200 OK",
                &[("Content-Type", "text/html; charset=utf-8")],
                body,
            )],
        )?;
        let output = sandbox.run(&["get", &server.url])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let message = format!(
            "{} came back as a web page, not a data file. the link may open a page instead of downloading\n  help: look for a download or raw link",
            &path[1..]
        );
        assert_eq!(stderr.contains(&message), warns, "{stderr}");
    }
    Ok(())
}

#[test]
fn a_link_that_answers_with_a_web_page_is_refused_before_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let listing = || {
        reply(
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8")],
            b"<html><a href=\"part-1.zip\">part-1.zip</a></html>",
        )
    };
    let server = Server::start("/shards/", vec![listing(), listing()])?;
    for command in ["get", "add"] {
        let output = sandbox.run(&["--json", command, &server.url])?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(10), "{command}: {document}");
        assert_eq!(document["errors"][0]["kind"], "source.invalid");
        assert_eq!(
            document["errors"][0]["message"],
            format!(
                "{} answered with a web page, such as a folder listing, not a file",
                server.url
            )
        );
        assert_eq!(document["work"]["objects_written"], 0);
    }
    assert_eq!(server.requests().len(), 2);
    assert_eq!(objects(&sandbox), 0);
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join("shards").exists());
    Ok(())
}

#[test]
fn repository_file_pages_report_the_raw_link_before_downloading() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&[
        "get",
        "https://github.com/mwaskom/seaborn-data/blob/master/iris.csv",
        "--offline",
    ])?;
    let stderr = text(&output.stderr)?;
    assert!(!output.status.success());
    assert!(stderr.contains("Using raw file"), "{stderr}");
    assert!(
        stderr.contains("https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn extensionless_links_use_redirect_names_and_prefer_final_names() -> Result {
    for final_name in [None, Some("final.mat")] {
        let sandbox = Sandbox::new()?;
        let mut headers = vec![("Content-Type", "application/octet-stream")];
        let disposition = final_name.map(|name| format!("attachment; filename={name}"));
        if let Some(value) = disposition.as_deref() {
            headers.push(("Content-Disposition", value));
        }
        let server = Server::start(
            "/files/9778519",
            vec![
                reply(
                    "302 Found",
                    &[
                        ("Location", "/storage"),
                        ("Content-Disposition", "attachment;filename=dir.mat"),
                    ],
                    b"",
                ),
                reply("200 OK", &headers, IRIS),
            ],
        )?;
        let output = sandbox.run(&["get", &server.url])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        let (folder, name) = if final_name.is_some() {
            ("final", "final.mat")
        } else {
            ("dir", "dir.mat")
        };
        assert_eq!(fs::read(sandbox.work().join(folder).join(name))?, IRIS);
        let offline = sandbox.run(&["get", &server.url, "--offline", "--dir", "cached"])?;
        assert!(offline.status.success(), "{}", text(&offline.stderr)?);
        assert_eq!(fs::read(sandbox.work().join("cached").join(name))?, IRIS);
        assert_eq!(server.requests().len(), 2);
    }
    Ok(())
}

#[test]
fn unnamed_numeric_links_keep_the_number_and_suggest_an_article_or_doi() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/9778519", vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(sandbox.work().join("9778519").join("9778519"))?,
        IRIS
    );
    assert!(stderr.contains("article or DOI link"), "{stderr}");
    Ok(())
}

#[test]
fn offline_identifiers_explain_why_lookup_needs_the_internet() -> Result {
    for reference in [
        "zenodo:3608990",
        "figshare:123",
        "doi:10.5281/zenodo.3608990",
    ] {
        let sandbox = Sandbox::new()?;
        let output = sandbox.run(&["get", reference, "--offline"])?;
        assert_eq!(output.status.code(), Some(20));
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.contains(&format!("looking up {reference} needs the internet")),
            "{stderr}"
        );
        assert!(
            stderr.contains(
                "help: run it again without --offline. after that, a link that keeps one file works offline too"
            ),
            "{stderr}"
        );
        assert!(!stderr.contains("project"), "{stderr}");
        assert!(!stderr.contains("to pin it"), "{stderr}");
    }
    Ok(())
}

const WINE: &[u8] = b"alcohol,malic_acid,class\n14.23,1.71,1\n";
const IRIS_SHA256: &str = "d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d";

/// two links on servers of their own, each serving its file once.
fn two_files() -> Result<(Server, Server)> {
    Ok((
        Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?,
        Server::start("/wine.csv", vec![file(WINE, "\"w1\"")])?,
    ))
}

#[test]
fn several_links_land_in_folders_of_their_own_inside_the_output() -> Result {
    let sandbox = Sandbox::new()?;
    let (iris, wine) = two_files()?;
    let output = sandbox.run(&["get", &iris.url, &wine.url, "--dir", "downloads"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let downloads = sandbox.work().join("downloads");
    assert_eq!(fs::read(downloads.join("iris").join("iris.csv"))?, IRIS);
    assert_eq!(fs::read(downloads.join("wine").join("wine.csv"))?, WINE);
    assert_eq!(
        text(&output.stdout)?,
        format!(
            "{}\n{}\n",
            downloads.join("iris").display(),
            downloads.join("wine").display()
        ),
        "one folder a line, in the order given"
    );
    assert!(stderr.contains("Finished 2 links · 2 files"), "{stderr}");
    Ok(())
}

#[test]
fn an_input_file_lists_links_and_skips_blank_lines_and_comments() -> Result {
    let sandbox = Sandbox::new()?;
    let (iris, wine) = two_files()?;
    // links in a list are not typed on the command line, so their servers must be allowed
    sandbox.allow(&iris.url);
    sandbox.allow(&wine.url);
    let list = format!(
        "# measurements to compare\r\n\r\n{}\n   {}   \n# done\n",
        iris.url, wine.url
    );
    fs::write(sandbox.work().join("links.txt"), list)?;
    let output = sandbox.run(&["--json", "get", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let items = document["result"]["items"]
        .as_array()
        .ok_or("items is not an array")?;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["reference"], iris.url.as_str());
    assert_eq!(items[1]["reference"], wine.url.as_str());
    assert!(items.iter().all(|item| item["status"] == "ok"), "{items:?}");
    assert_eq!(document["result"]["folder"], serde_json::Value::Null);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(
        fs::read(sandbox.work().join("wine").join("wine.csv"))?,
        WINE
    );
    Ok(())
}

#[test]
fn get_reads_an_optional_sha256_per_line() -> Result {
    let sandbox = Sandbox::new()?;
    let (iris, wine) = two_files()?;
    sandbox.allow(&iris.url);
    sandbox.allow(&wine.url);
    let wrong = "0".repeat(64);
    fs::write(
        sandbox.work().join("links.txt"),
        format!("{} sha256:{IRIS_SHA256}\n{} {wrong}\n", iris.url, wine.url),
    )?;
    let output = sandbox.run(&["--json", "get", "-i", "links.txt", "--dir", "out"])?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let items = &document["result"]["items"];
    assert_eq!(items[0]["status"], "ok", "{items}");
    assert_eq!(items[1]["status"], "error", "{items}");
    assert_eq!(items[1]["error"]["kind"], "integrity.mismatch");
    assert_eq!(
        items[1]["error"]["message"],
        "wine.csv does not match the SHA-256 on line 2 of links.txt"
    );
    let out = sandbox.work().join("out");
    assert_eq!(fs::read(out.join("iris").join("iris.csv"))?, IRIS);
    assert!(
        !out.join("wine").exists(),
        "a wrong hash keeps its file out"
    );
    Ok(())
}

#[test]
fn a_dash_reads_the_links_from_stdin() -> Result {
    let sandbox = Sandbox::new()?;
    let (iris, wine) = two_files()?;
    sandbox.allow(&iris.url);
    sandbox.allow(&wine.url);
    let mut child = sandbox
        .command(&["get", "--input-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("stdin was not piped")?;
    std::io::Write::write_all(
        &mut stdin,
        format!("{}\n{}\n", iris.url, wine.url).as_bytes(),
    )?;
    drop(stdin);
    let output = child.wait_with_output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(
        fs::read(sandbox.work().join("wine").join("wine.csv"))?,
        WINE
    );
    Ok(())
}

#[test]
fn one_failed_link_leaves_the_others_and_fails_the_run() -> Result {
    let sandbox = Sandbox::new()?;
    let unchanged = || reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let gone = || reply("404 Not Found", &[("Content-Length", "0")], b"");
    let iris = Server::start("/iris.csv", vec![file(IRIS, "\"v1\""), unchanged()])?;
    let missing = Server::start("/gone.csv", vec![gone(), gone()])?;
    let wine = Server::start("/wine.csv", vec![file(WINE, "\"w1\"")])?;
    let output = sandbox.run(&["--json", "get", &iris.url, &missing.url, &wine.url])?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(
        fs::read(sandbox.work().join("wine").join("wine.csv"))?,
        WINE
    );
    assert!(!sandbox.work().join("gone").exists());
    let document = json(&output)?;
    assert_eq!(document["status"], "error");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved");
    assert_eq!(error["message"], "1 of 3 links could not be fetched");
    assert_eq!(error["failures"][0]["file"], missing.url.as_str());
    assert_eq!(error["failures"][0]["kind"], "reference.unresolved");
    let items = document["result"]["items"]
        .as_array()
        .ok_or("a failed run keeps its items")?;
    let statuses: Vec<&str> = items
        .iter()
        .filter_map(|item| item["status"].as_str())
        .collect();
    assert_eq!(statuses, ["ok", "error", "ok"]);
    assert_eq!(items[1]["error"]["kind"], "reference.unresolved");
    assert_eq!(items[1]["folder"], serde_json::Value::Null);
    assert_eq!(
        items[2]["folder"],
        sandbox.work().join("wine").display().to_string()
    );
    assert_eq!(items[2]["files"][0]["path"], "wine.csv");

    let told = sandbox.run(&["get", &iris.url, &missing.url])?;
    let stderr = text(&told.stderr)?;
    assert_eq!(told.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("error: 1 of 2 links could not be fetched"),
        "{stderr}"
    );
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
    Ok(())
}

/// the cause and its help come once, with each link once, while `--json` keeps every link's own
/// failure; `add -i` tells the cause once too.
#[test]
fn links_that_fail_alike_are_told_once() -> Result {
    let sandbox = Sandbox::new()?;
    let site = crate::support::Routes::start(vec![(
        "/denied.csv".to_owned(),
        reply("403 Forbidden", &[("Content-Length", "0")], b""),
    )])?;
    sandbox.allow(&site.url);
    let links: Vec<String> = ["gone/a.csv", "gone/b.csv", "denied.csv", "gone/d.csv"]
        .iter()
        .map(|path| format!("{}/{path}", site.url))
        .collect();
    fs::write(sandbox.work().join("links.txt"), links.join("\n"))?;
    let output = sandbox.run(&["get", "-i", "links.txt"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    for link in &links {
        assert_eq!(stderr.matches(link.as_str()).count(), 1, "{link}\n{stderr}");
    }
    assert_eq!(stderr.matches("has nothing at").count(), 1, "{stderr}");
    assert_eq!(stderr.matches("if it moved").count(), 1, "{stderr}");
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));

    let document = json(&sandbox.run(&["--json", "get", "-i", "links.txt"])?)?;
    let failures = document["errors"][0]["failures"]
        .as_array()
        .ok_or("each failed link keeps its failure")?;
    assert_eq!(failures.len(), links.len(), "{document}");
    for (failure, link) in failures.iter().zip(&links) {
        assert_eq!(failure["file"], link.as_str(), "{document}");
        let message = failure["message"].as_str().unwrap_or_default();
        assert!(message.contains(link.as_str()), "{document}");
    }

    let added = sandbox.run(&["add", "-i", "links.txt"])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(10), "{stderr}");
    assert_eq!(stderr.matches("has nothing at").count(), 1, "{stderr}");
    assert_eq!(stderr.matches("if it moved").count(), 1, "{stderr}");
    Ok(())
}

#[test]
fn a_matching_sha256_lets_the_file_in() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let output = sandbox.run(&["get", &server.url, "--sha256", &IRIS_SHA256.to_uppercase()])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// `--sha256` takes a hash as `-i` lists and sites write it, after `sha256:`.
#[test]
fn sha256_takes_the_form_sites_write() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let prefixed = format!("sha256:{IRIS_SHA256}");
    let output = sandbox.run(&["get", &server.url, "-o", "data.csv", "--sha256", &prefixed])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(sandbox.work().join("data.csv"))?, IRIS);
    Ok(())
}

/// a hash that differs late still reads apart: both print whole, one under the other within 80
/// columns, to check against a published checksum, and JSON gives them in one detail.
#[test]
fn another_sha256_stops_the_file_before_it_is_placed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let late = if &IRIS_SHA256[30..31] == "0" {
        "1"
    } else {
        "0"
    };
    let wrong = format!("{}{late}{}", &IRIS_SHA256[..30], &IRIS_SHA256[31..]);
    let output = sandbox.run(&["--json", "get", &server.url, "--sha256", &wrong])?;
    assert_eq!(output.status.code(), Some(30), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "integrity.mismatch");
    assert_eq!(error["message"], "iris.csv does not match --sha256");
    assert_eq!(
        error["details"][0],
        format!("expected sha256 {wrong}, got {IRIS_SHA256}")
    );
    let told = sandbox.run(&["get", &server.url, "--sha256", &wrong])?;
    assert_eq!(told.status.code(), Some(30));
    assert!(
        text(&told.stderr)?.contains(&format!("  expected {wrong}\n  got      {IRIS_SHA256}\n")),
        "{}",
        text(&told.stderr)?
    );
    assert!(!sandbox.work().join("iris").exists());
    Ok(())
}

#[test]
fn several_links_take_no_search_words_and_one_sha256_takes_one_link() -> Result {
    let sandbox = Sandbox::new()?;
    for (arguments, expected) in [
        (
            &["get", "fashion", "mnist"][..],
            "quote the words of one search, as in `fetchloom get \"fashion mnist\"`, or get one dataset at a time",
        ),
        (
            &["get", "https://example.org/iris.csv", "iris"],
            "\"iris\" is not a link, a DOI or an id",
        ),
        (
            &[
                "get",
                "https://example.org/iris.csv",
                "https://example.org/wine.csv",
                "--sha256",
                IRIS_SHA256,
            ],
            "--sha256 checks one file, and this get has 2 links",
        ),
        (
            &["get", "https://example.org/iris.csv", "--sha256", "abc"],
            "error: --sha256 cannot be \"abc\"\n  help: a SHA-256 is 64 hex digits",
        ),
    ] {
        let output = sandbox.run(arguments)?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {stderr}");
        assert!(stderr.contains(expected), "{arguments:?}: {stderr}");
    }
    fs::write(
        sandbox.work().join("links.txt"),
        "https://example.org/iris.csv\nfashion mnist\n",
    )?;
    let listed = sandbox.run(&["get", "-i", "links.txt"])?;
    let stderr = text(&listed.stderr)?;
    assert_eq!(listed.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("line 2 of links.txt is not a link, a DOI or an id"),
        "{stderr}"
    );
    fs::write(sandbox.work().join("empty.txt"), "# nothing yet\n\n")?;
    let empty = sandbox.run(&["get", "-i", "empty.txt"])?;
    assert_eq!(empty.status.code(), Some(2));
    let stderr = text(&empty.stderr)?;
    assert!(stderr.contains("empty.txt lists no links"), "{stderr}");
    assert!(
        stderr.contains("help: put one link per line in it."),
        "{stderr}"
    );
    Ok(())
}
#[test]
fn links_named_alike_each_get_their_own_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let first = Server::start("/data.csv", vec![file(IRIS, "\"v1\"")])?;
    let second = Server::start("/data.csv", vec![file(WINE, "\"w1\"")])?;
    let output = sandbox.run(&["--json", "get", &first.url, &second.url])?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    assert_eq!(
        fs::read(sandbox.work().join("data").join("data.csv"))?,
        IRIS
    );
    assert_eq!(
        fs::read(sandbox.work().join("data-2").join("data.csv"))?,
        WINE
    );
    let document = json(&output)?;
    let folders: Vec<String> = document["result"]["items"]
        .as_array()
        .ok_or("no items")?
        .iter()
        .filter_map(|item| item["folder"].as_str().map(str::to_owned))
        .collect();
    assert!(
        folders[0].ends_with("data") && folders[1].ends_with("data-2"),
        "{folders:?}"
    );
    Ok(())
}

/// a link or junction to the first link's folder is that folder, so the second gets its own.
#[test]
fn a_folder_reached_through_a_link_is_not_filled_twice() -> Result {
    let sandbox = Sandbox::new()?;
    let one = sandbox.work().join("one");
    fs::create_dir_all(&one)?;
    if !link_folder(&one, &sandbox.work().join("two")) {
        return Ok(());
    }
    let zipped = |bytes: &[u8]| -> Result<Vec<u8>> { zip_of(&[("data.csv", bytes)]) };
    let first = Server::start("/one.zip", vec![file(&zipped(IRIS)?, "\"one\"")])?;
    let second = Server::start("/two.zip", vec![file(&zipped(WINE)?, "\"two\"")])?;
    let output = sandbox.run(&["--json", "get", &first.url, &second.url])?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    assert_eq!(fs::read(one.join("data.csv"))?, IRIS);
    assert_eq!(
        fs::read(sandbox.work().join("two-2").join("data.csv"))?,
        WINE
    );
    let document = json(&output)?;
    let folders: Vec<&str> = document["result"]["items"]
        .as_array()
        .ok_or("no items")?
        .iter()
        .filter_map(|item| item["folder"].as_str())
        .collect();
    assert!(
        folders.len() == 2 && folders[0].ends_with("one") && folders[1].ends_with("two-2"),
        "{folders:?}"
    );
    Ok(())
}

#[test]
fn duplicate_stems_reuse_their_folders_on_a_rerun() -> Result {
    for listed in [false, true] {
        let sandbox = Sandbox::new()?;
        let first = Server::start(
            "/data.csv",
            vec![
                file(IRIS, "\"v1\""),
                reply("304 Not Modified", &[("ETag", "\"v1\"")], b""),
            ],
        )?;
        sandbox.allow(&first.url);
        let second = Server::start(
            "/data.tsv",
            vec![
                file(WINE, "\"w1\""),
                reply("304 Not Modified", &[("ETag", "\"w1\"")], b""),
            ],
        )?;
        sandbox.allow(&second.url);
        fs::write(
            sandbox.work().join("links.txt"),
            format!("{}\n{}\n", first.url, second.url),
        )?;
        let args = if listed {
            vec!["--json", "get", "-i", "links.txt", "--dir", "downloads"]
        } else {
            vec![
                "--json",
                "get",
                first.url.as_str(),
                second.url.as_str(),
                "--dir",
                "downloads",
            ]
        };
        for again in [false, true] {
            let output = sandbox.run(&args)?;
            assert!(
                output.status.success(),
                "listed={listed}, again={again}: {}",
                text(&output.stdout)?
            );
            let document = json(&output)?;
            let items = document["result"]["items"].as_array().ok_or("no items")?;
            assert_eq!(items.len(), 2);
            assert!(items.iter().all(|item| item["status"] == "ok"));
            let parent = sandbox.work().join("downloads");
            for (index, folder, name, bytes) in [
                (0, "data", "data.csv", IRIS),
                (1, "data-2", "data.tsv", WINE),
            ] {
                assert_eq!(
                    items[index]["folder"],
                    parent.join(folder).display().to_string()
                );
                assert_eq!(fs::read(parent.join(folder).join(name))?, bytes);
            }
            if again {
                assert_eq!(document["work"]["bytes_down"], 0);
            }
        }
        assert_eq!(first.requests().len(), 2);
        assert_eq!(second.requests().len(), 2);
    }
    Ok(())
}

/// `--yes` goes on anyway, as a disk that compresses what it stores may hold it.
#[test]
fn a_download_the_cache_disk_cannot_hold_asks_first() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2048];
    let server = Server::start(PATH, vec![file(&body, "\"v1\""), file(&body, "\"v1\"")])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_TEST_FREE", "1000")
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "downloading iris.csv needs at least 2.0 KiB on the disk holding the cache at {}, which has 1000 B free",
            sandbox.cache().display()
        )
    );
    assert!(
        document["errors"][0]["help"]
            .as_str()
            .is_some_and(|help| help.contains("FETCHLOOM_CACHE") && help.contains("--yes")),
        "{}",
        document["errors"][0]["help"]
    );
    assert_eq!(document["work"]["bytes_down"], 0);
    let output = sandbox
        .command(&["get", "--yes", &server.url])
        .env("FETCHLOOM_TEST_FREE", "1000")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    Ok(())
}

/// the bytes are counted as they come, so it stops before the disk fills; `--yes` goes on.
#[test]
fn a_download_of_unknown_size_stops_once_it_passes_the_free_space() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 4096];
    // without a length, the body ends when the server hangs up
    let server = Server::start(
        PATH,
        vec![reply("200 OK", &[], &body), reply("200 OK", &[], &body)],
    )?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_TEST_FREE", "1000")
        .output()?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{error}");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with("downloading iris.csv needs at least ")
            && message.ends_with(&format!(
                " on the disk holding the cache at {}, which has 1000 B free",
                sandbox.cache().display()
            )),
        "{message}"
    );
    assert!(!sandbox.work().join("iris").exists());
    let kept: u64 = partial_files(&sandbox.cache())
        .iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "part")
        })
        .filter_map(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum();
    assert!(kept <= 1000, "{kept} bytes went to a disk with 1000 B free");
    let output = sandbox
        .command(&["get", "--yes", &server.url])
        .env("FETCHLOOM_TEST_FREE", "1000")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        body
    );
    Ok(())
}

/// a terminal's question names the cache folder as the error without a terminal does.
#[cfg(target_os = "linux")]
#[test]
fn the_question_about_room_names_the_cache_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(&[b'x'; 2048], "\"v1\"")])?;
    let mut command = sandbox.command(&["get", &server.url]);
    command.env("FETCHLOOM_TEST_FREE", "1000");
    let Some((exit, screen)) =
        crate::support::typed(&sandbox.work(), &command, "", "go on anyway?", "n\r")?
    else {
        return Ok(());
    };
    assert_eq!(exit, Some(1), "{screen}");
    let asked = format!(
        "downloading iris.csv needs at least 2.0 KiB on the disk holding the cache at {}, which has 1000 B free. go on anyway? [y/N]",
        sandbox.cache().display()
    );
    assert!(screen.contains(&asked), "{screen}");
    Ok(())
}

/// FAT32 holds no file of 4 GiB, the size only the server states; exFAT does, so that run goes on
/// to ask about the size.
#[test]
fn a_download_fat32_cannot_hold_is_refused_before_any_byte_moves() -> Result {
    let sandbox = Sandbox::new()?;
    let stated = [("Content-Length", "4294967296")];
    let server = Server::start(
        PATH,
        vec![reply("200 OK", &stated, b""), reply("200 OK", &stated, b"")],
    )?;
    let mut kinds = Vec::new();
    for filesystem in ["fat32", "exfat"] {
        let output = sandbox
            .command(&["--json", "get", &server.url])
            .env("FETCHLOOM_TEST_FILESYSTEM", filesystem)
            .output()?;
        let document = json(&output)?;
        kinds.push(document["errors"][0]["kind"].clone());
        assert_eq!(document["work"]["bytes_down"], 0, "{filesystem}");
        if filesystem == "fat32" {
            assert_eq!(output.status.code(), Some(50));
            assert_eq!(
                document["errors"][0]["message"],
                format!(
                    "the cache at {} is on a FAT32 disk, which holds no file of 4 GiB or more, and iris.csv is 4.0 GiB",
                    sandbox.cache().display()
                )
            );
        }
    }
    assert_eq!(kinds, ["local.io", "unconfirmed"]);
    Ok(())
}

/// get marks its work finishing once its files are placed or it begins the `-o` file, so a ctrl-c
/// while the disk writes a large copy lets it end with exit 0 rather than as stopped.
#[cfg(unix)]
#[test]
fn a_ctrl_c_past_gets_mark_lets_it_finish() -> Result {
    for (args, placed) in [
        (vec!["get"], "iris/iris.csv"),
        (vec!["get", "-o", "data.csv"], "data.csv"),
    ] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
        let mut words = args.clone();
        words.insert(1, &server.url);
        let (running, release) =
            crate::support::held_at_its_mark(&sandbox, &mut sandbox.command(&words))?;
        let stderr = crate::support::finished_anyway(running, &release)?;
        assert_eq!(
            fs::read(sandbox.work().join(placed))?,
            IRIS,
            "{args:?}: {stderr}"
        );
    }
    Ok(())
}
