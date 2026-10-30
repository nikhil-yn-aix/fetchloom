//! `data.lock`: what each dataset resolved to and every file it downloaded. only fetchloom writes
//! it, in a fixed order, so the same inputs always give the same bytes and a diff shows exactly
//! what changed.

mod canonical;

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer};

use super::manifest::Entry;
use crate::digest::{self, Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;

pub(crate) const FILE: &str = "data.lock";
const VERSION: u32 = 1;
/// how to recover a data.lock this fetchloom cannot use. `fetchloom lock` alone reads the same
/// file first and fails the same way.
pub(crate) const BROKEN: &str = "restore data.lock from git, or delete it and run `fetchloom lock`, which resolves every dataset again and may pin newer data";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Lock {
    /// sorted by name.
    pub(crate) datasets: Vec<Locked>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Locked {
    pub(crate) name: String,
    #[serde(rename = "ref")]
    pub(crate) reference: String,
    /// the hash of the data.toml keys that decide what is downloaded. while it matches, the
    /// dataset is not resolved again.
    pub(crate) spec: String,
    pub(crate) resolved: String,
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
    /// when the pinned data last changed, in UTC.
    #[serde(rename = "locked")]
    pub(crate) timestamp: String,
    /// sorted by path.
    pub(crate) files: Vec<LockedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LockedFile {
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: Sha256,
    pub(crate) md5: Option<Md5>,
    pub(crate) url: String,
    /// for an archive, the tree hash of the files it unpacks to.
    #[serde(default, deserialize_with = "tree")]
    pub(crate) unpacks_to: Option<Sha256>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    #[serde(default)]
    dataset: Vec<Locked>,
}

#[derive(Deserialize)]
struct VersionOnly {
    version: Option<u32>,
}

impl Lock {
    pub(crate) fn parse(text: &str) -> Result<Lock, Error> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if let Some(lock) = canonical::read(text) {
            return Ok(lock);
        }
        match toml::from_str::<Stored>(text) {
            Ok(stored) if stored.version == VERSION => {
                let mut datasets = stored.dataset;
                datasets.sort_by(|a, b| a.name.cmp(&b.name));
                for dataset in &mut datasets {
                    dataset.files.sort_by(|a, b| a.path.cmp(&b.path));
                    check_paths(dataset)?;
                }
                Ok(Lock { datasets })
            }
            Ok(stored) => Err(other_version(stored.version)),
            Err(err) => match toml::from_str::<VersionOnly>(text) {
                Ok(VersionOnly {
                    version: Some(version),
                }) if version != VERSION => Err(other_version(version)),
                _ => Err(Error::new(
                    Kind::ProjectInvalid,
                    format!("data.lock cannot be read: {}", err.message()),
                )
                .help(BROKEN)),
            },
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Locked> {
        self.datasets.iter().find(|dataset| dataset.name == name)
    }

    /// the lock's text, the same bytes for the same contents.
    pub(crate) fn render(&self) -> String {
        let mut datasets: Vec<&Locked> = self.datasets.iter().collect();
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = format!("version = {VERSION}\n");
        for dataset in datasets {
            out.push_str("\n[[dataset]]\n");
            for (key, value) in [
                ("name", Some(&dataset.name)),
                ("ref", Some(&dataset.reference)),
                ("spec", Some(&dataset.spec)),
                ("resolved", Some(&dataset.resolved)),
                ("title", dataset.title.as_ref()),
                ("license", dataset.license.as_ref()),
                ("doi", dataset.doi.as_ref()),
                ("locked", Some(&dataset.timestamp)),
            ] {
                if let Some(value) = value {
                    let _ = writeln!(out, "{key} = {}", quoted(value));
                }
            }
            let mut files: Vec<&LockedFile> = dataset.files.iter().collect();
            files.sort_by(|a, b| a.path.cmp(&b.path));
            if files.is_empty() {
                out.push_str("files = []\n");
                continue;
            }
            out.push_str("files = [\n");
            for file in files {
                let _ = write!(
                    out,
                    "  {{ path = {}, size = {}, sha256 = \"{}\"",
                    quoted(&file.path),
                    file.size,
                    file.sha256
                );
                if let Some(md5) = file.md5 {
                    let _ = write!(out, ", md5 = \"{md5}\"");
                }
                let _ = write!(out, ", url = {}", quoted(&file.url));
                if let Some(tree) = file.unpacks_to {
                    let _ = write!(out, ", unpacks_to = \"sha256:{tree}\"");
                }
                out.push_str(" },\n");
            }
            out.push_str("]\n");
        }
        out
    }
}

/// the hash of everything in a data.toml entry that decides what is downloaded. `copy` and
/// `path` only change where files go, so they are left out.
pub(crate) fn spec(entry: &Entry) -> String {
    let mut text = format!("ref={}\n", entry.reference);
    for (key, globs) in [("select", &entry.select), ("exclude", &entry.exclude)] {
        for glob in globs.iter().map(Glob::as_str) {
            let _ = writeln!(text, "{key}={glob}");
        }
    }
    let _ = writeln!(text, "extract={}", entry.extract);
    format!("sha256:{}", digest::sha256(text.as_bytes()))
}

/// the current time as `data.lock` records it: `2026-10-27T17:30:00Z`.
pub(crate) fn timestamp(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // days since 1970 to a civil date, as in Howard Hinnant's chrono algorithms
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// a TOML basic string.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn other_version(version: u32) -> Error {
    let error = Error::new(
        Kind::ProjectInvalid,
        format!("data.lock is in format {version}, and this fetchloom reads format {VERSION}"),
    );
    // no fetchloom ever wrote an older format, so one is a broken file
    if version > VERSION {
        error.help("install a newer fetchloom, then run the same command again")
    } else {
        error.help(BROKEN)
    }
}

/// refuses a lock that would place a file outside its dataset's folder. fetchloom never writes
/// such a path, so the lock was edited or made by someone else.
fn check_paths(dataset: &Locked) -> Result<(), Error> {
    for file in &dataset.files {
        if let Some(reason) = name::escapes(&file.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "data.lock lists {} in {}, which fetchloom refuses because {reason}",
                    quoted(&file.path),
                    dataset.name
                ),
            )
            .help(BROKEN));
        }
    }
    Ok(())
}

fn tree<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Sha256>, D::Error> {
    let Some(text) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let digest = text.strip_prefix("sha256:").and_then(Sha256::from_hex);
    digest
        .map(Some)
        .ok_or_else(|| D::Error::custom("unpacks_to is sha256: and 64 hex digits"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::project::manifest::Manifest;

    fn sample() -> Lock {
        let file = |path: &str, bytes: &[u8]| LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to: None,
        };
        Lock {
            datasets: vec![
                Locked {
                    name: "zeta".to_owned(),
                    reference: "https://example.org/z.csv".to_owned(),
                    spec: "sha256:00".to_owned(),
                    resolved: "https://example.org/z.csv".to_owned(),
                    title: None,
                    license: None,
                    doi: None,
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    files: vec![file("z.csv", b"z")],
                },
                Locked {
                    name: "iris".to_owned(),
                    reference: "zenodo:3608990".to_owned(),
                    spec: "sha256:11".to_owned(),
                    resolved: "zenodo:3608990".to_owned(),
                    title: Some("Iris \"classic\"\tset".to_owned()),
                    license: Some("CC-BY-4.0".to_owned()),
                    doi: Some("10.5281/zenodo.3608990".to_owned()),
                    timestamp: "2026-10-27T17:30:00Z".to_owned(),
                    files: vec![
                        LockedFile {
                            md5: Md5::from_hex("900150983cd24fb0d6963f7d28e17f72"),
                            unpacks_to: Some(digest::sha256(b"tree")),
                            ..file("b/iris.zip", b"abc")
                        },
                        file("a.csv", b"a"),
                    ],
                },
            ],
        }
    }

    #[test]
    fn the_same_contents_always_give_the_same_bytes() {
        let text = sample().render();
        assert!(
            text.starts_with("version = 1\n\n[[dataset]]\nname = \"iris\"\n"),
            "{text}"
        );
        assert!(
            text.contains("title = \"Iris \\\"classic\\\"\\tset\"\n"),
            "{text}"
        );
        assert!(
            text.find("path = \"a.csv\"") < text.find("path = \"b/iris.zip\""),
            "files are sorted"
        );
        assert!(text.contains(", md5 = \"900150983cd24fb0d6963f7d28e17f72\", url = \"https://example.org/b/iris.zip\", unpacks_to = \"sha256:"), "{text}");
        let read = Lock::parse(&text).unwrap();
        let mut sorted = sample();
        sorted.datasets.sort_by(|a, b| a.name.cmp(&b.name));
        for dataset in &mut sorted.datasets {
            dataset.files.sort_by(|a, b| a.path.cmp(&b.path));
        }
        assert_eq!(read, sorted);
        assert_eq!(
            read.render(),
            text,
            "reading and writing again changes nothing"
        );
    }

    #[test]
    fn another_format_or_a_broken_file_is_refused_with_the_fix() {
        let newer = Lock::parse("version = 2\n[[dataset]]\nwhatever = 1\n").unwrap_err();
        assert!(newer.message.contains("format 2"), "{}", newer.message);
        assert_eq!(
            newer.help.as_deref(),
            Some("install a newer fetchloom, then run the same command again")
        );
        for text in ["version = 0\n", "version = 1\n[[dataset]]\nname = 3\n"] {
            let broken = Lock::parse(text).unwrap_err();
            assert_eq!(broken.kind, Kind::ProjectInvalid);
            assert_eq!(broken.help.as_deref(), Some(BROKEN), "{text}");
        }
        assert!(
            BROKEN.contains("delete it and run `fetchloom lock`, which resolves every dataset")
        );
        assert_eq!(Lock::parse("version = 1\n").unwrap(), Lock::default());
    }

    #[test]
    fn a_path_that_leaves_its_folder_is_refused() {
        for path in [
            "../../outside.txt",
            "a/../../b",
            "..\\\\..\\\\outside.txt",
            "/etc/passwd",
            "C:/Windows/x",
            "a//b",
        ] {
            let text = format!(
                "version = 1\n[[dataset]]\nname = \"x\"\nref = \"https://example.org/x\"\nspec = \"sha256:00\"\nresolved = \"https://example.org/x\"\nlocked = \"2026-10-27T17:30:00Z\"\nfiles = [\n  {{ path = \"{path}\", size = 1, sha256 = \"{}\", url = \"https://example.org/x\" }},\n]\n",
                digest::sha256(b"x")
            );
            let err = Lock::parse(&text).unwrap_err();
            assert_eq!(err.kind, Kind::ProjectInvalid, "{path}");
            assert!(err.message.contains("data.lock"), "{}", err.message);
            assert_eq!(err.help.as_deref(), Some(BROKEN), "{path}");
        }
    }

    #[test]
    fn the_spec_follows_what_decides_the_download() {
        let entry = |extra: &str| {
            Manifest::parse(&format!("[datasets.x]\nref = \"zenodo:1\"\n{extra}"))
                .unwrap()
                .datasets["x"]
                .clone()
        };
        let plain = spec(&entry(""));
        assert_eq!(
            plain,
            spec(&entry("copy = true\npath = \"elsewhere\"\n")),
            "copy and path do not change what is downloaded"
        );
        assert_ne!(plain, spec(&entry("select = \"*.csv\"\n")));
        assert_ne!(plain, spec(&entry("extract = false\n")));
        assert!(plain.starts_with("sha256:") && plain.len() == 71);
    }

    #[test]
    #[expect(
        clippy::duration_suboptimal_units,
        reason = "unix timestamps count seconds"
    )]
    fn timestamps_are_utc_and_exact() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_793_122_200)),
            "2026-10-27T17:30:00Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        assert_eq!(quoted("a\u{1}b\u{7f}"), "\"a\\u0001b\\u007F\"");
    }
}
