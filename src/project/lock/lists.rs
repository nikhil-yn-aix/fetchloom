//! the file lists in `data.lock.d`, of sources past [`INLINE`] files, compressed and in parts so data.lock stays reviewable and git takes each part under its size
//! limits. data.lock records each list's count, bytes and SHA-256 to catch a changed part.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use super::LockedFile;
use crate::digest::{Hasher, Md5, Sha256};
use crate::error::{Error, Kind};

/// the folder beside data.lock that holds the lists.
pub(crate) const FOLDER: &str = "data.lock.d";
/// the most files a source lists in data.lock itself. a rule of the format, not a setting, so
/// every machine writes the same lock for the same files.
pub(crate) const INLINE: usize = 50_000;
/// [`INLINE`], or fewer when a debug build's CLI test asks.
pub(crate) fn inline() -> usize {
    #[cfg(debug_assertions)]
    if let Some(inline) = std::env::var("FETCHLOOM_TEST_INLINE")
        .ok()
        .and_then(|text| text.parse().ok())
    {
        return inline;
    }
    INLINE
}

/// the most files one part holds, which keeps a part to a few tens of megabytes. tests split
/// sooner, so lists of several parts stay small.
const PART: usize = if cfg!(test) { 1_000 } else { 250_000 };
/// how hard parts are compressed. a set level keeps the same bytes for the same list.
const LEVEL: i32 = 9;
/// the most memory decompressing a part may take: a window of 2^27 bytes.
const WINDOW_LOG: u32 = 27;
/// the longest line a part may hold, so one damaged line cannot fill memory.
const LONGEST: u64 = 1 << 20;
/// the longest path a row may hold, in bytes, as an archive's member or an imported file may.
pub(crate) const LONGEST_PATH: usize = 4096;
/// the longest link a row may hold, in bytes, far past what servers take in a request. with
/// [`LONGEST_PATH`] it keeps a cloned part of long rows, a few bytes each once compressed, from
/// asking for gigabytes.
pub(crate) const LONGEST_URL: usize = 64 << 10;
/// what to do about a list whose columns this fetchloom does not know.
const NEWER: &str = "install a newer fetchloom to read it; if someone edited data.lock.d by hand, restore it from git";
/// how to recover a list fetchloom cannot use.
pub(super) fn recovery() -> String {
    format!(
        "restore data.lock and data.lock.d from git, or delete data.lock and run `{}`, which resolves every dataset again and may pin newer data",
        crate::suggested::command("lock")
    )
}

/// a dataset's list as data.lock names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    /// each part's path below the project, with `/` between folders.
    pub(crate) parts: Vec<String>,
    pub(crate) count: u64,
    pub(crate) bytes: u64,
    /// the SHA-256 of the parts' text, one part after another.
    pub(crate) sha256: Sha256,
}

/// the first line of every part, naming its columns: path, size, SHA-256, MD5, link and the tree
/// an archive unpacks to.
const HEADER: &str = "path\tsize\tsha256\tmd5\turl\tunpacks_to\n";

/// a file a source pins as a line of a list.
impl LockedFile {
    /// the bytes of text the row holds, its path and link, which its memory grows with.
    fn text(&self) -> u64 {
        (self.path.len() + self.url.len()) as u64
    }

    /// what of the row is longer than a list may hold, [`LONGEST_PATH`] or [`LONGEST_URL`].
    fn too_long(&self) -> Option<&'static str> {
        if self.path.len() > LONGEST_PATH {
            Some("its path")
        } else {
            (self.url.len() > LONGEST_URL).then_some("its link")
        }
    }

    /// appends the row's line, its fields between tabs, without the line break.
    fn write_row(&self, text: &mut String) {
        escape(&self.path, text);
        text.push('\t');
        text.push_str(&self.size.to_string());
        text.push('\t');
        text.push_str(&self.sha256.hex());
        text.push('\t');
        if let Some(md5) = self.md5 {
            text.push_str(&md5.hex());
        }
        text.push('\t');
        escape(&self.url, text);
        text.push('\t');
        if let Some(tree) = self.unpacks_to {
            text.push_str(&tree.hex());
        }
    }

    /// the row a line holds, without its line break.
    fn parse_row(line: &str) -> Option<LockedFile> {
        let mut fields = line.split('\t');
        let path = unescape(fields.next()?)?;
        let size = size(fields.next()?)?;
        let sha256 = Sha256::from_hex(fields.next()?)?;
        let md5 = match fields.next()? {
            "" => None,
            hex => Some(Md5::from_hex(hex)?),
        };
        let url = unescape(fields.next()?)?;
        let unpacks_to = match fields.next()? {
            "" => None,
            hex => Some(Sha256::from_hex(hex)?),
        };
        let file = LockedFile {
            path,
            size,
            sha256,
            md5,
            url,
            unpacks_to,
        };
        (fields.next().is_none() && file.too_long().is_none()).then_some(file)
    }
}

