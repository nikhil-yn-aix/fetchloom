//! the documentation against the binary: options on their pages, commands and options in the
//! agent guides, only real ones in shell examples, every setting, and each JSON result.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::support::{Result, Sandbox, json};

/// the options each command family accepts, and the global ones, as `help --json` teaches them.
struct Cli {
    global: BTreeSet<String>,
    families: BTreeMap<String, BTreeSet<String>>,
    commands: Vec<(String, Vec<String>)>,
}

fn cli() -> Result<Cli> {
    let output = Sandbox::new()?.run(&["help", "--json"])?;
    let document = json(&output)?;
    let result = &document["result"];
    let names = |arguments: &serde_json::Value| -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for argument in arguments.as_array().into_iter().flatten() {
            for key in ["name", "short"] {
                if let Some(name) = argument[key].as_str().filter(|name| name.starts_with('-')) {
                    names.insert(name.to_owned());
                }
            }
        }
        names
    };
    let global = names(&result["options"]);
    let mut families: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut commands = Vec::new();
    for command in result["commands"].as_array().into_iter().flatten() {
        let name = command["name"]
            .as_str()
            .ok_or("a command without a name")?
            .to_owned();
        let options = names(&command["arguments"]);
        let mut words = name.split(' ');
        let family = words.next().unwrap_or_default().to_owned();
        let entry = families.entry(family).or_default();
        entry.extend(options.iter().cloned());
        entry.extend(words.map(str::to_owned));
        commands.push((name, options.into_iter().collect()));
    }
    Ok(Cli {
        global,
        families,
        commands,
    })
}

fn repository() -> PathBuf {
    crate::support::workspace().to_path_buf()
}

fn markdown(folder: &Path, found: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            markdown(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "md") {
            found.push(path);
        }
    }
    Ok(())
}

/// pages a user or an agent reads: the user guide, the front page and the agent guides.
fn user_pages() -> Result<Vec<PathBuf>> {
    let root = repository();
    let mut pages = Vec::new();
    markdown(&root.join("docs/user"), &mut pages)?;
    pages
        .extend(["README.md", "llms.txt", "skills/fetchloom/SKILL.md"].map(|page| root.join(page)));
    pages.sort();
    Ok(pages)
}

/// lines inside fenced shell examples, with their line numbers.
fn shell_lines(text: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut shell = None;
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(language) = trimmed.strip_prefix("```") {
            shell = match shell {
                Some(_) => None,
                None => Some(matches!(
                    language.trim(),
                    "sh" | "bash" | "shell" | "console" | "powershell"
                )),
            };
        } else if shell == Some(true) {
            lines.push((number + 1, trimmed.to_owned()));
        }
    }
    lines
}

/// shell words up to the first comment, pipe or command separator, without their quotes.
fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for character in line.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '#' | '|' | ';' | '&') if word.is_empty() => break,
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            (Some(_) | None, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let redirect = |word: &String| {
        word.trim_start_matches(|c: char| c.is_ascii_digit())
            .starts_with(['>', '<'])
    };
    words.truncate(words.iter().position(redirect).unwrap_or(words.len()));
    words
}

#[test]
fn every_option_is_on_its_command_page() -> Result {
    let cli = cli()?;
    let mut missing = Vec::new();
    for (name, options) in &cli.commands {
        if name == "help" {
            continue;
        }
        let family = name.split(' ').next().unwrap_or_default();
        let page = repository().join(format!("docs/user/commands/{family}.md"));
        let text = fs::read_to_string(&page).map_err(|err| format!("{}: {err}", page.display()))?;
        for word in name
            .split(' ')
            .skip(1)
            .chain(options.iter().map(String::as_str))
        {
            if !text.contains(word) {
                missing.push(format!(
                    "{family}.md does not mention {word} of `fetchloom {name}`"
                ));
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    Ok(())
}

/// whether `line` holds `option` as a word of its own, so `-o` is not found inside `--offline`.
fn names_option(line: &str, option: &str) -> bool {
    line.match_indices(option).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + option.len()..].chars().next();
        before.is_none_or(|c| !c.is_alphanumeric() && c != '-')
            && after.is_none_or(|c| !c.is_alphanumeric() && c != '-')
    })
}

