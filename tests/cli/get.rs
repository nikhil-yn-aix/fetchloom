//! `fetchloom get`: one link into one folder, through the cache.

use std::fs;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::support::{Part, Result, Sandbox, Server, file, json, link_folder, reply, text};

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
    let name = sandbox.placed_story("a_link_lands_in_a_folder_named_after_its_file")?;
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!(name, stderr));
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
    // the best the sandbox's drive makes: a reflink, a hardlink, or else a copy
    let links = sandbox.links()?;
    let (expected, counter) = if links.reflink {
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
    let name = sandbox.placed_story("a_busy_server_is_tried_again")?;
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!(name, stderr));
    Ok(())
}

/// zenodo's answer once its limit is used up: a 429 that says when the limit resets.
fn rate_limited(date: &str, reset: u64, retry_after: u64) -> Vec<Part> {
    reply(
        "429 Too Many Requests",
        &[
            ("Content-Length", "0"),
            ("Date", date),
            ("X-RateLimit-Limit", "133"),
            ("X-RateLimit-Remaining", "0"),
            ("X-RateLimit-Reset", &reset.to_string()),
            ("Retry-After", &retry_after.to_string()),
        ],
        b"",
    )
}

#[test]
fn a_rate_limit_is_waited_out_without_spending_tries() -> Result {
    let sandbox = Sandbox::new()?;
    let now = SystemTime::now();
    let date = httpdate::fmt_http_date(now);
    let second = now.duration_since(UNIX_EPOCH)?.as_secs();
    // more refusals than a request has tries, all but the first with a limit that resets at once
    let mut responses = vec![rate_limited(&date, second + 1, 1)];
    responses.extend((0..5).map(|_| rate_limited(&date, second, 0)));
    responses.push(file(IRIS, "\"v1\""));
    let server = Server::start(PATH, responses)?;
    let started = Instant::now();
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(server.requests().len(), 7);
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "the first refusal asked for a second"
    );
    let name = sandbox.placed_story("a_rate_limit_is_waited_out_without_spending_tries")?;
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!(name, stderr));
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
    let name = sandbox.placed_story("a_changed_file_replaces_the_copy_fetched_before")?;
    sandbox
        .filters()
        .bind(|| insta::assert_snapshot!(name, stderr));
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

#[test]
fn a_large_unknown_size_needs_approval_before_reading_the_body() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/large.bin",
        vec![reply("200 OK", &[("Content-Length", "1073741825")], b"")],
    )?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "unconfirmed");
    assert_eq!(document["work"]["bytes_down"], 0);
    assert_eq!(document["work"]["objects_written"], 0);
    assert!(!sandbox.work().join("large").exists());
    Ok(())
}

/// the folder you name for `get` may be a link to a folder elsewhere, as when a drive is mounted
/// into a home folder.
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
        let output = sandbox.run(&["get", &server.url, "-o", "linked"])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{run} run: {stderr}");
        assert_eq!(fs::read(real.join("iris.csv"))?, bytes, "{run} run");
    }
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
        assert!(stderr.contains("linked from the cache"), "{stderr}");
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
        ("/download", false),
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
            "{} came back as a web page, not a data file. the link may open a page instead of downloading; look for a download or raw link",
            &path[1..]
        );
        assert_eq!(stderr.contains(&message), warns, "{stderr}");
    }
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
