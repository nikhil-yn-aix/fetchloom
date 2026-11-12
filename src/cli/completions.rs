//! `fetchloom completions`: a script that teaches a shell fetchloom's commands, options and
//! dataset names. the script is text for the shell, so `--json` is a usage error rather than a
//! document around it.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Instant;

use clap::CommandFactory;
use clap_complete::Shell;

use super::{Context, args, finish};
use crate::error::{Error, Kind};

/// prints the script for the shell `asked` names on stdout, or the usage error `--json` gets.
/// the scripts' own call, `--names`, gets the dataset names [`names`] finds instead.
pub(super) fn print(context: &Context, asked: &args::Completions, started: Instant) -> ExitCode {
    if let Some(current) = &asked.names {
        return names_told(current, &asked.words);
    }
    let shell = asked.shell;
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

/// prints the names [`names`] finds for `current`, one to a line, and exits 0, or prints nothing
/// and exits 1 where it is no dataset's name, so the script completes it as it would without
/// them.
fn names_told(current: &str, before: &[String]) -> ExitCode {
    let Some(names) = names(current, before) else {
        return ExitCode::FAILURE;
    };
    let mut lines = String::new();
    for name in &names {
        lines.push_str(name);
        lines.push('\n');
    }
    match write(lines.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// the names of the datasets data.toml has, when the word `current`, after the words `before` it
/// on a command line as the shell holds them, stands where a dataset's name goes: after a command
/// such as `path` or `sync`, or an option such as `lock --upgrade`. the project is the one `-C`
/// names, else the one the current folder is in, found as every command finds it. none for any
/// other word, such as an option or the file of `name/file`, and outside a project. only
/// data.toml is read, and nothing is asked of the network.
fn names(current: &str, before: &[String]) -> Option<Vec<String>> {
    if current.starts_with('-') || current.contains('/') {
        return None;
    }
    let root = args::Args::command();
    let mut command = &root;
    let mut pending: Option<&clap::Arg> = None;
    let mut directory: Option<&str> = None;
    for word in before {
        if word.len() > 1 && word.starts_with('-') {
            pending = None;
            if let Some((arg, attached)) = option(&root, command, word) {
                if arg.get_short() == Some('C') {
                    directory = attached.or(directory);
                }
                if arg.get_action().takes_values() && attached.is_none() {
                    pending = Some(arg);
                }
            }
            continue;
        }
        if let Some(arg) = pending {
            if arg.get_short() == Some('C') {
                directory = Some(word);
            }
            if arg
                .get_num_args()
                .is_none_or(|range| range.max_values() <= 1)
            {
                pending = None;
            }
            continue;
        }
        if let Some(sub) = command.find_subcommand(word) {
            command = sub;
        }
    }
    let wanted = match pending {
        Some(arg) => takes_names(arg),
        None => command.get_positionals().any(takes_names),
    };
    if !wanted {
        return None;
    }
    let start = std::path::absolute(from_home(directory.unwrap_or("."))).ok()?;
    let project = crate::project::Project::find(&start)?;
    let text = std::fs::read_to_string(project.manifest_path()).ok()?;
    dataset_names(&text)
}

/// the names of the datasets data.toml's `text` has. a data.toml fetchloom wrote names each in a
/// `[datasets.<name>]` header, which a look at each line finds; any other shape is parsed whole.
fn dataset_names(text: &str) -> Option<Vec<String>> {
    let names = if let Some(names) = headed(text) {
        names
    } else {
        let manifest: toml::Table = text.parse().ok()?;
        manifest
            .get("datasets")?
            .as_table()?
            .keys()
            .cloned()
            .collect()
    };
    // data.toml may come from anyone, and a shell puts a completion on the command line as it
    // is, so a name fetchloom would refuse, which could hold `$(…)`, is never offered
    Some(
        names
            .into_iter()
            .filter(|name| crate::name::is_dataset_name(name))
            .collect(),
    )
}

/// the names in `[datasets.<name>]` headers, or none where a line alone cannot tell them: no such
/// header, a multi-line string, a `[datasets]` table, dotted keys or a quoted name.
fn headed(text: &str) -> Option<Vec<String>> {
    if text.contains("\"\"\"") || text.contains("'''") {
        return None;
    }
    let mut names = std::collections::BTreeSet::new();
    for line in text.lines().map(str::trim_start) {
        if line.starts_with("datasets") {
            return None;
        }
        let Some(header) = line.strip_prefix('[') else {
            continue;
        };
        if !header.contains("datasets") {
            continue;
        }
        let rest = header.strip_prefix("datasets.")?;
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))?;
        let (name, after) = rest.split_at(end);
        if name.is_empty() || !(after.starts_with(']') || after.starts_with('.')) {
            return None;
        }
        names.insert(name.to_owned());
    }
    (!names.is_empty()).then(|| names.into_iter().collect())
}

/// `folder` with a leading `~` read as the home folder, which a shell has not yet replaced in the
/// words it completes.
fn from_home(folder: &str) -> std::path::PathBuf {
    let rest = folder
        .strip_prefix('~')
        .filter(|rest| rest.is_empty() || rest.starts_with(std::path::is_separator));
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    match (rest, home) {
        (Some(rest), Some(home)) => {
            std::path::Path::new(&home).join(rest.trim_start_matches(std::path::is_separator))
        }
        _ => folder.into(),
    }
}

/// the option `word` names, of `command` or of the program itself, with a value written in the
/// same word, as `-Cdata` or `--color=never`.
fn option<'c>(
    root: &'c clap::Command,
    command: &'c clap::Command,
    word: &'c str,
) -> Option<(&'c clap::Arg, Option<&'c str>)> {
    let mut known = command.get_arguments().chain(root.get_arguments());
    if let Some(long) = word.strip_prefix("--") {
        let (name, value) = long
            .split_once('=')
            .map_or((long, None), |(name, value)| (name, Some(value)));
        return known
            .find(|arg| arg.get_long() == Some(name))
            .map(|arg| (arg, value));
    }
    let short = word.strip_prefix('-')?;
    let letter = short.chars().next()?;
    let arg = known.find(|arg| arg.get_short() == Some(letter))?;
    let rest = &short[letter.len_utf8()..];
    let value = (!rest.is_empty() && arg.get_action().takes_values()).then_some(rest);
    Some((arg, value))
}

