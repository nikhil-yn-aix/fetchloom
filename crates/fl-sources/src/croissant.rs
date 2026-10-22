//! Croissant: a dataset described by its JSON-LD metadata, whose file objects name their URLs,
//! sizes and digests.

use std::sync::Arc;

use fl_model::digest::{Algorithm, Digest};
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use fl_transfer::fetch::Request;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::license;
use crate::listing::{Entry, Listing, Meta};
use crate::record;
use crate::source::{Source, SourceError};

const FILE_OBJECT: [&str; 3] = ["cr:FileObject", "FileObject", "sc:FileObject"];
const FILE_SET: [&str; 3] = ["cr:FileSet", "FileSet", "sc:FileSet"];

/// Serves `croissant:` references, the URL of a Croissant metadata file after the colon. Each
/// file object with an http or https `contentUrl` is one file, its `sha256` and `md5` stated
/// when they are digests. One request.
pub struct Croissant {
    schemes: Vec<String>,
    client: Arc<Client>,
}

impl Croissant {
    /// The adapter for Croissant metadata, fetched through `client`.
    #[must_use]
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            schemes: vec!["croissant".to_owned()],
            client,
        }
    }

    async fn listing(&self, reference: &Reference) -> Result<Listing, SourceError> {
        let raw = reference.as_str();
        let invalid = |reason: String| SourceError::Invalid {
            reference: raw.to_owned(),
            reason,
        };
        let Target::Scheme { body, .. } = reference.target() else {
            return Err(SourceError::Unresolved {
                reference: raw.to_owned(),
            });
        };
        let url = SafeUrl::parse(body)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https"))
            .ok_or_else(|| invalid("it names no http or https URL".to_owned()))?;
        let request = Request {
            url: &url,
            follow: true,
            typed: true,
        };
        let (page, metadata) = record::json(&self.client, &request, raw).await?;
        let objects = match metadata.get("distribution") {
            Some(Value::Array(objects)) => objects.as_slice(),
            Some(one @ Value::Object(_)) => std::slice::from_ref(one),
            _ => &[],
        };
        let mut entries = Vec::new();
        for object in objects.iter().filter(|object| typed(object, &FILE_OBJECT)) {
            if let Some(entry) = entry(object, &page.url)? {
                entries.push(entry);
            }
        }
        if entries.is_empty() && objects.iter().any(|object| typed(object, &FILE_SET)) {
            return Err(invalid(
                "its files are only in file sets inside a repository or archive; use the reference of the repository instead".to_owned(),
            ));
        }
        Ok(Listing {
            resolved: reference.clone(),
            meta: meta(&metadata),
            entries,
        })
    }
}

impl Source for Croissant {
    fn schemes(&self) -> &[String] {
        &self.schemes
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(self.listing(reference))
    }
}

fn typed(object: &Value, names: &[&str]) -> bool {
    match object.get("@type") {
        Some(Value::String(name)) => names.contains(&name.as_str()),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .any(|name| names.contains(&name)),
        _ => false,
    }
}

fn text<'v>(object: &'v Value, key: &str) -> Option<&'v str> {
    object.get(key).and_then(Value::as_str)
}

fn entry(object: &Value, page: &SafeUrl) -> Result<Option<Entry>, SourceError> {
    let Some(location) = text(object, "contentUrl") else {
        return Ok(None);
    };
    let Ok(url) = page.join(location) else {
        return Ok(None);
    };
    if !matches!(url.scheme(), "http" | "https")
        || text(object, "encodingFormat") == Some("git+https")
    {
        return Ok(None);
    }
    let id = text(object, "@id").filter(|id| !id.contains(':') && !id.starts_with('#'));
    let name = id
        .or_else(|| text(object, "name"))
        .ok_or_else(|| SourceError::Record {
            path: "$.distribution[*].name".to_owned(),
            url: page.clone(),
            problem: "no value",
        })?;
    let path =
        DataPath::normalize(name.trim_start_matches('/')).map_err(|_| SourceError::Record {
            path: "$.distribution[*].name".to_owned(),
            url: page.clone(),
            problem: "a value that is not a valid path",
        })?;
    let size = match object.get("contentSize") {
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(size)) => size.trim().trim_end_matches('B').trim().parse().ok(),
        _ => None,
    };
    let stated = [("sha256", Algorithm::Sha256), ("md5", Algorithm::Md5)]
        .into_iter()
        .filter_map(|(key, algorithm)| {
            let hex = text(object, key)?.to_ascii_lowercase();
            Digest::from_hex(algorithm, &hex).ok()
        })
        .collect();
    Ok(Some(Entry {
        path,
        size,
        stated,
        urls: vec![url],
    }))
}

fn meta(metadata: &Value) -> Meta {
    let license = match metadata.get("license") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => items.first().and_then(Value::as_str).map(str::to_owned),
        Some(object @ Value::Object(_)) => text(object, "@id")
            .or_else(|| text(object, "name"))
            .map(str::to_owned),
        _ => None,
    };
    let creators = match metadata.get("creator") {
        Some(Value::Array(people)) => people.iter().filter_map(person).collect(),
        Some(one) => person(one).into_iter().collect(),
        None => Vec::new(),
    };
    Meta {
        title: text(metadata, "name").map(str::to_owned),
        version: text(metadata, "version").map(str::to_owned),
        license: license.map(|text| license::spdx(&text)),
        doi: None,
        creators,
    }
}

fn person(value: &Value) -> Option<String> {
    if let Some(name) = text(value, "name") {
        return Some(name.to_owned());
    }
    match (text(value, "givenName"), text(value, "familyName")) {
        (Some(given), Some(family)) => Some(format!("{family}, {given}")),
        (given, family) => given.or(family).map(str::to_owned),
    }
}
