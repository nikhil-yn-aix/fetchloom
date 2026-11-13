//! `fetchloom completions`: the script each shell loads to complete commands, options and the
//! names of datasets.

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

/// the script completes fetchloom and fl, and `fl ca` becomes `fl cache` through whatever bash
/// registered for fl. skipped where bash is not installed.
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
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return crate::support::skipped("bash", "no bash, so no bash completion to load");
        }
        Err(err) => return Err(err.into()),
    };
    assert!(loaded.status.success(), "{}", text(&loaded.stderr)?);
    let said = text(&loaded.stdout)?;
    assert!(said.contains("-F _fetchloom fetchloom\n"), "{said}");
    assert!(said.contains("-F _fetchloom fl\n"), "{said}");
    assert!(said.lines().any(|line| line == "offered cache"), "{said}");
    Ok(())
}

/// names come from data.toml alone, in the project or through -C, past a broken settings file;
/// the rest completes as the static script does. skipped where bash is not installed.
#[cfg(unix)]
#[test]
fn bash_completes_the_dataset_names_data_toml_has() -> Result {
    if std::process::Command::new("bash")
        .args(["-c", ":"])
        .output()
        .is_err()
    {
        return crate::support::skipped("bash", "no bash, so no bash completion to load");
    }
    let sandbox = Sandbox::new()?;
    std::fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.iris]\nref = \"https://example.org/iris.csv\"\n\n[datasets.irises]\nref = \"https://example.org/irises.csv\"\n\n[datasets.cows]\nref = \"https://example.org/cows.csv\"\n",
    )?;
    let elsewhere = sandbox.folder("elsewhere")?;
    let settings = elsewhere.join("settings.toml");
    std::fs::write(&settings, "jobs = \"many\"\n")?;
    let script = elsewhere.join("fetchloom.bash");
    std::fs::write(&script, sandbox.run(&["completions", "bash"])?.stdout)?;
    let program = crate::support::executable();
    let folder = program.parent().ok_or("the program has no folder")?;
    let path = std::env::join_paths(std::iter::once(folder.to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))?;
    let home = sandbox
        .work()
        .parent()
        .ok_or("work has no folder")?
        .to_path_buf();
    let offered = |here: &std::path::Path, words: &str| -> Result<Vec<String>> {
        let complete = format!(
            r#". "$1" && COMP_WORDS=({words}) && COMP_CWORD=$((${{#COMP_WORDS[@]}} - 1)) &&
            _fetchloom fetchloom "${{COMP_WORDS[COMP_CWORD]}}" "${{COMP_WORDS[COMP_CWORD - 1]}}" &&
            printf 'offered %s\n' "${{COMPREPLY[@]}}""#
        );
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(complete)
            .arg("bash")
            .arg(&script)
            .current_dir(here)
            .env("PATH", &path)
            .env("FETCHLOOM_SETTINGS", &settings)
            .env("HOME", &home)
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        Ok(text(&output.stdout)?
            .lines()
            .filter_map(|line| line.strip_prefix("offered "))
            .filter(|offer| !offer.is_empty())
            .map(str::to_owned)
            .collect())
    };
    let work = sandbox.work();
    assert_eq!(offered(&work, "fetchloom path ir")?, ["iris", "irises"]);
    for words in [
        "fetchloom sync cows ir",
        "fetchloom remove ir",
        "fetchloom build --dry-run ir",
        "fetchloom lock --upgrade ir",
    ] {
        assert_eq!(offered(&work, words)?, ["iris", "irises"], "{words}");
    }
    let through = format!("fetchloom -C {} path ir", work.display());
    assert_eq!(offered(&elsewhere, &through)?, ["iris", "irises"]);
    // a shell leaves the ~ of the words it completes for the program to read
    let tilde = "fetchloom -C '~/work' path ir";
    assert_eq!(offered(&elsewhere, tilde)?, ["iris", "irises"]);
    assert_eq!(offered(&work, "fetchloom ca")?, ["cache"]);
    assert_eq!(offered(&work, "fetchloom sync --lo")?, ["--locked"]);
    assert_eq!(
        offered(&elsewhere, "fetchloom path ir")?,
        Vec::<String>::new()
    );
    Ok(())
}

