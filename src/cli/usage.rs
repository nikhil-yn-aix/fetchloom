//! a command line clap cannot read, answered like every other failure.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Instant;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{ArgMatches, CommandFactory};

use super::{Context, args, finish, level};
use crate::error::{Error, Kind};
use crate::name;
use crate::net;
use crate::output::{ColorChoice, Level, Output, Style};

/// a command-line mistake as one of our errors, in the document `--json` asks for or with the
/// detail and colors the options clap read ask for. argument reading routes JSON help itself.
pub(super) fn answer(err: &clap::Error, started: Instant) -> ExitCode {
    if err.kind() == ErrorKind::DisplayHelp {
        let color = args::color();
        let output = Output::new(Style::detect(color), Level::Normal, false, false);
        let shown = args::short_help()
            .and_then(|topic| super::help::short(&topic))
            .unwrap_or_else(|| super::help::examples_first(&err.render().to_string()));
        let outcome = crate::output::help(&shown, color);
        return finish(&Context::new(output), None, outcome, started);
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
    let output = Output::new(Style::detect(color), level, asks_for_json(), false)
        .fitted(crate::output::story_columns());
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

/// `verify` for `cache verify`: the full command when `command` only exists one level down.
fn nested(command: &str) -> Option<String> {
    args::Args::command().get_subcommands().find_map(|parent| {
        parent
            .find_subcommand(command)
            .map(|_| format!("{} {command}", parent.get_name()))
    })
}

/// the first value clap recorded for `kind`, if any, a secret in it masked.
fn context_value(err: &clap::Error, kind: ContextKind) -> Option<String> {
    typed(err, kind)
        .into_iter()
        .next()
        .map(|value| net::mask_query(&value).into_owned())
}

/// every value clap recorded for `kind`, as it was typed.
fn typed(err: &clap::Error, kind: ContextKind) -> Vec<String> {
    match err.get(kind) {
        Some(ContextValue::String(value)) => vec![value.clone()],
        Some(ContextValue::Strings(values)) => values.clone(),
        _ => Vec::new(),
    }
}

/// a conflict whose reason one sentence gives better than clap's words, as message and help.
fn explained(err: &clap::Error) -> Option<(&'static str, &'static str)> {
    if err.kind() != ErrorKind::ArgumentConflict {
        return None;
    }
    let arg = context_value(err, ContextKind::InvalidArg)?;
    let prior = context_value(err, ContextKind::PriorArg)?;
    let pair = [bare(&arg), bare(&prior)];
    (pair.contains(&"--force") && pair.contains(&"--dir")).then_some((
        "--force replaces only the file -o names, so it cannot go with --dir",
        "leave out --force and give --dir an empty folder, or write one file with -o <file> --force",
    ))
}

/// the error line, in fetchloom's words where clap's are stiff.
fn message(err: &clap::Error) -> String {
    let named = |kind| context_value(err, kind);
    match (
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
        (ErrorKind::InvalidValue | ErrorKind::ValueValidation, _, Some(arg)) => {
            match named(ContextKind::InvalidValue) {
                Some(value) if !value.is_empty() => format!("{} cannot be \"{value}\"", bare(&arg)),
                _ => format!("{} needs a value", bare(&arg)),
            }
        }
        (ErrorKind::ArgumentConflict, _, Some(arg)) => match named(ContextKind::PriorArg) {
            Some(prior) => format!(
                "{} and {} cannot be used together",
                bare(&arg),
                bare(&prior)
            ),
            None => first_line(err),
        },
        _ => first_line(err),
    }
}

/// what a command people bring from pip, git, gh, npm or wget is called here: the nearest
/// spelling would send `install` to `list` and `update` to `path`.
fn habit(command: &str) -> Option<&'static str> {
    Some(match command {
        "auth status" => "`fetchloom auth` alone lists each account and whether it is signed in",
        "init" => "no init needed: `fetchloom add <link>` starts a project in this folder",
        "new" => "no setup needed: `fetchloom add <link>` starts a project in this folder",
        "install" => "`fetchloom add <name>` adds a dataset; `fetchloom sync` fills data/",
        "download" | "fetch" => {
            "`fetchloom get <name>` downloads once; `fetchloom add <name>` pins it"
        }
        "pull" => "`fetchloom sync` puts the files data.lock pins into data/",
        "update" | "upgrade" => {
            "`fetchloom lock --upgrade` looks for newer data, then `fetchloom sync`"
        }
        "delete" | "uninstall" => "`fetchloom remove <name>` drops a dataset from the project",
        _ => return None,
    })
}

fn describe(err: &clap::Error, parent: Option<&str>) -> Error {
    if let Some((message, help)) = explained(err) {
        return Error::new(Kind::Usage, message).help(help);
    }
    let named = |kind| context_value(err, kind);
    // a missing or wrong value is easiest to fix next to the usage lines, one for each form
    let usage: Option<Vec<String>> = match err.get(ContextKind::Usage) {
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
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )
        }
        _ => None,
    };
    let message = match (&usage, parent) {
        (Some(forms), Some(parent)) if err.kind() == ErrorKind::MissingRequiredArgument => {
            needs(parent, forms)
        }
        _ => message(err),
    };
    let placement = (err.kind() == ErrorKind::UnknownArgument)
        .then(|| named(ContextKind::InvalidArg).and_then(|arg| misplaced_option(&arg, parent)))
        .flatten();
    let help = if let Some(placement) = placement {
        placement
    } else if let Some(meant) = (err.kind() == ErrorKind::InvalidSubcommand)
        .then(|| named(ContextKind::InvalidSubcommand))
        .flatten()
        .and_then(|command| {
            let command = command.to_lowercase();
            match parent {
                Some(parent) => habit(&format!("{parent} {command}")),
                None => habit(&command),
            }
        })
    {
        meant.to_owned()
    } else if let Some(meant) = guess(err, parent) {
        format!("did you mean `{meant}`?")
    } else if let Some(reason) = (err.kind() == ErrorKind::ValueValidation)
        .then(|| std::error::Error::source(err))
        .flatten()
    {
        // the readers in args.rs word a refusal as what to give instead
        reason.to_string()
    } else if let Some(ContextValue::Strings(values)) = err
        .get(ContextKind::ValidValue)
        .filter(|values| !matches!(values, ContextValue::Strings(values) if values.is_empty()))
    {
        match values.as_slice() {
            [] => String::new(),
            [only] => format!("use {only}"),
            many => format!("use one of {}", crate::units::listed(many, "or")),
        }
    } else if let Some(first) = usage.as_ref().and_then(|forms| forms.first()) {
        let command: Vec<&str> = first
            .split_whitespace()
            .take_while(|word| !word.starts_with(['[', '<']))
            .collect();
        format!("`{} --help` says what it takes", command.join(" "))
    } else if let Some(parent) = parent {
        format!("`fetchloom {parent} --help` says what it takes")
    } else {
        "`fetchloom --help` lists commands and options".to_owned()
    };
    let mut error = Error::new(Kind::Usage, message);
    for (at, form) in usage.iter().flatten().enumerate() {
        error = error.detail(if at == 0 {
            format!("usage: {form}")
        } else {
            format!("       {form}")
        });
    }
    error.help(help)
}

