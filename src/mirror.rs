//! mirrors: folders, web servers and S3 buckets standing in for a file's source, holding pinned
//! files under their SHA-256. a mirror is never trusted: the cache keeps only what matches its pin.
//! each lays files out alike:
//!
//! ```text
//! fetchloom-mirror           first line "fetchloom mirror 1"
//! objects/<2 hex>/<62 hex>   a pinned file, named by its SHA-256
//! trees/<2 hex>/<62 hex>     an archive's file list, named by the SHA-256 of its bytes
//! ```

pub(crate) mod folder;
pub(crate) mod s3;
pub(crate) mod web;

use std::fmt;
use std::path::{Path, PathBuf};

use url::Url;

use crate::digest::Sha256;

/// the file at the top of every mirror, which tells a mirror from an empty or unmounted folder.
pub(crate) const MARKER: &str = "fetchloom-mirror";
/// the first line of that file.
const MARKER_LINE: &str = "fetchloom mirror 1";

/// whether `text`, what a marker file holds, starts as a mirror's marker does.
fn is_marker(text: &str) -> bool {
    text.lines().next().map(str::trim) == Some(MARKER_LINE)
}
/// the largest tree read from a mirror, about 600,000 files. a bigger archive comes as itself.
const LARGEST_TREE: u64 = 64 * 1024 * 1024;
pub(crate) const OBJECTS: &str = "objects";
pub(crate) const TREES: &str = "trees";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    Folder(PathBuf),
    /// an http or https address, ending in `/`.
    Web(Url),
    /// a key prefix in an S3 bucket, as `s3://lab-data/fetchloom`.
    S3(s3::Top),
}

impl Location {
    /// reads a mirror as a person writes it: text with `s3:` is a bucket, as the `s3_buckets`
    /// setting writes one, then text with `://` is a web address and anything else a folder, as
    /// `C:\lab`, a relative one from `base` when given. a [`Refusal`] says what to do.
    pub(crate) fn parse(text: &str, base: Option<&Path>) -> Result<Location, Refusal> {
        let text = text.trim();
        if text
            .get(..3)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("s3:"))
        {
            return s3::Top::parse(text).map(Location::S3);
        }
        if !text.contains("://") {
            let folder = PathBuf::from(text);
            return Ok(Location::Folder(match base {
                Some(base) if folder.is_relative() => base.join(folder),
                _ => folder,
            }));
        }
        let url = Url::parse(text).map_err(|_| Refusal {
            reason: "it is no valid address".to_owned(),
            instead: "write an http or https address, such as https://data.lab.example/fetchloom, or a folder"
                .to_owned(),
        })?;
        match url.scheme() {
            "http" | "https" => {}
            "file" => {
                return Err(Refusal {
                    reason: "a file:// address names a folder".to_owned(),
                    instead: "write the folder path itself".to_owned(),
                });
            }
            scheme => {
                return Err(Refusal {
                    reason: format!(
                        "a mirror is a folder, an http or https address or an S3 bucket, not {scheme}"
                    ),
                    instead: "write an http or https address, s3://bucket/prefix, or a folder"
                        .to_owned(),
                });
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            // a sign-in is kept for its host and port, as `auth login` takes them
            let host = match url.port() {
                Some(port) => format!("{}:{port}", crate::net::host(&url)),
                None => crate::net::host(&url).to_owned(),
            };
            let sign_in = crate::account::Account::Login(host).command();
            return Err(Refusal {
                reason: "it holds a login".to_owned(),
                instead: format!("write the address without it, and sign in with `{sign_in}`"),
            });
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Refusal {
                reason: "it holds a query or a fragment".to_owned(),
                instead: "write the address of the mirror's top folder alone".to_owned(),
            });
        }
        let mut base = url;
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        Ok(Location::Web(base))
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Folder(root) => write!(f, "{}", root.display()),
            Location::Web(base) => f.write_str(base.as_str().trim_end_matches('/')),
            Location::S3(top) => top.fmt(f),
        }
    }
}

/// why some text is no mirror, and what to write instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) reason: String,
    pub(crate) instead: String,
}

/// the mirrors a run asks, in order, and whether it may ask nothing else.
#[derive(Clone, Debug, Default)]
pub(crate) struct Mirrors {
    pub(crate) list: Vec<Location>,
    /// how many of `list`, from its start, the command line named.
    pub(crate) flagged: usize,
    /// requests go to mirrors alone, as the `mirrors_only` setting asks.
    pub(crate) only: bool,
}

