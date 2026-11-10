//! the file lists of datasets too big to list inline, kept beside data.lock in `data.lock.d`. a
//! dataset that lists more than [`INLINE`] files keeps them there, compressed and in parts, so
//! data.lock stays small enough to read and review, and git takes every part under its size
//! limits. data.lock records each part with the count, the bytes and the SHA-256 of the text the
//! parts hold, so a part that changed or went missing is found as the lock is read.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use super::LockedFile;
use crate::digest::{Hasher, Md5, Sha256};
use crate::error::{Error, Kind};

/// the folder beside data.lock that holds the lists.
pub(crate) const FOLDER: &str = "data.lock.d";
/// the most files a dataset lists in data.lock itself. a rule of the format, not a setting, so
/// every machine writes the same lock for the same files.
pub(crate) const INLINE: usize = 50_000;
/// the most files a dataset lists in data.lock itself in this run: [`INLINE`], or fewer for a CLI
/// test that asks for them, which release builds never read.
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
/// the first line of every part, naming its columns.
const HEADER: &str = "path\tsize\tsha256\tmd5\turl\tunpacks_to\n";
/// how hard parts are compressed. a set level keeps the same bytes for the same list.
const LEVEL: i32 = 9;
/// the most memory decompressing a part may take: a window of 2^27 bytes.
const WINDOW_LOG: u32 = 27;
/// the longest line a part may hold, so a damaged part cannot fill memory.
const LONGEST: u64 = 1 << 20;
/// how to recover a list fetchloom cannot use.
const BROKEN: &str = "restore data.lock and data.lock.d from git, or delete data.lock and run `fetchloom lock`, which resolves every dataset again and may pin newer data";

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

/// the texts of the parts that list `files`, in order, each beginning with the header.
pub(crate) fn texts(files: &[LockedFile]) -> Vec<String> {
    files
        .chunks(PART)
        .map(|chunk| {
            let mut text = String::from(HEADER);
            for file in chunk {
                escape(&file.path, &mut text);
                text.push('\t');
                text.push_str(&file.size.to_string());
                text.push('\t');
                text.push_str(&file.sha256.hex());
                text.push('\t');
                if let Some(md5) = file.md5 {
                    text.push_str(&md5.hex());
                }
                text.push('\t');
                escape(&file.url, &mut text);
                text.push('\t');
                if let Some(tree) = file.unpacks_to {
                    text.push_str(&tree.hex());
                }
                text.push('\n');
            }
            text
        })
        .collect()
}

/// how data.lock names the list of the dataset `name`, which holds `files` in parts of `texts`.
pub(crate) fn listed(name: &str, files: &[LockedFile], texts: &[String]) -> Listed {
    let mut hasher = Hasher::default();
    for text in texts {
        hasher.update(text.as_bytes());
    }
    let sha256 = hasher.finish().0;
    Listed {
        parts: names(name, &sha256, texts.len()),
        count: files.len() as u64,
        bytes: files
            .iter()
            .map(|file| file.size)
            .fold(0, u64::saturating_add),
        sha256,
    }
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

/// the bytes a part is kept in.
pub(crate) fn compress(text: &str) -> std::io::Result<Vec<u8>> {
    zstd::encode_all(text.as_bytes(), LEVEL)
}

/// the files of the dataset `name`, read from the parts `listed` names below `root` and checked
/// against what data.lock records of them. a list of more files than a run may list is refused
/// before it is read, as a listing of them would be.
pub(crate) fn read(root: &Path, name: &str, listed: &Listed) -> Result<Vec<LockedFile>, Error> {
    if listed.parts.is_empty() || listed.parts != names(name, &listed.sha256, listed.parts.len()) {
        return Err(broken(format!(
            "data.lock names the file list of {name} in files fetchloom does not write"
        )));
    }
    let count = usize::try_from(listed.count).unwrap_or(usize::MAX);
    let most = crate::source::most_files();
    if count > most {
        return Err(Error::new(
            Kind::ProjectInvalid,
            format!("data.lock lists {count} files in {name}, more than the {most} this run lists for one dataset"),
        )
        .help("raise FETCHLOOM_MOST_FILES, or most_files in the settings file; each listed file takes about 2 KiB of memory"));
    }
    let mut files: Vec<LockedFile> = Vec::with_capacity(count.min(1 << 20));
    let mut hasher = Hasher::default();
    let mut bytes: u64 = 0;
    for part in &listed.parts {
        let path = root.join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
        let file = fs::File::open(&path).map_err(|err| unreadable(part, &err))?;
        let mut decoder =
            zstd::stream::read::Decoder::new(file).map_err(|err| unreadable(part, &err))?;
        decoder
            .window_log_max(WINDOW_LOG)
            .map_err(|err| unreadable(part, &err))?;
        let mut reader = BufReader::new(decoder);
        let mut line = Vec::new();
        let mut number = 0;
        loop {
            line.clear();
            let read = (&mut reader)
                .take(LONGEST)
                .read_until(b'\n', &mut line)
                .map_err(|err| unreadable(part, &err))?;
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
                    return Err(damaged(part, number));
                }
                continue;
            }
            let file = parse(&line).ok_or_else(|| damaged(part, number))?;
            if files.len() == count || files.last().is_some_and(|last| last.path >= file.path) {
                return Err(damaged(part, number));
            }
            bytes = bytes.saturating_add(file.size);
            files.push(file);
        }
        if number == 0 {
            return Err(damaged(part, 1));
        }
    }
    if files.len() != count || bytes != listed.bytes || hasher.finish().0 != listed.sha256 {
        return Err(broken(format!(
            "the file list of {name} in {FOLDER} is not the one data.lock records"
        )));
    }
    Ok(files)
}

