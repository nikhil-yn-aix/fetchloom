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
--exclude choose which of its files to keep. an archive, such as a .tar shard,
is unpacked; a members table in data.toml keeps some of the files inside it,
and extract = false keeps it packed, as webdataset reads shards.

a fingerprint is a file's SHA-256 hash. to pin a dataset, fetchloom records
the fingerprint of each of its files in data.lock, so every machine gets the
same bytes. if a source changes a file, sync stops and says so.

the cache keeps each file once, under its fingerprint. a file in data/ is a
link to it where the disk allows, or a copy, so projects that use the same file
store it once. files in data/ are read-only by default. to edit a dataset's
files, set copy = true for it in data.toml.

a recipe is a dataset made from others by a command of yours, written with
from and run in data.toml. only `fetchloom build` runs it: it pins what the
command wrote in data.lock and places it in data/ like any dataset.

place = false keeps a dataset pinned in data.lock but out of data/, as for raw
data that only a recipe reads. a build takes its files from the cache.

a mirror is a folder or web server that keeps pinned files, such as a lab
disk. fetchloom asks the mirrors you name before the sources, and
`fetchloom push` copies a project's pinned files into a folder mirror.

commit data.toml and data.lock with your code, and data.lock.d once it is
there: each recipe's result and each big dataset keep their file lists in it.
leave data/ out. anyone who clones the project runs `fetchloom sync` to place
the pinned files. sync warns when it keeps edited or conflicting files.
`fetchloom status` shows what differs.

`fetchloom get` fetches a dataset once, with no list and no receipt, for a
quick look.";

/// `fetchloom help environment`: every variable fetchloom reads, at terminal size. the docs page
/// has the details; a test keeps the two naming the same variables.
const ENVIRONMENT: &str = "fetchloom environment

  FETCHLOOM_CACHE      the cache folder, instead of the system's cache folder
  FETCHLOOM_OFFLINE    true works offline, as --offline does
  FETCHLOOM_RESOURCES  lean, default or max: lean spends less memory and runs
                       fewer downloads at once, max runs more; FETCHLOOM_JOBS,
                       FETCHLOOM_MEMORY and the like still win over it
  FETCHLOOM_JOBS       how many files download at once, 1 to 256; 8 by default
  FETCHLOOM_HOST_JOBS  how many of those one site gets at once, 1 to 256;
                       4 by default
  FETCHLOOM_RATE       a cap on download speed in bytes a second, as 2.5M
  FETCHLOOM_RETRIES    how many more times a failed request is tried, 0 to 20;
                       4 by default. raise it if the network drops for minutes
  FETCHLOOM_TIMEOUT    seconds a site may send nothing before a try fails,
                       1 to 3600; 30 by default
  FETCHLOOM_ASK_ABOVE  a download bigger than this needs a yes, as 20G;
                       1G by default
  FETCHLOOM_MEMORY     memory for download buffers, as 512M; on linux a quarter
                       of what is free by default, from 256M to 4G; else 256M
  FETCHLOOM_MOST_FILES the most files one dataset may list; by default 1 for
                       each 2K of FETCHLOOM_MEMORY, at least 100000
  FETCHLOOM_ARCHIVE_FILES, FETCHLOOM_ARCHIVE_SIZE, FETCHLOOM_ARCHIVE_RATIO
                       the most entries, bytes and times its size one archive
                       unpacks to: 10000000, 16T and 1000 by default
  FETCHLOOM_THREADS    a cap on local worker threads, 1 to 256
  FETCHLOOM_SPLIT      false never splits a large download into parts, true
                       splits it from the start; unset splits a slow one
  FETCHLOOM_SETTINGS   the settings file, instead of settings.toml in
                       fetchloom's folder
  FETCHLOOM_NO_CONFIG  true leaves the settings file unread, as --no-config does
  FETCHLOOM_ALLOWED_ORIGINS
                       servers on your network that data.toml may name, as
                       http://lab.local:8080, separated by commas
  FETCHLOOM_S3_BUCKETS private S3 buckets your AWS keys may sign for, as
                       s3://lab-data, separated by commas
  FETCHLOOM_GCS_BUCKETS
                       private cloud storage buckets your google credentials
                       may sign in to, as gs://lab-data, separated by commas
  FETCHLOOM_AZURE_CONTAINERS
                       private azure containers your azure credentials may
                       sign in to, as azure:lab/raw, separated by commas
  FETCHLOOM_DRIVE_FOLDERS
                       private google drive folders and files your google
                       credentials may read, by id or sharing link,
                       separated by commas
  FETCHLOOM_MIRRORS    folders or web addresses to copy files from before
                       their sources, separated by commas
  FETCHLOOM_MIRRORS_ONLY
                       true asks the mirrors alone, never a source
  FETCHLOOM_PUSH_TO    the folder fetchloom push copies files into, unless
                       --to names one
  FETCHLOOM_BUILD_FLOOR
                       the least free space a build's step may leave on the
                       cache's disk, as 20G; 1G or 1% of that disk by default
  FETCHLOOM_LOG        a log filter such as fetchloom=debug, as -vv uses
  FETCHLOOM_TRACE      a file to write a timeline of the run to, for profiling
  FETCHLOOM_COLORS     other colors, as done=blue:accent=#d787ff
  FETCHLOOM_NO_HINTS   true drops the hint: and next: lines
  FETCHLOOM_ACCESSIBLE true asks on plain lines, takes a choice by its number
                       and draws no live progress, for screen readers
  FETCHLOOM_KEYRING    false keeps keys out of the system's credential store
  FETCHLOOM_BUILDING   set by fetchloom build for its steps: the project
                       a fetchloom inside a step may not lock

  HTTPS_PROXY, HTTP_PROXY, ALL_PROXY
                       the proxy for https links, for http links, or for both
                       when those two are unset, as http://proxy.lab:8080;
                       lowercase names work too
  NO_PROXY             hosts reached without the proxy, separated by commas

  NO_COLOR, CLICOLOR, CLICOLOR_FORCE, TERM=dumb   color, as most tools read them
  CI, AI_AGENT, coding agents' variables   no questions, color or animation