/// what a command needs, from the part of each usage line after its options, without what is
/// optional there, as in `add needs <link>, <path> or -i <file>` and `path needs <name>`.
fn needs(command: &str, forms: &[String]) -> String {
    let wanted: Vec<String> = forms
        .iter()
        .map(|form| {
            let wanted = form
                .split_once("[options] ")
                .map_or(form.as_str(), |(_, wanted)| wanted);
            let mut kept = String::new();
            let mut optional = 0_usize;
            for c in wanted.chars() {
                match c {
                    '[' => optional += 1,
                    ']' => optional = optional.saturating_sub(1),
                    c if optional == 0 => kept.push(c),
                    _ => {}
                }
            }
            kept.trim().trim_end_matches("...").to_owned()
        })
        .collect();
    format!("{command} needs {}", crate::units::listed(&wanted, "or"))
}

/// the command line the person likely meant, with a wrong command or option corrected.
fn guess(err: &clap::Error, parent: Option<&str>) -> Option<String> {
    let raw = |kind| typed(err, kind).into_iter().next();
    match err.kind() {
        ErrorKind::InvalidSubcommand => {
            command_meant(&raw(ContextKind::InvalidSubcommand)?, parent)
        }
        ErrorKind::UnknownArgument => option_meant(&raw(ContextKind::InvalidArg)?, parent),
        _ => None,
    }
}

