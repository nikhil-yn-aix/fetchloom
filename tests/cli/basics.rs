//! the version, the welcome, help and usage errors.

use serde_json::Value;

use crate::support::{Result, Sandbox, text};

#[test]
fn version() -> Result {
    let output = Sandbox::new()?.run(&["--version"])?;
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)?,
        format!("fetchloom {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn fl_is_the_same_program() -> Result {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fl"))
        .arg("--version")
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)?,
        format!("fetchloom {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_is_lowercase() -> Result {
    let output = Sandbox::new()?.run(&["--help"])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

#[test]
fn help_follows_color_flags_anywhere() -> Result {
    let sandbox = Sandbox::new()?;
    for (arguments, colored) in [
        (&["--color", "always", "--help"][..], true),
        (&["--help", "--color", "always"], true),
        (&["--color", "always", "get", "-h"], true),
        (&["get", "--color=always", "--help"], true),
        (&["cache", "gc", "-h", "--color=always"], true),
        (&["--color", "always", "help", "get"], true),
        (&["help", "cache", "gc", "--color", "always"], true),
        (&["--color", "never", "--help"], false),
        (&["get", "-h", "--color=never"], false),
        (&["help", "get", "--color", "never"], false),
        (&["--help", "--", "--color", "always"], false),
        (&["--help", "--color", "invalid"], false),
    ] {
        let output = sandbox
            .command(arguments)
            .env("NO_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .output()?;
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            text(&output.stderr)?
        );
        let stdout = text(&output.stdout)?;
        assert_eq!(stdout.contains("\x1b["), colored, "{arguments:?}: {stdout}");
        assert!(stdout.contains("usage: "), "{arguments:?}: {stdout}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
    }
    Ok(())
}

#[test]
fn help_follows_color_environment() -> Result {
    let sandbox = Sandbox::new()?;
    for arguments in [
        &["--help"][..],
        &["get", "-h"],
        &["help", "get"],
        &["cache", "gc", "--help"],
        &["help", "cache", "gc"],
    ] {
        for (no_color, force, colored) in [("1", "1", false), ("", "1", true), ("", "0", false)] {
            let output = sandbox
                .command(arguments)
                .env("NO_COLOR", no_color)
                .env("CLICOLOR_FORCE", force)
                .output()?;
            assert!(
                output.status.success(),
                "{arguments:?}: {}",
                text(&output.stderr)?
            );
            let stdout = text(&output.stdout)?;
            assert_eq!(
                stdout.contains("\x1b["),
                colored,
                "{arguments:?}, NO_COLOR={no_color}, CLICOLOR_FORCE={force}: {stdout}"
            );
        }
    }
    Ok(())
}

#[test]
fn get_has_its_own_help() -> Result {
    let output = Sandbox::new()?.run(&["get", "--help"])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

/// docs/scope.md names options of `add` and `get`, and each one is in that command's help.
#[test]
fn the_options_scope_names_are_the_ones_add_and_get_take() -> Result {
    let scope = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("scope.md"),
    )?;
    let options = scope
        .lines()
        .find(|line| line.starts_with("every command takes"))
        .ok_or("scope.md names no options")?;
    let sandbox = Sandbox::new()?;
    for (commands, intro) in [
        (&["add", "get"][..], "`add` and `get` also take "),
        (&["add"], "`add` takes "),
        (&["get"], "`get` takes "),
    ] {
        let (_, rest) = options.split_once(intro).ok_or(intro)?;
        let named = rest
            .split(". ")
            .next()
            .and_then(|sentence| sentence.split(", and ").next())
            .unwrap_or_default();
        for command in commands {
            let help = text(&sandbox.run(&[command, "--help"])?.stdout)?;
            for option in named.split('`').skip(1).step_by(2) {
                let flag = option.split(' ').next().unwrap_or(option);
                assert!(
                    help.contains(&format!("{flag} ")) || help.contains(&format!("{flag},")),
                    "scope.md says {command} takes {flag}"
                );
            }
        }
    }
    Ok(())
}

/// the analogy a beginner meets on the first run, in the top of `--help` and in `help concepts`.
pub(crate) const ANALOGY: [&str; 4] = [
    "data.toml, the shopping list: which datasets you want",
    "data.lock, the receipt: the exact files and their fingerprints",
    "the cache, the pantry: shared by every project on this machine",
    "sync, restocking: data/ ends up matching the receipt",
];

#[test]
fn welcome() -> Result {
    let output = Sandbox::new()?.run(&[])?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert!(stdout.lines().count() <= 16, "{stdout}");
    for line in ANALOGY {
        assert!(stdout.contains(&format!("\n  {line}\n")), "{line}");
    }
    assert!(
        stdout.contains("\n  fetchloom add zenodo:3608990 "),
        "the welcome has a first command to copy"
    );
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.contains("`fetchloom help concepts`")),
        "the welcome ends with where to learn more"
    );
    insta::assert_snapshot!(stdout);
    Ok(())
}

