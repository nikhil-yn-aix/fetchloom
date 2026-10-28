//! links that carry secrets: never written into data.toml or data.lock, never printed.

use std::fs;

use crate::support::{Result, Sandbox, Server, reply, text};

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
