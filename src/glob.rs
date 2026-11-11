//! glob patterns for `select` and `exclude`. a pattern matches the whole path inside a dataset:
//! `*` and `?` stay within one folder, `**` spans any number of folders, and `[...]` matches one
//! character from a set.

use std::collections::HashMap;

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

    /// the folder every path the pattern matches lies in, written out in full: `data/train` for
    /// `data/train/**` or `data/train/*.csv`. none when a match may lie anywhere, as for `*.csv`
    /// or `**/x.csv`.
    pub(crate) fn folder(&self) -> Option<String> {
        let mut names = Vec::new();
        // the last part names the files, so only the parts before it can name a folder
        for part in &self.parts[..self.parts.len().saturating_sub(1)] {
            let Part::Name(tokens) = part else {
                break;
            };
            let Some(name) = literal(tokens) else {
                break;
            };
            names.push(name);
        }
        (!names.is_empty()).then(|| names.join("/"))
    }

    /// the one path the pattern matches, when it has no wildcard: a plain path, or one
    /// [`escape`] wrote.
    pub(crate) fn literal(&self) -> Option<String> {
        let names: Option<Vec<String>> = self
            .parts
            .iter()
            .map(|part| match part {
                Part::Name(tokens) => literal(tokens),
                Part::Any => None,
            })
            .collect();
        names.map(|names| names.join("/"))
    }
}

/// patterns that match a path when any one of them does. a pattern without wildcards, as `drop`
/// writes one for each file it leaves out, is looked up by its path, and only the others are
/// tried in turn, so ten thousand of them cost one lookup per path.
#[derive(Clone, Debug, Default)]
pub(crate) struct Set {
    /// every pattern, as written.
    globs: Vec<Glob>,
    /// the path each pattern without wildcards matches, with the place of the first such pattern.
    literal: HashMap<String, usize>,
    /// the places of the patterns with wildcards, in order.
    wild: Vec<usize>,
}