/// one line of a part, without its line break: path, size, SHA-256, MD5, link and the tree an
/// archive unpacks to, between tabs.
fn parse(line: &[u8]) -> Option<LockedFile> {
    let text = std::str::from_utf8(line.strip_suffix(b"\n")?).ok()?;
    let mut fields = text.split('\t');
    let path = unescape(fields.next()?)?;
    let size = fields.next()?;
    if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let size = size.parse().ok()?;
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
    if fields.next().is_some() {
        return None;
    }
    Some(LockedFile {
        path,
        size,
        sha256,
        md5,
        url,
        unpacks_to,
    })
}

/// `text` with backslashes, tabs and line breaks written as `\\`, `\t`, `\n` and `\r`, so a field
/// never splits a line.
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
    Error::new(Kind::ProjectInvalid, message).help(BROKEN)
}

fn unreadable(part: &str, err: &std::io::Error) -> Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        return broken(format!("data.lock names {part}, which is not there"));
    }
    broken(format!("could not read {part}: {err}"))
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

    /// writes `files` as `name`'s list below `root`, as a writer would, and says how data.lock
    /// names it.
    fn write(root: &Path, name: &str, files: &[LockedFile]) -> Listed {
        let texts = texts(files);
        let listed = listed(name, files, &texts);
        fs::create_dir_all(root.join(FOLDER)).unwrap();
        for (part, text) in listed.parts.iter().zip(&texts) {
            fs::write(root.join(part), compress(text).unwrap()).unwrap();
        }
        listed
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
        assert_eq!(read(root.path(), "big", &listed).unwrap(), written);
        for part in &listed.parts {
            assert!(
                is_part(part.strip_prefix("data.lock.d/").unwrap()),
                "{part}"
            );
        }
        assert!(!is_part("notes.txt"));
        assert!(!is_part("big.tsv.zst"));
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
        assert_eq!(read(root.path(), "odd", &listed).unwrap(), odd);
    }

    #[test]
    fn a_list_that_changed_or_names_other_files_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let written = files(10);
        let listed = write(root.path(), "set", &written);
        // a part that holds other files
        let other = texts(&files(11));
        fs::write(
            root.path().join(&listed.parts[0]),
            compress(&other[0]).unwrap(),
        )
        .unwrap();
        let error = read(root.path(), "set", &listed).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(
            error.message.contains("holds") || error.message.contains("damaged"),
            "{}",
            error.message
        );
        // a part fetchloom does not write, such as one outside its folder
        let elsewhere = Listed {
            parts: vec!["../outside.tsv.zst".to_owned()],
            ..listed.clone()
        };
        assert!(
            read(root.path(), "set", &elsewhere)
                .unwrap_err()
                .message
                .contains("does not write")
        );
        // a part that is gone
        fs::remove_file(root.path().join(&listed.parts[0])).unwrap();
        let gone = read(root.path(), "set", &listed).unwrap_err();
        assert!(
            gone.message.ends_with("which is not there"),
            "{}",
            gone.message
        );
        assert_eq!(gone.help.as_deref(), Some(BROKEN));
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
            let error = read(root.path(), "set", &listed).unwrap_err();
            assert!(
                error.message.contains("damaged at line"),
                "{}",
                error.message
            );
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
        let error = read(Path::new("/nowhere"), "huge", &listed).unwrap_err();
        assert!(error.help.unwrap().contains("FETCHLOOM_MOST_FILES"));
    }
}
