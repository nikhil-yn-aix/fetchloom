//! a fast reader for data.lock’s writer shape, optionally accepting comments around it.
//! other TOML returns none, so the general reader retains its validation and exact errors.

use std::borrow::Cow;
use std::collections::BTreeMap;

use super::derived::{self, Code, Derived, Input};
use super::lists::Listed;
use super::{Locked, LockedFile, Parsed, VERSION, prefixed};
use crate::digest::{Md5, Sha256};
use crate::name;
use crate::source::Access;

/// [`read_only`] of every dataset with its files.
#[cfg(test)]
pub(super) fn read(text: &str, comments: bool) -> Option<Vec<Parsed>> {
    read_only(text, comments, &|_| true)
}

/// the datasets `text` gives in the exact form [`super::Lock::render`] writes, either line
/// ending; `comments` also allows blank lines and TOML comments. a source `keep` refuses is
/// read with no files, their lines passed over unread where they cannot hide a line of the lock.
pub(super) fn read_only(
    text: &str,
    comments: bool,
    keep: &dyn Fn(&str) -> bool,
) -> Option<Vec<Parsed>> {
    let mut lines = Lines {
        rest: text,
        comments,
    };
    if lines.next()? != format!("version = {VERSION}") {
        return None;
    }
    let mut datasets: Vec<Parsed> = Vec::new();
    loop {
        lines.skip_comments()?;
        if lines.rest.is_empty() {
            break;
        }
        if !comments && !lines.next()?.is_empty() {
            return None;
        }
        if lines.next()? != "[[dataset]]" {
            return None;
        }
        let dataset = dataset(&mut lines, keep)?;
        if datasets
            .last()
            .is_some_and(|last| last.name() >= dataset.name())
        {
            return None;
        }
        datasets.push(dataset);
    }
    Some(datasets)
}

/// the lines of a text that ends every line with `\n` or `\r\n`.
struct Lines<'a> {
    rest: &'a str,
    comments: bool,
}

impl<'a> Lines<'a> {
    fn line(&self) -> Option<(&'a str, &'a str)> {
        match self.rest.split_once('\n') {
            Some((line, rest)) => Some((line.strip_suffix('\r').unwrap_or(line), rest)),
            None => (self.comments && !self.rest.is_empty()).then_some((self.rest, "")),
        }
    }

    fn skip_comments(&mut self) -> Option<()> {
        if self.comments {
            while let Some((line, rest)) = self.line() {
                let content = line.trim_start_matches([' ', '\t']);
                if let Some(comment) = content.strip_prefix('#') {
                    if comment.contains(refused) {
                        return None;
                    }
                } else if !content.is_empty() {
                    break;
                }
                self.rest = rest;
            }
        }
        Some(())
    }

    fn next(&mut self) -> Option<&'a str> {
        self.skip_comments()?;
        let (line, rest) = self.line()?;
        self.rest = rest;
        if self.comments {
            uncommented(line)
        } else {
            Some(line)
        }
    }

    /// the string of a line `key = "value"`, when the next line is one.
    fn value(&mut self, key: &str) -> Option<String> {
        let line = self.next()?;
        let (value, rest) = string(line.strip_prefix(key)?.strip_prefix(" = ")?)?;
        rest.is_empty().then(|| value.into_owned())
    }

    /// the number of a line `key = 123`, when the next line is one.
    fn number(&mut self, key: &str) -> Option<u64> {
        let digits = self.next()?.strip_prefix(key)?.strip_prefix(" = ")?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        integer(digits)
    }

    /// passes over a list of files up to its closing `]` unread, when no line can hide that `]`
    /// in a literal or multi-line string; false, having passed nothing, otherwise.
    fn pass_files(&mut self) -> bool {
        if self.comments {
            return false;
        }
        let mut rest = self.rest;
        while let Some((line, after)) = rest.split_once('\n') {
            if line.strip_suffix('\r').unwrap_or(line) == "]" {
                let passed = &self.rest[..self.rest.len() - rest.len()];
                if passed.contains('\'') || passed.contains("\"\"\"") {
                    return false;
                }
                self.rest = after;
                return true;
            }
            rest = after;
        }
        false
    }

    /// whether the next line sets `key`, leaving it unread.
    fn sets(&mut self, key: &str) -> bool {
        if self.skip_comments().is_none() {
            return false;
        }
        self.rest
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(" = "))
    }
}

