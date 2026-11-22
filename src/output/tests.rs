use std::io::Write;
use std::time::{Duration, Instant};

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

/// the fix a run's `--json` finds in `help`, with cli reading each quoted command, each one as if
/// this run wrote it.
fn fix_in(help: &str, locked: bool) -> Option<&str> {
    super::fix(help, locked, judged, |_| true)
}

/// a name in a help may spell out a command between backticks, as a code path from a cloned
/// data.toml can: only a command this run wrote is a fix, and one spelled out first leaves none.
#[test]
fn a_fix_is_only_a_command_this_run_wrote() {
    let written = crate::suggested::command("lock --check --offline");
    let help = format!("bring x back; `{written}` shows what it pins");
    assert_eq!(
        super::fix(&help, false, judged, crate::suggested::is_written),
        Some(written.as_str())
    );
    let spelled = format!("bring x`fetchloom audit --check`y back; `{written}` shows what it pins");
    assert_eq!(
        super::fix(&spelled, false, judged, crate::suggested::is_written),
        None
    );
}

/// each rule in `fix`'s doc has a row: placeholders, examples, guesses, a person's terminal,
/// hand fixes first, "if intended" upgrades and, when locked, lock writers are no fix.
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
            "to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run this command again with `--mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade iris`",
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
            "run `fetchloom sync --locked --dry-run` to see what it places",
            Some("fetchloom sync --locked --dry-run"),
        ),
        ("run `fetchloom sync --locked` again later", None),
    ] {
        assert_eq!(fix_in(help, true), fix, "{help}");
    }
    // a lock may download a file to pin it, so only its check or dry run is a fix
    assert_eq!(
        fix_in("run `fetchloom lock` and commit data.lock", false),
        None
    );
    assert_eq!(
        fix_in("run `fetchloom lock --dry-run` to see what it pins", false),
        Some("fetchloom lock --dry-run")
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
            "a program holds the unreadable files or their permissions keep them from you: `fetchloom cache verify` checks them again",
            Some("fetchloom cache verify"),
        ),
    ] {
        assert_eq!(fix_in(help, false), fix, "{help}");
    }
}

/// a command after a step the person takes first is no fix, as the command that failed is once a
/// file is mended: an agent that ran it at once failed the same way, again and again.
#[test]
fn a_command_after_a_step_of_the_persons_is_no_fix() {
    for help in [
        "fix that line, then run `fetchloom status --json` again",
        "save it as UTF-8, then run `fetchloom list` again",
        "close programs using the unreadable files or check their permissions, then run `fetchloom cache verify` again",
        "rename it to \"iris\" in data.toml and run `fetchloom status` again",
        "move the folder aside, then `fetchloom status` says what is missing",
    ] {
        assert_eq!(fix_in(help, false), None, "{help}");
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
        ("add", false, &["--dry-run"]),
        ("remove", false, &["--dry-run"]),
        ("exclude", false, &["--dry-run"]),
        ("drop", false, &["--dry-run"]),
        ("sync", false, &["--dry-run"]),
        ("lock", false, &["--check", "--dry-run"]),
        ("status", true, &["--verify", "--check"]),
        ("audit", true, &["--check"]),
        ("diff", true, &[]),
        ("list", true, &[]),
        ("export", true, &["--format"]),
        ("push", false, &["--dry-run"]),
        ("path", false, &[]),
        ("cache", true, &[]),
        ("cache gc", false, &["--dry-run"]),
        ("cache verify", true, &[]),
        ("settings", true, &[]),
        ("settings set", false, &[]),
        ("settings unset", false, &[]),
        ("get", false, &["--dry-run"]),
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

/// report rows are colored as stdout decides, apart from stderr; quiet keeps them, as the answer,
/// and `--json` prints none.
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
        ("Waiting", super::Tone::Warn),
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
    // a row is the answer, which -q keeps, and JSON holds it in its own document
    let mut quiet = Output::new(style(false), Level::Quiet, false, false);
    quiet.report_color = false;
    assert_eq!(
        quiet
            .report_text("Missing", "data/iris/iris.csv")
            .as_deref(),
        Some("    Missing data/iris/iris.csv")
    );
    let json = Output::new(style(false), Level::Normal, true, false);
    assert_eq!(json.report_text("Missing", "data/iris/iris.csv"), None);
    json.report("Missing", "data/iris/iris.csv");
}

/// a status row once broke inside `data.lock` at an 80-column terminal, which got each row whole:
/// at a terminal a row wraps between words under its verb, as a story line does, and a script
/// reading a pipe still gets each row on one line.
#[test]
fn a_row_wraps_at_a_terminals_width_and_stays_whole_in_a_pipe() {
    let style = |watched| Style {
        color: false,
        unicode: true,
        interactive: watched,
        watched,
        links: false,
        accessible: false,
    };
    let text = "rat · not in data.toml, so `fetchloom sync` takes it out of data.lock";
    let rows = |output: &Output| {
        let mut rows = Vec::new();
        output
            .write_rows(&mut rows, "Removed", std::iter::once(text))
            .unwrap();
        String::from_utf8(rows).unwrap()
    };
    let mut terminal = Output::new(style(true), Level::Normal, false, false);
    terminal.bars = None;
    terminal.report_color = false;
    terminal.answer_columns = Some(|| Some(80));
    assert_eq!(
        rows(&terminal),
        "    Removed rat · not in data.toml, so `fetchloom sync` takes it out of\n            data.lock\n"
    );
    let mut piped = Output::new(style(false), Level::Normal, false, false);
    piped.report_color = false;
    assert_eq!(rows(&piped), format!("    Removed {text}\n"));
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
        format!("could not read /cache/unpacked/{hash}/data.csv"),
        format!("sha256:{hash}"),
        format!("{hash}0 is no hash"),
        "expected 32 bytes, got 15".to_owned(),
    ] {
        assert_eq!(super::hashes_apart(&untouched), untouched);
    }
}

