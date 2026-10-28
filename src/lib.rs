//! fetchloom puts the exact data a project needs onto any machine with one command.
//!
//! the `fetchloom` and `fl` binaries both call [`main`]. everything else is private to the crate.
#![expect(
    dead_code,
    reason = "the sources and the project loop are being built on these parts"
)]

mod cache;
mod cli;
mod digest;
mod error;
mod name;
mod net;
mod output;
mod place;
mod source;
mod stop;
mod units;
mod work;

/// runs fetchloom with the arguments of the current process and returns its exit code.
#[must_use]
pub fn main() -> std::process::ExitCode {
    cli::run()
}
