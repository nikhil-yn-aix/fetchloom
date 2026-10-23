//! The command line: flags every command takes, and the commands.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

const HELP: &str = "\
{about-with-newline}
{usage-heading} {usage}

Start a project:
  init     Create a data.toml here
  add      Add a dataset to data.toml, lock it and download it

Keep data/ in line:
  sync     Make data/ match data.lock
  status   Show how data/ compares with data.lock
  restore  Put changed or missing files back, without downloading
  lock     Bring data.lock in line with data.toml
  remove   Drop datasets from data.toml, data.lock and data/

Use the data:
  path     Print where a dataset is, for scripts
  get      Fetch something once, without a project

Options:
{options}{after-help}";

/// `fl` and everything after it.
#[derive(Parser, Debug)]
#[command(
    name = "fl",
    bin_name = "fl",
    version,
    about = "uv for research data",
    help_template = HELP,
    after_help = "\nhttps://github.com/nikhil-yn-aix/fetchloom"
)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) global: Global,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// Flags every command takes.
#[derive(Args, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is a flag of its own on the command line"
)]
pub(crate) struct Global {
    /// Run as if fl was started in DIR
    #[arg(short = 'C', global = true, value_name = "DIR")]
    pub(crate) directory: Option<PathBuf>,
    /// Print one JSON document on stdout instead of the human story on stderr
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Say more: work and timings with -v, debug logs with -vv, every request with -vvv
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,
    /// Say less: only the answer with -q, nothing but the exit code with -qq
    #[arg(short, long, action = ArgAction::Count, global = true, conflicts_with = "verbose")]
    pub(crate) quiet: u8,
    /// Answer yes to every question, such as downloading a large dataset
    #[arg(short, long, global = true)]
    pub(crate) yes: bool,
    /// Make no network request at all, not even a DNS lookup
    #[arg(long, global = true)]
    pub(crate) offline: bool,
    /// When to color the output: auto, always or never
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        default_value = "auto",
        hide_possible_values = true,
        hide_default_value = true
    )]
    pub(crate) color: Color,
    /// Draw no progress bars, even on a terminal
    #[arg(long, global = true)]
    pub(crate) no_progress: bool,
    /// Use DIR as the store for this run
    #[arg(long, global = true, value_name = "DIR")]
    pub(crate) store: Option<PathBuf>,
    /// Transfers running at once
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) jobs: Option<u16>,
    /// The longest silence tolerated on a connection, in seconds
    #[arg(long, global = true, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) timeout: Option<u64>,
    /// Write debug logs to PATH as JSON lines
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) log_file: Option<PathBuf>,
    /// Write every span to PATH as a Chrome trace that opens in Perfetto
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) trace_file: Option<PathBuf>,
}

/// When output is colored.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Color {
    /// When the stream is a terminal and `NO_COLOR` is not set.
    #[default]
    Auto,
    /// Always.
    Always,
    /// Never.
    Never,
}

/// The commands.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Create a data.toml here
    Init {
        /// The project's name
        #[arg(long)]
        name: Option<String>,
        /// Ask for the name, a first dataset and where the store lives
        #[arg(short, long)]
        interactive: bool,
    },
    /// Add a dataset to data.toml, lock it and download it
    Add(Add),
    /// Drop datasets from data.toml, data.lock and data/
    Remove {
        /// The datasets
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
    },
    /// Bring data.lock in line with data.toml
    Lock {
        /// Resolve the named datasets again, or all of them, and show what changed
        #[arg(long, value_name = "NAME", num_args = 0..)]
        upgrade: Option<Vec<String>>,
        /// Change nothing and fail when data.lock is out of date, for CI
        #[arg(long, conflicts_with = "upgrade")]
        check: bool,
    },
    /// Make data/ match data.lock
    Sync(Sync),
    /// Show how data/ compares with data.lock
    Status {
        /// Only these datasets
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Fail when anything differs, for CI
        #[arg(long)]
        check: bool,
    },
    /// Put changed or missing files back, without downloading
    Restore {
        /// Only these files or folders under data/
        #[arg(value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
    /// Print the absolute path of a dataset, syncing it first
    Path {
        /// The dataset, optionally followed by a path inside it: NAME[/SUBPATH]
        name: String,
        /// Fail instead of syncing when the dataset is not there
        #[arg(long)]
        no_sync: bool,
    },
    /// Fetch a reference into a directory, without a project
    Get(Get),
}

