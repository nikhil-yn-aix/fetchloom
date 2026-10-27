//! the binary as a user runs it.

use assert_cmd::Command;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn run(binary: &str, args: &[&str]) -> Result<std::process::Output> {
    let mut command = Command::cargo_bin(binary)?;
    command
        .args(args)
        .env_remove("NO_COLOR")
        .env("CLICOLOR", "0");
    Ok(command.output()?)
}

fn text(bytes: &[u8]) -> Result<String> {
    Ok(String::from_utf8(bytes.to_vec())?)
}

#[test]
fn version() -> Result {
    let output = run("fetchloom", &["--version"])?;
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)?,
        format!("fetchloom {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn fl_is_the_same_program() -> Result {
    let output = run("fl", &["--version"])?;
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)?,
        format!("fetchloom {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_is_lowercase() -> Result {
    let output = run("fetchloom", &["--help"])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

#[test]
fn welcome() -> Result {
    let output = run("fetchloom", &[])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

#[test]
fn an_unknown_option_is_a_usage_error() -> Result {
    let output = run("fetchloom", &["--verison"])?;
    assert_eq!(output.status.code(), Some(2));
    insta::assert_snapshot!(text(&output.stderr)?);
    Ok(())
}
