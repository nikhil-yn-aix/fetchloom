//! the manual page the release packages carry, docs/man/fetchloom.1. a test renders it from the
//! command line's definition, so the page lists the commands and options the help does.

use clap_mangen::Man;
use roff::{Roff, bold, roman};

/// the page as the definition renders it: the top command's synopsis, description and options,
/// then each command with what it does. it names no version, so only a change to the help
/// changes it.
fn rendered() -> String {
    let definition = crate::cli::definition();
    let commands: Vec<(String, String)> = definition
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| {
            let about = command.get_about().map(ToString::to_string);
            (command.get_name().to_owned(), about.unwrap_or_default())
        })
        .collect();
    let intro = definition
        .get_about()
        .map(ToString::to_string)
        .unwrap_or_default();
    // the name line takes the one-line summary and the description the help's opening. clap_mangen
    // would point each command at a page of its own, which the release does not ship
    let mut top = definition
        .about(env!("CARGO_PKG_DESCRIPTION"))
        .long_about(intro)
        .version(None::<&str>);
    for (name, _) in &commands {
        top = top.mut_subcommand(name, |command| command.hide(true));
    }
    let mut page = Vec::new();
    Man::new(top)
        .source("fetchloom")
        .manual("fetchloom manual")
        .render(&mut page)
        .unwrap();
    let mut rest = Roff::new();
    rest.control("SH", ["COMMANDS"]);
    for (name, about) in &commands {
        rest.control("TP", []).text([bold(name.as_str())]);
        for line in about.lines() {
            rest.text([roman(line)]);
        }
    }
    rest.control("SH", ["SEE ALSO"]).text([
        bold("fetchloom <command> --help"),
        roman(" lists what a command takes, and "),
        bold("fetchloom help <topic>"),
        roman(" explains concepts, recipes, the environment and exit codes. the guide is at "),
        roman("https://nikhil-yn-aix.github.io/fetchloom/"),
    ]);
    String::from_utf8(page).unwrap() + &rest.to_roff()
}

#[test]
fn the_manual_page_is_what_the_help_renders() {
    let page = rendered();
    if std::env::var_os("FETCHLOOM_TEST_WRITE_MANUAL").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/man/fetchloom.1");
        std::fs::write(path, &page).unwrap();
        return;
    }
    assert!(
        include_str!("../docs/man/fetchloom.1") == page,
        "docs/man/fetchloom.1 is not what the help renders: run \
         `FETCHLOOM_TEST_WRITE_MANUAL=1 cargo test --locked --lib manual` and commit the page"
    );
}
