//! `fetchloom help`: the ideas behind fetchloom, or the help of one command. like `--help`, it
//! prints text for people, even with `--json`.

use std::io::Write as _;
use std::process::ExitCode;
use std::time::Instant;

use clap::CommandFactory;
use clap::error::ErrorKind;

use super::{Context, args, finish};
use crate::error::{Error, Kind};
use crate::name;

/// `fetchloom help concepts`: what a beginner needs to know, in the same kitchen as the first run.
const CONCEPTS: &str = "fetchloom concepts

fetchloom keeps the data a project reads exact, and the same on every machine. it works like a
kitchen:

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by every project on this machine
  sync, restocking: data/ ends up matching the receipt

a dataset is one entry in data.toml: a name, such as iris, and a reference to where it is
published, such as zenodo:3608990, a DOI or a link. --select and --exclude choose which of its
files to keep.

a fingerprint is a file's SHA-256 hash. data.lock records one for every file, so every machine
gets the same bytes. when a source changes a file, sync stops and says so instead of using it.

the cache keeps each file once, under its fingerprint. data/ links to it where the disk allows,
so projects that use the same file store it once. files in data/ are read-only by default.

commit data.toml and data.lock with your code, and leave data/ out. anyone who clones the
project runs `fetchloom sync` and gets the same files. `fetchloom status` shows what differs.

`fetchloom get` fetches a dataset once, with no list and no receipt, for a quick look.";

/// prints the topic, or the help of the command `topic` names. an unknown topic is a usage error
/// that names no command, since help is never one.
pub(super) fn show(context: &Context, topic: &[String], started: Instant) -> ExitCode {
    if let [only] = topic
        && only == "concepts"
    {
        let _ = writeln!(std::io::stdout().lock(), "{CONCEPTS}");
        return ExitCode::SUCCESS;
    }
    let line = std::iter::once("fetchloom")
        .chain(topic.iter().map(String::as_str))
        .chain(std::iter::once("--help"));
    match args::Args::command().try_get_matches_from(line) {
        Err(help) if help.kind() == ErrorKind::DisplayHelp => {
            let _ = help.print();
            ExitCode::SUCCESS
        }
        _ => finish::<()>(context, None, Err(unknown(topic)), started),
    }
}

/// the error for a topic that is neither `concepts` nor a command, suggesting the closest one.
fn unknown(topic: &[String]) -> Error {
    let asked = topic.join(" ");
    let mut topics = vec!["concepts".to_owned()];
    for command in args::Args::command().get_subcommands() {
        topics.push(command.get_name().to_owned());
        for inner in command.get_subcommands() {
            topics.push(format!("{} {}", command.get_name(), inner.get_name()));
        }
    }
    let error = Error::new(
        Kind::Usage,
        format!("there is no help topic called \"{asked}\""),
    );
    match name::closest(&asked, topics.iter().map(String::as_str)) {
        Some(best) => error.help(format!("did you mean `fetchloom help {best}`?")),
        None => error.help("`fetchloom help concepts` explains the ideas, and `fetchloom --help` lists the commands"),
    }
}
