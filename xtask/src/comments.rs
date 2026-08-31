//! The checker for the comment and decoration rules no lint expresses.

use std::fmt;
use std::path::{Path, PathBuf};

/// What a file broke, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The file the rule was broken in.
    pub file: PathBuf,
    /// The line the rule was broken on, counting from one.
    pub line: usize,
    /// The rule that was broken.
    pub rule: Rule,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file.display(), self.line, self.rule)
    }
}

/// A rule the checker enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// A comment that is not a docstring and not a safety line.
    Comment,
    /// A block comment, which is never permitted in any form.
    BlockComment,
    /// A safety line that does not precede an unsafe block.
    SafetyOnSafeCode,
    /// A decorative symbol.
    Decoration,
    /// A banner or a section divider.
    Banner,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Comment => "comment that is not a docstring",
            Self::BlockComment => "block comment",
            Self::SafetyOnSafeCode => "safety line that does not precede an unsafe block",
            Self::Decoration => "decorative symbol",
            Self::Banner => "banner or section divider",
        };
        f.write_str(text)
    }
}

const BANNER_CHARACTERS: [char; 6] = ['=', '*', '#', '~', '_', '+'];

fn is_decoration(character: char) -> bool {
    matches!(character,
        '\u{1F000}'..='\u{1FAFF}'
            | '\u{2600}'..='\u{27BF}'
            | '\u{2B00}'..='\u{2BFF}'
            | '\u{2190}'..='\u{21FF}'
            | '\u{FE0F}'
    )
}

/// Checks one Rust source file.
#[must_use]
pub fn check_rust(file: &Path, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (offset, content) in text.lines().enumerate() {
        if content.chars().any(is_decoration) {
            findings.push(Finding {
                file: file.to_path_buf(),
                line: offset + 1,
                rule: Rule::Decoration,
            });
        }
    }

    let bytes: Vec<char> = text.chars().collect();
    let mut index = 0;
    let mut line = 1;
    let mut safety_lines: Vec<usize> = Vec::new();

    while index < bytes.len() {
        match bytes[index] {
            '\n' => {
                line += 1;
                index += 1;
            }
            '"' => {
                index = skip_string(&bytes, index + 1, &mut line);
            }
            '\'' => {
                index = skip_character(&bytes, index, &mut line);
            }
            'r' if starts_raw_string(&bytes, index) => {
                index = skip_raw_string(&bytes, index, &mut line);
            }
            'b' if index + 1 < bytes.len() && bytes[index + 1] == '"' => {
                index = skip_string(&bytes, index + 2, &mut line);
            }
            '/' if index + 1 < bytes.len() && bytes[index + 1] == '*' => {
                findings.push(Finding {
                    file: file.to_path_buf(),
                    line,
                    rule: Rule::BlockComment,
                });
                index = skip_block_comment(&bytes, index + 2, &mut line);
            }
            '/' if index + 1 < bytes.len() && bytes[index + 1] == '/' => {
                let kind = line_comment_kind(&bytes, index);
                let start_line = line;
                let end = end_of_line(&bytes, index);
                let body: String = bytes[index..end].iter().collect();
                match kind {
                    LineComment::Doc => {}
                    LineComment::Safety => safety_lines.push(start_line),
                    LineComment::Plain => {
                        let rule = if body
                            .trim_start_matches('/')
                            .trim_start()
                            .starts_with("SAFETY:")
                        {
                            Rule::SafetyOnSafeCode
                        } else {
                            Rule::Comment
                        };
                        findings.push(Finding {
                            file: file.to_path_buf(),
                            line: start_line,
                            rule,
                        });
                    }
                }
                index = end;
            }
            _ => index += 1,
        }
    }

    let lines: Vec<&str> = text.lines().collect();
    for safety_line in safety_lines {
        if !precedes_unsafe(&lines, safety_line) {
            findings.push(Finding {
                file: file.to_path_buf(),
                line: safety_line,
                rule: Rule::SafetyOnSafeCode,
            });
        }
    }

    findings.sort_by_key(|finding| finding.line);
    findings
}

