//! The `fl` command line of fetchloom: arguments, configuration, the order of work in each command, and rendering.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

const WELCOME: &str = concat!(
    "fl ",
    env!("CARGO_PKG_VERSION"),
    ", uv for research data\n",
    "fetchloom by KairosLab\n"
);

#[derive(Parser)]
#[command(name = "fl", version, about = "uv for research data")]
struct Cli {}

/// Runs `fl` with the arguments of this process and returns its exit code.
///
/// `--help`, `--version` and usage errors are answered by the argument parser, which exits the
/// process itself with code 0 or 2. With no arguments the welcome is written to stdout, and a
/// failed write gives exit code 1.
#[must_use]
pub fn run() -> ExitCode {
    let Cli {} = Cli::parse();
    match welcome(&mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn welcome(out: &mut impl Write) -> io::Result<()> {
    out.write_all(WELCOME.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn welcome_writes_version_and_maker() {
        let mut out = Vec::new();
        welcome(&mut out).unwrap();
        let expected = format!(
            "fl {}, uv for research data\nfetchloom by KairosLab\n",
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn welcome_returns_the_write_error() {
        let err = welcome(&mut Broken).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
