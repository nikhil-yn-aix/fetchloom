//! osf: projects and components by their short id, their files walked folder by folder through
//! the OSF API.

mod walk;

use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{Listing, Reference, Remote, Var, malformed};
use crate::error::{Error, Kind};
use crate::net::{Api, Credential};

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

    /// a pasted osf.io link.
    pub(super) fn from_link(url: &Url) -> Option<Node> {
        if !matches!(url.host_str()?, "osf.io" | "www.osf.io") {
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
            .append_pair("embed", "identifiers");
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
                Ok(metadata.listing(self, files))
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
    fn listing(self, node: &Node, files: Vec<Remote>) -> Listing {
        Listing {
            resolved: Reference::Osf(node.clone()),
            title: Some(self.attributes.title),
            version: None,
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

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "osf:{}", self.id)
    }
}

/// `OSF_TOKEN`, for private projects.
pub(super) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let token = var("OSF_TOKEN")
        .and_then(|token| token.into_string().ok())
        .filter(|token| !token.trim().is_empty());
    token
        .map(|token| Credential {
            host: "api.osf.io",
            header: "authorization",
            value: format!("Bearer {}", token.trim()),
        })
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod walk_tests;
