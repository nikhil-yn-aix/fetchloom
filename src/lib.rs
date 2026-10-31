//! fetchloom puts the exact data a project needs onto any machine with one command.
//!
//! the `fetchloom` binary calls [`main`]. everything else is private to the crate.

mod archive;
mod cache;
mod cli;
mod digest;
mod error;
mod glob;
mod name;
mod net;
mod output;
mod place;
mod plan;
mod project;
mod source;
mod stop;
mod threads;
mod units;
mod volume;
mod work;

/// runs fetchloom with the arguments of the current process and returns its exit code.
#[must_use]
pub fn main() -> std::process::ExitCode {
    cli::run()
}
