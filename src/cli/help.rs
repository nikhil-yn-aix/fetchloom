//! `fetchloom help`: the ideas behind fetchloom, or the help of one command. like `--help`, it
//! prints text for people. `fetchloom help --json` instead describes every command, so a script
//! or an agent learns all of them in one call.

use std::process::ExitCode;
use std::time::Instant;

use clap::CommandFactory;
use clap::error::ErrorKind;
use serde::Serialize;

use super::{Context, args, finish};
use crate::error::{Error, Kind};
use crate::name;

/// `fetchloom help concepts`: what a beginner needs to know, in the same kitchen as the first run.
const CONCEPTS: &str = "fetchloom concepts

fetchloom keeps the data a project reads exact, and the same on every machine.
it works like a kitchen:

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by projects using it
  sync, restocking: fill data/ from the receipt

a dataset is one entry in data.toml: a name, such as iris, and a reference to
where it is published, such as zenodo:3608990, a DOI or a link. --select and
--exclude choose which of its files to keep.

a fingerprint is a file's SHA-256 hash. data.lock records one for every file,
so every machine gets the same bytes. when a source changes a file, sync stops
and says so instead of using it.

the cache keeps each file once, under its fingerprint. data/ links to it where
the disk allows, so projects that use the same file store it once. files in
data/ are read-only by default.

commit data.toml and data.lock with your code, and leave data/ out. anyone who
clones the project runs `fetchloom sync` to place the pinned files. sync warns
when it keeps edited or conflicting files. `fetchloom status` shows what
differs.

`fetchloom get` fetches a dataset once, with no list and no receipt, for a
quick look.";

/// `fetchloom help environment`: every variable fetchloom reads, at terminal size. the docs page
/// has the details; a test keeps the two naming the same variables.
const ENVIRONMENT: &str = "fetchloom environment

  FETCHLOOM_CACHE     the cache folder, instead of the system's cache folder
  FETCHLOOM_OFFLINE   1 works offline, as --offline does
  FETCHLOOM_JOBS      how many files download at once, 1 to 256; 8 by default.
                      one host still gets at most 4 at once
  FETCHLOOM_RATE      a cap on download speed in bytes a second, as 500K or 2.5M
  FETCHLOOM_THREADS   a cap on local worker threads, 1 to 256
  FETCHLOOM_SPLIT     0 never splits a slow download into parts, 1 always does
  FETCHLOOM_LOG       a log filter such as fetchloom=debug, as -vv uses
  FETCHLOOM_TRACE     a file to write a timeline of the run to, for profiling
  FETCHLOOM_COLORS    other colors, as done=blue:accent=#d787ff
  FETCHLOOM_NO_HINTS  1 drops the hint: and next: lines
  FETCHLOOM_KEYRING   0 keeps keys out of the system's credential store

  NO_COLOR, CLICOLOR, CLICOLOR_FORCE, TERM=dumb   color, as most tools read them
  CI, AI_AGENT, coding agents' variables   no questions, color or animation

keys are read where each source's own tools keep them: HF_TOKEN and the hugging
face token file, KAGGLE_API_TOKEN and ~/.kaggle, ZENODO_TOKEN, FIGSHARE_TOKEN,
OSF_TOKEN, DATAVERSE_API_TOKEN with DATAVERSE_URL, and .netrc or the file NETRC
names.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/environment.html";

/// `fetchloom help exit-codes`: what each exit code means, for a script deciding what to do.
const EXIT_CODES: &str = "fetchloom exit codes

    0  success, even when sync kept edited files; status shows them
    1  unconfirmed or lock.outdated: approve with --yes, or run fetchloom lock
    2  usage: a mistake on the command line
   10  reference.*, source.invalid: the link or id is wrong, or names nothing
   20  network.*, offline.missing: retry later, or go online
   30  integrity.mismatch: the bytes are not the ones data.lock pins
   40  auth.required: sign in, or accept the source's terms
   50  local.*, project.invalid: disk, permissions, or a broken project file
  130  interrupted

with --json, errors[0].kind names the exact cause and errors[0].fix, when
present, the command to run next.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/exit-codes.html";

/// help topics that are not commands, each with its text.
const TOPICS: [(&str, &str); 3] = [
    ("concepts", CONCEPTS),
    ("environment", ENVIRONMENT),
    ("exit-codes", EXIT_CODES),
];

