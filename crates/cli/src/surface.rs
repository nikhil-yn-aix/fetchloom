//! The command surface.
//!
//! Before 1.0 a command, flag, or value exists here only once it performs what
//! contracts.md says it does. Nothing is present and unable to act, so the
//! generated help and completion scripts describe exactly what this build can
//! do. The roadmap says which phase delivers each of the rest.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

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

/// Flags for the commands that materialize bytes.
#[derive(Args, Clone, Debug, Default)]
pub struct TransferFlags {
    /// Destination directory.
    #[arg(long, short, value_name = "path")]
    pub output: Option<PathBuf>,
    /// How far a write is pushed before publication.
    #[arg(long, value_name = "strict|normal|fast")]
    pub durability: Option<DurabilityChoice>,
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
