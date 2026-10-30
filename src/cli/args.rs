//! the command line as clap reads it. every heading and placeholder is lowercase, like the docs.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

use crate::output::ColorChoice;

const TEMPLATE: &str = "{about}\n\nusage: {usage}\n\n{all-args}{after-help}";

/// what each command's help ends with: command lines to copy, then what the command changes on
/// disk.
mod examples {
    pub(super) const ADD: &str = "examples:
  fetchloom add zenodo:3608990
  fetchloom add doi:10.5281/zenodo.3608990 --as flowers
  fetchloom add zenodo:3608990 --as measurements --select \"*.csv\"

on disk: writes data.toml, data.lock and data/<name>, and adds data/ to a git project's .gitignore";

    pub(super) const REMOVE: &str = "examples:
  fetchloom remove iris
  fetchloom remove flowers --json

on disk: edits data.toml and data.lock, and deletes the unchanged files it placed in data/";

    pub(super) const SYNC: &str = "examples:
  fetchloom sync
  fetchloom sync --offline
  fetchloom sync --force

on disk: makes data/ match data.lock, and writes data.lock first when data.toml changed";

    pub(super) const LOCK: &str = "examples:
  fetchloom lock
  fetchloom lock --check
  fetchloom lock --upgrade iris

on disk: writes data.lock and may add files to the cache. --check writes nothing";

    pub(super) const STATUS: &str = "examples:
  fetchloom status
  fetchloom status --json

on disk: nothing. it reads data.toml, data.lock and data/";

    pub(super) const PATH: &str = "examples:
  fetchloom path iris
  fetchloom path iris/iris.csv
  fetchloom path iris -q

on disk: nothing when the dataset is ready. otherwise it syncs first, as `fetchloom sync` does";

    pub(super) const CACHE: &str = "examples:
  fetchloom cache
  fetchloom cache gc
  fetchloom cache verify

on disk: nothing. `cache gc` and `cache verify` delete from the cache, never from data/";

    pub(super) const GC: &str = "examples:
  fetchloom cache gc
  fetchloom cache gc --json

on disk: deletes cached files no project needs once they are a day old. data/ stays as it is";

    pub(super) const VERIFY: &str = "examples:
  fetchloom cache verify
  fetchloom cache verify --json

on disk: deletes cached files whose bytes changed. data/ stays as it is";

    pub(super) const GET: &str = "examples:
  fetchloom get zenodo:3608990
  fetchloom get zenodo:3608990 -o flowers
  fetchloom get https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv

on disk: writes the output folder through the cache. no data.toml, data.lock or data/";

    pub(super) const HELP: &str = "examples:
  fetchloom help concepts
  fetchloom help add
  fetchloom help cache gc

on disk: nothing";
}

/// what a beginner meets first, on the first run and at the top of `fetchloom --help`: what
/// fetchloom does, the analogy, commands to copy and where to learn more.
pub(crate) const INTRO: &str =
    "fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by every project on this machine
  sync, restocking: data/ ends up matching the receipt

try it with zenodo:3608990, a 4 KB iris.csv:

  fetchloom add zenodo:3608990   put iris.csv in data/iris and pin it
  fetchloom sync                 restock data/ in a project you cloned
  fetchloom get zenodo:3608990   fetch it once, without a project

learn more with `fetchloom help concepts` and `fetchloom <command> --help`";

/// put the exact data a project needs onto any machine with one command
#[derive(Debug, Parser)]
#[command(
    name = "fetchloom",
    about = INTRO,
    long_about = None,
    version,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    subcommand_help_heading = "commands",
    subcommand_value_name = "command",
    override_usage = "fetchloom [options] <command>",
    help_template = TEMPLATE
)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    #[command(flatten)]
    pub(crate) global: Global,

    /// print help
    #[arg(short, long, action = ArgAction::Help, global = true, help_heading = "options")]
    help: Option<bool>,

    /// print the version
    #[arg(short = 'V', long, help_heading = "options")]
    pub(crate) version: bool,
}

