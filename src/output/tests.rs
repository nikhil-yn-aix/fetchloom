use std::io::Write;
use std::time::Duration;

use anstyle::Style as Paint;

use super::screen::Screen;
use super::{Level, Output, Pieces, Style, accented, masked, plain, subject, write_answer};
use crate::cli::judged;
use crate::work::Counts;

#[test]
fn a_document_in_pieces_keeps_its_bytes_and_no_piece_outgrows_its_first_size() {
    let mut pieces = Pieces::default();
    let mut expected = Vec::new();
    let mut seed = 7_u32;
    // writes of 1 to 4,000 bytes, and one of 3 MiB, end at every place in a piece
    for round in 0..3_000_u32 {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let len = if round == 1_500 {
            3 << 20
        } else {
            1 + (seed >> 16) as usize % 4_000
        };
        let low = round.to_le_bytes()[0];
        let bytes: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i % 251).unwrap() ^ low)
            .collect();
        pieces.write_all(&bytes).unwrap();
        expected.extend_from_slice(&bytes);
    }
    let joined: Vec<u8> = pieces.slices().flatten().copied().collect();
    assert!(
        joined == expected,
        "the pieces hold {} bytes, not the {} written",
        joined.len(),
        expected.len()
    );
    let capacities: Vec<usize> = pieces
        .full
        .iter()
        .chain([&pieces.current])
        .map(Vec::capacity)
        .collect();
    assert_eq!(
        capacities[0],
        8 << 10,
        "a small document takes one small piece"
    );
    assert!(
        capacities[1..].iter().all(|&capacity| capacity == 1 << 20),
        "a piece that grew copied itself: {capacities:?}"
    );
}

#[test]
fn a_document_from_one_buffer_is_that_buffer() {
    let pieces = Pieces::from(b"{\"schema\":1}".to_vec());
    let joined: Vec<u8> = pieces.slices().flatten().copied().collect();
    assert_eq!(joined, b"{\"schema\":1}");
}

#[test]
fn answer_writes_and_flushes_propagate_failures_except_broken_pipes() {
    struct Sink {
        kind: std::io::ErrorKind,
        fail_write: bool,
        flushed: bool,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_write {
                Err(self.kind.into())
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushed = true;
            Err(self.kind.into())
        }
    }
    for kind in [
        std::io::ErrorKind::BrokenPipe,
        std::io::ErrorKind::StorageFull,
    ] {
        for fail_write in [false, true] {
            let mut sink = Sink {
                kind,
                fail_write,
                flushed: false,
            };
            let result = write_answer(&mut sink, |out| out.write_all(b"answer\n"));
            assert_eq!(sink.flushed, !fail_write);
            if kind == std::io::ErrorKind::BrokenPipe {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().kind, crate::error::Kind::LocalIo);
            }
        }
    }
    let mut bytes = Vec::new();
    write_answer(&mut bytes, |out| out.write_all(b"answer\n")).unwrap();
    assert_eq!(bytes, b"answer\n");
}

#[test]
fn help_wraps_rows_under_their_column_and_continues_long_examples() {
    let text = "options:\n  -o, --output <dir>  put the download in <dir> instead of a new folder named after it\n\nexamples:\n  fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom";
    let wrapped = super::wrap_help(text, 60, true);
    assert_eq!(
        wrapped,
        format!(
            "options:\n  -o, --output <dir>  put the download in <dir> instead of a\n                      new folder named after it\n\nexamples:\n  fetchloom completions bash{}\n    > ~/.local/share/bash-completion/completions/fetchloom",
            super::fit::MARK
        )
    );
    // prose wraps flush, one-letter words keep their spaces, and nothing is lost
    let prose = "a b c d e f g h i j k l m n o p q r s t u v w x y z a b c d e f g h";
    let wrapped = super::wrap_help(prose, 20, true);
    assert!(
        wrapped.lines().all(|line| line.chars().count() <= 20),
        "{wrapped}"
    );
    assert_eq!(
        wrapped.split_whitespace().collect::<Vec<_>>().join(" "),
        prose
    );
    // a name that is not ascii still lines its description up by characters
    let row = "  --données  une description assez longue pour passer à la ligne suivante";
    let wrapped = super::wrap_help(row, 40, true);
    let column = wrapped.lines().next().unwrap().find("une").unwrap();
    let first = &wrapped.lines().next().unwrap()[..column];
    assert!(
        wrapped.lines().skip(1).all(|line| {
            line.chars().take_while(|c| *c == ' ').count() == first.chars().count()
        }),
        "{wrapped}"
    );
    // a chinese name takes two columns for each character, so its row wraps by what it shows
    let row = "  --目录名称  a description long enough to go on to the next line";
    let wrapped = super::wrap_help(row, 40, true);
    let first = wrapped.lines().next().unwrap();
    let column = console::measure_text_width(&first[..first.find("a description").unwrap()]);
    assert!(
        wrapped
            .lines()
            .all(|line| console::measure_text_width(line) <= 40),
        "{wrapped}"
    );
    assert!(
        wrapped
            .lines()
            .skip(1)
            .all(|line| line.len() - line.trim_start().len() == column),
        "{wrapped}"
    );
}

/// a row whose name leaves too little room beside it, at a narrow terminal, takes a line of its
/// own with its description below, so the name never runs into the words that describe it.
#[test]
fn a_row_too_wide_for_its_column_puts_its_description_below() {
    let text = "options:\n  -o, --output <file>       write the downloaded file to <file> as it came\n      --force               replace a file already at <file>\n\n  fetchloom add iris    choose an iris dataset, put it in data/ and pin it";
    assert_eq!(
        super::wrap_help(text, 40, true),
        "options:\n  -o, --output <file>\n          write the downloaded file to\n          <file> as it came\n      --force\n          replace a file already at\n          <file>\n\n  fetchloom add iris\n          choose an iris dataset, put it\n          in data/ and pin it"
    );
    // a description below its name may start as a name does, with < or -, and stays plain
    let shown = super::help_text(&super::wrap_help(text, 40, true), true);
    assert!(
        shown.contains("  \u{1b}[36m-o, --output <file>\u{1b}[0m\n"),
        "{shown}"
    );
    assert!(shown.contains("\n          <file> as it came\n"), "{shown}");
}

