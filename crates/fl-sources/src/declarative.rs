//! The declarative adapter: a TOML file describes where a repository's record lives and where
//! in its JSON the files and metadata sit.

use std::collections::BTreeMap;
use std::sync::Arc;

use fl_model::digest::{Algorithm, Digest};
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::listing::{Entry, Listing, Meta};
use crate::record;
use crate::source::{Source, SourceError};

const ALGORITHMS: [Algorithm; 4] = [
    Algorithm::Blake3,
    Algorithm::Sha256,
    Algorithm::Sha1,
    Algorithm::Md5,
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    adapter: Spec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    name: String,
    scheme: String,
    pattern: String,
    record: String,
    resolved: String,
    files: String,
    path: String,
    size: Option<String>,
    url: String,
    #[serde(default)]
    hash: BTreeMap<String, String>,
    #[serde(default)]
    strip: BTreeMap<String, String>,
    #[serde(default)]
    meta: MetaSpec,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetaSpec {
    title: Option<String>,
    version: Option<String>,
    license: Option<String>,
    doi: Option<String>,
}

struct Query {
    text: String,
    path: JsonPath,
}

impl Query {
    fn parse(text: &str) -> Result<Self, SourceError> {
        JsonPath::parse(text)
            .map(|path| Self {
                text: text.to_owned(),
                path,
            })
            .map_err(|error| adapter(format!("`{text}` is not a JSONPath: {error}")))
    }

    fn optional<'v>(&self, value: &'v Value) -> Result<Option<&'v Value>, &'static str> {
        self.path
            .query(value)
            .at_most_one()
            .map_err(|_| "more than one value")
    }

    fn required<'v>(&self, value: &'v Value) -> Result<&'v Value, &'static str> {
        self.optional(value)?.ok_or("no value")
    }
}

enum Part {
    Literal(String),
    Value(Query),
}

struct Hash {
    algorithm: Algorithm,
    query: Query,
    strip: Option<String>,
}

#[derive(Default)]
struct MetaQueries {
    title: Option<Query>,
    version: Option<Query>,
    license: Option<Query>,
    doi: Option<Query>,
}

/// A source described by a TOML adapter file: the schemes it serves, the record to fetch for a
/// reference, and the `JSONPath`s that read the files and metadata out of that record.
pub struct Declarative {
    name: String,
    schemes: Vec<String>,
    record: String,
    resolved: Vec<Part>,
    resolved_text: String,
    files: Query,
    path: Query,
    size: Option<Query>,
    url: Query,
    hashes: Vec<Hash>,
    meta: MetaQueries,
    client: Arc<Client>,
}

impl Declarative {
    /// Reads an adapter file.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Adapter`] when the text is not TOML, names a key the adapter
    /// format does not have, uses a pattern other than `{id}`, or holds an invalid `JSONPath`.
    pub fn from_toml(text: &str, client: Arc<Client>) -> Result<Self, SourceError> {
        let File { adapter: spec } =
            toml::from_str(text).map_err(|error| adapter(error.message().to_owned()))?;
        if spec.pattern != "{id}" {
            return Err(adapter(format!(
                "the pattern `{}` is not supported, only `{{id}}` is",
                spec.pattern
            )));
        }
        if let Some(algorithm) = spec
            .strip
            .keys()
            .find(|name| !spec.hash.contains_key(*name))
        {
            return Err(adapter(format!(
                "`strip` names `{algorithm}`, which `hash` does not"
            )));
        }
        let hashes = spec
            .hash
            .iter()
            .map(|(name, path)| {
                let algorithm = ALGORITHMS
                    .into_iter()
                    .find(|algorithm| algorithm.name() == name)
                    .ok_or_else(|| adapter(format!("`{name}` is not a hash algorithm")))?;
                Ok(Hash {
                    algorithm,
                    query: Query::parse(path)?,
                    strip: spec.strip.get(name).cloned(),
                })
            })
            .collect::<Result<Vec<_>, SourceError>>()?;
        let optional = |text: &Option<String>| text.as_deref().map(Query::parse).transpose();
        Ok(Self {
            schemes: vec![spec.scheme],
            record: spec.record,
            resolved: template(&spec.resolved)?,
            resolved_text: spec.resolved,
            files: Query::parse(&spec.files)?,
            path: Query::parse(&spec.path)?,
            size: optional(&spec.size)?,
            url: Query::parse(&spec.url)?,
            hashes,
            meta: MetaQueries {
                title: optional(&spec.meta.title)?,
                version: optional(&spec.meta.version)?,
                license: optional(&spec.meta.license)?,
                doi: optional(&spec.meta.doi)?,
            },
            name: spec.name,
            client,
        })
    }

