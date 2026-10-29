//! shared folders, read from the page drive embeds folders with. the page lists every entry of
//! one folder, without sizes and without pages to follow, so each subfolder costs one request.

use std::collections::{HashSet, VecDeque};

use url::Url;

use super::{Item, ItemKind, file_url, html};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Listing, MOST_FILES, Reference, Remote};
use crate::units;

/// how many of the items drive cannot send as files an error names. the rest are counted.
const NAMED: usize = 5;

/// a drive id, with the resource key drive asks for along with items shared before 2021.
#[derive(Debug, PartialEq, Eq)]
struct Key {
    id: String,
    resource: Option<String>,
}

/// one entry of a folder page: its name, and what it links to.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    name: String,
    /// the file or folder it links to, or none for a google doc, sheet or slides, which drive
    /// keeps as no file at all.
    link: Option<(Key, bool)>,
}

/// a folder page: the folder's name and its entries.
#[derive(Debug, PartialEq, Eq)]
struct Page {
    name: String,
    entries: Vec<Entry>,
}

/// the files found so far, and the folders still to read.
#[derive(Default)]
struct Walk {
    files: Vec<Remote>,
    folders: VecDeque<(Key, String)>,
    seen: HashSet<String>,
}

/// lists every file in the folder and in its subfolders, each at its path inside the folder.
pub(super) async fn list(item: &Item, api: &impl Api) -> Result<Listing, Error> {
    let root = Key {
        id: item.id.clone(),
        resource: item.resource.clone(),
    };
    let page = read(item, api, &root, "").await?;
    let title = Some(page.name.clone()).filter(|name| !name.is_empty());
    let mut walk = Walk::default();
    walk.seen.insert(root.id);
    walk.add(item, "", page)?;
    while let Some((key, path)) = walk.folders.pop_front() {
        let page = read(item, api, &key, &path).await?;
        walk.add(item, &path, page)?;
    }
    if walk.files.is_empty() {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("{} holds no files", link(&item.id)),
        )
        .help("check the link in a browser. a link to one file looks like drive.google.com/file/d/..."));
    }
    Ok(Listing {
        resolved: Reference::Drive(item.clone()),
        title,
        version: None,
        license: None,
        doi: None,
        files: walk.files,
    })
}

impl Walk {
    /// takes in the entries of the folder at `parent`, queueing its subfolders.
    fn add(&mut self, item: &Item, parent: &str, page: Page) -> Result<(), Error> {
        let path = |name: &str| {
            if parent.is_empty() {
                name.to_owned()
            } else {
                format!("{parent}/{name}")
            }
        };
        let unsent: Vec<String> = page
            .entries
            .iter()
            .filter(|entry| entry.link.is_none())
            .map(|entry| path(&entry.name))
            .collect();
        if !unsent.is_empty() {
            return Err(unsent_items(item, parent, &unsent));
        }
        for entry in page.entries {
            let Some((key, folder)) = entry.link else {
                continue;
            };
            let path = path(&entry.name);
            if let Some(reason) = crate::name::escapes(&path) {
                return Err(unreadable(item, &format!("the path {path} {reason}")));
            }
            if folder {
                if !self.seen.insert(key.id.clone()) {
                    return Err(unreadable(
                        item,
                        &format!("the folder {path} appears twice"),
                    ));
                }
                if self.seen.len() > MOST_FILES {
                    return Err(unreadable(item, "the listing holds too many folders"));
                }
                self.folders.push_back((key, path));
            } else {
                self.files.push(Remote {
                    path,
                    size: None,
                    sha256: None,
                    md5: None,
                    url: file_url(&key.id, key.resource.as_deref())?,
                    named_by_server: false,
                });
            }
            if self.files.len() > MOST_FILES {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "{} holds more than {MOST_FILES} files, more than fetchloom handles in one dataset",
                        link(&item.id)
                    ),
                )
                .help("get the subfolders you need by their own links"));
            }
        }
        Ok(())
    }
}

/// asks drive for the page of the folder `key` names, found at `path` inside the folder `item`.
async fn read(item: &Item, api: &impl Api, key: &Key, path: &str) -> Result<Page, Error> {
    let mut url = Url::parse("https://drive.google.com/embeddedfolderview")
        .map_err(|err| unreadable(item, &err.to_string()))?;
    url.query_pairs_mut().append_pair("id", &key.id);
    if let Some(resource) = &key.resource {
        url.query_pairs_mut().append_pair("resourcekey", resource);
    }
    let response = api.get(&url).await?;
    let what = if path.is_empty() {
        link(&item.id)
    } else {
        format!("the folder {path} in {}", link(&item.id))
    };
    let signing_in = response.url.host_str() == Some("accounts.google.com");
    match response.status {
        200 if !signing_in => {
            parse(&response.text()).ok_or_else(|| unreadable(item, "a folder page has no list"))
        }
        200 | 401 | 403 => Err(Error::new(
            Kind::AuthRequired,
            format!("{what} is not shared with anyone who has the link"),
        )
        .help("ask its owner to share it with anyone with the link. fetchloom does not sign in to google")),
        404 | 410 => Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("{what} does not exist"),
        )
        .help("check the link in a browser. a folder that was deleted cannot be fetched")),
        status => Err(Error::new(
            Kind::NetworkStatus,
            format!("drive answered {status} for {what}"),
        )),
    }
}

/// reads a folder page: the folder's name, then the name and link of each entry. none when the
/// page holds no list of entries.
fn parse(text: &str) -> Option<Page> {
    let base = Url::parse("https://drive.google.com/").ok()?;
    let name = html::text(html::between(text, "<title>", "</title>")?);
    let list = &text[text.find("<div class=\"flip-entries\">")?..];
    let mut entries = Vec::new();
    for part in list.split("<div class=\"flip-entry\"").skip(1) {
        let href = html::attribute(part, "href")?;
        let name = html::text(html::between(
            part,
            "<div class=\"flip-entry-title\">",
            "</div>",
        )?);
        let link = base.join(&href).ok().and_then(|url| {
            let item = Item::from_link(&url)?;
            let folder = match item.kind {
                ItemKind::File => false,
                ItemKind::Folder => true,
                ItemKind::Open => return None,
            };
            Some((
                Key {
                    id: item.id,
                    resource: item.resource,
                },
                folder,
            ))
        });
        entries.push(Entry { name, link });
    }
    Some(Page { name, entries })
}

/// the error for items in a folder that drive cannot send as files.
fn unsent_items(item: &Item, parent: &str, paths: &[String]) -> Error {
    let place = if parent.is_empty() {
        link(&item.id)
    } else {
        format!("the folder {parent} in {}", link(&item.id))
    };
    let mut error = Error::new(
        Kind::SourceInvalid,
        format!(
            "{place} holds {} drive cannot send as files",
            units::count(paths.len() as u64, "item", "items")
        ),
    );
    for path in paths.iter().take(NAMED) {
        error = error.detail(path.clone());
    }
    if paths.len() > NAMED {
        error = error.detail(format!("and {} more", paths.len() - NAMED));
    }
    error
        .detail("google docs exports can change between downloads; fetchloom needs files with fixed bytes")
        .help("ask the folder's owner to add them as files, such as pdf or csv, or get the other files by their own links")
}

fn unreadable(item: &Item, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "drive sent a folder page for {} that fetchloom cannot read: {reason}",
            link(&item.id)
        ),
    )
    .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
}

/// a folder's link as drive shares it.
fn link(id: &str) -> String {
    format!("https://drive.google.com/drive/folders/{id}")
}
