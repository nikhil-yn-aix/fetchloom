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
    /// fetch something once, without a project
    #[command(override_usage = "fetchloom get [options] <link>", help_template = TEMPLATE)]
    Get(Get),
}

/// fetch something once, without a project
#[derive(Debug, clap::Args)]
pub(crate) struct Get {
    /// what to download: any link you would paste in a browser
    #[arg(value_name = "link", help_heading = "arguments")]
    pub(crate) link: String,

    /// put the download in <dir> instead of a new folder named after it
    #[arg(short, long, value_name = "dir", help_heading = "options")]
    pub(crate) output: Option<PathBuf>,
}
