//! drive items shared with an account rather than anyone with the link, read through drive's api
//! once drive refused them without a token. the network decides which requests carry the token.

use std::collections::{HashSet, VecDeque};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::{Item, ItemKind, file_url, folder, is_id, unsigned};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::net::{Api, Private, Response};
use crate::source::{
    Access, DriveId, Listing, Reference, Remote, Unavailable, most_files, too_many_of,
};

/// where drive's api reads files and lists folders.
const FILES: &str = "https://www.googleapis.com/drive/v3/files";
/// what drive's api says of each item: enough to name, size and pin a file.
const FIELDS: &str = "id,name,mimeType,size,md5Checksum,sha256Checksum";
const FOLDER: &str = "application/vnd.google-apps.folder";
const SHORTCUT: &str = "application/vnd.google-apps.shortcut";
/// the kinds of google docs, sheets, slides and the like, which drive keeps as no file.
const GOOGLE_APPS: &str = "application/vnd.google-apps.";

/// a drive item as drive's api describes it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct About {
    id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    mime_type: String,
    /// drive writes sizes as text.
    size: Option<String>,
    md5_checksum: Option<String>,
    sha256_checksum: Option<String>,
}

/// one page of a folder's items.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    #[serde(default)]
    files: Vec<About>,
    next_page_token: Option<String>,
    #[serde(default)]
    incomplete_search: bool,
}

/// the files found so far, and the folders still to list.
#[derive(Default)]
struct Walk {
    files: Vec<Remote>,
    unavailable: Vec<Unavailable>,
    folders: VecDeque<(String, String)>,
    seen: HashSet<String>,
    /// the most files, and the most folders, the walk takes in.
    most: usize,
}

/// folder `item`, whose page drive `refused` to anyone with its link: listed through drive's api
/// when the network may send it a token, else the refusal, saying what would let it.
pub(super) async fn refused(item: &Item, api: &impl Api, refused: Error) -> Result<Listing, Error> {
    match api.drive_refused(&item.id).await? {
        Private::Ready => list(item, api).await,
        Private::Unsigned => Err(unsigned(refused)),
        Private::Unlisted => Err(refused),
    }
}

/// what an open link names, once drive `refused` to say to anyone with the link: drive's api
/// tells whether it is a folder, to list, or a file, whose download names it.
pub(super) async fn opened(item: &Item, api: &impl Api, refused: Error) -> Result<Listing, Error> {
    match api.drive_refused(&item.id).await? {
        Private::Ready => {}
        Private::Unsigned => return Err(unsigned(refused)),
        Private::Unlisted => return Err(refused),
    }
    let about = about(api, &item.id).await?;
    let kind = if about.mime_type == FOLDER {
        ItemKind::Folder
    } else {
        ItemKind::File
    };
    let item = Item {
        id: item.id.clone(),
        kind,
        resource: item.resource.clone(),
    };
    match kind {
        ItemKind::Folder => list(&item, api).await,
        _ => item.lone(),
    }
}

/// learns what listed folder `root` holds, so drive's api may be asked for those items with the
/// token: from its page first, then from drive's api once that page refused. a listed file, or an
/// empty folder, holds nothing to learn.
///
/// # Errors
///
/// a stop, or why drive's api would not list the folder, such as an account it is not shared
/// with.
pub(crate) async fn search(api: &impl Api, root: &DriveId) -> Result<(), Error> {
    let item = Item {
        id: root.as_str().to_owned(),
        kind: ItemKind::Folder,
        resource: None,
    };
    match folder::read_root(&item, api).await {
        Err(refused) if refused.kind == Kind::AuthRequired => {}
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        Ok(()) | Err(_) => return Ok(()),
    }
    if api.drive_refused(&item.id).await? != Private::Ready {
        return Ok(());
    }
    match list(&item, api).await {
        Err(error) if error.kind != Kind::ReferenceUnresolved => Err(error),
        _ => Ok(()),
    }
}

/// what drive's api says of item `id`.
pub(super) async fn about(api: &impl Api, id: &str) -> Result<About, Error> {
    let mut url = Url::parse(&format!("{FILES}/{id}")).map_err(link_error)?;
    url.query_pairs_mut()
        .append_pair("fields", FIELDS)
        .append_pair("supportsAllDrives", "true");
    read(&api.get(&url).await?)
}

/// what drive's api says of file `id`, which must be a file with bytes to download.
///
/// # Errors
///
/// why drive's api would not say, and `reference.unresolved` for a folder, a google doc or a
/// shortcut, which have no bytes of their own.
pub(crate) async fn file(api: &impl Api, id: &str) -> Result<About, Error> {
    let about = about(api, id).await?;
    if about.mime_type.starts_with(GOOGLE_APPS) {
        let what = match about.mime_type.as_str() {
            FOLDER => "a folder",
            SHORTCUT => "a shortcut to an item elsewhere in drive",
            _ => "a google doc, sheet or slides, which drive keeps as no file",
        };
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("drive's {id} is {what}, with no bytes of its own to download"),
        )
        .help("add a folder by its folder link, an item a shortcut points to by its own link, and export a google doc to a file in drive first"));
    }
    Ok(about)
}

/// every file in private folder `item` and its subfolders, at its path inside the folder, as
/// drive's api lists them.
pub(super) async fn list(item: &Item, api: &impl Api) -> Result<Listing, Error> {
    list_at_most(item, api, most_files()).await
}

