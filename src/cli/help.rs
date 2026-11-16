//! `fetchloom help`: the ideas behind fetchloom, or one command's help, as text for people.
//! `fetchloom help --json` instead describes every command, so a script learns them in one call.

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

a mirror is a folder, web server or S3 bucket that keeps pinned files, such
as a lab disk. fetchloom asks the mirrors you name before the sources, and
`fetchloom push` copies a project's pinned files into a folder or bucket.

commit data.toml and data.lock with your code, and data.lock.d once it is
there: each big dataset keeps its file list in it. leave data/ out. anyone
who clones the project runs `fetchloom sync` to place the pinned files.
sync warns when it keeps edited or conflicting files.
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
                       4 by default. raise it if the network drops for minutes.
                       with no network a run first waits for it, up to 5 min
                       at a terminal and 30 s elsewhere; 0 fails at once
  FETCHLOOM_TIMEOUT    how long a site may send nothing, or less than 1K, before
                       a try fails, as 30s or 2min, up to 1h; 30s by default
  FETCHLOOM_ASK_ABOVE  a download bigger than this needs a yes, as 20G;
                       10G by default
  FETCHLOOM_MEMORY     memory for download buffers, as 512M; a quarter of what
                       is free by default, from 256M to 4G
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
  FETCHLOOM_MIRRORS    folders, web addresses or S3 buckets, as
                       s3://lab-data/fetchloom, to copy files from before
                       their sources, separated by commas
  FETCHLOOM_MIRRORS_ONLY
                       true asks the mirrors alone, never a source
  FETCHLOOM_PUSH_TO    the folder or S3 bucket fetchloom push copies files
                       into, unless --to names one
  FETCHLOOM_CACHE_BUDGET
                       how much the cache's files may hold, as 200G: a sync
                       that would download past it asks first; none by default
  FETCHLOOM_LOG        a log filter such as fetchloom=debug, as -vv uses
  FETCHLOOM_TRACE      a file to write a timeline of the run to, for profiling
  FETCHLOOM_COLORS     other colors, as done=blue:accent=#d787ff
  FETCHLOOM_NO_HINTS   true leaves out the hint: and next: lines
  FETCHLOOM_ACCESSIBLE true asks on plain lines, takes a choice by its number
                       and draws no live progress, for screen readers
  FETCHLOOM_KEYRING    false keeps keys out of the system's credential store

  HTTPS_PROXY, HTTP_PROXY, ALL_PROXY
                       the proxy for https links, for http links, or for both
                       when those two are unset, as http://proxy.lab:8080;
                       lowercase names work too. a name this machine cannot
                       look up goes to the proxy, unless it looks like a
                       private network's, as one word or metadata.internal
                       does: list such a server in allowed_origins
  NO_PROXY             hosts reached without the proxy, separated by commas
  HF_ENDPOINT          a mirror of huggingface.co, as https://hf-mirror.com,
                       that every request for huggingface.co goes to
                       instead; hugging face's token never goes there

  NO_COLOR, CLICOLOR, CLICOLOR_FORCE, TERM=dumb   color, as most tools read them
  CI, AI_AGENT, coding agents' variables   no questions, color or animation

a settings file keeps cache, resources, jobs, host_jobs, retries, timeout,
ask_above, memory, most_files, the archive_ settings, allowed_origins,
s3_buckets, gcs_buckets, azure_containers, drive_folders, mirrors,
mirrors_only, push_to, cache_budget and rate for every run, one per line,
as jobs = 16. a variable wins over the file.
fetchloom settings shows each value and where it came from, and
fetchloom settings set jobs 16 saves one.

