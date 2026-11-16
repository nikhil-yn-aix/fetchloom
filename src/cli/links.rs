//! links for one command: the list `get -i` reads, in the one form every list takes, and several
//! links typed for one `add` or `get`. a list is a file anyone could have written, so the whole
//! list is checked before any request.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{IsTerminal as _, Read as _};
use std::path::Path;

use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::output::Output;
use crate::source::list::{self, Checksum, Named};
use crate::source::search::is_search;
use crate::source::{Parsed, Reference, local};
use crate::stop::Stop;

/// the keys that end what a terminal's stdin gives.
#[cfg(not(windows))]
const ENDS: &str = "ctrl-d ends";
#[cfg(windows)]
const ENDS: &str = "ctrl-z, then enter, ends";

/// a link of a list `get -i` read, and the hash its line states for its one file.
#[derive(Debug)]
pub(super) struct Listed {
    pub(super) link: String,
    /// where the line is in its list, as `line 2 of links.txt`.
    pub(super) at: String,
    pub(super) checksum: Option<Checksum>,
}

/// the text of the list at `path`, or of stdin for `-`, and the name messages give it.
pub(super) async fn read(
    path: &Path,
    output: &Output,
    stop: &Stop,
) -> Result<(String, String), Error> {
    stop.check()?;
    if path == Path::new("-") && std::io::stdin().is_terminal() {
        output.awaiting(&format!("reading links from stdin, one per line; {ENDS}"));
    }
    let path = path.to_path_buf();
    let reading = tokio::task::spawn_blocking(move || read_text(&path, list::MOST_BYTES));
    tokio::select! {
        biased;
        () = stop.asked() => Err(Stop::stopped()),
        outcome = reading => outcome.map_err(|err| Error::new(
            Kind::LocalIo,
            format!("could not read the link list: {err}"),
        ))?,
    }
}

/// the list at `path`, or stdin for `-`, read up to `most` bytes, as a list may be no longer.
fn read_text(path: &Path, most: u64) -> Result<(String, String), Error> {
    let (name, reading): (String, Box<dyn std::io::Read>) = if path == Path::new("-") {
        ("stdin".to_owned(), Box::new(std::io::stdin()))
    } else {
        let name = crate::shown::mask_query(&path.display().to_string()).into_owned();
        let file = std::fs::File::open(path)
            .map_err(|err| Error::io(format_args!("could not read {name}"), &err))?;
        (name, Box::new(file))
    };
    let mut bytes = Vec::new();
    reading
        .take(most + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| Error::io(format_args!("could not read {name}"), &err))?;
    if bytes.len() as u64 > most {
        return Err(list::too_big(&name, most));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::new(Kind::Usage, format!("{name} is not UTF-8 text"))
            .help("save the list as UTF-8, then run the command again")
    })?;
    Ok((name, text))
}

/// the links the list `name` holds in `text`, at most `most`, each with the hash its line states:
/// none on this machine, and no two naming the same data. a path below a folder is refused, as a
/// list on this machine has no folder.
pub(super) fn listed(name: &str, text: &str, most: usize) -> Result<Vec<Listed>, Error> {
    let entries = list::read(name, text, most)?;
    if entries.is_empty() {
        return Err(
            Error::new(Kind::Usage, format!("{name} lists no links")).help(
                "put one link per line in it. blank lines and lines starting with # are skipped",
            ),
        );
    }
    let mut seen: HashMap<(String, Option<String>, Option<String>), usize> = HashMap::new();
    let mut listed = Vec::with_capacity(entries.len());
    for entry in entries {
        let at = entry.at(name);
        let link = match &entry.named {
            Named::Link(link)
            | Named::Path {
                link: Some(link), ..
            } => link.clone(),
            Named::Path { path, link: None } => {
                // the line as written, since a path on this machine lost its `./` as a path
                let written = text
                    .trim_start_matches('\u{feff}')
                    .lines()
                    .nth(entry.number - 1)
                    .and_then(|line| line.split_whitespace().next())
                    .unwrap_or(path);
                if local::is_path(written) {
                    return Err(on_this_machine(&at, written));
                }
                return Err(list::no_folder(name, &entry, path));
            }
        };
        if local::is_path(&link) {
            return Err(on_this_machine(&at, &link));
        }
        let parsed = Reference::parse(&link).map_err(|error| on_line(error, &at))?;
        match seen.entry(same_data(&parsed)) {
            Entry::Occupied(first) => {
                return Err(Error::new(
                    Kind::Usage,
                    format!("{at} names the same data as line {}", first.get()),
                )
                .help("keep one of the two lines"));
            }
            Entry::Vacant(free) => {
                free.insert(entry.number);
            }
        }
        listed.push(Listed {
            link,
            at,
            checksum: entry.checksum,
        });
    }
    Ok(listed)
}

