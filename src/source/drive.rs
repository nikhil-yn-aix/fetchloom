//! google drive: shared files and folders, pasted as links. a file downloads from drive's
//! download host, which may first send a page that confirms the download or refuses it.

mod folder;
mod html;
mod link;
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
    kind: ItemKind,
    resource: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemKind {
    File,
    Folder,
    Open,
}

impl Item {
    /// reads a file, folder or unresolved open reference.
    pub(super) fn parse(text: &str) -> Result<Item, Error> {
        let text = text.trim();
        let bad = || {
            malformed(
                &format!("drive:{text}"),
                "the link drive gives when you share a file or folder",
            )
        };
        let (name, query) = text
            .split_once('?')
            .map_or((text, None), |(name, query)| (name, Some(query)));
        let (id, kind) = if let Some(id) = name.strip_prefix("folders/") {
            (id, ItemKind::Folder)
        } else if let Some(id) = name.strip_prefix("open/") {
            (id, ItemKind::Open)
        } else {
            (name, ItemKind::File)
        };
        if !is_id(id) {
            return Err(bad());
        }
        let resource = match query {
            None => None,
            Some(query) => {
                let mut pairs = url::form_urlencoded::parse(query.as_bytes());
                match (pairs.next(), pairs.next()) {
                    (Some((key, value)), None) if key == "resourcekey" && !value.is_empty() => {
                        Some(value.into_owned())
                    }
                    _ => return Err(bad()),
                }
            }
        };
        Ok(Item {
            id: id.to_owned(),
            kind,
            resource,
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
        let query = |key: &str| {
            url.query_pairs()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.into_owned())
        };
        let (id, kind) = match parts.as_slice() {
            ["file", "d", id, ..] => ((*id).to_owned(), ItemKind::File),
            ["drive", "folders", id, ..] | ["drive", "u", _, "folders", id, ..] => {
                ((*id).to_owned(), ItemKind::Folder)
            }
            ["folderview" | "embeddedfolderview"] => (query("id")?, ItemKind::Folder),
            ["open"] => (query("id")?, ItemKind::Open),
            ["uc" | "download"] => (query("id")?, ItemKind::File),
            _ => return None,
        };
        let resource = query("resourcekey").filter(|key| !key.is_empty());
        is_id(&id).then_some(Item { id, kind, resource })
    }

    /// a file is one download whose name the server sends. a folder lists its files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let opened = if self.kind == ItemKind::Open {
            Some(self.opened(api).await?)
        } else {
            None
        };
        let item = opened.as_ref().unwrap_or(self);
        if item.kind == ItemKind::Folder {
            return folder::list(item, api).await;
        }
        Ok(Listing {
            unavailable: Vec::new(),
            resolved: Reference::Drive(item.clone()),
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "download".to_owned(),
                size: None,
                sha256: None,
                md5: None,
                url: file_url(&item.id, item.resource.as_deref())?,
                named_by_server: true,
            }],
        })
    }
}

/// the lock's file identity, separate from the download host and its confirmation fields.
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
        let prefix = match self.kind {
            ItemKind::File => "",
            ItemKind::Folder => "folders/",
            ItemKind::Open => "open/",
        };
        write!(f, "drive:{prefix}{}", self.id)?;
        if let Some(resource) = &self.resource {
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("resourcekey", resource)
                .finish();
            write!(f, "?{query}")?;
        }
        Ok(())
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

#[cfg(test)]
mod link_tests;