/// the quick start in README.md shows the first run as it is.
#[test]
fn the_readme_quick_start_is_the_first_run() -> Result {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"),
    )?
    .replace("\r\n", "\n");
    let (_, start) = readme
        .split_once("## quick start\n")
        .ok_or("README.md has no quick start")?;
    let (_, block) = start
        .split_once("```text\n")
        .ok_or("the quick start shows no first run")?;
    let (shown, _) = block.split_once("```").ok_or("the first run never ends")?;
    let welcome = text(&Sandbox::new()?.run(&[])?.stdout)?;
    assert_eq!(shown, welcome);
    Ok(())
}

#[test]
fn help_opens_like_the_first_run() -> Result {
    let sandbox = Sandbox::new()?;
    let welcome = text(&sandbox.run(&[])?.stdout)?;
    let (version, intro) = welcome
        .split_once("\n\n")
        .ok_or("the welcome has no version line")?;
    assert_eq!(version, format!("fetchloom {}", env!("CARGO_PKG_VERSION")));
    let help = text(&sandbox.run(&["--help"])?.stdout)?;
    assert!(
        help.starts_with(&format!("{}\n\nusage: ", intro.trim_end())),
        "{help}"
    );
    Ok(())
}

#[test]
fn help_concepts_explains_the_ideas_with_the_analogy() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["help", "concepts"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let stdout = text(&output.stdout)?;
    assert!(stdout.lines().count() < 30, "{stdout}");
    for line in ANALOGY {
        assert!(stdout.contains(&format!("\n  {line}\n")), "{line}");
    }
    assert_eq!(
        sandbox.run(&["--json", "help", "concepts"])?.stdout,
        output.stdout,
        "help stays text"
    );
    insta::assert_snapshot!(stdout);
    Ok(())
}