/// a size as a part writes it: decimal digits only.
fn size(digits: &str) -> Option<u64> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// refuses `rows`, the files of `name`'s list, when one is longer than [`read`] takes, so
/// fetchloom never writes a list it would refuse.
pub(crate) fn listable(name: &str, rows: &[LockedFile]) -> Result<(), Error> {
    let Some((row, what)) = rows
        .iter()
        .find_map(|row| row.too_long().map(|what| (row, what)))
    else {
        return Ok(());
    };
    let longest = if what == "its path" {
        LONGEST_PATH
    } else {
        LONGEST_URL
    };
    Err(Error::new(
        Kind::ProjectInvalid,
        format!(
            "{name}/{} cannot be pinned: {what} is longer than {}, the most data.lock.d keeps",
            crate::name::abridged(&row.path),
            crate::units::count(longest as u64, "byte", "bytes")
        ),
    ))
}

/// the texts of the parts that list `rows`, in order, each beginning with the header. a list of
/// no rows is one part holding the header alone.
pub(crate) fn texts(rows: &[LockedFile]) -> Vec<String> {
    if rows.is_empty() {
        return vec![HEADER.to_owned()];
    }
    rows.chunks(PART)
        .map(|chunk| {
            let mut text = String::from(HEADER);
            for row in chunk {
                row.write_row(&mut text);
                text.push('\n');
            }
            text
        })
        .collect()
}

/// how data.lock names the list of the dataset `name`, which holds `rows` in parts of `texts`.
pub(crate) fn listed(name: &str, rows: &[LockedFile], texts: &[String]) -> Listed {
    let mut hasher = Hasher::default();
    for text in texts {
        hasher.update(text.as_bytes());
    }
    let sha256 = hasher.finish().0;
    Listed {
        parts: names(name, &sha256, texts.len()),
        count: rows.len() as u64,
        bytes: rows.iter().fold(0, |bytes, row| add(bytes, row.size)),
        sha256,
    }
}

/// `bytes` and `size` together, held to the largest integer TOML can write, so data.lock can
/// always read the sum it records.
fn add(bytes: u64, size: u64) -> u64 {
    bytes.saturating_add(size).min(i64::MAX.unsigned_abs())
}

/// the paths of the `count` parts of `name`'s list with `sha256`. the name changes with the list,
/// so writing a new list never touches the parts the lock on disk still names.
fn names(name: &str, sha256: &Sha256, count: usize) -> Vec<String> {
    let short = &sha256.hex()[..12];
    if count == 1 {
        return vec![format!("{FOLDER}/{name}.{short}.tsv.zst")];
    }
    (1..=count)
        .map(|part| format!("{FOLDER}/{name}.{short}.{part}.tsv.zst"))
        .collect()
}

/// whether `part`, a path below the project at `root` as data.lock names it, is not there at
/// all, as a link or a damaged part still is.
pub(crate) fn missing(root: &Path, part: &str) -> bool {
    let path = root.join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
    fs::symlink_metadata(path).is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
}

/// whether `listed` names its parts as fetchloom writes them for `name`, each a file of
/// [`FOLDER`]: the only paths a list is read from, since data.lock may come from someone else.
pub(crate) fn named_as_written(name: &str, listed: &Listed) -> bool {
    !listed.parts.is_empty() && listed.parts == names(name, &listed.sha256, listed.parts.len())
}

/// whether `file`, a name in [`FOLDER`], is one fetchloom writes for a list.
pub(crate) fn is_part(file: &str) -> bool {
    let Some(stem) = file.strip_suffix(".tsv.zst") else {
        return false;
    };
    let mut pieces = stem.rsplitn(3, '.');
    let (Some(last), Some(middle)) = (pieces.next(), pieces.next()) else {
        return false;
    };
    let short = |text: &str| text.len() == 12 && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    short(last) || (last.bytes().all(|byte| byte.is_ascii_digit()) && short(middle))
}

/// whether `file`, a name in [`FOLDER`], is a part an older run began to write there and never
/// finished, as a kill while writing one left it.
pub(crate) fn is_unfinished(file: &str) -> bool {
    file.strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(part, random)| {
            random.len() == 8
                && random.bytes().all(|byte| byte.is_ascii_hexdigit())
                && is_part(part)
        })
}

/// the bytes a part is kept in.
pub(crate) fn compress(text: &str) -> std::io::Result<Vec<u8>> {
    zstd::encode_all(text.as_bytes(), LEVEL)
}

/// whether the part at `path` holds `text` once decompressed. a link, a special file and a part
/// that does not decompress hold nothing.
pub(crate) fn holds(path: &Path, text: &str) -> bool {
    let Ok(file) = crate::volume::read_unfollowed(path) else {
        return false;
    };
    let Ok(mut decoder) = zstd::stream::read::Decoder::new(file) else {
        return false;
    };
    if decoder.window_log_max(WINDOW_LOG).is_err() {
        return false;
    }
    let mut read = Vec::with_capacity(text.len());
    // a byte past the text tells a longer part from it
    decoder
        .take(text.len() as u64 + 1)
        .read_to_end(&mut read)
        .is_ok_and(|_| read == text.as_bytes())
}