    /// The adapter's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    async fn listing(&self, reference: &Reference) -> Result<Listing, SourceError> {
        let Target::Scheme { body, .. } = reference.target() else {
            return Err(SourceError::Unresolved {
                reference: reference.as_str().to_owned(),
            });
        };
        let invalid = |reason: &str| SourceError::Invalid {
            reference: reference.as_str().to_owned(),
            reason: reason.to_owned(),
        };
        if body.is_empty()
            || !body
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(invalid(
                "the identifier must be made of letters, digits, dots, underscores and hyphens",
            ));
        }
        let location = SafeUrl::parse(&self.record.replace("{id}", body))
            .map_err(|error| invalid(&error.to_string()))?;
        let record = record::fetch(&self.client, &location, reference.as_str()).await?;
        self.read(&record, &location)
    }

    fn read(&self, record: &Value, location: &SafeUrl) -> Result<Listing, SourceError> {
        let fail = |query: &Query, problem| SourceError::Record {
            path: query.text.clone(),
            url: location.clone(),
            problem,
        };
        let resolved = self.render(record, location)?;
        let mut entries = Vec::new();
        for file in self.files.path.query(record) {
            entries.push(self.entry(file, &fail)?);
        }
        Ok(Listing {
            resolved,
            meta: self.meta(record),
            entries,
        })
    }

    fn render(&self, record: &Value, location: &SafeUrl) -> Result<Reference, SourceError> {
        let fail = |path: &str, problem| SourceError::Record {
            path: path.to_owned(),
            url: location.clone(),
            problem,
        };
        let mut text = String::new();
        for part in &self.resolved {
            match part {
                Part::Literal(literal) => text.push_str(literal),
                Part::Value(query) => {
                    let value = query
                        .required(record)
                        .map_err(|problem| fail(&query.text, problem))?;
                    text.push_str(&plain(value).ok_or_else(|| {
                        fail(&query.text, "a value that is not text or a number")
                    })?);
                }
            }
        }
        text.parse()
            .map_err(|_| fail(&self.resolved_text, "a reference that is not valid"))
    }

    fn entry(
        &self,
        file: &Value,
        fail: &dyn Fn(&Query, &'static str) -> SourceError,
    ) -> Result<Entry, SourceError> {
        let text = |query: &Query| -> Result<String, SourceError> {
            let value = query
                .required(file)
                .map_err(|problem| fail(query, problem))?;
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| fail(query, "a value that is not text"))
        };
        let path = DataPath::new(&text(&self.path)?)
            .map_err(|_| fail(&self.path, "a value that is not a valid path"))?;
        let url = SafeUrl::parse(&text(&self.url)?)
            .map_err(|_| fail(&self.url, "a value that is not a URL"))?;
        let size = match &self.size {
            None => None,
            Some(query) => match query
                .optional(file)
                .map_err(|problem| fail(query, problem))?
            {
                None => None,
                Some(value) => Some(
                    value
                        .as_u64()
                        .ok_or_else(|| fail(query, "a value that is not a whole number"))?,
                ),
            },
        };
        let mut stated = Vec::new();
        for hash in &self.hashes {
            let Some(value) = hash
                .query
                .optional(file)
                .map_err(|problem| fail(&hash.query, problem))?
            else {
                continue;
            };
            let full = value
                .as_str()
                .ok_or_else(|| fail(&hash.query, "a value that is not text"))?;
            let hex = hash
                .strip
                .as_deref()
                .map_or(full, |prefix| full.strip_prefix(prefix).unwrap_or(full));
            stated.push(
                Digest::from_hex(hash.algorithm, hex)
                    .map_err(|_| fail(&hash.query, "a value that is not a valid digest"))?,
            );
        }
        Ok(Entry {
            path,
            size,
            stated,
            urls: vec![url],
        })
    }

    fn meta(&self, record: &Value) -> Meta {
        let read = |query: &Option<Query>| {
            query
                .as_ref()
                .and_then(|query| query.optional(record).ok().flatten())
                .and_then(plain)
        };
        Meta {
            title: read(&self.meta.title),
            version: read(&self.meta.version),
            license: read(&self.meta.license),
            doi: read(&self.meta.doi),
        }
    }
}

impl Source for Declarative {
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

fn adapter(reason: String) -> SourceError {
    SourceError::Adapter { reason }
}

fn plain(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn template(text: &str) -> Result<Vec<Part>, SourceError> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{$") {
        let end = rest
            .find('}')
            .filter(|end| *end > start)
            .ok_or_else(|| adapter(format!("`{text}` has a `{{$` that is never closed")))?;
        if start > 0 {
            parts.push(Part::Literal(rest[..start].to_owned()));
        }
        parts.push(Part::Value(Query::parse(&rest[start + 1..end])?));
        rest = &rest[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(Part::Literal(rest.to_owned()));
    }
    Ok(parts)
}