impl Mirrors {
    /// the mirrors `given` on the command line, then those the settings list, each once.
    pub(crate) fn new(given: Vec<Location>, listed: &[Location], only: bool) -> Mirrors {
        let mut list: Vec<Location> = Vec::with_capacity(given.len() + listed.len());
        for location in given {
            if !list.contains(&location) {
                list.push(location);
            }
        }
        let flagged = list.len();
        for location in listed {
            if !list.contains(location) {
                list.push(location.clone());
            }
        }
        Mirrors {
            list,
            flagged,
            only,
        }
    }

    pub(crate) fn web(&self) -> impl Iterator<Item = &Url> {
        self.list.iter().filter_map(|location| match location {
            Location::Web(base) => Some(base),
            Location::Folder(_) | Location::S3(_) => None,
        })
    }

    /// the buckets of the S3 mirrors, which a person named, so their keys may sign for them.
    pub(crate) fn buckets(&self) -> impl Iterator<Item = &crate::net::s3::Bucket> {
        self.list.iter().filter_map(|location| match location {
            Location::S3(top) => Some(top.bucket()),
            Location::Folder(_) | Location::Web(_) => None,
        })
    }

    /// whether any mirror is asked over the network: a web server or a bucket.
    pub(crate) fn any_remote(&self) -> bool {
        self.list
            .iter()
            .any(|location| !matches!(location, Location::Folder(_)))
    }
}

/// the path below a mirror's top where it keeps `digest` under `kind`, as the module doc lays out.
pub(crate) fn below(kind: &str, digest: &Sha256) -> [String; 3] {
    let hex = digest.hex();
    let (fan, rest) = hex.split_at(2);
    [kind.to_owned(), fan.to_owned(), rest.to_owned()]
}

/// a pinned file wanted from the mirrors: its SHA-256, its size, and the name a person knows it
/// by, such as `pets/images/cat_001.png`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wanted {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_parse_folders_and_web_addresses_and_refuse_the_rest() {
        let base = Path::new("/home/ada/.config/fetchloom");
        assert_eq!(
            Location::parse("/mnt/lab/fetchloom", Some(base)),
            Ok(Location::Folder(PathBuf::from("/mnt/lab/fetchloom")))
        );
        assert_eq!(
            Location::parse("mirror", Some(base)),
            Ok(Location::Folder(base.join("mirror")))
        );
        assert_eq!(
            Location::parse("mirror", None),
            Ok(Location::Folder(PathBuf::from("mirror")))
        );
        assert_eq!(
            Location::parse(r"C:\lab", None),
            Ok(Location::Folder(PathBuf::from(r"C:\lab")))
        );
        let web = Location::parse(" https://data.corp.internal/fetchloom ", None).unwrap();
        assert_eq!(web.to_string(), "https://data.corp.internal/fetchloom");
        let digest = crate::digest::sha256(b"pets");
        let hex = digest.hex();
        assert_eq!(below("objects", &digest), ["objects", &hex[..2], &hex[2..]]);
        for (text, instead) in [
            ("file:///mnt/lab", "write the folder path itself"),
            (
                "ftp://lab.example/m",
                "write an http or https address, s3://bucket/prefix, or a folder",
            ),
            (
                "https://ada:secret@lab.example/m",
                "write the address without it, and sign in with `fetchloom auth login lab.example`",
            ),
            // a login is kept for its host and port, and a host goes quoted for a shell
            (
                "https://ada:secret@lab.example:8443/m",
                "write the address without it, and sign in with `fetchloom auth login lab.example:8443`",
            ),
            (
                "https://ada:secret@x$(id).example/m",
                // windows marks the host for a placeholder, since cmd would run what it holds
                if cfg!(windows) {
                    "write the address without it, and sign in with `fetchloom auth login \u{f8e0}nx$(id).example\u{f8e1}`"
                } else {
                    "write the address without it, and sign in with `fetchloom auth login 'x$(id).example'`"
                },
            ),
            (
                "https://lab.example/m?token=abc",
                "write the address of the mirror's top folder alone",
            ),
        ] {
            let refusal = Location::parse(text, None).unwrap_err();
            assert_eq!(refusal.instead, instead, "{text}");
            assert!(!refusal.reason.contains("secret"), "{}", refusal.reason);
        }
    }

    #[test]
    fn mirrors_on_the_command_line_come_before_the_settings_and_each_comes_once() {
        let folder = |name: &str| Location::Folder(PathBuf::from(name));
        let mirrors = Mirrors::new(
            vec![folder("/usb"), folder("/nas")],
            &[folder("/nas"), folder("/home/lab")],
            false,
        );
        assert_eq!(
            mirrors.list,
            [folder("/usb"), folder("/nas"), folder("/home/lab")]
        );
        assert_eq!(mirrors.flagged, 2);
    }
}
