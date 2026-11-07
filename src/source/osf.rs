//! osf: projects and components by their short id, their files walked folder by folder through
//! the OSF API.

mod walk;

use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{Listing, Reference, Remote, malformed};
use crate::error::{Error, Kind};
use crate::net::{self, Api, Expired};

/// first path segments on osf.io that are pages, not ids.
const PAGES: [&str; 12] = [
    "preprints",
    "registries",
    "institutions",
    "search",
    "dashboard",
    "meetings",
    "collections",
    "settings",
    "support",
    "login",
    "register",
    "myprojects",
];

/// an OSF project, component or file, by its short id such as `abc12`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    id: String,
}

impl Node {
    /// reads the part after `osf:`.
    pub(super) fn parse(text: &str) -> Result<Node, Error> {
        let id = text.trim().to_ascii_lowercase();
        if is_id(&id) {
            Ok(Node { id })
        } else {
            Err(malformed(&format!("osf:{text}"), "osf:abc12"))
        }
    }

    /// a pasted osf.io link. a link asking for one version of a file is no node, since a node
    /// follows the file's current version; [`versioned`] takes it.
    pub(super) fn from_link(url: &Url) -> Option<Node> {
        if !is_osf(url) || asks_for_version(url) {
            return None;
        }
        let mut parts = url.path_segments()?.filter(|part| !part.is_empty());
        let first = parts.next()?;
        let id = if first == "download" {
            parts.next()?
        } else {
            first
        }
        .to_ascii_lowercase();
        (is_id(&id) && !PAGES.contains(&id.as_str())).then_some(Node { id })
    }

    /// lists the project's files or the single file named by a GUID.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let mut url = self.endpoint("nodes")?;
        url.query_pairs_mut()
            .append_pair("embed", "license")
            .append_pair("embed", "identifiers")
            .append_pair("related_counts", "children");
        let mut response = api.get(&url).await?;
        if response.status == 404 {
            // file GUIDs are not nodes; the GUID endpoint redirects to their file metadata.
            response = api.get(&self.endpoint("guids")?).await?;
        }
        let answer: Document<Resource> = super::json(self, &response)?;
        match answer.data {
            Resource::Node(metadata) => {
                if metadata.id != self.id {
                    return Err(self.invalid("the node id does not match"));
                }
                let mut root = self.endpoint("nodes")?;
                root.path_segments_mut()
                    .map_err(|()| self.invalid("the files URL has no path"))?
                    .pop_if_empty()
                    .extend(["files", "osfstorage", ""]);
                root.query_pairs_mut().append_pair("page[size]", "100");
                let files = walk::files(self, api, root).await?;
                let components = metadata.components();
                let mut listing = metadata.listing(self, files);
                if components > 0 {
                    listing.warnings.push(format!(
                        "{self} has {}, and their files are not listed with it. add a component as a dataset of its own, as osf:<its id>",
                        crate::units::count(components, "component", "components")
                    ));
                }
                Ok(listing)
            }
            Resource::File(file) => file.listing(self),
        }
    }

    fn endpoint(&self, resource: &str) -> Result<Url, Error> {
        Url::parse(&format!("https://api.osf.io/v2/{resource}/{}/", self.id))
            .map_err(|_| self.invalid("the API URL is invalid"))
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("open the project or file on osf.io and report the unsupported listing to its owner")
    }
}

