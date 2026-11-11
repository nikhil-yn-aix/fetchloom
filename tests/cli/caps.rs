//! resource caps and the other settings, read from the environment and the settings file.

use std::path::PathBuf;

use crate::support::{Part, Result, Sandbox, Server, file, json, reply, text};

/// where fetchloom looks for its settings file in `sandbox`, which gives it `home` as the home
/// and application data folder.
fn settings_file(sandbox: &Sandbox) -> Result<PathBuf> {
    let home = sandbox.folder("home")?;
    let folder = if cfg!(windows) {
        home.join("fetchloom")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/fetchloom")
    } else {
        home.join(".config/fetchloom")
    };
    std::fs::create_dir_all(&folder)?;
    Ok(folder.join("settings.toml"))
}

/// the entry `fetchloom settings --json` reports for `name`.
pub(crate) fn shown<'a>(document: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    document["result"]["settings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|setting| setting["name"] == name)
        .unwrap_or(&serde_json::Value::Null)
}

#[test]
fn settings_show_each_value_and_where_it_came_from() -> Result {
    let sandbox = Sandbox::new()?;
    let path = settings_file(&sandbox)?;
    std::fs::write(
        &path,
        "host_jobs = 6\ntimeout = 90\ncache = \"shared-cache\"\n",
    )?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env_remove("FETCHLOOM_CACHE")
            .env("FETCHLOOM_JOBS", "12")
            .output()?,
    )?;
    assert_eq!(document["status"], "ok");
    let cache = path.with_file_name("shared-cache");
    for (name, value, origin) in [
        ("jobs", serde_json::json!(12), "environment"),
        ("host_jobs", serde_json::json!(6), "file"),
        ("timeout", serde_json::json!(90), "file"),
        ("retries", serde_json::json!(4), "default"),
        ("ask_above", serde_json::json!(1_u64 << 30), "default"),
        (
            "cache",
            serde_json::json!(cache.display().to_string()),
            "file",
        ),
    ] {
        let setting = shown(&document, name);
        assert_eq!(setting["value"], value, "{name}");
        assert_eq!(setting["origin"], origin, "{name}");
    }
    assert_eq!(
        shown(&document, "host_jobs")["variable"],
        "FETCHLOOM_HOST_JOBS"
    );
    assert_eq!(
        document["result"]["file"]["path"],
        path.display().to_string()
    );
    assert_eq!(document["result"]["file"]["found"], true);
    // the cache the file names is the one every command uses
    let info = json(
        &sandbox
            .command(&["--json", "cache"])
            .env_remove("FETCHLOOM_CACHE")
            .output()?,
    )?;
    assert_eq!(info["result"]["path"], cache.display().to_string());
    let table = sandbox.run(&["settings"])?;
    assert!(table.status.success());
    let stdout = text(&table.stdout)?;
    let row = stdout
        .lines()
        .find(|line| line.starts_with("host_jobs"))
        .ok_or("no host_jobs row")?;
    assert!(
        row.contains(" 6 ")
            && row.contains("settings file")
            && row.ends_with("FETCHLOOM_HOST_JOBS"),
        "{row}"
    );
    Ok(())
}

#[test]
fn a_bad_settings_file_stops_commands_but_not_help_or_version() -> Result {
    let sandbox = Sandbox::new()?;
    let path = settings_file(&sandbox)?;
    std::fs::write(&path, "job = 3\n")?;
    let output = sandbox.run(&["--json", "cache"])?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "the settings file {} has \"job\", which is not a setting",
            path.display()
        )
    );
    assert_eq!(
        document["errors"][0]["help"],
        "the settings file takes cache, jobs, host_jobs, retries, timeout, ask_above, memory, most_files, archive_files, archive_size, archive_ratio, allowed_origins, s3_buckets, mirrors, mirrors_only, push_to, build_floor, each a number or text"
    );
    for arguments in [
        &["--version"][..],
        &["-v", "--version"],
        &["help"],
        &["--help"],
        &["completions", "bash"],
    ] {
        let output = sandbox.run(arguments)?;
        assert!(output.status.success(), "{arguments:?}");
    }
    std::fs::write(&path, "jobs = 0\n")?;
    let document = json(&sandbox.run(&["--json", "cache"])?)?;
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "jobs in the settings file {} is \"0\", which is not a number from 1 to 256",
            path.display()
        )
    );
    // the variable wins over the file, so it also works around a bad value there
    let output = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_JOBS", "2")
        .output()?;
    assert!(output.status.success());
    Ok(())
}

#[test]
fn a_settings_file_named_by_the_variable_must_exist() -> Result {
    let sandbox = Sandbox::new()?;
    let missing = sandbox.work().join("nowhere.toml");
    let output = sandbox
        .command(&["--json", "cache"])
        .env("FETCHLOOM_SETTINGS", &missing)
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        format!(
            "FETCHLOOM_SETTINGS names {}, which does not exist",
            missing.display()
        )
    );
    let named = sandbox.work().join("fast.toml");
    std::fs::write(&named, "jobs = 32\nhost_jobs = 16\n")?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SETTINGS", &named)
            .output()?,
    )?;
    assert_eq!(shown(&document, "jobs")["value"], 32);
    assert_eq!(shown(&document, "host_jobs")["origin"], "file");
    Ok(())
}

