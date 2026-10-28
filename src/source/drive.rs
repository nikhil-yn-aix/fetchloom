//! google drive: shared files and folders, pasted as links. a file is fetched through the `uc`
//! endpoint, where large files first send a confirmation page.

use std::fmt;

use url::Url;

use super::{Listing, Reference, Remote, malformed};
use crate::error::Error;
use crate::net::{Api, Divert};

/// a shared drive file or folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    id: String,
    folder: bool,
}

impl Item {
    /// reads the part after `drive:`: a file id, or `folders/` and a folder id.
    pub(super) fn parse(text: &str) -> Result<Item, Error> {
        let text = text.trim();
        let (id, folder) = match text.strip_prefix("folders/") {
            Some(id) => (id, true),
            None => (text, false),
        };
        if !is_id(id) {
            return Err(malformed(
                &format!("drive:{text}"),
                "the link drive gives when you share a file or folder",
            ));
        }
        Ok(Item {
            id: id.to_owned(),
            folder,
        })
    }

    /// a pasted drive link, in any of the shapes drive hands out.
    pub(super) fn from_link(url: &Url) -> Option<Item> {
        if !matches!(
            url.host_str()?,
            "drive.google.com" | "docs.google.com" | "drive.usercontent.google.com"
        ) {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let query_id = || {
            url.query_pairs()
                .find(|(name, _)| name == "id")
                .map(|(_, value)| value.into_owned())
        };
        let (id, folder) = match parts.as_slice() {
            ["file", "d", id, ..] => ((*id).to_owned(), false),
            ["drive", "folders", id, ..] | ["drive", "u", _, "folders", id, ..] => {
                ((*id).to_owned(), true)
            }
            ["folderview"] => (query_id()?, true),
            ["open" | "uc" | "download"] => (query_id()?, false),
            _ => return None,
        };
        is_id(&id).then_some(Item { id, folder })
    }

    /// a file is one download whose name the server sends. a folder lists its files.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "folders arrive with the drive milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        if self.folder {
            return Err(super::not_yet("drive folder"));
        }
        let url = Url::parse(&format!("https://drive.google.com/uc?id={}", self.id))
            .map_err(|err| malformed(&format!("drive:{}: {err}", self.id), "a drive link"))?;
        Ok(Listing {
            resolved: Reference::Drive(self.clone()),
            title: None,
            version: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "download".to_owned(),
                size: None,
                sha256: None,
                md5: None,
                url,
                named_by_server: true,
            }],
        })
    }
}

/// drive ids are long runs of letters, digits, `-` and `_`.
fn is_id(text: &str) -> bool {
    text.len() >= 10
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.folder {
            write!(f, "drive:folders/{}", self.id)
        } else {
            write!(f, "drive:{}", self.id)
        }
    }
}

/// where drive serves a file's bytes, and how to read the page it sends for large files.
pub(super) fn fetch_plan(_url: &Url) -> Option<(Url, Option<Divert>)> {
    None
}
