//! a fast reader for data.lock exactly as fetchloom writes it. building a whole TOML document
//! first costs ten times the time and hundreds of megabytes for a lock of 100,000 files. anything
//! else, such as a lock edited by hand, gets none here, and the TOML reader takes over with its
//! exact errors.

use std::borrow::Cow;

use super::{Lock, Locked, LockedFile, VERSION};
use crate::digest::{Md5, Sha256};
use crate::name;

/// the lock `text` holds, when it is in the exact form [`Lock::render`] writes, with either line
/// ending.
pub(super) fn read(text: &str) -> Option<Lock> {
    let mut lines = Lines { rest: text };
    if lines.next()? != format!("version = {VERSION}") {
        return None;
    }
    let mut datasets: Vec<Locked> = Vec::new();
    while !lines.rest.is_empty() {
        if !lines.next()?.is_empty() || lines.next()? != "[[dataset]]" {
            return None;
        }
        let dataset = dataset(&mut lines)?;
        if datasets
            .last()
            .is_some_and(|last| last.name >= dataset.name)
        {
            return None;
        }
        datasets.push(dataset);
    }
    Some(Lock { datasets })
}

/// the lines of a text that ends every line with `\n` or `\r\n`.
struct Lines<'a> {
    rest: &'a str,
}

impl<'a> Lines<'a> {
    fn next(&mut self) -> Option<&'a str> {
        let (line, rest) = self.rest.split_once('\n')?;
        self.rest = rest;
        Some(line.strip_suffix('\r').unwrap_or(line))
    }

    /// the string of a line `key = "value"`, when the next line is one.
    fn value(&mut self, key: &str) -> Option<String> {
        let line = self.next()?;
        let (value, rest) = string(line.strip_prefix(key)?.strip_prefix(" = ")?)?;
        rest.is_empty().then(|| value.into_owned())
    }

    /// whether the next line sets `key`, leaving it unread.
    fn sets(&self, key: &str) -> bool {
        self.rest
            .strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(" = "))
    }
}

fn dataset(lines: &mut Lines) -> Option<Locked> {
    let name = lines.value("name")?;
    let reference = lines.value("ref")?;
    let spec = lines.value("spec")?;
    let resolved = lines.value("resolved")?;
    let mut optional = [None, None, None];
    for (key, value) in ["title", "license", "doi"].into_iter().zip(&mut optional) {
        if lines.sets(key) {
            *value = Some(lines.value(key)?);
        }
    }
    let [title, license, doi] = optional;
    let timestamp = lines.value("locked")?;
    let mut files: Vec<LockedFile> = Vec::new();
    match lines.next()? {
        "files = []" => {}
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
        _ => return None,
    }
    Some(Locked {
        name,
        reference,
        spec,
        resolved,
        title,
        license,
        doi,
        timestamp,
        files,
    })
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
    let mut value = String::with_capacity(body.len());
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

/// whether TOML refuses `c` as it is inside a basic string: a control character below U+0020
/// other than tab, or U+007F.
fn refused(c: char) -> bool {
    (c < ' ' && c != '\t') || c == '\u{7f}'
}

#[cfg(test)]
mod tests {
    use super::*;
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
            resolved: format!("zenodo:{name}@1"),
            title: extra.then(|| "Iris \"classic\"\tset\nline\r\u{1f}\u{7}".to_owned()),
            license: extra.then(|| "CC-BY-4.0".to_owned()),
            doi: extra.then(|| "10.5281/zenodo.3608990".to_owned()),
            timestamp: "2026-10-27T17:30:00Z".to_owned(),
            files: files.to_vec(),
        };
        vec![
            Lock::default(),
            Lock {
                datasets: vec![dataset("only", &files, true)],
            },
            Lock {
                datasets: vec![
                    dataset("a", &[], false),
                    dataset("b", &files[..3], true),
                    dataset("c", &files[3..], false),
                ],
            },
        ]
    }

    /// the lock as the TOML reader reads it: a comment at the end is something only a person
    /// writes, so the fast reader steps aside.
    fn full(text: &str) -> Lock {
        Lock::parse(&format!("{text}# a comment\n")).unwrap()
    }

    #[test]
    fn the_writers_form_reads_as_the_toml_reader_reads_it() {
        for lock in samples() {
            let text = lock.render();
            assert_eq!(read(&text), Some(full(&text)), "{text}");
            assert_eq!(read(&text), Some(lock.clone()));
            assert_eq!(read(&text.replace('\n', "\r\n")), Some(lock));
        }
        let text = samples()[1].render().replacen("\\t", "\t", 1);
        assert_eq!(read(&text), Some(full(&text)), "a tab as it is reads alike");
    }

    #[test]
    fn anything_unusual_is_left_to_the_toml_reader() {
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
            assert_eq!(read(&text), None, "{text}");
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

    /// any text: the fast reader takes only what the TOML reader reads alike, and any lock read
    /// is written and read back unchanged.
    #[test]
    fn fuzz_lock_reader() {
        bolero::check!().for_each(|bytes| {
            let Ok(text) = std::str::from_utf8(bytes) else {
                return;
            };
            if let Some(lock) = read(text) {
                assert_eq!(
                    Lock::parse(&format!("{text}# a comment\n")).ok().as_ref(),
                    Some(&lock),
                    "{text:?}"
                );
                assert_eq!(read(&lock.render()).as_ref(), Some(&lock), "{text:?}");
            }
            if let Ok(lock) = Lock::parse(text) {
                let written = lock.render();
                assert_eq!(Lock::parse(&written).ok(), Some(lock), "{written:?}");
            }
        });
    }
}