/// the pin's head hangs before the name, with the purpose under the name, where both lines fit.
#[test]
fn the_pins_head_hangs_before_the_name_only_where_it_fits() {
    let purpose = "exact research data for every project";
    let title = |width, color| super::pinned_title("0.1.0", purpose, width, color, true);
    assert_eq!(title(39, false), "");
    assert_eq!(
        title(40, false),
        format!("\u{2022} fetchloom 0.1.0\n  {purpose}\n\n")
    );
    assert!(title(40, true).contains('\u{1b}'));
}

/// a terminal that shows only ascii gets no mark: the name, with the purpose under it.
#[test]
fn a_terminal_without_unicode_gets_the_name_alone() {
    assert_eq!(
        super::pinned_title("0.1.0", "exact data", 80, false, false),
        "fetchloom 0.1.0\nexact data\n\n"
    );
}

/// under `LANG=C` a cron log once read `Saved retries - not set → 5`, and writing every `→` as
/// `->` then changed `x→y.csv` in the command a warning suggests: fetchloom's symbols have ascii
/// forms, its arrow chosen as a line is made, and a name's own letters stay as they are.
#[test]
fn a_terminal_that_shows_only_ascii_gets_every_symbol_in_ascii() {
    let line = "Saved retries \u{b7} a\u{2026} \u{203a} \u{2501} r\u{e9}sum\u{e9}";
    assert_eq!(
        super::shown(line, false),
        "Saved retries - a... > - r\u{e9}sum\u{e9}"
    );
    assert_eq!(super::shown(line, true), line);
    let suggested = "to keep your own, run `fetchloom exclude data/sim/x\u{2192}y.csv`";
    assert_eq!(super::shown(suggested, false), suggested);
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
        "\nglobal options:\n  -q, --quiet  print less\n",
        "\nin a project you cloned:\n  fetchloom sync    put the pinned files in data/\n",
        "\n  data.lock   each file's size and SHA-256\n",
    );
    assert_eq!(super::help_text(text, false), text);
    let shown = super::help_text(text, true);
    for heading in [
        "usage:",
        "commands:",
        "global options:",
        "in a project you cloned:",
    ] {
        assert!(
            shown.contains(&format!("\u{1b}[1m{heading}\u{1b}[0m")),
            "{heading}"
        );
    }
    assert!(shown.contains("\u{1b}[36mfetchloom get zenodo:1\u{1b}[0m"));
    assert!(shown.contains("  \u{1b}[36mget\u{1b}[0m  fetch once"));
    assert!(shown.contains("data.toml\u{1b}[0m, the shopping list: which datasets you want"));
    assert!(shown.contains("  \u{1b}[1mdata.lock\u{1b}[0m   each file's size and SHA-256"));
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

