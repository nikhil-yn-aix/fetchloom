//! the version, the welcome, help and usage errors.

use serde_json::Value;

use crate::support::{Result, Sandbox, text};

#[test]
fn version() -> Result {
    let output = Sandbox::new()?.run(&["--version"])?;
    assert!(output.status.success());
    assert_eq!(
        text(&output.stdout)?,
        format!("fetchloom {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

#[test]
fn help_is_lowercase() -> Result {
    let output = Sandbox::new()?.run(&["--help"])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

#[test]
fn help_follows_color_flags_anywhere() -> Result {
    let sandbox = Sandbox::new()?;
    for (arguments, colored) in [
        (&["--color", "always", "--help"][..], true),
        (&["--help", "--color", "always"], true),
        (&["--color", "always", "get", "-h"], true),
        (&["get", "--color=always", "--help"], true),
        (&["cache", "gc", "-h", "--color=always"], true),
        (&["--color", "always", "help", "get"], true),
        (&["help", "cache", "gc", "--color", "always"], true),
        (&["--color", "never", "--help"], false),
        (&["get", "-h", "--color=never"], false),
        (&["help", "get", "--color", "never"], false),
        (&["--help", "--", "--color", "always"], false),
        (&["--help", "--color", "invalid"], false),
    ] {
        let output = sandbox
            .command(arguments)
            .env("NO_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .output()?;
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            text(&output.stderr)?
        );
        let stdout = text(&output.stdout)?;
        assert_eq!(stdout.contains("\x1b["), colored, "{arguments:?}: {stdout}");
        assert!(stdout.contains("usage:"), "{arguments:?}: {stdout}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
    }
    Ok(())
}

#[test]
fn help_follows_color_environment() -> Result {
    let sandbox = Sandbox::new()?;
    for arguments in [
        &["--help"][..],
        &["get", "-h"],
        &["help", "get"],
        &["cache", "gc", "--help"],
        &["help", "cache", "gc"],
    ] {
        // CLICOLOR_FORCE colors even into a pipe, as the convention says; NO_COLOR still wins
        for (no_color, force, colored) in [("1", "1", false), ("", "1", true), ("", "0", false)] {
            let output = sandbox
                .command(arguments)
                .env("NO_COLOR", no_color)
                .env("CLICOLOR_FORCE", force)
                .output()?;
            assert!(
                output.status.success(),
                "{arguments:?}: {}",
                text(&output.stderr)?
            );
            let stdout = text(&output.stdout)?;
            assert_eq!(
                stdout.contains("\x1b["),
                colored,
                "{arguments:?}, NO_COLOR={no_color}, CLICOLOR_FORCE={force}: {stdout}"
            );
        }
    }
    Ok(())
}

/// the welcome and `help concepts` follow color flags. only `help concepts` tells the analogy;
/// the welcome names each thing itself.
#[test]
fn welcome_and_concepts_follow_color_flags() -> Result {
    let sandbox = Sandbox::new()?;
    for (arguments, colored) in [
        (&["--color", "always"][..], true),
        (&["--color", "never"][..], false),
        (&[][..], false),
        (&["help", "concepts", "--color", "always"][..], true),
        (&["help", "concepts", "--color", "never"][..], false),
    ] {
        let output = sandbox
            .command(arguments)
            .env("NO_COLOR", "1")
            .env("CLICOLOR_FORCE", "1")
            .output()?;
        assert!(output.status.success(), "{arguments:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}");
        let stdout = text(&output.stdout)?;
        assert_eq!(stdout.contains("\x1b["), colored, "{arguments:?}: {stdout}");
        let concepts = arguments.first() == Some(&"help");
        for analogy in [
            "the shopping list",
            "the receipt",
            "the pantry",
            "restocking",
        ] {
            assert_eq!(
                stdout.contains(analogy),
                concepts,
                "{arguments:?}: {stdout}"
            );
        }
        if colored {
            assert!(stdout.contains("\x1b[1mfetchloom\x1b[0m"), "{stdout}");
            assert!(stdout.contains("\x1b[36m"), "{stdout}");
        }
    }
    Ok(())
}

#[test]
fn a_palette_recolors_output_and_a_typo_only_warns() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox
        .command(&["help", "concepts", "--color", "always"])
        .env("FETCHLOOM_COLORS", "accent=magenta:done=teal")
        .output()?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert!(stdout.contains("\x1b[35m"), "{stdout}");
    assert!(!stdout.contains("\x1b[36m"), "{stdout}");
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("FETCHLOOM_COLORS: kept the default for done=teal\n"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn help_json_teaches_every_command_in_one_call() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["help", "--json"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(document["command"], "help");
    let commands = document["result"]["commands"]
        .as_array()
        .ok_or("no commands")?;
    for name in ["add", "sync", "get", "search", "cache gc", "auth login"] {
        let command = commands
            .iter()
            .find(|command| command["name"] == name)
            .ok_or(name)?;
        assert!(
            command["usage"]
                .as_str()
                .is_some_and(|usage| usage.starts_with(&format!("fetchloom {name}"))),
            "{name}"
        );
        assert!(
            command["examples"]
                .as_array()
                .is_some_and(|examples| !examples.is_empty()),
            "{name}"
        );
    }
    let options = document["result"]["options"]
        .as_array()
        .ok_or("no options")?;
    assert!(options.iter().any(|option| option["name"] == "--json"));
    assert!(
        options
            .iter()
            .any(|option| option["name"] == "--version" && option["short"] == "-V")
    );
    let one = serde_json::from_slice::<Value>(&sandbox.run(&["help", "get", "--json"])?.stdout)?;
    assert_eq!(one["result"]["commands"].as_array().map(Vec::len), Some(1));
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));
    Ok(())
}

#[test]
fn json_errors_carry_the_command_that_fixes_them() -> Result {
    let sandbox = Sandbox::new()?;
    let document: Value = serde_json::from_slice(&sandbox.run(&["--json", "sycn"])?.stdout)?;
    // a guess is said in help, never handed to a script to run
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom sync`?"
    );
    assert_eq!(document["errors"][0]["fix"], Value::Null);
    // an agent reads stdout as JSON whatever the terminal says, so no escape code may reach it
    for args in [
        &["--json", "--color", "always", "sycn"][..],
        &["--json", "--color", "always", "auth", "--offline"],
        &["--json", "--color", "always", "help"],
    ] {
        let output = sandbox.run(args)?;
        assert!(!output.stdout.contains(&0x1b), "{args:?}");
        serde_json::from_slice::<Value>(&output.stdout)?;
    }
    let document: Value = serde_json::from_slice(&sandbox.run(&["--json", "status"])?.stdout)?;
    assert_eq!(document["errors"][0]["kind"], "project.invalid");
    assert!(
        document["errors"][0]["fix"].is_null(),
        "a command with a placeholder is not runnable"
    );
    Ok(())
}

#[test]
fn get_has_its_own_help() -> Result {
    let output = Sandbox::new()?.run(&["get", "--help"])?;
    assert!(output.status.success());
    insta::assert_snapshot!(text(&output.stdout)?);
    Ok(())
}

#[test]
fn the_options_scope_names_are_the_ones_add_and_get_take() -> Result {
    let scope = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs")
            .join("user/commands/README.md"),
    )?;
    let options = scope
        .lines()
        .find(|line| line.starts_with("every command takes"))
        .ok_or("commands/README.md names no options")?;
    let sandbox = Sandbox::new()?;
    for (commands, intro) in [
        (
            &[
                &["add"][..],
                &["get"],
                &["sync"],
                &["status"],
                &["search"],
                &["cache", "gc"],
            ][..],
            "every command takes ",
        ),
        (
            &[&["add"][..], &["remove"], &["sync"], &["cache", "gc"]][..],
            "`add`, `remove`, `sync` and `cache gc` also take ",
        ),
        (&[&["add"][..], &["get"]], "`add` and `get` also take "),
        (&[&["add"][..]], "`add` takes "),
        (&[&["get"][..]], "`get` takes "),
    ] {
        let (_, rest) = options.split_once(intro).ok_or(intro)?;
        // a sentence lists one command's options, or two joined by ", and `get` takes"
        let named = rest
            .split(". ")
            .next()
            .and_then(|sentence| sentence.split(", and `get` takes").next())
            .unwrap_or_default();
        for command in commands {
            let help = text(&sandbox.run(&[*command, &["--help"]].concat())?.stdout)?;
            for option in named.split('`').skip(1).step_by(2) {
                let flag = option.split(' ').next().unwrap_or(option);
                assert!(
                    help.contains(&format!("{flag} ")) || help.contains(&format!("{flag},")),
                    "commands/README.md says {} takes {flag}",
                    command.join(" ")
                );
            }
        }
    }
    Ok(())
}

/// the analogy `help concepts` explains the ideas with, and only it.
pub(crate) const ANALOGY: [&str; 4] = [
    "data.toml, the shopping list: which datasets you want",
    "data.lock, the receipt: the exact files and their fingerprints",
    "the cache, the pantry: shared by projects using it",
    "sync, restocking: fill data/ from the receipt",
];

/// what a beginner meets on the first run and at the top of `--help`: each thing by its own name,
/// and `pin` explained where it first appears.
const NAMED: [&str; 4] = [
    "data.toml  the datasets you want",
    "pin        record a dataset's exact files and their SHA-256 in data.lock",
    "data.lock  the list of pinned files, so every machine gets the same bytes",
    "the cache  one folder that keeps each file once, for all your projects",
];

#[test]
fn welcome() -> Result {
    let output = Sandbox::new()?.run(&[])?;
    assert!(output.status.success());
    let stdout = text(&output.stdout)?;
    assert!(stdout.lines().count() <= 17, "{stdout}");
    assert!(
        stdout.contains(&format!("\n  {}\n", NAMED.join("\n  "))),
        "{stdout}"
    );
    for line in ANALOGY {
        assert!(!stdout.contains(line), "{line}");
    }
    assert!(
        stdout.contains("\n  fetchloom add iris "),
        "the welcome has a first command to copy"
    );
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.contains("`fetchloom help concepts`")),
        "the welcome ends with where to learn more"
    );
    let mut settings = insta::Settings::clone_current();
    settings.add_filter(r"^fetchloom \d+\.\d+\.\d+\S*", "fetchloom [version]");
    settings.bind(|| insta::assert_snapshot!(stdout));
    Ok(())
}