/// how many times its size on disk a part's text may run to for [`whole`] to hash it, beside
/// [`WHOLE_SLACK`]: a real list compresses a few times at most, as each row's SHA-256 is 64 hex
/// digits that do not compress, so only a part made to decompress far past its size runs past it.
const WHOLE_RATIO: u64 = 64;

/// the bytes of text a part may hold past [`WHOLE_RATIO`] times its size, for a small part.
const WHOLE_SLACK: u64 = 1 << 20;

/// whether the parts `listed` names below the project at `root` hold the list data.lock records
/// for `name`, decompressed and hashed without building a row: named as fetchloom writes them,
/// within the files a run lists, each a plain file whose first line names the columns this
/// fetchloom reads and whose text runs to no more than [`WHOLE_RATIO`] times its size on disk
/// and [`WHOLE_SLACK`], so a part planted to decompress far past its size costs no more than
/// that. any doubt says no, and [`read`] then says what is wrong, a row at a time.
pub(crate) fn whole(root: &Path, name: &str, listed: &Listed) -> bool {
    let within = usize::try_from(listed.count).is_ok_and(|count| count <= super::most_files());
    if !named_as_written(name, listed) || !within {
        return false;
    }
    let mut hasher = Hasher::default();
    for part in &listed.parts {
        let path = root.join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(file) = crate::volume::read_unfollowed(&path) else {
            return false;
        };
        let Ok(size) = file.metadata().map(|found| found.len()) else {
            return false;
        };
        let Ok(mut decoder) = zstd::stream::read::Decoder::new(file) else {
            return false;
        };
        if decoder.window_log_max(WINDOW_LOG).is_err() {
            return false;
        }
        let most = size.saturating_mul(WHOLE_RATIO).saturating_add(WHOLE_SLACK);
        // a byte past the most tells a part that runs past it from one that holds that much
        let mut reader = BufReader::new(decoder.take(most.saturating_add(1)));
        let mut header = Vec::new();
        let read = (&mut reader).take(LONGEST).read_until(b'\n', &mut header);
        if read.is_err() || header != HEADER.as_bytes() {
            return false;
        }
        hasher.update(&header);
        let mut held = header.len() as u64;
        let mut block = vec![0; 64 << 10];
        loop {
            match reader.read(&mut block) {
                Ok(0) => break,
                Ok(read) => {
                    held += read as u64;
                    hasher.update(&block[..read]);
                }
                Err(_) => return false,
            }
        }
        if held > most {
            return false;
        }
    }
    hasher.finish().0 == listed.sha256
}

