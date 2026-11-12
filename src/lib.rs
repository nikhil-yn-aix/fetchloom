//! fetchloom puts the exact data a project needs onto any machine with one command.
//!
//! the `fetchloom` binary calls [`main`]. everything else is private to the crate.

mod archive;
mod auth;
mod build;
mod cache;
mod cli;
mod digest;
mod error;
mod glob;
mod machine;
mod mirror;
mod name;
mod net;
mod output;
mod place;
mod plan;
mod project;
mod recipe;
mod settings;
mod source;
mod space;
mod stop;
mod threads;
mod units;
mod volume;
mod work;

#[cfg(test)]
mod manual;
#[cfg(test)]
#[path = "../tests/support/paths.rs"]
mod test_paths;
#[cfg(test)]
#[path = "../tests/support/skips.rs"]
mod test_skips;

/// runs fetchloom with the arguments of the current process and returns its exit code.
#[must_use]
#[expect(
    clippy::print_stderr,
    reason = "a crashing program has nothing but stderr left to say it on"
)]
pub fn main() -> std::process::ExitCode {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let rust = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            eprintln!(
                "fetchloom stopped on a bug of its own, not anything you did. please report it at \
                 https://github.com/nikhil-yn-aix/fetchloom/issues with the command you ran and the \
                 output of `fetchloom --version -v`. what it said:"
            );
            rust(info);
        }));
    });
    cli::run()
}
