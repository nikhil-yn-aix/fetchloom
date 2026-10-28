//! the version, the welcome, help and usage errors.

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
    insta::assert_snapshot!(text(&output.stdout)?);
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