/// the refusal of `written`, the path on this machine the line `at` names.
fn on_this_machine(at: &str, written: &str) -> Error {
    Error::new(
        Kind::ReferenceInvalid,
        format!("{at} is a path on this machine, and a list holds links, DOIs and ids"),
    )
    .detail(crate::shown::mask_query(written).into_owned())
    .help("give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990")
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

pub(super) fn on_line(mut error: Error, at: &str) -> Error {
    error.message = format!("{at}: {}", error.message);
    error
}

/// how to add files on this machine, which several links cannot.
fn one_at_a_time() -> String {
    format!(
        "add files on this machine one file or folder at a time, as in `{}`",
        super::project::command("add ./flowers")
    )
}

/// links typed for one add, each named by its own link: search words go alone, and a path on
/// this machine is added one at a time.
pub(super) fn typed(links: Vec<String>) -> Result<Vec<(String, Parsed)>, Error> {
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
    for link in links {
        let shown = crate::shown::mask_query(&link).into_owned();
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
        checked.push((link, parsed));
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

    /// a list for `get -i` gives each link with the MD5 or SHA-256 its line states, in any of the
    /// forms a list takes.
    #[test]
    fn a_list_gives_links_with_their_hashes() {
        let md5 = "d41d8cd98f00b204e9800998ecf8427e";
        let text = format!(
            "\u{feff}# measurements to compare\r\n\r\nzenodo:3608990\n  https://example.org/iris.csv   {IRIS}  \nhttps://example.org/tips.csv md5:{md5}\n{IRIS}  https://example.org/wine.csv\nnames.csv sha256:{IRIS} https://elsewhere.org/names.csv\n# done\n",
        );
        let got: Vec<(String, String, Option<Checksum>)> = listed("links.txt", &text, 10)
            .unwrap()
            .into_iter()
            .map(|listed| (listed.link, listed.at, listed.checksum))
            .collect();
        let sha = Sha256::from_hex(IRIS).map(Checksum::Sha256);
        assert_eq!(
            got,
            [
                (
                    "zenodo:3608990".to_owned(),
                    "line 3 of links.txt".to_owned(),
                    None
                ),
                (
                    "https://example.org/iris.csv".to_owned(),
                    "line 4 of links.txt".to_owned(),
                    sha
                ),
                (
                    "https://example.org/tips.csv".to_owned(),
                    "line 5 of links.txt".to_owned(),
                    crate::digest::Md5::from_hex(md5).map(Checksum::Md5)
                ),
                (
                    "https://example.org/wine.csv".to_owned(),
                    "line 6 of links.txt".to_owned(),
                    sha
                ),
                (
                    "https://elsewhere.org/names.csv".to_owned(),
                    "line 7 of links.txt".to_owned(),
                    sha
                ),
            ]
        );
        let error = listed("links.txt", &format!("{IRIS}  iris.csv\n"), 10).unwrap_err();
        assert_eq!(
            error.message,
            "line 1 of links.txt names \"iris.csv\", a path, and only a list at a link has a folder to read it below"
        );
    }

    /// anyone could have written the list, so the search its help names keeps each word of the
    /// line one literal word for a shell.
    #[test]
    fn a_line_of_search_words_names_a_search_a_shell_reads_as_written() {
        let error = listed("links.txt", "iris $(id) a;b&c it's\n", 10)
            .unwrap_err()
            .told();
        let help = error.help.as_deref().unwrap_or_default();
        let search = help
            .strip_prefix("find its id with `")
            .and_then(|rest| rest.strip_suffix("`, then list that instead"))
            .unwrap_or_default();
        // no command on windows holds `$` or `&` safely, so placeholders stand in for them there
        if cfg!(windows) {
            assert_eq!(
                search, "fetchloom search iris \"<name>\" \"<name 2>\" \"it's\"",
                "{help}"
            );
            for named in ["<name> is $(id), which", "<name 2> is a;b&c, which"] {
                assert!(
                    error.details.iter().any(|line| line.starts_with(named)),
                    "{:?}",
                    error.details
                );
            }
            return;
        }
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

    /// a list for get may hold a secret, which it masks, and is refused at its first bad line.
    #[test]
    fn a_list_is_refused_at_its_first_unusable_line_with_any_secret_masked() {
        let text = "https://example.org/iris.csv\nhttps://example.org/wine.csv?token=hunter2\nzenodo:not-a-number\n";
        let error = listed("links.txt", text, 10).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid);
        assert!(
            error.message.starts_with("line 3 of links.txt: "),
            "{error:?}"
        );
        assert!(!format!("{error:?}").contains("hunter2"), "{error:?}");
        for (text, kind, message) in [
            (
                "zenodo:3608990\nhttps://zenodo.org/records/3608990\n",
                Kind::Usage,
                "line 2 of links.txt names the same data as line 1",
            ),
            ("# nothing yet\n\n", Kind::Usage, "links.txt lists no links"),
        ] {
            let error = listed("links.txt", text, 10).unwrap_err();
            assert_eq!(
                (error.kind, error.message.as_str()),
                (kind, message),
                "{text}"
            );
        }
        let files =
            "https://zenodo.org/records/1/files/a.csv\nhttps://zenodo.org/records/1/files/b.csv\n";
        assert_eq!(
            listed("links.txt", files, 10).unwrap().len(),
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
        let links = typed(&["zenodo:1", "https://example.org/x.csv"]).unwrap();
        assert_eq!(
            links
                .iter()
                .map(|(link, _)| link.as_str())
                .collect::<Vec<_>>(),
            ["zenodo:1", "https://example.org/x.csv"]
        );
    }

    #[test]
    fn local_paths_and_file_links_are_refused_in_lists() {
        for link in ["./flowers", "file:///etc/hostname", "~/flowers.csv"] {
            let text = format!("zenodo:1\n{link}\n");
            let error = listed("links.txt", &text, 10).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{link}");
            assert_eq!(
                error.message,
                "line 2 of links.txt is a path on this machine, and a list holds links, DOIs and ids"
            );
            assert_eq!(error.details, [link]);
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990"
                ),
                "{link}"
            );
        }
    }

    /// a list longer than a list may be is refused before it is read whole.
    #[test]
    fn a_list_past_its_size_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("links.txt");
        std::fs::write(&path, "https://example.org/iris.csv\n").unwrap();
        let error = read_text(&path, 10).unwrap_err();
        assert!(
            error
                .message
                .ends_with("is larger than 10 B, the most fetchloom reads of a list"),
            "{error:?}"
        );
        assert!(read_text(&path, 100).is_ok());
    }
}
