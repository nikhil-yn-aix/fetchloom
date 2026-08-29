//! The command surface, configuration precedence, and the observers behind it.

pub mod config;

use clap_complete as _;
#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use tempfile as _;
pub mod explain;
pub mod materialize;
pub mod observer;
pub mod policy;
pub mod run;
pub mod settings;
pub mod surface;
pub mod terminal;
