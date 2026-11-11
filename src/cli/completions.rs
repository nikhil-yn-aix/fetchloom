//! `fetchloom completions`: a script that teaches a shell fetchloom's commands and options. the
//! script is text for the shell, so `--json` is a usage error rather than a document around it.

use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Instant;

use clap::CommandFactory;
use clap_complete::Shell;

use super::{Context, args, finish};
use crate::error::{Error, Kind};

/// prints the script for `shell` on stdout, or the usage error `--json` gets.
pub(super) fn print(context: &Context, shell: Shell, started: Instant) -> ExitCode {
    let outcome = if context.output.json() {
        Err(Error::new(
            Kind::Usage,
            "completions prints a script for a shell, which a JSON document cannot hold",
        )
        .help(format!(
            "run `fetchloom completions {shell}` without --json"
        )))
    } else {
        write(&script(shell))
    };
    finish(context, Some("completions"), outcome, started)
}

/// the completion script for `shell`, for a program on PATH as `fetchloom`, which completes
/// `fl` the same way: the short name installers make for the same program.
pub(super) fn script(shell: Shell) -> Vec<u8> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut args::Args::command(), "fetchloom", &mut script);
    with_short_name(shell, &String::from_utf8_lossy(&script)).into_bytes()
}

/// `script` with `fl` beside `fetchloom` wherever the shell names the commands it completes.
fn with_short_name(shell: Shell, script: &str) -> String {
    match shell {
        Shell::Bash => script.replace("-o default fetchloom\n", "-o default fetchloom fl\n"),
        // compinit reads the first line of a script in fpath; the last line serves one sourced
        Shell::Zsh => script
            .replacen("#compdef fetchloom\n", "#compdef fetchloom fl\n", 1)
            .replace(
                "compdef _fetchloom fetchloom\n",
                "compdef _fetchloom fetchloom fl\n",
            ),
        Shell::PowerShell => script.replacen(
            "-CommandName 'fetchloom' ",
            "-CommandName 'fetchloom', 'fl' ",
            1,
        ),
        Shell::Fish => format!("{script}complete -c fl -w fetchloom\n"),
        Shell::Elvish => format!(
            "{script}set edit:completion:arg-completer[fl] = $edit:completion:arg-completer[fetchloom]\n"
        ),
        _ => script.to_owned(),
    }
}

fn write(script: &[u8]) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(script).and_then(|()| stdout.flush()) {
        // a reader such as `head` that stops early wanted no more
        Err(err) if err.kind() != io::ErrorKind::BrokenPipe => {
            Err(Error::io("could not print the completion script", &err))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shell_completes_every_command() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
        ] {
            let script = String::from_utf8(script(shell)).unwrap();
            for command in ["add", "sync", "status", "gc", "verify", "completions"] {
                assert!(script.contains(command), "{shell} lacks {command}");
            }
            // fish names a long option without its dashes, as `-l offline`
            assert!(script.contains("offline"), "{shell} lacks --offline");
        }
    }
}