/// removes only comments outside basic strings; escaped quotes keep their string boundary.
fn uncommented(line: &str) -> Option<&str> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if quoted => escaped = true,
            b'"' => quoted = !quoted,
            b'#' if !quoted => {
                if line[index + 1..].contains(refused) {
                    return None;
                }
                return Some(line[..index].trim_end_matches([' ', '\t']));
            }
            _ => {}
        }
    }
    Some(line.trim_end_matches([' ', '\t']))
}

fn dataset(lines: &mut Lines, keep: &dyn Fn(&str) -> bool) -> Option<Parsed> {
    let name = lines.value("name")?;
    if lines.sets("from") {
        return recipe(name, lines);
    }
    let reference = lines.value("ref")?;
    let spec = lines.value("spec")?;
    // the writer records extract only when it is false
    let extract = !lines.sets("extract");
    if !extract && lines.next()? != "extract = false" {
        return None;
    }
    let resolved = lines.value("resolved")?;
    let mut optional = [None, None, None];
    for (key, value) in ["title", "license", "doi"].into_iter().zip(&mut optional) {
        if lines.sets(key) {
            *value = Some(lines.value(key)?);
        }
    }
    let [title, license, doi] = optional;
    let mut access = Access::Unknown;
    if lines.sets("access") {
        let word = lines.value("access")?;
        access = [Access::Public, Access::Login, Access::Terms]
            .into_iter()
            .find(|access| access.word() == word)?;
    }
    let timestamp = lines.value("locked")?;
    let mut files: Vec<LockedFile> = Vec::new();
    let mut listed = None;
    match lines.next()? {
        "files = []" => {}
        "files = [" if !keep(&name) && lines.pass_files() => {}
        "files = [" => loop {
            let line = lines.next()?;
            if line == "]" {
                break;
            }
            let file = file(line)?;
            if files.last().is_some_and(|last| last.path >= file.path)
                || name::escapes(&file.path).is_some()
            {
                return None;
            }
            files.push(file);
        },
        line => listed = Some(list(line, lines)?),
    }
    let locked = Locked {
        name,
        reference,
        spec,
        extract,
        resolved,
        title,
        license,
        doi,
        access,
        timestamp,
        files,
    };
    Some(Parsed::Source(locked, listed))
}

/// a list kept beside data.lock: the line `files_in = ["…", "…"]`, then the lines of
/// `file_count`, `file_bytes` and `files_sha256`.
fn list(line: &str, lines: &mut Lines) -> Option<Listed> {
    Some(Listed {
        parts: strings(line.strip_prefix("files_in = ")?)?,
        count: lines.number("file_count")?,
        bytes: lines.number("file_bytes")?,
        sha256: prefixed(&lines.value("files_sha256")?)?,
    })
}