/// options every command takes.
#[derive(Debug, clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a yes or no on the command line"
)]
pub(crate) struct Global {
    /// print only the answer and errors
    #[arg(
        short,
        long,
        global = true,
        conflicts_with = "verbose",
        help_heading = "options"
    )]
    pub(crate) quiet: bool,

    /// say more: -v adds detail and counters, -vv adds debug logs
    #[arg(short, long, action = ArgAction::Count, global = true, help_heading = "options")]
    pub(crate) verbose: u8,

    /// print one JSON document on stdout instead of the story
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) json: bool,

    /// use only what is already on this machine
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) offline: bool,

    /// answer yes to every question, such as before a large download
    #[arg(short, long, global = true, help_heading = "options")]
    pub(crate) yes: bool,

    /// when to use color
    #[arg(long, value_enum, value_name = "when", default_value_t = ColorChoice::Auto, global = true, help_heading = "options")]
    pub(crate) color: ColorChoice,

    /// run as if fetchloom started in <dir>
    #[arg(
        short = 'C',
        value_name = "dir",
        global = true,
        help_heading = "options"
    )]
    pub(crate) directory: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// add a dataset, pin it and download it into the data folder
    #[command(override_usage = "fetchloom add [options] <link>", help_template = TEMPLATE, after_help = examples::ADD)]
    Add(Add),

    /// drop a dataset and the files fetchloom placed for it
    #[command(override_usage = "fetchloom remove [options] <name>", help_template = TEMPLATE, after_help = examples::REMOVE)]
    Remove(Remove),

    /// make the data folder match data.lock on this machine
    #[command(override_usage = "fetchloom sync [options]", help_template = TEMPLATE, after_help = examples::SYNC)]
    Sync(Sync),

    /// bring data.lock in line with data.toml without downloading what is pinned
    #[command(override_usage = "fetchloom lock [options]", help_template = TEMPLATE, after_help = examples::LOCK)]
    Lock(Lock),

    /// list files that are missing, changed by hand or extra
    #[command(override_usage = "fetchloom status [options]", help_template = TEMPLATE, after_help = examples::STATUS)]
    Status,

    /// print where a dataset or a file in it is, syncing first if needed
    #[command(override_usage = "fetchloom path [options] <name>[/file]", help_template = TEMPLATE, after_help = examples::PATH)]
    Path(Path),

    /// show the cache, free what no project needs, or check every file in it
    #[command(override_usage = "fetchloom cache [options] [command]", help_template = TEMPLATE, subcommand_help_heading = "commands", subcommand_value_name = "command", after_help = examples::CACHE)]
    Cache(Cache),

    /// fetch something once, without a project
    #[command(override_usage = "fetchloom get [options] <link>", help_template = TEMPLATE, after_help = examples::GET)]
    Get(Get),

    /// explain the ideas behind fetchloom, or one command
    #[command(override_usage = "fetchloom help [topic]", help_template = TEMPLATE, after_help = examples::HELP)]
    Help(Help),
}

impl Command {
    /// the command's name, as `--json` reports it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Command::Add(_) => "add",
            Command::Remove(_) => "remove",
            Command::Sync(_) => "sync",
            Command::Lock(_) => "lock",
            Command::Status => "status",
            Command::Path(_) => "path",
            Command::Cache(Cache { action: None }) => "cache",
            Command::Cache(Cache {
                action: Some(CacheAction::Gc),
            }) => "cache gc",
            Command::Cache(Cache {
                action: Some(CacheAction::Verify),
            }) => "cache verify",
            Command::Get(_) => "get",
            Command::Help(_) => "help",
        }
    }
}

/// show the cache, free what no project needs, or check every file in it
#[derive(Debug, clap::Args)]
pub(crate) struct Cache {
    #[command(subcommand)]
    pub(crate) action: Option<CacheAction>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CacheAction {
    /// free what no project needs, once it is a day old
    #[command(override_usage = "fetchloom cache gc [options]", help_template = TEMPLATE, after_help = examples::GC)]
    Gc,

    /// hash every file in the cache again and remove any that changed
    #[command(override_usage = "fetchloom cache verify [options]", help_template = TEMPLATE, after_help = examples::VERIFY)]
    Verify,
}

/// which files of a dataset to keep, for `add` and `get`.
#[derive(Debug, clap::Args)]
pub(crate) struct Filter {
    /// keep only files matching this pattern, such as "**/*.csv". repeat it for more
    #[arg(long, value_name = "glob", help_heading = "options")]
    pub(crate) select: Vec<String>,

    /// leave out files matching this pattern. repeat it for more
    #[arg(long, value_name = "glob", help_heading = "options")]
    pub(crate) exclude: Vec<String>,
}

/// add a dataset, pin it and download it into the data folder
#[derive(Debug, clap::Args)]
pub(crate) struct Add {
    /// a link, a DOI, or an id such as zenodo:3608990
    #[arg(value_name = "link", help_heading = "arguments")]
    pub(crate) link: String,

    /// the dataset's name in data.toml and its folder's name
    #[arg(long = "as", value_name = "name", help_heading = "options")]
    pub(crate) name: Option<String>,

    #[command(flatten)]
    pub(crate) filter: Filter,

    /// keep zip and tar archives packed, as extract = false in data.toml does
    #[arg(long, help_heading = "options")]
    pub(crate) no_extract: bool,
}

/// drop a dataset and the files fetchloom placed for it
#[derive(Debug, clap::Args)]
pub(crate) struct Remove {
    /// the dataset's name in data.toml
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) name: String,
}