keys are read as each source's own tools read them, and a variable wins over
a key fetchloom auth login saved:

  HF_TOKEN, HF_TOKEN_PATH, HF_HOME
                       hugging face, else the token file the hf command saved
  KAGGLE_API_TOKEN, KAGGLE_USERNAME, KAGGLE_KEY, KAGGLE_CONFIG_DIR
                       kaggle, else the kaggle.json the kaggle cli reads
  ZENODO_TOKEN, FIGSHARE_TOKEN, OSF_TOKEN, DRYAD_TOKEN, GH_TOKEN, GITHUB_TOKEN,
  GITLAB_TOKEN, S2_API_KEY
                       the key of the source each names, S2 for semantic
                       scholar
  DATAVERSE_API_TOKEN  dataverse, for the installation DATAVERSE_URL names
  NETRC                the netrc file with logins for other sites, instead of
                       ~/.netrc
  AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, AWS_SESSION_TOKEN, AWS_PROFILE,
  AWS_DEFAULT_PROFILE, AWS_CONFIG_FILE, AWS_SHARED_CREDENTIALS_FILE, AWS_REGION,
  AWS_DEFAULT_REGION, AWS_ENDPOINT_URL, AWS_ENDPOINT_URL_S3,
  AWS_IGNORE_CONFIGURED_ENDPOINT_URLS
                       S3 buckets in s3_buckets and S3 mirrors, as the aws cli
                       reads them. with an endpoint set, a mirror written as
                       s3://bucket/prefix is at that service
  GOOGLE_APPLICATION_CREDENTIALS, CLOUDSDK_CONFIG
                       cloud storage buckets in gcs_buckets and drive items in
                       drive_folders: the key file, else the login that
                       gcloud auth application-default login saved
  AZURE_STORAGE_SAS_TOKEN, AZURE_STORAGE_KEY, AZURE_STORAGE_ACCOUNT,
  AZURE_TOKEN_CREDENTIALS, AZURE_CLIENT_ID, AZURE_CONFIG_DIR
                       azure containers in azure_containers: a SAS, else the
                       key, else the az cli's login or a managed identity, as
                       AZURE_TOKEN_CREDENTIALS chooses
  AZURE_CLIENT_SECRET, AZURE_CLIENT_CERTIFICATE_PATH,
  AZURE_FEDERATED_TOKEN_FILE
                       a service principal's secret or certificate, or a
                       workload identity's token, which fetchloom does not
                       sign in with; under AZURE_TOKEN_CREDENTIALS=prod, one
                       set stops the command, since azure's libraries would
                       sign in with it first

drive needs the google login made through an oauth client of your own: see the
google drive page.

  SSL_CERT_FILE, SSL_CERT_DIR
                       on linux, certificates to trust instead of the system's
  HOME, USERPROFILE, APPDATA, LOCALAPPDATA, XDG_CONFIG_HOME, XDG_CACHE_HOME,
  XDG_STATE_HOME       where the home, settings, cache and state folders are
  TERM_PROGRAM, COLORTERM, LANG and other terminal variables
                       unicode, links and the color theme, as the page below
                       lists them

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/environment/";

/// `fetchloom help exit-codes`: what each exit code means, for a script deciding what to do.
const EXIT_CODES: &str = "fetchloom exit codes

    0  success, even when sync kept edited files; status shows them
    1  unconfirmed or lock.outdated: say yes with --yes, or run fetchloom lock;
       from audit --check, a source lists a newer version or changed.
       data.not_ready: sync --strict or status --check found data not ready,
       or audit --check found a pinned file served nowhere
    2  usage: a mistake on the command line
   10  reference.*, source.invalid: the link or id is wrong, or names nothing.
       network.refused: the link leads where fetchloom will not go
   20  other network.* kinds, offline.missing: retry later, or go online
   30  integrity.mismatch: the bytes are not the ones data.lock pins
   40  auth.required: sign in, or accept the source's terms
   50  local.*, project.*: disk, permissions, a broken or changed project file
  130  interrupted

with --json, errors[0].kind names the exact cause and errors[0].fix, when
present, the command to run next.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/exit-codes/";

/// `fetchloom help agents`: what an ai agent or a script needs to run fetchloom without a person,
/// offline. a test holds its kinds and dry runs to the code.
const AGENTS: &str = "fetchloom for agents and scripts

an ai agent or a script can run every fetchloom command with nobody to
answer it. the skill skills/fetchloom/SKILL.md, in fetchloom's repository,
has the usual tasks.

--json, on any command but completions, prints one JSON document on stdout:
  schema    1
  command   the command it ran, such as sync or cache gc
  status    ok or error
  result    what the command found or did, in fields of its own
  warnings  each with kind, message, details and help
  errors    none on success, else one, with kind, message, details, help
            and fix
`fetchloom help --json` describes every command and its options.

errors[0].kind, and the exit code it gives:
    1  unconfirmed, lock.outdated, data.not_ready
    2  usage
   10  reference.invalid, reference.unresolved, reference.ambiguous,
       source.invalid, network.refused
   20  network.unreachable, network.status, network.rate_limited,
       offline.missing
   30  integrity.mismatch
   40  auth.required
   50  local.io, local.not_empty, project.invalid, project.changed
  130  interrupted
`fetchloom help exit-codes` says what to do about each.