/// a dataset a recipe made, after its name: `from` and `code` a line an entry, `run` on one line,
/// `env` when it has variables, then `key`, `locked`, `tree` and the list of its files.
fn recipe(name: String, lines: &mut Lines) -> Option<Parsed> {
    if lines.next()? != "from = [" {
        return None;
    }
    let mut from = Vec::new();
    loop {
        let line = lines.next()?;
        if line == "]" {
            break;
        }
        let (input, rest) = string(line.strip_prefix("  { name = ")?)?;
        let (content, rest) = string(rest.strip_prefix(", content = ")?)?;
        if rest != " }," {
            return None;
        }
        from.push(Input {
            name: input.into_owned(),
            content: prefixed(&content)?,
        });
    }
    let run = strings(lines.next()?.strip_prefix("run = ")?)?;
    let mut code = Vec::new();
    match lines.next()? {
        "code = []" => {}
        "code = [" => loop {
            let line = lines.next()?;
            if line == "]" {
                break;
            }
            let (path, rest) = string(line.strip_prefix("  { path = ")?)?;
            let (sha256, folder, rest) = if let Some(rest) = rest.strip_prefix(", tree = ") {
                let (tree, rest) = string(rest)?;
                (prefixed(&tree)?, true, rest)
            } else {
                let (sha256, rest) = string(rest.strip_prefix(", sha256 = ")?)?;
                (Sha256::from_hex(&sha256)?, false, rest)
            };
            if rest != " }," {
                return None;
            }
            code.push(Code {
                path: path.into_owned(),
                sha256,
                folder,
            });
        },
        _ => return None,
    }
    let mut env = BTreeMap::new();
    if lines.sets("env") {
        let mut rest = lines.next()?.strip_prefix("env = { ")?.strip_suffix(" }")?;
        loop {
            let (variable, after) = key(rest)?;
            let (value, after) = string(after.strip_prefix(" = ")?)?;
            if env
                .last_key_value()
                .is_some_and(|(last, _): (&String, _)| *last >= variable)
            {
                return None;
            }
            env.insert(variable, value.into_owned());
            if after.is_empty() {
                break;
            }
            rest = after.strip_prefix(", ")?;
        }
    }
    let key = prefixed(&lines.value("key")?)?;
    let timestamp = lines.value("locked")?;
    let tree = prefixed(&lines.value("tree")?)?;
    let listed = list(lines.next()?, lines)?;
    let derived = Derived {
        name,
        from,
        run,
        code,
        env,
        key,
        timestamp,
        tree,
        files: Vec::new(),
    };
    derived::check(&derived).ok()?;
    Some(Parsed::Derived(derived, listed))
}

/// a one-line array of basic strings, `["…", "…"]`, holding at least one.
fn strings(text: &str) -> Option<Vec<String>> {
    let mut rest = text.strip_prefix('[')?.strip_suffix(']')?;
    let mut values = Vec::new();
    loop {
        let (value, after) = string(rest)?;
        values.push(value.into_owned());
        if after.is_empty() {
            return Some(values);
        }
        rest = after.strip_prefix(", ")?;
    }
}

/// the key at the start of `text`, bare or a basic string, and the text after it.
fn key(text: &str) -> Option<(String, &str)> {
    if text.starts_with('"') {
        let (key, rest) = string(text)?;
        return Some((key.into_owned(), rest));
    }
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(text.len());
    (end > 0).then(|| (text[..end].to_owned(), &text[end..]))
}

/// one line of a `files` array: `  { path = "…", size = 1, sha256 = "…", url = "…" },`, with
/// `md5` before `url` and `unpacks_to` after it when they are known.
fn file(line: &str) -> Option<LockedFile> {
    let (path, rest) = string(line.strip_prefix("  { path = ")?)?;
    let rest = rest.strip_prefix(", size = ")?;
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let size = integer(&rest[..digits])?;
    let (sha256, rest) = string(rest[digits..].strip_prefix(", sha256 = ")?)?;
    let sha256 = Sha256::from_hex(&sha256)?;
    let (md5, rest) = match rest.strip_prefix(", md5 = ") {
        Some(rest) => {
            let (md5, rest) = string(rest)?;
            (Some(Md5::from_hex(&md5)?), rest)
        }
        None => (None, rest),
    };
    let (url, rest) = string(rest.strip_prefix(", url = ")?)?;
    let (unpacks_to, rest) = match rest.strip_prefix(", unpacks_to = ") {
        Some(rest) => {
            let (tree, rest) = string(rest)?;
            (Some(Sha256::from_hex(tree.strip_prefix("sha256:")?)?), rest)
        }
        None => (None, rest),
    };
    (rest == " },").then(|| LockedFile {
        path: path.into_owned(),
        size,
        sha256,
        md5,
        url: url.into_owned(),
        unpacks_to,
    })
}

