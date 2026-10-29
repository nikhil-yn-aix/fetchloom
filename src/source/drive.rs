//! google drive: shared files and folders, pasted as links. a file downloads from drive's
//! download host, which may first send a page that confirms the download or refuses it.

mod folder;
mod html;
mod page;

use std::fmt;

use url::Url;

use super::{Listing, Reference, Remote, malformed};
use crate::error::{Error, Kind};
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
            ["folderview" | "embeddedfolderview"] => (query_id()?, true),
            ["open" | "uc" | "download"] => (query_id()?, false),
            _ => return None,
        };
        is_id(&id).then_some(Item { id, folder })
    }

    /// a file is one download whose name the server sends. a folder lists its files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        if self.folder {
            return folder::list(self, api).await;
        }
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
                url: file_url(&self.id, None)?,
                named_by_server: true,
            }],
        })
    }
}

/// the link data.lock keeps for a drive file. drive's `uc` link has outlived the hosts that serve
/// the bytes, so only `fetch_plan` names those.
fn file_url(id: &str, resource: Option<&str>) -> Result<Url, Error> {
    let mut url = Url::parse("https://drive.google.com/uc").map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make a drive link: {err}"),
        )
    })?;
    url.query_pairs_mut().append_pair("id", id);
    if let Some(resource) = resource {
        url.query_pairs_mut().append_pair("resourcekey", resource);
    }
    Ok(url)
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

/// where drive serves the bytes of a file known by its `uc` link, and how to read the pages it
/// sends instead.
pub(super) fn fetch_plan(url: &Url) -> Option<(Url, Option<Divert>)> {
    if url.host_str() != Some("drive.google.com") || url.path() != "/uc" {
        return None;
    }
    let mut from = Url::parse("https://drive.usercontent.google.com/download").ok()?;
    from.query_pairs_mut()
        .extend_pairs(url.query_pairs())
        .append_pair("export", "download")
        // skips the form drive sends before a file too large to scan for viruses
        .append_pair("confirm", "t");
    Some((from, Some(page::divert)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_download_from_drives_download_host() {
        let id = "1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb";
        let known = Url::parse(&format!("https://drive.google.com/uc?id={id}")).unwrap();
        let (from, divert) = fetch_plan(&known).unwrap();
        assert_eq!(
            from.as_str(),
            format!(
                "https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t"
            )
        );
        assert!(divert.is_some());
        let elsewhere = Url::parse(&format!("https://example.org/uc?id={id}")).unwrap();
        assert!(fetch_plan(&elsewhere).is_none());
    }
}

#[cfg(test)]
mod folder_tests;