/// at 60 columns comments go below their commands, every one in the block once one does not
/// fit, and a long command goes on after a shell's continuation mark. at 80 all fit as written.
#[test]
fn help_examples_wrap_as_a_shell_reads_them() {
    let examples = "examples:\n  fetchloom add iris    # at a terminal, pick one from a list\n  fetchloom add hf:datasets/scikit-learn/iris --select \"*.csv\" --dry-run\n  fetchloom add ./flowers    # files on this machine, copied into the cache\n  fetchloom add -i links.txt    # a dataset for each link in a list";
    let mark = if cfg!(windows) { '`' } else { '\\' };
    assert_eq!(
        super::wrap_help(examples, 60, true),
        format!(
            "examples:\n  fetchloom add iris\n    # at a terminal, pick one from a list\n  fetchloom add hf:datasets/scikit-learn/iris {mark}\n    --select \"*.csv\" --dry-run\n  fetchloom add ./flowers\n    # files on this machine, copied into the cache\n  fetchloom add -i links.txt\n    # a dataset for each link in a list"
        )
    );
    assert_eq!(super::wrap_help(examples, 80, true), examples);
    assert_eq!(super::wrap_help(examples, 60, false), examples);
    let wide = "examples:\n  fetchloom add ./鸢尾花数据集/鸢尾花.csv --as iris";
    let wrapped = super::wrap_help(wide, 40, true);
    assert!(
        wrapped
            .lines()
            .all(|line| console::measure_text_width(line) <= 40),
        "a wide character takes two columns:\n{wrapped}"
    );
}

/// a link wider than the terminal gets a line of its own.
#[test]
fn help_keeps_a_word_wider_than_the_terminal_whole() {
    let link = "https://huggingface.co/datasets/ILSVRC/imagenet-1k/resolve/main/train.tar";
    let text = format!("request access and accept the terms at {link} while signed in");
    let wrapped = super::wrap_help(&text, 40, true);
    assert!(wrapped.lines().any(|line| line == link), "{wrapped}");
    assert_eq!(
        wrapped.split_whitespace().collect::<Vec<_>>(),
        text.split_whitespace().collect::<Vec<_>>()
    );
}

/// fetchloom never wraps an error: each line of it is written whole, and a terminal narrower
/// than a line wraps it where its last column falls, even inside a word.
#[test]
fn an_error_is_written_one_line_for_each_part() {
    let said = "huggingface.co says: Access to dataset ILSVRC/imagenet-1k is restricted. You must have access to it and be authenticated to access it. Please log in.";
    let help = "request access and accept the terms at https://huggingface.co/datasets/ILSVRC/imagenet-1k while signed in, then sign in here with `fetchloom auth login hf`";
    let error = crate::error::Error::new(
        crate::error::Kind::AuthRequired,
        "hf:datasets/ILSVRC/imagenet-1k requires permission to download its files",
    )
    .detail(said)
    .help(help);
    let shown = super::error_text(&error, Level::Normal, false);
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        [
            "error: hf:datasets/ILSVRC/imagenet-1k requires permission to download its files",
            &format!("  {said}"),
            &format!("  help: {help}"),
        ]
    );
}

/// a stop the person asked for is no failure: a `Stopped` line in the warning color, its
/// supporting lines dim and its help a hint, never `error:`, even with -v.
#[test]
fn a_stop_is_a_plain_line_not_an_error() {
    use crate::error::{Error, Kind};
    let kept = "the 18.3 MiB downloaded so far is kept, and the next run continues from there";
    let stop = Error::new(Kind::Interrupted, "stopped while talking to 127.0.0.1")
        .detail(kept)
        .help("run `fetchloom sync` again");
    assert_eq!(
        super::failure_text(&stop, Level::Verbose, false)
            .lines()
            .collect::<Vec<_>>(),
        [
            "    Stopped while talking to 127.0.0.1",
            &format!("            {kept}"),
            "  hint: run `fetchloom sync` again",
        ]
    );
    for (message, shown) in [
        ("stopped", "    Stopped"),
        (
            "stopped, and nothing was added",
            "    Stopped, and nothing was added",
        ),
        ("the list closed", "    Stopped: the list closed"),
    ] {
        let stop = Error::new(Kind::Interrupted, message);
        assert_eq!(super::failure_text(&stop, Level::Normal, false), shown);
    }
    let warn = super::toned(true, super::Tone::Warn);
    let dim = Paint::new().dimmed();
    assert_eq!(
        super::failure_text(&stop, Level::Normal, true)
            .lines()
            .take(2)
            .collect::<Vec<_>>(),
        [
            format!("{warn}    Stopped{warn:#} while talking to 127.0.0.1"),
            format!("            {dim}{kept}{dim:#}"),
        ]
    );
    // a cancel at a question, which ctrl-c ends as interrupted too, still says what it cancelled
    let cancel = Error::new(Kind::Interrupted, "stopped").cancelled("the question");
    assert_eq!(
        super::failure_text(&cancel, Level::Normal, false),
        "  Cancelled the question"
    );
}

#[test]
fn a_pasted_secret_shows_only_its_ends_and_length() {
    assert_eq!(masked("q"), "* \u{b7} 1 character");
    assert_eq!(
        masked("KGAT_0123456789abcdef0123456789abcd"),
        "KGAT...abcd \u{b7} 35 characters"
    );
    assert_eq!(masked("hunter2"), "******* \u{b7} 7 characters");
    assert_eq!(masked(&"p".repeat(20)), "************ \u{b7} 20 characters");
}

