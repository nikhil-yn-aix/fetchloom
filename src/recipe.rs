//! recipes: a dataset made from other datasets by a command data.toml names. this module reads
//! the command's words, orders recipes after the datasets they read, and keys each one by
//! everything its result depends on, so fetchloom can tell when a pinned result is out of date.
//! running a step belongs to `fetchloom build` alone.

pub(crate) mod code;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::path::Path;

use crate::digest::{self, Sha256};
use crate::error::Error;
use crate::glob::Glob;
use crate::project::lock::derived::{Code, Derived, Input};
use crate::project::lock::{Lock, Locked};
use crate::project::manifest::{Entry, Manifest, Members, Origin};

/// the name a word of `run` uses for the folder the result goes in, so no input may have it.
pub(crate) const OUT: &str = "out";
/// how a key is made. a change to what enters a key, or how, takes a new name, so no result made
/// the old way stands for the new.
const SCHEME: &str = "fetchloom-recipe-1";

/// one word of a recipe's `run` as data.toml writes it: text, with `{name}` for the folder of an
/// input and `{out}` for the folder the result goes in. `{{` and `}}` stand for one brace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template {
    text: String,
    pieces: Vec<Piece>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Folder(String),
}

/// a folder a word of `run` names: an input's, by its name, or the result's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot<'a> {
    Input(&'a str),
    Out,
}

impl Template {
    /// reads one word of `run`, or says why it is refused: a brace that is not part of `{name}`,
    /// `{{` or `}}`, or a control character, which no program reads as meant.
    pub(crate) fn parse(text: &str) -> Result<Template, &'static str> {
        if text.contains(crate::name::is_control) {
            return Err("holds a control character");
        }
        let mut pieces = Vec::new();
        let mut plain = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    plain.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    plain.push('}');
                }
                '}' => return Err("has a } with no { before it"),
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some('{') | None => return Err("has a { with no } after it"),
                            Some(c) => name.push(c),
                        }
                    }
                    if name.is_empty() {
                        return Err("has {} with no name in it");
                    }
                    if !plain.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut plain)));
                    }
                    pieces.push(Piece::Folder(name));
                }
                c => plain.push(c),
            }
        }
        if !plain.is_empty() {
            pieces.push(Piece::Text(plain));
        }
        Ok(Template {
            text: text.to_owned(),
            pieces,
        })
    }

    /// the word as data.toml writes it, which the key and data.lock keep.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// the folders the word names, in order.
    pub(crate) fn slots(&self) -> impl Iterator<Item = Slot<'_>> {
        self.pieces.iter().filter_map(|piece| match piece {
            Piece::Text(_) => None,
            Piece::Folder(name) if name == OUT => Some(Slot::Out),
            Piece::Folder(name) => Some(Slot::Input(name)),
        })
    }

    /// the word as the step gets it, with each folder `folder` gives in place. a folder stays
    /// inside one argument, spaces and all, since no shell splits it.
    pub(crate) fn expand(&self, folder: &dyn Fn(Slot<'_>) -> OsString) -> OsString {
        let mut word = OsString::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => word.push(text),
                Piece::Folder(name) if name == OUT => word.push(folder(Slot::Out)),
                Piece::Folder(name) => word.push(folder(Slot::Input(name))),
            }
        }
        word
    }
}

/// every dataset in an order where each comes after the datasets it is made from, ties by name:
/// Kahn's algorithm with a name-ordered queue. a cycle gives its path instead, as `a`, `b`, `a`.
/// every name a recipe reads must be in `datasets`.
pub(crate) fn order(datasets: &BTreeMap<String, Entry>) -> Result<Vec<&str>, Vec<&str>> {
    let inputs = |name: &str| -> &[String] {
        datasets
            .get(name)
            .and_then(Entry::recipe)
            .map_or(&[], |recipe| recipe.from.as_slice())
    };
    let mut waiting: BTreeMap<&str, usize> = BTreeMap::new();
    let mut readers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in datasets.keys() {
        let from = inputs(name);
        waiting.insert(name, from.len());
        for input in from {
            readers.entry(input).or_default().push(name);
        }
    }
    let mut ready: BTreeSet<&str> = waiting
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut ordered = Vec::with_capacity(datasets.len());
    while let Some(name) = ready.pop_first() {
        ordered.push(name);
        for reader in readers.get(name).into_iter().flatten() {
            if let Some(count) = waiting.get_mut(reader) {
                *count -= 1;
                if *count == 0 {
                    ready.insert(reader);
                }
            }
        }
    }
    if ordered.len() == datasets.len() {
        return Ok(ordered);
    }
    // every dataset left reads another one left, so following inputs from the first name must
    // come back to a name it passed
    let placed: BTreeSet<&str> = ordered.iter().copied().collect();
    let left = |name: &&str| !placed.contains(name);
    let mut path: Vec<&str> = Vec::new();
    let mut at = datasets
        .keys()
        .map(String::as_str)
        .find(left)
        .unwrap_or_default();
    while !path.contains(&at) {
        path.push(at);
        at = inputs(at)
            .iter()
            .map(String::as_str)
            .filter(left)
            .min()
            .unwrap_or(at);
    }
    let start = path.iter().position(|name| *name == at).unwrap_or(0);
    let mut cycle = path.split_off(start);
    cycle.push(at);
    Err(cycle)
}

