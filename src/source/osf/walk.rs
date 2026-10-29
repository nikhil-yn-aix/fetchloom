//! OSF storage folders, their pages, and the versions that keep each file's bytes fixed.

use std::collections::{HashSet, VecDeque};

use serde::Deserialize;
use url::Url;

use super::Node;
use crate::digest::{Md5, Sha256};
use crate::error::Error;
use crate::net::Api;
use crate::source::{Listing, MOST_FILES, Reference, Remote};

#[derive(Deserialize)]
struct Page {
    data: Vec<Entry>,
    links: Pages,
}

#[derive(Deserialize)]
struct Pages {
    next: Option<String>,
}

/// a file or folder in OSF storage.
#[derive(Deserialize)]
pub(super) struct Entry {
    id: String,
    attributes: Attributes,
    links: Links,
    #[serde(default)]
    relationships: Relationships,
}

#[derive(Deserialize)]
struct Attributes {
    name: String,
    kind: EntryKind,
    provider: String,
    size: Option<u64>,
    current_version: Option<u64>,
    extra: Option<Extra>,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum EntryKind {
    File,
    Folder,
}

#[derive(Deserialize)]
struct Extra {
    hashes: Option<Hashes>,
}

#[derive(Default, Deserialize)]
struct Hashes {
    sha256: Option<Sha256>,
    md5: Option<Md5>,
}

#[derive(Deserialize)]
struct Links {
    download: Option<String>,
}

#[derive(Default, Deserialize)]
struct Relationships {
    files: Option<Related>,
}

#[derive(Deserialize)]
struct Related {
    links: RelatedLinks,
}

#[derive(Deserialize)]
struct RelatedLinks {
    related: Href,
}

#[derive(Deserialize)]
struct Href {
    href: String,
}

struct Folder {
    url: Url,
    path: String,
}

#[derive(Default)]
struct Walk {
    files: Vec<Remote>,
    folders: VecDeque<Folder>,
    seen: HashSet<String>,
}

/// follows each folder and server-supplied page once.
pub(super) async fn files(node: &Node, api: &impl Api, root: Url) -> Result<Vec<Remote>, Error> {
    let mut walk = Walk::default();
    walk.folders.push_back(Folder {
        url: root,
        path: String::new(),
    });
    let mut pages = HashSet::new();
    while let Some(folder) = walk.folders.pop_front() {
        let mut url = folder.url;
        loop {
            if !pages.insert(url.clone()) {
                return Err(node.invalid("the files API repeated a page or folder"));
            }
            let response = api.get(&url).await?;
            let page: Page = crate::source::json(node, &response)?;
            if page.data.is_empty() && page.links.next.is_some() {
                return Err(node.invalid("an empty file page points to another page"));
            }
            for entry in page.data {
                walk.add(node, &url, &folder.path, entry)?;
            }
            let Some(next) = page.links.next else { break };
            url = api_url(node, &url, &next)?;
        }
    }
    Ok(walk.files)
}

impl Walk {
    fn add(&mut self, node: &Node, page: &Url, parent: &str, entry: Entry) -> Result<(), Error> {
        if entry.id.is_empty() || !self.seen.insert(entry.id.clone()) {
            return Err(node.invalid("the files API repeated a file or folder id"));
        }
        if self.seen.len() > MOST_FILES {
            return Err(node.invalid("the files API listed too many files or folders"));
        }
        let path = if parent.is_empty() {
            entry.attributes.name.clone()
        } else {
            format!("{parent}/{}", entry.attributes.name)
        };
        if let Some(reason) = crate::name::escapes(&path) {
            return Err(node.invalid(&format!("the path \"{path}\" {reason}")));
        }
        match entry.attributes.kind {
            EntryKind::File => self.files.push(entry.remote(node, path)?),
            EntryKind::Folder => {
                let related = entry
                    .relationships
                    .files
                    .ok_or_else(|| node.invalid("a folder has no files relationship"))?;
                let mut url = api_url(node, page, &related.links.related.href)?;
                // OSF caps page[size] at 100, even when a caller asks for 1,000.
                url.query_pairs_mut()
                    .clear()
                    .append_pair("page[size]", "100");
                self.folders.push_back(Folder { url, path });
            }
        }
        Ok(())
    }
}

fn api_url(node: &Node, base: &Url, link: &str) -> Result<Url, Error> {
    let bad = || node.invalid("a file page or folder link leaves the OSF API");
    let url = base.join(link).map_err(|_| bad())?;
    let prefix = format!("/v2/nodes/{}/files/osfstorage/", node.id);
    let is_listing = url.path().strip_prefix(&prefix).is_some_and(|rest| {
        rest.is_empty()
            || rest.strip_suffix('/').is_some_and(|id| {
                !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
    });
    if !is_listing
        || url.scheme() != "https"
        || url.host_str() != Some("api.osf.io")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url
            .query_pairs()
            .any(|(key, _)| !matches!(key.as_ref(), "page" | "page[size]"))
    {
        return Err(bad());
    }
    Ok(url)
}

impl Entry {
    /// lists the one file named by a GUID.
    pub(super) fn listing(self, node: &Node) -> Result<Listing, Error> {
        if self.attributes.kind != EntryKind::File {
            return Err(node.invalid("the GUID names a folder rather than a file"));
        }
        let title = self.attributes.name.clone();
        let file = self.remote(node, title.clone())?;
        Ok(Listing {
            resolved: Reference::Osf(node.clone()),
            title: Some(title),
            license: None,
            doi: None,
            files: vec![file],
        })
    }

    fn version(&self, node: &Node) -> Result<u64, Error> {
        self.attributes
            .current_version
            .filter(|version| *version > 0)
            .ok_or_else(|| {
                node.invalid(&format!(
                    "\"{}\" has no file version to pin",
                    self.attributes.name
                ))
            })
    }

    fn remote(self, node: &Node, path: String) -> Result<Remote, Error> {
        if self.attributes.provider != "osfstorage" {
            return Err(node.invalid(&format!("\"{path}\" is not stored in OSF storage")));
        }
        let url = self.download(node, self.version(node)?)?;
        let hashes = self
            .attributes
            .extra
            .and_then(|extra| extra.hashes)
            .unwrap_or_default();
        Ok(Remote {
            path,
            size: self.attributes.size,
            sha256: hashes.sha256,
            md5: hashes.md5,
            url,
            named_by_server: false,
        })
    }

    fn download(&self, node: &Node, version: u64) -> Result<Url, Error> {
        let bad = || {
            node.invalid(&format!(
                "\"{}\" has no stable OSF download link",
                self.attributes.name
            ))
        };
        let mut url = self
            .links
            .download
            .as_ref()
            .and_then(|link| Url::parse(link).ok())
            .ok_or_else(bad)?;
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("osf.io" | "files.osf.io"))
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(bad());
        }
        let mut pairs = Vec::new();
        for (key, value) in url.query_pairs() {
            match (key.as_ref(), value.as_ref()) {
                ("version", _) => (),
                ("action", "download") | ("direct", "" | "1" | "true") => {
                    pairs.push((key.into_owned(), value.into_owned()));
                }
                _ => return Err(bad()),
            }
        }
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("version", &version.to_string());
        Ok(url)
    }
}