/// Checks one Markdown file.
#[must_use]
pub fn check_markdown(file: &Path, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (offset, content) in text.lines().enumerate() {
        let line = offset + 1;
        if content.chars().any(is_decoration) {
            findings.push(Finding {
                file: file.to_path_buf(),
                line,
                rule: Rule::Decoration,
            });
        }
        if is_banner(content) {
            findings.push(Finding {
                file: file.to_path_buf(),
                line,
                rule: Rule::Banner,
            });
        }
    }
    findings
}

fn is_banner(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.chars().count() < 4 {
        return false;
    }
    let Some(first) = trimmed.chars().next() else {
        return false;
    };
    BANNER_CHARACTERS.contains(&first) && trimmed.chars().all(|character| character == first)
}

enum LineComment {
    Doc,
    Safety,
    Plain,
}

fn line_comment_kind(bytes: &[char], index: usize) -> LineComment {
    let third = bytes.get(index + 2).copied();
    let fourth = bytes.get(index + 3).copied();
    if third == Some('!') || (third == Some('/') && fourth != Some('/')) {
        return LineComment::Doc;
    }
    let end = end_of_line(bytes, index);
    let body: String = bytes[index + 2..end].iter().collect();
    if body
        .strip_prefix(' ')
        .is_some_and(|rest| rest.starts_with("SAFETY: "))
    {
        LineComment::Safety
    } else {
        LineComment::Plain
    }
}

fn precedes_unsafe(lines: &[&str], safety_line: usize) -> bool {
    lines
        .iter()
        .skip(safety_line)
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.contains("unsafe"))
}

fn end_of_line(bytes: &[char], from: usize) -> usize {
    let mut index = from;
    while index < bytes.len() && bytes[index] != '\n' {
        index += 1;
    }
    index
}

fn skip_string(bytes: &[char], from: usize, line: &mut usize) -> usize {
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            '\\' => index += 2,
            '"' => return index + 1,
            '\n' => {
                *line += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    index
}

fn skip_character(bytes: &[char], from: usize, line: &mut usize) -> usize {
    let mut index = from + 1;
    if index < bytes.len() && bytes[index] == '\\' {
        index += 2;
    } else if index < bytes.len() {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == '\'' {
        return index + 1;
    }
    if bytes.get(from) == Some(&'\n') {
        *line += 1;
    }
    from + 1
}

fn starts_raw_string(bytes: &[char], index: usize) -> bool {
    let mut cursor = index + 1;
    while bytes.get(cursor) == Some(&'#') {
        cursor += 1;
    }
    bytes.get(cursor) == Some(&'"')
}

fn skip_raw_string(bytes: &[char], from: usize, line: &mut usize) -> usize {
    let mut hashes = 0;
    let mut cursor = from + 1;
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
            let mut closing = 0;
            while bytes.get(cursor + 1 + closing) == Some(&'#') && closing < hashes {
                closing += 1;
            }
            if closing == hashes {
                return cursor + 1 + hashes;
            }
        }
        cursor += 1;
    }
    cursor
}

