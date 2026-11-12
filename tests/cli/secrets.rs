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
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    let sandbox = Sandbox::new()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (output, hello) = std::thread::scope(|scope| -> Result<_> {
        let attempt = scope.spawn(move || -> std::io::Result<_> {
            let (mut client, _) = listener.accept()?;
            client.set_read_timeout(Some(Duration::from_secs(30)))?;
            let mut first = [0];
            let read = client.read(&mut first)?;
            Ok((read == 1).then_some(first[0]))
        });
        let output = sandbox
            .command(&["--json", "add", &format!("dataverse:{address}/doi:10.1/X")])
            .env("FETCHLOOM_RETRIES", "0")
            .output();
        // a refusal before connecting must still release the observer, with no TLS hello.
        let mut wake = TcpStream::connect(address).ok();
        if let Some(wake) = &mut wake {
            let _ = wake.write_all(&[0]);
        }
        let hello = attempt
            .join()
            .map_err(|_| "admission observer panicked")??;
        Ok((output?, hello))
    })?;
    assert_eq!(
        hello,
        Some(0x16),
        "the typed private host received no TLS handshake"
    );
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "network.unreachable");
    assert_eq!(document["work"]["requests"], 1);
    assert_eq!(document["work"]["retries"], 0);
    Ok(())
}

/// a cloned project's data.toml names a server on this machine, which neither a sync nor the
/// build of a recipe that reads it reaches until `allowed_origins` lists it. neither takes a link
/// on its command line, so the help names that setting alone.
#[test]
fn a_host_in_data_toml_reaches_this_machine_only_as_an_allowed_origin() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    let origin = server.url.trim_end_matches(PATH).to_owned();
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.local]\nref = \"{}\"\n\n[datasets.made]\nfrom = [\"local\"]\nrun = [\"recipe-step\", \"copy\", \"{{local}}\", \"{{out}}\"]\n",
            server.url
        ),
    )?;
    let refused = |command: &str, allowed: &str, cache: &std::path::Path| -> Result<_> {
        let output = sandbox
            .command(&["--json", command])
            .env("FETCHLOOM_ALLOWED_ORIGINS", allowed)
            .env("FETCHLOOM_CACHE", cache)
            .output()?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(20), "{allowed}: {document}");
        assert_eq!(document["errors"][0]["kind"], "network.refused");
        Ok(document["errors"][0].clone())
    };
    let untrusted = format!(
        "if you trust that server, put `allowed_origins = [\"{origin}\"]` in {}, or set `FETCHLOOM_ALLOWED_ORIGINS={origin}` for one run",
        settings_file(&sandbox)?
    );
    assert_eq!(refused("sync", "", &sandbox.cache())?["help"], untrusted);
    // another port of the host is another origin, which the refusal tells apart
    let error = refused("sync", "http://127.0.0.1:9", &sandbox.cache())?;
    assert_eq!(
        error["details"][0],
        trusted_alone("http://127.0.0.1:9", "127.0.0.1")
    );
    assert_eq!(
        error["help"],
        format!("to reach it too, set `FETCHLOOM_ALLOWED_ORIGINS=http://127.0.0.1:9,{origin}`")
    );
    assert_eq!(server.requests(), Vec::<String>::new());
    let output = sandbox
        .command(&["--json", "sync"])
        .env("FETCHLOOM_ALLOWED_ORIGINS", &origin)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let placed = sandbox.work().join("data").join("local").join("data.csv");
    assert_eq!(fs::read(&placed)?, b"a,b\n");
    // as in a fresh clone, the build must download what the recipe reads
    crate::support::delete(&placed)?;
    assert_eq!(
        refused("build", "", &sandbox.folder("empty")?)?["help"],
        untrusted
    );
    assert_eq!(server.requests().len(), 1, "only the allowed sync asked");
    Ok(())
}

/// the settings file fetchloom reads in `sandbox`, as `fetchloom settings` names it.
fn settings_file(sandbox: &Sandbox) -> Result<String> {
    let document = json(&sandbox.run(&["--json", "settings"])?)?;
    let path = document["result"]["file"]["path"]
        .as_str()
        .ok_or("settings names no file")?;
    Ok(path.to_owned())
}

/// the line a refusal adds when `trusted` is another origin on `host`, the refused link's host.
fn trusted_alone(trusted: &str, host: &str) -> String {
    format!("only {trusted} is trusted on {host}, and trust covers that origin alone")
}