/// a name a cloned data.toml could hold if no rule stopped it, which runs a command wherever a
/// shell reads it as code instead of as a word.
#[cfg(unix)]
const PROBE: &str = "ir$(echo RAN >&2)x";

/// a folder holding a stand-in for fetchloom that prints `names` for any arguments, as the
/// scripts' call for names would get them past the program's own name rules.
#[cfg(unix)]
fn printing(sandbox: &Sandbox, names: &[&str]) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let folder = sandbox.folder("stand-in")?;
    let quoted: Vec<String> = names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', r"'\''")))
        .collect();
    let program = folder.join("fetchloom");
    std::fs::write(
        &program,
        format!("#!/bin/sh\nprintf '%s\\n' {}\n", quoted.join(" ")),
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))?;
    Ok(folder)
}

/// `folder` first on PATH, then the folders of this machine's PATH.
#[cfg(unix)]
fn before_path(folder: &std::path::Path) -> Result<std::ffi::OsString> {
    Ok(std::env::join_paths(
        std::iter::once(folder.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )?)
}

/// bash expands no name while completing and quotes each on the line, so none runs at a tab or
/// when the line runs. skipped where bash is not installed.
#[cfg(unix)]
#[test]
fn bash_takes_the_names_it_is_given_literally() -> Result {
    let sandbox = Sandbox::new()?;
    let script = sandbox.folder("scripts")?.join("fetchloom.bash");
    std::fs::write(&script, sandbox.run(&["completions", "bash"])?.stdout)?;
    let stand_in = printing(&sandbox, &["iris", "irises", PROBE, "cows"])?;
    let complete = r#". "$1" && COMP_WORDS=(fetchloom path ir) && COMP_CWORD=2 &&
        _fetchloom fetchloom ir path && for offer in "${COMPREPLY[@]}"; do
        eval "set -- $offer" && printf 'reads %s\n' "$1"; done"#;
    let output = match std::process::Command::new("bash")
        .arg("-c")
        .arg(complete)
        .arg("bash")
        .arg(&script)
        .env("PATH", before_path(&stand_in)?)
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return crate::support::skipped("bash", "no bash, so no bash completion to load");
        }
        Err(err) => return Err(err.into()),
    };
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("RAN"), "a name ran: {stderr}");
    let reads: Vec<String> = text(&output.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix("reads "))
        .map(str::to_owned)
        .collect();
    assert_eq!(reads, ["iris", "irises", PROBE]);
    Ok(())
}

/// a name of more than letters, digits, `-`, `_` and `.` comes in single quotes. skipped where
/// pwsh is not installed.
#[cfg(unix)]
#[test]
fn powershell_takes_the_names_it_is_given_literally() -> Result {
    let sandbox = Sandbox::new()?;
    let script = sandbox.folder("scripts")?.join("fetchloom.ps1");
    std::fs::write(&script, sandbox.run(&["completions", "powershell"])?.stdout)?;
    let stand_in = printing(&sandbox, &["iris", PROBE, "it's", "cows"])?;
    let complete = format!(
        "Get-Content -Raw '{}' | Invoke-Expression; $line = 'fetchloom path i'; \
         (TabExpansion2 -inputScript $line -cursorColumn $line.Length).CompletionMatches | \
         ForEach-Object {{ 'offers ' + $_.CompletionText }}",
        script.display()
    );
    let output = match std::process::Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", &complete])
        .env("PATH", before_path(&stand_in)?)
        .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
        .env("POWERSHELL_UPDATECHECK", "Off")
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return crate::support::skipped("pwsh", "no pwsh, so no powershell to complete in");
        }
        Err(err) => return Err(err.into()),
    };
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("RAN"), "a name ran: {stderr}");
    let offers: Vec<String> = text(&output.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix("offers "))
        .map(str::to_owned)
        .collect();
    assert_eq!(offers, ["iris", "'ir$(echo RAN >&2)x'", "'it''s'"]);
    Ok(())
}
