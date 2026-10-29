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
    assert!(stderr.contains("(os error 32)"), "{stderr}");
    assert!(
        stderr.ends_with("help: close the program using the file, then run the command again\n"),
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
    assert!(stderr.contains("(os error 5)"), "{stderr}");
    assert!(
        stderr.ends_with("help: close any program that has it open and check its permissions, then run the command again\n"),
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
    assert!(stderr.contains("data.toml:1   [datasets.iris"), "{stderr}");
    assert!(
        stderr.ends_with("help: fix that line, then run the command again\n"),
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
        "✗ data.toml is not UTF-8 text\n  help: save it as UTF-8, then run the command again\n"
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
            "✗ data.toml already has a dataset called flowers\n",
        ),
        ("Flowers", "✗ \"Flowers\" is not a valid dataset name\n"),
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
fn offline_hints_name_the_settings_that_enabled_it() -> Result {
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
            "needs an answer from zenodo.org",
        ),
        (
            &["get", server.url.as_str()],
            sandbox.work(),
            "is not in the cache",
        ),
        (&["sync"], unresolved, "its entry in data.toml changed"),
        (&["sync"], locked, "the lock names is not in the cache"),
    ];
    for (flag, environment, expected) in [
        (
            false,
            Some("1"),
            "unset FETCHLOOM_OFFLINE, then run the same command",
        ),
        (true, None, "run the same command without --offline"),
        (
            true,
            Some("1"),
            "remove --offline and unset FETCHLOOM_OFFLINE, then run the same command",
        ),
        (true, Some("0"), "run the same command without --offline"),
    ] {
        for (args, folder, message) in &cases {
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
                help.starts_with(expected),
                "{args:?}, flag={flag}, environment={environment:?}: {help}"
            );
            assert_eq!(document["work"]["requests"], 0);
        }
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}