/// a resized terminal once got every later line wrapped for the width the run began at, which the
/// terminal then broke inside words: each line wraps at the width the terminal has as it is told.
#[test]
fn a_line_wraps_at_the_width_the_terminal_has_as_it_is_told() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static WIDTH: AtomicUsize = AtomicUsize::new(200);
    let output = telling(false).fitted(Some(|| Some(WIDTH.load(Ordering::Relaxed))));
    let text = "s9.bin - 2.0 MiB in 5.2 s - 393.2 KiB/s";
    output.step("Downloaded", text);
    WIDTH.store(30, Ordering::Relaxed);
    output.step("Downloaded", text);
    assert_eq!(
        output.said(),
        [
            format!(" Downloaded {text}"),
            " Downloaded s9.bin - 2.0 MiB\n            in 5.2 s -\n            393.2 KiB/s"
                .to_owned()
        ]
    );
}

/// plain batch lines wait for answers too, so none writes over a question in accessible output.
#[test]
fn lines_told_during_a_question_wait_for_the_answer() {
    let output = telling(false);
    output.asking(|| {
        output.step("Resolving", "iris");
        let _batch = output.ticker("Searching", "zenodo".to_owned(), [None]);
        assert_eq!(
            output.said(),
            Vec::<String>::new(),
            "nothing before the answer"
        );
    });
    let said = output.said();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].contains("Resolving iris"), "{said:?}");
    assert!(said[1].contains("Searching zenodo"), "{said:?}");
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

