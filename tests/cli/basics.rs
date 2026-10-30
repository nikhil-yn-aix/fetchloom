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