/// `names` and every dataset they are made from, directly or not, in the order [`order`] gives.
pub(crate) fn closure<'a>(datasets: &'a BTreeMap<String, Entry>, names: &[&str]) -> Vec<&'a str> {
    let mut needed: BTreeSet<&str> = BTreeSet::new();
    let mut next: Vec<&str> = names.to_vec();
    while let Some(name) = next.pop() {
        let Some((name, entry)) = datasets.get_key_value(name) else {
            continue;
        };
        if needed.insert(name)
            && let Some(recipe) = entry.recipe()
        {
            next.extend(recipe.from.iter().map(String::as_str));
        }
    }
    order(datasets)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| needed.contains(name))
        .collect()
}

/// what a recipe reads of a dataset fetched from a source: each file data.lock pins, and an
/// archive that unpacks by the tree of what it unpacks to, never its own bytes, since the step
/// finds its files and not it: a source that packs the same files again keeps the content. the
/// lines decide which files the step finds and where. links, versions and when it was pinned
/// stay out, so a source that moves or renames a version keeps the content. the dataset's
/// members tables, which choose files inside its archives, follow as lines of their own, sorted,
/// so a dataset without them keeps its content.
pub(crate) fn content(locked: &Locked, members: &Members) -> Sha256 {
    let mut files: Vec<_> = locked.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut text = String::new();
    for file in files {
        let _ = match file.unpacks_to {
            Some(tree) => writeln!(text, "{}\tsha256:{tree}", file.path),
            None => writeln!(text, "{}\t{}\t{}", file.path, file.size, file.sha256),
        };
    }
    for (archive, filter) in members {
        for (kind, patterns) in [("select", &filter.select), ("exclude", &filter.exclude)] {
            let mut patterns: Vec<&str> = patterns.globs().iter().map(Glob::as_str).collect();
            patterns.sort_unstable();
            patterns.dedup();
            for pattern in patterns {
                let _ = writeln!(text, "member\t{archive}\t{kind}\t{pattern}");
            }
        }
    }
    digest::sha256(text.as_bytes())
}

/// the key of a recipe's result: the hash of the JSON of [`SCHEME`], the content of each dataset
/// it reads, by name, its command as written, the hash of each file or folder of its code, by
/// path, and its variables, by name. it holds nothing of the machine, the project's place, the
/// dataset's own name or when it ran, so a moved project or a renamed recipe keeps its key.
pub(crate) fn key(
    from: &[Input],
    run: &[String],
    code: &[Code],
    env: &BTreeMap<String, String>,
) -> Sha256 {
    let mut inputs: Vec<(&str, String)> = from
        .iter()
        .map(|input| (input.name.as_str(), format!("sha256:{}", input.content)))
        .collect();
    inputs.sort();
    let mut hashes: Vec<(&str, String)> = code
        .iter()
        .map(|code| {
            let prefix = if code.folder { "tree:" } else { "" };
            (
                code.path.as_str(),
                format!("{prefix}sha256:{}", code.sha256),
            )
        })
        .collect();
    hashes.sort();
    let pairs = |pairs: &mut dyn Iterator<Item = (&str, &str)>| {
        let pairs: Vec<String> = pairs
            .map(|(name, value)| format!("[{},{}]", json(name), json(value)))
            .collect();
        format!("[{}]", pairs.join(","))
    };
    let words: Vec<String> = run.iter().map(|word| json(word)).collect();
    let text = format!(
        "{{\"scheme\":{},\"from\":{},\"run\":[{}],\"code\":{},\"env\":{}}}",
        json(SCHEME),
        pairs(&mut inputs.iter().map(|(name, hash)| (*name, hash.as_str()))),
        words.join(","),
        pairs(&mut hashes.iter().map(|(path, hash)| (*path, hash.as_str()))),
        pairs(
            &mut env
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
        ),
    );
    digest::sha256(text.as_bytes())
}