/// a wait on an owner record another system wrote lost its verb and, at 80 columns, the middle of
/// the path to delete, and a ctrl-c erased it: words too long for a line are told whole once, the
/// live line counting under them.
#[test]
fn a_wait_too_long_for_a_line_is_told_whole_and_counts_below() {
    let owner = "/datadisk/fl-perf/qa-ux/p5-adversary/work/conc/cc/.fetchloom/owner";
    let text = format!(
        "for `fetchloom sync` (windows, pid 4242, started 2026-11-19T02:00:00Z) working in this project; if that process is gone, delete {owner}"
    );
    let mut terminal = telling(false).fitted(Some(|| Some(80)));
    terminal.bars = Some(indicatif::MultiProgress::with_draw_target(
        indicatif::ProgressDrawTarget::term_like(Box::new(Screen::default())),
    ));
    let live = terminal.waiting("Waiting", &text);
    assert_eq!(live.told().as_deref(), Some(""));
    let said = terminal.said();
    assert_eq!(said.len(), 1, "{said:?}");
    let lines: Vec<&str> = said[0].lines().collect();
    assert!(
        lines[0].starts_with("    Waiting for `fetchloom sync` (windows, pid 4242,"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.trim_start() == owner),
        "{lines:#?}"
    );
    assert!(lines.iter().all(|line| line.len() <= 80), "{lines:#?}");
    let short = "for `fetchloom add` (pid 7) working in this project";
    let quick = terminal.waiting("Waiting", short);
    assert_eq!(
        quick.told().as_deref(),
        Some(short),
        "a line that fits stays live"
    );
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

/// a step told at once says so as work, then neither it nor the work's plain lines say it again;
/// under `-q` or `--json` it says nothing and leaves saying it to those lines, which `--progress`
/// keeps beside `--json`.
#[test]
fn a_step_told_at_once_is_said_once() {
    let output = telling(false);
    let lasting = output.lasting("Placing", "2 files".to_owned());
    output.beginning(&lasting.begun(), "Placing", "2 files");
    output.beginning(&lasting.begun(), "Placing", "2 files");
    assert_eq!(output.said(), ["    Placing 2 files"]);
    assert!(!lasting.begun().first());

    for (level, json) in [(Level::Quiet, false), (Level::Normal, true)] {
        let style = Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let output = Output::new(style, level, json, false).with_progress(true);
        let lasting = output.lasting("Placing", "2 files".to_owned());
        output.beginning(&lasting.begun(), "Placing", "2 files");
        assert_eq!(output.said(), Vec::<String>::new());
        assert!(lasting.begun().first(), "{level:?} {json}");
    }
}

/// work a command reaches once it has lasted its moment is told at once, however long before
/// it the command spent elsewhere, and work reached sooner waits out the rest of that moment.
#[test]
fn work_reached_past_the_command_s_moment_is_told_at_once() {
    let hour = Duration::from_secs(3600);
    let started = Instant::now();
    let screen = Screen::default();
    let mut output = Output::on_screen_after(screen.clone(), hour);
    output.started = started;
    let reading = output.lasting_at("Reading", "./flowers".to_owned(), started + 2 * hour);
    assert!(screen.shows("Reading ./flowers"), "{:?}", screen.records());
    drop(reading);

    let screen = Screen::default();
    let mut output = Output::on_screen_after(screen.clone(), hour);
    output.started = started;
    drop(output.lasting_at("Reading", "./flowers".to_owned(), started + hour / 2));
    assert_eq!(screen.records(), Vec::<String>::new());
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

/// a match whose title has no room beside what to type puts the title on a line under it, then
/// its facts, dim, on one more.
#[test]
fn a_match_without_room_for_its_title_puts_it_under() {
    let output = Output::new(
        Style {
            color: false,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        },
        Level::Normal,
        false,
        false,
    );
    *output.held.lock().unwrap() = Some(Vec::new());
    output.candidate_under(
        "arxiv:1706.03762",
        "Attention Is All You Need",
        "no open-access pdf",
        false,
    );
    output.candidate_under("arxiv:1810.04805", "BERT", "", false);
    let lines = output.held.lock().unwrap().take().unwrap();
    assert_eq!(
        lines,
        [
            "            arxiv:1706.03762",
            "            Attention Is All You Need",
            "            no open-access pdf",
            "            arxiv:1810.04805",
            "            BERT",
        ]
    );
    // on a list too narrow for some id past the verb column, every id starts near the edge,
    // and a size padded for its column sits under the title, not past it
    *output.held.lock().unwrap() = Some(Vec::new());
    output.candidate_under("kaggle:uciml/iris", "Iris Species", "  3.6 KiB", true);
    output.candidate_under("hf:datasets/scikit-learn/iris", "", " 18.1 KiB", true);
    let lines = output.held.lock().unwrap().take().unwrap();
    assert_eq!(
        lines,
        [
            "  kaggle:uciml/iris",
            "    Iris Species",
            "    3.6 KiB",
            "  hf:datasets/scikit-learn/iris",
            "    18.1 KiB",
        ]
    );
}

/// an author line a server gave shows its control characters as escapes beside a title, as it
/// does under one, so a surname cannot steer the terminal.
#[test]
fn a_candidates_facts_beside_its_title_show_control_characters_escaped() {
    let output = Output::new(
        Style {
            color: false,
            unicode: true,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        },
        Level::Normal,
        false,
        false,
    );
    *output.held.lock().unwrap() = Some(Vec::new());
    output.candidate(
        "arxiv:1706.03762",
        16,
        "Attention Is All You Need",
        ("Lovelace\u{1b}]0;x\u{7} et al. 2017", false),
    );
    let lines = output.held.lock().unwrap().take().unwrap();
    assert_eq!(
        lines,
        [
            "            arxiv:1706.03762  Attention Is All You Need  Lovelace\\u{1b}]0;x\\u{7} et al. 2017"
        ]
    );
}

/// every character past ASCII leaves as an escape, a surrogate pair past U+FFFF, so a reader that
/// decodes stdout with another code page, as powershell 5.1 does with the console's, reads the
/// same document; it parses back to the same names.
#[test]
fn json_leaves_as_ascii_and_reads_back_the_same() {
    let named = serde_json::json!({"file": "数据/数据.csv", "note": "ok 🧪", "数据": [1]});
    for indented in [false, true] {
        let pieces = super::encoded(&named, indented).unwrap();
        let bytes: Vec<u8> = pieces.slices().flatten().copied().collect();
        assert!(bytes.is_ascii(), "{}", String::from_utf8_lossy(&bytes));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r"\u6570\u636e.csv"), "{text}");
        assert!(text.contains(r"\ud83e\uddea"), "{text}");
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, named);
    }
}