/// another port of a host that `allowed_origins` lists is another origin, which nobody trusted: it
/// may reach public addresses only, so this machine's is refused once the name is looked up.
#[test]
fn another_port_of_an_allowed_host_reaches_no_private_address() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    let link = server.url.replace("127.0.0.1", "localhost");
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.local]\nref = \"{link}\"\n"),
    )?;
    let output = sandbox
        .command(&["--json", "sync"])
        .env("FETCHLOOM_ALLOWED_ORIGINS", "http://localhost:9")
        .output()?;
    assert_eq!(server.requests(), Vec::<String>::new());
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "network.refused");
    assert_eq!(
        error["message"],
        format!("fetchloom will not follow {link}: localhost resolves only to private addresses")
    );
    assert_eq!(
        error["details"][0],
        trusted_alone("http://localhost:9", "localhost")
    );
    assert_eq!(
        error["help"],
        format!(
            "to reach it too, set `FETCHLOOM_ALLOWED_ORIGINS=http://localhost:9,{}`",
            link.trim_end_matches(PATH)
        )
    );
    Ok(())
}

/// a server on this machine that serves `body` at [`PATH`], pinned by a link typed for `add` in
/// the sandbox's work folder, which `allowed_origins` does not list.
fn pinned(sandbox: &Sandbox, body: &[u8]) -> Result<Routes> {
    let source = Routes::start(vec![(PATH.to_owned(), file(body, "\"v1\""))])?;
    let added = sandbox.run(&["add", &format!("{}{PATH}", source.url)])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    Ok(source)
}

/// a mirror a person names is trusted at its own origin alone. in a clone, a mirror on another
/// port of the same host lets data.lock reach no server on this machine that `allowed_origins`
/// does not list, and the refusal names the mirror's origin.
#[test]
fn a_mirror_on_this_machine_trusts_its_own_origin_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let source = pinned(&sandbox, b"a,b\n")?;
    let asked = source.requests().len();
    let mirror = Routes::start(Vec::new())?;
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let base = format!("{}/m", mirror.url);
    let output = sandbox
        .command_in(&clone, &["--json", "sync", "--locked", "--mirror", &base])
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(
        source.requests().len(),
        asked,
        "the clone asked the source: {}",
        text(&output.stdout)?
    );
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    let link = format!("{}{PATH}", source.url);
    assert_eq!(error["kind"], "network.refused");
    assert_eq!(
        error["message"],
        format!("fetchloom will not follow {link}: it points at the private address 127.0.0.1")
    );
    assert_eq!(error["details"][0], trusted_alone(&mirror.url, "127.0.0.1"));
    assert_eq!(
        error["help"],
        format!(
            "to reach it too, put `allowed_origins = [\"{}\"]` in {}, or set `FETCHLOOM_ALLOWED_ORIGINS={}` for one run",
            source.url,
            settings_file(&sandbox)?,
            source.url
        )
    );
    Ok(())
}

/// a link typed in a clone is trusted at its own origin alone: the server data.lock pins on
/// another port of the same host stays out of reach.
#[test]
fn a_link_typed_in_a_clone_trusts_its_own_origin_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let source = pinned(&sandbox, b"a,b\n")?;
    let asked = source.requests().len();
    let other = Routes::start(vec![("/hello.txt".to_owned(), file(b"hello\n", "\"h1\""))])?;
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let output = sandbox
        .command_in(
            &clone,
            &["--json", "add", &format!("{}/hello.txt", other.url)],
        )
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(
        source.requests().len(),
        asked,
        "the clone asked the source: {}",
        text(&output.stdout)?
    );
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "network.refused", "{error}");
    assert_eq!(error["details"][0], trusted_alone(&other.url, "127.0.0.1"));
    Ok(())
}

