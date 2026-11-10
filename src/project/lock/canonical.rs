//! a fast reader for data.lock’s writer shape, optionally accepting comments around it.
//! other TOML returns none, so the general reader retains its validation and exact errors.

use std::borrow::Cow;

use super::lists::Listed;
use super::{Locked, LockedFile, Parsed, VERSION, prefixed};
use crate::digest::{Md5, Sha256};
use crate::name;

/// the datasets `text` gives, when it is in the exact form [`super::Lock::render`] writes, with
/// either line ending. `comments` additionally accepts blank lines and valid TOML comments around
/// it.
pub(super) fn read(text: &str, comments: bool) -> Option<Vec<Parsed>> {
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
        let dataset = dataset(&mut lines)?;
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

fn dataset(lines: &mut Lines) -> Option<Parsed> {
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
    let mut listed = None;
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
        line => listed = Some(list(line, lines)?),
    }
    let locked = Locked {
        name,
        reference,
        spec,
        resolved,
        title,
        license,
        doi,
        timestamp,
        files,
    };
    Some(Parsed::Source(locked, listed))
}

/// a list kept beside data.lock: the line `files_in = ["…", "…"]`, then the lines of
/// `file_count`, `file_bytes` and `files_sha256`.
fn list(line: &str, lines: &mut Lines) -> Option<Listed> {
    let mut rest = line.strip_prefix("files_in = [")?.strip_suffix(']')?;
    let mut parts = Vec::new();
    loop {
        let (part, after) = string(rest)?;
        parts.push(part.into_owned());
        if after.is_empty() {
            break;
        }
        rest = after.strip_prefix(", ")?;
    }
    Some(Listed {
        parts,
        count: lines.number("file_count")?,
        bytes: lines.number("file_bytes")?,
        sha256: prefixed(&lines.value("files_sha256")?)?,
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

/// whether TOML refuses `c` as it is inside a basic string: a control character below U+0020
/// other than tab, or U+007F.
fn refused(c: char) -> bool {
    (c < ' ' && c != '\t') || c == '\u{7f}'
}

#[cfg(test)]
mod tests {
    use super::super::Lock;
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
            assert_eq!(Lock::parse(&commented).unwrap(), lock);
            assert_eq!(Lock::parse(&commented.replace('\n', "\r\n")).unwrap(), lock);
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
            let Parsed::Source(dataset, _) = &read[0];
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
