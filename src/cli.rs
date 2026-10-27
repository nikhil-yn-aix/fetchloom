//! the command line: read the arguments, run the command, return the exit code.

mod args;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};

use crate::error::{Error, Kind};
use crate::output::{ColorChoice, Output, Style};

pub(crate) fn run() -> ExitCode {
    if let Err(err) = args::Args::try_parse() {
        return usage(&err);
    }
    let _ = writeln!(std::io::stdout().lock(), "{}", welcome());
    ExitCode::SUCCESS
}

fn welcome() -> String {
    format!(
        "fetchloom {}\n\nput the exact data a project needs onto any machine with one command.\n\n`fetchloom --help` lists every option.",
        env!("CARGO_PKG_VERSION")
    )
}

/// help and version go to stdout as clap writes them. a mistake on the command line becomes one
/// of our own errors, so it reads like every other failure.
fn usage(err: &clap::Error) -> ExitCode {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    let error = describe(err);
    Output::new(Style::detect(ColorChoice::Auto)).error(&error);
    ExitCode::from(error.kind.exit_code())
}

fn describe(err: &clap::Error) -> Error {
    let named = |context| match err.get(context) {
        Some(ContextValue::String(value)) => Some(value.clone()),
        Some(ContextValue::Strings(values)) => values.first().cloned(),
        _ => None,
    };
    let message = match (
        err.kind(),
        named(ContextKind::InvalidSubcommand),
        named(ContextKind::InvalidArg),
    ) {
        (ErrorKind::InvalidSubcommand, Some(command), _) => {
            format!("there is no command called \"{command}\"")
        }
        (ErrorKind::UnknownArgument, _, Some(arg)) => {
            format!("there is no option called \"{arg}\"")
        }
        _ => first_line(err),
    };
    let help = if let Some(command) = named(ContextKind::SuggestedSubcommand) {
        format!("did you mean `fetchloom {command}`?")
    } else if let Some(arg) = named(ContextKind::SuggestedArg) {
        format!("did you mean `{arg}`?")
    } else {
        "`fetchloom --help` lists every command and option".to_owned()
    };
    Error::new(Kind::Usage, message).help(help)
}

fn first_line(err: &clap::Error) -> String {
    let text = err.to_string();
    let line = text.lines().next().unwrap_or_default();
    line.strip_prefix("error: ").unwrap_or(line).to_owned()
}
