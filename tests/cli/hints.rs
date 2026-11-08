//! errors whose help names the fix that works for how the command was given.

use std::fs;

use crate::support::{Result, Sandbox, Server, text};

/// windows holds a file another program opened without sharing it, as Excel does, so reading it
/// fails, and replacing it fails while that program keeps it open even for reading.
#[cfg(windows)]
#[test]
fn a_file_another_program_holds_says_to_close_it() -> Result {
    use std::os::windows::fs::OpenOptionsExt;
    const SHARE_READ: u32 = 1;
    let sandbox = Sandbox::new()?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        "[datasets.iris]\nref = \"https://example.org/iris.csv\"\n",
    )?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&manifest)?;
    let output = sandbox.run(&["status"])?;
    drop(held);
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("being used by another process"), "{stderr}");
    assert!(
        stderr.ends_with(
            "help: close the program using the file, then run `fetchloom status` again\n"
        ),
        "{stderr}"
    );
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(&manifest)?;
    let output = sandbox.run(&["remove", "iris"])?;
    drop(held);
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("permission was denied"), "{stderr}");
    assert!(
        stderr.ends_with("help: close any program that has it open and check its permissions, then run `fetchloom remove iris` again\n"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn patterns_are_loosened_where_they_were_given() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", Vec::new())?;
    for command in ["add", "get"] {
        let output = sandbox.run(&[command, &server.url, "--exclude", "*"])?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{command}: the command line was wrong"
        );
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.ends_with("help: loosen the patterns given to --select and --exclude\n"),
            "{command}: {stderr}"
        );
    }
    let manifest = format!(
        "[datasets.iris]\nref = \"{}\"\nexclude = \"*\"\n",
        server.url
    );
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("data.toml:3   exclude = \"*\""), "{stderr}");
    assert!(
        stderr.ends_with("help: loosen the patterns in data.toml\n"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_folder_that_is_not_there_or_a_data_toml_that_does_not_parse_says_what_to_do() -> Result {
    let sandbox = Sandbox::new()?;
    let elsewhere = sandbox.run(&["-C", "no-such-folder", "status"])?;
    assert_eq!(elsewhere.status.code(), Some(2));
    let stderr = text(&elsewhere.stderr)?;
    assert!(
        stderr.ends_with("help: give -C a folder that exists and that you may open\n"),
        "{stderr}"
    );
    let manifest = sandbox.work().join("data.toml");
    fs::write(&manifest, "[datasets.iris\nref = \"zenodo:3608990\"\n")?;
    let broken = sandbox.run(&["status"])?;
    assert_eq!(broken.status.code(), Some(50));
    let stderr = text(&broken.stderr)?;
    assert!(stderr.contains("data.toml:1\n"), "{stderr}");
    assert!(!stderr.contains("[datasets.iris"), "{stderr}");
    assert!(
        stderr.ends_with("help: fix that line, then run `fetchloom status` again\n"),
        "{stderr}"
    );
    // notepad's "unicode" is UTF-16, which no TOML reader takes
    let wide: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain("[datasets]\n".encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    fs::write(&manifest, wide)?;
    let wide = sandbox.run(&["status"])?;
    assert_eq!(wide.status.code(), Some(50));
    let stderr = text(&wide.stderr)?;
    assert_eq!(
        stderr,
        "error: data.toml is not UTF-8 text\n  help: save it as UTF-8, then run `fetchloom status` again\n"
    );
    Ok(())
}

#[test]
fn a_name_add_cannot_use_is_refused_before_any_request() -> Result {
    let sandbox = Sandbox::new()?;
    let manifest = "[datasets.flowers]\nref = \"zenodo:1\"\n";
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    for (name, message) in [
        (
            "flowers",
            "error: data.toml already has a dataset called flowers\n",
        ),
        (
            "Flowers",
            "error: \"Flowers\" is not a valid dataset name\n",
        ),
    ] {
        // offline, resolving the link first would fail with exit 20 instead
        let output = sandbox.run(&["--offline", "add", "zenodo:3608990", "--as", name])?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(2), "{stderr}");
        assert!(stderr.starts_with(message), "{stderr}");
    }
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    Ok(())
}

#[test]
fn offline_hints_distinguish_listings_from_cached_downloads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![crate::support::file(b"iris", "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let locked = sandbox.folder("locked")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), locked.join(name))?;
    }
    let unresolved = sandbox.folder("unresolved")?;
    fs::write(
        unresolved.join("data.toml"),
        "[datasets.iris]\nref = \"zenodo:1\"\n",
    )?;
    let cache = sandbox.folder("empty-cache")?;
    let cases = [
        (
            &["get", "zenodo:1"][..],
            sandbox.work(),
            "looking up zenodo:1 needs the internet",
            Some("in a project prepared online, fetchloom sync --offline works without it"),
        ),
        (
            &["get", server.url.as_str()],
            sandbox.work(),
            "a new link must be downloaded once to learn its SHA-256",
            None,
        ),
        (
            &["sync"],
            unresolved,
            "needs a new entry in data.lock",
            None,
        ),
        (
            &["sync"],
            locked,
            "data.lock names is not in the cache",
            None,
        ),
    ];
    for (flag, environment, expected) in [
        (
            false,
            Some("1"),
            "unset FETCHLOOM_OFFLINE, then run `fetchloom ",
        ),
        (true, None, "run it again without --offline"),
        (
            true,
            Some("1"),
            "unset FETCHLOOM_OFFLINE, then run it again without --offline",
        ),
        (true, Some("0"), "run it again without --offline"),
    ] {
        for (args, folder, message, listing_help) in &cases {
            let mut command = sandbox.command_in(folder, args);
            command.arg("--json").env("FETCHLOOM_CACHE", &cache);
            if flag {
                command.arg("--offline");
            }
            if let Some(value) = environment {
                command.env("FETCHLOOM_OFFLINE", value);
            }
            let output = command.output()?;
            assert_eq!(
                output.status.code(),
                Some(20),
                "{args:?}: {}",
                text(&output.stderr)?
            );
            let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            let error = &document["errors"][0];
            assert_eq!(error["kind"], "offline.missing");
            assert!(
                error["message"]
                    .as_str()
                    .ok_or("missing message")?
                    .contains(*message)
            );
            let help = error["help"].as_str().ok_or("missing help")?;
            assert!(
                help.starts_with(listing_help.unwrap_or(expected)),
                "{args:?}, flag={flag}, environment={environment:?}: {help}"
            );
            assert_eq!(document["work"]["requests"], 0);
        }
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn errors_messages_account_typos_never_echo_link_secrets() -> Result {
    let sandbox = Sandbox::new()?;
    for account in [
        "https://example.org/?token=EM_CANARY",
        "https://user:EM_CANARY@example.org/",
    ] {
        for json in [false, true] {
            let mut args = vec!["auth", "login", account];
            if json {
                args.push("--json");
            }
            let output = sandbox.run(&args)?;
            assert_eq!(output.status.code(), Some(2));
            let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
            assert!(!shown.contains("EM_CANARY"), "{shown}");
        }
    }
    Ok(())
}

#[test]
fn normalized_account_typos_never_echo_link_secrets() -> Result {
    let sandbox = Sandbox::new()?;
    for account in [
        "ht\ttps://reader:mask_canary@example.org/data",
        "http\rs://reader:mask_canary@example.org/data",
        "https:\n//reader:mask%5Fcanary@example.org/data",
        "example.org?token=mask_canary",
        "example.org#client_secret=mask_canary",
        "https%3A%2F%2Freader%3Amask_canary%40example.org",
    ] {
        for action in ["login", "logout"] {
            for flags in [&[][..], &["--json"], &["-vv"], &["--json", "-vv"]] {
                let output = sandbox
                    .command(&["--offline", "auth", action, account])
                    .args(flags)
                    .output()?;
                assert_eq!(output.status.code(), Some(2));
                let shown = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
                assert!(
                    !shown.contains("mask_canary") && !shown.contains("mask%5Fcanary"),
                    "{shown}"
                );
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn errors_messages_json_help_survives_non_unicode_arguments() -> Result {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    let sandbox = Sandbox::new()?;
    for before in [&["--help"][..], &["get"], &["get", "--help"]] {
        let output = sandbox
            .command(before)
            .arg(OsString::from_vec(vec![0xff]))
            .args(["--help", "--json"])
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        let document = crate::support::json(&output)?;
        assert_eq!(document["schema"], 1);
        assert_eq!(document["command"], "help");
    }
    let after_separator = sandbox.run(&["--help", "--", "--json"])?;
    assert!(after_separator.status.success());
    assert!(serde_json::from_slice::<serde_json::Value>(&after_separator.stdout).is_err());
    Ok(())
}

#[test]
fn errors_messages_a_broken_lock_has_no_unusable_fix() -> Result {
    let sandbox = Sandbox::new()?;
    fs::write(sandbox.work().join("data.toml"), "[datasets]\n")?;
    fs::write(sandbox.work().join("data.lock"), "version = [\n")?;
    let output = sandbox.run(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(50));
    let document = crate::support::json(&output)?;
    let fix = &document["errors"][0]["fix"];
    if let Some(command) = fix.as_str() {
        assert_eq!(command, "fetchloom lock");
        let repeated = sandbox.run(&["--json", "lock"])?;
        assert!(
            fix.is_null(),
            "fix {fix} repeated exit {:?}: {}",
            repeated.status.code(),
            text(&repeated.stdout)?
        );
    }
    Ok(())
}

#[test]
fn errors_messages_flag_help_uses_the_same_palette() -> Result {
    let sandbox = Sandbox::new()?;
    let flag = sandbox
        .command(&["--color", "always", "get", "--help"])
        .env("FETCHLOOM_COLORS", "accent=magenta")
        .output()?;
    let named = sandbox
        .command(&["--color", "always", "help", "get"])
        .env("FETCHLOOM_COLORS", "accent=magenta")
        .output()?;
    assert!(text(&named.stdout)?.contains("\u{1b}[35m"));
    assert!(
        text(&flag.stdout)?.contains("\u{1b}[35m"),
        "flag help used no magenta"
    );
    assert!(!text(&flag.stdout)?.contains("\u{1b}[36m"));
    assert_eq!(flag.stdout, named.stdout);
    Ok(())
}

#[test]
fn errors_messages_flag_help_warns_about_invalid_palette_entries() -> Result {
    let sandbox = Sandbox::new()?;
    let flag = sandbox
        .command(&["--color", "always", "get", "--help"])
        .env("FETCHLOOM_COLORS", "accent=magenta:done=teal")
        .output()?;
    let named = sandbox
        .command(&["--color", "always", "help", "get"])
        .env("FETCHLOOM_COLORS", "accent=magenta:done=teal")
        .output()?;
    assert!(text(&named.stderr)?.contains("FETCHLOOM_COLORS"));
    assert_eq!(flag.stderr, named.stderr, "the flag-help warning was lost");
    Ok(())
}

#[test]
fn errors_messages_bundled_help_uses_json_and_the_right_command() -> Result {
    let sandbox = Sandbox::new()?;
    for (args, expected) in [
        (&["--json", "-qh"][..], None),
        (&["get", "--json", "-qh"][..], Some("get")),
        (
            &["-qC", "cache", "get", "--help", "--json"][..],
            Some("get"),
        ),
        (&["-qCcache", "get", "--help", "--json"][..], Some("get")),
    ] {
        let output = sandbox.run(args)?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        let document = crate::support::json(&output)?;
        assert_eq!(document["command"], "help");
        if let Some(expected) = expected {
            let commands = document["result"]["commands"]
                .as_array()
                .ok_or("missing help commands")?;
            assert_eq!(commands.len(), 1, "{args:?}: {document}");
            assert_eq!(commands[0]["name"], expected, "{args:?}: {document}");
        }
    }
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn attached_non_unicode_directories_keep_the_help_topic() -> Result {
    use std::ffi::OsString;
    #[cfg(unix)]
    let folder = {
        use std::os::unix::ffi::OsStringExt as _;
        OsString::from_vec(b"project-\xff".to_vec())
    };
    #[cfg(windows)]
    let folder = {
        use std::os::windows::ffi::OsStringExt as _;
        OsString::from_wide(&[112, 114, 111, 106, 101, 99, 116, 45, 0xd800])
    };
    let sandbox = Sandbox::new()?;
    for flag in ["-C", "-qC"] {
        let mut attached = OsString::from(flag);
        attached.push(&folder);
        for json in [false, true] {
            let mut command = sandbox.command(&[]);
            command.arg(&attached).args(["get", "--help"]);
            if json {
                command.arg("--json");
            }
            let output = command.output()?;
            assert!(output.status.success(), "{}", text(&output.stderr)?);
            if json {
                let document = crate::support::json(&output)?;
                let commands = document["result"]["commands"]
                    .as_array()
                    .ok_or("missing help commands")?;
                assert_eq!(commands.len(), 1, "{flag}: {document}");
                assert_eq!(commands[0]["name"], "get");
            } else {
                assert!(text(&output.stdout)?.contains("usage: fetchloom get"));
            }
        }
    }
    Ok(())
}