/// a name withheld from a command on windows that reaches a line no help went through still shows
/// as a placeholder, named on a line under it, so no line holds what a shell there would run.
#[test]
fn a_withheld_name_on_a_plain_line_is_named_under_it() {
    let output = telling(false);
    let marked = crate::suggested::shell_word_for("a&b.example", true);
    output.step("Signed", &format!("in to {marked}"));
    let said = output.said();
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].ends_with(r#"Signed in to "<name>""#), "{said:?}");
    assert_eq!(
        said[1].trim_start(),
        "<name> is a&b.example, which no command here can hold safely"
    );
}

/// marks a file's own name holds are no withheld word: the line names the file as it is, the
/// marks written out, with no placeholder and no line under it.
#[test]
fn marks_a_name_holds_are_written_out_on_a_line() {
    let output = telling(false);
    output.step("Put", "back data/z/c\u{f8e0}d\u{f8e1} z.csv");
    let said = output.said();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].ends_with(r"Put back data/z/c\u{f8e0}d\u{f8e1} z.csv"),
        "{said:?}"
    );
}

/// a command naming the run's folder, which windows withholds when it holds `&`, as the status
/// lines do after `-C`.
fn naming_a_withheld_folder() -> String {
    let folder = crate::suggested::shell_word_for(r"C:\Users\ada\R&D\proj", true);
    format!("fetchloom -C {folder} sync")
}

/// what the placeholder of [`naming_a_withheld_folder`] stands for, as the one line says it.
const NAMED_FOLDER: &str =
    r"<name> is C:\Users\ada\R&D\proj, which no command here can hold safely";

/// a warning's message, help and details name a withheld folder by one placeholder, in its text
/// and in `--json` alike, and one line says which folder it stands for.
#[test]
fn a_warning_names_a_withheld_folder_alike_in_each_part() {
    let output = telling(false);
    let command = naming_a_withheld_folder();
    output.warn_detailed(
        super::Warning::ArchivePacked,
        &format!("`{command}` places data/iris"),
        Some(&format!("run `{command}`")),
        [format!("`{command}` places it")],
    );
    let shown = r#"fetchloom -C "<name>" sync"#;
    let warned = output.warnings.lock().unwrap()[0].clone();
    assert_eq!(warned.message, format!("`{shown}` places data/iris"));
    assert_eq!(
        warned.help.as_deref(),
        Some(format!("run `{shown}`").as_str())
    );
    assert_eq!(
        warned.details,
        [format!("`{shown}` places it"), NAMED_FOLDER.to_owned()]
    );
    let said = output.said().join("\n");
    assert!(!said.contains('\u{f8e0}'), "{said}");
    assert!(!said.contains("<name 2>"), "{said}");
    assert_eq!(said.matches(NAMED_FOLDER).count(), 1, "{said}");
}