#[test]
fn final_lines_share_the_verb_column_without_status_symbols() {
    for (verb, text, expected) in [
        (
            "Added",
            "iris, its files are in data/iris",
            "      Added iris, its files are in data/iris",
        ),
        (
            "Fetched",
            "iris.csv into iris",
            "    Fetched iris.csv into iris",
        ),
        (
            "Synced",
            "1,000 datasets into data",
            "     Synced 1,000 datasets into data",
        ),
        (
            "Ok",
            "everything matches data.lock",
            "         Ok everything matches data.lock",
        ),
    ] {
        let shown = super::verb_line(verb, text, super::Tone::Done, false);
        assert_eq!(shown, expected);
        assert!(!shown.contains('\u{1b}'));
    }
    assert_eq!(
        super::verb_line(
            "Added",
            "iris, its files are in data/iris",
            super::Tone::Done,
            true,
        ),
        "\u{1b}[1m\u{1b}[32m      Added\u{1b}[0m iris, its files are in data/iris"
    );
}

/// the fix a run's `--json` finds in `help`, with cli reading each quoted command.
fn fix_in(help: &str, locked: bool) -> Option<&str> {
    super::fix(help, locked, judged)
}

/// each rule in `fix`'s doc has a row: placeholders, examples, guesses, a person's terminal,
/// hand fixes first, builds, "if intended" upgrades and, when locked, lock writers are no fix.
#[test]
fn a_fix_is_the_first_runnable_command_in_the_help() {
    assert_eq!(
        fix_in("run `fetchloom add <link>` to start a project here", false),
        None
    );
    assert_eq!(
        fix_in("add --yes to download it without being asked", false),
        None
    );
    assert_eq!(fix_in("quote it, as in `\"fashion mnist\"`", false), None);
    assert_eq!(
        fix_in(
            "use a link or an id, as in `fetchloom add zenodo:3608990`",
            false
        ),
        None
    );
    assert_eq!(fix_in("did you mean `fetchloom remove iris`?", false), None);
    // a command named without what it needs is no fix a script could run
    assert_eq!(
        fix_in(
            "ask its owner for a public link to pass to `fetchloom add` or `fetchloom get`",
            false
        ),
        None
    );
    assert_eq!(
        fix_in(
            "in your own terminal, run `fetchloom auth login hf`. a script can pipe the key in, as in `fetchloom auth login hf --with-token`",
            false
        ),
        None
    );
    assert_eq!(
        fix_in(
            "delete git's conflict lines by hand, then run `fetchloom lock`",
            false
        ),
        None
    );
    assert_eq!(
        fix_in(
            "downloaded files stay in the cache. fix the errors above, then run `fetchloom sync` again",
            false
        ),
        None,
        "files that failed on their own fail again until someone fixes them"
    );
    assert_eq!(
        fix_in(
            "run `fetchloom build clean`, then commit data.lock and data.lock.d",
            false
        ),
        None,
        "a build runs the project's code"
    );
    assert_eq!(
        fix_in("run `fetchloom build -n clean` to see what it runs", false),
        Some("fetchloom build -n clean")
    );
    assert_eq!(
        fix_in(
            "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade iris`",
            false
        ),
        None,
        "only a person may take other bytes than data.lock pins"
    );
    // a run asked to leave data.lock as it is gets no fix that writes it, and keeps one that
    // does not
    for (help, fix) in [
        ("run `fetchloom lock` and commit data.lock", None),
        ("run `fetchloom sync iris`", None),
        (
            "run `fetchloom lock --check` to see what moved",
            Some("fetchloom lock --check"),
        ),
        (
            "run `fetchloom sync --locked` again later",
            Some("fetchloom sync --locked"),
        ),
    ] {
        assert_eq!(fix_in(help, true), fix, "{help}");
    }
    assert_eq!(
        fix_in("run `fetchloom lock` and commit data.lock", false),
        Some("fetchloom lock")
    );
}

/// signing in needs a person to paste the key, even when it is the one command a help names: an
/// agent that ran it only failed.
#[test]
fn signing_in_is_never_a_fix() {
    assert_eq!(
        fix_in(
            "if the dataset is private or restricted, sign in with `fetchloom auth login hf`, then run the same command again",
            false
        ),
        None
    );
}

/// which way to go is the person's call: a fix that pinned a folder named like the search they
/// meant copied it whole into the cache. an `or` within one way's clause offers no choice.
#[test]
fn a_help_that_offers_a_choice_has_no_fix() {
    for (help, fix) in [
        (
            "run `fetchloom add ./flowers` to pin it, or `fetchloom search flowers` to search",
            None,
        ),
        (
            "run `fetchloom add <link>` to start, or `fetchloom search iris`",
            None,
        ),
        (
            "restore data.lock and data.lock.d from git, or delete data.lock and run `fetchloom lock`, which resolves every dataset again and may pin newer data",
            None,
        ),
        (
            "the key in HF_TOKEN was refused, or has no access to it: `fetchloom auth` shows where it comes from and whose it is",
            Some("fetchloom auth"),
        ),
        (
            "close programs using the unreadable files or check their permissions, then run `fetchloom cache verify` again",
            Some("fetchloom cache verify"),
        ),
    ] {
        assert_eq!(fix_in(help, false), fix, "{help}");
    }
}

/// a fix never says yes for a person, and a help naming such a command first has none: an agent
/// ran the fix of an add whose name data.toml had and lost the dataset. a dry run only shows.
#[test]
fn a_fix_never_deletes_overwrites_repins_or_runs_code() {
    for help in [
        "give this one another name with --as <name>, or run `fetchloom remove notes` first",
        "to sign them out as well, run `fetchloom auth logout hf --yes`",
        "restore data.lock from git, or run `fetchloom lock --upgrade iris`, which resolves it again and may pin newer data",
        "`fetchloom lock --upgrade` looks for newer data, then `fetchloom sync`",
        "run `fetchloom push --yes` once everyone who reads the mirror may have the files",
    ] {
        assert_eq!(fix_in(help, false), None, "{help}");
    }
    assert_eq!(
        fix_in(
            "run `fetchloom remove notes --dry-run` to see what it takes out",
            false
        ),
        Some("fetchloom remove notes --dry-run")
    );
}

