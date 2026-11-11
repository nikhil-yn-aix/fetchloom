//! datasets a recipe makes: what each was made from, by which command and code, and the tree of
//! files its result holds. only `fetchloom build` makes them; every other command reads them,
//! keeps them as they are and places nothing from them yet. a result's files always live in a
//! list in [`lists::FOLDER`], since a rebuild rewrites them wholesale and data.lock stays about
//! recipes and pins.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{broken, lists, quoted};
use crate::cache::{Member, tree_hash};
use crate::digest::Sha256;
use crate::error::Error;
use crate::name;

/// a dataset a recipe made, as data.lock pins it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Derived {
    pub(crate) name: String,
    /// the datasets it is made from, in data.toml's order, each with the content its key took.
    pub(crate) from: Vec<Input>,
    /// the command as data.toml writes it, `{name}` and `{out}` not yet filled in.
    pub(crate) run: Vec<String>,
    /// the project files whose bytes entered the key, sorted by path.
    pub(crate) code: Vec<Code>,
    /// the variables set for the command.
    pub(crate) env: BTreeMap<String, String>,
    /// the hash of everything the result was made from, which names it in the cache.
    pub(crate) key: Sha256,
    /// when the key or the tree last changed, in UTC.
    pub(crate) timestamp: String,
    /// the tree hash of `files`, in the encoding of the cache's records of unpacked archives.
    pub(crate) tree: Sha256,
    /// the files the result holds, sorted by path.
    pub(crate) files: Vec<Member>,
}

/// a dataset a recipe reads, and the hash of its content the key took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Input {
    pub(crate) name: String,
    pub(crate) content: Sha256,
}

/// a project file whose bytes entered the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Code {
    pub(crate) path: String,
    pub(crate) sha256: Sha256,
}

/// refuses what fetchloom never writes for a recipe: no input, the dataset as its own input or
/// one input twice, no program, a code path that leaves the project or comes twice, or a
/// variable no program can be given. the files are checked once their list is read.
pub(super) fn check(derived: &Derived) -> Result<(), Error> {
    let name = &derived.name;
    if derived.from.is_empty() {
        return Err(broken(format!(
            "data.lock gives {name} no dataset it is made from"
        )));
    }
    for (index, input) in derived.from.iter().enumerate() {
        if input.name.is_empty()
            || input.name == *name
            || derived.from[..index]
                .iter()
                .any(|earlier| earlier.name == input.name)
        {
            return Err(broken(format!(
                "data.lock gives {name} the input {} twice, as itself or with no name",
                quoted(&input.name)
            )));
        }
    }
    if derived.run.first().is_none_or(String::is_empty)
        || derived.run.iter().any(|argument| has_nul(argument))
    {
        return Err(broken(format!(
            "data.lock gives {name} no program to run, or an argument no program can be given"
        )));
    }
    for (index, code) in derived.code.iter().enumerate() {
        if let Some(reason) = name::escapes(&code.path) {
            return Err(broken(format!(
                "data.lock names the code {} of {name}, which fetchloom refuses because {reason}",
                quoted(&code.path)
            )));
        }
        if index > 0 && derived.code[index - 1].path >= code.path {
            return Err(broken(format!(
                "data.lock names the code {} of {name} twice",
                quoted(&code.path)
            )));
        }
    }
    for (variable, value) in &derived.env {
        if variable.is_empty() || variable.contains('=') || has_nul(variable) || has_nul(value) {
            return Err(broken(format!(
                "data.lock gives {name} the variable {}, which no program can be given",
                quoted(variable)
            )));
        }
    }
    Ok(())
}

fn has_nul(text: &str) -> bool {
    text.contains('\0')
}

/// refuses files that would land outside the result's folder, or that are not the tree data.lock
/// records.
pub(super) fn check_files(derived: &Derived) -> Result<(), Error> {
    let name = &derived.name;
    for file in &derived.files {
        if let Some(reason) = name::escapes(&file.path) {
            return Err(broken(format!(
                "data.lock lists {} in {name}, which fetchloom refuses because {reason}",
                quoted(&file.path)
            )));
        }
    }
    if tree_hash(&derived.files) != derived.tree {
        return Err(Error::new(
            crate::error::Kind::ProjectInvalid,
            format!(
                "the files of {name} in {} are not the tree data.lock records",
                lists::FOLDER
            ),
        )
        .help(lists::BROKEN));
    }
    Ok(())
}

/// the dataset's table in the order data.lock keeps its keys.
pub(super) fn write(out: &mut String, derived: &Derived) {
    out.push_str("\n[[dataset]]\n");
    let _ = writeln!(out, "name = {}", quoted(&derived.name));
    out.push_str("from = [\n");
    for input in &derived.from {
        let _ = writeln!(
            out,
            "  {{ name = {}, content = \"sha256:{}\" }},",
            quoted(&input.name),
            input.content
        );
    }
    out.push_str("]\n");
    let run: Vec<String> = derived
        .run
        .iter()
        .map(|argument| quoted(argument))
        .collect();
    let _ = writeln!(out, "run = [{}]", run.join(", "));
    let mut code: Vec<&Code> = derived.code.iter().collect();
    code.sort_by(|a, b| a.path.cmp(&b.path));
    if code.is_empty() {
        out.push_str("code = []\n");
    } else {
        out.push_str("code = [\n");
        for file in code {
            let _ = writeln!(
                out,
                "  {{ path = {}, sha256 = \"{}\" }},",
                quoted(&file.path),
                file.sha256
            );
        }
        out.push_str("]\n");
    }
    if !derived.env.is_empty() {
        let pairs: Vec<String> = derived
            .env
            .iter()
            .map(|(variable, value)| format!("{} = {}", key(variable), quoted(value)))
            .collect();
        let _ = writeln!(out, "env = {{ {} }}", pairs.join(", "));
    }
    let _ = writeln!(out, "key = \"sha256:{}\"", derived.key);
    let _ = writeln!(out, "locked = {}", quoted(&derived.timestamp));
    let _ = writeln!(out, "tree = \"sha256:{}\"", derived.tree);
    let files = sorted(&derived.files);
    let texts = lists::texts(&files);
    super::write_listed(out, &lists::listed(&derived.name, &files, &texts));
}

/// the parts of the result's list, each with its path below the project and its text.
pub(super) fn parts(derived: &Derived) -> Vec<(String, String)> {
    let files = sorted(&derived.files);
    let texts = lists::texts(&files);
    let listed = lists::listed(&derived.name, &files, &texts);
    listed.parts.into_iter().zip(texts).collect()
}

fn sorted(files: &[Member]) -> Vec<Member> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted
}

/// a TOML key: bare when it is letters, digits, `_` and `-` only, otherwise a quoted string.
pub(super) fn key(text: &str) -> String {
    let bare = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if bare { text.to_owned() } else { quoted(text) }
}
