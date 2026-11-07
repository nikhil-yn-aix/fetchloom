//! links that carry secrets: never written into data.toml or data.lock, never printed.

use std::fs;

use crate::support::{Result, Routes, Sandbox, Server, file, json, link_folder, reply, text};

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

const DUMMY_KEYS: &[(&str, &str)] = &[
    ("HF_TOKEN", "hf_wave1_huggingface_dummy_secret"),
    ("ZENODO_TOKEN", "wave1_zenodo_dummy_secret"),
    ("FIGSHARE_TOKEN", "wave1_figshare_dummy_secret"),
    ("OSF_TOKEN", "wave1_osf_dummy_secret"),
    ("KAGGLE_API_TOKEN", "KGAT_wave1_kaggle_dummy_secret"),
    ("DATAVERSE_API_TOKEN", "wave1_dataverse_dummy_secret"),
];
const LINK_SECRET: &str = "wave1-link-dummy-secret";
const MODES: &[&[&str]] = &[&[], &["--json"], &["-vv"], &["--json", "-vv"]];

fn keyed_command(
    sandbox: &Sandbox,
    args: &[&str],
    trace: &std::path::Path,
) -> std::process::Command {
    let mut command = sandbox.command(args);
    for (name, value) in DUMMY_KEYS {
        command.env(name, value);
    }
    command.env("DATAVERSE_URL", "https://dataverse.example.invalid:8443");
    command.env("FETCHLOOM_TRACE", trace);
    command
}

fn scan_secrets(name: &str, bytes: &[u8], secrets: &[&str], failures: &mut Vec<String>) {
    for secret in secrets {
        if bytes
            .windows(secret.len())
            .any(|part| part == secret.as_bytes())
        {
            failures.push(format!("{name} contains {secret}"));
        }
    }
}

fn trace_bytes(path: &std::path::Path, download: bool) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    let document: serde_json::Value = serde_json::from_slice(&bytes)?;
    let events = document
        .as_array()
        .or_else(|| document.get("traceEvents")?.as_array())
        .ok_or("no trace events")?;
    if download {
        assert!(
            events.iter().any(|event| event["name"] == "download"),
            "download was not traced"
        );
    }
    Ok(bytes)
}