/// the table decides for every command, alone and with each option, so one added later is no
/// fix until someone decides here that a script may run it without asking a person.
#[test]
fn a_fix_is_only_a_command_the_table_allows() {
    // each command, whether it is a fix alone, and the options it stays one with
    let table: &[(&str, bool, &[&str])] = &[
        (
            "add",
            true,
            &[
                "--as",
                "--input-file",
                "--select",
                "--exclude",
                "--no-extract",
                "--dry-run",
            ],
        ),
        ("remove", false, &["--dry-run"]),
        ("drop", false, &["--dry-run"]),
        ("sync", true, &["--locked", "--strict", "--dry-run"]),
        ("lock", true, &["--check", "--dry-run"]),
        ("build", false, &["--dry-run"]),
        ("status", true, &["--verify", "--check"]),
        ("list", true, &[]),
        ("export", true, &["--format", "--output"]),
        ("push", true, &["--to", "--inputs", "--dry-run"]),
        ("path", true, &["--locked"]),
        ("cache", true, &[]),
        ("cache gc", false, &["--dry-run"]),
        ("cache verify", true, &[]),
        ("settings", true, &[]),
        (
            "get",
            true,
            &[
                "--output",
                "--dir",
                "--input-file",
                "--sha256",
                "--select",
                "--exclude",
                "--no-extract",
                "--dry-run",
            ],
        ),
        ("search", true, &["--source", "--limit"]),
        ("auth", true, &[]),
        ("auth login", false, &[]),
        ("auth logout", false, &[]),
        ("completions", true, &["--names"]),
        ("help", true, &[]),
    ];
    let mut walked = Vec::new();
    commands(&crate::cli::definition(), "", &mut walked);
    let named: Vec<&str> = table.iter().map(|(name, _, _)| *name).collect();
    let found: Vec<&str> = walked.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(found, named, "the table names every command, in order");
    for ((name, command), (_, alone, options)) in walked.iter().zip(table) {
        for (line, option) in lines(name, command) {
            let fixed = fix_in(&format!("run `{line}`"), false).is_some();
            let allowed = option.map_or(*alone, |option| options.contains(&option.as_str()));
            assert_eq!(fixed, allowed, "{line}");
        }
    }
}

/// every command under `parent`, named by its words, in the order help lists them.
fn commands(parent: &clap::Command, path: &str, found: &mut Vec<(String, clap::Command)>) {
    for command in parent.get_subcommands() {
        let name = format!("{path} {}", command.get_name()).trim().to_owned();
        found.push((name.clone(), command.clone()));
        commands(command, &name, found);
    }
}

/// command lines for `name`: alone, then with each option of its own beside it, given a value
/// or the option it needs, as `export --force` needs `--output`.
fn lines(name: &str, command: &clap::Command) -> Vec<(String, Option<String>)> {
    let values = |arg: &clap::Arg| -> Vec<String> {
        if !arg.get_action().takes_values() {
            return vec![String::new()];
        }
        match arg.get_possible_values().first() {
            Some(value) => vec![value.get_name().to_owned()],
            // a word, a number or a SHA-256, whichever the option reads
            None => vec!["x".to_owned(), "1".to_owned(), "0".repeat(64)],
        }
    };
    let options: Vec<&clap::Arg> = command
        .get_arguments()
        .filter(|arg| !arg.is_global_set())
        .filter(|arg| arg.get_long().is_some_and(|long| long != "help"))
        .collect();
    let flag = |arg: &clap::Arg| format!("--{}", arg.get_long().unwrap_or_default());
    // a positional only `--` reaches, as the words `completions --names` reads, would follow
    // every option, and no command needs one
    let positionals: Vec<String> = command
        .get_positionals()
        .filter(|arg| !arg.is_last_set())
        .map(|arg| values(arg).swap_remove(0))
        .collect();
    let mut companions = vec![String::new()];
    companions.extend(
        options
            .iter()
            .map(|other| format!("{} {}", flag(other), values(other).swap_remove(0))),
    );
    let mut tried = vec![(None, vec![String::new()])];
    tried.extend(options.iter().map(|arg| (Some(flag(arg)), values(arg))));
    tried
        .into_iter()
        .map(|(option, given)| {
            let mut candidates = Vec::new();
            for value in &given {
                for words in [positionals.join(" "), String::new()] {
                    for companion in &companions {
                        let option = option.clone().unwrap_or_default();
                        let line = format!("fetchloom {name} {words} {option} {value} {companion}");
                        candidates.push(line.split_whitespace().collect::<Vec<_>>().join(" "));
                    }
                }
            }
            let line = candidates
                .iter()
                .find(|line| judged(line).is_some())
                .unwrap_or_else(|| panic!("no line of {candidates:?} runs"))
                .clone();
            (line, option)
        })
        .collect()
}

