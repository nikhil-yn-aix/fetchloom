//! a list of links, as `add -i` and `get -i` read it: one link a line, and after a link, where
//! the line gives one, the SHA-256 its one file must have. a list is a file anyone could have
//! written, so the whole list is checked before any request.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::Read as _;
use std::path::Path;

use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net;
use crate::source::search::is_search;
use crate::source::{Parsed, Reference, local};
use crate::stop::Stop;

/// what a line holds, for messages that cannot read it.
const FORM: &str = "a line is a link, or a link and its SHA-256: 64 hex digits, or sha256: and 64 hex digits. blank lines and lines starting with # are skipped";

/// one link of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Line {
    /// where the line is in its list, counting from 1.
    pub(super) number: usize,
    pub(super) link: String,
    /// the SHA-256 the line gives for the link's one file.
    pub(super) sha256: Option<Sha256>,
}

impl Line {
    /// how messages name the line, as `line 2 of links.txt`.
    pub(super) fn named(&self, list: &str) -> String {
        format!("line {} of {list}", self.number)
    }
}

/// the text of the list at `path`, or of stdin for `-`, and the name messages give it.
pub(super) async fn read(path: &Path, stop: &Stop) -> Result<(String, String), Error> {
    stop.check()?;
    let path = path.to_path_buf();
    let reading = tokio::task::spawn_blocking(move || read_text(&path));
    tokio::select! {
        biased;
        () = stop.asked() => Err(Stop::stopped()),
        outcome = reading => outcome.map_err(|err| Error::new(
            Kind::LocalIo,
            format!("could not read the link list: {err}"),
        ))?,
    }
}

fn read_text(path: &Path) -> Result<(String, String), Error> {
    let mut text = String::new();
    if path == Path::new("-") {
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|err| Error::io("could not read links from stdin", &err))?;
        return Ok(("stdin".to_owned(), text));
    }
    let name = path.display().to_string();
    std::fs::File::open(path)
        .and_then(|mut file| file.read_to_string(&mut text))
        .map_err(|err| Error::io(format_args!("could not read {name}"), &err))?;
    Ok((name, text))
}

/// the links `text`, the list `name`, gives, each with the SHA-256 its line gives. a line of
/// search words is refused here, since its second word is no hash.
pub(super) fn parse(name: &str, text: &str) -> Result<Vec<Line>, Error> {
    let mut lines = Vec::new();
    for (index, written) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = index + 1;
        let written = written.trim();
        if written.is_empty() || written.starts_with('#') {
            continue;
        }
        let mut words = written.split_whitespace();
        let link = words.next().unwrap_or(written);
        if is_search(link) {
            return Err(not_a_link(name, number, written));
        }
        let given = words.next();
        let sha256 = given.map(sha256);
        let at = format!("line {number} of {name}");
        if words.next().is_some() {
            return Err(Error::new(
                Kind::Usage,
                format!("{at} holds more than a link and its SHA-256"),
            )
            .help(FORM));
        }
        if let (Some(given), Some(None)) = (given, sha256) {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "\"{}\" on {at} is not a SHA-256",
                    net::mask_query(given).escape_debug()
                ),
            )
            .help(FORM));
        }
        lines.push(Line {
            number,
            link: link.to_owned(),
            sha256: sha256.flatten(),
        });
    }
    Ok(lines)
}

/// reads a line's SHA-256: 64 hex digits in either case, or the same after `sha256:`.
fn sha256(text: &str) -> Option<Sha256> {
    let hex = match text.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("sha256:") => &text[7..],
        _ => text,
    };
    Sha256::from_hex(hex)
}

/// `lines` of the list `name`, each with its reference, once every line names data to fetch: no
/// path on this machine or line naming what another names, and, `for_project`, no link carrying
/// a secret data.toml and data.lock would keep. the first line that fails is named, any secret
/// in it masked.
pub(super) fn checked(
    name: &str,
    lines: Vec<Line>,
    for_project: bool,
) -> Result<Vec<(Line, Parsed)>, Error> {
    if lines.is_empty() {
        return Err(
            Error::new(Kind::Usage, format!("{name} lists no links")).help(
                "put one link per line in it. blank lines and lines starting with # are skipped",
            ),
        );
    }
    let mut seen: HashMap<(String, Option<String>, Option<String>), usize> = HashMap::new();
    let mut checked = Vec::with_capacity(lines.len());
    for line in lines {
        let at = line.named(name);
        if local::is_path(&line.link) {
            let help = if for_project {
                "add files on this machine one file or folder at a time, as in `fetchloom add ./flowers`"
            } else {
                "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990"
            };
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!("{at} is a path on this machine, and a list holds links, DOIs and ids"),
            )
            .detail(net::mask_query(&line.link).into_owned())
            .help(help));
        }
        let parsed = Reference::parse(&line.link).map_err(|error| on_line(error, &at))?;
        if for_project {
            parsed
                .reference
                .check_shareable()
                .map_err(|error| on_line(error, &at))?;
        }
        let key = (
            parsed.reference.to_string(),
            parsed.file.clone(),
            parsed.folder.clone(),
        );
        match seen.entry(key) {
            Entry::Occupied(first) => {
                return Err(Error::new(
                    Kind::Usage,
                    format!("{at} names the same data as line {}", first.get()),
                )
                .help("keep one of the two lines"));
            }
            Entry::Vacant(free) => {
                free.insert(line.number);
            }
        }
        checked.push((line, parsed));
    }
    Ok(checked)
}

