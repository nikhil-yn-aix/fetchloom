//! mirrors: folders and web servers that hold pinned files under their SHA-256, so a lab disk, a
//! usb drive or a company server can stand in for a file's source. a mirror is never trusted:
//! every file and tree it gives is checked against its pin before the cache keeps it.
//!
//! a mirror is laid out the same on a disk and on a web server:
//!
//! ```text
//! fetchloom-mirror           first line "fetchloom mirror 1"
//! objects/<2 hex>/<62 hex>   a pinned file, named by its SHA-256
//! trees/<2 hex>/<62 hex>     an archive's file list, named by the SHA-256 of its bytes
//! ```

pub(crate) mod folder;

use std::fmt;
use std::path::{Path, PathBuf};

use url::Url;

use crate::digest::Sha256;

/// the file at the top of every mirror, which tells a mirror from an empty or unmounted folder.
pub(crate) const MARKER: &str = "fetchloom-mirror";
/// the first line of that file.
pub(crate) const MARKER_LINE: &str = "fetchloom mirror 1";
/// the largest tree read from a mirror, about 600,000 files. a bigger archive comes as itself.
pub(crate) const LARGEST_TREE: u64 = 64 * 1024 * 1024;

/// where a mirror is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Location {
    Folder(PathBuf),
    /// an http or https address, ending in `/`.
    Web(Url),
}

impl Location {
    /// reads a mirror as a person writes it: text holding `://` is a web address and anything
    /// else a folder, so `C:\lab` is a folder. a relative folder is taken from `base` when one is
    /// given, as the settings file's folder is for the file's mirrors.
    ///
    /// # Errors
    ///
    /// why the text is no mirror, with what to write instead, for a usage error to carry.
    pub(crate) fn parse(text: &str, base: Option<&Path>) -> Result<Location, Refusal> {
        let text = text.trim();
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
                        "a mirror is a folder or an http or https address, not {scheme}"
                    ),
                    instead: "write an http or https address, or a folder".to_owned(),
                });
            }
        }
        if !url.username().is_empty() || url.password().is_some() {
            let host = url.host_str().unwrap_or("the host");
            return Err(Refusal {
                reason: "it holds a login".to_owned(),
                instead: format!(
                    "write the address without it, and sign in with `fetchloom auth login {host}`"
                ),
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
    /// requests go to mirrors alone, as the `mirrors_only` setting asks.
    pub(crate) only: bool,
}

impl Mirrors {
    /// the mirrors `given` on the command line, then those the settings list, each once.
    pub(crate) fn new(given: Vec<Location>, listed: &[Location], only: bool) -> Mirrors {
        let mut list: Vec<Location> = Vec::with_capacity(given.len() + listed.len());
        for location in given.into_iter().chain(listed.iter().cloned()) {
            if !list.contains(&location) {
                list.push(location);
            }
        }
        Mirrors { list, only }
    }
}

/// where a mirror keeps a file or a tree, below its top: `objects` or `trees`, then the first two
/// hex digits of `digest`, then the other 62.
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
                "write an http or https address, or a folder",
            ),
            (
                "https://ada:secret@lab.example/m",
                "write the address without it, and sign in with `fetchloom auth login lab.example`",
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
    }
}