impl Command {
    /// `fl status` with nothing narrowed, which `fl` alone runs inside a project.
    pub(crate) const fn status() -> Self {
        Self::Status {
            names: Vec::new(),
            check: false,
        }
    }
}

/// What `fl add` takes.
#[derive(Args, Debug, Default)]
pub(crate) struct Add {
    /// What to add: a DOI, a link, zenodo:ID, hf:datasets/ORG/NAME, a local path, or search words
    pub(crate) reference: Option<String>,
    /// The dataset's name; the reference's last segment by default
    #[arg(long = "as", value_name = "NAME")]
    pub(crate) name: Option<String>,
    #[command(flatten)]
    pub(crate) filters: FilterArgs,
    /// How files are placed: auto links them, copy makes real copies tools may write to
    #[arg(long, value_name = "MODE")]
    pub(crate) link: Option<LinkMode>,
    /// Write data.toml and data.lock only
    #[arg(long)]
    pub(crate) no_sync: bool,
}

/// The filters `fl add` and `fl get` take.
#[derive(Args, Debug, Default, Clone)]
pub(crate) struct FilterArgs {
    /// Keep only files whose path matches GLOB
    #[arg(long, value_name = "GLOB")]
    pub(crate) select: Vec<String>,
    /// Drop files whose path matches GLOB
    #[arg(long, value_name = "GLOB")]
    pub(crate) exclude: Vec<String>,
    /// Skip files larger than SIZE, like 2GiB
    #[arg(long, value_name = "SIZE")]
    pub(crate) max_file_size: Option<String>,
    /// Keep N files, or P% of them, the same ones on every machine
    #[arg(long, value_name = "N|P%")]
    pub(crate) sample: Option<String>,
    /// The seed that picks the sample
    #[arg(long, value_name = "N", requires = "sample")]
    pub(crate) seed: Option<u64>,
}

/// How a dataset's files are placed.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkMode {
    /// The fastest safe link the disk allows.
    Auto,
    /// Real copies.
    Copy,
}

/// What `fl sync` takes.
#[derive(Args, Debug, Default)]
pub(crate) struct Sync {
    /// Only these datasets
    #[arg(value_name = "NAME")]
    pub(crate) names: Vec<String>,
    /// Fail instead of locking when data.lock is out of date, for CI
    #[arg(long)]
    pub(crate) locked: bool,
    /// Overwrite files that were changed by hand
    #[arg(long)]
    pub(crate) force: bool,
    /// Remove files under data/ that fetchloom did not place
    #[arg(long)]
    pub(crate) clean: bool,
}

/// What `fl get` takes.
#[derive(Args, Debug)]
pub(crate) struct Get {
    /// What to fetch: a DOI, a link, zenodo:ID, hf:datasets/ORG/NAME or a local path
    pub(crate) reference: String,
    /// Where the files land; the reference's name beside you by default
    #[arg(short, long, value_name = "DIR")]
    pub(crate) output: Option<PathBuf>,
    #[command(flatten)]
    pub(crate) filters: FilterArgs,
    /// Replace files in the directory that hold other bytes
    #[arg(long)]
    pub(crate) force: bool,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_lists_every_command_under_a_purpose() {
        let help = Cli::command().render_help().to_string();
        for command in Cli::command().get_subcommands() {
            let name = command.get_name();
            assert!(
                help.contains(&format!("\n  {name} ")),
                "{name} is missing from the help:\n{help}"
            );
        }
        assert!(help.contains("Usage: fl [OPTIONS] [COMMAND]"), "{help}");
        assert!(help.contains("Start a project:"), "{help}");
    }
}
