//! The command surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fetchloom_engine::limits::Bandwidth;
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

/// When output carries color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    /// Color when the stream is a terminal that permits it.
    Auto,
    /// Color whatever the stream is.
    Always,
    /// Never color.
    Never,
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
    /// Raise the log level one step. Repeatable.
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// When output is colored.
    #[arg(long, global = true, value_name = "auto|always|never")]
    pub color: Option<ColorChoice>,
    /// Never print a hint.
    #[arg(long, global = true)]
    pub no_hints: bool,
    /// Progress presentation.
    #[arg(long, global = true, value_name = "plain|live|none")]
    pub display: Option<DisplayMode>,
    /// Disable redrawing.
    #[arg(long, global = true)]
    pub no_animation: bool,
    /// Ceiling on threads used for processor work.
    #[arg(long, global = true, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
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

/// Which write path a run takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum IoChoice {
    /// Let the volume's own capabilities decide.
    Auto,
    /// Write through the operating system's page cache.
    Buffered,
    /// Write without leaving the bytes in the operating system's page cache.
    Uncached,
}

/// The value `--bandwidth` was given, parsed into a ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateArg(pub Bandwidth);

impl std::str::FromStr for RateArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// The value `--timeout` was given, parsed into a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurationArg(pub std::time::Duration);

impl std::str::FromStr for DurationArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refusal = || format!("{text} is not a duration such as 30s, 500ms, or 2m");
        let (digits, unit) = text.split_at(
            text.find(|letter: char| !letter.is_ascii_digit())
                .ok_or_else(refusal)?,
        );
        let count: u64 = digits.parse().map_err(|_| refusal())?;
        let span = match unit {
            "ms" => std::time::Duration::from_millis(count),
            "s" => std::time::Duration::from_secs(count),
            "m" => std::time::Duration::from_secs(count.saturating_mul(60)),
            "h" => std::time::Duration::from_secs(count.saturating_mul(3600)),
            _ => return Err(refusal()),
        };
        if span.is_zero() {
            return Err(refusal());
        }
        Ok(Self(span))
    }
}

/// The value `--layout` was given, parsed into what selection acts on.
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
    /// Ceiling on transfers in flight across every host.
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub concurrency: Option<u32>,
    /// Ceiling on transfers in flight for one host.
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_host: Option<u32>,
    /// Ceiling on how fast the run may transfer.
    #[arg(long, value_name = "rate")]
    pub bandwidth: Option<RateArg>,
    /// Attempts per transient failure.
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub retries: Option<u32>,
    /// Idle timeout per connection.
    #[arg(long, value_name = "duration")]
    pub timeout: Option<DurationArg>,
    /// Which write path a run takes.
    #[arg(long, value_name = "auto|buffered|uncached")]
    pub io: Option<IoChoice>,
    /// Raise politeness ceilings.
    #[arg(long)]
    pub aggressive: bool,
    /// Disable adaptation, so two runs do identical work.
    #[arg(long)]
    pub deterministic_io: bool,
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
    /// Rebuild the derived data the cache can regenerate from what it holds.
    Repair,
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
        #[arg(value_name = "ref")]
        reference: String,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Infer a manifest and write it.
    Init {
        /// What to infer a manifest for.
        #[arg(value_name = "url|dir")]
        reference: String,
        /// Where the manifest is written, rather than to standard output.
        #[arg(long, short, value_name = "path")]
        output: Option<PathBuf>,
        /// Overwrite the file the manifest is written to.
        #[arg(long)]
        force: bool,
    },
    /// Resolve and report what a run would do, moving no bytes.
    Plan {
        /// What to plan.
        #[arg(value_name = "ref")]
        reference: String,
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
    /// Refetch the damaged ranges of a cached object.
    Repair {
        /// What to repair.
        #[arg(value_name = "ref")]
        reference: String,
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
    /// Render a run's event stream, live or after the fact.
    Watch {
        /// The event stream to render, or `-` for standard input.
        #[arg(id = "watched", value_name = "events")]
        stream: String,
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
    /// Check the environment, changing nothing.
    Doctor,
    /// Explain the resolution, source choice, and trust reasoning for a
    /// reference.
    Why {
        /// The reference to explain.
        #[arg(value_name = "ref")]
        reference: String,
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
