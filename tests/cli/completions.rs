//! `fetchloom completions`: the script each shell loads to complete commands and options.

use serde_json::Value;

use crate::support::{Result, Sandbox, command_help, text};

/// each shell, a line only its script has, and the text that completes `fl` the same way for
/// every way the shell may load the script.
const SHELLS: [(&str, &str, &[&str]); 5] = [
    (
        "bash",
        "complete -F _fetchloom",
        &[
            "complete -F _fetchloom -o nosort -o bashdefault -o default fetchloom fl\n",
            "complete -F _fetchloom -o bashdefault -o default fetchloom fl\n",
        ],
    ),
    (
        "zsh",
        "#compdef fetchloom",
        &[
            "#compdef fetchloom fl\n",
            "compdef _fetchloom fetchloom fl\n",
        ],
    ),
    (
        "fish",
        "complete -c fetchloom",
        &["\ncomplete -c fl -w fetchloom\n"],
    ),
    (
        "powershell",
        "Register-ArgumentCompleter",
        &["-CommandName 'fetchloom', 'fl' -ScriptBlock"],
    ),
    (
        "elvish",
        "edit:completion:arg-completer[fetchloom]",
        &["\nset edit:completion:arg-completer[fl] = $edit:completion:arg-completer[fetchloom]\n"],
    ),
];

#[test]
fn completions_print_a_script_for_each_shell() -> Result {
    let sandbox = Sandbox::new()?;
    for (shell, marker, short) in SHELLS {
        let output = sandbox.run(&["completions", shell])?;
        assert!(
            output.status.success(),
            "{shell}: {}",
            text(&output.stderr)?
        );
        assert!(
            output.stderr.is_empty(),
            "{shell}: {}",
            text(&output.stderr)?
        );
        let script = text(&output.stdout)?;
        assert!(script.contains(marker), "{shell} lacks {marker}");
        for line in short {
            assert!(
                script.contains(line),
                "{shell} does not complete fl: {line}"
            );
        }
        for word in ["sync", "status", "gc", "verify", "completions", "offline"] {
            assert!(script.contains(word), "{shell} lacks {word}");
        }
    }
    Ok(())
}

#[test]
fn completions_ignore_settings_that_would_stop_other_commands() -> Result {
    let sandbox = Sandbox::new()?;
    let expected = sandbox.run(&["completions", "zsh"])?.stdout;
    let missing = sandbox.work().join("missing");
    let missing = missing.to_str().ok_or("the sandbox path is not unicode")?;
    let output = sandbox
        .command(&["-C", missing, "--offline", "-q", "completions", "zsh"])
        .env("FETCHLOOM_JOBS", "0")
        .env("FETCHLOOM_THREADS", "many")
        .env("FETCHLOOM_RATE", "fast")
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
    assert_eq!(output.stdout, expected);
    Ok(())
}

#[test]
fn completions_refuse_json_and_unknown_shells() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["--json", "completions", "bash"])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr)?);
    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(document["command"], "completions");
    assert_eq!(document["status"], "error");
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["help"],
        "run `fetchloom completions bash` without --json"
    );

    let unknown = sandbox.run(&["completions", "tcsh"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert_eq!(text(&unknown.stdout)?, "");
    let stderr = text(&unknown.stderr)?;
    for (shell, _, _) in SHELLS {
        assert!(stderr.contains(shell), "{shell}: {stderr}");
    }
    Ok(())
}

#[test]
fn completions_have_their_own_help() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["completions", "--help"])?;
    assert!(output.status.success());
    let help = text(&output.stdout)?;
    let examples: Vec<&str> = help
        .lines()
        .skip_while(|line| *line != "examples:")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .collect();
    assert!((2..=4).contains(&examples.len()), "{help}");
    for example in examples {
        assert!(example.starts_with("  fetchloom completions "), "{example}");
    }
    assert_eq!(
        sandbox.run(&["help", "completions"])?.stdout,
        output.stdout,
        "help completions shows the same"
    );
    command_help().bind(|| insta::assert_snapshot!(help));
    Ok(())
}

/// bash loads the script it printed, which completes fetchloom and fl, and `fl ca` becomes
/// `fl cache` through whatever bash registered for fl. skipped where bash is not installed.
#[cfg(unix)]
#[test]
fn bash_loads_the_script() -> Result {
    let sandbox = Sandbox::new()?;
    let script = sandbox.folder("scripts")?.join("fetchloom.bash");
    std::fs::write(&script, sandbox.run(&["completions", "bash"])?.stdout)?;
    let complete = r#". "$1" && complete -p fetchloom && spec=$(complete -p fl) && echo "$spec" &&
        function=${spec#*-F } && function=${function%% *} &&
        COMP_WORDS=(fl ca) COMP_CWORD=1 COMP_LINE="fl ca" COMP_POINT=5 &&
        "$function" fl ca fl && printf 'offered %s\n' "${COMPREPLY[@]}""#;
    let loaded = match std::process::Command::new("bash")
        .arg("-c")
        .arg(complete)
        .arg("bash")
        .arg(&script)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    assert!(loaded.status.success(), "{}", text(&loaded.stderr)?);
    let said = text(&loaded.stdout)?;
    assert!(said.contains("-F _fetchloom fetchloom\n"), "{said}");
    assert!(said.contains("-F _fetchloom fl\n"), "{said}");
    assert!(said.lines().any(|line| line == "offered cache"), "{said}");
    Ok(())
}