a settings file keeps cache, jobs, host_jobs, retries, timeout, ask_above,
memory, most_files, the archive_ settings, allowed_origins, s3_buckets,
gcs_buckets, azure_containers, drive_folders, mirrors, mirrors_only, push_to
and build_floor for every run, one per line, as jobs = 16. a variable wins over
the file.
fetchloom settings shows each value and where it came from.

keys are read where each source's own tools keep them: HF_TOKEN and the hugging
face token file, KAGGLE_API_TOKEN and ~/.kaggle, ZENODO_TOKEN, FIGSHARE_TOKEN,
OSF_TOKEN, DATAVERSE_API_TOKEN with DATAVERSE_URL, .netrc or the file NETRC
names, for S3 buckets in s3_buckets, AWS_ACCESS_KEY_ID with
AWS_SECRET_ACCESS_KEY, or the profile AWS_PROFILE names in ~/.aws, for cloud
storage buckets in gcs_buckets and drive items in drive_folders, the key file
that GOOGLE_APPLICATION_CREDENTIALS names, else the login that
gcloud auth application-default login saved, and for azure containers in
azure_containers, the SAS AZURE_STORAGE_SAS_TOKEN holds, else the key
AZURE_STORAGE_KEY holds for AZURE_STORAGE_ACCOUNT, else the az cli's login,
as AZURE_TOKEN_CREDENTIALS chooses. drive needs the google login made through
an oauth client of your own: see the google drive page.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/environment.html";

/// `fetchloom help exit-codes`: what each exit code means, for a script deciding what to do.
const EXIT_CODES: &str = "fetchloom exit codes

    0  success, even when sync kept edited files; status shows them
    1  unconfirmed or lock.outdated: say yes with --yes, or run fetchloom lock.
       data.not_ready: sync --strict or status --check found data not ready
    2  usage: a mistake on the command line
   10  reference.*, source.invalid: the link or id is wrong, or names nothing
   20  network.*, offline.missing: retry later, or go online
   30  integrity.mismatch: the bytes are not the ones data.lock pins
   40  auth.required: sign in, or accept the source's terms
   50  local.*, project.*: disk, permissions, a broken or changed project file
   60  build.needed: a recipe changed or is not built; it runs your code, so
       build it yourself. build.failed: a step failed. build.differs: a step
       built again made other files than data.lock pins
  130  interrupted

with --json, errors[0].kind names the exact cause and errors[0].fix, when
present, the command to run next.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/exit-codes.html";

/// `fetchloom help recipes`: how to write a recipe in data.toml, with an example to copy. the
/// docs page has the details; a test reads the example as data.toml.
const RECIPES: &str = "fetchloom recipes