/// report rows are colored as stdout decides, apart from stderr; quiet and `--json` print none.
#[test]
fn report_rows_follow_stdout_alone() {
    let style = |color| Style {
        color,
        unicode: true,
        interactive: false,
        watched: false,
        links: false,
        accessible: false,
    };
    let mut colored_story = Output::new(style(true), Level::Normal, false, false);
    colored_story.report_color = false;
    assert_eq!(
        colored_story
            .report_text("Missing", "data/iris/iris.csv")
            .as_deref(),
        Some("    Missing data/iris/iris.csv")
    );
    let mut plain_story = Output::new(style(false), Level::Normal, false, false);
    plain_story.report_color = true;
    let row = plain_story
        .report_text("Missing", "data/iris/iris.csv")
        .unwrap_or_default();
    assert!(row.contains("\u{1b}["), "{row:?}");
    assert_eq!(
        console::strip_ansi_codes(&row),
        "    Missing data/iris/iris.csv"
    );
    // a row is a difference whatever its verb, as a mark is, never a finished step, and
    // wrong bytes or an entry in the way are failures
    for (verb, tone) in [
        ("Removed", super::Tone::Warn),
        ("Unbuilt", super::Tone::Warn),
        ("Damaged", super::Tone::Fail),
        ("Blocked", super::Tone::Fail),
    ] {
        let shown = plain_story
            .report_text(verb, "data/iris/iris.csv")
            .unwrap_or_default();
        assert_eq!(
            shown,
            super::verb_line(verb, "data/iris/iris.csv", tone, true),
            "{verb}"
        );
        assert_eq!(
            super::verb_color(verb) == super::Tone::Fail,
            tone == super::Tone::Fail
        );
    }
    for (level, json) in [(Level::Quiet, false), (Level::Normal, true)] {
        let silent = Output::new(style(false), level, json, false);
        assert_eq!(silent.report_text("Missing", "data/iris/iris.csv"), None);
        silent.report("Missing", "data/iris/iris.csv");
    }
}

/// a placement from bytes the cache held is not a skipped download, and a file a one-off
/// `get -o` wrote to the cache, then moved out, is not said to be in it still.
#[test]
fn work_counters_say_what_happened_in_the_run() {
    let second = Duration::from_secs(2);
    let reused = Counts {
        requests: 1,
        bytes_down: 50_000_000,
        hardlinks: 1,
        placed_from_cache: 1,
        ..Counts::default()
    };
    assert_eq!(
        super::work_line(reused, second),
        "1 request · 47.7 MiB down · 1 hardlink · 1 placed from the cache · 2.0 s in all"
    );
    let one_off = Counts {
        requests: 1,
        bytes_down: 4551,
        objects_written: 1,
        copies: 1,
        ..Counts::default()
    };
    assert_eq!(
        super::work_line(one_off, second),
        "1 request · 4.4 KiB down · 1 file written to the cache · 1 copy · 2.0 s in all"
    );
}

/// a hash comparison fits 80 columns with its indent: each label leads its hash where both fit,
/// else stands above it. a lone hash reads as [`super::hashes_apart`] cuts it.
#[test]
fn a_hash_comparison_reads_whole_one_value_under_the_other() {
    let wrong = "0".repeat(64);
    let got = "492d5ea49605a5bb7e1c3c4d8b1f0f6c2a9d3e7b5c1a0f9e8d7c6b5a49382716";
    let (stated, md5) = ("6eff3450105497cc2ce22ea267f564ba", &got[..32]);
    let cases = [
        (
            format!("stated md5 {stated}, got {md5}"),
            2,
            vec![format!("stated md5 {stated}"), format!("got        {md5}")],
        ),
        (
            format!("expected sha256 {wrong}, got {got}"),
            2,
            vec![format!("expected {wrong}"), format!("got      {got}")],
        ),
        (
            format!("expected sha256 {wrong}, the source states {got}"),
            2,
            vec![
                "expected:".to_owned(),
                format!("  {wrong}"),
                "the source states:".to_owned(),
                format!("  {got}"),
            ],
        ),
        (
            format!("--sha256 names {wrong}, data/iris/iris.csv holds {got}"),
            2,
            vec![
                "--sha256 names:".to_owned(),
                format!("  {wrong}"),
                "data/iris/iris.csv holds:".to_owned(),
                format!("  {got}"),
            ],
        ),
        // under a `Stopped` line, whose details start at its twelfth column
        (
            format!("expected sha256 {wrong}, got {got}"),
            12,
            vec![
                "expected:".to_owned(),
                format!("  {wrong}"),
                "got:".to_owned(),
                format!("  {got}"),
            ],
        ),
        (
            format!("got {got}."),
            2,
            vec![format!("got {}.", &got[..12])],
        ),
    ];
    for (line, indent, shown) in cases {
        let read = super::readable(&line, indent);
        assert_eq!(read, shown, "{line}");
        assert!(
            read.iter().all(|line| indent + line.chars().count() <= 80),
            "{read:?}"
        );
    }
}

#[test]
fn hashes_on_a_line_read_apart_however_late_they_differ() {
    let hash = "e54cc4d2ce1bff65d32ca60b3e4b802e06bde1d7e7caf6f796f6bf7370e863b1";
    let at = |index: usize| {
        let mut other = hash.to_owned();
        other.replace_range(
            index..=index,
            if &hash[index..=index] == "0" {
                "1"
            } else {
                "0"
            },
        );
        other
    };
    let (late, early) = (at(40), at(3));
    assert_eq!(
        super::hashes_apart(&format!("expected sha256 {hash}, got {late}")),
        format!("expected sha256 {}, got {}", &hash[..41], &late[..41])
    );
    assert_eq!(
        super::hashes_apart(&format!("expected sha256 {hash}, got {early}")),
        format!("expected sha256 {}, got {}", &hash[..12], &early[..12])
    );
    assert_eq!(
        super::hashes_apart(&format!("got {hash}.")),
        format!("got {}.", &hash[..12])
    );
    for untouched in [
        format!("could not read /cache/builds/{hash}/lock"),
        format!("sha256:{hash}"),
        format!("{hash}0 is no hash"),
        "expected 32 bytes, got 15".to_owned(),
    ] {
        assert_eq!(super::hashes_apart(&untouched), untouched);
    }
}

#[test]
fn the_wordmark_shows_only_where_it_fits() {
    assert_eq!(super::wordmark(24, false, true), "");
    assert_eq!(
        super::wordmark(25, false, true),
        "█▀ ▄█▀ ▄█▀ ▄\n ▄█▀ ▄█▀ ▄█▀   fetchloom\n█▀ ▄█▀ ▄█▀ ▄\n\n"
    );
    assert!(super::wordmark(25, true, true).contains('\u{1b}'));
}

