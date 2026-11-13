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

/// each knob lean sets shows `resources` as its origin, but `most_files` is not sized from lean's
/// memory, and a knob set by name keeps its own value.
#[test]
fn settings_show_what_a_resources_level_derives() -> Result {
    let sandbox = Sandbox::new()?;
    let settings = |pairs: &[(&str, &str)]| -> Result<serde_json::Value> {
        let mut command = sandbox.command(&["--json", "settings"]);
        command.envs(pairs.iter().copied());
        let document = json(&command.output()?)?;
        assert_eq!(document["status"], "ok", "{document}");
        Ok(document)
    };
    let lean = settings(&[("FETCHLOOM_RESOURCES", "lean")])?;
    let resources = shown(&lean, "resources");
    assert_eq!(resources["value"], "lean");
    assert_eq!(resources["origin"], "environment");
    assert_eq!(resources["variable"], "FETCHLOOM_RESOURCES");
    let processors = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    for (name, value) in [
        ("jobs", serde_json::json!(2)),
        ("host_jobs", serde_json::json!(2)),
        ("memory", serde_json::json!(128_u64 << 20)),
        ("threads", serde_json::json!(processors.min(2))),
    ] {
        assert_eq!(shown(&lean, name)["value"], value, "{name}");
        assert_eq!(shown(&lean, name)["origin"], "resources", "{name}");
    }
    let default = settings(&[])?;
    assert_eq!(shown(&default, "resources")["value"], "default");
    // each run sizes most_files from the memory free at that moment, so two runs can differ;
    // sized from lean's 128 MiB it would be the floor of 100,000
    let most_files = shown(&lean, "most_files");
    assert_eq!(
        most_files["origin"],
        shown(&default, "most_files")["origin"]
    );
    assert_ne!(most_files["value"], 100_000, "{most_files}");
    let named = settings(&[("FETCHLOOM_RESOURCES", "lean"), ("FETCHLOOM_JOBS", "5")])?;
    assert_eq!(shown(&named, "jobs")["value"], 5);
    assert_eq!(shown(&named, "jobs")["origin"], "environment");
    Ok(())
}

/// each amount not at fetchloom's own default names what set it; without `-v` the line stays out.
#[test]
fn a_verbose_run_says_what_it_spends_and_why() -> Result {
    let sandbox = Sandbox::new()?;
    std::fs::write(sandbox.work().join("data.toml"), "")?;
    let named = [
        ("FETCHLOOM_JOBS", "3"),
        ("FETCHLOOM_HOST_JOBS", "5"),
        ("FETCHLOOM_THREADS", "1"),
        ("FETCHLOOM_MEMORY", "64M"),
    ];
    let output = sandbox.command(&["-v", "sync"]).envs(named).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // one worker is what FETCHLOOM_THREADS holds it to on a machine of more processors
    let processors = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let held = if processors > 1 {
        [
            "local workers: FETCHLOOM_THREADS\n",
            "hash lanes: FETCHLOOM_THREADS\n",
        ]
    } else {
        ["", "hash lanes: one processor\n"]
    };
    for line in [
        "Resources default: 3 downloads at once, 3 to one host, 1 local worker, 64.0 MiB for buffers, 1 hash lane\n",
        "downloads: FETCHLOOM_JOBS\n",
        "to one host: no more than the downloads at once\n",
        "buffers: FETCHLOOM_MEMORY\n",
    ]
    .into_iter()
    .chain(held)
    {
        assert!(stderr.contains(line), "{line}{stderr}");
    }
    let quiet = sandbox.command(&["sync"]).envs(named).output()?;
    assert!(!text(&quiet.stderr)?.contains("Resources"));
    Ok(())
}

