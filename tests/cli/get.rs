//! `fetchloom get`: one link into one folder, through the cache.

use std::fs;
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::support::{Part, Result, Sandbox, Server, file, reply, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";
const PATH: &str = "/records/3608990/files/iris.csv";

fn json(output: &Output) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
}

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
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
    let method = std::mem::replace(
        &mut document["result"]["files"][0]["method"],
        "[link]".into(),
    );
    assert!(method == "reflink" || method == "hardlink", "{method}");
    let work = &mut document["work"];
    let links = work["reflinks"].as_u64().unwrap_or_default()
        + work["hardlinks"].as_u64().unwrap_or_default();
    assert_eq!(links, 1);
    for key in ["reflinks", "hardlinks", "elapsed_ms"] {
        work[key] = "[varies]".into();
    }
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
fn a_dropped_connection_resumes_where_it_stopped() -> Result {
    let body: Vec<u8> = (0..100_000u32).flat_map(u32::to_le_bytes).collect();
    let cut = 150_000;
    let length = body.len().to_string();
    let rest = (body.len() - cut).to_string();
    let range = format!("bytes {cut}-{}/{}", body.len() - 1, body.len());
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
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let work = &json(&output)?["work"];
    assert_eq!(work["requests"], 2);
    assert_eq!(work["retries"], 1);
    assert_eq!(work["resumes"], 1);
    assert_eq!(
        work["bytes_down"],
        body.len(),
        "no byte is downloaded twice"
    );
    assert_eq!(fs::read(sandbox.work().join("big").join("big.bin"))?, body);
    let resumed = &server.requests()[1];
    assert!(
        resumed.contains(&format!("range: bytes={cut}-")),
        "{resumed}"
    );
    assert!(resumed.contains("if-range: \"v1\""), "{resumed}");
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

/// waits until an unfinished download in the cache holds `size` bytes, or the run ends, and says
/// how many it holds.
fn part_reaches(sandbox: &Sandbox, size: u64, run: &mut Child) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut on_disk = 0;
    while on_disk < size && run.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        // the size a folder listing shows lags behind a file another process still writes on
        // windows, so each file is asked directly
        on_disk = fs::read_dir(sandbox.cache().join("partial"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
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
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
        .command(&["--json", "get", &server.url, "-o", "again"])
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
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
    let huge = reply("200 OK", &[("Content-Length", "2147483648")], b"");
    let server = Server::start("/huge.bin", vec![huge])?;
    let output = sandbox.run(&["get", &server.url])?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr)?;
    sandbox.filters().bind(|| insta::assert_snapshot!(stderr));
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
    slow.push(Part::Pause(Duration::from_millis(1500)));
    slow.push(Part::Bytes(IRIS.to_vec()));
    let unchanged = reply("304 Not Modified", &[("ETag", "\"v1\"")], b"");
    let server = Server::start(PATH, vec![slow, unchanged])?;
    let first = sandbox
        .command(&["get", &server.url, "-o", "first"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(300));
    let second = sandbox.run(&["get", &server.url, "-o", "second"])?;
    let first = first.wait_with_output()?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr)?);
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