/// `text` as a JSON string, escaped as `serde_json` and python's `json.dumps` escape it.
fn json(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// a recipe as it stands now: the content data.lock pins of each dataset it reads, its command
/// as data.toml writes it, the hash of its code on disk and its variables, which make its key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) name: String,
    /// each dataset it reads, in data.toml's order.
    pub(crate) inputs: Vec<Read>,
    pub(crate) run: Vec<String>,
    /// each file or folder of its code, sorted by path, or none where nothing is.
    pub(crate) code: Vec<(String, Option<Code>)>,
    pub(crate) env: BTreeMap<String, String>,
}

/// a dataset a step reads, as data.lock pins it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) name: String,
    /// the content data.lock pins of it, or none while it pins none.
    pub(crate) content: Option<Sha256>,
    /// whether a recipe makes it, so only a build pins it.
    pub(crate) made: bool,
}

impl Step {
    /// the recipe `name` of `manifest` as it stands in the project at `root`, with what `lock`
    /// pins, or none when `name` is no recipe.
    ///
    /// # Errors
    ///
    /// the code's own refusals and read failures, from [`code::hash`].
    pub(crate) fn read(
        root: &Path,
        manifest: &Manifest,
        lock: &Lock,
        name: &str,
    ) -> Result<Option<Step>, Error> {
        let Some(recipe) = manifest.datasets.get(name).and_then(Entry::recipe) else {
            return Ok(None);
        };
        let inputs = recipe
            .from
            .iter()
            .map(|input| Read {
                name: input.clone(),
                content: pinned(manifest, lock, input),
                made: manifest
                    .datasets
                    .get(input)
                    .is_some_and(|entry| entry.recipe().is_some()),
            })
            .collect();
        let mut hashed = Vec::with_capacity(recipe.code.len());
        for path in &recipe.code {
            hashed.push((path.clone(), code::hash(root, name, path)?));
        }
        hashed.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Some(Step {
            name: name.to_owned(),
            inputs,
            run: recipe
                .run
                .iter()
                .map(|word| word.text().to_owned())
                .collect(),
            code: hashed,
            env: recipe.env.clone(),
        }))
    }

    /// each dataset it reads with its content, as data.lock records them, once data.lock pins
    /// every one.
    pub(crate) fn from(&self) -> Option<Vec<Input>> {
        self.inputs
            .iter()
            .map(|read| {
                Some(Input {
                    name: read.name.clone(),
                    content: read.content?,
                })
            })
            .collect()
    }

    /// its code with each hash, once every file and folder of it is there.
    pub(crate) fn hashed(&self) -> Option<Vec<Code>> {
        self.code.iter().map(|(_, code)| code.clone()).collect()
    }

    /// its key, once data.lock pins every dataset it reads and all its code is there.
    pub(crate) fn key(&self) -> Option<Sha256> {
        Some(key(&self.from()?, &self.run, &self.hashed()?, &self.env))
    }

    /// what moved since `pinned`, data.lock's result of it, was built: nothing when it still
    /// answers the recipe as it stands.
    pub(crate) fn changes(&self, pinned: Option<&Derived>) -> Vec<Change> {
        let unpinned = self
            .inputs
            .iter()
            .filter(|read| read.content.is_none())
            .map(|read| {
                if read.made {
                    Change::Unmade(read.name.clone())
                } else {
                    Change::Unpinned(read.name.clone())
                }
            });
        let Some(pinned) = pinned else {
            return std::iter::once(Change::Unbuilt).chain(unpinned).collect();
        };
        let mut changes: Vec<Change> = unpinned.collect();
        let now: BTreeMap<&str, Sha256> = self
            .inputs
            .iter()
            .filter_map(|read| Some((read.name.as_str(), read.content?)))
            .collect();
        let was: BTreeMap<&str, Sha256> = pinned
            .from
            .iter()
            .map(|input| (input.name.as_str(), input.content))
            .collect();
        let read: BTreeSet<&str> = now.keys().chain(was.keys()).copied().collect();
        for name in read {
            match (now.get(name), was.get(name)) {
                (Some(now), Some(was)) if now == was => {}
                (None, Some(_)) if self.inputs.iter().any(|read| read.name == name) => {}
                _ => changes.push(Change::Input(name.to_owned())),
            }
        }
        if self.run != pinned.run {
            changes.push(Change::Run);
        }
        let paths: BTreeSet<&str> = self
            .code
            .iter()
            .map(|(path, _)| path.as_str())
            .chain(pinned.code.iter().map(|code| code.path.as_str()))
            .collect();
        for path in paths {
            let now = self.code.iter().find(|(known, _)| known == path);
            let was = pinned.code.iter().find(|code| code.path == path);
            match (now, was) {
                (Some((_, None)), _) => changes.push(Change::Gone(path.to_owned())),
                (Some((_, Some(now))), Some(was)) if now == was => {}
                _ => changes.push(Change::Code(path.to_owned())),
            }
        }
        if self.env != pinned.env {
            changes.push(Change::Env);
        }
        if changes.is_empty() && self.key() != Some(pinned.key) {
            changes.push(Change::Key);
        }
        changes
    }
}