/// an agent may read only its skill or llms.txt, so each names a command's options on its line.
#[test]
fn the_agent_guides_name_every_command_and_option() -> Result {
    let document = json(&Sandbox::new()?.run(&["help", "--json"])?)?;
    let result = &document["result"];
    let spellings = |argument: &serde_json::Value| -> Vec<String> {
        let mut names: Vec<String> = ["name", "short"]
            .iter()
            .filter_map(|key| argument[*key].as_str())
            .filter(|name| name.starts_with('-'))
            .map(str::to_owned)
            .collect();
        names.dedup();
        names
    };
    let global: Vec<Vec<String>> = result["options"]
        .as_array()
        .into_iter()
        .flatten()
        .map(spellings)
        .filter(|names| {
            !names
                .iter()
                .any(|name| name == "--help" || name == "--version")
        })
        .collect();
    let mut missing = Vec::new();
    for guide in ["skills/fetchloom/SKILL.md", "llms.txt"] {
        let text = fs::read_to_string(repository().join(guide))?;
        for names in &global {
            if !names.iter().any(|name| names_option(&text, name)) {
                missing.push(format!("{guide} names no {}", names.join(" or ")));
            }
        }
        for command in result["commands"].as_array().into_iter().flatten() {
            let name = command["name"].as_str().ok_or("a command without a name")?;
            let forms = [
                format!("fetchloom {name}"),
                format!("`{name}"),
                format!("[{name}]"),
            ];
            let lines: Vec<&str> = text
                .lines()
                .filter(|line| forms.iter().any(|form| line.contains(form.as_str())))
                .collect();
            if lines.is_empty() {
                missing.push(format!("{guide} does not name `fetchloom {name}`"));
                continue;
            }
            for names in command["arguments"]
                .as_array()
                .into_iter()
                .flatten()
                .map(spellings)
            {
                if names.is_empty() || global.contains(&names) {
                    continue;
                }
                let named = lines
                    .iter()
                    .any(|line| names.iter().any(|option| names_option(line, option)));
                if !named {
                    missing.push(format!(
                        "{guide} names no {} on a line about `fetchloom {name}`",
                        names.join(" or ")
                    ));
                }
            }
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
    Ok(())
}

#[test]
fn shell_examples_use_real_commands_and_options() -> Result {
    let cli = cli()?;
    let mut wrong = Vec::new();
    for page in user_pages()? {
        let text = fs::read_to_string(&page)?;
        for (number, line) in shell_lines(&text) {
            let line = line.strip_prefix("$ ").unwrap_or(&line);
            let words = words(line);
            if words.first().is_none_or(|first| first != "fetchloom") {
                continue;
            }
            let at = format!("{}:{number}", page.strip_prefix(repository())?.display());
            let mut rest = words[1..].iter().skip_while(|word| word.starts_with('-'));
            let Some(command) = rest.next() else { continue };
            let Some(known) = cli.families.get(command.as_str()) else {
                if command != "help" {
                    wrong.push(format!("{at}: `fetchloom {command}` is not a command"));
                }
                continue;
            };
            for word in &words[1..] {
                let flag = word.split('=').next().unwrap_or(word);
                let option = flag.starts_with("--") || (flag.len() == 2 && flag.starts_with('-'));
                if option
                    && !known.contains(flag)
                    && !cli.global.contains(flag)
                    && !["--help", "-h", "--version", "-V"].contains(&flag)
                {
                    wrong.push(format!("{at}: `fetchloom {command}` has no option {flag}"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}

/// `FETCHLOOM_` names in a text, without the bare prefix.
fn settings(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find("FETCHLOOM_") {
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            .unwrap_or(tail.len());
        let name = tail[..end].trim_end_matches('_');
        if name.len() > "FETCHLOOM_".len() {
            found.insert(name.to_owned());
        }
        rest = &tail[end.max(1)..];
    }
    found
}

#[test]
fn every_setting_is_documented_and_every_documented_one_is_read() -> Result {
    let root = repository();
    let mut sources = Vec::new();
    collect_rust(&root.join("src"), &mut sources)?;
    let mut read = BTreeSet::new();
    for source in &sources {
        let text = fs::read_to_string(source)?;
        for name in settings(&text) {
            // test hooks and build-time stamps are not user settings
            let build_time = text.contains(&format!("option_env!(\"{name}\")"));
            if !name.starts_with("FETCHLOOM_TEST_") && !build_time {
                read.insert(name);
            }
        }
    }
    let installers: BTreeSet<String> = ["install.sh", "install.ps1"]
        .iter()
        .map(|name| fs::read_to_string(root.join(name)))
        .collect::<std::io::Result<Vec<_>>>()?
        .iter()
        .flat_map(|text| settings(text))
        .collect();
    let environment = fs::read_to_string(root.join("docs/user/reference/environment.md"))?;
    let mut problems: Vec<String> = read
        .iter()
        .filter(|name| !environment.contains(name.as_str()))
        .map(|name| {
            format!("{name} is read by fetchloom but not in docs/user/reference/environment.md")
        })
        .collect();
    let mut documented = BTreeSet::new();
    for page in user_pages()? {
        documented.extend(settings(&fs::read_to_string(page)?));
    }
    for name in documented {
        if !read.contains(&name)
            && !installers.contains(&name)
            && !name.starts_with("FETCHLOOM_TEST_")
        {
            problems.push(format!(
                "the user docs name {name}, which neither fetchloom nor its installers read"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

/// `completions` is left out, since its `--json` is always a usage error.
#[test]
fn every_command_result_is_in_the_json_reference() -> Result {
    let cli = cli()?;
    let page = fs::read_to_string(repository().join("docs/user/reference/json-output.md"))?;
    let listed: BTreeSet<&str> = page
        .lines()
        .skip_while(|line| *line != "## command results")
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .filter_map(|row| row.split('|').nth(1))
        .flat_map(|cell| cell.split('`').skip(1).step_by(2))
        .collect();
    let missing: Vec<&str> = cli
        .commands
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| *name != "completions" && !listed.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/user/reference/json-output.md has no result row for {missing:?}"
    );
    Ok(())
}

fn collect_rust(folder: &Path, found: &mut Vec<PathBuf>) -> Result {
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rust(&path, found)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    Ok(())
}