errors[0].fix is the command to run next, or null. it is safe to run
without asking: it never downloads into place, writes outside the project,
publishes, deletes, overwrites or repins your files, signs in or answers a
question for you, and in a --locked run it never writes data.lock. so add,
sync, lock, path, get and push are no fix but with --dry-run, or lock with
--check. help still names them, as steps for you to choose.

nothing asks a question with --json, when stdin or stderr is no terminal,
or where CI, AI_AGENT or a coding agent's own variable is set. a step that
needs a yes then fails as unconfirmed and says what it would do. --yes
answers yes, as to a large download or a project in the home folder, but
never chooses for you: give add the reference of a dataset, which
`fetchloom search <words> --json` finds, rather than words to pick from.

--dry-run, on add, remove, exclude, drop, sync, lock, push, get and cache gc,
says what would change and changes nothing.

status, diff, list, search, help, lock --check, and settings, cache and
auth without a subcommand, read and change nothing; export writes only the
file -o names. path syncs the dataset it names first when that is not
ready, and cache verify notes changed cached files.

more: https://nikhil-yn-aix.github.io/fetchloom/user/reference/json-output/";

/// help topics that are not commands, each with its text.
const TOPICS: [(&str, &str); 4] = [
    ("agents", AGENTS),
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
    /// the options fetchloom itself takes: those every command takes, and `--version`.
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
    /// `--select`, `-C` for an option with no long spelling, or `<link>` for a positional argument.
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

/// `command`'s own arguments, or with `top` the options fetchloom itself takes.
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

/// the error for a topic that names no help topic or command, suggesting the closest one.
fn unknown(topic: &[String]) -> Error {
    let asked = topic.join(" ");
    let shown = crate::shown::mask_query(&asked);
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
    // a short form, as `env` for environment, is too far from the whole name to be a typo
    let begun = |topic: &&str| asked.len() >= 3 && topic.starts_with(asked.as_str());
    let shortened = || {
        let mut begun = topics.iter().map(String::as_str).filter(begun);
        begun.next().filter(|_| begun.next().is_none())
    };
    match name::closest(&asked, topics.iter().map(String::as_str)).or_else(shortened) {
        Some(best) => {
            let typed: Vec<&str> = topic.iter().map(String::as_str).collect();
            let meant: Vec<&str> = best.split(' ').collect();
            let shown = super::meant(&typed, &meant, format!("fetchloom help {best}"));
            error.help(format!("did you mean `{shown}`?"))
        }
        None => error
            .detail("topics: agents, concepts, environment, exit-codes,")
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

/// what `-h` shows: the commands and options used most, or a command's usage, examples and own
/// options, then where `--help` shows the rest. none when `topic` names no command.
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
        root = root.after_help(args::TOP_EXAMPLES);
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
            "`fetchloom {} --help` also lists the global options",
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
/// so a person reads the commands to copy first, and its arguments named as its usage names them.
pub(super) fn examples_first(text: &str) -> String {
    let text = agreed(text);
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(start) = lines.iter().position(|line| *line == "examples:") else {
        return text;
    };
    let end = lines[start..]
        .iter()
        .position(|line| {
            ["on disk:", "help topics:", "docs:"]
                .iter()
                .any(|heading| line.starts_with(heading))
        })
        .map_or(lines.len(), |at| start + at);
    let Some(usage) = lines.iter().position(|line| line.starts_with("usage:")) else {
        return text;
    };
    let Some(below) = lines[usage..]
        .iter()
        .position(|line| line.is_empty())
        .map(|at| usage + at)
        .filter(|below| *below < start)
    else {
        return text;
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

/// `text` with each argument listed as the usage above it writes it: clap brackets an argument
/// it is not told is required, as `[link]...`, though the command reports it missing itself.
fn agreed(text: &str) -> String {
    let Some((usage, listed)) = text.split_once("\narguments:\n") else {
        return text.to_owned();
    };
    let mut lines: Vec<String> = Vec::new();
    let mut entries = true;
    for line in listed.split('\n') {
        entries &= !line.is_empty();
        let name = line
            .strip_prefix("  [")
            .and_then(|rest| rest.split_once(']'))
            .map(|(name, _)| name);
        match name {
            Some(name) if entries && usage.contains(&format!("<{name}>")) => {
                lines.push(line.replacen(&format!("[{name}]"), &format!("<{name}>"), 1));
            }
            _ => lines.push(line.to_owned()),
        }
    }
    format!("{usage}\narguments:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{AGENTS, CONCEPTS, ENVIRONMENT, EXIT_CODES, TOPICS, args, arguments};

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

    /// the environment topic names every key the settings file takes, the archive_ ones as a
    /// group.
    #[test]
    fn the_environment_topic_names_every_settings_file_key() {
        let paragraph = ENVIRONMENT
            .split("\n\n")
            .find(|part| part.starts_with("a settings file keeps"))
            .unwrap();
        let words: Vec<&str> = paragraph
            .split([' ', ',', '\n'])
            .filter(|word| !word.is_empty())
            .collect();
        for key in crate::settings::keys().filter(|key| !key.starts_with("archive_")) {
            assert!(words.contains(&key), "{key} is not in: {paragraph}");
        }
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

    /// the quoted names in `code` shaped like a variable, as `"HF_TOKEN"` or `"NETRC"`.
    fn quoted_variables(code: &str) -> Vec<&str> {
        let quotes: Vec<usize> = code.match_indices('"').map(|(at, _)| at).collect();
        quotes
            .windows(2)
            .map(|pair| &code[pair[0] + 1..pair[1]])
            .filter(|word| word.contains('_') || *word == "NETRC")
            .filter(|word| word.starts_with(|c: char| c.is_ascii_uppercase()))
            .filter(|word| {
                word.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
            .collect()
    }

    /// a person whose key is refused or whose network swaps certificates looks here for what
    /// set it, so each variable the key and certificate code reads is named in both places.
    #[test]
    fn the_environment_topic_names_every_key_and_certificate_variable() {
        let docs = include_str!("../../docs/user/reference/environment.md");
        let topic: std::collections::BTreeSet<&str> = ENVIRONMENT
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .collect();
        let code = [
            include_str!("../auth.rs"),
            include_str!("../auth/aws.rs"),
            include_str!("../auth/azure.rs"),
            include_str!("../auth/google.rs"),
            include_str!("../auth/kaggle.rs"),
            include_str!("../auth/netrc.rs"),
            include_str!("../net/roots.rs"),
            include_str!("../account.rs"),
        ];
        let read: std::collections::BTreeSet<&str> =
            code.into_iter().flat_map(quoted_variables).collect();
        assert!(
            read.contains("GH_TOKEN") && read.contains("SSL_CERT_FILE"),
            "{read:?}"
        );
        for name in read {
            assert!(topic.contains(name), "{name}");
            assert!(docs.contains(&format!("`{name}`")), "{name}");
        }
    }

    /// the agents topic gives every kind fetchloom has with its exit code, and no other.
    #[test]
    fn the_agents_topic_gives_every_kind_its_exit_code() {
        let table: Vec<(u8, String)> = AGENTS
            .lines()
            .skip_while(|line| !line.starts_with("errors[0].kind"))
            .skip(1)
            .take_while(|line| line.starts_with("  "))
            .scan(0, |code, line| {
                let (number, kinds) = line.trim().split_once("  ").unwrap_or(("", line));
                if let Ok(number) = number.parse() {
                    *code = number;
                }
                Some((*code, kinds.trim().to_owned()))
            })
            .flat_map(|(code, kinds)| {
                let names: Vec<String> = kinds
                    .split(',')
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .collect();
                names.into_iter().map(move |name| (code, name))
            })
            .collect();
        let mut listed = table.clone();
        listed.sort();
        let mut kinds: Vec<(u8, String)> = crate::error::tests::every()
            .iter()
            .map(|kind| (kind.exit_code(), kind.name().to_owned()))
            .collect();
        kinds.sort();
        assert_eq!(listed, kinds);
    }

    /// the agents topic names each command that takes --dry-run, and only those.
    #[test]
    fn the_agents_topic_names_every_dry_run() {
        let sentence: String = AGENTS
            .split("\n\n")
            .find(|paragraph| paragraph.starts_with("--dry-run, on "))
            .unwrap()
            .replace('\n', " ");
        let named = sentence
            .trim_start_matches("--dry-run, on ")
            .split_once(", says")
            .unwrap()
            .0
            .replace(" and ", ", ");
        let mut named: Vec<&str> = named.split(", ").collect();
        named.sort_unstable();
        let root = args::Args::command();
        let mut taking: Vec<String> = Vec::new();
        for command in root.get_subcommands() {
            let inner = command
                .get_subcommands()
                .map(|sub| (format!("{} {}", command.get_name(), sub.get_name()), sub));
            for (name, command) in
                std::iter::once((command.get_name().to_owned(), command)).chain(inner)
            {
                if command.get_arguments().any(|arg| arg.get_id() == "dry_run") {
                    taking.push(name);
                }
            }
        }
        taking.sort_unstable();
        assert_eq!(named, taking);
    }

    /// a second paragraph in a command's about made clap put each option's text on a line of
    /// its own in `-h`, unlike every other command.
    #[test]
    fn every_short_help_puts_the_text_beside_each_option() {
        let root = args::Args::command();
        for command in root.get_subcommands() {
            let name = command.get_name();
            let text = super::short(&[name.to_owned()]).unwrap_or_default();
            for line in text
                .lines()
                .filter(|line| line.trim_start().starts_with('-'))
            {
                assert!(line.trim().contains("  "), "{name} -h: {line}");
            }
        }
    }

    /// a person behind a company proxy looks in this topic for where the proxy is set.
    #[test]
    fn the_environment_topic_names_the_proxy_variables() {
        let docs = include_str!("../../docs/user/reference/environment.md");
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY"] {
            assert!(ENVIRONMENT.contains(name), "{name}");
            assert!(docs.contains(&format!("`{name}`")), "{name}");
        }
    }

    /// a kind is listed by its name, or as one of a family such as `network.*`.
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

    /// topics are listed in `help --help`'s argument, an unknown topic's help and `--help`.
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
        let full = args::command()
            .try_get_matches_from(["fetchloom", "--help"])
            .unwrap_err()
            .render()
            .to_string();
        let listed: String = full
            .lines()
            .filter(|line| line.starts_with("help topics: ") || line.starts_with("for agents"))
            .collect();
        for (name, _) in TOPICS {
            assert!(argument.contains(name), "{argument} lacks {name}");
            assert!(details.contains(name), "{details} lacks {name}");
            assert!(listed.contains(name), "--help lacks {name}");
        }
        let lines = unknown.details.iter().map(AsRef::as_ref);
        for line in lines.chain(unknown.help.as_deref()) {
            assert!(line.chars().count() <= 56, "{line}");
        }
    }

    /// a topic is found from a short form of its name, as `env`, when only one starts so.
    #[test]
    fn a_shortened_topic_is_suggested_whole() {
        for (asked, meant) in [("env", "environment"), ("exit", "exit-codes")] {
            let error = super::unknown(&[asked.to_owned()]);
            assert_eq!(
                error.help.as_deref(),
                Some(format!("did you mean `fetchloom help {meant}`?").as_str()),
                "{asked}"
            );
        }
        // `co` starts concepts, completions and more; too short and too many to choose from
        assert_eq!(
            super::unknown(&["co".to_owned()]).help.as_deref(),
            Some("`fetchloom --help` lists the commands")
        );
    }

    /// moving the examples up keeps every line the help had, its arguments named as its usage
    /// names them.
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
        let agreed = super::agreed(&rendered);
        let mut before: Vec<&str> = agreed.lines().filter(|line| !line.is_empty()).collect();
        let mut after: Vec<&str> = lines.into_iter().filter(|line| !line.is_empty()).collect();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after);
    }

    /// `command --help` as a person reads it.
    fn full_help(command: &str) -> String {
        let rendered = args::command()
            .try_get_matches_from(["fetchloom", command, "--help"])
            .unwrap_err()
            .render()
            .to_string();
        super::examples_first(&rendered)
    }

    /// a command's own options come first, and those every command takes follow under a heading
    /// of their own.
    #[test]
    fn the_global_options_follow_a_command_own() {
        let help = full_help("add");
        let own = help.find("\noptions:\n").unwrap();
        let global = help.find("\nglobal options:\n").unwrap();
        assert!(own < global, "{help}");
        assert!(help[own..global].contains("--select <glob>"), "{help}");
        for option in ["-q, --quiet", "--json", "-h, --help"] {
            assert!(help[global..].contains(option), "{option}: {help}");
            assert!(!help[own..global].contains(option), "{option}: {help}");
        }
    }

    /// the arguments are listed as the usage writes them, though clap brackets an argument the
    /// command reports missing itself.
    #[test]
    fn the_arguments_are_named_as_the_usage_names_them() {
        for (command, name) in [
            ("add", "<link>..."),
            ("get", "<link>..."),
            ("remove", "<name>"),
        ] {
            let help = full_help(command);
            let (usage, listed) = help.split_once("\narguments:\n").unwrap();
            assert!(usage.contains(name), "{usage}");
            assert!(listed.trim_start().starts_with(name), "{help}");
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