/// [`list`] of a folder that may hold at most `most` files and `most` folders.
pub(super) async fn list_at_most(
    item: &Item,
    api: &impl Api,
    most: usize,
) -> Result<Listing, Error> {
    let root = about(api, &item.id).await?;
    if root.mime_type != FOLDER {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("drive's {} is a file, not a folder", item.id),
        )
        .help(format!("add it as drive:{}", item.id)));
    }
    let mut walk = Walk {
        most,
        ..Walk::default()
    };
    walk.seen.insert(item.id.clone());
    walk.folders.push_back((item.id.clone(), String::new()));
    // drive may send a page with no items and a token for the next, but not forever
    let mut pages = crate::source::Pages::most(most + 1);
    let mut listed = 0;
    while let Some((id, path)) = walk.folders.pop_front() {
        let mut token = None;
        pages.begin();
        loop {
            pages
                .next((id.clone(), token.clone()), listed)
                .map_err(|reason| unreadable(item, &reason))?;
            let page: Page = read(&api.get(&children(&id, token.as_deref())?).await?)?;
            if page.incomplete_search {
                return Err(unreadable(item, "drive said its listing is incomplete"));
            }
            listed += page.files.len();
            walk.add(item, &path, page.files)?;
            match page.next_page_token.filter(|next| !next.is_empty()) {
                Some(next) => token = Some(next),
                None => break,
            }
        }
    }
    if walk.files.is_empty() && walk.unavailable.is_empty() {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("{} holds no files", folder::link(&item.id)),
        )
        .help("check the folder in a browser, signed in as the account that reads it"));
    }
    Ok(Listing {
        within: Vec::new(),
        access: Access::Login,
        unavailable: walk.unavailable,
        warnings: Vec::new(),
        resolved: Reference::Drive(item.clone()),
        title: Some(root.name).filter(|name| !name.is_empty()),
        license: None,
        doi: None,
        files: walk.files,
    })
}

impl Walk {
    /// takes in the items of the folder at `parent`, queueing its subfolders.
    fn add(&mut self, item: &Item, parent: &str, found: Vec<About>) -> Result<(), Error> {
        for entry in found {
            if !is_id(&entry.id) {
                return Err(unreadable(item, "an item has no drive id"));
            }
            let path = crate::name::joined(parent, &entry.name);
            if let Some(reason) = crate::name::escapes(&path) {
                return Err(unreadable(item, &format!("the path {path} {reason}")));
            }
            if entry.mime_type == FOLDER {
                if !self.seen.insert(entry.id.clone()) {
                    return Err(unreadable(
                        item,
                        &format!("the folder {path} appears twice"),
                    ));
                }
                if self.seen.len() > self.most {
                    return Err(too_many(item, self.most));
                }
                self.folders.push_back((entry.id, path));
            } else if entry.mime_type == SHORTCUT {
                self.unavailable.push(Unavailable {
                    path,
                    reason:
                        "a shortcut to an item elsewhere in drive: add that item by its own link"
                            .to_owned(),
                });
            } else if entry.mime_type.starts_with(GOOGLE_APPS) {
                self.unavailable.push(Unavailable {
                    path,
                    reason: "google docs exports can change between downloads, so a lock cannot pin them".to_owned(),
                });
            } else {
                self.files.push(Remote {
                    size: entry
                        .size
                        .as_deref()
                        .and_then(|size| size.trim().parse().ok()),
                    sha256: entry.sha256_checksum.as_deref().and_then(Sha256::from_hex),
                    md5: entry.md5_checksum.as_deref().and_then(Md5::from_hex),
                    url: file_url(&entry.id, None)?,
                    named_by_server: false,
                    etag: None,
                    path,
                });
            }
            if self.files.len() + self.unavailable.len() > self.most {
                return Err(too_many(item, self.most));
            }
        }
        Ok(())
    }
}

/// the request for one page of the items in folder `id`, after the page `token` names.
fn children(id: &str, token: Option<&str>) -> Result<Url, Error> {
    let mut url = Url::parse(FILES).map_err(link_error)?;
    url.query_pairs_mut()
        .append_pair("q", &format!("'{id}' in parents and trashed = false"))
        .append_pair(
            "fields",
            &format!("nextPageToken,incompleteSearch,files({FIELDS})"),
        )
        .append_pair("pageSize", "1000")
        .append_pair("supportsAllDrives", "true")
        .append_pair("includeItemsFromAllDrives", "true");
    if let Some(token) = token {
        url.query_pairs_mut().append_pair("pageToken", token);
    }
    Ok(url)
}

/// the answer of drive's api, read, or why it refused.
fn read<T: DeserializeOwned>(response: &Response) -> Result<T, Error> {
    if response.status == 200 {
        return response.json();
    }
    Err(match &response.drive {
        Some(answered) => answered.error(response.status, &response.body),
        None => Error::new(
            Kind::NetworkStatus,
            format!("google drive's api answered {}", response.status),
        ),
    })
}

fn too_many(item: &Item, most: usize) -> Error {
    too_many_of(
        folder::link(&item.id),
        most,
        "pass the link of each subfolder you need to `fetchloom add`",
    )
}

fn unreadable(item: &Item, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "drive's api sent a listing of {} that fetchloom cannot read: {reason}",
            folder::link(&item.id)
        ),
    )
    .help(crate::error::report_answer())
}

fn link_error(err: url::ParseError) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("could not make a link to drive's api: {err}"),
    )
}

#[cfg(test)]
#[path = "private_tests.rs"]
mod tests;