/// whether `arg` takes the name of a dataset data.toml has: `add --as` names a new one.
fn takes_names(arg: &clap::Arg) -> bool {
    arg.get_long() != Some("as")
        && arg
            .get_value_names()
            .is_some_and(|names| names.iter().any(|name| name == "name"))
}

/// the completion script for `shell`, for a program on PATH as `fetchloom`, which completes
/// `fl` the same way: the short name installers make for the same program. after a command that
/// takes a dataset's name, it asks fetchloom for the names data.toml has; everything else it
/// completes from fetchloom's command line as it was written.
pub(super) fn script(shell: Shell) -> Vec<u8> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut completed(), "fetchloom", &mut script);
    let named = with_names(shell, &String::from_utf8_lossy(&script));
    with_short_name(shell, &named).into_bytes()
}

/// fetchloom's command line as the scripts complete it, without the arguments only the scripts
/// give `completions`: clap writes hidden arguments into the scripts too.
fn completed() -> clap::Command {
    let command = args::Args::command();
    // mut_subcommand moves the command it changes to the end, so those after it follow it again
    let after: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .skip_while(|name| name != "completions")
        .skip(1)
        .collect();
    let command = command.mut_subcommand("completions", |completions| {
        let shown: Vec<clap::Arg> = completions
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .cloned()
            .collect();
        let fresh = clap::Command::new("completions").args(shown);
        match completions.get_about() {
            Some(about) => fresh.about(about.clone()),
            None => fresh,
        }
    });
    after.iter().fold(command, |command, name| {
        command.mut_subcommand(name, |sub| sub)
    })
}

/// the commands that take a dataset's name, as `path` and `lock --upgrade` do.
fn naming() -> Vec<String> {
    args::Args::command()
        .get_subcommands()
        .filter(|command| command.get_arguments().any(takes_names))
        .map(|command| command.get_name().to_owned())
        .collect()
}

