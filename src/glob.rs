//! glob patterns for `select` and `exclude`. a pattern matches the whole path inside a dataset:
//! `*` and `?` stay within one folder, `**` spans any number of folders, and `[...]` matches one
//! character from a set.

/// a checked pattern, split into its folder parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Glob {
    text: String,
    parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// `**`: any number of folders, none included.
    Any,
    Name(Vec<Token>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Char(char),
    /// `?`
    One,
    /// `*`
    Many,
    Set {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Glob {
    pub(crate) fn new(text: &str) -> Result<Glob, String> {
        if text.is_empty() {
            return Err("a pattern cannot be empty".to_owned());
        }
        if text.contains('\\') {
            return Err(format!(
                "\"{text}\" has a \\. patterns use / between folders"
            ));
        }
        if let Some(c) = text.chars().find(|c| c.is_control()) {
            return Err(format!("\"{text}\" holds the control character {c:?}"));
        }
        let trimmed = text.trim_start_matches('/');
        if trimmed.split('/').any(str::is_empty) {
            return Err(format!(
                "\"{text}\" has an empty folder name. for everything in a folder, write folder/**"
            ));
        }
        let parts = trimmed
            .split('/')
            .map(|part| {
                if part == "**" {
                    Ok(Part::Any)
                } else {
                    tokens(part).map(Part::Name)
                }
            })
            .collect::<Result<_, _>>()
            .map_err(|reason| format!("\"{text}\" {reason}"))?;
        Ok(Glob {
            text: text.to_owned(),
            parts,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// whether the pattern matches the whole of `path`, a `/`-separated path inside a dataset.
    pub(crate) fn matches(&self, path: &str) -> bool {
        let names: Vec<&str> = path.split('/').collect();
        // reachable[j]: the parts matched so far can end just before name j
        let mut reachable = vec![false; names.len() + 1];
        reachable[0] = true;
        for part in &self.parts {
            let mut next = vec![false; names.len() + 1];
            match part {
                Part::Any => {
                    let mut open = false;
                    for (j, slot) in next.iter_mut().enumerate() {
                        open |= reachable[j];
                        *slot = open;
                    }
                }
                Part::Name(tokens) => {
                    for (j, name) in names.iter().enumerate() {
                        if reachable[j] && matches_name(tokens, name) {
                            next[j + 1] = true;
                        }
                    }
                }
            }
            reachable = next;
        }
        reachable[names.len()]
    }

    /// whether the pattern names files only by their name, with no folder in it, which matches
    /// only files at the top of a dataset.
    pub(crate) fn is_top_level_only(&self) -> bool {
        self.parts.len() == 1 && matches!(self.parts[0], Part::Name(_))
    }
}

fn tokens(part: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = part.chars().peekable();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '?' => Token::One,
            '*' => {
                if chars.peek() == Some(&'*') {
                    return Err(
                        "has ** inside a name. use ** alone between slashes, as in data/**/x.csv"
                            .to_owned(),
                    );
                }
                Token::Many
            }
            '[' => set(&mut chars)?,
            c => Token::Char(c),
        });
    }
    Ok(tokens)
}

fn set(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<Token, String> {
    let negated = chars.next_if(|c| *c == '!' || *c == '^').is_some();
    let mut ranges = Vec::new();
    let mut first = true;
    loop {
        let c = chars.next().ok_or("has a [ that is never closed")?;
        if c == ']' && !first {
            break;
        }
        first = false;
        if chars.peek() == Some(&'-') {
            chars.next();
            match chars.next() {
                Some(']') => {
                    ranges.push((c, c));
                    ranges.push(('-', '-'));
                    break;
                }
                Some(end) if end >= c => ranges.push((c, end)),
                Some(end) => return Err(format!("has the range {c}-{end}, which runs backwards")),
                None => return Err("has a [ that is never closed".to_owned()),
            }
        } else {
            ranges.push((c, c));
        }
    }
    Ok(Token::Set { negated, ranges })
}

/// whether one folder or file name matches its tokens, with `*` backtracking to the last star.
fn matches_name(tokens: &[Token], name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    let (mut t, mut c) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while c < chars.len() {
        match tokens.get(t) {
            Some(Token::Many) => {
                star = Some((t, c));
                t += 1;
            }
            Some(token) if fits(token, chars[c]) => {
                t += 1;
                c += 1;
            }
            _ => match star {
                Some((star_t, star_c)) => {
                    t = star_t + 1;
                    c = star_c + 1;
                    star = Some((star_t, star_c + 1));
                }
                None => return false,
            },
        }
    }
    tokens[t..].iter().all(|token| *token == Token::Many)
}

fn fits(token: &Token, c: char) -> bool {
    match token {
        Token::Char(expected) => *expected == c,
        Token::One => true,
        Token::Many => false,
        Token::Set { negated, ranges } => {
            ranges.iter().any(|(low, high)| (*low..=*high).contains(&c)) != *negated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        Glob::new(pattern).unwrap().matches(path)
    }

    #[test]
    fn stars_stay_inside_one_folder() {
        assert!(matches("*.csv", "iris.csv"));
        assert!(!matches("*.csv", "raw/iris.csv"));
        assert!(matches("raw/*.csv", "raw/iris.csv"));
        assert!(matches("*", "anything"));
        assert!(!matches("*", "a/b"));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(!matches("a*b*c", "aXXbYY"));
    }

    #[test]
    fn double_stars_span_folders() {
        assert!(matches("**/*.edf", "sub-01/eeg/sub-01_task-mi_eeg.edf"));
        assert!(
            matches("**/*.edf", "top.edf"),
            "** may match no folder at all"
        );
        assert!(matches("**", "a/b/c"));
        assert!(matches("data/**", "data/x/y.csv"));
        assert!(matches("a/**/z", "a/z"));
        assert!(matches("a/**/z", "a/b/c/z"));
        assert!(!matches("a/**/z", "b/z"));
        assert!(!matches("**/derivatives/**", "sub-01/eeg/x.edf"));
        assert!(matches("**/derivatives/**", "derivatives/x.edf"));
    }

    #[test]
    fn question_marks_and_sets() {
        assert!(matches("sub-0?/x", "sub-07/x"));
        assert!(!matches("sub-0?/x", "sub-7/x"));
        assert!(matches("[abc].txt", "b.txt"));
        assert!(!matches("[!abc].txt", "b.txt"));
        assert!(matches("[a-c]*.txt", "cat.txt"));
        assert!(matches("[]].txt", "].txt"));
        assert!(matches("x[a-].txt", "x-.txt"));
    }

    #[test]
    fn bad_patterns_say_why() {
        for bad in ["", "a\\b", "[abc", "a**b", "[z-a]", "data/", "a//b"] {
            assert!(Glob::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_name_without_folders_only_matches_the_top() {
        assert!(Glob::new("*.edf").unwrap().is_top_level_only());
        assert!(!Glob::new("**/*.edf").unwrap().is_top_level_only());
    }
}