/// what data.lock pins of the dataset `name` that a recipe reads: a fetched dataset's content, or
/// the tree of a recipe's result.
fn pinned(manifest: &Manifest, lock: &Lock, name: &str) -> Option<Sha256> {
    match &manifest.datasets.get(name)?.origin {
        Origin::Source(_) => {
            source_read(manifest, lock, name).map(|(locked, members)| content(locked, members))
        }
        Origin::Derived(_) => lock.result(name).map(|result| result.tree),
    }
}

/// what a recipe reads of the fetched dataset `name`: its pin in `lock` and the members tables
/// `manifest` gives it, which choose the files inside its archives. its content, and so a key,
/// and the files a build lays out for the step both come from here, so they cannot disagree.
pub(crate) fn source_read<'a>(
    manifest: &'a Manifest,
    lock: &'a Lock,
    name: &str,
) -> Option<(&'a Locked, &'a Members)> {
    let source = manifest.datasets.get(name)?.source()?;
    Some((lock.get(name)?, &source.members))
}

/// what moved since a recipe's result was built, so data.lock no longer pins what building it now
/// would make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// data.lock pins no result of it yet.
    Unbuilt,
    /// data.lock pins nothing yet of a fetched dataset it reads.
    Unpinned(String),
    /// data.lock pins no result yet of a recipe it reads.
    Unmade(String),
    /// a dataset it reads holds other content, or it reads another set of datasets.
    Input(String),
    Run,
    /// a file or folder of its code changed, or code names another set of them.
    Code(String),
    /// a file or folder of its code is not there.
    Gone(String),
    Env,
    /// everything it records agrees, yet its key does not: data.lock made it another way.
    Key,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::Unbuilt => f.write_str("not built yet"),
            Change::Unpinned(input) => write!(f, "{input} is not pinned yet"),
            Change::Unmade(input) => write!(f, "{input} is not built yet"),
            Change::Input(input) => write!(f, "{input} changed"),
            Change::Run => f.write_str("run changed"),
            Change::Code(path) => write!(f, "{path} changed"),
            Change::Gone(path) => write!(f, "{path} is missing"),
            Change::Env => f.write_str("env changed"),
            Change::Key => f.write_str("its key in data.lock is not the one its recipe makes"),
        }
    }
}