/// the long option whose name is nearest to `typed` in spelling, ignoring case, among those of
/// the command `parent` and those every command takes.
fn option_meant(typed: &str, parent: Option<&str>) -> Option<String> {
    let long = typed.strip_prefix("--")?.to_lowercase();
    let root = args::Args::command();
    let command = parent.map_or(Some(&root), |parent| {
        parent
            .split(' ')
            .try_fold(&root, |command, name| command.find_subcommand(name))
    })?;
    let own = parent
        .map(|_| command.get_arguments())
        .into_iter()
        .flatten();
    let every = root
        .get_arguments()
        .filter(|arg| parent.is_none() || arg.is_global_set());
    let names = own
        .chain(every)
        .filter(|arg| !arg.is_hide_set())
        .filter_map(clap::Arg::get_long);
    let option = format!("--{}", name::closest(&long, names)?);
    Some(super::meant(&[typed], &[&option], option.clone()))
}

/// the command nearest to `typed` in spelling, ignoring case, among those of `parent`, or the
/// whole command when `typed` names one a level down, as `verify` does `cache verify`.
fn command_meant(typed: &str, parent: Option<&str>) -> Option<String> {
    let root = args::Args::command();
    let holder = parent.map_or(Some(&root), |parent| {
        parent
            .split(' ')
            .try_fold(&root, |command, name| command.find_subcommand(name))
    })?;
    let lower = typed.to_lowercase();
    // what other tools call them, which no spelling reaches
    let other = match lower.as_str() {
        "rm" => Some("remove"),
        "ls" => Some("list"),
        "version" => Some("--version"),
        _ => None,
    };
    if let Some(meant) = other.filter(|_| parent.is_none()) {
        return Some(super::meant(
            &[typed],
            &[meant],
            format!("fetchloom {meant}"),
        ));
    }
    let names = holder
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(clap::Command::get_name);
    if let Some(best) = name::closest(&lower, names) {
        let instead = match parent {
            Some(parent) => format!("fetchloom {parent} {best}"),
            None => format!("fetchloom {best}"),
        };
        return Some(super::meant(&[typed], &[best], instead));
    }
    let nested = nested(&lower).filter(|_| parent.is_none())?;
    let meant: Vec<&str> = nested.split(' ').collect();
    Some(super::meant(
        &[typed],
        &meant,
        format!("fetchloom {nested}"),
    ))
}

/// an argument as a person types it: `--as <name>` is `--as`, and `--verbose...` is `--verbose`.
fn bare(arg: &str) -> &str {
    arg.split(' ').next().unwrap_or(arg).trim_end_matches("...")
}

