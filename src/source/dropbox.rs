//! dropbox: shared links, fetched with `dl=1`. a shared folder arrives as one zip, which is
//! unpacked like any other archive.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Listing, Reference, Remote};
use crate::error::{Error, Kind};
use crate::net::Divert;

/// a dropbox shared link, kept as the link itself with `dl=1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    url: Url,
}

impl Link {
    /// a pasted shared link: `/s/`, `/sh/`, `/scl/fi/` or `/scl/fo/`.
    pub(super) fn from_link(url: &Url) -> Option<Link> {
        if !matches!(url.host_str()?, "dropbox.com" | "www.dropbox.com") {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        if !matches!(
            parts.as_slice(),
            ["s" | "sh", ..] | ["scl", "fi" | "fo", ..]
        ) {
            return None;
        }
        let mut link = url.clone();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(name, _)| !matches!(name.as_ref(), "dl" | "raw"))
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        link.query_pairs_mut()
            .clear()
            .extend_pairs(kept)
            .append_pair("dl", "1");
        link.set_fragment(None);
        Some(Link { url: link })
    }

    /// one download, named by the link until dropbox sends the file's real name.
    pub(super) fn listing(&self) -> Listing {
        let parts: Vec<&str> = self
            .url
            .path_segments()
            .map(|parts| parts.filter(|part| !part.is_empty()).collect())
            .unwrap_or_default();
        let guess = match parts.as_slice() {
            ["s", _, name, ..] | ["scl", "fi", _, name, ..] => {
                percent_decode_str(name).decode_utf8_lossy().into_owned()
            }
            _ => "download".to_owned(),
        };
        Listing {
            resolved: Reference::Dropbox(self.clone()),
            title: None,
            version: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: crate::name::sanitize(&guess).unwrap_or_else(|| "download".to_owned()),
                size: None,
                sha256: None,
                md5: None,
                url: self.url.clone(),
                named_by_server: true,
            }],
        }
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

/// a shared link can answer with a page instead of a download, which must not enter the cache.
pub(super) fn fetch_plan(url: &Url) -> Option<(Url, Option<Divert>)> {
    let link = Link::from_link(url)?;
    Some((link.url, Some(page)))
}

fn page(_url: &Url, status: u16, _text: &str) -> Result<Url, Error> {
    let error = match status {
        200 => Error::new(Kind::SourceInvalid, "dropbox sent an HTML page instead of the shared file or folder")
            .help("open the shared link in a browser and check that it allows downloads without signing in"),
        401 | 403 => Error::new(Kind::AuthRequired, "dropbox refused access to the shared file or folder")
            .help("ask its owner for a public link that allows downloads without signing in"),
        404 | 410 => Error::new(Kind::ReferenceUnresolved, "dropbox could not find the shared file or folder")
            .help("check the shared link in a browser, or ask its owner for a current link"),
        status => Error::new(Kind::NetworkStatus, format!("dropbox answered {status} instead of the shared file or folder"))
            .help("open the shared link in a browser to check why the download was refused"),
    };
    Err(error)
}

#[cfg(test)]
mod tests;
