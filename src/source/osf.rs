//! osf: projects and components by their short id, their files walked folder by folder through
//! the OSF API.

use std::fmt;

use url::Url;

use super::{Listing, Var, malformed};
use crate::error::Error;
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

/// an OSF project or component, by its short id such as `abc12`.
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
        let first = url
            .path_segments()?
            .find(|part| !part.is_empty())?
            .to_ascii_lowercase();
        (is_id(&first) && !PAGES.contains(&first.as_str())).then_some(Node { id: first })
    }

    /// walks the project's storage through the OSF API.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the osf milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("osf"))
    }
}

/// OSF ids are five or more lowercase letters and digits.
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