fn misplaced_option(argument: &str, parent: Option<&str>) -> Option<String> {
    let long = argument.strip_prefix("--")?.split('=').next()?;
    let command = args::Args::command();
    let nested = command.get_subcommands().flat_map(|outer| {
        std::iter::once((outer, outer.get_name().to_owned())).chain(
            outer
                .get_subcommands()
                .map(move |inner| (inner, format!("{} {}", outer.get_name(), inner.get_name()))),
        )
    });
    let owners: Vec<_> = nested
        .filter_map(|(command, name)| {
            let option = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some(long))?;
            Some((command, name, option))
        })
        .collect();
    let (first, first_name, option) = owners.first()?;
    let names: Vec<_> = owners.iter().map(|(_, name, _)| name.as_str()).collect();
    let commands = crate::units::listed(&names, "and");
    let mut example = format!("fetchloom {first_name}");
    // a positional also counts when the command needs it or an option in its place
    let needed = |arg: &clap::Arg| {
        arg.is_required_set()
            || first.get_groups().any(|group| {
                group.is_required_set() && group.get_args().any(|id| id == arg.get_id())
            })
    };
    for arg in first.get_positionals().filter(|arg| needed(arg)) {
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
    // after a command that lacks it, the place was right and the command was not
    if let Some(parent) = parent.filter(|parent| !names.contains(parent)) {
        return Some(format!(
            "{parent} has no --{long}; it is an option of {commands}, as in {example}"
        ));
    }
    Some(format!(
        "--{long} is an option of {commands}, so it goes after the command, as in {example}"
    ))
}

