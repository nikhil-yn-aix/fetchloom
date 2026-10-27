//! the command line as clap reads it. every heading and placeholder is lowercase, like the docs.

use clap::{ArgAction, Parser};

/// put the exact data a project needs onto any machine with one command
#[derive(Debug, Parser)]
#[command(
    name = "fetchloom",
    version,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    override_usage = "fetchloom [options]",
    help_template = "{about}\n\nusage: {usage}\n\n{all-args}"
)]
pub(crate) struct Args {
    /// print help
    #[arg(short, long, action = ArgAction::Help, global = true, help_heading = "options")]
    help: Option<bool>,

    /// print the version
    #[arg(short = 'V', long, action = ArgAction::Version, help_heading = "options")]
    version: Option<bool>,
}
