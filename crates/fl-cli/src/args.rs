//! The command line: flags every command takes, and the commands.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// `fl` and everything after it.
#[derive(Parser, Debug)]
#[command(
    name = "fl",
    version,
    about = "uv for research data",
    after_help = "https://github.com/nikhil-yn-aix/fetchloom"
)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) global: Global,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// Flags every command takes.
#[derive(Args, Debug, Default)]
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
    /// Write debug logs to PATH as JSON lines
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) log_file: Option<PathBuf>,
    /// Write every span to PATH as a Chrome trace that opens in Perfetto
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) trace_file: Option<PathBuf>,
}

/// The commands.
#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// Create a data.toml here
    Init {
        /// The project's name
        #[arg(long)]
        name: Option<String>,
    },
    /// Add a dataset to data.toml, lock it and sync it
    Add(Add),
    /// Bring data.lock in line with data.toml
    Lock,
    /// Make data/ match data.lock
    Sync,
    /// Fetch a reference into a directory, without a project
    Get {
        /// What to fetch: zenodo:ID or an https URL
        reference: String,
        /// Where the files land; the reference's name beside you by default
        #[arg(short, long, value_name = "DIR")]
        output: Option<PathBuf>,
    },
    /// Print the absolute path of a dataset, syncing it first
    Path {
        /// The dataset, optionally followed by a path inside it: NAME[/SUBPATH]
        name: String,
        /// Fail instead of syncing when the dataset is not there
        #[arg(long)]
        no_sync: bool,
    },
    /// Show how data/ compares with data.lock
    Status,
}

/// What `fl add` takes.
#[derive(Args, Debug)]
pub(crate) struct Add {
    /// What to add: zenodo:ID or an https URL
    pub(crate) reference: String,
    /// The dataset's name; the reference's last segment by default
    #[arg(long = "as", value_name = "NAME")]
    pub(crate) name: Option<String>,
    /// Keep only files whose path matches GLOB
    #[arg(long, value_name = "GLOB")]
    pub(crate) select: Vec<String>,
    /// Drop files whose path matches GLOB
    #[arg(long, value_name = "GLOB")]
    pub(crate) exclude: Vec<String>,
    /// Write data.toml and data.lock only
    #[arg(long)]
    pub(crate) no_sync: bool,
}