/// a link data.lock gives that carries a secret, as a hand edit or a mistaken commit leaves one,
/// stops each command that reads data.lock before it asks any server, and none shows the secret.
#[test]
fn a_secret_in_a_link_data_lock_gives_stops_every_command_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    let source = pinned(&sandbox, b"a,b\n")?;
    sandbox.allow(&source.url);
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let lock = clone.join("data.lock");
    let link = format!("{}{PATH}", source.url);
    let written = fs::read_to_string(&lock)?;
    let edited = written.replace(
        &format!("url = \"{link}\""),
        &format!("url = \"{link}?token={LINK_SECRET}\""),
    );
    assert_ne!(edited, written);
    fs::write(&lock, edited)?;
    let asked = source.requests().len();
    let refused =
        "the link data.lock gives data/data.csv carries token, a secret fetchloom never pins";
    let mut failures = Vec::new();
    for args in [
        &["status"][..],
        &["lock", "--check"],
        &["sync"],
        &["-vv", "sync"],
        &["--json", "sync"],
        &["export", "--format", "links"],
    ] {
        let output = sandbox
            .command_in(&clone, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let shown = if args.contains(&"--json") {
            let error = json(&output)?["errors"][0].clone();
            if error["kind"] != "project.invalid" || !error["fix"].is_null() {
                failures.push(format!("{args:?}: {error}"));
            }
            error["message"].as_str().unwrap_or_default().to_owned()
        } else {
            text(&output.stderr)?
        };
        if output.status.code() != Some(50) || !shown.contains(refused) {
            failures.push(format!(
                "{args:?} exited {:?}: {shown}",
                output.status.code()
            ));
        }
        for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            scan_secrets(
                &format!("{args:?}/{stream}"),
                bytes,
                &[LINK_SECRET],
                &mut failures,
            );
        }
    }
    let sent = source.requests().len() - asked;
    if sent != 0 {
        failures.push(format!("{sent} requests went to the server"));
    }
    if clone.join("data").exists() {
        failures.push("a sync placed files".to_owned());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// data.toml alone names a server on this machine that `allowed_origins` does not list. a link to
/// another port of the same host, typed for `add`, leaves that server out of reach, and the add
/// says why.
#[test]
fn a_typed_link_reaches_no_other_port_that_data_toml_names() -> Result {
    let sandbox = Sandbox::new()?;
    let named = Routes::start(vec![(PATH.to_owned(), file(b"internal\n", "\"n1\""))])?;
    let link = format!("{}{PATH}", named.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.internal]\nref = \"{link}\"\n"),
    )?;
    let typed = Routes::start(vec![("/y.csv".to_owned(), file(b"y\n1\n", "\"y1\""))])?;
    let output = sandbox.run(&["--json", "add", &format!("{}/y.csv", typed.url)])?;
    assert_eq!(
        named.requests(),
        Vec::<String>::new(),
        "{}",
        text(&output.stdout)?
    );
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "network.refused", "{error}");
    assert_eq!(
        error["message"],
        format!("fetchloom will not follow {link}: it points at the private address 127.0.0.1")
    );
    assert_eq!(error["details"][0], trusted_alone(&typed.url, "127.0.0.1"));
    assert_eq!(
        error["help"],
        format!(
            "to reach it too, put `allowed_origins = [\"{}\"]` in {}, or set `FETCHLOOM_ALLOWED_ORIGINS={}` for one run",
            named.url,
            settings_file(&sandbox)?,
            named.url
        )
    );
    Ok(())
}

