//! links that carry secrets: never written into data.toml or data.lock, never printed.

use std::fs;

use crate::support::{Result, Sandbox, Server, file, json, link_folder, reply, text};

const PATH: &str = "/files/data.csv";

#[test]
fn add_refuses_a_presigned_link_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![])?;
    let signed = format!("{}?X-Amz-Expires=600&X-Amz-Signature=abc123", server.url);
    let output = sandbox.run(&["add", &signed])?;
    assert_eq!(output.status.code(), Some(10));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("carries x-amz-signature"), "{stderr}");
    assert!(!sandbox.work().join("data.toml").exists());
    assert_eq!(server.requests().len(), 0, "no request is sent");
    Ok(())
}

#[test]
fn sync_refuses_a_token_written_into_data_toml() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![])?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.data]\nref = \"{}?token=abc123\"\n", server.url),
    )?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(10));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("carries token"), "{stderr}");
    assert!(stderr.contains("data.toml:2"), "{stderr}");
    assert!(!stderr.contains("abc123"), "{stderr}");
    let json = sandbox.run(&["--json", "sync"])?;
    assert!(!text(&json.stdout)?.contains("abc123"));
    assert!(!sandbox.work().join("data.lock").exists());
    Ok(())
}

#[test]
fn a_failing_link_never_shows_its_token() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![reply("404 Not Found", &[("Content-Length", "0")], b"")],
    )?;
    let output = sandbox.run(&["get", &format!("{}?token=abc123", server.url)])?;
    assert_eq!(output.status.code(), Some(10));
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains("abc123"), "{stderr}");
    assert!(stderr.contains("token=***"), "{stderr}");
    Ok(())
}

#[test]
fn a_dropped_connection_never_shows_the_token() -> Result {
    let sandbox = Sandbox::new()?;
    let dropped = Vec::new();
    let gone = reply("404 Not Found", &[("Content-Length", "0")], b"");
    let server = Server::start(PATH, vec![dropped, gone])?;
    let output = sandbox.run(&["get", &format!("{}?private_token=abc123", server.url)])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("trying again"),
        "the retry is told: {stderr}"
    );
    assert!(!stderr.contains("abc123"), "{stderr}");
    Ok(())
}

#[test]
fn a_dataverse_host_typed_for_add_may_be_on_a_private_network() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![])?;
    let host = server
        .url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .ok_or("no host")?
        .to_owned();
    let output = sandbox.run(&["--json", "add", &format!("dataverse:{host}/doi:10.1/X")])?;
    let document = json(&output)?;
    // the test server speaks plain http, so the request fails, but only after it was let through
    assert_ne!(
        document["errors"][0]["kind"], "network.refused",
        "{document}"
    );
    assert!(!text(&output.stderr)?.contains("private address"));
    Ok(())
}

#[test]
fn a_refused_redirect_never_shows_the_token() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![reply(
            "302 Found",
            &[
                ("Location", "http://10.0.0.1/data.csv?token=abc123"),
                ("Content-Length", "0"),
            ],
            b"",
        )],
    )?;
    let output = sandbox.run(&["get", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains("private address"), "{stderr}");
    assert!(!stderr.contains("abc123"), "{stderr}");
    Ok(())
}

#[test]
fn patterns_that_leave_nothing_never_show_the_token() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![])?;
    let link = format!("{}?token=abc123", server.url);
    let output = sandbox.run(&["get", &link, "--exclude", "*"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(!stderr.contains("abc123"), "{stderr}");
    Ok(())
}

#[test]
fn get_json_never_shows_the_token() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    let output = sandbox.run(&["--json", "get", &format!("{}?token=abc123", server.url)])?;
    let stdout = text(&output.stdout)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!stdout.contains("abc123"), "{stdout}");
    assert!(stdout.contains("token=***"), "{stdout}");
    Ok(())
}

#[test]
fn debug_logs_never_show_a_token_or_a_stored_key() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    let output = sandbox
        .command(&["-vv", "get", &format!("{}?token=abc123", server.url)])
        .env("HF_TOKEN", "hf_fakekey456")
        .env("ZENODO_TOKEN", "fakezenodokey789")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("DEBUG"), "debug logs are on: {stderr}");
    for secret in ["abc123", "hf_fakekey456", "fakezenodokey789"] {
        assert!(!stderr.contains(secret), "{secret} leaked: {stderr}");
    }
    Ok(())
}

#[test]
fn an_unknown_help_topic_never_shows_the_token() -> Result {
    let sandbox = Sandbox::new()?;
    let link = "https://example.org/file?token=abc123";
    for args in [vec!["help", link], vec!["--json", "help", link]] {
        let output = sandbox.run(&args)?;
        let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        assert_eq!(output.status.code(), Some(2), "{shown}");
        assert!(!shown.contains("abc123"), "{shown}");
        assert!(shown.contains("token=***"), "{shown}");
    }
    Ok(())
}

fn marker_zip() -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("marker.txt", zip::write::SimpleFileOptions::default())?;
    zip.write_all(b"harmless marker")?;
    Ok(zip.finish()?.into_inner())
}

#[test]
fn wave1_generated_output_roots_refuse_links_outside_the_working_directory() -> Result {
    for archive in [false, true] {
        for multiple in [false, true] {
            let sandbox = Sandbox::new()?;
            let outside = sandbox.folder("outside")?;
            let root = if multiple {
                sandbox.work().join("chosen")
            } else {
                sandbox.work()
            };
            fs::create_dir_all(&root)?;
            if !link_folder(&outside, &root.join("payload")) {
                return Ok(());
            }
            let body = if archive {
                marker_zip()?
            } else {
                b"harmless marker".to_vec()
            };
            let path = if archive {
                "/payload.zip"
            } else {
                "/payload.csv"
            };
            let server = Server::start(path, vec![file(&body, "\"v1\"")])?;
            let other = Server::start("/other.csv", vec![file(b"other", "\"v2\"")])?;
            let args = if multiple {
                vec![
                    "--json",
                    "get",
                    server.url.as_str(),
                    other.url.as_str(),
                    "-o",
                    "chosen",
                ]
            } else {
                vec!["--json", "get", server.url.as_str()]
            };
            let output = sandbox.run(&args)?;
            assert!(
                !outside.join("marker.txt").exists() && !outside.join("payload.csv").exists(),
                "generated root escaped: archive={archive}, multiple={multiple}, output={}",
                text(&output.stdout)?
            );
            assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        }
    }
    Ok(())
}