#[test]
fn the_readme_quick_start_is_the_first_run() -> Result {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"),
    )?
    .replace("\r\n", "\n");
    let (_, start) = readme
        .split_once("## quick start\n")
        .ok_or("README.md has no quick start")?;
    let (_, block) = start
        .split_once("```text\n")
        .ok_or("the quick start shows no first run")?;
    let (shown, _) = block.split_once("```").ok_or("the first run never ends")?;
    let welcome = text(&Sandbox::new()?.run(&[])?.stdout)?;
    // the version line is left out, so a release never dates the readme
    let (_, welcome) = welcome
        .split_once("\n\n")
        .ok_or("the welcome has no version line")?;
    assert_eq!(shown, welcome);
    Ok(())
}

#[test]
fn help_opens_like_the_first_run() -> Result {
    let sandbox = Sandbox::new()?;
    let welcome = text(&sandbox.run(&[])?.stdout)?;
    let (version, intro) = welcome
        .split_once("\n\n")
        .ok_or("the welcome has no version line")?;
    assert_eq!(version, format!("fetchloom {}", env!("CARGO_PKG_VERSION")));
    let help = text(&sandbox.run(&["--help"])?.stdout)?;
    assert!(
        help.starts_with(&format!("{}\n\nusage: ", intro.trim_end())),
        "{help}"
    );
    Ok(())
}