/// a terminal that shows only ascii gets the twill one row a line, in `#`.
#[test]
fn the_wordmark_falls_back_to_ascii() {
    assert_eq!(
        super::wordmark(25, false, false),
        "##  ##  ##\n#  ##  ##  #   fetchloom\n  ##  ##  ##\n\n"
    );
}

/// `Would` ends where every other verb does, however long its action.
#[test]
fn a_dry_run_verb_sits_in_the_verb_column() {
    let text = "1 file from data/ that no dataset needs";
    assert_eq!(
        super::verb_line("Would remove", text, super::Tone::Work, false),
        format!("      Would remove {text}")
    );
    assert_eq!(
        super::verb_line("Removed", text, super::Tone::Done, false),
        format!("    Removed {text}")
    );
}

#[test]
fn step_colors_follow_the_work_being_reported() {
    use super::Tone::{Accent, Done, Fail, Warn, Work};

    for (verbs, expected) in [
        (
            &["Downloaded", "Unpacked", "Locked", "Placed", "Removed"][..],
            Done,
        ),
        (
            &[
                "Downloading",
                "Unpacking",
                "Waiting",
                "Searching",
                "Would place",
                "Would fetch",
                "Would remove",
            ][..],
            Work,
        ),
        (&["1", "12"][..], Accent),
        (
            &[
                "Missing",
                "Extra",
                "Kept",
                "Edited",
                "Untracked",
                "Outdated",
            ][..],
            Warn,
        ),
        (&["Failed"][..], Fail),
    ] {
        for verb in verbs {
            assert_eq!(super::verb_color(verb), expected, "{verb}");
        }
    }
}

#[test]
fn error_and_help_are_plain_when_color_is_off() {
    let error = crate::error::Error::new(crate::error::Kind::OfflineMissing, "iris is missing")
        .detail("iris.csv is not in the cache")
        .help("run `fetchloom sync` again");
    let shown = super::error_text(&error, Level::Normal, false);
    assert_eq!(
        shown,
        "error: iris is missing\n  iris.csv is not in the cache\n  help: run `fetchloom sync` again"
    );
    assert!(!shown.contains('\u{1b}'));
    let verbose = super::error_text(&error, Level::Verbose, false);
    assert!(verbose.ends_with("\n  kind: offline.missing"));
    assert!(!verbose.contains('\u{1b}'));
}

#[test]
fn error_and_help_color_the_labels_and_dim_the_details() {
    let error = crate::error::Error::new(crate::error::Kind::OfflineMissing, "iris is missing")
        .detail("iris.csv is not in the cache")
        .help("run `fetchloom sync` again");
    assert_eq!(
        super::error_text(&error, Level::Normal, true),
        concat!(
            "\u{1b}[1m\u{1b}[31merror:\u{1b}[0m iris is missing",
            "\n  \u{1b}[2miris.csv is not in the cache\u{1b}[0m",
            "\n  \u{1b}[1m\u{1b}[36mhelp:\u{1b}[0m run ",
            "\u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m again",
        )
    );
}

#[test]
fn hints_are_dim_with_commands_in_bold_cyan() {
    let text = "try `fetchloom path iris` next";
    assert_eq!(
        super::hint_line(text, false),
        "  hint: try `fetchloom path iris` next"
    );
    assert_eq!(
        super::hint_line(text, true),
        concat!(
            "  \u{1b}[2mhint: try \u{1b}[0m\u{1b}[1m\u{1b}[36m",
            "`fetchloom path iris`\u{1b}[0m\u{1b}[2m next\u{1b}[0m",
        )
    );
}

#[test]
fn help_styles_headings_and_commands_without_changing_plain_text() {
    let text = concat!(
        "usage: fetchloom get [options] <link>\n\ncommands:\n",
        "  get  fetch once\n\nexamples:\n  fetchloom get zenodo:1\n",
        "\n  data.toml, the shopping list: which datasets you want\n",
    );
    assert_eq!(super::help_text(text, false), text);
    let shown = super::help_text(text, true);
    assert!(shown.contains("\u{1b}[1musage:\u{1b}[0m"));
    assert!(shown.contains("\u{1b}[1mcommands:\u{1b}[0m"));
    assert!(shown.contains("\u{1b}[36mfetchloom get zenodo:1\u{1b}[0m"));
    assert!(shown.contains("  \u{1b}[36mget\u{1b}[0m  fetch once"));
    assert!(shown.contains("data.toml\u{1b}[0m, the shopping list: which datasets you want"));
}

#[test]
fn the_first_ctrl_c_at_a_question_is_interrupted() {
    for key in [
        Ok(console::Key::CtrlC),
        Err(std::io::ErrorKind::Interrupted.into()),
    ] {
        let mut keys = std::iter::once(key);
        let error = super::answer(|| keys.next().unwrap(), |_| {}, false).unwrap_err();
        assert_eq!(error.kind, crate::error::Kind::Interrupted);
        assert_eq!(error.kind.exit_code(), 130);
        assert_eq!(error.message, "stopped");
        assert_eq!(error.cancelled.as_deref(), Some("the question"));
    }
}

/// y, yes, n and no answer in any case; q typed to quit is asked again, never taken as a yes.
#[test]
fn an_answer_other_than_yes_or_no_is_asked_again_not_taken_as_the_default() {
    for default in [true, false] {
        for (typed, said) in [
            ("y", Some(true)),
            ("Yes", Some(true)),
            (" yes ", Some(true)),
            ("n", Some(false)),
            ("NO", Some(false)),
            ("", Some(default)),
            ("q", None),
            ("quit", None),
            ("yeah", None),
            ("y n", None),
        ] {
            let mut keys = typed
                .chars()
                .map(console::Key::Char)
                .chain([console::Key::Enter])
                .map(Ok);
            let read = super::answer(|| keys.next().unwrap(), |_| {}, default);
            assert_eq!(read.unwrap(), said, "{typed:?}, {default} by default");
        }
    }
}

