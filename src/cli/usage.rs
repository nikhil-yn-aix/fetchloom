//! a command line clap cannot read, answered like every other failure.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Instant;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{ArgMatches, CommandFactory};

use super::{Context, args, finish, level};
use crate::error::{Error, Kind};
use crate::output::{ColorChoice, Level, Output, Style};

/// help goes to stdout as clap writes it, even with `--json`, since it is for people. a mistake
/// on the command line becomes one of our own errors, printed like every other failure: in the
/// document `--json` asks for, or with the detail and colors the options clap read ask for.
pub(super) fn answer(err: &clap::Error, started: Instant) -> ExitCode {
    if err.kind() == ErrorKind::DisplayHelp {
        crate::output::help(&err.render().to_string(), args::color());
        return ExitCode::SUCCESS;
    }
    let read = args::Args::command()
        .ignore_errors(true)
        .try_get_matches()
        .ok();
    let (level, color) = read
        .as_ref()
        .map_or((Level::Normal, ColorChoice::Auto), |read| {
            let quiet = read.get_flag("quiet");
            let color = read.get_one("color").copied().unwrap_or_default();
            (level(quiet, read.get_count("verbose")), color)
        });
    let output = Output::new(Style::detect(color), level, asks_for_json(), false);
    // help prints text, never a document, so a mistake after it names no command
    let command = read
        .as_ref()
        .and_then(named_command)
        .filter(|name| name != "help");
    finish::<()>(
        &Context::new(output),
        command.as_deref(),
        Err(describe(err, command.as_deref())),
        started,
    )
}

/// whether the command line asks for `--json`, read from the arguments themselves, since clap
/// stops at the first mistake and `--json` may come after it.
fn asks_for_json() -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json")
}

/// the command `read` names, such as `cache` in `fetchloom cache gx`.
fn named_command(read: &ArgMatches) -> Option<String> {
    let mut names = Vec::new();
    let mut current = read;
    while let Some((name, inner)) = current.subcommand() {
        names.push(name);
        current = inner;
    }
    (!names.is_empty()).then(|| names.join(" "))
}
fn describe(err: &clap::Error, parent: Option<&str>) -> Error {
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
            format!("unexpected argument \"{arg}\"")
        }
        (ErrorKind::MissingRequiredArgument, _, Some(arg)) => format!("{arg} is missing"),
        _ => first_line(err),
    };
    // a missing or wrong value is easiest to fix next to the usage line
    let usage = match err.get(ContextKind::Usage) {
        Some(ContextValue::StyledStr(text))
            if matches!(
                err.kind(),
                ErrorKind::MissingRequiredArgument | ErrorKind::InvalidValue
            ) =>
        {
            let text = text.to_string();
            let text = text.trim();
            Some(
                text.strip_prefix("Usage:")
                    .map_or(text, str::trim)
                    .to_owned(),
            )
        }
        _ => None,
    };
    let placement = (err.kind() == ErrorKind::UnknownArgument)
        .then(|| named(ContextKind::InvalidArg).and_then(|arg| misplaced_option(&arg)))
        .flatten();
    let help = if let Some(placement) = placement {
        placement
    } else if let Some(command) = named(ContextKind::SuggestedSubcommand) {
        match parent {
            Some(parent) => format!("did you mean `fetchloom {parent} {command}`?"),
            None => format!("did you mean `fetchloom {command}`?"),
        }
    } else if let Some(arg) = named(ContextKind::SuggestedArg) {
        format!("did you mean `{arg}`?")
    } else if let Some(ContextValue::Strings(values)) = err.get(ContextKind::ValidValue) {
        format!("use one of: {}", values.join(", "))
    } else if let Some(usage) = &usage {
        let command: Vec<&str> = usage
            .split_whitespace()
            .take_while(|word| !word.starts_with(['[', '<']))
            .collect();
        format!("`{} --help` says what it takes", command.join(" "))
    } else if let Some(parent) = parent {
        format!("`fetchloom {parent} --help` says what it takes")
    } else {
        "`fetchloom --help` lists commands and options".to_owned()
    };
    let error = Error::new(Kind::Usage, message);
    match usage {
        Some(usage) => error.detail(format!("usage: {usage}")),
        None => error,
    }
    .help(help)
}