/// the options that take a dataset's name, each with its command, as `lock` and `upgrade`.
fn naming_options() -> Vec<(String, String)> {
    let line = args::Args::command();
    line.get_subcommands()
        .flat_map(|command| {
            command
                .get_arguments()
                .filter(|arg| !arg.is_positional() && takes_names(arg))
                .filter_map(move |arg| {
                    Some((command.get_name().to_owned(), arg.get_long()?.to_owned()))
                })
        })
        .collect()
}

/// `script` with a completer that asks `fetchloom completions --names` for the dataset names
/// after a command that takes them, and leaves every other word to the script's own completer,
/// as when it answers that the word is no name.
fn with_names(shell: Shell, script: &str) -> String {
    let commands = naming();
    match shell {
        Shell::Bash => {
            let wrapper = BASH_NAMES.replace("COMMANDS", &commands.join("|"));
            script
                .replacen("_fetchloom() {", "_fetchloom_static() {", 1)
                .replacen(
                    "\nif [[ \"${BASH_VERSINFO[0]}\" -eq 4",
                    &format!("\n{wrapper}\nif [[ \"${{BASH_VERSINFO[0]}}\" -eq 4"),
                    1,
                )
        }
        Shell::Zsh => {
            let wrapper = ZSH_NAMES.replace("COMMANDS", &commands.join("|"));
            script
                .replacen("\n_fetchloom() {", "\n_fetchloom_static() {", 1)
                .replacen(
                    "\nif [ \"$funcstack[1]\" = \"_fetchloom\" ]; then",
                    &format!("\n{wrapper}\nif [ \"$funcstack[1]\" = \"_fetchloom\" ]; then"),
                    1,
                )
        }
        Shell::Fish => {
            let mut glue = FISH_NAMES.replace("COMMANDS", &commands.join(" "));
            // fish completes the value of an option only from the entries for that option
            for (command, long) in naming_options() {
                let _ = writeln!(
                    glue,
                    "complete -c fetchloom -n '__fish_seen_subcommand_from {command}; and \
                     __fetchloom_names' -l {long} -r -f -a '$__fetchloom_found'"
                );
            }
            format!("{script}{glue}")
        }
        Shell::Elvish => format!(
            "{script}{}",
            ELVISH_NAMES.replace("COMMANDS", &commands.join(" "))
        ),
        Shell::PowerShell => {
            let quoted: Vec<String> = commands.iter().map(|name| format!("'{name}'")).collect();
            format!(
                "{}{}",
                script.replacen(
                    "Register-ArgumentCompleter -Native -CommandName 'fetchloom' -ScriptBlock {",
                    "$global:FetchloomStatic = {",
                    1,
                ),
                POWERSHELL_NAMES.replace("COMMANDS", &quoted.join(", "))
            )
        }
        _ => script.to_owned(),
    }
}

/// bash: the dataset names after a command that takes them, else the static completer.
const BASH_NAMES: &str = r#"_fetchloom() {
    local word names
    for word in "${COMP_WORDS[@]:1:COMP_CWORD-1}"; do
        case "$word" in
            COMMANDS)
                if names=$("$1" completions bash --names="$2" -- "${COMP_WORDS[@]:1:COMP_CWORD-1}" 2>/dev/null); then
                    local IFS=$'\n'
                    COMPREPLY=($(compgen -W "$names" -- "$2"))
                    return 0
                fi
                break
                ;;
        esac
    done
    _fetchloom_static "$@"
}
"#;

/// zsh: the same, adding the names as matches.
const ZSH_NAMES: &str = r#"_fetchloom() {
    local word names
    for word in "${(@Q)words[2,CURRENT-1]}"; do
        case $word in
            (COMMANDS)
                if names=$("${words[1]}" completions zsh --names="${(Q)PREFIX}" -- "${(@Q)words[2,CURRENT-1]}" 2>/dev/null); then
                    compadd -- ${(f)names}
                    return
                fi
                break
                ;;
        esac
    done
    _fetchloom_static "$@"
}
"#;

/// fish: the names, in place of files, where fetchloom finds them.
const FISH_NAMES: &str = r#"function __fetchloom_names
    set -l words (commandline -opc)
    set -l current (commandline -ct)
    set -g __fetchloom_found ($words[1] completions fish --names="$current" -- $words[2..-1] 2>/dev/null)
end
complete -c fetchloom -n '__fish_seen_subcommand_from COMMANDS; and __fetchloom_names' -f -a '$__fetchloom_found'
"#;