/// a closed terminal counts as the end of the question's input.
#[test]
fn a_question_whose_terminal_closed_is_answered_no() {
    let closed = || Err(std::io::ErrorKind::UnexpectedEof.into());
    assert_eq!(super::answer(closed, |_| {}, true).unwrap(), Some(false));
}

#[test]
fn commands_in_backticks_take_the_accent_and_keep_their_backticks() {
    let dim = Paint::new().dimmed();
    assert_eq!(
        accented("run `fetchloom sync` again", Paint::new(), true),
        "run \u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m again"
    );
    assert_eq!(
        accented("commit it; then `fetchloom sync`", dim, true),
        "commit it; then \u{1b}[0m\u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m\u{1b}[2m"
    );
    assert_eq!(
        accented("an odd ` mark", Paint::new(), true),
        "an odd ` mark"
    );
}

#[test]
fn a_step_names_its_subject_in_bold() {
    assert_eq!(
        subject("iris · 1 file in data.lock", true),
        "\u{1b}[1miris\u{1b}[0m\u{1b}[2m · 1 file in data.lock\u{1b}[0m"
    );
    assert_eq!(
        subject("data/iris/iris.csv", true),
        "\u{1b}[1mdata/iris/iris.csv\u{1b}[0m"
    );
    assert_eq!(
        subject("for another fetchloom working in this project", true),
        "for another fetchloom working in this project"
    );
}

#[test]
fn server_text_cannot_steer_the_terminal() {
    assert_eq!(plain("iris.csv · 4.1 KiB"), "iris.csv · 4.1 KiB");
    assert_eq!(plain("\u{1b}[2Jgone"), "\\u{1b}[2Jgone");
    assert_eq!(plain("two\nlines"), "two\\nlines");
    assert_eq!(plain("\u{9b}2J"), "\\u{9b}2J");
    // bidi controls would show a line's text in another order than its bytes
    for (text, shown) in [
        ("report\u{202e}txt.exe", "report\\u{202e}txt.exe"),
        (
            "a\u{61c}b\u{200e}c\u{200f}d",
            "a\\u{61c}b\\u{200e}c\\u{200f}d",
        ),
        (
            "\u{202a}\u{202b}\u{202c}\u{202d}",
            "\\u{202a}\\u{202b}\\u{202c}\\u{202d}",
        ),
        (
            "\u{2066}x\u{2067}y\u{2068}z\u{2069}",
            "\\u{2066}x\\u{2067}y\\u{2068}z\\u{2069}",
        ),
        ("données \u{200e}· كلمة", "données \\u{200e}· كلمة"),
    ] {
        assert_eq!(plain(text), shown, "{text:?}");
    }
}

#[test]
fn the_next_step_is_only_for_a_person_at_a_terminal() {
    let style = |watched| Style {
        color: false,
        unicode: true,
        interactive: false,
        watched,
        links: false,
        accessible: false,
    };
    let shows = |watched, level, json| Output::new(style(watched), level, json, false).shows_next();
    assert!(shows(true, Level::Normal, false));
    assert!(
        !Output::new(style(true), Level::Normal, false, false)
            .without_hints(true)
            .shows_next(),
        "FETCHLOOM_NO_HINTS=1"
    );
    assert!(shows(true, Level::Verbose, false));
    assert!(!shows(true, Level::Quiet, false), "-q");
    assert!(!shows(true, Level::Normal, true), "--json");
    assert!(
        !shows(false, Level::Normal, false),
        "stdout or stderr is not a terminal"
    );
}

#[test]
fn bars_keep_moving_while_a_question_waits() {
    let style = Style {
        color: false,
        unicode: false,
        interactive: true,
        watched: true,
        links: false,
        accessible: false,
    };
    let output = Output::new(style, Level::Normal, false, false);
    let bars = output.bars.as_ref().unwrap();
    let bar = bars.add(indicatif::ProgressBar::new(1000));
    let moved = output.asking(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let other = bar.clone();
        std::thread::spawn(move || {
            for _ in 0..100 {
                other.inc(1);
                other.tick();
            }
            let _ = sender.send(());
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok()
    });
    assert!(moved, "a download stood still while the question waited");
    assert_eq!(bar.position(), 100);
}

#[test]
fn a_typed_line_ends_at_enter_and_ctrl_c_cancels_it() {
    use console::Key::{Backspace, Char, CtrlC, Enter, Tab};

    use super::Edit::{Erased, Typed};
    fn keys(
        keys: Vec<std::io::Result<console::Key>>,
    ) -> impl FnMut() -> std::io::Result<console::Key> {
        let mut keys = keys.into_iter();
        move || keys.next().unwrap_or(Ok(Enter))
    }
    let mut edits = Vec::new();
    let line = super::typed_line(
        keys(vec![
            Ok(Backspace),
            Ok(Char('a')),
            Ok(Char('b')),
            Ok(Backspace),
            Ok(Char('é')),
            Ok(Char(' ')),
            Ok(Tab),
            Ok(Char('\u{4}')),
            Ok(Enter),
            Ok(Char('x')),
        ]),
        |edit| edits.push(edit),
    )
    .unwrap();
    assert_eq!(line, "aé ");
    assert_eq!(
        edits,
        [Typed('a'), Typed('b'), Erased('b'), Typed('é'), Typed(' ')]
    );
    for stop in [Ok(CtrlC), Err(std::io::ErrorKind::Interrupted.into())] {
        let error = super::typed_line(keys(vec![Ok(Char('s')), stop]), |_| {}).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }
    let error = super::typed_line(
        keys(vec![Err(std::io::ErrorKind::UnexpectedEof.into())]),
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
}

fn telling(interactive: bool) -> Output {
    let style = Style {
        color: false,
        unicode: false,
        interactive,
        watched: interactive,
        links: false,
        accessible: false,
    };
    let mut output = Output::new(style, Level::Normal, false, false);
    output.bars = None;
    output
}

/// plain batch lines wait for answers too, so none writes over a question in accessible output.
#[test]
fn lines_told_during_a_question_wait_for_the_answer() {
    let output = telling(false);
    output.asking(|| {
        output.step("Resolving", "iris");
        let _batch = output.ticker("Downloading", "iris.csv".to_owned(), [Some(81)]);
        assert_eq!(
            output.said(),
            Vec::<String>::new(),
            "nothing before the answer"
        );
    });
    let said = output.said();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].contains("Resolving iris"), "{said:?}");
    assert!(said[1].contains("Downloading iris.csv"), "{said:?}");
}