#[derive(Deserialize)]
struct Document<T> {
    data: T,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Resource {
    #[serde(rename = "nodes")]
    Node(Box<Metadata>),
    #[serde(rename = "files")]
    File(Box<walk::Entry>),
}

#[derive(Deserialize)]
struct Metadata {
    id: String,
    attributes: Attributes,
    #[serde(default)]
    embeds: Embeds,
    #[serde(default)]
    relationships: serde_json::Value,
}

#[derive(Deserialize)]
struct Attributes {
    title: String,
}

#[derive(Default, Deserialize)]
struct Embeds {
    license: Option<Document<Option<License>>>,
    identifiers: Option<Document<Vec<Identifier>>>,
}

#[derive(Deserialize)]
struct License {
    attributes: LicenseName,
}

#[derive(Deserialize)]
struct LicenseName {
    name: String,
}

#[derive(Deserialize)]
struct Identifier {
    attributes: IdentifierValue,
}

#[derive(Deserialize)]
struct IdentifierValue {
    category: String,
    value: String,
}

impl Metadata {
    /// the node's components, which `related_counts=children` counts. their files live in
    /// storage of their own, which the walk does not reach.
    fn components(&self) -> u64 {
        self.relationships
            .pointer("/children/links/related/meta/count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    }

    fn listing(self, node: &Node, files: Vec<Remote>) -> Listing {
        Listing {
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Osf(node.clone()),
            title: Some(self.attributes.title),
            license: self
                .embeds
                .license
                .and_then(|license| license.data)
                .map(|license| license.attributes.name),
            doi: self
                .embeds
                .identifiers
                .and_then(|identifiers| {
                    identifiers
                        .data
                        .into_iter()
                        .find(|identifier| identifier.attributes.category == "doi")
                })
                .map(|identifier| identifier.attributes.value),
            files,
        }
    }
}

/// OSF ids are five to twelve lowercase letters and digits.
fn is_id(text: &str) -> bool {
    (5..=12).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn is_osf(url: &Url) -> bool {
    matches!(url.host_str(), Some("osf.io" | "www.osf.io"))
}

fn asks_for_version(url: &Url) -> bool {
    url.query_pairs().any(|(key, _)| key == "version")
}

/// an osf.io link to one file or folder in a project's storage, such as
/// `https://osf.io/ezcuj/files/osfstorage/553e69248c5e4a219919ea54`, is refused: its last part is
/// a storage id, not a path a selection could match, and the whole project is more than was asked
/// for.
pub(super) fn stored_item(url: &Url) -> Option<Error> {
    if !is_osf(url) {
        return None;
    }
    let parts: Vec<&str> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let [id, "files", _provider, _item, ..] = parts.as_slice() else {
        return None;
    };
    let id = id.to_ascii_lowercase();
    if !is_id(&id) || PAGES.contains(&id.as_str()) {
        return None;
    }
    Some(
        Error::new(
            Kind::ReferenceInvalid,
            format!(
                "{} names one file or folder of osf:{id} by a storage id, from which fetchloom cannot tell its path",
                net::Shown(url)
            ),
        )
        .help(format!(
            "for a file, paste its download link, which starts with https://osf.io/download/. for a folder, add osf:{id} with --select \"<folder>/**\""
        )),
    )
}

/// an osf.io link that asks for one version of a file. its download link fetches that version
/// as a plain link, pinned to those bytes, while any other such link is refused, since an OSF
/// reference follows the file's current version.
pub(super) fn versioned(url: &Url) -> Option<Result<Url, Error>> {
    if !is_osf(url) || !asks_for_version(url) {
        return None;
    }
    let first = url.path_segments()?.find(|part| !part.is_empty());
    if first == Some("download") {
        return Some(Ok(url.clone()));
    }
    let id = first.map(str::to_ascii_lowercase).filter(|id| is_id(id));
    let version = url
        .query_pairs()
        .find(|(key, _)| key == "version")
        .map(|(_, version)| version.into_owned())
        .filter(|version| !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()));
    let help = match (id, version) {
        (Some(id), Some(version)) => format!(
            "paste its download link, https://osf.io/download/{id}/?version={version}, or osf:{id} for the current version"
        ),
        _ => {
            "paste the file's download link, which starts with https://osf.io/download/".to_owned()
        }
    };
    Some(Err(Error::new(
        Kind::ReferenceInvalid,
        format!(
            "{} asks for one version of an OSF file, which only its download link fetches",
            net::Shown(url)
        ),
    )
    .help(help)))
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "osf:{}", self.id)
    }
}

/// OSF redirects can expire while a slow first connection reaches its storage provider.
pub(super) fn expiry_check(url: &Url) -> Option<Expired> {
    (url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && matches!(url.host_str(), Some("osf.io" | "files.osf.io"))
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(expired)
}

fn expired(text: &str) -> bool {
    text.contains("<Code>AccessDenied</Code>")
        && text.contains("<Message>Request has expired. X-Amz-Date = ")
        && text.contains(", X-Amz-Expires = ")
}
#[cfg(test)]
mod tests;
#[cfg(test)]
mod walk_tests;
