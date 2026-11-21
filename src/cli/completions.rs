//! `fetchloom completions`: a script that teaches a shell fetchloom's commands, options and
//! dataset names. it is text for the shell, so `--json` is a usage error rather than a document.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::process::ExitCode;
use std::time::Instant;

use clap::CommandFactory;
use clap_complete::Shell;

use super::{Context, args, finish};
use crate::error::{Error, Kind};

/// prints the script, or for the scripts' own call, `--names`, the dataset names [`names`] finds.
pub(super) fn print(context: &Context, asked: &args::Completions, started: Instant) -> ExitCode {
    if let Some(current) = &asked.names {
        return names_told(current, &asked.words, asked.shell == Shell::Bash);
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
        let program = std::env::current_exe().ok();
        let short = short_name_runs(std::env::var_os("PATH").as_deref(), program.as_deref());
        write(&script(shell, short))
    };
    finish(context, Some("completions"), outcome, started)
}

/// exits 1 where no dataset's name goes, so the script falls back on its own completer.
/// `narrowed` leaves out the names that cannot start with `current`, as bash's script keeps only
/// those: at 100,000 datasets, its own filter over every name took most of a tab's time.
fn names_told(current: &str, before: &[String], narrowed: bool) -> ExitCode {
    let Some(names) = names(current, before) else {
        return ExitCode::FAILURE;
    };
    let mut lines = String::new();
    for name in names
        .iter()
        .filter(|name| !narrowed || may_start(name, current))
    {
        lines.push_str(name);
        lines.push('\n');
    }
    match write(lines.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// whether `name` may be one bash's script keeps after `current` was typed, `[[ $name ==
/// "$current"* ]]`: a start the same in ascii letters of either case, which `nocasematch` may
/// take, and every name when `current` is not ascii, which a locale may fold in its own way.
fn may_start(name: &str, current: &str) -> bool {
    !current.is_ascii()
        || name
            .as_bytes()
            .get(..current.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(current.as_bytes()))
}

/// the dataset names of the project `-C` names, else the current folder's, when `current`, after
/// `before`, stands where a dataset's name goes, as after `path` or `lock --upgrade`; after
/// `path <name>/`, what the dataset's folder holds. it reads data.toml and the data folder alone
/// and asks nothing of the network.
fn names(current: &str, before: &[String]) -> Option<Vec<String>> {
    if current.starts_with('-') {
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
    // a fifo there would hold the tab until something wrote to it
    let text = project.manifest_text().ok()?;
    match current.split_once('/') {
        None => dataset_names(&text),
        Some((name, rest)) if pending.is_none() && command.get_name() == "path" => {
            placed(&project, &text, name, rest)
        }
        Some(_) => None,
    }
}

/// what `path` names below dataset `name`'s folder after `rest`, as in `path iris/images/`: the
/// files and folders in the folder `rest` ends in, a folder with `/` after it. a name a shell
/// could read as more than one word, or a hidden one, is never offered.
fn placed(
    project: &crate::project::Project,
    text: &str,
    name: &str,
    rest: &str,
) -> Option<Vec<String>> {
    let manifest = crate::project::Project::parse_manifest(text).ok()?;
    if !manifest.datasets.contains_key(name) || !crate::name::is_dataset_name(name) {
        return None;
    }
    let inner = rest.rsplit_once('/').map_or("", |(inner, _)| inner);
    let mut folder = project.root.join(crate::plan::folder(&manifest, name));
    for part in inner.split('/').filter(|part| !part.is_empty()) {
        if matches!(part, "." | "..") {
            return None;
        }
        folder.push(part);
    }
    let before = if inner.is_empty() {
        format!("{name}/")
    } else {
        format!("{name}/{inner}/")
    };
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&folder).ok()?.flatten() {
        let Ok(file) = entry.file_name().into_string() else {
            continue;
        };
        let plain = file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+,@=".contains(c));
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if plain && !file.starts_with('.') {
            let slash = if kind.is_dir() { "/" } else { "" };
            found.push(format!("{before}{file}{slash}"));
        }
    }
    found.sort();
    Some(found)
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

/// the completion script for `shell`, for `fetchloom` on PATH and with `short` for `fl` too,
/// asking fetchloom for dataset names where they go.
pub(super) fn script(shell: Shell, short: bool) -> Vec<u8> {
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut completed(), "fetchloom", &mut script);
    let named = with_names(shell, &String::from_utf8_lossy(&script));
    if short {
        with_short_name(shell, &named).into_bytes()
    } else {
        named.into_bytes()
    }
}

/// whether `fl`, the short name the installers and the python package make for fetchloom, runs
/// `program` here: the first `fl` on `path` is it through a link, or the python package's
/// starter. another program called fl keeps its own completions.
fn short_name_runs(path: Option<&std::ffi::OsStr>, program: Option<&std::path::Path>) -> bool {
    let (Some(path), Some(program)) = (path, program) else {
        return false;
    };
    let name = if cfg!(windows) { "fl.exe" } else { "fl" };
    let Some(found) = std::env::split_paths(path)
        .map(|folder| folder.join(name))
        .find(|file| file.is_file())
    else {
        return false;
    };
    same_file::is_same_file(&found, program).unwrap_or(false) || starter(&found)
}

/// whether `file` is the python package's starter, which names fetchloom's module: a short
/// script, or on windows a small program with the script at its end.
fn starter(file: &std::path::Path) -> bool {
    const MODULE: &[u8] = b"fetchloom.__main__";
    std::fs::metadata(file).is_ok_and(|found| found.len() <= 1 << 20)
        && std::fs::read(file)
            .is_ok_and(|bytes| bytes.windows(MODULE.len()).any(|window| window == MODULE))
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

/// `script` with a completer that asks `fetchloom completions --names` after a command that takes
/// dataset names, and else, or when that fails, uses the script's own completer.
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
            glue.push_str(&FISH_KEYS.replace(
                "KEYS",
                &crate::settings::keys().collect::<Vec<_>>().join(" "),
            ));
            // an option still waiting for its value, as `--color ` is, fails argparse, and the line
            // has no command yet, so the top's entries, that option's values among them, still hold
            let script = script.replacen(
                "-- $cmd 2>/dev/null\n    or return\n",
                "-- $cmd 2>/dev/null\n    or return 0\n",
                1,
            );
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

/// bash: the dataset names after a command that takes them, else the static completer. each
/// name is matched as plain text and quoted for the line, since `compgen -W` would expand it.
const BASH_NAMES: &str = r#"_fetchloom() {
    local word names name quoted glob=1
    local -a all
    for word in "${COMP_WORDS[@]:1:COMP_CWORD-1}"; do
        case "$word" in
            COMMANDS)
                if names=$("$1" completions bash --names="$2" -- "${COMP_WORDS[@]:1:COMP_CWORD-1}" 2>/dev/null); then
                    # split on lines alone, with globbing off as long as that takes
                    [[ $- == *f* ]] && glob=0
                    set -f
                    local IFS=$'\n'
                    all=($names)
                    ((glob)) && set +f
                    COMPREPLY=()
                    for name in "${all[@]}"; do
                        if [[ $name == "$2"* ]]; then
                            printf -v quoted '%q' "$name"
                            COMPREPLY+=("$quoted")
                        fi
                    done
                    # a folder alone is completed further, as a file name would be
                    if [[ ${#COMPREPLY[@]} -eq 1 && ${COMPREPLY[0]} == */ ]]; then
                        compopt -o nospace
                    fi
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
                    local -a all=(${(f)names})
                    # a folder takes no space after it, so its files complete next
                    compadd -S '' -- ${(M)all:#*/}
                    compadd -- ${all:#*/}
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

/// fish: the keys `settings set` and `settings unset` take, as the key alone, which clap's fish
/// script leaves to file names.
const FISH_KEYS: &str = r#"function __fetchloom_settings_key
    set -l words (commandline -opc)
    set -l at (contains -i -- set $words; or contains -i -- unset $words)
    test -n "$at"; and test (count $words) -eq $at
end
complete -c fetchloom -n '__fish_fetchloom_using_subcommand settings; and __fetchloom_settings_key' -f -a 'KEYS'
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

/// powershell: the names, else the static completer, which the script keeps in a variable. a
/// name that is more than letters, digits, `-`, `_` and `.` goes on the line in single quotes.
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
            $typed = [WildcardPattern]::Escape($wordToComplete) + '*'
            foreach ($name in $names) {
                if ($name -like $typed) {
                    $text = if ($name -match '^[A-Za-z0-9_.-]+$') { $name } else { "'" + ($name -replace '[''\u2018-\u201B]', '$0$0') + "'" }
                    [CompletionResult]::new($text, $name, [CompletionResultType]::ParameterValue, $name)
                }
            }
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

    /// a name may start with the typed word in ascii letters of either case, and every name may
    /// when the word is not ascii, so bash's own filter, `nocasematch` or not, keeps the same.
    #[test]
    fn a_name_may_start_with_a_word_of_either_case_or_beyond_ascii() {
        for (name, current, may) in [
            ("iris", "", true),
            ("iris", "ir", true),
            ("iris", "IR", true),
            ("iris", "Iris", true),
            ("iris", "iris", true),
            ("iris", "irises", false),
            ("iris", "co", false),
            ("iris-2", "iris-", true),
            ("iris/Iris.csv", "iris/i", true),
            ("iris", "\u{e9}", true),
            ("iris", "ir\u{212a}", true),
        ] {
            assert_eq!(may_start(name, current), may, "{name}, {current}");
        }
    }

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

    /// fl is completed only where it runs fetchloom: a link to it, or the python package's
    /// starter; someone's own fl, or none, keeps its completions.
    #[cfg(unix)]
    #[test]
    fn fl_is_completed_only_where_it_runs_fetchloom() {
        let folder = tempfile::tempdir().unwrap();
        let program = folder.path().join("fetchloom");
        std::fs::write(&program, "the program").unwrap();
        let path = |name: &str| {
            let bin = folder.path().join(name);
            std::fs::create_dir(&bin).unwrap();
            (bin.join("fl"), bin.into_os_string())
        };
        let (linked, linked_path) = path("linked");
        std::os::unix::fs::symlink(&program, linked).unwrap();
        let (started, started_path) = path("started");
        let python = "#!/usr/bin/python3\nimport sys\nfrom fetchloom.__main__ import main\nsys.exit(main())\n";
        std::fs::write(started, python).unwrap();
        let (other, other_path) = path("other");
        std::fs::write(other, "#!/bin/sh\necho flights\n").unwrap();
        let (_, empty_path) = path("empty");
        for (path, runs) in [
            (linked_path, true),
            (started_path, true),
            (other_path.clone(), false),
            (empty_path, false),
        ] {
            assert_eq!(
                short_name_runs(Some(&path), Some(&program)),
                runs,
                "{path:?}"
            );
        }
        assert!(!short_name_runs(None, Some(&program)));
        assert!(!short_name_runs(Some(&other_path), None));
        assert!(
            !String::from_utf8(script(Shell::Bash, false))
                .unwrap()
                .contains(" fl\n")
        );
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
            let script = String::from_utf8(script(shell, true)).unwrap();
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
            let script = String::from_utf8(script(shell, true)).unwrap();
            assert!(script.contains(kept), "{shell} lost its own completer");
        }
    }
}