/// the kernel would otherwise end the run partway, with no word of why.
#[test]
fn a_project_too_big_for_its_container_is_refused_before_it_is_read() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("parts")?;
    for name in ["a.csv", "b.csv", "c.csv"] {
        std::fs::write(folder.join(name), name)?;
    }
    let added = sandbox.run(&["add", "../parts"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let small = sandbox
        .command(&["--json", "status"])
        .env("FETCHLOOM_TEST_ROOM", (1_u64 << 20).to_string())
        .output()?;
    let document = json(&small)?;
    assert_eq!(small.status.code(), Some(50), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "local.io", "{document}");
    let message = error["message"].as_str().unwrap_or_default();
    assert!(message.contains("3 files"), "{message}");
    assert!(
        message.contains("1.0 MiB this container leaves"),
        "{message}"
    );
    assert!(
        error["help"]
            .as_str()
            .unwrap_or_default()
            .contains("memory limit"),
        "{document}"
    );
    let roomy = sandbox
        .command(&["--json", "status"])
        .env("FETCHLOOM_TEST_ROOM", (1_u64 << 30).to_string())
        .output()?;
    assert_eq!(roomy.status.code(), Some(0), "{}", text(&roomy.stdout)?);
    Ok(())
}

/// `FETCHLOOM_NO_CONFIG` works as `--no-config` does, `settings` says the file was bypassed, and
/// data.toml is still read.
#[test]
fn no_config_bypasses_the_settings_file_for_one_run() -> Result {
    let sandbox = Sandbox::new()?;
    let path = settings_file(&sandbox)?;
    std::fs::write(&path, "jbos = 4\n")?;
    let refused = sandbox.run(&["--json", "settings"])?;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stdout)?);
    let told = sandbox.run(&["--no-config", "settings"])?;
    let stderr = text(&told.stderr)?;
    assert_eq!(told.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(" (bypassed by --no-config)\n"), "{stderr}");
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_NO_CONFIG", "1")
            .output()?,
    )?;
    assert_eq!(document["status"], "ok", "{document}");
    assert_eq!(
        document["result"]["file"]["path"],
        path.display().to_string()
    );
    assert_eq!(document["result"]["file"]["bypassed"], true);
    assert_eq!(shown(&document, "jobs")["origin"], "default");
    std::fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.iris]\nref = \"zenodo:1\"\nselectt = \"*.csv\"\n",
    )?;
    let project = json(&sandbox.run(&["--json", "--no-config", "status"])?)?;
    assert_eq!(project["errors"][0]["kind"], "project.invalid", "{project}");
    let unread = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_NO_CONFIG", "maybe")
            .output()?,
    )?;
    assert_eq!(
        unread["errors"][0]["message"],
        "FETCHLOOM_NO_CONFIG is \"maybe\", which is not true or false"
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
    assert_eq!(document["errors"][0]["help"], "did you mean `jobs`?");
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
        (
            "FETCHLOOM_SPLIT",
            "yes",
            "FETCHLOOM_SPLIT is \"yes\", which is not true or false",
            Some(
                "set FETCHLOOM_SPLIT to true to split large downloads from the start or false to never split them, or unset it to split one once it proves slow",
            ),
        ),
        (
            "FETCHLOOM_KEYRING",
            "no",
            "FETCHLOOM_KEYRING is \"no\", which is not true or false",
            Some(
                "set FETCHLOOM_KEYRING to false to keep keys out of the system's credential store, or unset it",
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

/// spaces around `true` or `false` are allowed, as are `1` and `0`.
#[test]
fn on_off_variables_take_true_and_false_in_any_case() -> Result {
    let sandbox = Sandbox::new()?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SPLIT", " False ")
            .env("FETCHLOOM_ACCESSIBLE", "TRUE")
            .output()?,
    )?;
    assert_eq!(document["status"], "ok", "{document}");
    assert_eq!(document["warnings"], serde_json::json!([]), "{document}");
    assert_eq!(shown(&document, "split")["value"], "never");
    let accessible = shown(&document, "accessible");
    assert_eq!(accessible["value"], true, "{document}");
    assert_eq!(accessible["origin"], "environment");
    assert_eq!(accessible["variable"], "FETCHLOOM_ACCESSIBLE");
    let told = sandbox
        .command(&["settings"])
        .env("FETCHLOOM_NO_HINTS", "true")
        .output()?;
    let stderr = text(&told.stderr)?;
    assert_eq!(told.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("hint:"), "{stderr}");
    Ok(())
}

/// `FETCHLOOM_ACCESSIBLE` and `FETCHLOOM_NO_HINTS` change only what a person sees, so a bad value
/// never stops the run.
#[test]
fn a_display_variable_it_cannot_read_stays_off_with_a_warning() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["--json", "settings"])
        .env("FETCHLOOM_ACCESSIBLE", "yes")
        .env("FETCHLOOM_NO_HINTS", "on")
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(0), "{document}");
    assert_eq!(shown(&document, "accessible")["value"], false);
    let kinds: Vec<&str> = document["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|warning| warning["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["settings.switch_unread"; 2], "{document}");
    assert_eq!(
        crate::support::warned(&document),
        [
            "FETCHLOOM_ACCESSIBLE is \"yes\", which is not true or false, so it stays off; set it to true for plain lines a screen reader reads well, or unset it",
            "FETCHLOOM_NO_HINTS is \"on\", which is not true or false, so it stays off; set it to true to drop the hint: and next: lines, or unset it",
        ]
    );
    let told = sandbox
        .command(&["settings"])
        .env("FETCHLOOM_NO_HINTS", "on")
        .output()?;
    let stderr = text(&told.stderr)?;
    assert_eq!(told.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.starts_with(
            "warning: FETCHLOOM_NO_HINTS is \"on\", which is not true or false, so it stays off\n"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("hint:"), "{stderr}");
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

/// other roots never overrule `SSL_CERT_FILE`. macos and windows ask their own verifier, which
/// does not read it.
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

/// fetchloom carries the mozilla roots, for a container built without a certificate store.
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