/// [`FOLDER`] below the project at `root`, there or not yet, refused when it is a link or a
/// file: a cloned project's link would take reading, writing and removing lists outside it.
pub(crate) fn folder(root: &Path) -> Result<PathBuf, Error> {
    let folder = root.join(FOLDER);
    let found = match fs::symlink_metadata(&folder) {
        Ok(found) => found.file_type(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(folder),
        Err(err) => {
            return Err(Error::io(
                format_args!("could not read {}", folder.display()),
                &err,
            ));
        }
    };
    if found.is_symlink() || !found.is_dir() {
        let what = if found.is_symlink() {
            "a link"
        } else {
            "a file"
        };
        return Err(Error::new(
            Kind::ProjectInvalid,
            format!(
                "{} is {what}, and fetchloom keeps data.lock's file lists only in a real folder",
                folder.display()
            ),
        )
        .help(format!(
            "replace {FOLDER} with a real folder of the lists data.lock names before running this command"
        )));
    }
    Ok(folder)
}

/// where the parts of a lock's lists come from.
#[derive(Clone, Copy)]
pub(crate) enum Parts<'a> {
    /// [`FOLDER`] below the project at this folder.
    Folder(&'a Path),
    /// the bytes this gives for a part by its path below the project, as a git commit holds it.
    Given(&'a dyn Fn(&str) -> Result<Vec<u8>, Error>),
    /// none read: a list is checked by what data.lock says of it, its rows left out, for a reader
    /// that needs no listed file once [`super::lists_whole`] found every part there and whole.
    Unread,
}

/// the rows of `name`'s list from the parts `listed` names in `parts`, checked against what
/// data.lock records; refused unread when a part's path is not one fetchloom writes for `name`,
/// when the list holds more files than a run may list, or when [`FOLDER`] is no real folder.
/// each row's text is weighed against `budget` as it is read.
pub(crate) fn read(
    parts: Parts<'_>,
    name: &str,
    listed: &Listed,
    budget: &mut super::Budget,
) -> Result<Vec<LockedFile>, Error> {
    if !named_as_written(name, listed) {
        return Err(broken(format!(
            "data.lock names the file list of {name} in files fetchloom does not write"
        )));
    }
    let count = usize::try_from(listed.count).unwrap_or(usize::MAX);
    let most = super::most_files();
    if count > most {
        return Err(Error::new(
            Kind::ProjectInvalid,
            format!(
                "data.lock lists {} files in {name}, more than the {} this run lists for one dataset",
                crate::units::grouped(listed.count),
                crate::units::grouped(most as u64)
            ),
        )
        .help("raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory"));
    }
    match parts {
        Parts::Folder(root) => {
            folder(root)?;
        }
        Parts::Given(_) => {}
        Parts::Unread => return Ok(Vec::new()),
    }
    let mut rows: Vec<LockedFile> = Vec::with_capacity(count.min(1 << 20));
    let mut hasher = Hasher::default();
    let mut bytes: u64 = 0;
    for part in &listed.parts {
        let file: Box<dyn Read> = match parts {
            Parts::Folder(root) => {
                let path = root.join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
                // a clone can hold a link to a fifo or a device, which would hang or fill memory
                Box::new(
                    crate::volume::read_unfollowed(&path)
                        .map_err(|err| unreadable(name, part, &err, Some(root)))?,
                )
            }
            Parts::Given(given) => Box::new(std::io::Cursor::new(given(part)?)),
            Parts::Unread => return Ok(Vec::new()),
        };
        let mut decoder = zstd::stream::read::Decoder::new(file)
            .map_err(|err| unreadable(name, part, &err, None))?;
        decoder
            .window_log_max(WINDOW_LOG)
            .map_err(|err| unreadable(name, part, &err, None))?;
        let mut reader = BufReader::new(decoder);
        let mut line = Vec::new();
        let mut number = 0;
        loop {
            line.clear();
            let read = (&mut reader)
                .take(LONGEST)
                .read_until(b'\n', &mut line)
                .map_err(|err| unread_line(part, number + 1, &err))?;
            if read == 0 {
                break;
            }
            number += 1;
            if line.last() != Some(&b'\n') {
                return Err(damaged(part, number));
            }
            hasher.update(&line);
            if number == 1 {
                if line != HEADER.as_bytes() {
                    return Err(newer_columns(part, &line).unwrap_or_else(|| damaged(part, 1)));
                }
                continue;
            }
            let row = std::str::from_utf8(&line[..line.len() - 1])
                .ok()
                .and_then(LockedFile::parse_row)
                .ok_or_else(|| damaged(part, number))?;
            if rows.len() == count || rows.last().is_some_and(|last| last.path >= row.path) {
                return Err(damaged(part, number));
            }
            budget.take(name, row.text())?;
            bytes = add(bytes, row.size);
            rows.push(row);
        }
        if number == 0 {
            return Err(damaged(part, 1));
        }
    }
    if rows.len() != count || bytes != listed.bytes || hasher.finish().0 != listed.sha256 {
        return Err(broken(format!(
            "the file list of {name} in {FOLDER} is not the one data.lock records"
        )));
    }
    Ok(rows)
}

/// [`read`] building only the rows whose paths `keep` takes, every other row checked as [`read`]
/// and data.lock's reader check it, the whole list counted, summed and hashed against what
/// data.lock records. none when a row unpacks an archive, when another row's link holds what only
/// a closer look can clear (`@`, `?` or `#`), or when anything is not as [`read`] takes it, which
/// then says what.
pub(crate) fn read_some(
    root: &Path,
    name: &str,
    listed: &Listed,
    budget: &mut super::Budget,
    keep: &dyn Fn(&str) -> bool,
) -> Option<Vec<LockedFile>> {
    let count = usize::try_from(listed.count).ok()?;
    if !named_as_written(name, listed) || count > super::most_files() {
        return None;
    }
    folder(root).ok()?;
    let mut rows = Vec::new();
    let mut hasher = Hasher::default();
    let (mut bytes, mut seen) = (0, 0);
    let mut previous = String::new();
    let mut line = Vec::new();
    for part in &listed.parts {
        let path = root.join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
        let file = crate::volume::read_unfollowed(&path).ok()?;
        let mut decoder = zstd::stream::read::Decoder::new(file).ok()?;
        decoder.window_log_max(WINDOW_LOG).ok()?;
        let mut reader = BufReader::new(decoder);
        let mut number = 0;
        loop {
            line.clear();
            let read = (&mut reader)
                .take(LONGEST)
                .read_until(b'\n', &mut line)
                .ok()?;
            if read == 0 {
                break;
            }
            number += 1;
            if line.last() != Some(&b'\n') {
                return None;
            }
            hasher.update(&line);
            if number == 1 {
                if line != HEADER.as_bytes() {
                    return None;
                }
                continue;
            }
            let text = std::str::from_utf8(&line[..line.len() - 1]).ok()?;
            let row = Fields::of(text)?;
            if row.unpacks || seen == count || (seen > 0 && previous.as_str() >= &*row.path) {
                return None;
            }
            budget
                .take(name, (row.path.len() + row.url.len()) as u64)
                .ok()?;
            if keep(&row.path) {
                rows.push(LockedFile::parse_row(text)?);
            } else if crate::name::escapes(&row.path).is_some() || row.url.contains(['@', '?', '#'])
            {
                return None;
            }
            seen += 1;
            bytes = add(bytes, row.size);
            previous.clear();
            previous.push_str(&row.path);
        }
        if number == 0 {
            return None;
        }
    }
    (seen == count && bytes == listed.bytes && hasher.finish().0 == listed.sha256).then_some(rows)
}

/// a row's fields as [`LockedFile::parse_row`] takes them, read without building the file:
/// none for a row it refuses.
struct Fields<'a> {
    path: std::borrow::Cow<'a, str>,
    size: u64,
    url: std::borrow::Cow<'a, str>,
    /// whether the row is an archive fetchloom unpacks.
    unpacks: bool,
}

impl<'a> Fields<'a> {
    fn of(line: &'a str) -> Option<Fields<'a>> {
        let unescaped = |text: &'a str| -> Option<std::borrow::Cow<'a, str>> {
            if text.contains('\\') {
                unescape(text).map(std::borrow::Cow::Owned)
            } else {
                Some(std::borrow::Cow::Borrowed(text))
            }
        };
        let mut fields = line.split('\t');
        let path = unescaped(fields.next()?)?;
        let size = size(fields.next()?)?;
        Sha256::from_hex(fields.next()?)?;
        let md5 = fields.next()?;
        if !md5.is_empty() {
            Md5::from_hex(md5)?;
        }
        let url = unescaped(fields.next()?)?;
        let unpacks_to = fields.next()?;
        if !unpacks_to.is_empty() {
            Sha256::from_hex(unpacks_to)?;
        }
        let fits = path.len() <= LONGEST_PATH && url.len() <= LONGEST_URL;
        (fields.next().is_none() && fits).then_some(Fields {
            path,
            size,
            url,
            unpacks: !unpacks_to.is_empty(),
        })
    }
}

/// the error for `part`, a list whose `header` names a column this fetchloom does not know, as a
/// newer fetchloom may add one: none when it names no columns at all, or only known ones, as a
/// damaged part's first line does.
fn newer_columns(part: &str, header: &[u8]) -> Option<Error> {
    let header = std::str::from_utf8(header).ok()?.strip_suffix('\n')?;
    let columns: Vec<&str> = header.split('\t').collect();
    let named = |column: &&str| {
        !column.is_empty()
            && column.len() <= 40
            && column
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    if !columns.iter().all(named) {
        return None;
    }
    let known: Vec<&str> = HEADER.trim_end().split('\t').collect();
    let unknown: Vec<&str> = columns
        .into_iter()
        .filter(|column| !known.contains(column))
        .collect();
    let message = match unknown.as_slice() {
        [] => return None,
        [one] => format!(
            "{part} has the column {one}, which this fetchloom does not know, so a newer fetchloom likely wrote it"
        ),
        many => format!(
            "{part} has the columns {}, which this fetchloom does not know, so a newer fetchloom likely wrote it",
            crate::units::listed(many, "and")
        ),
    };
    Some(Error::new(Kind::ProjectInvalid, message).help(NEWER))
}

/// appends `text` with backslashes, tabs and line breaks escaped, so a field never splits a line.
fn escape(text: &str, out: &mut String) {
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
}

fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            out.push(match characters.next()? {
                '\\' => '\\',
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            });
        } else {
            out.push(character);
        }
    }
    Some(out)
}

