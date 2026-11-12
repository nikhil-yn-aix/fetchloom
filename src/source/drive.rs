//! google drive: shared files and folders, pasted as links. a file downloads from drive's
//! download host, which may first send a page that confirms the download or refuses it. an item
//! shared with an account rather than with anyone who has the link is read through drive's api,
//! once drive refused it without a token, when the `drive_folders` setting names it.

mod folder;
mod html;
mod link;
mod page;
mod private;

use std::fmt;

use url::Url;

pub(crate) use self::private::{file, search};
use super::{Access, Listing, Reference, Remote, malformed};
use crate::error::{Error, Kind};
use crate::net::{Api, Divert};

/// where fetchloom's docs say how to let it read private drive items.
pub(crate) const GUIDE: &str =
    "https://nikhil-yn-aix.github.io/fetchloom/user/sources/google-drive.html";
/// the login that grants the drive scope, which google keeps gcloud's own oauth client from.
pub(crate) const LOGIN: &str = "`gcloud auth application-default login --client-id-file=CLIENT_FILE --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform`";

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

    /// a file is one download whose name the server sends. a folder lists its files, through
    /// drive's api when drive refused its page and `drive_folders` names it.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let opened = if self.kind == ItemKind::Open {
            match self.opened(api).await {
                Ok(opened) => Some(opened),
                Err(refused) if refused.kind == Kind::AuthRequired => {
                    return private::opened(self, api, refused).await;
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let item = opened.as_ref().unwrap_or(self);
        if item.kind == ItemKind::Folder {
            return match folder::list(item, api).await {
                Err(refused) if refused.kind == Kind::AuthRequired => {
                    private::refused(item, api, refused).await
                }
                listed => listed,
            };
        }
        item.lone()
    }

    /// a file alone, whose name its download tells.
    fn lone(&self) -> Result<Listing, Error> {
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Drive(self.clone()),
            title: None,
            license: None,
            doi: None,
            files: vec![Remote {
                path: "download".to_owned(),
                size: None,
                sha256: None,
                md5: None,
                url: file_url(&self.id, self.resource.as_deref())?,
                named_by_server: true,
                etag: None,
            }],
        })
    }
}

/// the id of a drive file known by its `uc` link, as data.lock pins drive files.
pub(crate) fn file_id(url: &Url) -> Option<String> {
    if url.host_str() != Some("drive.google.com") || url.path() != "/uc" {
        return None;
    }
    url.query_pairs()
        .find(|(key, _)| key == "id")
        .map(|(_, id)| id.into_owned())
        .filter(|id| is_id(id))
}

/// how to let fetchloom send google credentials to drive for the item `id`, which drive refused
/// to anyone with its link.
pub(crate) fn trust(id: &str) -> String {
    format!(
        "ask its owner to share it with anyone who has the link. if it is shared with your google account or a service account of yours, and you trust it with your google credentials, add {id} to drive_folders in the settings file or FETCHLOOM_DRIVE_FOLDERS"
    )
}

/// `refused`, the refusal of an item `drive_folders` names, when no google credentials were
/// found to ask drive's api with.
pub(crate) fn unsigned(mut refused: Error) -> Error {
    refused.message = format!(
        "{}, and no google application default credentials were found",
        refused.message
    );
    refused.help(format!(
        "set GOOGLE_APPLICATION_CREDENTIALS to the key file of a service account it is shared with, or sign in with an oauth client of your own: {LOGIN}. how: {GUIDE}"
    ))
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

/// a file or folder the `drive_folders` setting names, by its drive id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DriveId(String);

impl DriveId {
    /// an id as drive writes it, or the id of a drive link or `drive:` reference.
    pub(crate) fn parse(text: &str) -> Option<DriveId> {
        let text = text.trim();
        if is_id(text) {
            return Some(DriveId(text.to_owned()));
        }
        let item = match text.strip_prefix("drive:") {
            Some(rest) => Item::parse(rest).ok()?,
            None => Item::from_link(&Url::parse(text).ok()?)?,
        };
        Some(DriveId(item.id))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DriveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// drive ids are long runs of letters, digits, `-` and `_`.
pub(crate) fn is_id(text: &str) -> bool {
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