/// `error`, about the link on `at`, a line such as `line 2 of links.txt`, saying so.
fn on_line(mut error: Error, at: &str) -> Error {
    error.message = format!("{at}: {}", error.message);
    error
}

fn not_a_link(name: &str, number: usize, line: &str) -> Error {
    let line = net::mask_query(line);
    Error::new(
        Kind::Usage,
        format!("line {number} of {name} is not a link, a DOI or an id"),
    )
    .detail(line.clone().into_owned())
    .help(format!(
        "find its id with `fetchloom search {line}`, then list that instead"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IRIS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn line(number: usize, link: &str, sha256: Option<&str>) -> Line {
        Line {
            number,
            link: link.to_owned(),
            sha256: sha256.and_then(Sha256::from_hex),
        }
    }

    #[test]
    fn a_line_takes_a_link_and_an_optional_sha256() {
        let text = format!(
            "\u{feff}# measurements to compare\r\n\r\nzenodo:3608990\n  https://example.org/iris.csv   {IRIS}  \nhf:datasets/scikit-learn/iris SHA256:{}\n10.5281/zenodo.1 {IRIS}\n# done\n",
            IRIS.to_uppercase()
        );
        assert_eq!(
            parse("links.txt", &text).unwrap(),
            [
                line(3, "zenodo:3608990", None),
                line(4, "https://example.org/iris.csv", Some(IRIS)),
                line(5, "hf:datasets/scikit-learn/iris", Some(IRIS)),
                line(6, "10.5281/zenodo.1", Some(IRIS)),
            ]
        );
        for (text, message) in [
            (
                "zenodo:1 abc".to_owned(),
                "\"abc\" on line 1 of links.txt is not a SHA-256",
            ),
            (
                format!("zenodo:1 {IRIS} zenodo:2"),
                "line 1 of links.txt holds more than a link and its SHA-256",
            ),
            (
                format!("\nfashion mnist\nzenodo:1 {IRIS}"),
                "line 2 of links.txt is not a link, a DOI or an id",
            ),
            (
                format!("iris {IRIS}"),
                "line 1 of links.txt is not a link, a DOI or an id",
            ),
        ] {
            let error = parse("links.txt", &text).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{text}");
            assert_eq!(error.message, message, "{text}");
        }
    }

    #[test]
    fn the_whole_list_is_checked_before_any_request() {
        let lines = parse(
            "links.txt",
            "https://example.org/iris.csv\nhttps://example.org/wine.csv?token=hunter2\nzenodo:not-a-number\n",
        )
        .unwrap();
        let shared = checked("links.txt", lines.clone(), true).unwrap_err();
        assert_eq!(shared.kind, Kind::ReferenceInvalid);
        assert_eq!(
            shared.message,
            "line 2 of links.txt: this link carries token, a secret that data.toml and data.lock would keep and share"
        );
        let whole = format!("{shared:?}");
        assert!(!whole.contains("hunter2"), "{whole}");
        let once = checked("links.txt", lines, false).unwrap_err();
        assert_eq!(once.kind, Kind::ReferenceInvalid);
        assert!(
            once.message.starts_with("line 3 of links.txt: "),
            "{once:?}"
        );
        for (text, kind, message) in [
            (
                "zenodo:3608990\nhttps://zenodo.org/records/3608990\n",
                Kind::Usage,
                "line 2 of links.txt names the same data as line 1",
            ),
            ("# nothing yet\n\n", Kind::Usage, "links.txt lists no links"),
        ] {
            let error = checked("links.txt", parse("links.txt", text).unwrap(), true).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (kind, message),
                "{text}"
            );
        }
        let files =
            "https://zenodo.org/records/1/files/a.csv\nhttps://zenodo.org/records/1/files/b.csv\n";
        assert_eq!(
            checked("links.txt", parse("links.txt", files).unwrap(), true)
                .unwrap()
                .len(),
            2,
            "two files of one record are two links"
        );
    }

    #[test]
    fn local_paths_and_file_links_are_refused_in_lists() {
        for (link, for_project, help) in [
            (
                "./flowers",
                true,
                "add files on this machine one file or folder at a time, as in `fetchloom add ./flowers`",
            ),
            (
                "file:///etc/hostname",
                true,
                "add files on this machine one file or folder at a time, as in `fetchloom add ./flowers`",
            ),
            (
                "~/flowers.csv",
                false,
                "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990",
            ),
        ] {
            let text = format!("zenodo:1\n{link}\n");
            let error =
                checked("links.txt", parse("links.txt", &text).unwrap(), for_project).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{link}");
            assert_eq!(
                error.message,
                "line 2 of links.txt is a path on this machine, and a list holds links, DOIs and ids"
            );
            assert_eq!(error.details, [link]);
            assert_eq!(error.help.as_deref(), Some(help), "{link}");
        }
    }
}