fn topic_text(topic: &[String]) -> Option<&'static str> {
    match topic {
        [only] => TOPICS
            .iter()
            .find(|(name, _)| name == only)
            .map(|(_, text)| *text),
        _ => None,
    }
}

/// prints the topic, or the help of the command `topic` names. an unknown topic is a usage error
/// that names no command, since help is never one.
pub(super) fn show(context: &Context, topic: &[String], started: Instant) -> ExitCode {
    if context.output.json() && topic_text(topic).is_none() {
        let asked = topic.join(" ");
        let commands: Vec<Described> = described(&args::Args::command(), "")
            .into_iter()
            .filter(|command| asked.is_empty() || command.name == asked)
            .collect();
        if commands.is_empty() {
            return finish::<()>(context, None, Err(unknown(topic)), started);
        }
        let options = arguments(&args::Args::command(), true);
        return finish(
            context,
            Some("help"),
            Ok(Tree { options, commands }),
            started,
        );
    }
    if let Some(text) = topic_text(topic) {
        // under --json, stdout is one document, so a text topic arrives as one too
        if context.output.json() {
            return finish(
                context,
                Some("help"),
                Ok(Topic {
                    topic: topic.join(" "),
                    text,
                }),
                started,
            );
        }
        crate::output::help(text, args::color());
        return ExitCode::SUCCESS;
    }
    let line = std::iter::once("fetchloom")
        .chain(topic.iter().map(String::as_str))
        .chain(std::iter::once("--help"));
    match args::command().try_get_matches_from(line) {
        Err(help) if help.kind() == ErrorKind::DisplayHelp => {
            crate::output::help(&help.render().to_string(), args::color());
            ExitCode::SUCCESS
        }
        _ => finish::<()>(context, None, Err(unknown(topic)), started),
    }
}

/// what `fetchloom help <topic> --json` returns for a topic that is text, such as concepts.
#[derive(Serialize)]
struct Topic {
    topic: String,
    text: &'static str,
}

/// what `fetchloom help --json` returns.
#[derive(Serialize)]
struct Tree {
    /// the options fetchloom itself takes: those every command takes, and `--version`
    options: Vec<Argument>,
    commands: Vec<Described>,
}

/// one command as a script needs it: its name, what it does, how to call it and its examples.
#[derive(Serialize)]
struct Described {
    name: String,
    about: String,
    usage: String,
    arguments: Vec<Argument>,
    examples: Vec<String>,
}

#[derive(Serialize)]
struct Argument {
    /// `--select`, `-C` for an option with no long spelling, or `<link>` for a positional argument
    name: String,
    short: Option<String>,
    value: Option<String>,
    help: String,
    required: bool,
    repeats: bool,
}

