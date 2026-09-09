//! The check that keeps this codebase's one documentation rule mechanical:
//! names and doc comments say what code means, and the only plain comment any
//! file may hold is a safety note on an unsafe operation.

use std::path::{Path, PathBuf};

pub struct PlainComment {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

impl std::fmt::Display for PlainComment {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "{}:{}: {}", self.path.display(), self.line, self.text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Doc,
    Safety,
    Plain,
}

/// Every plain comment in one file of Rust, by line.
///
/// A doc comment is not one, nor is a safety note or a line continuing one.
/// Everything else is: after code, inside a test, inside a macro body, or
/// written as a block.
#[must_use]
pub fn plain_comments_in(source: &str) -> Vec<(usize, String)> {
    let bytes: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    let mut line = 1usize;
    let mut at = 0usize;
    let mut previous_line_allowed: Option<usize> = None;
    while at < bytes.len() {
        let here = bytes[at];
        let next = bytes.get(at + 1).copied();
        match (here, next) {
            ('/', Some('/')) => {
                let start = line;
                let mut text = String::new();
                at += 2;
                while at < bytes.len() && bytes[at] != '\n' {
                    text.push(bytes[at]);
                    at += 1;
                }
                let kind = line_kind(&text, start, previous_line_allowed);
                if kind == Kind::Plain {
                    found.push((start, format!("//{text}").trim_end().to_owned()));
                } else {
                    previous_line_allowed = Some(start);
                }
            }
            ('/', Some('*')) => {
                let start = line;
                let doc = matches!(bytes.get(at + 2), Some('*' | '!'))
                    && !matches!(bytes.get(at + 3), Some('/'));
                let mut depth = 1usize;
                let mut text = String::new();
                at += 2;
                while at < bytes.len() && depth > 0 {
                    if bytes[at] == '\n' {
                        line += 1;
                    }
                    if bytes[at] == '/' && bytes.get(at + 1) == Some(&'*') {
                        depth += 1;
                        at += 2;
                        continue;
                    }
                    if bytes[at] == '*' && bytes.get(at + 1) == Some(&'/') {
                        depth -= 1;
                        at += 2;
                        continue;
                    }
                    if text.len() < 60 {
                        text.push(bytes[at]);
                    }
                    at += 1;
                }
                if !doc {
                    found.push((start, format!("/*{}", text.trim())));
                }
            }
            ('r', Some('"' | '#')) if starts_raw_string(&bytes, at) => {
                at = skip_raw_string(&bytes, at, &mut line);
            }
            ('"', _) => {
                at = skip_quoted(&bytes, at, '"', &mut line);
            }
            ('\'', _) => {
                at = skip_character(&bytes, at, &mut line);
            }
            ('\n', _) => {
                line += 1;
                at += 1;
            }
            _ => at += 1,
        }
    }
    found
}

fn line_kind(text: &str, line: usize, previous_line_allowed: Option<usize>) -> Kind {
    if text.starts_with('!') || (text.starts_with('/') && !text.starts_with("//")) {
        return Kind::Doc;
    }
    if text.trim_start().starts_with("SAFETY:") {
        return Kind::Safety;
    }
    if previous_line_allowed.is_some_and(|allowed| allowed + 1 == line) {
        return Kind::Safety;
    }
    Kind::Plain
}

fn starts_raw_string(bytes: &[char], at: usize) -> bool {
    let prefixed = at > 0 && matches!(bytes[at - 1], 'b' | 'c');
    let opens = at
        .checked_sub(usize::from(prefixed) + 1)
        .is_none_or(|before| !bytes[before].is_alphanumeric() && bytes[before] != '_');
    if !opens {
        return false;
    }
    if at > 0 && !prefixed && (bytes[at - 1].is_alphanumeric() || bytes[at - 1] == '_') {
        return false;
    }
    let mut cursor = at + 1;
    while bytes.get(cursor) == Some(&'#') {
        cursor += 1;
    }
    bytes.get(cursor) == Some(&'"')
}

fn skip_raw_string(bytes: &[char], at: usize, line: &mut usize) -> usize {
    let mut hashes = 0usize;
    let mut cursor = at + 1;
    while bytes.get(cursor) == Some(&'#') {
        hashes += 1;
        cursor += 1;
    }
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == '\n' {
            *line += 1;
        }
        if bytes[cursor] == '"' {
            let closing = (1..=hashes).all(|step| bytes.get(cursor + step) == Some(&'#'));
            if closing {
                return cursor + hashes + 1;
            }
        }
        cursor += 1;
    }
    cursor
}

fn skip_quoted(bytes: &[char], at: usize, terminator: char, line: &mut usize) -> usize {
    let mut cursor = at + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            '\\' => cursor += 2,
            '\n' => {
                *line += 1;
                cursor += 1;
            }
            found if found == terminator => return cursor + 1,
            _ => cursor += 1,
        }
    }
    cursor
}

fn skip_character(bytes: &[char], at: usize, line: &mut usize) -> usize {
    let closes = (1..=4).any(|step| bytes.get(at + step) == Some(&'\''));
    if closes {
        return skip_quoted(bytes, at, '\'', line);
    }
    at + 1
}

