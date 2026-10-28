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

#[test]
fn welcome() -> Result {
    let output = Sandbox::new()?.run(&[])?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert_eq!(
        stdout.lines().last(),
        Some("  fetchloom add zenodo:3608990"),
        "the welcome ends with a command to copy"
    );
    insta::assert_snapshot!(stdout);
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