/// an error's message, details and help name a withheld folder by one placeholder, told and in
/// `--json` alike, and one line says which folder it stands for; a failure of its own says so
/// again, since a script reads each failure apart.
#[test]
fn an_error_names_a_withheld_folder_alike_in_each_part() {
    use crate::error::{Error, FileFailure, Kind};
    let command = naming_a_withheld_folder();
    let mut error = Error::new(Kind::NotReady, format!("`{command}` cannot finish"))
        .detail(format!("`{command}` places data/iris"))
        .help(format!("run `{command}`"));
    let text = super::failure_text(&error, Level::Normal, false);
    assert!(!text.contains('\u{f8e0}'), "{text}");
    assert!(!text.contains("<name 2>"), "{text}");
    assert_eq!(text.matches(NAMED_FOLDER).count(), 1, "{text}");
    error.failures = Box::new([FileFailure {
        file: "data/iris/iris.csv".to_owned(),
        kind: "network.status",
        message: "not found".to_owned(),
        details: Vec::new(),
        help: Some(format!("run `{command}`")),
    }]);
    let told = error.told();
    let shown = r#"fetchloom -C "<name>" sync"#;
    assert_eq!(told.message, format!("`{shown}` cannot finish"));
    assert_eq!(
        told.help.as_deref(),
        Some(format!("run `{shown}`").as_str())
    );
    assert_eq!(
        told.details,
        [
            format!("`{shown}` places data/iris"),
            NAMED_FOLDER.to_owned()
        ]
    );
    assert_eq!(told.failures[0].help, Some(format!("run `{shown}`")));
    assert_eq!(told.failures[0].details, [NAMED_FOLDER]);
}

/// a row on stdout names a withheld folder by its placeholder too, and the line saying which
/// folder it stands for comes once in a run, after the first row that shows it.
#[test]
fn rows_on_stdout_name_a_withheld_folder_once() {
    let output = telling(false);
    let command = naming_a_withheld_folder();
    let row = format!("    Missing data/iris/iris.csv · `{command}` places it");
    let shown = r#"    Missing data/iris/iris.csv · `fetchloom -C "<name>" sync` places it"#;
    assert_eq!(
        output.answered(&row),
        [shown.to_owned(), format!("        {NAMED_FOLDER}")]
    );
    assert_eq!(output.answered(&row), [shown.to_owned()]);
}

/// a download's closing line names the time a slow disk took after its last byte, unless a test
/// turned off what only the machine's speed decides, as `FETCHLOOM_TEST_TICKER=off` does.
#[test]
fn a_slow_disk_is_named_unless_a_test_turned_timed_lines_off() {
    let style = Style {
        color: false,
        unicode: false,
        interactive: false,
        watched: false,
        links: false,
        accessible: false,
    };
    let mut output = Output::new(style, Level::Normal, false, false);
    let took = crate::units::Took {
        arrived: Duration::from_secs(1),
        all: Duration::from_secs(3),
    };
    output.moment = Some(Duration::from_secs(2));
    assert_eq!(
        crate::units::downloaded([(81, 81)], output.took(took)),
        "81 B in 3.0 s, 2.0 s of it waiting for the disk · 81 B/s"
    );
    output.moment = None;
    assert_eq!(
        crate::units::downloaded([(81, 81)], output.took(took)),
        "81 B in 1.0 s · 81 B/s"
    );
}

/// a writer that keeps what it was given and the last byte of each write that gave it.
#[derive(Default)]
struct Counted {
    bytes: Vec<u8>,
    ends: Vec<u8>,
}

impl Write for Counted {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.ends.extend(buf.last());
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// status printed 100,001 missing files in 100,001 writes. rows written together go out a few
/// writes for 10,000 rows, each write ending with a whole row.
#[test]
fn rows_written_together_go_out_in_few_writes_of_whole_rows() {
    let output = telling(false);
    let texts: Vec<String> = (0..10_000).map(|n| format!("data/s/f{n}.csv")).collect();
    let mut counted = Counted::default();
    output
        .write_rows(&mut counted, "Missing", texts.iter().map(String::as_str))
        .unwrap();
    let mut expected = String::new();
    for text in &texts {
        expected.push_str("    Missing ");
        expected.push_str(text);
        expected.push('\n');
    }
    assert_eq!(String::from_utf8(counted.bytes).unwrap(), expected);
    let writes = counted.ends.len();
    assert!(
        (2..=expected.len() / super::ROWS_AT_ONCE + 1).contains(&writes),
        "{writes} writes for {} bytes",
        expected.len()
    );
    assert!(
        counted.ends.iter().all(|end| *end == b'\n'),
        "{:?}",
        counted.ends
    );
}