fn misplaced_option(argument: &str) -> Option<String> {
    let long = argument.strip_prefix("--")?.split('=').next()?;
    let command = args::Args::command();
    let owners: Vec<_> = command
        .get_subcommands()
        .filter_map(|command| {
            let option = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(long))?;
            Some((command, option))
        })
        .collect();
    let (first, option) = *owners.first()?;
    let names: Vec<_> = owners
        .iter()
        .map(|(command, _)| command.get_name())
        .collect();
    let commands = match names.split_last()? {
        (last, []) => (*last).to_owned(),
        (last, rest) => format!("{} and {last}", rest.join(", ")),
    };
    let mut example = format!("fetchloom {}", first.get_name());
    for arg in first.get_positionals().filter(|arg| arg.is_required_set()) {
        let name = arg
            .get_value_names()
            .and_then(|names| names.first())
            .map_or(arg.get_id().as_str(), |name| name.as_str());
        let _ = write!(example, " <{name}>");
    }
    let _ = write!(example, " --{long}");
    if option.get_action().takes_values() {
        let value = option
            .get_value_names()
            .and_then(|names| names.first())
            .map_or("value", |name| name.as_str());
        if value == "glob" {
            example.push_str(" \"*.csv\"");
        } else {
            let _ = write!(example, " <{value}>");
        }
    }
    Some(format!(
        "--{long} is an option of {commands}, so it goes after the command, as in {example}"
    ))
}

fn first_line(err: &clap::Error) -> String {
    let text = err.to_string();
    let line = text.lines().next().unwrap_or_default();
    line.strip_prefix("error: ").unwrap_or(line).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(arguments: &[&str]) -> Result<Error, Box<dyn std::error::Error>> {
        let error = args::Args::command()
            .try_get_matches_from(arguments)
            .err()
            .ok_or("the invalid command unexpectedly parsed")?;
        let parsed = args::Args::command()
            .ignore_errors(true)
            .try_get_matches_from(arguments)?;
        Ok(describe(&error, named_command(&parsed).as_deref()))
    }

    #[test]
    fn misplaced_options_name_their_commands_and_show_correct_placement()
    -> Result<(), Box<dyn std::error::Error>> {
        for (option, arguments, expected) in [
            (
                "--select",
                vec!["fetchloom", "--select", "*.csv", "add", "zenodo:1"],
                "--select is an option of add and get, so it goes after the command, as in fetchloom add <link> --select \"*.csv\"",
            ),
            (
                "--force",
                vec!["fetchloom", "--force", "sync"],
                "--force is an option of sync, so it goes after the command, as in fetchloom sync --force",
            ),
        ] {
            let error = problem(&arguments)?;
            assert_eq!(error.help.as_deref(), Some(expected), "{option}");
        }
        Ok(())
    }

    #[test]
    fn extra_positionals_are_arguments_not_options() -> Result<(), Box<dyn std::error::Error>> {
        for (arguments, expected) in [
            (
                &["fetchloom", "sync", "iris"][..],
                "unexpected argument \"iris\"",
            ),
            (
                &["fetchloom", "get", "https://example.org/file.csv", "iris"],
                "unexpected argument \"iris\"",
            ),
            (
                &["fetchloom", "sync", "--", "-iris"],
                "unexpected argument \"-iris\"",
            ),
        ] {
            assert_eq!(problem(arguments)?.message, expected);
        }
        Ok(())
    }

    #[test]
    fn command_errors_link_to_their_own_help() -> Result<(), Box<dyn std::error::Error>> {
        for (arguments, expected) in [
            (
                &["fetchloom", "get", "--wat"][..],
                "`fetchloom get --help` says what it takes",
            ),
            (
                &["fetchloom", "sync", "iris"],
                "`fetchloom sync --help` says what it takes",
            ),
            (
                &["fetchloom", "cache", "verify", "--wat"],
                "`fetchloom cache verify --help` says what it takes",
            ),
            (
                &["fetchloom", "--wat"],
                "`fetchloom --help` lists commands and options",
            ),
        ] {
            assert_eq!(problem(arguments)?.help.as_deref(), Some(expected));
        }
        Ok(())
    }
}
