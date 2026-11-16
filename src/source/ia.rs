//! the internet archive: an item's files by its identifier, listed from archive.org's metadata
//! api with the size and MD5 it states for each, and downloaded from archive.org, which sends each
//! download on to a server that holds the item. a file the item keeps private is left out, and so
//! are archive.org's own records of the item, which it remakes whenever the item changes.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use url::Url;

use super::{Access, Listing, Reference, Remote, invalid, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::Api;

const SITE: &str = "https://archive.org";

/// the most names of private files a warning lists before it counts the rest.
const NAMED: usize = 3;

/// an internet archive item, by its identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Item {
    /// the identifier as archive.org writes it, whose case counts.
    id: String,
}

impl Item {
    /// reads the part after `ia:`.
    pub(super) fn parse(text: &str) -> Result<Item, Error> {
        Item::named(text.trim()).ok_or_else(|| malformed(&format!("ia:{text}"), "ia:iris_20240626"))
    }

    /// an identifier as archive.org makes one: letters, digits, `_`, `-` and `.`, a letter or
    /// digit first, at most 100 long.
    fn named(text: &str) -> Option<Item> {
        let valid = (1..=100).contains(&text.len())
            && text.starts_with(|c: char| c.is_ascii_alphanumeric())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        valid.then(|| Item {
            id: text.to_owned(),
        })
    }

    /// the item's page on archive.org.
    pub(super) fn page(&self) -> String {
        format!("https://archive.org/details/{}", self.id)
    }

    /// a pasted link to an item's page or to its folder of downloads on archive.org. a link to
    /// one file stays a plain link, since it may name a file inside one of the item's zips.
    pub(super) fn from_link(url: &Url) -> Option<Item> {
        if !matches!(url.host_str()?, "archive.org" | "www.archive.org") {
            return None;
        }
        let parts: Vec<&str> = url.path_segments()?.collect();
        match parts.as_slice() {
            ["details" | "download", id] | ["details" | "download", id, ""] => Item::named(id),
            _ => None,
        }
    }

    /// asks archive.org's metadata api for the item's files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let response = api.get(&self.url(&["metadata", &self.id])?).await?;
        let answer: Answer = super::json(self, &response)?;
        if answer.is_dark {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("archive.org has taken {self} down"),
            )
            .help(super::UNKNOWN_HELP));
        }
        let Some(metadata) = answer.metadata else {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} does not exist, or is not public"),
            )
            .help(format!(
                "{}; archive.org's identifiers tell capitals apart",
                super::UNKNOWN_HELP
            )));
        };
        if text(&metadata.identifier).as_deref() != Some(self.id.as_str()) {
            return Err(invalid(self, "the answer names another item"));
        }
        if text(&metadata.mediatype).as_deref() == Some("collection") {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!("{self} is a collection of items, which holds no data of its own"),
            )
            .help(format!(
                "add the items you need from {}, each as ia:<identifier>",
                self.page()
            )));
        }
        if answer.files.len() > api.most_files() {
            return Err(super::too_many(
                self,
                api.most_files(),
                "download the files you need from the item's page one at a time",
            ));
        }
        let mut files = Vec::new();
        let mut private = Vec::new();
        for file in answer.files {
            if file.is_record() {
                continue;
            }
            if file.private.as_ref().is_some_and(is_true) {
                private.push(file.name);
                continue;
            }
            files.push(file.remote(self)?);
        }
        if files.is_empty() && !private.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("archive.org keeps every file of {self} private"),
            )
            .help(format!(
                "check the item at {}: archive.org lends or holds back its files, which fetchloom does not download",
                self.page()
            )));
        }
        let warnings = if private.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "archive.org keeps {} of {self} private, so {} left out: {}",
                crate::units::count(private.len() as u64, "file", "files"),
                if private.len() == 1 {
                    "it is"
                } else {
                    "they are"
                },
                crate::units::some_of(&private, NAMED)
            )]
        };
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            unavailable: Vec::new(),
            warnings,
            resolved: Reference::InternetArchive(self.clone()),
            title: text(&metadata.title).filter(|title| !title.trim().is_empty()),
            license: text(&metadata.licenseurl),
            doi: None,
            files,
        })
    }

    /// the link to `parts` on archive.org, each one encoded path segment.
    fn url(&self, parts: &[&str]) -> Result<Url, Error> {
        let mut url = Url::parse(SITE).map_err(|_| invalid(self, "the site's URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| invalid(self, "the site's URL has no path"))?
            .clear()
            .extend(parts);
        Ok(url)
    }
}

/// a value of archive.org's metadata, which writes a field given once as a string and a field
/// given more than once as a list: the string, or the first in the list.
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(values) => values.first().and_then(text),
        _ => None,
    }
}

/// whether a flag of archive.org's metadata is set, as it writes `"true"`.
fn is_true(value: &Value) -> bool {
    matches!(value, Value::Bool(true)) || value.as_str() == Some("true")
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    is_dark: bool,
    metadata: Option<Metadata>,
    #[serde(default)]
    files: Vec<File>,
}

#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    identifier: Value,
    #[serde(default)]
    mediatype: Value,
    #[serde(default)]
    title: Value,
    #[serde(default)]
    licenseurl: Value,
}

#[derive(Deserialize)]
struct File {
    name: String,
    #[serde(default)]
    source: Value,
    #[serde(default)]
    format: Value,
    #[serde(default)]
    size: Value,
    #[serde(default)]
    md5: Value,
    private: Option<Value>,
    /// set where `md5` sums the item's files rather than hashing this one, as for `_files.xml`.
    summation: Option<Value>,
}

impl File {
    /// whether the file is one of archive.org's own records of the item: its metadata, its list
    /// of files, its torrent or its tile, which archive.org remakes whenever the item changes.
    fn is_record(&self) -> bool {
        text(&self.source).as_deref() == Some("metadata")
            || matches!(
                text(&self.format).as_deref(),
                Some("Metadata" | "Item Tile")
            )
    }

    fn remote(self, item: &Item) -> Result<Remote, Error> {
        let size = match &self.size {
            Value::Null => None,
            stated => Some(number(stated).ok_or_else(|| {
                invalid(
                    item,
                    &format!(
                        "the size of \"{}\" is not a number",
                        self.name.escape_debug()
                    ),
                )
            })?),
        };
        let md5 = match (&self.md5, &self.summation) {
            (Value::Null, _) | (_, Some(_)) => None,
            (stated, None) => Some(
                text(stated)
                    .and_then(|hex| Md5::from_hex(&hex))
                    .ok_or_else(|| {
                        invalid(
                            item,
                            &format!("the MD5 of \"{}\" is not an MD5", self.name.escape_debug()),
                        )
                    })?,
            ),
        };
        let mut parts = vec!["download", item.id.as_str()];
        parts.extend(self.name.split('/'));
        Ok(Remote {
            url: item.url(&parts)?,
            path: self.name,
            size,
            sha256: None,
            md5,
            named_by_server: false,
            etag: None,
        })
    }
}

/// a count archive.org writes as a string of digits, or as a number.
fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|digits| digits.parse().ok()))
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ia:{}", self.id)
    }
}

#[cfg(test)]
mod tests;