fn skip_block_comment(bytes: &[char], from: usize, line: &mut usize) -> usize {
    let mut depth = 1;
    let mut index = from;
    while index < bytes.len() && depth > 0 {
        if bytes[index] == '\n' {
            *line += 1;
        }
        if bytes[index] == '/' && bytes.get(index + 1) == Some(&'*') {
            depth += 1;
            index += 2;
        } else if bytes[index] == '*' && bytes.get(index + 1) == Some(&'/') {
            depth -= 1;
            index += 2;
        } else {
            index += 1;
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::{Rule, check_markdown, check_rust};
    use std::path::Path;

    fn rules(text: &str) -> Vec<Rule> {
        check_rust(Path::new("probe.rs"), text)
            .into_iter()
            .map(|finding| finding.rule)
            .collect()
    }

    #[test]
    fn a_docstring_is_permitted() {
        assert_eq!(rules("/// Reads a thing.\npub fn read() {}\n"), Vec::new());
    }

    #[test]
    fn a_module_docstring_is_permitted() {
        assert_eq!(rules("//! A module.\n"), Vec::new());
    }

    #[test]
    fn a_plain_comment_is_rejected() {
        assert_eq!(rules("let x = 1; // set x\n"), vec![Rule::Comment]);
    }

    #[test]
    fn four_slashes_are_not_a_docstring() {
        assert_eq!(rules("//// divider\n"), vec![Rule::Comment]);
    }

    #[test]
    fn a_block_comment_is_rejected() {
        assert_eq!(rules("/* note */\n"), vec![Rule::BlockComment]);
    }

    #[test]
    fn a_nested_block_comment_is_rejected_once() {
        assert_eq!(rules("/* a /* b */ c */\n"), vec![Rule::BlockComment]);
    }

    #[test]
    fn a_doc_comment_containing_a_block_opener_is_permitted() {
        assert_eq!(
            rules("/// mentions /* and nothing else\npub fn f() {}\n"),
            Vec::new()
        );
    }

    #[test]
    fn slashes_inside_a_string_are_not_a_comment() {
        assert_eq!(rules("let url = \"https://host/x\";\n"), Vec::new());
    }

    #[test]
    fn slashes_inside_a_raw_string_are_not_a_comment() {
        assert_eq!(rules("let text = r\"// not a comment\";\n"), Vec::new());
    }

    #[test]
    fn slashes_inside_a_hashed_raw_string_are_not_a_comment() {
        assert_eq!(rules("let text = r#\"// not a comment \"#;\n"), Vec::new());
    }

    #[test]
    fn a_block_opener_inside_a_raw_string_is_not_a_block_comment() {
        assert_eq!(
            rules("let text = r#\"/* not a comment */\"#;\n"),
            Vec::new()
        );
    }

    #[test]
    fn a_safety_line_before_an_unsafe_block_is_permitted() {
        assert_eq!(
            rules("// SAFETY: the handle is owned and open for the call.\nunsafe { call() }\n"),
            Vec::new()
        );
    }

    #[test]
    fn a_safety_line_before_safe_code_is_rejected() {
        assert_eq!(
            rules("// SAFETY: nothing here needs one.\nlet x = 1;\n"),
            vec![Rule::SafetyOnSafeCode]
        );
    }

    #[test]
    fn a_safety_line_with_no_space_is_rejected() {
        assert_eq!(
            rules("//SAFETY: shaped wrong\nunsafe { call() }\n"),
            vec![Rule::SafetyOnSafeCode]
        );
    }

    #[test]
    fn a_decorative_symbol_is_rejected() {
        assert_eq!(
            rules("/// a rocket \u{1F680}\npub fn f() {}\n"),
            vec![Rule::Decoration]
        );
    }

    #[test]
    fn an_arrow_is_rejected() {
        assert_eq!(
            rules("/// a to b \u{2192}\npub fn f() {}\n"),
            vec![Rule::Decoration]
        );
    }

    #[test]
    fn a_lifetime_is_not_a_character_literal() {
        assert_eq!(
            rules("fn f<'a>(x: &'a str) -> &'a str { x } // trailing\n"),
            vec![Rule::Comment]
        );
    }

    #[test]
    fn a_byte_literal_range_is_not_a_string() {
        assert_eq!(
            rules("match b { b'0'..=b'9' => 1, _ => 0 } // trailing\n"),
            vec![Rule::Comment]
        );
    }

    fn markdown_rules(text: &str) -> Vec<Rule> {
        check_markdown(Path::new("probe.md"), text)
            .into_iter()
            .map(|finding| finding.rule)
            .collect()
    }

    #[test]
    fn a_markdown_banner_is_rejected() {
        assert_eq!(markdown_rules("# Title\n\n=====\n"), vec![Rule::Banner]);
    }

    #[test]
    fn a_markdown_horizontal_rule_is_permitted() {
        assert_eq!(markdown_rules("# Title\n\n---\n"), Vec::new());
    }

    #[test]
    fn a_markdown_table_separator_is_permitted() {
        assert_eq!(markdown_rules("| a | b |\n|---|---|\n"), Vec::new());
    }

    #[test]
    fn a_markdown_heading_is_permitted() {
        assert_eq!(markdown_rules("#### Four levels deep\n"), Vec::new());
    }

    #[test]
    fn a_markdown_decorative_symbol_is_rejected() {
        assert_eq!(markdown_rules("done \u{2705}\n"), vec![Rule::Decoration]);
    }
}
