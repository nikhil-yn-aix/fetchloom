//! the fetchloom commands help, hints and `next:` lines name. after a run given `-C`, each names
//! the folder the run worked in, so it finds the same project from whatever folder it runs in.

use std::sync::OnceLock;

/// the folder a run given `-C` works in, quoted for a shell. unset without `-C`, as in tests.
static DIRECTORY: OnceLock<String> = OnceLock::new();

/// has every later [`command`] name `directory` after `-C`: the folder the run works in, quoted for
/// a shell.
pub(crate) fn set_directory(directory: String) {
    let _ = DIRECTORY.set(directory);
}

/// a command a help names, with `-C` and the folder the run worked in when the run was given `-C`,
/// so it finds the same project from whatever folder a person runs it in.
pub(crate) fn command(words: &str) -> String {
    match DIRECTORY.get() {
        Some(directory) => format!("fetchloom -C {directory} {words}"),
        None => format!("fetchloom {words}"),
    }
}