/// what moved since data.lock's result of the recipe `name` was built, as [`Step::changes`]
/// finds it; nothing for a dataset fetched from a source.
///
/// # Errors
///
/// as [`Step::read`].
pub(crate) fn drift(
    root: &Path,
    manifest: &Manifest,
    lock: &Lock,
    name: &str,
) -> Result<Vec<Change>, Error> {
    Ok(Step::read(root, manifest, lock, name)?
        .map(|step| step.changes(lock.result(name)))
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Member;
    use crate::project::lock::LockedFile;
    use crate::source::Access;

    /// a recipe's inputs, command, code and variables, with escapes and unicode in them.
    fn parts() -> (Vec<Input>, Vec<String>, Vec<Code>, BTreeMap<String, String>) {
        let input = |name: &str| Input {
            name: name.to_owned(),
            content: digest::sha256(name.as_bytes()),
        };
        let run = [
            "python3",
            "clean.py",
            "{raw}",
            "{labels}",
            "{out}",
            "--sep=\"é\\",
        ];
        let code = [
            Code {
                path: "lib".to_owned(),
                sha256: digest::sha256(b"lib"),
                folder: true,
            },
            Code {
                path: "clean.py".to_owned(),
                sha256: digest::sha256(b"clean"),
                folder: false,
            },
        ];
        let env = [("SEED", "1"), ("NOTE", "a\nb\u{1}")]
            .map(|(name, value)| (name.to_owned(), value.to_owned()));
        (
            vec![input("raw"), input("labels")],
            run.map(str::to_owned).to_vec(),
            code.to_vec(),
            env.into_iter().collect(),
        )
    }

    /// a build reads what each recipe is made from first: every dataset a name needs comes
    /// before it, ties go by name, and datasets nothing named needs stay out.
    #[test]
    fn closure_orders_what_names_need() {
        let manifest = Manifest::parse(
            "[datasets.raw]\nref = \"zenodo:1\"\n\n[datasets.labels]\nref = \"zenodo:2\"\n\n[datasets.unused]\nref = \"zenodo:3\"\n\n[datasets.b]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\n\n[datasets.a]\nfrom = [\"labels\", \"b\"]\nrun = [\"t\", \"{labels}\", \"{b}\", \"{out}\"]\n\n[datasets.c]\nfrom = [\"raw\"]\nrun = [\"t\", \"{raw}\", \"{out}\"]\n",
        )
        .unwrap();
        assert_eq!(
            closure(&manifest.datasets, &["a"]),
            ["labels", "raw", "b", "a"]
        );
        assert_eq!(
            closure(&manifest.datasets, &["c", "a"]),
            ["labels", "raw", "b", "a", "c"]
        );
        assert_eq!(closure(&manifest.datasets, &["raw"]), ["raw"]);
    }

    /// the key's JSON, as python's `json.dumps(obj, separators=(",", ":"), ensure_ascii=False)`
    /// writes it, gave this hash; a change to how keys are made fails here first.
    #[test]
    fn a_known_key_matches_python() {
        let (from, run, code, env) = parts();
        assert_eq!(
            key(&from, &run, &code, &env).hex(),
            "8b4d3b6143e8731693dab2b9bec3dd9e1e1910268c5d4c4ceca028111f31a654"
        );
    }

    #[test]
    fn the_key_follows_run_code_env_and_inputs() {
        let (from, run, code, env) = parts();
        let base = key(&from, &run, &code, &env);
        let mut reordered = (from.clone(), code.clone());
        reordered.0.reverse();
        reordered.1.reverse();
        assert_eq!(key(&reordered.0, &run, &reordered.1, &env), base);
        let mut changed_input = from.clone();
        changed_input[0].content = digest::sha256(b"other");
        let mut changed_run = run.clone();
        changed_run.push("--fast".to_owned());
        let mut changed_code = code.clone();
        changed_code[1].sha256 = digest::sha256(b"clean, edited");
        let mut as_file = code.clone();
        as_file[0].folder = false;
        let mut changed_env = env.clone();
        changed_env.insert("SEED".to_owned(), "2".to_owned());
        for other in [
            key(&changed_input, &run, &code, &env),
            key(&from, &changed_run, &code, &env),
            key(&from, &run, &changed_code, &env),
            key(&from, &run, &as_file, &env),
            key(&from, &run, &code, &changed_env),
            key(&from[..1], &run, &code, &env),
        ] {
            assert_ne!(other, base);
        }
    }

    fn locked(files: Vec<LockedFile>) -> Locked {
        Locked {
            name: "raw".to_owned(),
            reference: "zenodo:1".to_owned(),
            spec: "sha256:00".to_owned(),
            extract: true,
            resolved: "zenodo:1@1".to_owned(),
            title: Some("raw".to_owned()),
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            files,
        }
    }

    fn pinned(path: &str, bytes: &[u8], unpacks_to: Option<Sha256>) -> LockedFile {
        LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to,
        }
    }

    #[test]
    fn source_content_ignores_urls_and_versions() {
        let files = vec![
            pinned("b/x.zip", b"zipped", Some(digest::sha256(b"tree"))),
            pinned("a.csv", b"abc", None),
        ];
        let raw = locked(files.clone());
        let none = Members::new();
        // the lines `path \t size \t sha256`, or `path \t sha256:<tree>` for an archive that
        // unpacks, hashed by python
        assert_eq!(
            content(&raw, &none).hex(),
            "44f9f9781b9da7e9e6ab6aca1196d45e10f048968b6f5e2c994f47b0be1bc8a6"
        );
        let mut moved = raw.clone();
        moved.reference = "zenodo:2".to_owned();
        moved.resolved = "zenodo:2@7".to_owned();
        moved.spec = "sha256:11".to_owned();
        moved.title = None;
        moved.timestamp = "2027-01-01T00:00:00Z".to_owned();
        moved.files.reverse();
        for file in &mut moved.files {
            file.url = format!("https://mirror.example.org/{}", file.path);
            file.md5 = crate::digest::Md5::from_hex("900150983cd24fb0d6963f7d28e17f72");
        }
        assert_eq!(content(&moved, &none), content(&raw, &none));
        // the step reads the archive's files, so the same files packed again keep the content
        let mut rezipped = raw.clone();
        let tree = rezipped.files[0].unpacks_to;
        rezipped.files[0] = pinned("b/x.zip", b"zipped again", tree);
        assert_eq!(content(&rezipped, &none), content(&raw, &none));
        let mut packed = raw.clone();
        packed.files[0].unpacks_to = None;
        let mut edited = raw.clone();
        edited.files[1] = pinned("a.csv", b"abd", None);
        let mut renamed = raw.clone();
        renamed.files[1].path = "A.csv".to_owned();
        for other in [packed, edited, renamed] {
            assert_ne!(content(&other, &none), content(&raw, &none));
        }
    }

    /// a dataset's members tables decide which files a step finds, so each pattern follows the
    /// files as a line of its own, in an order that does not depend on how they are written. a
    /// dataset without them keeps the content it had.
    #[test]
    fn member_filters_change_content() {
        let raw = locked(vec![
            pinned("b/x.zip", b"zipped", Some(digest::sha256(b"tree"))),
            pinned("a.csv", b"abc", None),
        ]);
        let members = |table: &str| {
            Manifest::parse(&format!("[datasets.raw]\nref = \"zenodo:1\"\n{table}"))
                .unwrap()
                .datasets["raw"]
                .source()
                .unwrap()
                .members
                .clone()
        };
        let filtered = members(
            "[datasets.raw.members.\"b/x.zip\"]\nexclude = [\"pets/x.png\", \"pets/a.png\", \"pets/x.png\"]\nselect = \"pets/**\"\n",
        );
        // the lines of the files, then `member \t <archive> \t select or exclude \t <pattern>`,
        // hashed by python
        assert_eq!(
            content(&raw, &filtered).hex(),
            "dd3f97738327867f28f3bb40bcf757c2c31678258620821025953f369470eecd"
        );
        let reordered = members(
            "[datasets.raw.members.\"b/x.zip\"]\nselect = [\"pets/**\"]\nexclude = [\"pets/a.png\", \"pets/x.png\"]\n",
        );
        assert_eq!(content(&raw, &reordered), content(&raw, &filtered));
        assert_eq!(
            content(&raw, &members("")).hex(),
            "44f9f9781b9da7e9e6ab6aca1196d45e10f048968b6f5e2c994f47b0be1bc8a6"
        );
    }

    /// a project with the dataset `raw` pinned in data.lock and the recipe `name` made from it
    /// with the code `code`, each a path below the project and its bytes.
    fn project(name: &str, code: &[(&str, &[u8])]) -> (tempfile::TempDir, Manifest, Lock) {
        let root = tempfile::tempdir().unwrap();
        for (path, bytes) in code {
            let at = root.path().join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, bytes).unwrap();
        }
        let paths: Vec<String> = code.iter().map(|(path, _)| format!("\"{path}\"")).collect();
        let manifest = Manifest::parse(&format!(
            "[datasets.raw]\nref = \"zenodo:1\"\n\n[datasets.{name}]\nfrom = [\"raw\"]\nrun = [\"python3\", \"clean.py\", \"{{raw}}\", \"{{out}}\"]\ncode = [{}]\nenv = {{ SEED = \"1\" }}\n",
            paths.join(", ")
        ))
        .unwrap();
        let lock = Lock {
            datasets: vec![locked(vec![pinned("a.csv", b"abc", None)])],
            ..Lock::default()
        };
        (root, manifest, lock)
    }

    /// a moved project, a renamed recipe, code listed in another order or checked out with CRLF
    /// line endings keep the key, so a result built anywhere stands for any clone.
    #[test]
    fn the_key_ignores_paths_names_and_code_order() {
        let code: [(&str, &[u8]); 2] = [("clean.py", b"print()\n"), ("lib/util.py", b"x = 1\n")];
        let (here, manifest, lock) = project("clean", &code);
        let step = Step::read(here.path(), &manifest, &lock, "clean")
            .unwrap()
            .unwrap();
        let key = step.key().unwrap();
        let mut other: Vec<(&str, &[u8])> =
            vec![("lib/util.py", b"x = 1\r\n"), ("clean.py", b"print()\r\n")];
        let (there, renamed, lock) = project("tidy", &other);
        let moved = Step::read(there.path(), &renamed, &lock, "tidy")
            .unwrap()
            .unwrap();
        assert_ne!(here.path(), there.path());
        assert_eq!(moved.key(), Some(key));
        other[0].1 = b"x = 2\n";
        let (edited, manifest, lock) = project("clean", &other);
        let changed = Step::read(edited.path(), &manifest, &lock, "clean")
            .unwrap()
            .unwrap();
        assert_ne!(changed.key(), Some(key));
        assert_eq!(
            Step::read(here.path(), &manifest, &lock, "raw").unwrap(),
            None
        );
    }

    #[test]
    fn changes_name_what_moved_since_the_build() {
        let (root, manifest, lock) = project("clean", &[("clean.py", b"print()\n")]);
        let step = Step::read(root.path(), &manifest, &lock, "clean")
            .unwrap()
            .unwrap();
        let built = Derived {
            name: "clean".to_owned(),
            from: step.from().unwrap(),
            run: step.run.clone(),
            code: step.hashed().unwrap(),
            env: step.env.clone(),
            key: step.key().unwrap(),
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            tree: crate::cache::tree_hash(&[]),
            files: Vec::<Member>::new(),
        };
        assert_eq!(step.changes(Some(&built)), []);
        assert_eq!(step.changes(None), [Change::Unbuilt]);
        let mut old = built.clone();
        old.from[0].content = digest::sha256(b"older raw");
        old.run.pop();
        old.code[0].sha256 = digest::sha256(b"older code");
        old.env.clear();
        assert_eq!(
            step.changes(Some(&old)),
            [
                Change::Input("raw".to_owned()),
                Change::Run,
                Change::Code("clean.py".to_owned()),
                Change::Env
            ]
        );
        let mut rekeyed = built.clone();
        rekeyed.key = digest::sha256(b"another scheme");
        assert_eq!(step.changes(Some(&rekeyed)), [Change::Key]);
        std::fs::remove_file(root.path().join("clean.py")).unwrap();
        let unpinned = Lock::default();
        let gone = Step::read(root.path(), &manifest, &unpinned, "clean")
            .unwrap()
            .unwrap();
        assert_eq!(gone.key(), None);
        assert_eq!(
            gone.changes(Some(&built)),
            [
                Change::Unpinned("raw".to_owned()),
                Change::Gone("clean.py".to_owned())
            ]
        );
        assert_eq!(
            gone.changes(None)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["not built yet", "raw is not pinned yet"]
        );
    }

    #[test]
    fn templates_name_folders_and_keep_literal_braces() {
        let template = Template::parse("--images={pets}/{{a}}:{out}").unwrap();
        assert_eq!(template.text(), "--images={pets}/{{a}}:{out}");
        assert_eq!(
            template.slots().collect::<Vec<_>>(),
            [Slot::Input("pets"), Slot::Out]
        );
        let folder = |slot: Slot<'_>| match slot {
            Slot::Input(name) => OsString::from(format!("/in/{name} and more")),
            Slot::Out => OsString::from("/out"),
        };
        assert_eq!(
            template.expand(&folder),
            "--images=/in/pets and more/{a}:/out",
            "a folder with spaces stays inside its one argument"
        );
        assert_eq!(Template::parse("plain").unwrap().expand(&folder), "plain");
        for (text, reason) in [
            ("{pets", "has a { with no } after it"),
            ("{a{b}", "has a { with no } after it"),
            ("a}", "has a } with no { before it"),
            ("{}", "has {} with no name in it"),
            ("a\tb", "holds a control character"),
            ("a\nb", "holds a control character"),
        ] {
            assert_eq!(Template::parse(text), Err(reason), "{text}");
        }
    }
}