a recipe makes a dataset from others with a command of yours, such as a
script that cleans a raw table. write it in data.toml with from and run in
place of ref:

  [datasets.iris]
  ref = \"hf:datasets/scikit-learn/iris\"
  select = \"Iris.csv\"

  [datasets.clean]
  from = [\"iris\"]
  run = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]
  code = [\"clean.py\"]

from names the datasets it reads. run is the program and its words, which no
shell reads: {iris} stands for a folder holding a copy of iris's files, and
{out} for the folder the step writes its result in. code lists the files of
the project the result depends on, such as the script, and
env = { NAME = \"value\" } sets variables for the step alone.

only `fetchloom build` runs a step, as you, with your files and keys, so read
the commands of a project you cloned with `fetchloom build --dry-run` first.
the build records what the step wrote, each file with its SHA-256, in
data.lock, keeps the file list in data.lock.d and places the files in
data/clean like any dataset: commit both.
`fetchloom status` says when a recipe changed since its build.

place = false on iris keeps raw data that only recipes read out of data/.

more: https://nikhil-yn-aix.github.io/fetchloom/user/recipes.html";

/// help topics that are not commands, each with its text.
const TOPICS: [(&str, &str); 4] = [
    ("concepts", CONCEPTS),
    ("environment", ENVIRONMENT),
    ("exit-codes", EXIT_CODES),
    ("recipes", RECIPES),
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
        let outcome = crate::output::help(text, args::color());
        return finish(context, None, outcome, started);
    }
    let line = std::iter::once("fetchloom")
        .chain(topic.iter().map(String::as_str))
        .chain(std::iter::once("--help"));
    match args::command().try_get_matches_from(line) {
        Err(help) if help.kind() == ErrorKind::DisplayHelp => {
            let shown = examples_first(&help.render().to_string());
            let outcome = crate::output::help(&shown, args::color());
            finish(context, None, outcome, started)
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
        Some(best) => {
            let typed: Vec<&str> = topic.iter().map(String::as_str).collect();
            let meant: Vec<&str> = best.split(' ').collect();
            let shown = super::meant(&typed, &meant, format!("fetchloom help {best}"));
            error.help(format!("did you mean `{shown}`?"))
        }
        None => error
            .detail("topics: concepts, recipes, environment, exit-codes")
            .detail("or a command, such as add or cache gc")
            .help("`fetchloom --help` lists the commands"),
    }
}

/// the commands `-h` shows of fetchloom's own, the ones used most. `--help` lists them all.
const COMMON_COMMANDS: [&str; 8] = [
    "add", "remove", "sync", "status", "list", "path", "get", "search",
];

/// the options `-h` shows of fetchloom's own, by their ids. a command's `-h` shows only the
/// options of its own, so none every command takes.
const COMMON_OPTIONS: [&str; 7] = [
    "quiet",
    "verbose",
    "json",
    "offline",
    "yes",
    "directory",
    "version",
];

/// what `-h` shows: for fetchloom itself what it does, the commands and options used most, and
/// for a command `topic` names its usage, examples and own options, each then saying where the
/// rest is, which `--help` shows. none when `topic` names no command.
pub(super) fn short(topic: &[String]) -> Option<String> {
    let mut root = args::command();
    let global: Vec<String> = root
        .get_arguments()
        .filter(|arg| arg.is_global_set() || arg.get_id() == "version")
        .map(|arg| arg.get_id().to_string())
        .filter(|id| !(topic.is_empty() && COMMON_OPTIONS.contains(&id.as_str())))
        .collect();
    for id in &global {
        root = root.mut_arg(id, |arg| arg.hide(true));
    }
    let pointer = if topic.is_empty() {
        let rare: Vec<String> = root
            .get_subcommands()
            .map(|command| command.get_name().to_owned())
            .filter(|name| !COMMON_COMMANDS.contains(&name.as_str()))
            .collect();
        for name in &rare {
            root = root.mut_subcommand(name, |command| command.hide(true));
        }
        let about = args::INTRO.lines().next().unwrap_or_default().to_owned();
        root = root.about(about).after_help("");
        "`fetchloom --help` shows every command and option".to_owned()
    } else {
        // the examples, without what the command leaves on disk or its page
        root = mut_command(root, topic, &|command| {
            let after = command
                .get_after_help()
                .map(ToString::to_string)
                .unwrap_or_default();
            let examples = after
                .split("\n\n")
                .find(|part| part.starts_with("examples:"))
                .unwrap_or_default()
                .to_owned();
            command.after_help(examples)
        });
        format!(
            "`fetchloom {} --help` also lists the options every command takes",
            topic.join(" ")
        )
    };
    let line = std::iter::once("fetchloom")
        .chain(topic.iter().map(String::as_str))
        .chain(std::iter::once("--help"));
    match root.try_get_matches_from(line) {
        Err(help) if help.kind() == ErrorKind::DisplayHelp => {
            let shown = examples_first(&help.render().to_string());
            Some(format!("{}\n\n{pointer}", shown.trim_end()))
        }
        _ => None,
    }
}

/// `root` with `change` made to the command the names in `path` lead to, as `cache gc`.
fn mut_command(
    root: clap::Command,
    path: &[String],
    change: &dyn Fn(clap::Command) -> clap::Command,
) -> clap::Command {
    match path {
        [] => change(root),
        [first, rest @ ..] => root.mut_subcommand(first, |inner| mut_command(inner, rest, change)),
    }
}

/// a command's help with its examples right below its usage, before the long list of options,
/// so a person reads the commands to copy first.
pub(super) fn examples_first(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|line| *line == "examples:") else {
        return text.to_owned();
    };
    let end = lines[start..]
        .iter()
        .position(|line| line.starts_with("on disk:") || line.starts_with("docs:"))
        .map_or(lines.len(), |at| start + at);
    let Some(usage) = lines.iter().position(|line| line.starts_with("usage:")) else {
        return text.to_owned();
    };
    let Some(below) = lines[usage..]
        .iter()
        .position(|line| line.is_empty())
        .map(|at| usage + at)
        .filter(|below| *below < start)
    else {
        return text.to_owned();
    };
    let mut examples = lines[start..end].to_vec();
    while examples.last().is_some_and(|line| line.is_empty()) {
        examples.pop();
    }
    lines[..=below]
        .iter()
        .chain(&examples)
        .chain(std::iter::once(&""))
        .chain(&lines[below + 1..start])
        .chain(&lines[end..])
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{CONCEPTS, ENVIRONMENT, EXIT_CODES, TOPICS, args, arguments};

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
        let folder = own.iter().find(|option| option.name == "--dir").unwrap();
        assert_eq!(folder.short.as_deref(), Some("-d"));
        assert_eq!(folder.value.as_deref(), Some("folder"));
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

    /// a person whose download fails behind a company proxy looks here for where the proxy is
    /// set, so the topic and the docs page both name the proxy variables.
    #[test]
    fn the_environment_topic_names_the_proxy_variables() {
        let docs = include_str!("../../docs/user/reference/environment.md");
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY"] {
            assert!(ENVIRONMENT.contains(name), "{name}");
            assert!(docs.contains(&format!("`{name}`")), "{name}");
        }
    }

    /// each code the reference lists, with each kind it gives the code, by its name or as one of
    /// a family such as `network.*`.
    #[test]
    fn the_exit_codes_topic_lists_every_documented_code_and_kind() {
        let docs = include_str!("../../docs/user/reference/exit-codes.md");
        for line in docs.lines().filter(|line| line.starts_with("| ")) {
            let Some(code) = line[2..]
                .split(' ')
                .next()
                .filter(|code| code.parse::<u8>().is_ok())
            else {
                continue;
            };
            let said: String = EXIT_CODES
                .lines()
                .skip_while(|topic| !topic.trim_start().starts_with(&format!("{code} ")))
                .enumerate()
                .take_while(|(at, topic)| *at == 0 || topic.starts_with("       "))
                .map(|(_, topic)| topic)
                .collect();
            assert!(!said.is_empty(), "exit {code}");
            for kind in line.split('`').skip(1).step_by(2) {
                let family = kind
                    .split_once('.')
                    .map(|(family, _)| format!("{family}.*"));
                assert!(
                    said.contains(kind) || family.is_some_and(|family| said.contains(&family)),
                    "exit {code} lacks {kind}: {said}"
                );
            }
        }
    }

    /// the topics say "pin" with its meaning and name things as the help screens do: a site,
    /// not an initial host, and yes to a question, not an approval.
    #[test]
    fn topics_name_things_plainly() {
        let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat(CONCEPTS).contains(
                "to pin a dataset, fetchloom records the fingerprint of each of its files in data.lock"
            ),
            "{CONCEPTS}"
        );
        assert!(flat(ENVIRONMENT).contains("how many of those one site gets at once"));
        assert!(flat(EXIT_CODES).contains("say yes with --yes"));
        for (name, text) in TOPICS {
            for word in ["initial host", "approve"] {
                assert!(!text.contains(word), "{name} says {word}");
            }
        }
    }

    /// every topic is named where a person looks for one: the argument of `help --help`, the
    /// help of a topic that does not exist, and the welcome.
    #[test]
    fn every_topic_is_named_where_topics_are_listed() {
        let command = args::Args::command();
        let help = command.find_subcommand("help").unwrap();
        let argument = help
            .get_arguments()
            .find(|arg| arg.is_positional())
            .unwrap();
        let argument = argument
            .get_help()
            .map(ToString::to_string)
            .unwrap_or_default();
        let unknown = super::unknown(&["nothing".to_owned()]);
        let details = unknown.details.join(" ");
        for (name, _) in TOPICS {
            assert!(argument.contains(name), "{argument} lacks {name}");
            assert!(details.contains(name), "{details} lacks {name}");
            assert!(args::INTRO.contains(name), "the welcome lacks {name}");
        }
        let lines = unknown.details.iter().map(AsRef::as_ref);
        for line in lines.chain(unknown.help.as_deref()) {
            assert!(line.chars().count() <= 56, "{line}");
        }
    }

    /// a command's help puts its examples right below its usage and before its options, and
    /// keeps every line it had.
    #[test]
    fn a_command_help_shows_its_examples_before_its_options() {
        let rendered = args::command()
            .try_get_matches_from(["fetchloom", "add", "--help"])
            .unwrap_err()
            .render()
            .to_string();
        let shown = super::examples_first(&rendered);
        let lines: Vec<&str> = shown.lines().collect();
        let at = |heading: &str| lines.iter().position(|line| *line == heading).unwrap();
        let usage = lines
            .iter()
            .position(|line| line.starts_with("usage:"))
            .unwrap();
        assert_eq!(lines[usage + 3], "", "{shown}");
        assert_eq!(at("examples:"), usage + 4, "{shown}");
        assert!(at("examples:") < at("arguments:") && at("arguments:") < at("options:"));
        assert!(lines.last().unwrap().starts_with("docs: "), "{shown}");
        let mut before: Vec<&str> = rendered.lines().filter(|line| !line.is_empty()).collect();
        let mut after: Vec<&str> = lines.into_iter().filter(|line| !line.is_empty()).collect();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after);
    }

    #[test]
    fn topics_fit_a_terminal() {
        for (name, text) in TOPICS {
            for line in text.lines() {
                assert!(line.chars().count() <= 80, "{name}: {line}");
            }
        }
    }

    /// a build keeps every recipe's file list in data.lock.d, even a result of one file, so the
    /// concepts ask to commit it for recipes too, not only for a big dataset.
    #[test]
    fn the_concepts_say_recipes_keep_their_lists_in_data_lock_d() {
        let flat = CONCEPTS.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("and data.lock.d once it is there: each recipe's result and each big dataset keep their file lists in it."),
            "{CONCEPTS}"
        );
    }

    /// `help recipes` shows a data.toml with a recipe, which fetchloom reads as written, and
    /// `build --help` points at it.
    #[test]
    fn the_recipes_topic_shows_a_recipe_that_reads() {
        let recipes = super::topic_text(&["recipes".to_owned()]).unwrap_or_default();
        let example = recipes
            .lines()
            .skip_while(|line| !line.starts_with("  [datasets."))
            .take_while(|line| line.is_empty() || line.starts_with("  "))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let manifest = crate::project::manifest::Manifest::parse(&example).unwrap();
        let recipe = manifest.datasets["clean"].recipe().unwrap();
        assert_eq!(recipe.from, ["iris"]);
        assert!(manifest.datasets["iris"].source().is_some(), "{example}");
        let command = args::Args::command();
        let build = command.find_subcommand("build").unwrap();
        let after = build.get_after_help().map(ToString::to_string);
        assert!(
            after
                .unwrap_or_default()
                .contains("`fetchloom help recipes`"),
            "build --help points at the topic"
        );
    }
}