#[test]
fn help_shows_a_command_and_suggests_a_topic() -> Result {
    let sandbox = Sandbox::new()?;
    for (topic, flag) in [
        (&[][..], &["--help"][..]),
        (&["get"], &["get", "--help"]),
        (&["cache", "gc"], &["cache", "gc", "--help"]),
    ] {
        let help = sandbox.run(&[&["help"][..], topic].concat())?;
        assert!(help.status.success(), "{topic:?}");
        assert_eq!(help.stdout, sandbox.run(flag)?.stdout, "{topic:?}");
    }
    let typo = sandbox.run(&["--json", "help", "concept"])?;
    assert_eq!(typo.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&typo.stdout)?;
    assert_eq!(document["command"], Value::Null, "help runs no command");
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom help concepts`?"
    );
    Ok(())
}

#[test]
fn an_unknown_option_is_a_usage_error() -> Result {
    let output = Sandbox::new()?.run(&["--verison"])?;
    assert_eq!(output.status.code(), Some(2));
    insta::assert_snapshot!(text(&output.stderr)?);
    Ok(())
}

#[test]
fn a_missing_link_shows_the_usage() -> Result {
    let output = Sandbox::new()?.run(&["get"])?;
    assert_eq!(output.status.code(), Some(2));
    insta::assert_snapshot!(text(&output.stderr)?);
    Ok(())
}

#[test]
fn a_usage_error_follows_verbose_and_color() -> Result {
    let sandbox = Sandbox::new()?;
    let verbose = sandbox.run(&["-v", "sycn"])?;
    assert_eq!(verbose.status.code(), Some(2));
    let stderr = text(&verbose.stderr)?;
    assert!(stderr.contains("\n  kind: usage\n"), "{stderr}");
    let colored = sandbox.run(&["--color", "always", "sycn"])?;
    let stderr = text(&colored.stderr)?;
    assert!(stderr.starts_with("\u{1b}["), "{stderr}");
    let plain = sandbox.run(&["sycn"])?;
    assert!(!text(&plain.stderr)?.contains('\u{1b}'));
    Ok(())
}

#[test]
fn color_marks_meaning_with_a_symbol_and_stays_off_when_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let colored = text(&sandbox.run(&["--color", "always", "sycn"])?.stderr)?;
    assert!(
        colored.contains("31m✗"),
        "an error is red and marked: {colored}"
    );
    assert!(
        colored.contains("\u{1b}[36m`fetchloom sync`\u{1b}[0m"),
        "a command takes the accent: {colored}"
    );
    for (flag, variables) in [
        (&["--color", "never"][..], &[("CLICOLOR_FORCE", "1")][..]),
        (&[], &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")]),
        (&[], &[]),
    ] {
        let mut command = sandbox.command(&[flag, &["sycn"]].concat());
        command.envs(variables.iter().copied());
        let stderr = text(&command.output()?.stderr)?;
        assert!(
            !stderr.contains('\u{1b}'),
            "{flag:?} {variables:?}: {stderr}"
        );
        assert!(stderr.starts_with('✗'), "{stderr}");
    }
    Ok(())
}

#[test]
fn json_gets_one_document_whatever_the_command_line() -> Result {
    let sandbox = Sandbox::new()?;
    let version = env!("CARGO_PKG_VERSION");
    for (args, command, code) in [
        (&["--json", "sycn"][..], Value::Null, 2),
        (&["sycn", "--json"], Value::Null, 2),
        (&["--json", "add"], "add".into(), 2),
        (&["cache", "gx", "--json"], "cache".into(), 2),
        (&["--json", "--version"], Value::Null, 0),
        (&["--json"], Value::Null, 0),
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        let document: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(document["schema"], 1, "{args:?}");
        assert_eq!(document["command"], command, "{args:?}");
        let errors = document["errors"].as_array().ok_or("no errors")?;
        if code == 0 {
            assert_eq!(document["status"], "ok", "{args:?}");
            assert_eq!(document["result"]["version"], version, "{args:?}");
            assert!(errors.is_empty(), "{args:?}");
        } else {
            assert_eq!(document["status"], "error", "{args:?}");
            assert_eq!(errors.len(), 1, "{args:?}");
            assert_eq!(errors[0]["kind"], "usage", "{args:?}");
        }
    }
    let typo = sandbox.run(&["--json", "sycn"])?;
    let document: Value = serde_json::from_slice(&typo.stdout)?;
    assert_eq!(
        document["errors"][0]["message"],
        "there is no command called \"sycn\""
    );
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom sync`?"
    );
    let help = sandbox.run(&["--json", "--help"])?;
    assert_eq!(
        help.stdout,
        sandbox.run(&["--help"])?.stdout,
        "help stays text"
    );
    Ok(())
}

#[test]
fn a_nested_command_typo_keeps_its_parent_in_the_suggestion() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["cache", "verfiy", "--json"])?;
    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom cache verify`?"
    );
    let corrected = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        corrected.status.code(),
        Some(0),
        "{}",
        text(&corrected.stderr)?
    );
    Ok(())
}
