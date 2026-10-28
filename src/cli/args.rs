//! the command line as clap reads it. every heading and placeholder is lowercase, like the docs.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

use crate::output::ColorChoice;

const TEMPLATE: &str = "{about}\n\nusage: {usage}\n\n{all-args}";

/// put the exact data a project needs onto any machine with one command
#[derive(Debug, Parser)]
#[command(
    name = "fetchloom",
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
    #[arg(short = 'V', long, action = ArgAction::Version, help_heading = "options")]
    version: Option<bool>,
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
    #[command(override_usage = "fetchloom add [options] <link>", help_template = TEMPLATE)]
    Add(Add),

    /// drop a dataset and the files fetchloom placed for it
    #[command(override_usage = "fetchloom remove [options] <name>", help_template = TEMPLATE)]
    Remove(Remove),

    /// make the data folder match data.lock on this machine
    #[command(override_usage = "fetchloom sync [options]", help_template = TEMPLATE)]
    Sync(Sync),

    /// bring data.lock in line with data.toml without downloading what is pinned
    #[command(override_usage = "fetchloom lock [options]", help_template = TEMPLATE)]
    Lock(Lock),

    /// list files that are missing, changed by hand or extra
    #[command(override_usage = "fetchloom status [options]", help_template = TEMPLATE)]
    Status,

    /// print where a dataset or a file in it is, syncing first if needed
    #[command(override_usage = "fetchloom path [options] <name>[/file]", help_template = TEMPLATE)]
    Path(Path),

    /// show the cache, free what no project needs, or check every file in it
    #[command(override_usage = "fetchloom cache [options] [command]", help_template = TEMPLATE, subcommand_help_heading = "commands", subcommand_value_name = "command")]
    Cache(Cache),

    /// fetch something once, without a project
    #[command(override_usage = "fetchloom get [options] <link>", help_template = TEMPLATE)]
    Get(Get),
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
    #[command(override_usage = "fetchloom cache gc [options]", help_template = TEMPLATE)]
    Gc,

    /// hash every file in the cache again and remove any that changed
    #[command(override_usage = "fetchloom cache verify [options]", help_template = TEMPLATE)]
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
    /// replace files that were changed by hand
    #[arg(long, help_heading = "options")]
    pub(crate) force: bool,
}

/// bring data.lock in line with data.toml
#[derive(Debug, clap::Args)]
pub(crate) struct Lock {
    /// move datasets to their newest version: all of them, or the ones named
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
}