impl Set {
    pub(crate) fn new(globs: Vec<Glob>) -> Set {
        let mut literal = HashMap::new();
        let mut wild = Vec::new();
        for (at, glob) in globs.iter().enumerate() {
            match glob.literal() {
                Some(path) => {
                    literal.entry(path).or_insert(at);
                }
                None => wild.push(at),
            }
        }
        Set {
            globs,
            literal,
            wild,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.globs.is_empty()
    }

    /// the patterns as written.
    pub(crate) fn globs(&self) -> &[Glob] {
        &self.globs
    }

    pub(crate) fn matches(&self, path: &str) -> bool {
        self.literal.contains_key(path) || self.wild.iter().any(|at| self.globs[*at].matches(path))
    }
}

/// the most folders listed one by one; past it one listing of everything costs fewer requests.
const MOST_FOLDERS: usize = 8;

/// the folders that hold every path `patterns` can match, none nested in another, so a source
/// can list those instead of everything. none when there are no patterns, when one can match
/// outside any folder, or when they name more folders than are worth listing one by one.
pub(crate) fn folders(patterns: &[Glob]) -> Option<Vec<String>> {
    if patterns.is_empty() {
        return None;
    }
    let mut folders: Vec<String> = patterns.iter().map(Glob::folder).collect::<Option<_>>()?;
    folders.sort();
    folders.dedup();
    let mut kept: Vec<String> = Vec::new();
    for folder in folders {
        // sorted, a folder comes after any folder holding it
        if !kept.iter().any(|outer| {
            folder
                .strip_prefix(outer.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        }) {
            kept.push(folder);
        }
    }
    (kept.len() <= MOST_FOLDERS).then_some(kept)
}

/// [`folders`] for patterns still as text, none when one is not a valid pattern, so the error
/// is told where patterns are checked.
pub(crate) fn folders_of(texts: &[String]) -> Option<Vec<String>> {
    let patterns: Vec<Glob> = texts
        .iter()
        .map(|text| Glob::new(text))
        .collect::<Result<_, _>>()
        .ok()?;
    folders(&patterns)
}

/// the one name `tokens` match, when they match only one: plain characters, and sets of one
/// character such as `[*]`, as [`escape`] writes them.
fn literal(tokens: &[Token]) -> Option<String> {
    tokens
        .iter()
        .map(|token| match token {
            Token::Char(c) => Some(*c),
            Token::Set {
                negated: false,
                ranges,
            } if ranges.len() == 1 && ranges[0].0 == ranges[0].1 => Some(ranges[0].0),
            _ => None,
        })
        .collect()
}

/// a pattern that matches exactly `path`, its `[`, `*` and `?` each made a set of itself, as
/// for the one file a pasted link names.
pub(crate) fn escape(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len());
    for character in path.chars() {
        match character {
            '[' => pattern.push_str("[[]"),
            '*' => pattern.push_str("[*]"),
            '?' => pattern.push_str("[?]"),
            other => pattern.push(other),
        }
    }
    pattern
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
    fn an_escaped_path_matches_only_itself() {
        for path in ["data[1].csv", "a*b?.csv", "plain/name.csv", "x]y.csv"] {
            assert!(matches(&escape(path), path), "{path}");
        }
        assert!(!matches(&escape("data[1].csv"), "data1.csv"));
        assert!(!matches(&escape("a*.csv"), "abc.csv"));
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

    #[test]
    fn a_pattern_names_the_folder_its_matches_lie_in() {
        let folder = |text: &str| Glob::new(text).unwrap().folder();
        assert_eq!(folder("data/train/**").as_deref(), Some("data/train"));
        assert_eq!(folder("/data/train/*.csv").as_deref(), Some("data/train"));
        assert_eq!(folder("data/*/x.csv").as_deref(), Some("data"));
        assert_eq!(folder("data/**/x.csv").as_deref(), Some("data"));
        // what a pasted link to a file in a folder selects
        assert_eq!(
            folder(&escape("odd [1]/a*b?.csv")).as_deref(),
            Some("odd [1]")
        );
        for anywhere in ["*.csv", "x.csv", "**/x.csv", "*/x.csv", "[ab]/x.csv"] {
            assert_eq!(folder(anywhere), None, "{anywhere}");
        }
    }

    /// a fixed-seed xorshift walk, so a failing case repeats.
    struct Walk(u64);

    impl Walk {
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            usize::try_from(self.0 % bound as u64).unwrap()
        }

        fn joined(&mut self, words: &[&str]) -> String {
            let count = 1 + self.below(3);
            (0..count)
                .map(|_| words[self.below(words.len())])
                .collect::<Vec<_>>()
                .join("/")
        }
    }

    #[test]
    fn patterns_agree_with_globs_on_random_paths() {
        let names = ["a", "b", "ab", "a*b", "x[1]", "x?", "é.csv", "a.csv"];
        let parts = [
            "a", "b", "*", "?", "**", "a*", "[ab]", "[!a]", "[*]", "x[[]1]", "*.csv", "é.csv",
        ];
        let mut walk = Walk(0x9e37_79b9_7f4a_7c15);
        for _ in 0..400 {
            let mut globs = Vec::new();
            for _ in 0..walk.below(6) {
                let text = match walk.below(3) {
                    0 => escape(&walk.joined(&names)),
                    1 => format!("/{}", walk.joined(&parts)),
                    _ => walk.joined(&parts),
                };
                globs.extend(Glob::new(&text).ok());
            }
            let patterns = Set::new(globs.clone());
            for _ in 0..40 {
                let path = walk.joined(&names);
                let expected = globs.iter().any(|glob| glob.matches(&path));
                assert_eq!(patterns.matches(&path), expected, "{path} by {globs:?}");
            }
        }
    }

    /// drop writes one escaped path per file it leaves out, so a set of ten thousand of them
    /// must cost a lookup per path, not ten thousand tries.
    #[test]
    fn literal_patterns_are_looked_up_rather_than_tried() {
        let mut globs: Vec<Glob> = (0..10_000)
            .map(|n| Glob::new(&escape(&format!("pets/images/{n}[1]*.png"))).unwrap())
            .collect();
        globs.push(Glob::new("**/*.tmp").unwrap());
        globs.push(Glob::new("/top/README").unwrap());
        let patterns = Set::new(globs);
        assert_eq!(patterns.wild.len(), 1, "only **/*.tmp is tried in turn");
        assert!(patterns.matches("pets/images/9999[1]*.png"));
        assert!(!patterns.matches("pets/images/99991.png"));
        assert!(patterns.matches("top/README"));
        assert!(patterns.matches("x/y.tmp"));
    }

    #[test]
    fn folders_cover_every_pattern_without_nesting() {
        let globs = |texts: &[&str]| {
            texts
                .iter()
                .map(|t| Glob::new(t).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            folders(&globs(&["b/**", "a/x/*.csv", "a/**", "a-b/**"])),
            Some(vec!["a".to_owned(), "a-b".to_owned(), "b".to_owned()])
        );
        assert_eq!(folders(&globs(&["train/**", "*.json"])), None);
        assert_eq!(folders(&[]), None);
        let many: Vec<String> = (0..=MOST_FOLDERS).map(|n| format!("f{n}/**")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(folders(&globs(&many)), None);
        assert_eq!(
            folders(&globs(&many[..MOST_FOLDERS])).map(|kept| kept.len()),
            Some(MOST_FOLDERS)
        );
    }
}