/// a size as the writer prints it: digits without a leading zero, no larger than TOML allows.
fn integer(digits: &str) -> Option<u64> {
    if digits.is_empty() || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    i64::try_from(value).is_ok().then_some(value)
}

/// the TOML basic string at the start of `text` and the text after it. only the escapes the
/// writer uses are read, and a control character TOML refuses gives none.
fn string(text: &str) -> Option<(Cow<'_, str>, &str)> {
    let body = text.strip_prefix('"')?;
    let end = body.find(['"', '\\'])?;
    if body.as_bytes()[end] == b'"' {
        let value = &body[..end];
        return (!value.contains(refused)).then_some((Cow::Borrowed(value), &body[end + 1..]));
    }
    let mut value = String::with_capacity(end);
    let mut chars = body.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Some((Cow::Owned(value), &body[at + 1..])),
            '\\' => value.push(match chars.next()?.1 {
                '"' => '"',
                '\\' => '\\',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'u' => {
                    let hex = body.get(at + 2..at + 6)?;
                    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return None;
                    }
                    chars.nth(3)?;
                    char::from_u32(u32::from_str_radix(hex, 16).ok()?)?
                }
                _ => return None,
            }),
            c if refused(c) => return None,
            c => value.push(c),
        }
    }
    None
}

/// whether TOML refuses `c` unescaped inside a basic string.
fn refused(c: char) -> bool {
    (c < ' ' && c != '\t') || c == '\u{7f}'
}

#[cfg(test)]
mod tests {
    use super::super::Lock;
    use super::*;
    use crate::cache::Member;
    use crate::digest;

    fn file(path: &str, index: usize) -> LockedFile {
        LockedFile {
            path: path.to_owned(),
            size: index as u64 * 1_000_003,
            sha256: digest::sha256(path.as_bytes()),
            md5: index
                .is_multiple_of(2)
                .then(|| Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap()),
            url: format!("https://example.org/{index}?q=\"a\\b\"\t\u{7}\u{7f}\u{85}"),
            unpacks_to: index.is_multiple_of(3).then(|| digest::sha256(b"tree")),
        }
    }

