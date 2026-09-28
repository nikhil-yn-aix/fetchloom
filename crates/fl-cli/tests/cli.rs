//! End to end tests of the `fl` and `fetchloom` executables.

use std::error::Error;
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn Error>>;

const FL: &str = env!("CARGO_BIN_EXE_fl");
const FETCHLOOM: &str = env!("CARGO_BIN_EXE_fetchloom");

fn run(binary: &str, args: &[&str]) -> std::io::Result<Output> {
    Command::new(binary).args(args).output()
}

fn expected_version() -> String {
    format!("fl {}\n", env!("CARGO_PKG_VERSION"))
}

#[test]
fn both_executables_print_the_name_and_version() -> TestResult {
    for binary in [FL, FETCHLOOM] {
        let out = run(binary, &["--version"])?;
        assert_eq!(out.status.code(), Some(0), "{binary}");
        assert_eq!(
            String::from_utf8(out.stdout)?,
            expected_version(),
            "{binary}"
        );
        assert!(out.stderr.is_empty(), "{binary}");
    }
    Ok(())
}

#[test]
fn no_arguments_prints_the_welcome() -> TestResult {
    let out = run(FL, &[])?;
    assert_eq!(out.status.code(), Some(0));
    let expected = format!(
        "fl {}, uv for research data\nfetchloom by KairosLab\n",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(String::from_utf8(out.stdout)?, expected);
    assert!(out.stderr.is_empty());
    Ok(())
}

#[test]
fn unknown_argument_fails_with_usage_on_stderr() -> TestResult {
    let out = run(FL, &["--bogus"])?;
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains("unexpected argument '--bogus'"), "{stderr}");
    assert!(stderr.contains("Usage: fl"), "{stderr}");
    Ok(())
}

#[test]
fn help_names_fl() -> TestResult {
    let out = run(FL, &["--help"])?;
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8(out.stdout)?;
    assert!(stdout.starts_with("uv for research data"), "{stdout}");
    assert!(stdout.contains("Usage: fl"), "{stdout}");
    Ok(())
}