/// a terminal drops what it had not yet shown at a ctrl-c. a closing line told after one, or
/// with no ctrl-c, is told once.
#[test]
fn a_closing_line_before_a_finishing_ctrl_c_is_told_again() {
    let added = "      Added iris, its files are in data/iris";
    let finishing = "  Finishing what it started (ctrl-c again to quit at once)";
    let late = telling(false);
    late.done("Added", "iris, its files are in data/iris");
    late.finishing();
    late.retell();
    assert_eq!(late.said(), [added, finishing, added]);
    let early = telling(false);
    early.finishing();
    early.done("Added", "iris, its files are in data/iris");
    early.retell();
    assert_eq!(early.said(), [finishing, added]);
    let calm = telling(false);
    calm.done("Added", "iris, its files are in data/iris");
    calm.retell();
    assert_eq!(calm.said(), [added]);
}

/// a terminal echoes `^C` where its cursor waits, so a line told then clears it first and every
/// line starts at its own start, keeping the verb column; a pipe gets the lines alone.
#[test]
fn a_line_told_at_a_ctrl_c_clears_the_echo() {
    let stopping = "   Stopping (press ctrl-c again to quit at once)";
    let terminal = telling(true);
    terminal.alone().stopping();
    terminal.alone().finishing_downloads(1);
    terminal.done("Added", "late, its files are in data/late");
    assert_eq!(
        terminal.said(),
        [
            format!("\r\u{1b}[2K{stopping}"),
            "\r\u{1b}[2K  Finishing (the download is complete; ctrl-c again to quit at once)"
                .to_owned(),
            "\r      Added late, its files are in data/late".to_owned(),
        ]
    );
    let piped = telling(false);
    piped.alone().stopping();
    assert_eq!(piped.said(), [stopping]);
}

/// a ctrl-c after a SIGTERM is told as a ctrl-c.
#[test]
fn a_line_told_at_a_sigterm_names_a_second_sigterm() {
    let output = telling(false);
    output.interrupted_by(super::Interrupt::Terminate);
    output.alone().stopping();
    output.finishing();
    output.alone().finishing_downloads(2);
    output.interrupted_by(super::Interrupt::CtrlC);
    output.alone().stopping();
    assert_eq!(
        output.said(),
        [
            "   Stopping (a second SIGTERM quits at once)",
            "  Finishing what it started (a second SIGTERM quits at once)",
            "  Finishing (the downloads are complete; a second SIGTERM quits at once)",
            "   Stopping (press ctrl-c again to quit at once)",
        ]
    );
}

/// a fixed line once stood still through a whole wait; a log gets one plain line.
#[test]
fn a_wait_for_another_program_counts_on_a_terminal() {
    let text = "for `fetchloom add` (pid 7) working in this project";
    let mut terminal = telling(false);
    terminal.bars = Some(indicatif::MultiProgress::with_draw_target(
        indicatif::ProgressDrawTarget::term_like(Box::new(Screen::default())),
    ));
    let live = terminal.waiting("Waiting", text);
    assert_eq!(live.told().as_deref(), Some(text));
    assert!(terminal.said().is_empty(), "{:?}", terminal.said());
    let piped = telling(false);
    let none = piped.waiting("Waiting", text);
    assert_eq!(none.told(), None);
    assert_eq!(piped.said(), [format!("    Waiting {text}")]);
}

/// placing many files once went untold for seconds after `Imported`.
#[test]
fn lasting_work_is_told_once_it_lasts() {
    let mut terminal = telling(false);
    let screen = Screen::default();
    terminal.bars = Some(indicatif::MultiProgress::with_draw_target(
        indicatif::ProgressDrawTarget::term_like(Box::new(screen.clone())),
    ));
    terminal.untold = Duration::ZERO;
    let placing = terminal.lasting("Placing", "20,000 files".to_owned());
    assert!(screen.shows("Placing 20,000 files"), "no line");
    drop(placing);
}

#[test]
fn a_run_that_ends_at_once_clears_its_bars_first() {
    let style = Style {
        color: false,
        unicode: false,
        interactive: false,
        watched: false,
        links: false,
        accessible: false,
    };
    let mut output = Output::new(style, Level::Normal, false, false);
    let screen = Screen::default();
    output.bars = Some(indicatif::MultiProgress::with_draw_target(
        indicatif::ProgressDrawTarget::term_like(Box::new(screen.clone())),
    ));
    let bars = output.bars.as_ref().unwrap();
    let bar = bars.add(indicatif::ProgressBar::new(10));
    bar.set_message("iris.csv");
    bar.tick();
    assert!(!bars.is_hidden());
    let drawn = screen.records().len();
    assert!(drawn > 0);

    output.abandon();

    let after = screen.records()[drawn..].to_vec();
    assert!(after.contains(&"clear".to_owned()), "{after:?}");
    assert!(bars.is_hidden());
    bar.inc(5);
    bar.tick();
    assert_eq!(screen.records().len(), drawn + after.len());
}