/// elvish: the names, else the static completer.
const ELVISH_NAMES: &str = r"var fetchloom-static = $edit:completion:arg-completer[fetchloom]
set edit:completion:arg-completer[fetchloom] = {|@words|
    var before = $words[1..-1]
    var naming = $false
    for word $before {
        if (has-value [COMMANDS] $word) {
            set naming = $true
        }
    }
    if $naming {
        try {
            (external $words[0]) completions elvish '--names='$words[-1] -- $@before 2>/dev/null | from-lines
        } catch {
            $fetchloom-static $@words
        }
    } else {
        $fetchloom-static $@words
    }
}
";

/// powershell: the names, else the static completer, which the script keeps in a variable.
const POWERSHELL_NAMES: &str = r#"
Register-ArgumentCompleter -Native -CommandName 'fetchloom' -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    $words = @(foreach ($element in $commandAst.CommandElements) {
        if ($element.Extent.EndOffset -ge $cursorPosition) { break }
        if ($element -is [StringConstantExpressionAst]) { $element.Value } else { $element.Extent.Text }
    })
    if ($words | Select-Object -Skip 1 | Where-Object { $_ -in @(COMMANDS) }) {
        $names = & fetchloom completions powershell "--names=$wordToComplete" -- @($words | Select-Object -Skip 1) 2>$null
        if ($LASTEXITCODE -eq 0) {
            $names | Where-Object { $_ -like "$wordToComplete*" } |
                ForEach-Object { [CompletionResult]::new($_, $_, [CompletionResultType]::ParameterValue, $_) }
            return
        }
    }
    & $global:FetchloomStatic $wordToComplete $commandAst $cursorPosition
}
"#;

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
    fn names_come_from_headers_or_else_from_the_whole_file() {
        let headed = "[datasets.iris]\nref = \"a\"\n\n[datasets.cows] # cattle\nref = \"b\"\n\
                      [datasets.cows.place]\nx = 1\n";
        assert_eq!(
            dataset_names(headed),
            Some(vec!["cows".into(), "iris".into()])
        );
        for text in [
            "[datasets]\niris = { ref = \"a\" }\n",
            "datasets.iris.ref = \"a\"\n",
            "[ datasets.iris ]\nref = \"a\"\n",
            "[datasets.iris]\nnote = \"\"\"\n[datasets.fake]\n\"\"\"\n",
        ] {
            assert_eq!(dataset_names(text), Some(vec!["iris".into()]), "{text}");
        }
        let quoted = "[datasets.\"iris-2\"]\nref = \"a\"\n";
        assert_eq!(dataset_names(quoted), Some(vec!["iris-2".into()]));
        let refused = "[datasets.\"ir$(touch x)\"]\nref = \"a\"\n\n[datasets.\"v1.2\"]\nref = \"b\"\n\n[datasets.irises]\nref = \"c\"\n";
        assert_eq!(dataset_names(refused), Some(vec!["irises".into()]));
        assert_eq!(dataset_names("[settings]\njobs = 2\n"), None);
        assert_eq!(dataset_names("[datasets.iris\n"), None);
    }

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
            let asks = [
                format!("completions {shell} --names="),
                format!("completions {shell} \"--names="),
                format!("completions {shell} '--names="),
            ];
            assert!(
                asks.iter().any(|asks| script.contains(asks)),
                "{shell} never asks for names"
            );
            assert!(
                !script.contains("for the scripts"),
                "{shell} offers what only the scripts give"
            );
        }
        let commands = |line: &clap::Command| -> Vec<String> {
            let subs = line.get_subcommands();
            subs.map(|sub| sub.get_name().to_owned()).collect()
        };
        assert_eq!(commands(&completed()), commands(&args::Args::command()));
        // the names' completer hands every other word to the script's own, renamed
        for (shell, kept) in [
            (Shell::Bash, "_fetchloom_static() {"),
            (Shell::Zsh, "_fetchloom_static() {"),
            (Shell::PowerShell, "\n$global:FetchloomStatic = {"),
            (Shell::Elvish, "var fetchloom-static = "),
        ] {
            let script = String::from_utf8(script(shell)).unwrap();
            assert!(script.contains(kept), "{shell} lost its own completer");
        }
    }
}