/// make the data folder match data.lock on this machine
#[derive(Debug, clap::Args)]
pub(crate) struct Sync {
    /// replace conflicting files at locked paths, including untracked files
    #[arg(long, help_heading = "options")]
    pub(crate) force: bool,
}

/// bring data.lock in line with data.toml
#[derive(Debug, clap::Args)]
pub(crate) struct Lock {
    /// re-resolve datasets, keeping explicit version pins
    #[arg(long, value_name = "name", num_args = 0.., help_heading = "options")]
    pub(crate) upgrade: Option<Vec<String>>,

    /// change nothing, and fail when data.lock is out of date, for CI
    #[arg(long, conflicts_with = "upgrade", help_heading = "options")]
    pub(crate) check: bool,
}

/// print where a dataset or a file in it is
#[derive(Debug, clap::Args)]
pub(crate) struct Path {
    /// a dataset's name, or a file inside it as name/path
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) target: String,
}

/// fetch something once, without a project
#[derive(Debug, clap::Args)]
pub(crate) struct Get {
    /// what to download: a link, a DOI, or an id such as zenodo:3608990
    #[arg(value_name = "link", help_heading = "arguments")]
    pub(crate) link: String,

    /// put the download in <dir> instead of a new folder named after it
    #[arg(short, long, value_name = "dir", help_heading = "options")]
    pub(crate) output: Option<PathBuf>,

    #[command(flatten)]
    pub(crate) filter: Filter,

    /// keep zip and tar archives packed instead of unpacking them
    #[arg(long, help_heading = "options")]
    pub(crate) no_extract: bool,
}

/// explain the ideas behind fetchloom, or one command
#[derive(Debug, clap::Args)]
pub(crate) struct Help {
    /// concepts, or a command such as add or cache gc
    #[arg(value_name = "topic", help_heading = "arguments")]
    pub(crate) topic: Vec<String>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Args;

    #[test]
    fn force_help_names_untracked_conflicts() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("sync")
            .ok_or("sync command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("replace conflicting files at locked paths, including untracked files"),
            "{help}"
        );
        Ok(())
    }

    #[test]
    fn upgrade_help_keeps_explicit_pins() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("lock")
            .ok_or("lock command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("re-resolve datasets, keeping explicit version pins"),
            "{help}"
        );
        Ok(())
    }

    /// the words a shell makes of `line`, where double quotes keep a pattern such as "*.csv"
    /// whole.
    fn words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word: Option<String> = None;
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    word.get_or_insert_default();
                }
                c if c.is_whitespace() && !quoted => words.extend(word.take()),
                c => word.get_or_insert_default().push(c),
            }
        }
        words.extend(word);
        words
    }

    /// the help of every command and subcommand, as `fetchloom <command> --help` prints it.
    fn every_help() -> Vec<(String, String)> {
        let root = Args::command();
        let mut names: Vec<Vec<String>> = Vec::new();
        for command in root.get_subcommands() {
            names.push(vec![command.get_name().to_owned()]);
            for inner in command.get_subcommands() {
                names.push(vec![
                    command.get_name().to_owned(),
                    inner.get_name().to_owned(),
                ]);
            }
        }
        names
            .into_iter()
            .map(|name| {
                let line = std::iter::once("fetchloom".to_owned())
                    .chain(name.iter().cloned())
                    .chain(std::iter::once("--help".to_owned()));
                let help = match Args::command().try_get_matches_from(line) {
                    Ok(_) => String::new(),
                    Err(err) => err.render().to_string(),
                };
                (name.join(" "), help)
            })
            .collect()
    }

    #[test]
    fn every_command_shows_examples_to_copy_and_what_it_changes_on_disk() {
        for (name, help) in every_help() {
            let examples: Vec<&str> = help
                .split_once("\nexamples:\n")
                .map(|(_, rest)| {
                    rest.lines()
                        .take_while(|line| line.starts_with("  "))
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                (2..=4).contains(&examples.len()),
                "{name} shows {} examples\n{help}",
                examples.len()
            );
            for example in examples {
                assert!(
                    example.starts_with(&format!("  fetchloom {name}")),
                    "{name}: {example}"
                );
                let words = words(example);
                if let Err(err) = Args::try_parse_from(&words) {
                    panic!("{name}: `{example}` does not parse: {err}");
                }
            }
            let disk: Vec<&str> = help
                .lines()
                .filter(|line| line.starts_with("on disk: "))
                .collect();
            assert_eq!(disk.len(), 1, "{name}\n{help}");
        }
    }
}
