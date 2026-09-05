//! Which public items of a library crate are named from outside that crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const LIBRARIES: [&str; 8] = [
    "engine", "cli", "cache", "faults", "sources", "archive", "platform", "view",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Free,
    Module,
    Member,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct PubItem {
    pub(crate) name: String,
    pub(crate) owner: Option<String>,
    pub(crate) kind: Kind,
    pub(crate) line: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Evidence {
    OtherCrate,
    OwnTargetsOnly,
    None,
}

#[derive(Default)]
struct Counts {
    public: usize,
    restricted: usize,
}

pub(crate) fn run(workspace: &Path, arguments: &[String]) -> ExitCode {
    let verbose = arguments.iter().any(|argument| argument == "--list");
    let only = arguments
        .iter()
        .position(|argument| argument == "--crate")
        .and_then(|index| arguments.get(index + 1));

    let sources = match collect_sources(workspace) {
        Ok(sources) => sources,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };

    let mut unnamed = 0usize;
    println!("crate      pub  pub(crate)  unnamed  own-targets-only");
    for library in LIBRARIES {
        if only.is_some_and(|wanted| wanted != library) {
            continue;
        }
        let report = examine(library, &sources);
        unnamed += report.unnamed.len();
        println!(
            "{:<9} {:>4}  {:>10}  {:>7}  {:>16}",
            library,
            report.counts.public,
            report.counts.restricted,
            report.unnamed.len(),
            report.own_targets_only.len()
        );
        if verbose {
            for (path, item) in &report.unnamed {
                println!(
                    "  unnamed  {}:{} {}",
                    path.display(),
                    item.line,
                    describe(item)
                );
            }
            for (path, item) in &report.own_targets_only {
                println!(
                    "  tests    {}:{} {}",
                    path.display(),
                    item.line,
                    describe(item)
                );
            }
        }
    }

    println!("{unnamed} public items are named by no other crate");
    ExitCode::SUCCESS
}

fn describe(item: &PubItem) -> String {
    match &item.owner {
        Some(owner) => format!("{owner}::{}", item.name),
        None => item.name.clone(),
    }
}

struct Report {
    counts: Counts,
    unnamed: Vec<(PathBuf, PubItem)>,
    own_targets_only: Vec<(PathBuf, PubItem)>,
}

struct Sources {
    files: Vec<(PathBuf, String)>,
}

impl Sources {
    fn text_outside(&self, library: &str) -> Vec<&str> {
        let own = format!("crates/{library}/src/");
        self.files
            .iter()
            .filter(|(path, _)| !slash(path).contains(&own))
            .filter(|(path, _)| !is_own_target(path, library))
            .map(|(_, text)| text.as_str())
            .collect()
    }

    fn text_of_own_targets(&self, library: &str) -> Vec<&str> {
        self.files
            .iter()
            .filter(|(path, _)| is_own_target(path, library))
            .map(|(_, text)| text.as_str())
            .collect()
    }

    fn source_files(&self, library: &str) -> Vec<(&PathBuf, &String)> {
        let own = format!("crates/{library}/src/");
        self.files
            .iter()
            .filter(|(path, _)| slash(path).contains(&own) && !is_own_target(path, library))
            .map(|(path, text)| (path, text))
            .collect()
    }
}

fn is_own_target(path: &Path, library: &str) -> bool {
    let text = slash(path);
    text.contains(&format!("crates/{library}/tests/"))
        || text.contains(&format!("crates/{library}/benches/"))
        || text == format!("crates/{library}/src/main.rs")
}

fn slash(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

fn collect_sources(workspace: &Path) -> Result<Sources, String> {
    let mut files = Vec::new();
    let mut roots = vec![workspace.join("xtask").join("src")];
    for library in LIBRARIES {
        let crate_root = workspace.join("crates").join(library);
        roots.push(crate_root.join("src"));
        roots.push(crate_root.join("tests"));
        roots.push(crate_root.join("benches"));
    }
    for root in roots {
        gather(&root, workspace, &mut files)?;
    }
    Ok(Sources { files })
}

fn gather(root: &Path, workspace: &Path, files: &mut Vec<(PathBuf, String)>) -> Result<(), String> {
    if !root.is_dir() {
        return Ok(());
    }
    let entries =
        std::fs::read_dir(root).map_err(|error| format!("{}: {error}", root.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            gather(&path, workspace, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let relative = path.strip_prefix(workspace).unwrap_or(&path).to_path_buf();
            files.push((relative, text));
        }
    }
    Ok(())
}

fn examine(library: &str, sources: &Sources) -> Report {
    let outside = sources.text_outside(library);
    let own_targets = sources.text_of_own_targets(library);
    let mut counts = Counts::default();
    let mut unnamed = Vec::new();
    let mut own_targets_only = Vec::new();

    let mut per_file: BTreeMap<PathBuf, Vec<PubItem>> = BTreeMap::new();
    for (path, text) in sources.source_files(library) {
        counts.restricted += restricted_in(text);
        let items = items_in(text);
        counts.public += items.len();
        per_file.insert(path.clone(), items);
    }

    for (path, items) in &per_file {
        for item in items {
            match evidence(item, &outside, &own_targets) {
                Evidence::OtherCrate => {}
                Evidence::OwnTargetsOnly => own_targets_only.push((path.clone(), item.clone())),
                Evidence::None => unnamed.push((path.clone(), item.clone())),
            }
        }
    }

    Report {
        counts,
        unnamed,
        own_targets_only,
    }
}

pub(crate) fn evidence(item: &PubItem, outside: &[&str], own_targets: &[&str]) -> Evidence {
    if named_in(item, outside) {
        Evidence::OtherCrate
    } else if named_in(item, own_targets) {
        Evidence::OwnTargetsOnly
    } else {
        Evidence::None
    }
}

fn named_in(item: &PubItem, corpus: &[&str]) -> bool {
    match (&item.kind, &item.owner) {
        (Kind::Free, _) | (Kind::Member, None) => {
            corpus.iter().any(|text| contains_word(text, &item.name))
        }
        (Kind::Module, _) => {
            let path = format!("{}::", item.name);
            corpus.iter().any(|text| contains_word(text, &path))
        }
        (Kind::Member, Some(owner)) => {
            corpus.iter().any(|text| contains_word(text, owner))
                && corpus.iter().any(|text| contains_word(text, &item.name))
        }
    }
}

pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    let mut from = 0usize;
    while let Some(found) = haystack.get(from..).and_then(|rest| rest.find(needle)) {
        let start = from + found;
        let end = start + needle.len();
        let before = start
            .checked_sub(1)
            .and_then(|index| bytes.get(index))
            .copied();
        let after = bytes.get(end).copied();
        let opens = needle.as_bytes().first().copied().is_some_and(is_word_byte);
        let closes = needle.as_bytes().last().copied().is_some_and(is_word_byte);
        let joined_before = opens && before.is_some_and(is_word_byte);
        let joined_after = closes && after.is_some_and(is_word_byte);
        if !joined_before && !joined_after {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn restricted_in(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("pub(crate)")
                || trimmed.starts_with("pub(super)")
                || trimmed.starts_with("pub(self)")
        })
        .count()
}

pub(crate) fn items_in(text: &str) -> Vec<PubItem> {
    let mut items = Vec::new();
    let mut owner: Option<String> = None;
    let mut member_context = false;
    let mut skipping = false;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indentation = line.len() - trimmed.len();

        if indentation == 0 {
            if skipping {
                skipping = trimmed != "}";
                continue;
            }
            if trimmed.starts_with("#[cfg(test)]") {
                skipping = true;
                continue;
            }
            owner = None;
            member_context = false;
            if let Some(rest) = trimmed.strip_prefix("impl") {
                owner = self_type_of(rest);
                member_context = owner.is_some();
                continue;
            }
            if let Some(item) = free_item(trimmed, number) {
                if matches!(head_of(trimmed), Some("struct")) {
                    owner = Some(item.name.clone());
                    member_context = true;
                }
                items.push(item);
                continue;
            }
            if trimmed.starts_with("struct ") || trimmed.starts_with("union ") {
                owner = declared_name(trimmed);
                member_context = owner.is_some();
            }
            continue;
        }

        if skipping || !member_context || indentation != 4 {
            continue;
        }
        if let Some(name) = member_name(trimmed) {
            items.push(PubItem {
                name,
                owner: owner.clone(),
                kind: Kind::Member,
                line: number,
            });
        }
    }

    items
}

fn head_of(trimmed: &str) -> Option<&str> {
    let without = trimmed
        .strip_prefix("pub ")
        .or_else(|| trimmed.strip_prefix("pub(crate) "))
        .unwrap_or(trimmed);
    without.split_whitespace().next()
}

fn free_item(trimmed: &str, line: usize) -> Option<PubItem> {
    let rest = trimmed.strip_prefix("pub ")?;
    let name = declared_name(rest)?;
    let kind = if rest.starts_with("mod ") {
        Kind::Module
    } else {
        Kind::Free
    };
    Some(PubItem {
        name,
        owner: None,
        kind,
        line,
    })
}

fn declared_name(rest: &str) -> Option<String> {
    let mut words = rest.split_whitespace();
    let keyword = words.next()?;
    match keyword {
        "fn" | "struct" | "enum" | "trait" | "type" | "mod" | "union" => {
            Some(identifier(words.next()?))
        }
        "const" | "static" => {
            let next = words.next()?;
            let next = if next == "mut" { words.next()? } else { next };
            Some(identifier(next))
        }
        "unsafe" | "async" | "extern" | "default" => declared_name(rest.split_once(' ')?.1),
        "use" => reexported_name(rest),
        _ => None,
    }
}

fn reexported_name(rest: &str) -> Option<String> {
    let path = rest.strip_prefix("use ")?.trim_end_matches([';', ' ']);
    if path.ends_with('}') || path.ends_with('*') || path.contains('{') {
        return None;
    }
    let last = path.rsplit("::").next()?;
    Some(identifier(last))
}

fn identifier(word: &str) -> String {
    word.trim_start_matches("r#")
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect()
}

fn member_name(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("pub ")?;
    if let Some(name) = declared_name(rest) {
        return Some(name);
    }
    let field = rest.split_once(':')?.0;
    let name = identifier(field.trim());
    if name.is_empty() { None } else { Some(name) }
}

fn self_type_of(rest: &str) -> Option<String> {
    let rest = rest.trim_start();
    let rest = if rest.starts_with('<') {
        after_generics(rest)?
    } else {
        rest
    };
    let head = rest.rsplit(" for ").next()?;
    let head = head.trim().trim_start_matches('&');
    let name = identifier(head.trim_start_matches("dyn ").trim());
    if name.is_empty() { None } else { Some(name) }
}

fn after_generics(rest: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (index, character) in rest.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return rest.get(index + 1..);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Evidence, Kind, contains_word, evidence, is_own_target, items_in};
    use std::path::Path;

    #[test]
    fn a_binary_beside_the_library_is_a_target_outside_it() {
        assert!(is_own_target(Path::new("crates/cli/src/main.rs"), "cli"));
        assert!(!is_own_target(Path::new("crates/cli/src/why.rs"), "cli"));
        assert!(is_own_target(
            Path::new("crates/cli/tests/policy/lock.rs"),
            "cli"
        ));
    }

    #[test]
    fn a_free_function_is_found_by_name() {
        let items = items_in("pub fn classify() -> u8 {\n    0\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "classify");
        assert_eq!(items[0].kind, Kind::Free);
        assert_eq!(items[0].line, 1);
    }

    #[test]
    fn a_restricted_item_is_not_a_public_item() {
        assert!(items_in("pub(crate) fn hidden() {}\n").is_empty());
        assert!(items_in("fn private() {}\n").is_empty());
    }

    #[test]
    fn a_method_carries_the_type_it_is_reached_through() {
        let items = items_in("impl RunId {\n    pub fn new() -> Self {}\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "new");
        assert_eq!(items[0].owner.as_deref(), Some("RunId"));
        assert_eq!(items[0].kind, Kind::Member);
    }

    #[test]
    fn a_blank_line_between_two_methods_does_not_end_the_block() {
        let items = items_in("impl RunId {\n    pub fn one() {}\n\n    pub fn two() {}\n}\n");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].name, "two");
        assert_eq!(items[1].owner.as_deref(), Some("RunId"));
    }

    #[test]
    fn a_generic_trait_implementation_names_the_self_type() {
        let items = items_in("impl<T> Store for Cache<T> {\n    pub fn get() {}\n}\n");
        assert_eq!(items[0].owner.as_deref(), Some("Cache"));
    }

    #[test]
    fn a_public_field_is_a_member_of_its_struct() {
        let items = items_in("pub struct Witness {\n    pub digest: ContentDigest,\n}\n");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].name, "digest");
        assert_eq!(items[1].owner.as_deref(), Some("Witness"));
    }

    #[test]
    fn a_private_struct_still_owns_the_fields_declared_public() {
        let items = items_in("struct Inner {\n    pub count: usize,\n}\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].owner.as_deref(), Some("Inner"));
    }

    #[test]
    fn a_unit_test_module_contributes_nothing() {
        let text = "#[cfg(test)]\nmod tests {\n    pub fn helper() {}\n}\npub fn real() {}\n";
        let items = items_in(text);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "real");
    }

    #[test]
    fn a_nested_body_is_not_mistaken_for_a_member() {
        let text =
            "pub fn outer() {\n    let closure = || {\n        pub fn inner() {}\n    };\n}\n";
        let items = items_in(text);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "outer");
    }

    #[test]
    fn a_single_reexport_is_named_but_a_group_is_not() {
        let items = items_in("pub use crate::store::Cache;\npub use crate::store::{A, B};\n");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Cache");
    }

    #[test]
    fn a_module_is_reached_only_through_a_path_that_names_it() {
        let items = items_in("pub mod diagnosis;\n");
        assert_eq!(items[0].kind, Kind::Module);
        assert_eq!(
            evidence(&items[0], &["let diagnosis = read();"], &[]),
            Evidence::None
        );
        assert_eq!(
            evidence(&items[0], &["fetchloom_cache::diagnosis::Diagnosis"], &[]),
            Evidence::OtherCrate
        );
    }

    #[test]
    fn a_word_match_respects_boundaries() {
        assert!(contains_word("let x = classify(a);", "classify"));
        assert!(!contains_word("reclassify(a)", "classify"));
        assert!(!contains_word("classifying(a)", "classify"));
    }

    #[test]
    fn a_member_is_reachable_only_where_both_its_type_and_its_own_name_are_read() {
        let items = items_in("impl RunId {\n    pub fn as_str() {}\n}\n");
        let item = &items[0];
        assert_eq!(evidence(item, &["value.as_str()"], &[]), Evidence::None);
        assert_eq!(evidence(item, &["RunId::new(x)"], &[]), Evidence::None);
        assert_eq!(
            evidence(item, &["RunId::from(x).as_str()"], &[]),
            Evidence::OtherCrate
        );
    }

    #[test]
    fn a_use_only_from_the_crates_own_targets_is_reported_apart() {
        let items = items_in("pub fn classify() {}\n");
        assert_eq!(
            evidence(&items[0], &[], &["classify(a)"]),
            Evidence::OwnTargetsOnly
        );
    }
}
