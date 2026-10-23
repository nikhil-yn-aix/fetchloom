//! The declarative adapter: a TOML file describes the references a repository takes, where
//! their records and file listings live, how listings page, and where in the JSON the files
//! and metadata sit.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Arc;

use fl_model::digest::{Algorithm, Digest};
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use fl_transfer::credentials::Variable;
use fl_transfer::fetch::Request;
use fl_transfer::url::{Origin, SafeUrl};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::license;
use crate::listing::{Entry, Listing, Meta};
use crate::paging::{self, Paging};
use crate::record;
use crate::source::{Hit, Read, Searchable, Source, SourceError};
use crate::template::{Context, Missing, Parts, Pattern, Query, Template, escape, plain, word};

/// The most folders one listing may walk into.
const FOLDERS: usize = 10_000;

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
    #[serde(default)]
    forms: Vec<FormSpec>,
    resolved: String,
    #[serde(default)]
    landing: Vec<String>,
    #[serde(default)]
    capture: BTreeMap<String, String>,
    listing: Option<String>,
    files: String,
    folders: Option<String>,
    descend: Option<String>,
    path: OneOrMany,
    #[serde(default)]
    size: OneOrMany,
    url: OneOrMany,
    #[serde(default)]
    digest: OneOrMany,
    #[serde(default)]
    paginate: PagingSpec,
    auth: Option<AuthSpec>,
    search: Option<SearchSpec>,
    #[serde(default)]
    meta: MetaSpec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormSpec {
    pattern: String,
    record: String,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn texts(&self) -> &[String] {
        match self {
            Self::None => &[],
            Self::One(text) => std::slice::from_ref(text),
            Self::Many(texts) => texts,
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum PagingSpec {
    #[default]
    None,
    Page {
        param: String,
        #[serde(default = "one")]
        start: u64,
        size: Option<usize>,
    },
    Cursor {
        param: String,
        field: String,
    },
    Link,
    Next {
        field: String,
    },
}

const fn one() -> u64 {
    1
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthSpec {
    header: String,
    format: String,
    env: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchSpec {
    url: String,
    items: String,
    #[serde(rename = "ref")]
    reference: String,
    title: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetaSpec {
    title: Option<String>,
    version: Option<String>,
    license: Option<String>,
    doi: Option<String>,
    creators: Option<String>,
}

struct Form {
    pattern: Pattern,
    record: Template,
}

struct Auth {
    authority: String,
    header: String,
    format: String,
    env: String,
}

struct Search {
    url: Template,
    items: Query,
    reference: Template,
    title: Option<Query>,
}

struct MetaQueries {
    title: Option<Query>,
    version: Option<Query>,
    license: Option<Query>,
    doi: Option<Query>,
    creators: Option<Query>,
}

/// A source described by a TOML adapter file: the patterns its references take, the record
/// and listing to fetch for one, how the listing pages, and the `JSONPath`s that read files
/// and metadata out of the JSON.
pub struct Declarative {
    name: String,
    schemes: Vec<String>,
    forms: Vec<Form>,
    resolved: Template,
    landing: Vec<Pattern>,
    capture: Vec<(String, Query)>,
    listing: Option<Template>,
    files: Query,
    folders: Option<(Query, Template)>,
    paths: Vec<Template>,
    sizes: Vec<Query>,
    urls: Vec<Template>,
    digests: Vec<Template>,
    paging: Paging,
    auth: Option<Auth>,
    search: Option<Search>,
    meta: MetaQueries,
    origin: Option<Origin>,
    client: Arc<Client>,
}

impl Declarative {
    /// Reads an adapter file.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Adapter`] naming the key at fault when the text is not TOML,
    /// names a key the adapter format does not have, holds a pattern, template or `JSONPath`
    /// that cannot be read, or uses a part its pattern does not name.
    pub fn from_toml(text: &str, client: Arc<Client>) -> Result<Self, SourceError> {
        let File { adapter: spec } =
            toml::from_str(text).map_err(|error| adapter(error.message().to_owned()))?;
        let own = format!("{}:", spec.scheme);
        let foreign = std::iter::once(("resolved", &spec.resolved))
            .chain(
                spec.search
                    .iter()
                    .map(|search| ("search.ref", &search.reference)),
            )
            .find(|(_, text)| !text.starts_with(&own));
        if let Some((key, text)) = foreign {
            return Err(adapter(format!("`{key}` `{text}` must start with `{own}`")));
        }
        let forms = forms(&spec)?;
        let patterns: Vec<&Pattern> = forms.iter().map(|form| &form.pattern).collect();
        let captured: Vec<&str> = spec.capture.keys().map(String::as_str).collect();
        let known = |key: &str, template: Template| {
            names_known(key, &template, &patterns, &captured).map(|()| template)
        };
        let list = |key: &str, many: &OneOrMany, context| {
            many.texts()
                .iter()
                .map(|text| known(key, template(key, text, context)?))
                .collect::<Result<Vec<_>, SourceError>>()
        };
        let capture = spec
            .capture
            .iter()
            .map(|(name, path)| Ok((name.clone(), query("capture", path)?)))
            .collect::<Result<_, SourceError>>()?;
        let listing = spec
            .listing
            .as_deref()
            .map(|text| known("listing", template("listing", text, Context::Url)?))
            .transpose()?;
        let folders = match (&spec.folders, &spec.descend) {
            (None, None) => None,
            (Some(folders), Some(descend)) => Some((
                query("folders", folders)?,
                known("descend", template("descend", descend, Context::Url)?)?,
            )),
            _ => return Err(adapter("`folders` and `descend` go together".to_owned())),
        };
        let paths = list("path", &spec.path, Context::Text)?;
        let urls = list("url", &spec.url, Context::Url)?;
        if paths.is_empty() || urls.is_empty() {
            return Err(adapter("`path` and `url` each need a template".to_owned()));
        }
        let origin = fixed_origin(&spec.record);
        Ok(Self {
            resolved: known(
                "resolved",
                template("resolved", &spec.resolved, Context::Text)?,
            )?,
            landing: landing(&spec.landing, &forms)?,
            digests: list("digest", &spec.digest, Context::Text)?,
            sizes: spec
                .size
                .texts()
                .iter()
                .map(|text| query("size", text))
                .collect::<Result<_, _>>()?,
            files: query("files", &spec.files)?,
            meta: MetaQueries {
                title: optional("meta.title", spec.meta.title.as_deref())?,
                version: optional("meta.version", spec.meta.version.as_deref())?,
                license: optional("meta.license", spec.meta.license.as_deref())?,
                doi: optional("meta.doi", spec.meta.doi.as_deref())?,
                creators: optional("meta.creators", spec.meta.creators.as_deref())?,
            },
            paging: paging(spec.paginate)?,
            auth: auth(spec.auth, origin.as_ref())?,
            search: search(spec.search)?,
            name: spec.name,
            schemes: vec![spec.scheme],
            forms,
            capture,
            listing,
            folders,
            paths,
            urls,
            origin,
            client,
        })
    }

    /// The adapter's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The origin that serves the adapter's records when the adapter names a fixed host,
    /// which only whoever installed the adapter can trust with private addresses.
    #[must_use]
    pub const fn record_origin(&self) -> Option<&Origin> {
        self.origin.as_ref()
    }

    /// Lists `record`, a record of `reference` already in hand, as the adapter reads the first
    /// page it fetches: the parts its pattern takes and captures, the resolved reference, the
    /// metadata and the files the record holds. Folders are not walked and no request is made.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Invalid`] when the reference matches no pattern, and
    /// [`SourceError::Record`] when the record lacks a value the adapter needs.
    pub fn read(&self, reference: &Reference, record: &Value) -> Result<Listing, SourceError> {
        let (form, mut parts) = self.form(reference)?;
        let url = locate(&form.record, &parts, &Value::Null, reference.as_str())?;
        let mut done = Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries: Vec::new(),
        };
        self.absorb(&mut parts, &mut done, record, &url)?;
        for file in self.files.all(record) {
            done.entries.push(self.entry(file, &parts, &url)?);
        }
        Ok(done)
    }

    fn form(&self, reference: &Reference) -> Result<(&Form, Parts), SourceError> {
        let Target::Scheme { body, .. } = reference.target() else {
            return Err(SourceError::Unresolved {
                reference: reference.as_str().to_owned(),
            });
        };
        self.forms
            .iter()
            .find_map(|form| form.pattern.matches(body).map(|parts| (form, parts)))
            .ok_or_else(|| {
                let patterns: Vec<&str> =
                    self.forms.iter().map(|form| form.pattern.text()).collect();
                SourceError::Invalid {
                    reference: reference.as_str().to_owned(),
                    reason: format!("it does not match {}", patterns.join(" or ")),
                }
            })
    }

    async fn listing(&self, reference: &Reference, written: bool) -> Result<Listing, SourceError> {
        let raw = reference.as_str();
        let (form, mut parts) = self.form(reference)?;
        let typed = written && form.pattern.has_host();
        let location = locate(&form.record, &parts, &Value::Null, raw)?;
        let trusted = location.origin().filter(|_| typed);
        let mut done = Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries: Vec::new(),
        };
        let mut first = true;
        let start = match &self.listing {
            None => location,
            Some(listing) => {
                let request = Request {
                    url: &location,
                    follow: true,
                    typed,
                };
                let (page, record) = record::json(&self.client, &request, raw).await?;
                self.absorb(&mut parts, &mut done, &record, &page.url)?;
                first = false;
                locate(listing, &parts, &record, raw)?
            }
        };
        let mut queue = VecDeque::from([start]);
        let mut walked = HashSet::new();
        while let Some(url) = queue.pop_front() {
            if !walked.insert(url.clone()) {
                continue;
            }
            if walked.len() > FOLDERS {
                return Err(SourceError::Body {
                    url,
                    reason: format!("holds more than {FOLDERS} folders"),
                });
            }
            paging::walk(
                &self.client,
                &url,
                trusted.as_ref(),
                &self.paging,
                raw,
                |body, page| {
                    if std::mem::take(&mut first) {
                        self.absorb(&mut parts, &mut done, body, page)?;
                    }
                    let files = self.files.all(body);
                    for file in &files {
                        done.entries.push(self.entry(file, &parts, page)?);
                    }
                    let mut items = files.len();
                    if let Some((folders, descend)) = &self.folders {
                        for folder in folders.all(body) {
                            let text = descend
                                .render(&parts, folder)
                                .map_err(|missing| shape(page, missing))?;
                            let next = page.join(&text).map_err(|_| SourceError::Record {
                                path: descend.text().to_owned(),
                                url: page.clone(),
                                problem: "a value that is not a URL",
                            })?;
                            queue.push_back(next);
                            items += 1;
                        }
                    }
                    Ok(items)
                },
            )
            .await?;
        }
        Ok(done)
    }

    fn absorb(
        &self,
        parts: &mut Parts,
        done: &mut Listing,
        record: &Value,
        url: &SafeUrl,
    ) -> Result<(), SourceError> {
        for (name, query) in &self.capture {
            if parts.contains_key(name) {
                continue;
            }
            let fail = |problem| SourceError::Record {
                path: query.text.clone(),
                url: url.clone(),
                problem,
            };
            let value = query
                .optional(record)
                .map_err(fail)?
                .and_then(plain)
                .ok_or_else(|| fail("no value that is text or a number"))?;
            if !word(&value) {
                return Err(fail("a value a reference part cannot hold"));
            }
            parts.insert(name.clone(), value);
        }
        done.resolved = self.resolved(parts, record, url)?;
        done.meta = self.meta(record);
        Ok(())
    }

    fn resolved(
        &self,
        parts: &Parts,
        record: &Value,
        url: &SafeUrl,
    ) -> Result<Reference, SourceError> {
        let text = self
            .resolved
            .render(parts, record)
            .map_err(|missing| shape(url, missing))?;
        text.parse().map_err(|_| SourceError::Record {
            path: self.resolved.text().to_owned(),
            url: url.clone(),
            problem: "a reference that is not valid",
        })
    }

    fn entry(&self, file: &Value, parts: &Parts, url: &SafeUrl) -> Result<Entry, SourceError> {
        let fail = |path: &str, problem| SourceError::Record {
            path: path.to_owned(),
            url: url.clone(),
            problem,
        };
        let path = first(&self.paths, parts, file).map_err(|missing| shape(url, missing))?;
        let path = DataPath::normalize(path.trim_start_matches('/'))
            .map_err(|_| fail(self.paths[0].text(), "a value that is not a valid path"))?;
        let location = first(&self.urls, parts, file).map_err(|missing| shape(url, missing))?;
        let location = url
            .join(&location)
            .ok()
            .filter(|location| matches!(location.scheme(), "http" | "https"))
            .ok_or_else(|| {
                fail(
                    self.urls[0].text(),
                    "a value that is not an http or https URL",
                )
            })?;
        let mut size = None;
        for query in &self.sizes {
            match query
                .optional(file)
                .map_err(|problem| fail(&query.text, problem))?
            {
                None | Some(Value::Null) => {}
                Some(Value::String(text)) if text.is_empty() => {}
                Some(value) => {
                    let whole = value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
                        .ok_or_else(|| fail(&query.text, "a value that is not a whole number"))?;
                    size = Some(whole);
                    break;
                }
            }
        }
        let mut stated = Vec::new();
        for template in &self.digests {
            let Ok(text) = template.render(parts, file) else {
                continue;
            };
            let Some((name, hex)) = text.split_once(':').filter(|(_, hex)| !hex.is_empty()) else {
                continue;
            };
            let name = name.replace('-', "").to_ascii_lowercase();
            let Some(algorithm) = ALGORITHMS
                .into_iter()
                .find(|algorithm| algorithm.name() == name)
            else {
                continue;
            };
            let Ok(digest) = Digest::from_hex(algorithm, &hex.to_ascii_lowercase()) else {
                continue;
            };
            if !stated.contains(&digest) {
                stated.push(digest);
            }
        }
        Ok(Entry {
            path,
            size,
            stated,
            urls: vec![location],
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
            license: read(&self.meta.license).map(|text| license::spdx(&text)),
            doi: read(&self.meta.doi),
            creators: self
                .meta
                .creators
                .as_ref()
                .map(|query| query.all(record).into_iter().filter_map(plain).collect())
                .unwrap_or_default(),
        }
    }

    async fn hits(&self, query: &str, limit: usize) -> Result<Vec<Hit>, SourceError> {
        let Some(search) = &self.search else {
            return Ok(Vec::new());
        };
        let mut parts = Parts::new();
        let mut escaped = String::new();
        escape(query, &mut escaped);
        parts.insert("query".to_owned(), escaped);
        parts.insert("limit".to_owned(), limit.to_string());
        let url = locate(&search.url, &parts, &Value::Null, query)?;
        let request = Request {
            url: &url,
            follow: true,
            typed: false,
        };
        let (page, body) = record::json(&self.client, &request, query).await?;
        search
            .items
            .all(&body)
            .into_iter()
            .take(limit)
            .map(|item| {
                let text = search
                    .reference
                    .render(&parts, item)
                    .map_err(|missing| shape(&page.url, missing))?;
                let reference = text.parse().map_err(|_| SourceError::Record {
                    path: search.reference.text().to_owned(),
                    url: page.url.clone(),
                    problem: "a reference that is not valid",
                })?;
                let title = search
                    .title
                    .as_ref()
                    .and_then(|query| query.optional(item).ok().flatten())
                    .and_then(plain);
                Ok(Hit { reference, title })
            })
            .collect()
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
        Box::pin(self.listing(reference, true))
    }

    fn resolve_routed<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(self.listing(reference, false))
    }

    fn credential(&self, read: Read) -> Option<Variable> {
        self.auth.as_ref().map(|auth| {
            Variable::new(
                &auth.authority,
                &auth.env,
                &auth.header,
                &auth.format,
                move |name| read(name),
            )
        })
    }

    fn searchable(&self) -> Option<&dyn Searchable> {
        self.search.as_ref().map(|_| self as &dyn Searchable)
    }

    fn landing(&self, url: &str) -> Option<Reference> {
        let parts = self
            .landing
            .iter()
            .find_map(|pattern| pattern.matches(url))?;
        let body = self
            .forms
            .iter()
            .filter(|form| form.pattern.names().all(|name| parts.contains_key(name)))
            .max_by_key(|form| form.pattern.names().count())?
            .pattern
            .render(&parts)?;
        format!("{}:{body}", self.schemes[0]).parse().ok()
    }
}

impl Searchable for Declarative {
    fn search<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<Hit>, SourceError>> {
        Box::pin(self.hits(query, limit))
    }
}

fn locate(
    template: &Template,
    parts: &Parts,
    record: &Value,
    reference: &str,
) -> Result<SafeUrl, SourceError> {
    let text = template
        .render(parts, record)
        .map_err(|missing| match missing {
            Missing::Part(name) => adapter(format!("`{}` names no part `{name}`", template.text())),
            Missing::Value(path, problem) => SourceError::Invalid {
                reference: reference.to_owned(),
                reason: format!("its record has {problem} at {path}"),
            },
        })?;
    SafeUrl::parse(&text).map_err(|error| SourceError::Invalid {
        reference: reference.to_owned(),
        reason: error.to_string(),
    })
}

fn template(key: &str, text: &str, context: Context) -> Result<Template, SourceError> {
    Template::parse(text, context).map_err(|reason| adapter(format!("`{key}`: {reason}")))
}

fn query(key: &str, text: &str) -> Result<Query, SourceError> {
    Query::parse(text).map_err(|reason| adapter(format!("`{key}`: {reason}")))
}

fn optional(key: &str, text: Option<&str>) -> Result<Option<Query>, SourceError> {
    text.map(|text| query(key, text)).transpose()
}

fn forms(spec: &Spec) -> Result<Vec<Form>, SourceError> {
    std::iter::once((&spec.pattern, &spec.record))
        .chain(spec.forms.iter().map(|form| (&form.pattern, &form.record)))
        .map(|(pattern, record)| {
            let pattern = Pattern::parse(pattern)
                .map_err(|reason| adapter(format!("`pattern`: {reason}")))?;
            let record = template("record", record, Context::Url)?;
            if record.reads() {
                return Err(adapter(format!(
                    "`record` `{}` reads a value, which only later requests can",
                    record.text()
                )));
            }
            names_known("record", &record, &[&pattern], &[])?;
            Ok(Form { pattern, record })
        })
        .collect()
}

fn landing(texts: &[String], forms: &[Form]) -> Result<Vec<Pattern>, SourceError> {
    texts
        .iter()
        .map(|text| {
            let pattern =
                Pattern::parse(text).map_err(|reason| adapter(format!("`landing`: {reason}")))?;
            let fits = forms.iter().any(|form| {
                form.pattern
                    .names()
                    .all(|name| pattern.names().any(|taken| taken == name))
            });
            if !fits {
                return Err(adapter(format!(
                    "`landing` `{text}` takes no part set a pattern needs"
                )));
            }
            Ok(pattern)
        })
        .collect()
}

fn paging(spec: PagingSpec) -> Result<Paging, SourceError> {
    Ok(match spec {
        PagingSpec::None => Paging::None,
        PagingSpec::Page { param, start, size } => Paging::Page { param, start, size },
        PagingSpec::Cursor { param, field } => Paging::Cursor {
            param,
            field: query("paginate.field", &field)?,
        },
        PagingSpec::Link => Paging::Link,
        PagingSpec::Next { field } => Paging::Next {
            field: query("paginate.field", &field)?,
        },
    })
}

fn auth(spec: Option<AuthSpec>, origin: Option<&Origin>) -> Result<Option<Auth>, SourceError> {
    spec.map(|auth| {
        let authority = origin
            .map(Origin::authority)
            .ok_or_else(|| adapter("`auth` needs a `record` on a fixed host".to_owned()))?;
        if !auth.format.contains("{token}") {
            return Err(adapter("`auth.format` must hold `{token}`".to_owned()));
        }
        Ok(Auth {
            authority,
            header: auth.header,
            format: auth.format,
            env: auth.env,
        })
    })
    .transpose()
}

fn search(spec: Option<SearchSpec>) -> Result<Option<Search>, SourceError> {
    spec.map(|search| {
        Ok(Search {
            url: template("search.url", &search.url, Context::Url)?,
            items: query("search.items", &search.items)?,
            reference: template("search.ref", &search.reference, Context::Text)?,
            title: optional("search.title", search.title.as_deref())?,
        })
    })
    .transpose()
}

fn adapter(reason: String) -> SourceError {
    SourceError::Adapter { reason }
}

fn shape(url: &SafeUrl, missing: Missing) -> SourceError {
    match missing {
        Missing::Value(path, problem) => SourceError::Record {
            path,
            url: url.clone(),
            problem,
        },
        Missing::Part(name) => adapter(format!("a template names no part `{name}`")),
    }
}

/// The text of the first of `templates` that `parts` and `value` fill, or why the last one
/// could not be.
fn first(templates: &[Template], parts: &Parts, value: &Value) -> Result<String, Missing> {
    let mut last = Missing::Part(String::new());
    for template in templates {
        match template.render(parts, value) {
            Ok(text) => return Ok(text),
            Err(missing) => last = missing,
        }
    }
    Err(last)
}

fn names_known(
    key: &str,
    template: &Template,
    patterns: &[&Pattern],
    captured: &[&str],
) -> Result<(), SourceError> {
    for name in template.parts() {
        let taken = captured.contains(&name)
            || patterns
                .iter()
                .all(|pattern| pattern.names().any(|known| known == name));
        if !taken {
            return Err(adapter(format!(
                "`{key}` `{}` names `{{{name}}}`, which a pattern does not take",
                template.text()
            )));
        }
    }
    Ok(())
}

fn fixed_origin(record: &str) -> Option<Origin> {
    let prefix = &record[..record.find('{').unwrap_or(record.len())];
    let after = prefix.find("://")? + 3;
    prefix[after..].find('/')?;
    SafeUrl::parse(prefix).ok()?.origin()
}