    /// locks with every optional key, escapes, unicode and an empty file list.
    fn samples() -> Vec<Lock> {
        let paths = [
            "a.csv",
            "b/c d.csv",
            "e/\u{e9}t\u{e9}.csv",
            "f/\u{540d}\u{524d}.csv",
            "g/quote\"mark.csv",
        ];
        let files: Vec<LockedFile> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| file(path, index))
            .collect();
        let dataset = |name: &str, files: &[LockedFile], extra: bool| Locked {
            name: name.to_owned(),
            reference: format!("zenodo:{name}"),
            spec: format!("sha256:{}", digest::sha256(name.as_bytes())),
            extract: !extra,
            resolved: format!("zenodo:{name}@1"),
            title: extra.then(|| "Iris \"classic\"\tset\nline\r\u{1f}\u{7}".to_owned()),
            license: extra.then(|| "CC-BY-4.0".to_owned()),
            doi: extra.then(|| "10.5281/zenodo.3608990".to_owned()),
            access: if extra {
                Access::Terms
            } else {
                Access::Unknown
            },
            timestamp: "2026-10-27T17:30:00Z".to_owned(),
            files: files.to_vec(),
        };
        vec![
            Lock::default(),
            Lock {
                datasets: vec![dataset("only", &files, true)],
                ..Lock::default()
            },
            Lock {
                datasets: vec![
                    dataset("a", &[], false),
                    dataset("b", &files[..3], true),
                    dataset("c", &files[3..], false),
                ],
                derived: vec![recipe("b-small", 2), recipe("d", 0)],
            },
        ]
    }

    /// a result of `count` files, escapes and unicode in its command, code and variables.
    fn recipe(name: &str, count: usize) -> Derived {
        let files: Vec<Member> = (0..count)
            .map(|index| Member {
                path: format!("out/\u{e9}t\u{e9} {index}.csv"),
                size: index as u64,
                sha256: digest::sha256(&[u8::try_from(index).unwrap()]),
            })
            .collect();
        Derived {
            name: name.to_owned(),
            from: vec![
                Input {
                    name: "b".to_owned(),
                    content: digest::sha256(b"b"),
                },
                Input {
                    name: "a".to_owned(),
                    content: digest::sha256(b"a"),
                },
            ],
            run: vec![
                "python3".to_owned(),
                "say \"hi\"\t{b}".to_owned(),
                "{out}".to_owned(),
            ],
            code: if count == 0 {
                Vec::new()
            } else {
                vec![
                    Code {
                        path: "clean.py".to_owned(),
                        sha256: digest::sha256(b"clean"),
                        folder: false,
                    },
                    Code {
                        path: "lib/\u{540d}".to_owned(),
                        sha256: digest::sha256(b"lib"),
                        folder: true,
                    },
                ]
            },
            env: [("SEED", "1"), ("two words", "a \"b\""), ("\u{e9}", "")]
                .into_iter()
                .take(count + 1)
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            key: digest::sha256(name.as_bytes()),
            timestamp: "2026-11-10T00:00:00Z".to_owned(),
            tree: crate::cache::tree_hash(&files),
            files,
        }
    }

    fn full(text: &str) -> Vec<Parsed> {
        super::super::parse_toml(text).unwrap()
    }

    #[test]
    fn the_writers_form_reads_as_the_toml_reader_reads_it() {
        for lock in samples() {
            let text = lock.render();
            assert_eq!(read(&text, false), Some(full(&text)), "{text}");
            assert_eq!(read(&text, false), Some(lock.parsed()));
            assert_eq!(
                read(&text.replace('\n', "\r\n"), false),
                Some(lock.parsed())
            );
        }
        let text = samples()[1].render().replacen("\\t", "\t", 1);
        assert_eq!(
            read(&text, false),
            Some(full(&text)),
            "a tab as it is reads alike"
        );
    }

    #[test]
    fn a_dataset_listed_beside_the_lock_reads_as_the_toml_reader_reads_it() {
        let text = samples()[2].render_with(2);
        assert!(text.contains("\nfiles_in = [\""), "{text}");
        let fast = read(&text, false).unwrap();
        assert_eq!(fast, full(&text));
        assert!(matches!(&fast[1], Parsed::Source(_, Some(listed)) if listed.count == 3));
        for broken in [
            text.replacen("file_count = 3", "file_count = +3", 1),
            text.replacen("files_in = [\"", "files_in = [ \"", 1),
            text.replacen("files_sha256 = \"sha256:", "files_sha256 = \"", 1),
        ] {
            assert_eq!(read(&broken, false), None, "{broken}");
        }
    }

    #[test]
    fn the_narrow_reader_defers_noncanonical_forms() {
        let text = samples()[2].render();
        let unusual = [
            text.replacen("version = 1", "version = 2", 1),
            text.replacen("version = 1", "version=1", 1),
            text.replacen(", size = ", ", size = 0", 1),
            text.replacen(", size = ", ", size = +", 1),
            text.replacen("name = \"a\"", "name = \"z\"", 1),
            text.replacen(" },\n", "},\n", 1),
            text.replacen("name = \"b\"", "name = \"b\" # mine", 1),
            text.replacen("[[dataset]]", "[[dataset]] ", 1),
            text.replacen("\\\"", "\\x22", 1),
            text.replacen("\\u0007", "\u{7}", 1),
            text.replacen("b/c d.csv", "../c d.csv", 1),
            text.replacen("sha256 = \"", "sha256 = \"x", 1),
            text.trim_end().to_owned(),
            format!("{text}\n"),
        ];
        for text in unusual {
            assert_eq!(read(&text, false), None, "{text}");
        }
    }

    #[test]
    fn commented_writer_output_uses_the_fast_reader() {
        for mut lock in samples() {
            if let Some(dataset) = lock.datasets.first_mut() {
                dataset.title = Some("literal # and an escaped \" quote".to_owned());
            }
            let text = lock.render();
            let mut commented = String::from("# leading\n\n");
            for line in text.lines() {
                commented.push_str(line);
                commented.push_str(" # inline\n# between fields\n");
            }
            commented.push_str("# trailing");
            assert_eq!(read(&commented, false), None);
            assert_eq!(read(&commented, true), Some(lock.parsed()));
            assert_eq!(full(&commented), lock.parsed());
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join(super::super::lists::FOLDER)).unwrap();
            for (part, text) in lock.lists() {
                let bytes = super::super::lists::compress(&text).unwrap();
                std::fs::write(root.path().join(part), bytes).unwrap();
            }
            let project = |text: &str| Lock::read(text, root.path()).unwrap();
            assert_eq!(project(&commented), lock);
            assert_eq!(project(&commented.replace('\n', "\r\n")), lock);
            for invalid in [
                format!("{commented}\0"),
                format!("{commented}\n#bad\u{7f}"),
                format!("{commented}\r"),
                format!("{commented}\n[[broken]]"),
            ] {
                assert!(read(&invalid, true).is_none());
                assert!(Lock::parse(&invalid).is_err());
            }
        }
    }

    #[test]
    fn escaped_and_unicode_strings_read_back_exactly() {
        assert_eq!(
            string(r#""a\"b\\c\nd\te\rf\u00e9\u0007g" rest"#),
            Some((
                Cow::Owned("a\"b\\c\nd\te\rf\u{e9}\u{7}g".to_owned()),
                " rest"
            ))
        );
        assert_eq!(
            string("\"plain \u{540d}\"!"),
            Some((Cow::Borrowed("plain \u{540d}"), "!"))
        );
        for refused in [
            r#""\uD800""#,
            r#""\u12""#,
            r#""\u+041""#,
            r#""\q""#,
            "\"open",
            "\"a\u{1}\"",
        ] {
            assert_eq!(string(refused), None, "{refused}");
        }
    }

    #[test]
    fn escaped_path_capacity_is_independent_of_following_fields() {
        let path = format!("a\"{}.bin", "p".repeat(34));
        let mut capacities = Vec::new();
        for url_length in [96, 4096] {
            let mut sample = samples().remove(1);
            let mut entry = file(&path, 1);
            let prefix = "https://example.org/";
            entry.url = format!("{prefix}{}", "u".repeat(url_length - prefix.len()));
            entry.size = 4096;
            sample.datasets[0].files = vec![entry];
            let read = read(&sample.render(), false).unwrap();
            let Parsed::Source(dataset, _) = &read[0] else {
                panic!("a source comes first");
            };
            let got = &dataset.files[0].path;
            assert_eq!(got, &path);
            capacities.push((url_length, got.len(), got.capacity()));
        }
        assert!(
            capacities
                .iter()
                .all(|(_, len, capacity)| *capacity <= 2 * len),
            "URL length, path length, path capacity: {capacities:?}"
        );
    }

    /// any text: the fast reader takes only what the TOML reader reads alike, and any lock read
    /// is written and read back unchanged.
    #[test]
    fn fuzz_lock_reader() {
        bolero::check!().for_each(|bytes| {
            let Ok(text) = std::str::from_utf8(bytes) else {
                return;
            };
            for comments in [false, true] {
                if let Some(parsed) = read(text, comments) {
                    assert_eq!(
                        super::super::parse_toml(text).ok().as_ref(),
                        Some(&parsed),
                        "{text:?}"
                    );
                    if let Ok(lock) = super::super::resolve(parsed, None) {
                        assert_eq!(read(&lock.render(), false), Some(lock.parsed()), "{text:?}");
                    }
                }
            }
            if let Ok(lock) = Lock::parse(text) {
                let written = lock.render();
                assert_eq!(Lock::parse(&written).ok(), Some(lock), "{written:?}");
            }
        });
    }
}
