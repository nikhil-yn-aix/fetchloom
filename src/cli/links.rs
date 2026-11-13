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

/// a line of search words is refused here, since its second word is no hash.
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
        let sha256 = given.map(sha256).transpose();
        let at = format!("line {number} of {name}");
        if words.next().is_some() {
            return Err(Error::new(
                Kind::Usage,
                format!("{at} holds more than a link and its SHA-256"),
            )
            .help(FORM));
        }
        let sha256 = sha256.map_err(|why| {
            Error::new(
                Kind::Usage,
                format!(
                    "\"{}\" on {at} is not a SHA-256",
                    net::mask_query(given.unwrap_or_default()).escape_debug()
                ),
            )
            .detail(why)
            .help(FORM)
        })?;
        lines.push(Line {
            number,
            link: link.to_owned(),
            sha256,
        });
    }
    Ok(lines)
}

/// reads a SHA-256 as sites and lists write one, bare or after `sha256:`. otherwise it says why,
/// naming the hash it looks like, since a site often states an MD5, or git a SHA-1, beside it.
pub(super) fn sha256(text: &str) -> Result<Sha256, String> {
    let text = text.trim();
    let hex = match text.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("sha256:") => &text[7..],
        _ => text,
    };
    Sha256::from_hex(hex).ok_or_else(|| {
        let bare = hex.split_once(':').map_or(hex, |(_, bare)| bare);
        let looks = if bare.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            match bare.len() {
                32 => Some(("an MD5", 32)),
                40 => Some(("a SHA-1", 40)),
                _ => None,
            }
        } else {
            None
        };
        match looks {
            Some((name, digits)) => format!(
                "this looks like {name}, which has {digits} hex digits. give the file's SHA-256, which has 64"
            ),
            None => "a SHA-256 is 64 hex digits".to_owned(),
        }
    })
}

/// `lines` with their references once every one names data to fetch: no path on this machine,
/// no two naming the same data and, `for_project`, no secret data.toml and data.lock would keep.
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
                one_at_a_time()
            } else {
                "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990".to_owned()
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
                .check_shareable()
                .map_err(|error| on_line(error, &at))?;
        }
        let key = same_data(&parsed);
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

pub(super) fn on_line(mut error: Error, at: &str) -> Error {
    error.message = format!("{at}: {}", error.message);
    error
}

/// how to add files on this machine, which a list or several links cannot.
fn one_at_a_time() -> String {
    format!(
        "add files on this machine one file or folder at a time, as in `{}`",
        super::project::command("add ./flowers")
    )
}

/// links typed for one add, checked as a list's lines are, each named by its own link: search
/// words go alone, and a path on this machine is added one at a time.
pub(super) fn typed(links: Vec<String>) -> Result<Vec<(Line, Parsed)>, Error> {
    if let Some(word) = links.iter().find(|link| is_search(link)) {
        let example = if links.iter().all(|link| is_search(link)) {
            links.join(" ")
        } else {
            "fashion mnist".to_owned()
        };
        return Err(Error::new(
            Kind::Usage,
            format!("\"{word}\" is not a link, a DOI or an id"),
        )
        .detail("several links add a dataset each, so each one names its data")
        .help(format!(
            "quote the words of one search, as in `{}`, or add one dataset at a time",
            super::project::command(&format!("add \"{example}\""))
        )));
    }
    let mut seen: HashMap<(String, Option<String>, Option<String>), String> = HashMap::new();
    let mut checked = Vec::with_capacity(links.len());
    for (index, link) in links.into_iter().enumerate() {
        let shown = net::mask_query(&link).into_owned();
        if local::is_path(&link) {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{shown} is a path on this machine, and only links, DOIs and ids add together"
                ),
            )
            .help(one_at_a_time()));
        }
        let parsed = Reference::parse(&link)?;
        parsed
            .check_shareable()
            .map_err(|error| on_line(error, &shown))?;
        match seen.entry(same_data(&parsed)) {
            Entry::Occupied(first) => {
                let message = if *first.get() == shown {
                    format!("{shown} is given twice")
                } else {
                    format!("{shown} names the same data as {}", first.get())
                };
                return Err(Error::new(Kind::Usage, message).help("keep one of the two"));
            }
            Entry::Vacant(free) => {
                free.insert(shown);
            }
        }
        let line = Line {
            number: index + 1,
            link,
            sha256: None,
        };
        checked.push((line, parsed));
    }
    Ok(checked)
}

/// what two links that name the same data share: the reference and the file or folder in it.
fn same_data(parsed: &Parsed) -> (String, Option<String>, Option<String>) {
    (
        parsed.reference.to_string(),
        parsed.file.clone(),
        parsed.folder.clone(),
    )
}