fn url_record_bytes(sandbox: &Sandbox) -> Result<Vec<(String, Vec<u8>)>> {
    let records = fs::read_dir(sandbox.cache().join("urls"))?
        .map(|entry| {
            let path = entry?.path();
            Ok((
                format!(
                    "cache/urls/{}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                fs::read(path)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    assert!(
        !records.is_empty(),
        "the URL-record check must not be vacuous"
    );
    Ok(records)
}

#[test]
fn wave1_configured_keys_stay_out_of_outputs_locks_state_and_url_records() -> Result {
    let secrets: Vec<_> = DUMMY_KEYS.iter().map(|(_, secret)| *secret).collect();
    let mut failures = Vec::new();
    for mode in MODES {
        let sandbox = Sandbox::new()?;
        let server = Routes::start(vec![(PATH.to_owned(), file(b"a,b\n", "\"v1\""))])?;
        let url = format!("{}{PATH}", server.url);
        fs::write(
            sandbox.work().join("data.toml"),
            format!("[datasets.data]\nref = \"{url}\"\n"),
        )?;
        for (name, args, download) in [
            ("sync", vec!["sync"], true),
            ("get", vec!["get", url.as_str(), "-o", "fetched"], true),
            ("auth", vec!["auth", "--offline"], false),
        ] {
            let trace = sandbox.folder("traces")?.join(format!("{name}.json"));
            let args: Vec<_> = mode.iter().copied().chain(args).collect();
            let output = keyed_command(&sandbox, &args, &trace).output()?;
            assert!(
                output.status.success(),
                "{args:?}: {}",
                text(&output.stderr)?
            );
            if mode.contains(&"--json") {
                assert!(json(&output)?["result"].is_object());
            }
            scan_secrets(
                &format!("{mode:?}/{name}/stdout"),
                &output.stdout,
                &secrets,
                &mut failures,
            );
            scan_secrets(
                &format!("{mode:?}/{name}/stderr"),
                &output.stderr,
                &secrets,
                &mut failures,
            );
            scan_secrets(
                &format!("{mode:?}/{name}/trace"),
                &trace_bytes(&trace, download)?,
                &secrets,
                &mut failures,
            );
        }
        for name in ["data.lock", ".fetchloom/state"] {
            scan_secrets(
                name,
                &fs::read(sandbox.work().join(name))?,
                &secrets,
                &mut failures,
            );
        }
        for (name, bytes) in url_record_bytes(&sandbox)? {
            scan_secrets(&name, &bytes, &secrets, &mut failures);
        }
        let asked = server.requests();
        assert!(!asked.is_empty());
        for request in asked {
            if secrets
                .iter()
                .any(|secret| request.contains(&secret.to_ascii_lowercase()))
            {
                failures.push("a configured key reached a plain-http request".to_owned());
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn secret_link_surfaces(suffix: &str, mode: &[&str]) -> Result<Vec<(String, Vec<u8>)>> {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    let link = format!("{}{suffix}", server.url);
    let trace = sandbox.folder("traces")?.join("get.json");
    let args: Vec<_> = mode.iter().copied().chain(["get", link.as_str()]).collect();
    let output = keyed_command(&sandbox, &args, &trace).output()?;
    assert!(
        output.status.success(),
        "{args:?}: {}",
        text(&output.stderr)?
    );
    if mode.contains(&"--json") {
        assert!(json(&output)?["result"].is_object());
    }
    if mode.contains(&"-vv") {
        assert!(
            text(&output.stderr)?.contains("DEBUG"),
            "debug logging was not enabled"
        );
    }
    assert_eq!(server.requests().len(), 1);
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
        ("trace".to_owned(), trace_bytes(&trace, true)?),
    ];
    surfaces.extend(url_record_bytes(&sandbox)?);
    Ok(surfaces)
}

#[test]
fn wave1_query_secrets_stay_out_of_each_output_surface() -> Result {
    let mut failures = Vec::new();
    for key in [
        "token",
        "PRIVATE_TOKEN",
        "%74oken",
        "X-Amz-Signature",
        "X-Amz-Credential",
        "sig",
        "Key-Pair-Id",
    ] {
        for mode in MODES {
            for (name, bytes) in
                secret_link_surfaces(&format!("?{key}={LINK_SECRET}&download=1"), mode)?
            {
                if !name.starts_with("cache/") {
                    scan_secrets(
                        &format!("{key}/{mode:?}/{name}"),
                        &bytes,
                        &[LINK_SECRET],
                        &mut failures,
                    );
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn wave1_fragment_secrets_stay_out_of_each_output_surface() -> Result {
    let mut failures = Vec::new();
    for suffix in [
        format!("#access_token={LINK_SECRET}"),
        format!("?download=1#%74oken={LINK_SECRET}&page=2"),
        format!("#page=2&client_secret={LINK_SECRET}"),
    ] {
        for mode in MODES {
            for (name, bytes) in secret_link_surfaces(&suffix, mode)? {
                if !name.starts_with("cache/") {
                    scan_secrets(
                        &format!("{suffix}/{mode:?}/{name}"),
                        &bytes,
                        &[LINK_SECRET],
                        &mut failures,
                    );
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn wave1_signed_urls_leave_no_secret_in_cache_url_records() -> Result {
    let mut failures = Vec::new();
    for suffix in [
        format!("?token={LINK_SECRET}"),
        format!("#access_token={LINK_SECRET}"),
    ] {
        for (name, bytes) in secret_link_surfaces(&suffix, &["--json", "-vv"])? {
            if name.starts_with("cache/") {
                scan_secrets(&name, &bytes, &[LINK_SECRET], &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn wave1_project_references_refuse_secret_fragments_without_persisting_them() -> Result {
    let mut failures = Vec::new();
    for mode in MODES {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
        let link = format!("{}#access_token={LINK_SECRET}", server.url);
        let args: Vec<_> = mode
            .iter()
            .copied()
            .chain(["add", link.as_str(), "--dry-run"])
            .collect();
        let trace = sandbox.folder("traces")?.join("add.json");
        let output = keyed_command(&sandbox, &args, &trace).output()?;
        if output.status.code() != Some(10) {
            failures.push(format!(
                "{mode:?}: secret reference was not refused: {:?}",
                output.status.code()
            ));
        }
        for (name, bytes) in [
            ("stdout", output.stdout),
            ("stderr", output.stderr),
            ("trace", trace_bytes(&trace, false)?),
        ] {
            scan_secrets(&name, &bytes, &[LINK_SECRET], &mut failures);
        }
        assert!(!sandbox.work().join("data.lock").exists());
        assert!(!sandbox.work().join("data.toml").exists());
        assert!(server.requests().is_empty());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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

#[test]
fn wave1_fragment_keys_cannot_be_pinned_into_a_project() -> Result {
    let mut failures = Vec::new();
    for mode in MODES {
        let sandbox = Sandbox::new()?;
        let server = Routes::start(vec![(PATH.to_owned(), file(b"a,b\n", "\"v1\""))])?;
        let link = format!("{}{PATH}#access_token={LINK_SECRET}", server.url);
        let trace = sandbox.folder("traces")?.join("add.json");
        let args: Vec<_> = mode.iter().copied().chain(["add", link.as_str()]).collect();
        let output = keyed_command(&sandbox, &args, &trace).output()?;
        if output.status.code() != Some(10) {
            failures.push(format!(
                "{mode:?}: secret reference was accepted: {:?}",
                output.status.code()
            ));
        }
        if output.status.success() {
            let trace = sandbox.folder("traces")?.join("sync.json");
            let synced = keyed_command(&sandbox, &["--json", "sync"], &trace).output()?;
            assert!(synced.status.success(), "{}", text(&synced.stdout)?);
        }
        for name in ["data.toml", "data.lock", ".fetchloom/state"] {
            let path = sandbox.work().join(name);
            if path.exists() {
                scan_secrets(
                    &format!("{mode:?}/{name}"),
                    &fs::read(path)?,
                    &[LINK_SECRET],
                    &mut failures,
                );
            }
        }
        for (name, bytes) in [
            ("stdout", output.stdout),
            ("stderr", output.stderr),
            ("trace", trace_bytes(&trace, false)?),
        ] {
            scan_secrets(
                &format!("{mode:?}/{name}"),
                &bytes,
                &[LINK_SECRET],
                &mut failures,
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn wave1_userinfo_links_are_rejected_without_leaking_or_making_a_request() -> Result {
    let secrets = ["wave1-userinfo-name", "wave1-userinfo-password"];
    for verb in ["get", "add"] {
        for mode in MODES {
            let sandbox = Sandbox::new()?;
            let server = Server::start(PATH, vec![])?;
            let link = server.url.replacen(
                "http://",
                &format!("http://{}:{}@", secrets[0], secrets[1]),
                1,
            );
            let trace = sandbox.folder("traces")?.join("userinfo.json");
            let args: Vec<_> = mode.iter().copied().chain([verb, link.as_str()]).collect();
            let output = keyed_command(&sandbox, &args, &trace).output()?;
            assert_eq!(output.status.code(), Some(10));
            let mut failures = Vec::new();
            for (name, bytes) in [
                ("stdout", output.stdout),
                ("stderr", output.stderr),
                ("trace", trace_bytes(&trace, false)?),
            ] {
                scan_secrets(name, &bytes, &secrets, &mut failures);
            }
            assert!(failures.is_empty(), "{}", failures.join("\n"));
            assert!(server.requests().is_empty());
            assert!(!sandbox.work().join("data.toml").exists());
            assert!(!sandbox.work().join("data.lock").exists());
        }
    }
    Ok(())
}