fn broken(message: String) -> Error {
    Error::new(Kind::ProjectInvalid, message).help(recovery())
}

/// the error for `part` of `name`'s list, which did not read as `err` says, below `root` where
/// the parts are a project's.
fn unreadable(name: &str, part: &str, err: &std::io::Error, root: Option<&Path>) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        return not_there(name, part, root);
    }
    broken(format!("could not read {part}: {err}"))
}

/// the error for `line` of `part`, which did not read as `err` says: the system's own failure in
/// its words, else a part the decoder refused, which is damaged there.
fn unread_line(part: &str, line: usize, err: &std::io::Error) -> Error {
    // the decoder's errors carry no code of the system's
    if err.raw_os_error().is_some() {
        return broken(format!("could not read {part}: {err}"));
    }
    damaged(part, line)
}

/// the error for `part` of `name`'s list, which is not beside data.lock in the project at `root`,
/// where known. the usual cause is a commit of data.lock without the new list.
pub(crate) fn not_there(name: &str, part: &str, root: Option<&Path>) -> Error {
    let checkout = format!("checkout -- {FOLDER}");
    let back = root.map_or_else(
        || format!("git {checkout}"),
        |root| crate::suggested::git(root, &checkout),
    );
    Error::new(
        Kind::ProjectInvalid,
        format!("data.lock names {part}, the file list of {name}, which is not there"),
    )
    .detail(format!(
        "{FOLDER} was probably not committed with data.lock: `git commit -a` leaves new files out"
    ))
    .help(format!(
        "where {name} was pinned, run `git add {FOLDER}` and commit, then pull that commit here. a part deleted here comes back with `{back}`"
    ))
}