fn first_line(err: &clap::Error) -> String {
    let text = err.to_string();
    let line = text.lines().next().unwrap_or_default();
    let mut line = line.strip_prefix("error: ").unwrap_or(line).to_owned();
    // clap quotes what was typed, which may be a link with a secret in its query
    for kind in [
        ContextKind::InvalidArg,
        ContextKind::InvalidValue,
        ContextKind::InvalidSubcommand,
    ] {
        for value in typed(err, kind) {
            let masked = net::mask_query(&value);
            if masked != value {
                line = line.replace(&value, &masked);
            }
        }
    }
    line
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
                "--force is an option of sync, export and get, so it goes after the command, as in fetchloom sync --force",
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
                &["fetchloom", "status", "iris"][..],
                "unexpected argument \"iris\"",
            ),
            (
                &["fetchloom", "status", "--", "-iris"],
                "unexpected argument \"-iris\"",
            ),
        ] {
            assert_eq!(problem(arguments)?.message, expected);
        }
        Ok(())
    }

    /// in one shape for every command: the command, what its usage says it needs, then the usage.
    #[test]
    fn a_missing_argument_is_named_as_what_the_command_needs()
    -> Result<(), Box<dyn std::error::Error>> {
        let table = [
            ("add", "add needs <link>, <path> or -i <file>"),
            ("get", "get needs <link>"),
            ("search", "search needs <words>"),
            ("drop", "drop needs <path>"),
            ("path", "path needs <name>"),
            ("completions", "completions needs <shell>"),
            ("auth login", "auth login needs <account>"),
            ("auth logout", "auth logout needs <account>"),
        ];
        for (command, expected) in table {
            let arguments: Vec<&str> = std::iter::once("fetchloom")
                .chain(command.split(' '))
                .collect();
            let error = problem(&arguments)?;
            assert_eq!(error.message, expected, "{command}");
            assert!(error.details[0].starts_with(&format!("usage: fetchloom {command} ")));
        }
        let root = args::Args::command();
        let mut commands: Vec<Vec<&str>> = root
            .get_subcommands()
            .map(|command| vec![command.get_name()])
            .collect();
        let mut refused = Vec::new();
        while let Some(path) = commands.pop() {
            let command = path
                .iter()
                .try_fold(&root, |command, name| command.find_subcommand(name))
                .ok_or("a listed command is not found")?;
            commands.extend(command.get_subcommands().map(|inner| {
                let mut deeper = path.clone();
                deeper.push(inner.get_name());
                deeper
            }));
            let arguments = std::iter::once("fetchloom").chain(path.iter().copied());
            if args::Args::command()
                .try_get_matches_from(arguments)
                .is_err_and(|err| err.kind() == ErrorKind::MissingRequiredArgument)
            {
                refused.push(path.join(" "));
            }
        }
        refused.sort_unstable();
        let mut named: Vec<&str> = table.iter().map(|(command, _)| *command).collect();
        named.sort_unstable();
        assert_eq!(
            refused, named,
            "every command refused without an argument is in the table"
        );
        Ok(())
    }

    #[test]
    fn a_secret_in_a_mistyped_link_stays_masked() -> Result<(), Box<dyn std::error::Error>> {
        let link = "https://example.org/file?token=AUDIT_CANARY";
        for arguments in [
            &["fetchloom", "status", link][..],
            &["fetchloom", link],
            &[
                "fetchloom",
                "get",
                "https://example.org/x",
                "--sha256",
                link,
            ],
            &["fetchloom", "--color", link, "sync"],
        ] {
            let error = problem(arguments)?;
            let shown = format!("{} {:?} {:?}", error.message, error.details, error.help);
            assert!(!shown.contains("AUDIT_CANARY"), "{arguments:?}: {shown}");
            assert!(shown.contains("token=***"), "{arguments:?}: {shown}");
        }
        Ok(())
    }

    #[test]
    fn command_typos_find_the_command_by_its_spelling() -> Result<(), Box<dyn std::error::Error>> {
        for (arguments, meant) in [
            (&["fetchloom", "seach", "iris"][..], "fetchloom search"),
            (&["fetchloom", "sarch"], "fetchloom search"),
            (&["fetchloom", "serach"], "fetchloom search"),
            (&["fetchloom", "saerch"], "fetchloom search"),
            (&["fetchloom", "Search", "iris"], "fetchloom search"),
            (&["fetchloom", "SEARCH"], "fetchloom search"),
            (&["fetchloom", "stauts"], "fetchloom status"),
            (&["fetchloom", "settigns"], "fetchloom settings"),
            (&["fetchloom", "auht"], "fetchloom auth"),
            (&["fetchloom", "pth"], "fetchloom path"),
            (&["fetchloom", "ad", "iris"], "fetchloom add"),
            (&["fetchloom", "Add"], "fetchloom add"),
            (&["fetchloom", "snyc"], "fetchloom sync"),
            (&["fetchloom", "cache", "gx"], "fetchloom cache gc"),
            (&["fetchloom", "VERIFY"], "fetchloom cache verify"),
            (&["fetchloom", "rm", "iris"], "fetchloom remove"),
            (&["fetchloom", "ls"], "fetchloom list"),
            (&["fetchloom", "version"], "fetchloom --version"),
        ] {
            assert_eq!(
                problem(arguments)?.help.as_deref(),
                Some(format!("did you mean `{meant}`?").as_str()),
                "{arguments:?}"
            );
        }
        assert_eq!(
            problem(&["fetchloom", "Install"])?.help.as_deref(),
            Some("`fetchloom add <name>` adds a dataset; `fetchloom sync` fills data/")
        );
        assert_eq!(
            problem(&["fetchloom", "banana"])?.help.as_deref(),
            Some("`fetchloom --help` lists commands and options")
        );
        Ok(())
    }

    #[test]
    fn option_typos_and_wrong_values_are_answered_alike() -> Result<(), Box<dyn std::error::Error>>
    {
        for (arguments, meant) in [
            (
                &["fetchloom", "add", "--yse", "zenodo:3608990"][..],
                "--yes",
            ),
            (&["fetchloom", "search", "iris", "--limt", "5"], "--limit"),
            (&["fetchloom", "search", "iris", "--limt=5"], "--limit"),
            (
                &["fetchloom", "search", "iris", "--sorce", "kaggle"],
                "--source",
            ),
            (&["fetchloom", "--ofline", "sync"], "--offline"),
            (&["fetchloom", "sync", "--Dry-Run"], "--dry-run"),
            (&["fetchloom", "--verison"], "--version"),
        ] {
            let error = problem(arguments)?;
            assert_eq!(
                error.help.as_deref(),
                Some(format!("did you mean `{meant}`?").as_str()),
                "{arguments:?}"
            );
        }
        for (arguments, message, help) in [
            (
                &["fetchloom", "search", "iris", "--limit", "50"][..],
                "--limit cannot be \"50\"",
                "use a whole number from 1 to 20",
            ),
            (
                &["fetchloom", "search", "iris", "--limit=0"],
                "--limit cannot be \"0\"",
                "use a whole number from 1 to 20",
            ),
            (
                &["fetchloom", "search", "iris", "--limit", "many"],
                "--limit cannot be \"many\"",
                "use a whole number from 1 to 20",
            ),
            (
                &[
                    "fetchloom",
                    "get",
                    "https://example.org/x",
                    "--sha256",
                    "abc",
                ],
                "--sha256 cannot be \"abc\"",
                "a SHA-256 is 64 hex digits",
            ),
        ] {
            let error = problem(arguments)?;
            assert_eq!(error.message, message, "{arguments:?}");
            assert_eq!(error.help.as_deref(), Some(help), "{arguments:?}");
        }
        Ok(())
    }

    #[test]
    fn init_says_no_setup_is_needed() -> Result<(), Box<dyn std::error::Error>> {
        let init = problem(&["fetchloom", "init"])?;
        assert_eq!(init.message, "there is no command called \"init\"");
        assert_eq!(
            init.help.as_deref(),
            Some("no init needed: `fetchloom add <link>` starts a project in this folder")
        );
        let new = problem(&["fetchloom", "new"])?;
        assert!(new.help.unwrap().starts_with("no setup needed: "));
        Ok(())
    }

    #[test]
    fn words_from_other_tools_point_to_the_command_meant() -> Result<(), Box<dyn std::error::Error>>
    {
        for (word, help) in [
            (
                "install",
                "`fetchloom add <name>` adds a dataset; `fetchloom sync` fills data/",
            ),
            (
                "download",
                "`fetchloom get <name>` downloads once; `fetchloom add <name>` pins it",
            ),
            (
                "fetch",
                "`fetchloom get <name>` downloads once; `fetchloom add <name>` pins it",
            ),
            (
                "pull",
                "`fetchloom sync` puts the files data.lock pins into data/",
            ),
            (
                "update",
                "`fetchloom lock --upgrade` looks for newer data, then `fetchloom sync`",
            ),
            (
                "upgrade",
                "`fetchloom lock --upgrade` looks for newer data, then `fetchloom sync`",
            ),
            (
                "delete",
                "`fetchloom remove <name>` drops a dataset from the project",
            ),
            (
                "uninstall",
                "`fetchloom remove <name>` drops a dataset from the project",
            ),
        ] {
            let error = problem(&["fetchloom", word])?;
            assert_eq!(
                error.message,
                format!("there is no command called \"{word}\"")
            );
            assert_eq!(error.help.as_deref(), Some(help), "{word}");
        }
        let status = problem(&["fetchloom", "auth", "status"])?;
        assert_eq!(status.message, "there is no command called \"status\"");
        assert_eq!(
            status.help.as_deref(),
            Some("`fetchloom auth` alone lists each account and whether it is signed in")
        );
        Ok(())
    }

    /// in either order, rather than only that the two cannot go together.
    #[test]
    fn force_with_a_folder_says_what_force_replaces() -> Result<(), Box<dyn std::error::Error>> {
        for arguments in [
            &["fetchloom", "get", "zenodo:1", "--dir", "a", "--force"][..],
            &["fetchloom", "get", "zenodo:1", "--force", "--dir", "a"],
        ] {
            let error = problem(arguments)?;
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(
                error.message,
                "--force replaces only the file -o names, so it cannot go with --dir"
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "leave out --force and give --dir an empty folder, or write one file with -o <file> --force"
                )
            );
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
                &["fetchloom", "status", "iris"],
                "`fetchloom status --help` says what it takes",
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