#[test]
fn bad_settings_in_the_environment_are_usage_errors() -> Result {
    let sandbox = Sandbox::new()?;
    for (name, value, said, help) in [
        (
            "FETCHLOOM_HOST_JOBS",
            "0",
            "FETCHLOOM_HOST_JOBS is \"0\", which is not a number from 1 to 256",
            None,
        ),
        (
            "FETCHLOOM_TIMEOUT",
            "0",
            "FETCHLOOM_TIMEOUT is \"0\", which is not a number from 1 to 3600",
            None,
        ),
        (
            "FETCHLOOM_ASK_ABOVE",
            "big",
            "FETCHLOOM_ASK_ABOVE is \"big\", which is not a size such as 512M or 4G",
            None,
        ),
        (
            "FETCHLOOM_MEMORY",
            "4gb",
            "FETCHLOOM_MEMORY is \"4gb\", which is not a size such as 512M or 4G",
            None,
        ),
        (
            "FETCHLOOM_RATE",
            "fast",
            "FETCHLOOM_RATE is \"fast\", which is not a positive byte rate",
            Some("unset FETCHLOOM_RATE for no cap, or use bytes per second such as 500K or 2.5M"),
        ),
        (
            "FETCHLOOM_THREADS",
            "257",
            "FETCHLOOM_THREADS is \"257\", which is not a number from 1 to 256",
            Some(
                "unset FETCHLOOM_THREADS to use the default worker counts, or set it to a number from 1 to 256",
            ),
        ),
    ] {
        let output = sandbox
            .command(&["--json", "cache"])
            .env(name, value)
            .output()?;
        assert_eq!(output.status.code(), Some(2), "{name}");
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], "usage", "{name}");
        assert_eq!(document["errors"][0]["message"], said, "{name}");
        if let Some(help) = help {
            assert_eq!(document["errors"][0]["help"], help, "{name}");
        }
    }
    Ok(())
}

#[test]
fn ask_above_sets_the_size_that_needs_a_yes() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2048];
    let server = Server::start(
        "/two.bin",
        vec![file(&body, "\"v1\""), file(&body, "\"v1\"")],
    )?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_ASK_ABOVE", "1K")
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)?["errors"][0]["kind"], "unconfirmed");
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_ASK_ABOVE", "4K")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    Ok(())
}

#[test]
fn the_timeout_setting_ends_a_stalled_download() -> Result {
    let sandbox = Sandbox::new()?;
    let (release, held) = std::sync::mpsc::channel();
    let mut stalled = reply("200 OK", &[("Content-Length", "4096")], b"");
    stalled.push(Part::Wait(held));
    stalled.push(Part::Bytes(vec![b'x'; 4096]));
    let server = Server::start("/stalled.bin", vec![stalled])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_TIMEOUT", "1")
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    drop(release);
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "network.unreachable");
    let message = document["errors"][0]["message"].to_string();
    assert!(message.contains("sent nothing for 1 s"), "{message}");
    Ok(())
}

#[test]
fn a_rate_capped_download_keeps_every_byte() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2 * 1024 * 1024];
    let server = Server::start("/capped.bin", vec![file(&body, "\"v1\"")])?;
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("FETCHLOOM_RATE", "1M")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["work"]["bytes_down"], body.len());
    assert_eq!(
        std::fs::read(sandbox.work().join("capped").join("capped.bin"))?,
        body
    );
    Ok(())
}

#[test]
fn one_thread_unpacks_places_and_verifies_a_project() -> Result {
    let sandbox = Sandbox::new()?;
    let zip = std::fs::read(
        crate::support::workspace()
            .join("tests")
            .join("fixtures")
            .join("archives")
            .join("python.zip"),
    )?;
    let server = Server::start("/dataset.zip", vec![file(&zip, "\"v1\"")])?;
    for args in [
        vec!["--json", "add", &server.url],
        vec!["--json", "sync"],
        vec!["--json", "status"],
        vec!["--json", "cache", "verify"],
    ] {
        let output = sandbox
            .command(&args)
            .env("FETCHLOOM_THREADS", "1")
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stdout)?);
    }
    assert_eq!(
        std::fs::read(
            sandbox
                .work()
                .join("data")
                .join("dataset")
                .join("README.md")
        )?,
        b"# sample dataset\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// a variable naming certificates fetchloom cannot read is said plainly, never overruled by other
/// roots. macos and windows ask their own verifier, which does not read it.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn certificates_named_but_unreadable_are_a_usage_error() -> Result {
    let sandbox = Sandbox::new()?;
    let empty = sandbox.work().join("none.pem");
    std::fs::write(&empty, "")?;
    let output = sandbox
        .command(&["--json", "get", "http://127.0.0.1:9/table.csv"])
        .env("SSL_CERT_FILE", &empty)
        .output()?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    let message = document["errors"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.starts_with(&format!(
            "SSL_CERT_FILE names {}, which holds no certificates fetchloom can read",
            empty.display()
        )),
        "{message}"
    );
    assert_eq!(
        document["errors"][0]["help"],
        "unset SSL_CERT_FILE to use the system's certificates, or set it to a file of PEM certificates"
    );
    Ok(())
}

/// a system without a certificate store, as in a container built without one, still downloads:
/// fetchloom trusts the mozilla roots it carries.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_system_without_certificates_still_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let body = b"a,b\n1,2\n";
    let server = Server::start("/table.csv", vec![file(body, "\"v1\"")])?;
    // an empty variable has the system's store read as empty
    let output = sandbox
        .command(&["--json", "get", &server.url])
        .env("SSL_CERT_FILE", "")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    Ok(())
}
