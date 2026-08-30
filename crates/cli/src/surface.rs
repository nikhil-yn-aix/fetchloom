//! The command surface.
//!
//! Before 1.0 a command, flag, or value exists here only once it performs what
//! contracts.md says it does. Nothing is present and unable to act, so the
//! generated help and completion scripts describe exactly what this build can
//! do. The roadmap says which phase delivers each of the rest.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fetchloom_engine::selection::Layout;

/// How progress is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum DisplayMode {
    /// One aggregated progress line and the final result.
    Plain,
    /// A redrawn view of the run's internals.
    Live,
    /// No progress output.
    None,
}

/// A shell a completion script can be written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Shell {
    /// The Bourne again shell.
    Bash,
    /// The Elvish shell.
    Elvish,
    /// The friendly interactive shell.
    Fish,
    /// PowerShell.
    Powershell,
    /// The Z shell.
    Zsh,
}

/// Flags that apply to every command.
#[derive(Args, Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct GlobalFlags {
    /// Use this configuration file only.
    #[arg(long, global = true, value_name = "path")]
    pub config: Option<PathBuf>,
    /// Ignore all configuration files.
    #[arg(long, global = true)]
    pub no_config: bool,
    /// Forbid all network activity.
    #[arg(long, global = true)]
    pub offline: bool,
    /// Machine-readable result on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    /// Newline-delimited event stream.
    #[arg(long, global = true, value_name = "path|-")]
    pub events: Option<String>,
    /// Suppress progress.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Progress presentation.
    #[arg(long, global = true, value_name = "plain|live|none")]
    pub display: Option<DisplayMode>,
    /// Disable redrawing.
    #[arg(long, global = true)]
    pub no_animation: bool,
    /// Ceiling on threads used for processor work.
    #[arg(long, global = true, value_name = "n")]
    pub threads: Option<u32>,
    /// Where the cache is.
    #[arg(long, global = true, value_name = "path")]
    pub cache_dir: Option<PathBuf>,
    /// Answer every confirmation with yes.
    #[arg(long, global = true)]
    pub yes: bool,
}

/// How far a write is pushed before publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum DurabilityChoice {
    /// Flush the file and its containing directory to the device.
    Strict,
    /// Flush the file.
    Normal,
    /// Flush nothing and rely on the atomic rename alone.
    Fast,
}

/// What a cache hit is checked against before it is reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum VerifyChoice {
    /// Reread and rehash the whole object.
    Always,
    /// Trust the object when its recorded filesystem fingerprint matches.
    Fingerprint,
    /// Trust the object unconditionally, which makes the result unverified.
    Never,
}

/// The value `--layout` was given, parsed into what selection acts on.
///
/// Takes `keep` or `flatten:<n>`. Fails when the text is neither, which
/// `clap` reports as a usage error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutArg(pub Layout);

impl std::str::FromStr for LayoutArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// Flags for the commands that materialize bytes.
#[derive(Args, Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct TransferFlags {
    /// Destination directory.
    #[arg(long, short, value_name = "path")]
    pub output: Option<PathBuf>,
    /// Include members. Repeatable.
    #[arg(long = "select", value_name = "glob")]
    pub select: Vec<String>,
    /// Exclude members. Repeatable.
    #[arg(long = "exclude", value_name = "glob")]
    pub exclude: Vec<String>,
    /// Path rewriting.
    #[arg(long, value_name = "keep|flatten:n")]
    pub layout: Option<LayoutArg>,
    /// How far a write is pushed before publication.
    #[arg(long, value_name = "strict|normal|fast")]
    pub durability: Option<DurabilityChoice>,
    /// Lock file location.
    #[arg(long, value_name = "path")]
    pub lock: Option<PathBuf>,
    /// Fail when resolution differs from the lock.
    #[arg(long)]
    pub locked: bool,
    /// Bypass the cache for this operation.
    #[arg(long)]
    pub no_cache: bool,
    /// Keep a recognized archive as a file rather than extracting it.
    #[arg(long)]
    pub no_extract: bool,
    /// What a cache hit is checked against before it is reused.
    #[arg(long, value_name = "always|fingerprint|never")]
    pub verify: Option<VerifyChoice>,
    /// Overwrite modified destination entries and remove foreign ones.
    #[arg(long)]
    pub force: bool,
    /// Accept current destination contents as correct.
    #[arg(long)]
    pub adopt: bool,
}

/// What to do with the cache.
#[derive(Subcommand, Clone, Debug)]
pub enum CacheCommand {
    /// Report what the cache holds.
    Status,
    /// List the objects the cache holds.
    Ls,
    /// Reread and rehash every object, quarantining each mismatch.
    Verify,
    /// Keep an object from ever being pruned.
    Pin {
        /// The content digest to pin.
        #[arg(value_name = "digest")]
        digest: String,
    },
    /// Remove the mark that kept an object from being pruned.
    Unpin {
        /// The content digest to unpin.
        #[arg(value_name = "digest")]
        digest: String,
    },
    /// Write every object the cache holds into a bundle.
    Export {
        /// Where the bundle is written.
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    /// Read the objects a bundle holds into the cache.
    Import {
        /// The bundle to read.
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    /// Mark what nothing refers to, then sweep what has been marked longest.
    Prune,
    /// Remove every object.
    Clear,
}

/// What Fetchloom was asked to do.
#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    /// Resolve, transfer, verify, and materialize.
    Get {
        /// What to fetch.
        #[arg(required = true, value_name = "ref")]
        references: Vec<String>,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Resolve and report what a run would do, moving no bytes.
    Plan {
        /// What to plan.
        #[arg(required = true, value_name = "ref")]
        references: Vec<String>,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Execute a plan.
    Apply {
        /// The plan to execute.
        #[arg(value_name = "plan")]
        plan: PathBuf,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Recompute the tree digest of a materialized directory.
    Verify {
        /// The path to verify.
        #[arg(value_name = "path")]
        target: String,
    },
    /// Write a shell completion script to stdout.
    Completions {
        /// The shell to write a script for.
        shell: Shell,
    },
    /// Read and change what the cache holds.
    Cache {
        /// What to do with the cache.
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Report effective settings and their origin.
    Explain {
        /// One setting to report, or every setting when absent.
        key: Option<String>,
    },
}

/// The whole command line.
#[derive(Parser, Debug)]
#[command(
    name = "fetchloom",
    version,
    about = "Turns a dataset reference into exact, verified local files.",
    disable_help_subcommand = true
)]
pub struct CommandLine {
    /// The command that was asked for.
    #[command(subcommand)]
    pub command: Command,
    /// The flags that apply to every command.
    #[command(flatten)]
    pub global: GlobalFlags,
}