/// Every Rust file under `root`, skipping the build directory.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            if path.is_dir() {
                if matches!(
                    name.to_string_lossy().as_ref(),
                    "target" | ".git" | "mutants.out.old"
                ) {
                    continue;
                }
                pending.push(path);
            } else if path.extension().is_some_and(|kind| kind == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Every plain comment in the workspace, and how many files were read to find
/// them, so a walk that read nothing cannot report clean.
#[must_use]
pub fn plain_comments(root: &Path) -> (Vec<PlainComment>, usize) {
    let mut found = Vec::new();
    let mut read = 0usize;
    for path in sources(root) {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        read += 1;
        let shown = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        for (line, text) in plain_comments_in(&source) {
            found.push(PlainComment {
                path: shown.clone(),
                line,
                text,
            });
        }
    }
    (found, read)
}

/// Reports every plain comment in the workspace and whether there were none.
#[must_use]
pub fn run(root: &Path) -> bool {
    let (found, read) = plain_comments(root);
    if read < 100 {
        println!("only {read} Rust files were read, so this check proves nothing");
        return false;
    }
    for comment in &found {
        println!("{comment}");
    }
    if found.is_empty() {
        println!("{read} files hold no comment outside a SAFETY block");
        return true;
    }
    println!(
        "{} plain comments in {read} files. Move what each says into a name, a doc comment, or docs/contracts.md",
        found.len()
    );
    false
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "test setup, where a failure to build the input is the assertion"
    )]

    use super::{plain_comments, plain_comments_in};

    fn lines(source: &str) -> Vec<usize> {
        plain_comments_in(source)
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    #[test]
    fn a_plain_comment_on_its_own_line_is_found() {
        assert_eq!(lines("fn a() {\n    // why\n}\n"), vec![2]);
    }

    #[test]
    fn a_comment_after_code_on_the_same_line_is_found() {
        assert_eq!(lines("let a = 1; // why\n"), vec![1]);
    }

    #[test]
    fn a_doc_comment_of_either_kind_is_not_a_comment() {
        assert!(lines("//! module\n/// item\nfn a() {}\n").is_empty());
    }

    #[test]
    fn four_slashes_are_a_plain_comment_rather_than_a_doc_comment() {
        assert_eq!(lines("//// why\n"), vec![1]);
    }

    #[test]
    fn a_safety_note_and_the_lines_it_wraps_onto_are_allowed() {
        let source = "// SAFETY: the handle is open for the whole call, which is\n// the whole contract of the call below.\nfn a() {}\n";
        assert!(lines(source).is_empty());
    }

    #[test]
    fn a_comment_under_a_blank_line_after_a_safety_note_is_not_a_continuation() {
        let source = "// SAFETY: open for the call\n\n// why\nfn a() {}\n";
        assert_eq!(lines(source), vec![3]);
    }

    #[test]
    fn a_comment_inside_a_test_is_found() {
        let source =
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {\n        // why\n    }\n}\n";
        assert_eq!(lines(source), vec![5]);
    }

    #[test]
    fn a_comment_inside_a_macro_body_is_found() {
        let source = "macro_rules! m {\n    () => {\n        // why\n    };\n}\n";
        assert_eq!(lines(source), vec![3]);
    }

    #[test]
    fn a_block_comment_is_found_and_a_block_doc_comment_is_not() {
        assert_eq!(lines("/* why */\nfn a() {}\n"), vec![1]);
        assert!(lines("/** item */\nfn a() {}\n").is_empty());
        assert!(lines("/*! module */\n").is_empty());
    }

    #[test]
    fn a_block_comment_spanning_lines_is_reported_at_the_line_it_opened() {
        assert_eq!(lines("fn a() {}\n/* why\n   and more */\n"), vec![2]);
    }

    #[test]
    fn two_slashes_inside_a_string_are_not_a_comment() {
        assert!(lines("let a = \"https://host/x\";\n").is_empty());
        assert!(lines("assert_eq!(EntryPath::new(\"a//b\"), Err(x));\n").is_empty());
    }

    #[test]
    fn two_slashes_inside_a_raw_string_are_not_a_comment() {
        assert!(lines("let a = r#\"<a href=\"//elsewhere/x\">x</a>\"#;\n").is_empty());
        assert!(lines("let a = r\"//x\";\n").is_empty());
    }

    #[test]
    fn a_quote_inside_a_comment_does_not_swallow_the_file() {
        assert_eq!(lines("// it's here\nfn a() {}\n// and here\n"), vec![1, 3]);
    }

    #[test]
    fn a_lifetime_is_not_a_character_literal() {
        assert_eq!(
            lines("struct A<'a> { a: &'a str }\n// why\n"),
            vec![2],
            "a lifetime was read as an unterminated character literal"
        );
    }

    #[test]
    fn a_slash_in_a_character_literal_is_not_a_comment() {
        assert!(lines("let a = '/';\nlet b = 1;\n").is_empty());
    }

    #[test]
    fn the_workspace_holds_no_plain_comment_outside_a_safety_block() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let (found, read) = plain_comments(&root);
        assert!(
            read > 100,
            "the walk read {read} Rust files, so it proves nothing"
        );
        let named: Vec<String> = found.iter().map(ToString::to_string).collect();
        assert!(
            named.is_empty(),
            "a plain comment says what a name or a doc comment should say: {named:#?}"
        );
    }
}