#[test]
fn a_host_in_a_list_of_links_reaches_this_machine_only_as_an_allowed_origin() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    fs::write(
        sandbox.work().join("links.txt"),
        format!("{}\n", server.url),
    )?;
    let output = sandbox.run(&["--json", "get", "-i", "links.txt"])?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(20), "{document}");
    assert_eq!(document["errors"][0]["kind"], "network.refused");
    assert_eq!(server.requests(), Vec::<String>::new());
    sandbox.allow(&server.url);
    let output = sandbox.run(&["--json", "get", "-i", "links.txt"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
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
    ("HF_TOKEN", "hf_huggingface_dummy_secret"),
    ("ZENODO_TOKEN", "zenodo_dummy_secret"),
    ("FIGSHARE_TOKEN", "figshare_dummy_secret"),
    ("OSF_TOKEN", "osf_dummy_secret"),
    ("KAGGLE_API_TOKEN", "KGAT_kaggle_dummy_secret"),
    ("DATAVERSE_API_TOKEN", "dataverse_dummy_secret"),
];
const LINK_SECRET: &str = "link-dummy-secret";
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
fn configured_keys_stay_out_of_outputs_locks_state_and_url_records() -> Result {
    let secrets: Vec<_> = DUMMY_KEYS.iter().map(|(_, secret)| *secret).collect();
    let mut failures = Vec::new();
    for mode in MODES {
        let sandbox = Sandbox::new()?;
        let server = Routes::start(vec![(PATH.to_owned(), file(b"a,b\n", "\"v1\""))])?;
        sandbox.allow(&server.url);
        let url = format!("{}{PATH}", server.url);
        fs::write(
            sandbox.work().join("data.toml"),
            format!("[datasets.data]\nref = \"{url}\"\n"),
        )?;
        for (name, args, download) in [
            ("sync", vec!["sync"], true),
            ("get", vec!["get", url.as_str(), "--dir", "fetched"], true),
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
        assert_ne!(asked, Vec::<String>::new());
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
fn query_secrets_stay_out_of_each_output_surface() -> Result {
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
fn fragment_secrets_stay_out_of_each_output_surface() -> Result {
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
fn signed_urls_leave_no_secret_in_cache_url_records() -> Result {
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
fn project_references_refuse_secret_fragments_without_persisting_them() -> Result {
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
            scan_secrets(name, &bytes, &[LINK_SECRET], &mut failures);
        }
        assert!(!sandbox.work().join("data.lock").exists());
        assert!(!sandbox.work().join("data.toml").exists());
        assert_eq!(server.requests(), Vec::<String>::new());
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
fn generated_output_roots_refuse_links_outside_the_working_directory() -> Result {
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
                    "--dir",
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
fn fragment_keys_cannot_be_pinned_into_a_project() -> Result {
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
fn userinfo_links_are_rejected_without_leaking_or_making_a_request() -> Result {
    let secrets = ["userinfo-name", "userinfo-password"];
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
            assert_eq!(server.requests(), Vec::<String>::new());
            assert!(!sandbox.work().join("data.toml").exists());
            assert!(!sandbox.work().join("data.lock").exists());
        }
    }
    Ok(())
}

/// `FETCHLOOM_OFFLINE` takes `true` as `FETCHLOOM_MIRRORS_ONLY` does, and a value it cannot read
/// stops a command before any request, where it used to go online without a word. help and
/// version still answer.
#[test]
fn the_offline_variable_never_lets_a_run_go_online_by_mistake() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(b"a,b\n", "\"v1\"")])?;
    for (value, code, kind) in [
        ("true", 20, "offline.missing"),
        ("yes", 2, "usage"),
        ("2", 2, "usage"),
    ] {
        let output = sandbox
            .command(&["--json", "get", &server.url])
            .env("FETCHLOOM_OFFLINE", value)
            .output()?;
        assert_eq!(output.status.code(), Some(code), "{value}");
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], kind, "{value}");
        assert_eq!(document["work"]["requests"], 0, "{value}");
    }
    assert_eq!(server.requests(), Vec::<String>::new());
    for asked in [&["--version"][..], &["help"]] {
        let output = sandbox
            .command(asked)
            .env("FETCHLOOM_OFFLINE", "yes")
            .output()?;
        assert_eq!(output.status.code(), Some(0), "{asked:?}");
    }
    Ok(())
}

/// a link's query or fragment may hold a secret under a name no list knows, such as the `rlkey`
/// of a dropbox link, and a notebook keeps the error it printed. help that says to run the
/// command again never repeats a line holding one, in text or in json, and says to run the same
/// command instead.
#[test]
fn a_retry_never_repeats_a_link_with_a_query_or_a_fragment() -> Result {
    const SENTINEL: &str = "s3ntinel";
    for suffix in ["?rlkey=s3ntinel&dl=1", "#s3ntinel"] {
        for mode in [&[][..], &["--json"]] {
            let sandbox = Sandbox::new()?;
            // nothing listens on the port once its listener is gone, so the request cannot
            // connect, which a retry could get past
            let port = std::net::TcpListener::bind("127.0.0.1:0")?
                .local_addr()?
                .port();
            let link = format!("http://127.0.0.1:{port}{PATH}{suffix}");
            let args: Vec<&str> = mode
                .iter()
                .copied()
                .chain(["get", "--output=x.csv", "--", &link])
                .collect();
            let output = sandbox
                .command(&args)
                .env("FETCHLOOM_RETRIES", "0")
                .output()?;
            let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
            assert_eq!(output.status.code(), Some(20), "{shown}");
            assert!(!shown.contains(SENTINEL), "{args:?}: {shown}");
            assert!(shown.contains("run the same command again"), "{shown}");
        }
    }
    Ok(())
}

/// in mirror-only mode a web mirror that sends fetchloom on to another origin is refused there,
/// and the refusal names that origin with its port and the mirror's that sent it, since both may
/// be on one host, where it named the host alone.
#[test]
fn a_mirror_that_sends_fetchloom_elsewhere_names_both_origins() -> Result {
    let sandbox = Sandbox::new()?;
    let body = b"a,b\n";
    let source = Routes::start(vec![(PATH.to_owned(), file(body, "\"v1\""))])?;
    let link = format!("{}{PATH}", source.url);
    sandbox.allow(&source.url);
    let added = sandbox.run(&["add", &link])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let hex = crate::add::sha256_hex(body);
    let mirror = Routes::start(vec![
        (
            "/m/fetchloom-mirror".to_owned(),
            file(b"fetchloom mirror 1\n", "\"m\""),
        ),
        (
            format!("/m/objects/{}/{}", &hex[..2], &hex[2..]),
            reply(
                "302 Found",
                &[("Location", link.as_str()), ("Content-Length", "0")],
                b"",
            ),
        ),
    ])?;
    let (clone, cache) = crate::mirrors::cold(&sandbox, "clone")?;
    let asked = source.requests().len();
    let output = sandbox
        .command_in(&clone, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", &cache)
        .env("FETCHLOOM_MIRRORS", format!("{}/m", mirror.url))
        .env("FETCHLOOM_MIRRORS_ONLY", "1")
        .output()?;
    let document = text(&output.stdout)?;
    assert_eq!(output.status.code(), Some(20), "{document}");
    assert_eq!(source.requests().len(), asked, "the source is never asked");
    let said = format!(
        "fetchloom will not ask {}, where {} sent it, since mirrors_only lets it ask its mirrors alone",
        source.url, mirror.url
    );
    assert!(document.contains(&said), "{document}");
    Ok(())
}
