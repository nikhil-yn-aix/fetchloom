//! The command surface, configuration precedence, and the observers behind it.

pub mod cache;
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

/// Writes a failure where the caller asked for it and returns its exit code.
///
/// Takes the failure and whether the result is machine readable. Returns the
/// code the layer of that failure maps to.
#[must_use]
pub fn report(
    error: &fetchloom_engine::error::Error,
    json: bool,
) -> fetchloom_engine::outcome::ExitCode {
    if json {
        match serde_json::to_string(error) {
            Ok(body) => println!("{body}"),
            Err(_) => eprintln!("{error}"),
        }
    } else {
        eprintln!("{error}");
        eprintln!("next: {}", error.next_action());
    }
    fetchloom_engine::outcome::ExitCode::from(error.layer())
}
