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

use std::fmt;
use std::path::{Path, PathBuf};

use url::Url;

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
}