/// every command under `command`, depth first, named the way people type them.
fn described(command: &clap::Command, parent: &str) -> Vec<Described> {
    let mut found = Vec::new();
    for inner in command
        .get_subcommands()
        .filter(|inner| !inner.is_hide_set())
    {
        let name = format!("{parent}{}", inner.get_name()).trim().to_owned();
        let after = inner
            .get_after_help()
            .map(ToString::to_string)
            .unwrap_or_default();
        let examples = after
            .split_once("examples:\n")
            .map(|(_, rest)| {
                rest.lines()
                    .take_while(|line| line.starts_with("  "))
                    .map(|line| line.trim().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let arguments = arguments(inner, false);
        found.push(Described {
            usage: {
                let usage = inner.clone().render_usage().to_string();
                usage.trim().trim_start_matches("Usage:").trim().to_owned()
            },
            about: inner
                .get_about()
                .map(ToString::to_string)
                .unwrap_or_default(),
            name: name.clone(),
            arguments,
            examples,
        });
        found.extend(described(inner, &format!("{name} ")));
    }
    found
}

/// `command`'s own arguments, or with `top` the options fetchloom itself takes: those every
/// command takes, and `--version`.
fn arguments(command: &clap::Command, top: bool) -> Vec<Argument> {
    command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .filter(|arg| {
            top || !arg.is_global_set() && !matches!(arg.get_id().as_str(), "help" | "version")
        })
        .map(|arg| {
            let value = arg
                .get_value_names()
                .and_then(|names| names.first())
                .map(ToString::to_string);
            let repeats = matches!(
                arg.get_action(),
                clap::ArgAction::Append | clap::ArgAction::Count
            ) || arg
                .get_num_args()
                .is_some_and(|range| range.max_values() > 1);
            let short = arg.get_short().map(|short| format!("-{short}"));
            // an option without a long spelling, such as -C, is named by its short one
            let name = if arg.is_positional() {
                format!(
                    "<{}>",
                    value.clone().unwrap_or_else(|| arg.get_id().to_string())
                )
            } else {
                arg.get_long()
                    .map(|long| format!("--{long}"))
                    .or_else(|| short.clone())
                    .unwrap_or_else(|| arg.get_id().to_string())
            };
            Argument {
                name,
                short,
                value: value.filter(|_| !arg.is_positional() && arg.get_action().takes_values()),
                help: arg.get_help().map(ToString::to_string).unwrap_or_default(),
                required: arg.is_required_set(),
                repeats,
            }
        })
        .collect()
}

/// the error for a topic that is neither `concepts` nor a command, suggesting the closest one.
fn unknown(topic: &[String]) -> Error {
    let asked = topic.join(" ");
    let shown = crate::net::mask_query(&asked);
    let mut topics: Vec<String> = TOPICS.iter().map(|(name, _)| (*name).to_owned()).collect();
    for command in args::Args::command()
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        topics.push(command.get_name().to_owned());
        for inner in command
            .get_subcommands()
            .filter(|inner| !inner.is_hide_set())
        {
            topics.push(format!("{} {}", command.get_name(), inner.get_name()));
        }
    }
    let error = Error::new(
        Kind::Usage,
        format!("there is no help topic called \"{shown}\""),
    );
    match name::closest(&asked, topics.iter().map(String::as_str)) {
        Some(best) => error.help(format!("did you mean `fetchloom help {best}`?")),
        None => error.help("`fetchloom help concepts` explains the ideas, `fetchloom help environment` and `fetchloom help exit-codes` list settings and codes, and `fetchloom --help` lists the commands"),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{ENVIRONMENT, EXIT_CODES, TOPICS, args, arguments};

    #[test]
    fn a_short_only_option_keeps_its_spelling_and_its_value() {
        let global = arguments(&args::Args::command(), true);
        let directory = global
            .iter()
            .find(|option| option.short.as_deref() == Some("-C"))
            .unwrap();
        assert_eq!(directory.name, "-C");
        assert_eq!(directory.value.as_deref(), Some("dir"));
        let command = args::Args::command();
        let get = command.find_subcommand("get").unwrap();
        let own = arguments(get, false);
        let output = own.iter().find(|option| option.name == "--output").unwrap();
        assert_eq!(output.short.as_deref(), Some("-o"));
        assert_eq!(output.value.as_deref(), Some("dir"));
        let links = own
            .iter()
            .find(|argument| argument.name == "<link>")
            .unwrap();
        assert_eq!(links.short, None);
        assert_eq!(links.value, None);
    }

    #[test]
    fn the_help_document_lists_the_version_option() {
        let options = arguments(&args::Args::command(), true);
        let version = options
            .iter()
            .find(|option| option.name == "--version")
            .unwrap();
        assert_eq!(version.short.as_deref(), Some("-V"));
        assert_eq!(version.value, None);
        for global in ["--json", "--help", "-C"] {
            assert!(
                options.iter().any(|option| option.name == global),
                "{global}"
            );
        }
    }

    /// the words in `text` shaped like fetchloom's own variables.
    fn variables(text: &str) -> std::collections::BTreeSet<&str> {
        text.split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .filter(|word| word.starts_with("FETCHLOOM_") && word.len() > "FETCHLOOM_".len())
            .collect()
    }

    #[test]
    fn the_environment_topic_names_every_variable_the_docs_do() {
        let docs = include_str!("../../docs/user/reference/environment.md");
        assert_eq!(variables(ENVIRONMENT), variables(docs));
    }

    #[test]
    fn the_exit_codes_topic_lists_every_documented_code() {
        let docs = include_str!("../../docs/user/reference/exit-codes.md");
        for line in docs.lines().filter(|line| line.starts_with("| ")) {
            let Some(code) = line[2..]
                .split(' ')
                .next()
                .filter(|code| code.parse::<u8>().is_ok())
            else {
                continue;
            };
            assert!(
                EXIT_CODES
                    .lines()
                    .any(|topic| topic.trim_start().starts_with(&format!("{code} "))),
                "exit {code}"
            );
        }
    }

    #[test]
    fn topics_fit_a_terminal() {
        for (name, text) in TOPICS {
            for line in text.lines() {
                assert!(line.chars().count() <= 80, "{name}: {line}");
            }
        }
    }
}