#[test]
fn help_concepts_explains_the_ideas_with_the_analogy() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["help", "concepts"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let stdout = text(&output.stdout)?;
    assert!(stdout.lines().count() < 30, "{stdout}");
    for line in ANALOGY {
        assert!(stdout.contains(&format!("\n  {line}\n")), "{line}");
    }
    // under --json a text topic is one document like every other answer, with no ANSI even when
    // color is forced
    for topic in ["concepts", "environment", "exit-codes"] {
        let json = sandbox
            .command(&["--json", "help", topic])
            .env("CLICOLOR_FORCE", "1")
            .output()?;
        let document: Value = serde_json::from_slice(&json.stdout)?;
        assert_eq!(document["command"], "help", "{topic}");
        assert_eq!(document["result"]["topic"], topic);
        assert!(!json.stdout.contains(&0x1b), "{topic}");
    }
    insta::assert_snapshot!(stdout);
    Ok(())
}

#[test]
fn help_shows_a_command_and_suggests_a_topic() -> Result {
    let sandbox = Sandbox::new()?;
    for (topic, flag) in [
        (&[][..], &["--help"][..]),
        (&["get"], &["get", "--help"]),
        (&["cache", "gc"], &["cache", "gc", "--help"]),
    ] {
        let help = sandbox.run(&[&["help"][..], topic].concat())?;
        assert!(help.status.success(), "{topic:?}");
        assert_eq!(help.stdout, sandbox.run(flag)?.stdout, "{topic:?}");
    }
    let typo = sandbox.run(&["--json", "help", "concept"])?;
    assert_eq!(typo.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&typo.stdout)?;
    assert_eq!(document["command"], Value::Null, "help runs no command");
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom help concepts`?"
    );
    Ok(())
}

#[test]
fn an_unknown_option_is_a_usage_error() -> Result {
    let output = Sandbox::new()?.run(&["--verison"])?;
    assert_eq!(output.status.code(), Some(2));
    insta::assert_snapshot!(text(&output.stderr)?);
    Ok(())
}

#[test]
fn a_missing_link_shows_the_usage() -> Result {
    let output = Sandbox::new()?.run(&["get"])?;
    assert_eq!(output.status.code(), Some(2));
    insta::assert_snapshot!(text(&output.stderr)?);
    Ok(())
}

#[test]
fn usage_errors_name_options_as_typed() -> Result {
    let sandbox = Sandbox::new()?;
    let first_two = |args: &[&str]| -> Result<String> {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(2));
        let stderr = text(&output.stderr)?;
        Ok(stderr
            .lines()
            .filter(|line| !line.starts_with("  usage:"))
            .collect::<Vec<_>>()
            .join("\n"))
    };
    assert_eq!(
        first_two(&["add", "--as"])?,
        "error: --as needs a value\n  help: `fetchloom add --help` says what it takes"
    );
    assert_eq!(
        first_two(&["completions", "nope"])?,
        "error: <shell> cannot be \"nope\"\n  help: use one of bash, elvish, fish, powershell or zsh"
    );
    assert_eq!(
        first_two(&["status", "-v", "-q"])?,
        "error: --verbose and --quiet cannot be used together\n  help: `fetchloom status --help` says what it takes"
    );
    Ok(())
}

#[test]
fn a_usage_error_names_its_kind_with_verbose() -> Result {
    let verbose = Sandbox::new()?.run(&["-v", "sycn"])?;
    assert_eq!(verbose.status.code(), Some(2));
    let stderr = text(&verbose.stderr)?;
    assert!(stderr.contains("\n  kind: usage\n"), "{stderr}");
    Ok(())
}

#[test]
fn color_marks_meaning_with_labels_and_stays_off_when_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let colored = text(&sandbox.run(&["--color", "always", "sycn"])?.stderr)?;
    assert!(
        colored.contains("31merror:"),
        "an error has a red label: {colored}"
    );
    assert!(
        colored.contains("\u{1b}[1m\u{1b}[36m`fetchloom sync`\u{1b}[0m"),
        "a command takes the accent: {colored}"
    );
    for (flag, variables) in [
        (&["--color", "never"][..], &[("CLICOLOR_FORCE", "1")][..]),
        (&[], &[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")]),
        (&[], &[]),
    ] {
        let mut command = sandbox.command(&[flag, &["sycn"]].concat());
        command.envs(variables.iter().copied());
        let stderr = text(&command.output()?.stderr)?;
        assert!(
            !stderr.contains('\u{1b}'),
            "{flag:?} {variables:?}: {stderr}"
        );
        assert!(stderr.starts_with("error: "), "{stderr}");
    }
    Ok(())
}

#[test]
fn json_gets_one_document_whatever_the_command_line() -> Result {
    let sandbox = Sandbox::new()?;
    let version = env!("CARGO_PKG_VERSION");
    for (args, command, code) in [
        (&["--json", "sycn"][..], Value::Null, 2),
        (&["sycn", "--json"], Value::Null, 2),
        (&["--json", "add"], "add".into(), 2),
        (&["cache", "gx", "--json"], "cache".into(), 2),
        (&["--json", "--version"], Value::Null, 0),
        (&["--json"], Value::Null, 0),
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        let document: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(document["schema"], 1, "{args:?}");
        assert_eq!(document["command"], command, "{args:?}");
        let errors = document["errors"].as_array().ok_or("no errors")?;
        if code == 0 {
            assert_eq!(document["status"], "ok", "{args:?}");
            assert_eq!(document["result"]["version"], version, "{args:?}");
            assert!(errors.is_empty(), "{args:?}");
        } else {
            assert_eq!(document["status"], "error", "{args:?}");
            assert_eq!(errors.len(), 1, "{args:?}");
            assert_eq!(errors[0]["kind"], "usage", "{args:?}");
        }
    }
    let typo = sandbox.run(&["--json", "sycn"])?;
    let document: Value = serde_json::from_slice(&typo.stdout)?;
    assert_eq!(
        document["errors"][0]["message"],
        "there is no command called \"sycn\""
    );
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom sync`?"
    );
    let nested: Value = serde_json::from_slice(&sandbox.run(&["--json", "verify"])?.stdout)?;
    assert_eq!(
        nested["errors"][0]["help"],
        "did you mean `fetchloom cache verify`?"
    );
    for (args, commands) in [
        (&["--json", "--help"][..], None),
        (&["add", "--help", "--json"], Some("add")),
        (&["-C", ".", "list", "--help", "--json"], Some("list")),
        (
            &["--color", "never", "cache", "gc", "--json", "--help"],
            Some("cache gc"),
        ),
    ] {
        let document: Value = serde_json::from_slice(&sandbox.run(args)?.stdout)?;
        assert_eq!(document["command"], "help", "{args:?}");
        if let Some(name) = commands {
            let listed = document["result"]["commands"]
                .as_array()
                .ok_or("no commands")?;
            assert_eq!(listed.len(), 1, "{args:?}");
            assert_eq!(listed[0]["name"], name, "{args:?}");
        }
    }
    Ok(())
}

