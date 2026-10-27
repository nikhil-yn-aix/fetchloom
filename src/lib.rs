//! fetchloom puts the exact data a project needs onto any machine with one command.
//!
//! the `fetchloom` and `fl` binaries both call [`main`]. everything else is private to the crate.

mod cli;
mod error;
mod output;

/// runs fetchloom with the arguments of the current process and returns its exit code.
#[must_use]
pub fn main() -> std::process::ExitCode {
    cli::run()
}
