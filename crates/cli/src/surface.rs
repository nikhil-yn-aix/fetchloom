//! The command surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fetchloom_engine::limits::Bandwidth;
use fetchloom_engine::selection::Layout;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum DisplayMode {
    Plain,
    Live,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Shell {
    Bash,
    Elvish,
    Fish,
    Powershell,
    Zsh,
}

#[derive(Args, Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct GlobalFlags {
    #[arg(long, global = true, value_name = "path")]
    pub config: Option<PathBuf>,
    #[arg(long, global = true)]
    pub no_config: bool,
    #[arg(long, global = true)]
    pub offline: bool,
    #[arg(long, global = true)]
    pub json: bool,
    #[arg(long, global = true, value_name = "path|-")]
    pub events: Option<String>,
    #[arg(long, global = true)]
    pub quiet: bool,
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    #[arg(long, global = true, value_name = "auto|always|never")]
    pub color: Option<ColorChoice>,
    #[arg(long, global = true)]
    pub no_hints: bool,
    #[arg(long, global = true, value_name = "plain|live|none")]
    pub display: Option<DisplayMode>,
    #[arg(long, global = true)]
    pub no_animation: bool,
    #[arg(long, global = true, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
    #[arg(long, global = true, value_name = "path")]
    pub cache_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    pub yes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum DurabilityChoice {
    Strict,
    Normal,
    Fast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum VerifyChoice {
    Always,
    Fingerprint,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum IoChoice {
    Auto,
    Buffered,
    Uncached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateArg(pub Bandwidth);

impl std::str::FromStr for RateArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutArg(pub Layout);

impl std::str::FromStr for LayoutArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

#[derive(Args, Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct TransferFlags {
    #[arg(long, short, value_name = "path")]
    pub output: Option<PathBuf>,
    #[arg(long = "select", value_name = "glob")]
    pub select: Vec<String>,
    #[arg(long = "exclude", value_name = "glob")]
    pub exclude: Vec<String>,
    #[arg(long, value_name = "keep|flatten:n")]
    pub layout: Option<LayoutArg>,
    #[arg(long, value_name = "strict|normal|fast")]
    pub durability: Option<DurabilityChoice>,
    #[arg(long, value_name = "path")]
    pub lock: Option<PathBuf>,
    #[arg(long)]
    pub locked: bool,
    #[arg(long)]
    pub no_cache: bool,
    #[arg(long)]
    pub no_extract: bool,
    #[arg(long, value_name = "always|fingerprint|never")]
    pub verify: Option<VerifyChoice>,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub adopt: bool,
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub concurrency: Option<u32>,
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_host: Option<u32>,
    #[arg(long, value_name = "rate")]
    pub bandwidth: Option<RateArg>,
    #[arg(long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub retries: Option<u32>,
    #[arg(long, value_name = "duration")]
    pub timeout: Option<DurationArg>,
    #[arg(long, value_name = "auto|buffered|uncached")]
    pub io: Option<IoChoice>,
    #[arg(long)]
    pub aggressive: bool,
    #[arg(long)]
    pub deterministic_io: bool,
}

#[derive(Subcommand, Clone, Debug)]
pub enum CacheCommand {
    Status,
    Ls,
    Verify,
    Pin {
        #[arg(value_name = "digest")]
        digest: String,
    },
    Unpin {
        #[arg(value_name = "digest")]
        digest: String,
    },
    Export {
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    Import {
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    Repair,
    Prune,
    Clear,
}

#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    Get {
        #[arg(value_name = "ref")]
        reference: String,
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    Init {
        #[arg(value_name = "url|dir")]
        reference: String,
        #[arg(long, short, value_name = "path")]
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    Plan {
        #[arg(value_name = "ref")]
        reference: String,
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    Apply {
        #[arg(value_name = "plan")]
        plan: PathBuf,
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    Repair {
        #[arg(value_name = "ref")]
        reference: String,
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    Verify {
        #[arg(value_name = "path")]
        target: String,
    },
    Watch {
        #[arg(id = "watched", value_name = "events")]
        stream: String,
    },
    Completions {
        shell: Shell,
    },
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    Doctor,
    Why {
        #[arg(value_name = "ref")]
        reference: String,
    },
    Explain {
        key: Option<String>,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "fetchloom",
    version,
    about = "Turns a dataset reference into exact, verified local files.",
    disable_help_subcommand = true
)]
pub struct CommandLine {
    #[command(subcommand)]
    pub command: Command,
    #[command(flatten)]
    pub global: GlobalFlags,
}