#[test]
fn a_nested_command_typo_keeps_its_parent_in_the_suggestion() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["cache", "verfiy", "--json"])?;
    assert_eq!(output.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        document["errors"][0]["help"],
        "did you mean `fetchloom cache verify`?"
    );
    let corrected = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        corrected.status.code(),
        Some(0),
        "{}",
        text(&corrected.stderr)?
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn cli_output_non_unicode_paths_return_error_documents() -> Result {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let sandbox = Sandbox::new()?;
    let project = sandbox
        .work()
        .join(OsString::from_vec(b"project-\xff".to_vec()));
    // apfs and other filesystems refuse a name that is not UTF-8, so no such path reaches
    // fetchloom there
    match std::fs::create_dir(&project) {
        Err(err) if err.raw_os_error() == Some(rustix::io::Errno::ILSEQ.raw_os_error()) => {
            return Ok(());
        }
        made => made?,
    }
    std::fs::write(project.join("data.toml"), "[datasets]\n")?;
    let cache = sandbox
        .work()
        .join(OsString::from_vec(b"cache-\xff".to_vec()));
    let commands = [sandbox.command_in(&project, &["--json", "status"]), {
        let mut command = sandbox.command(&["--json", "cache"]);
        command.env("FETCHLOOM_CACHE", cache);
        command
    }];
    for mut command in commands {
        let output = command.env("FETCHLOOM_COLORS", "done=teal").output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
        let document: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(document["status"], "error");
        assert_eq!(document["result"], Value::Null);
        assert_eq!(document["errors"][0]["kind"], "local.io");
        assert!(
            document["errors"][0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("UTF-8"))
        );
        assert!(
            document["warnings"]
                .as_array()
                .is_some_and(|warnings| warnings.iter().any(|warning| warning
                    .as_str()
                    .is_some_and(|text| text.contains("done=teal"))))
        );
    }
    let version = sandbox
        .command_in(&project, &["--json", "--version"])
        .output()?;
    assert!(version.status.success());
    serde_json::from_slice::<Value>(&version.stdout)?;
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn cli_output_full_stdout_is_an_error_for_answers_and_help() -> Result {
    let sandbox = Sandbox::new()?;
    for args in [
        &["--version"][..],
        &["--json", "--version"],
        &["--help"],
        &["get", "--help"],
        &["help", "get"],
        &["help", "concepts"],
        &["completions", "bash"],
    ] {
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full")?;
        let output = sandbox.command(args).stdout(full).output()?;
        assert_eq!(
            output.status.code(),
            Some(50),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        assert!(
            text(&output.stderr)?.contains("could not print"),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    Ok(())
}

/// a terminal narrower than a line wraps it at its last column, even inside a word: a
/// 120-column terminal showed "authenticat" and "ed" on two lines of a long error. fetchloom
/// itself wraps only help, and only between words, and writes each line of an error whole.
#[cfg(target_os = "linux")]
#[test]
fn at_a_narrow_terminal_help_wraps_between_words_and_errors_stay_whole() -> Result {
    let sandbox = Sandbox::new()?;
    let wide = text(&sandbox.run(&["get", "--help"])?.stdout)?;
    let help = sandbox.command(&["get", "--help"]);
    let Some((code, narrow)) =
        crate::support::typed(&sandbox.work(), &help, "rows 30 cols 40", "", "")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{narrow}");
    assert!(
        narrow.lines().count() > wide.lines().count(),
        "help wraps to the terminal\n{narrow}"
    );
    assert_eq!(
        narrow.split_whitespace().collect::<Vec<_>>(),
        wide.split_whitespace().collect::<Vec<_>>(),
        "no word is split or lost\n{narrow}"
    );
    let long = format!("/records/{}/iris.csv", "a".repeat(60));
    let server = crate::support::Server::start(
        &long,
        vec![crate::support::reply(
            "404 Not Found",
            &[("Content-Length", "0")],
            b"",
        )],
    )?;
    let get = sandbox.command(&["get", &server.url]);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &get, "rows 30 cols 40", "", "")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(10), "{screen}");
    // the terminal may clear a progress line first, so the error need not start its line
    assert!(
        screen
            .lines()
            .any(|line| line.contains("error: ") && line.contains(&server.url)),
        "{screen}"
    );
    Ok(())
}

/// files that `get` and `sync` link to the cache's copies are said to be linked to the cache,
/// where the bytes are.
#[test]
fn linked_files_are_said_to_be_linked_to_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let links = sandbox.links()?;
    if !links.reflink && !links.hardlink {
        return Ok(());
    }
    let server = crate::support::Server::start(
        "/files/iris.csv",
        vec![
            crate::support::file(b"a,b\n1,2\n", "\"v1\""),
            crate::support::file(b"a,b\n1,2\n", "\"v1\""),
        ],
    )?;
    let fetched = text(&sandbox.run(&["get", &server.url, "-o", "fetched"])?.stderr)?;
    assert!(
        fetched.contains("Fetched iris.csv into fetched · linked to the cache\n"),
        "{fetched}"
    );
    std::fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\n", server.url),
    )?;
    let synced = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        synced.contains("Placed data/iris · 1 file linked to the cache\n"),
        "{synced}"
    );
    Ok(())
}
