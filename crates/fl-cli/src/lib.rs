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
/// `--help` and `--version` print their text and give 0. A usage error prints the usage to stderr
/// and gives 2. With no arguments the welcome is written to stdout. A failed write gives 1.
#[must_use]
pub fn run() -> ExitCode {
    let written = match Cli::try_parse() {
        Ok(Cli {}) => welcome(&mut io::stdout().lock()),
        Err(parse) => match parse.print() {
            Ok(()) => return exit_code(parse.exit_code()),
            Err(write) => Err(write),
        },
    };
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

fn welcome(out: &mut impl Write) -> io::Result<()> {
    out.write_all(WELCOME.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing {
        on_write: bool,
    }

    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.on_write {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(bytes.len())
            }
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
        let err = welcome(&mut Failing { on_write: true }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn welcome_returns_the_flush_error() {
        let err = welcome(&mut Failing { on_write: false }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn exit_code_keeps_codes_that_fit_and_fails_the_rest() {
        assert_eq!(exit_code(0), ExitCode::SUCCESS);
        assert_eq!(exit_code(2), ExitCode::from(2));
        assert_eq!(exit_code(-1), ExitCode::FAILURE);
        assert_eq!(exit_code(256), ExitCode::FAILURE);
    }
}