fn damaged(part: &str, line: usize) -> Error {
    broken(format!("{part} is damaged at line {line}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;

    fn files(count: usize) -> Vec<LockedFile> {
        (0..count)
            .map(|index| {
                let path = format!("shard/{index:07}.jpg");
                LockedFile {
                    sha256: digest::sha256(path.as_bytes()),
                    md5: (index % 2 == 0).then(|| Md5::from_hex(&"ab".repeat(16)).unwrap()),
                    url: format!("https://example.org/data/{path}?download=1"),
                    unpacks_to: (index % 3 == 0).then(|| digest::sha256(b"tree")),
                    size: index as u64,
                    path,
                }
            })
            .collect()
    }

    /// writes `rows` as `name`'s list below `root`, returning how data.lock names it.
    fn write(root: &Path, name: &str, rows: &[LockedFile]) -> Listed {
        let texts = texts(rows);
        let listed = listed(name, rows, &texts);
        fs::create_dir_all(root.join(FOLDER)).unwrap();
        for (part, text) in listed.parts.iter().zip(&texts) {
            fs::write(root.join(part), compress(text).unwrap()).unwrap();
        }
        listed
    }

    /// [`super::read`] with all the memory there is.
    fn read(parts: Parts<'_>, name: &str, listed: &Listed) -> Result<Vec<LockedFile>, Error> {
        let mut budget = super::super::Budget::of(std::iter::empty(), u64::MAX).unwrap();
        super::read(parts, name, listed, &mut budget)
    }

    #[test]
    fn a_list_reads_back_the_files_it_was_written_from_across_parts() {
        let root = tempfile::tempdir().unwrap();
        let written = files(PART + 7);
        let listed = write(root.path(), "big", &written);
        assert_eq!(listed.parts.len(), 2);
        assert!(listed.parts[0].starts_with("data.lock.d/big."));
        assert!(listed.parts[0].ends_with(".1.tsv.zst"));
        assert_eq!(listed.count, (PART + 7) as u64);
        assert_eq!(
            read(Parts::Folder(root.path()), "big", &listed).unwrap(),
            written
        );
        for part in &listed.parts {
            assert!(
                is_part(part.strip_prefix("data.lock.d/").unwrap()),
                "{part}"
            );
        }
        assert!(!is_part("notes.txt"));
        assert!(!is_part("big.tsv.zst"));
    }

    /// a list whose parts hash to its record but decompress far past their size is not taken
    /// as whole, though it is one, so a part planted to decompress for ever stops a settled sync
    /// soon: the long way, which reads a row at a time, then reads it. a list of every day's
    /// rows, as compressed, is.
    #[test]
    fn a_list_that_decompresses_far_past_its_size_is_not_taken_as_whole() {
        let root = tempfile::tempdir().unwrap();
        let link = format!("https://example.org/{}", "a".repeat(3_000));
        let repeated: Vec<LockedFile> = (0..2 * PART)
            .map(|index| LockedFile {
                path: format!("f/{index:07}.bin"),
                size: 1,
                sha256: digest::sha256(b"one"),
                md5: None,
                url: link.clone(),
                unpacks_to: None,
            })
            .collect();
        let listed = write(root.path(), "repeated", &repeated);
        // each part's text runs past 64 times its size on disk and a MiB more, which whole keeps to
        for (part, text) in listed.parts.iter().zip(texts(&repeated)) {
            let compressed = fs::metadata(root.path().join(part)).unwrap().len();
            let text = text.len() as u64;
            assert!(text > compressed * 64 + (1 << 20), "{text} {compressed}");
        }
        assert!(!whole(root.path(), "repeated", &listed));
        assert_eq!(
            read(Parts::Folder(root.path()), "repeated", &listed).unwrap(),
            repeated
        );
        let listed = write(root.path(), "plain", &files(2 * PART));
        assert!(whole(root.path(), "plain", &listed));
    }

    /// lists a data.lock.d that is a link holds are never read, wherever it points, nor are
    /// those of one that is a file.
    #[test]
    fn a_list_is_read_only_from_a_real_folder() {
        let outside = tempfile::tempdir().unwrap();
        let listed = write(outside.path(), "set", &files(2));
        let filed = tempfile::tempdir().unwrap();
        fs::write(filed.path().join(FOLDER), "").unwrap();
        let file = read(Parts::Folder(filed.path()), "set", &listed).unwrap_err();
        assert_eq!(file.kind, Kind::ProjectInvalid);
        assert!(file.message.contains("is a file"), "{}", file.message);
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join(FOLDER);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path().join(FOLDER), &link).is_ok();
        // making a symlink on windows needs developer mode or admin
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path().join(FOLDER), &link).is_ok();
        if !made {
            return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
        }
        let linked = read(Parts::Folder(root.path()), "set", &listed).unwrap_err();
        assert_eq!(linked.kind, Kind::ProjectInvalid);
        assert!(linked.message.contains("is a link"), "{}", linked.message);
    }

    #[test]
    fn fields_with_backslashes_tabs_and_breaks_survive() {
        let root = tempfile::tempdir().unwrap();
        let mut odd = files(1);
        odd[0].path = "a\\b".to_owned();
        odd[0].url = "https://example.org/x\ty\nz\\w".to_owned();
        let listed = write(root.path(), "odd", &odd);
        assert_eq!(
            listed.parts,
            [format!(
                "data.lock.d/odd.{}.tsv.zst",
                &listed.sha256.hex()[..12]
            )]
        );
        assert_eq!(
            read(Parts::Folder(root.path()), "odd", &listed).unwrap(),
            odd
        );
    }

    /// a row is held to the longest path and link fetchloom pins, so a cloned part of rows a few
    /// bytes each once compressed cannot ask for gigabytes; the longest of each still reads, and
    /// a list fetchloom would refuse is never written.
    #[test]
    fn a_row_longer_than_fetchloom_writes_is_refused_unread() {
        let root = tempfile::tempdir().unwrap();
        let link = |length: usize| format!("https://e.org/{}", "a".repeat(length - 14));
        for (path, url, reads) in [
            ("p".repeat(4096), link(64 << 10), true),
            ("p".repeat(4097), link(100), false),
            ("p".to_owned(), link((64 << 10) + 1), false),
        ] {
            let mut rows = files(1);
            rows[0].path.clone_from(&path);
            rows[0].url.clone_from(&url);
            let lengths = (path.len(), url.len());
            assert_eq!(listable("set", &rows).is_ok(), reads, "{lengths:?}");
            let listed = write(root.path(), "set", &rows);
            match read(Parts::Folder(root.path()), "set", &listed) {
                Ok(read) => assert!(reads && read == rows, "{lengths:?}"),
                Err(error) => {
                    assert!(!reads, "{lengths:?}: {error:?}");
                    assert!(error.message.ends_with("is damaged at line 2"), "{error:?}");
                }
            }
        }
    }

    /// a part whose header names a column this fetchloom does not know, as a newer one may add,
    /// is no damage: the error asks for that newer fetchloom, never to pin anew. a first line
    /// that names no columns, or only known ones, is still damaged.
    #[test]
    fn a_column_a_newer_fetchloom_writes_asks_for_that_fetchloom() {
        let root = tempfile::tempdir().unwrap();
        let rows = files(2);
        let listed = write(root.path(), "set", &rows);
        let part = &listed.parts[0];
        let text = texts(&rows).remove(0);
        for (header, says) in [
            (
                "path\tsize\tsha256\tmd5\turl\tunpacks_to\tcitation\n",
                Some(format!(
                    "{part} has the column citation, which this fetchloom does not know, so a newer fetchloom likely wrote it"
                )),
            ),
            (
                "path\tsize\tsha256\tmd5\turl\tunpacks_to\tcitation\tlicense\n",
                Some(format!(
                    "{part} has the columns citation and license, which this fetchloom does not know, so a newer fetchloom likely wrote it"
                )),
            ),
            ("\u{1b}[2J\tpath\n", None),
            ("path\tsize\tsha256\tmd5\turl\n", None),
        ] {
            let changed = text.replacen(HEADER, header, 1);
            fs::write(root.path().join(part), compress(&changed).unwrap()).unwrap();
            let error = read(Parts::Folder(root.path()), "set", &listed).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            match says {
                Some(says) => {
                    assert_eq!(error.message, says);
                    assert_eq!(error.help.as_deref(), Some(NEWER));
                }
                None => assert_eq!(error.message, format!("{part} is damaged at line 1")),
            }
        }
        assert!(!NEWER.contains("delete") && !NEWER.contains("fetchloom lock"));
    }

    #[test]
    fn a_list_that_changed_or_names_other_files_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let written = files(2);
        let listed = write(root.path(), "set", &written);
        assert_eq!(
            read(Parts::Folder(root.path()), "set", &listed).unwrap(),
            written
        );
        let mut changed = written.clone();
        changed[1].sha256 = digest::sha256(b"changed");
        // a part's name holds only the hash's start, so changing its end reaches the content check
        let mut wrong_hash = listed.sha256.hex();
        let last = wrong_hash.pop().unwrap();
        wrong_hash.push(if last == '0' { '1' } else { '0' });
        let mismatch = "not the one data.lock records";
        for (reason, rows, summary, message) in [
            ("extra row", files(3), listed.clone(), "damaged"),
            ("changed row", changed, listed.clone(), mismatch),
            (
                "file count",
                written.clone(),
                Listed {
                    count: listed.count + 1,
                    ..listed.clone()
                },
                mismatch,
            ),
            (
                "byte count",
                written.clone(),
                Listed {
                    bytes: listed.bytes + 1,
                    ..listed.clone()
                },
                mismatch,
            ),
            (
                "list hash",
                written,
                Listed {
                    sha256: Sha256::from_hex(&wrong_hash).unwrap(),
                    ..listed.clone()
                },
                mismatch,
            ),
        ] {
            let text = texts(&rows).remove(0);
            fs::write(root.path().join(&listed.parts[0]), compress(&text).unwrap()).unwrap();
            let error = read(Parts::Folder(root.path()), "set", &summary).expect_err(reason);
            assert_eq!(error.kind, Kind::ProjectInvalid, "{reason}");
            assert!(
                error.message.contains(message),
                "{reason}: {}",
                error.message
            );
            assert_eq!(error.help.as_deref(), Some(recovery().as_str()), "{reason}");
        }
        // a part fetchloom does not write, such as one outside its folder
        let elsewhere = Listed {
            parts: vec!["../outside.tsv.zst".to_owned()],
            ..listed
        };
        assert!(
            read(Parts::Folder(root.path()), "set", &elsewhere)
                .unwrap_err()
                .message
                .contains("does not write")
        );
        // a part that is gone, as in a clone of a commit that left data.lock.d out: the help
        // gets the list, and never leads with deleting data.lock, which may pin newer data
        fs::remove_file(root.path().join(&listed.parts[0])).unwrap();
        let gone = read(Parts::Folder(root.path()), "set", &listed).unwrap_err();
        assert_eq!(
            gone.message,
            format!(
                "data.lock names {}, the file list of set, which is not there",
                listed.parts[0]
            )
        );
        assert_eq!(
            gone.details,
            [
                "data.lock.d was probably not committed with data.lock: `git commit -a` leaves new files out"
            ]
        );
        let checkout = crate::suggested::git(root.path(), "checkout -- data.lock.d");
        assert_eq!(
            gone.help.as_deref(),
            Some(&*format!(
                "where set was pinned, run `git add data.lock.d` and commit, then pull that commit here. a part deleted here comes back with `{checkout}`"
            ))
        );
    }

    /// a part is read only as a plain file: a link, as a clone can hold, to a fifo, a device or
    /// a file elsewhere is refused before anything is read through it.
    #[cfg(unix)]
    #[test]
    fn a_part_that_is_a_link_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let listed = write(root.path(), "set", &files(2));
        let part = root.path().join(&listed.parts[0]);
        let elsewhere = root.path().join("elsewhere.tsv.zst");
        fs::rename(&part, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &part).unwrap();
        let error = read(Parts::Folder(root.path()), "set", &listed).unwrap_err();
        assert!(
            error.message.contains("is a link or a special file"),
            "{}",
            error.message
        );
    }

    #[test]
    fn damaged_text_is_refused_at_its_line() {
        let root = tempfile::tempdir().unwrap();
        let listed = write(root.path(), "set", &files(3));
        let text = texts(&files(3)).remove(0);
        for damaged in [
            text.replacen("\t0\t", "\t+0\t", 1),
            text.replacen(".jpg", ".jpg\textra", 1),
            text.replace("shard/0000001.jpg", "shard/0000000.jpg"),
            text.replacen(HEADER, "path\tsize\n", 1),
        ] {
            fs::write(
                root.path().join(&listed.parts[0]),
                compress(&damaged).unwrap(),
            )
            .unwrap();
            let error = read(Parts::Folder(root.path()), "set", &listed).unwrap_err();
            assert!(
                error.message.contains("damaged at line"),
                "{}",
                error.message
            );
        }
    }

    /// a part that is no compressed list, or whose lines stop partway, is damaged at the first
    /// line it lacks, in words of fetchloom's rather than the decoder's.
    #[test]
    fn a_part_that_does_not_decompress_is_damaged_at_the_line_it_stops() {
        let root = tempfile::tempdir().unwrap();
        let listed = write(root.path(), "set", &files(3));
        let text = texts(&files(3)).remove(0);
        // the header and two files, so a part continuing past them stops at line 4
        let three = text.split_inclusive('\n').take(3).collect::<String>();
        let rest = compress(&text[three.len()..]).unwrap();
        let first = compress(&three).unwrap();
        for (reason, bytes, line) in [
            ("not compressed", text.into_bytes(), 1),
            (
                "no frame after",
                [first.clone(), b"rest".to_vec()].concat(),
                4,
            ),
            (
                "cut short",
                [first, rest[..rest.len() - 3].to_vec()].concat(),
                4,
            ),
        ] {
            fs::write(root.path().join(&listed.parts[0]), bytes).unwrap();
            let error = read(Parts::Folder(root.path()), "set", &listed).expect_err(reason);
            assert_eq!(
                error.message,
                format!("{} is damaged at line {line}", listed.parts[0]),
                "{reason}"
            );
            assert_eq!(error.help.as_deref(), Some(recovery().as_str()), "{reason}");
        }
    }

    #[test]
    fn a_list_past_the_most_files_a_run_lists_is_not_read() {
        let listed = Listed {
            parts: names("huge", &digest::sha256(b"x"), 1),
            count: u64::MAX,
            bytes: 0,
            sha256: digest::sha256(b"x"),
        };
        let error = read(Parts::Folder(Path::new("/nowhere")), "huge", &listed).unwrap_err();
        assert_eq!(
            error.help.as_deref(),
            Some(
                "raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory"
            )
        );
    }
}