fn not_a_link(name: &str, number: usize, line: &str) -> Error {
    let line = net::mask_query(line);
    let words: Vec<String> = line
        .split_whitespace()
        .map(|word| super::shell_word(word).into_owned())
        .collect();
    Error::new(
        Kind::Usage,
        format!("line {number} of {name} is not a link, a DOI or an id"),
    )
    .detail(line.clone().into_owned())
    .help(format!(
        "find its id with `fetchloom search {}`, then list that instead",
        words.join(" ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IRIS: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_sha256_is_read_as_copied_and_another_hash_is_named() {
        for text in [
            IRIS.to_owned(),
            format!("sha256:{IRIS}"),
            format!(" SHA256:{} ", IRIS.to_uppercase()),
        ] {
            assert_eq!(
                sha256(&text).map(|digest| digest.hex()),
                Ok(IRIS.to_owned())
            );
        }
        for (text, said) in [
            (
                "d41d8cd98f00b204e9800998ecf8427e",
                "this looks like an MD5, which has 32 hex digits. give the file's SHA-256, which has 64",
            ),
            (
                "md5:d41d8cd98f00b204e9800998ecf8427e",
                "this looks like an MD5, which has 32 hex digits. give the file's SHA-256, which has 64",
            ),
            (
                "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                "this looks like a SHA-1, which has 40 hex digits. give the file's SHA-256, which has 64",
            ),
            ("abc", "a SHA-256 is 64 hex digits"),
        ] {
            assert_eq!(
                sha256(text).map(|digest| digest.hex()),
                Err(said.to_owned())
            );
        }
    }

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

    /// anyone could have written the list, so the search its help names keeps each word of the
    /// line one literal word for a shell.
    #[test]
    fn a_line_of_search_words_names_a_search_a_shell_reads_as_written() {
        let error = parse("links.txt", "iris $(id) a;b&c it's\n").unwrap_err();
        let help = error.help.unwrap_or_default();
        let search = help
            .strip_prefix("find its id with `")
            .and_then(|rest| rest.strip_suffix("`, then list that instead"))
            .unwrap_or_default();
        assert_eq!(
            crate::suggested::command_words(search, cfg!(windows)),
            Some(
                ["fetchloom", "search", "iris", "$(id)", "a;b&c", "it's"]
                    .map(str::to_owned)
                    .to_vec()
            ),
            "{help}"
        );
    }

    /// a secret is refused only in a list for a project, whose data.toml would keep it, so a
    /// list for get is refused at its next bad line instead.
    #[test]
    fn a_list_is_refused_at_its_first_unusable_line_with_any_secret_masked() {
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
    fn typed_links_are_checked_and_named_by_their_links() {
        let typed = |links: &[&str]| typed(links.iter().map(|link| (*link).to_owned()).collect());
        let search = "quote the words of one search, as in `fetchloom add \"fashion mnist\"`, or add one dataset at a time";
        for (links, kind, message, help) in [
            (
                &["fashion", "mnist"][..],
                Kind::Usage,
                "\"fashion\" is not a link, a DOI or an id",
                Some(search),
            ),
            (
                &["zenodo:1", "iris"],
                Kind::Usage,
                "\"iris\" is not a link, a DOI or an id",
                Some(search),
            ),
            (
                &["zenodo:1", "./flowers"],
                Kind::Usage,
                "./flowers is a path on this machine, and only links, DOIs and ids add together",
                Some(
                    "add files on this machine one file or folder at a time, as in `fetchloom add ./flowers`",
                ),
            ),
            (
                &["zenodo:3608990", "https://zenodo.org/records/3608990"],
                Kind::Usage,
                "https://zenodo.org/records/3608990 names the same data as zenodo:3608990",
                Some("keep one of the two"),
            ),
            (
                &["zenodo:1", "zenodo:1"],
                Kind::Usage,
                "zenodo:1 is given twice",
                Some("keep one of the two"),
            ),
            (
                &["zenodo:1", "https://example.org/x.csv?token=hunter2"],
                Kind::ReferenceInvalid,
                "https://example.org/x.csv?token=***: this link carries token, a secret that data.toml and data.lock would keep and share",
                Some(
                    "use a link that works without it, or download the file once with `fetchloom get`",
                ),
            ),
        ] {
            let error = typed(links).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str(), error.help.as_deref()),
                (kind, message, help),
                "{links:?}"
            );
        }
        let lines = typed(&["zenodo:1", "https://example.org/x.csv"]).unwrap();
        assert_eq!(
            lines
                .iter()
                .map(|(line, _)| (line.number, line.link.as_str()))
                .collect::<Vec<_>>(),
            [(1, "zenodo:1"), (2, "https://example.org/x.csv")]
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
