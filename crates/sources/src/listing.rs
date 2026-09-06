//! One listing shape, described by an endpoint and a field mapping, and the
//! providers described against it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::Degradation;
use fetchloom_engine::digest::InteropDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, Listing, ListingEntry, Revalidated, Served, Serves, Source, SourceIdentity,
    SourceMetadata, Validator,
};
use fetchloom_engine::work::WorkCounter;

use crate::delegate::delegating_source;
use crate::http::{HttpBody, HttpSource, Method, check_fetch_status, header};

pub(crate) struct Field(pub &'static [&'static str]);

impl Field {
    fn at<'a>(&self, document: &'a serde_json::Value) -> Option<&'a serde_json::Value> {
        let mut here = document;
        for step in self.0 {
            here = here.get(step)?;
        }
        Some(here)
    }

    fn text<'a>(&self, document: &'a serde_json::Value) -> Option<&'a str> {
        self.at(document)?.as_str()
    }

    fn count(&self, document: &serde_json::Value) -> Option<u64> {
        let held = self.at(document)?;
        match held.as_u64() {
            Some(number) => Some(number),
            None => held.as_str()?.parse().ok(),
        }
    }
}

pub(crate) enum Held {
    Each(Field),
    One(Field),
}

pub(crate) enum Naming {
    Both {
        name: Field,
        location: Field,
    },
    FromLocation {
        location: Field,
    },
    Building {
        name: Field,
        build: fn(&Record, &str, &serde_json::Value) -> Option<String>,
    },
}

pub(crate) enum Stated {
    Nothing,
    Prefixed(Field),
    Labelled { algorithm: Field, value: Field },
}

pub(crate) enum Reaching {
    Fixed(&'static str),
    Named,
}

pub(crate) enum Identifying {
    Segments(usize),
    Peeled,
}

pub(crate) struct Described {
    pub scheme: &'static str,
    pub reaching: Reaching,
    pub identifying: Identifying,
    pub records: fn(&Record) -> String,
    pub held: Held,
    pub naming: Naming,
    pub folder: Option<Field>,
    pub sized: Field,
    pub stated: Stated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub origin: String,
    pub identifier: String,
    pub revision: Option<String>,
}

fn wanted(described: &Described) -> String {
    let identifier = match described.identifying {
        Identifying::Segments(count) => vec!["part"; count].join("/"),
        Identifying::Peeled => "record".to_owned(),
    };
    match described.reaching {
        Reaching::Fixed(_) => format!("{}:{identifier}", described.scheme),
        Reaching::Named => format!("{}:host/{identifier}", described.scheme),
    }
}

fn unreadable(described: &Described, reference: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "write it as {}, because {}: names {reference} and this build cannot tell what that is",
            wanted(described),
            described.scheme
        ),
    )
    .with_source(reference)
}

fn malformed(location: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name the files instead of the record, because {} answered with a listing this build cannot read",
            SafeUrl::new(location)
        ),
    )
    .with_source(location)
}

fn outside(location: &str, path: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name a record that lists its own files, because {} listed {} as a member, which is not one path under the record",
            SafeUrl::new(location),
            SafeUrl::new(path)
        ),
    )
    .with_source(location)
}

fn file_name_of(location: &str) -> &str {
    let without_query = location.split(['?', '#']).next().unwrap_or(location);
    without_query
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(without_query)
}

fn interop_of(prefixed: &str) -> Option<InteropDigest> {
    let hex = prefixed.strip_prefix("sha256:")?;
    format!("sha256:{}", hex.to_ascii_lowercase()).parse().ok()
}

fn labelled_interop(algorithm: &str, value: &str) -> Option<InteropDigest> {
    if !algorithm.eq_ignore_ascii_case("sha-256") && !algorithm.eq_ignore_ascii_case("sha256") {
        return None;
    }
    interop_of(&format!("sha256:{value}"))
}

fn trimmed(reference: &str) -> &str {
    reference.trim_end_matches('/')
}

pub struct DescribedSource {
    described: &'static Described,
    http: HttpSource,
    origin: String,
    resolved: Mutex<HashMap<String, String>>,
}

impl DescribedSource {
    #[must_use]
    pub(crate) fn new(
        described: &'static Described,
        limits: Limits,
        work: Arc<WorkCounter>,
    ) -> Self {
        Self {
            described,
            http: HttpSource::new(limits, work),
            origin: String::new(),
            resolved: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub(crate) fn reaching(
        described: &'static Described,
        origin: impl Into<String>,
        limits: Limits,
        work: Arc<WorkCounter>,
    ) -> Self {
        Self {
            described,
            http: HttpSource::new(limits, work),
            origin: origin.into(),
            resolved: Mutex::new(HashMap::new()),
        }
    }

    fn scheme(&self) -> &str {
        self.described.scheme
    }

    fn serving(&self, reference: &str) -> Serves {
        match (&self.described.identifying, self.record_of(reference)) {
            (Identifying::Segments(count), Ok(record))
                if segments_of(&record.identifier) > *count =>
            {
                Serves::Object
            }
            _ => Serves::Container,
        }
    }

    fn record_of(&self, reference: &str) -> Result<Record, Error> {
        let rest = reference
            .strip_prefix(self.described.scheme)
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or_else(|| unreadable(self.described, reference))?;
        let (rest, revision) = match rest.rsplit_once('@') {
            Some((before, after)) if !before.is_empty() && !after.is_empty() => {
                (before, Some(after.to_owned()))
            }
            _ => (rest, None),
        };
        let (origin, identifier) = match self.described.reaching {
            Reaching::Fixed(fixed) => {
                let origin = if self.origin.is_empty() {
                    fixed.to_owned()
                } else {
                    self.origin.clone()
                };
                (origin, rest.to_owned())
            }
            Reaching::Named if self.origin.is_empty() => {
                let (host, tail) = rest
                    .split_once('/')
                    .ok_or_else(|| unreadable(self.described, reference))?;
                if host.is_empty() || tail.is_empty() || host.contains(':') {
                    return Err(unreadable(self.described, reference));
                }
                (format!("https://{host}"), tail.to_owned())
            }
            Reaching::Named => (self.origin.clone(), rest.to_owned()),
        };
        if identifier.is_empty() {
            return Err(unreadable(self.described, reference));
        }
        Ok(Record {
            origin,
            identifier,
            revision,
        })
    }

    fn splits(&self, reference: &str) -> Result<Vec<(String, String)>, Error> {
        let record = self.record_of(reference)?;
        let segments: Vec<&str> = record
            .identifier
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let owned = match self.described.identifying {
            Identifying::Segments(count) => vec![count],
            Identifying::Peeled => {
                let mut order: Vec<usize> = (1..segments.len()).rev().collect();
                order.push(segments.len().max(1));
                order
            }
        };
        let mut candidates = Vec::new();
        for held in owned {
            if segments.len() < held {
                return Err(unreadable(self.described, reference));
            }
            candidates.push((
                self.container_of(&record, &segments[..held].join("/")),
                segments[held..].join("/"),
            ));
        }
        Ok(candidates)
    }

    fn container_of(&self, record: &Record, head: &str) -> String {
        let named = match self.described.reaching {
            Reaching::Named if self.origin.is_empty() => format!(
                "{}:{}/{head}",
                self.described.scheme,
                record
                    .origin
                    .strip_prefix("https://")
                    .unwrap_or(&record.origin)
            ),
            Reaching::Fixed(_) | Reaching::Named => {
                format!("{}:{head}", self.described.scheme)
            }
        };
        match &record.revision {
            Some(revision) => format!("{named}@{revision}"),
            None => named,
        }
    }

    fn remembered(&self, reference: &str) -> Option<String> {
        self.resolved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(reference)
            .cloned()
    }

    fn located(&self, reference: &str, credential: Option<&Credential>) -> Result<String, Error> {
        if let Some(known) = self.remembered(reference) {
            return Ok(known);
        }
        let candidates = self.splits(reference)?;
        let mut last = None;
        for (container, file) in &candidates {
            if file.is_empty() {
                let record = self.record_of(reference)?;
                return Ok((self.described.records)(&record));
            }
            match self.listed(container, credential) {
                Ok(_) => {
                    if let Some(known) = self.remembered(reference) {
                        return Ok(known);
                    }
                }
                Err(reason) => last = Some(reason),
            }
        }
        Err(last.unwrap_or_else(|| {
            Error::new(
                ErrorKind::ReferenceUnresolved,
                format!(
                    "name a file one of its records holds, because no record this reference could name states one called {}",
                    candidates
                        .last()
                        .map_or("", |(_, file)| file.as_str())
                ),
            )
            .with_source(reference)
        }))
    }

    fn metadata(
        &self,
        served: &str,
        answer: &ureq::http::Response<ureq::Body>,
        time_to_first_byte: std::time::Duration,
        reference: &str,
    ) -> SourceMetadata {
        let fields = crate::http::common_fields(answer);
        let record = self.record_of(reference).ok();
        let identity = match record.as_ref().and_then(|held| held.revision.clone()) {
            Some(revision) => SourceIdentity::ImmutableVersion(revision),
            None => crate::http::identity_of(header(answer, "etag").as_deref()),
        };
        let origin = record
            .as_ref()
            .map_or(self.origin.as_str(), |held| held.origin.as_str());
        SourceMetadata {
            location: SafeUrl::new(served),
            host: Host::of_location(origin),
            size: fields.size,
            content: None,
            interop: None,
            identity,
            last_modified: fields.last_modified,
            supports_ranges: fields.supports_ranges,
            time_to_first_byte,
            retry_after: fields.retry_after,
            cost: Cost::default(),
        }
    }

    fn listed(&self, reference: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        let record = self.record_of(reference)?;
        if let Identifying::Segments(count) = self.described.identifying
            && segments_of(&record.identifier) != count
        {
            return Err(unreadable(self.described, reference));
        }
        let endpoint = (self.described.records)(&record);
        let body = self.answered(&endpoint, credential)?;
        let document: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| malformed(&endpoint))?;
        let files = self.files_in(&document, &endpoint)?;
        let mut entries = Vec::new();
        for file in &files {
            let (path, location) = self.named(&record, file, &endpoint)?;
            if !is_under_the_record(&path) {
                return Err(outside(&endpoint, &path));
            }
            self.resolved
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(format!("{}/{path}", trimmed(reference)), location.clone());
            entries.push(ListingEntry {
                location: SafeUrl::new(&location),
                path,
                size: self.described.sized.count(file),
                content: None,
                interop: self.claimed(file),
            });
        }
        entries.sort_by(|one, other| one.path.as_bytes().cmp(other.path.as_bytes()));
        let limits = self.http.limits();
        let allowed = usize::try_from(limits.listing_entries).unwrap_or(usize::MAX);
        if entries.len() > allowed {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "ask for a narrower reference, because {} lists more than the {} entries a run reads",
                    SafeUrl::new(&endpoint),
                    limits.listing_entries
                ),
            )
            .with_source(&endpoint));
        }
        Ok(Listing {
            entries,
            skipped: 0,
        })
    }

    fn files_in(
        &self,
        document: &serde_json::Value,
        endpoint: &str,
    ) -> Result<Vec<serde_json::Value>, Error> {
        match &self.described.held {
            Held::Each(field) => Ok(field
                .at(document)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| malformed(endpoint))?
                .clone()),
            Held::One(field) => Ok(vec![
                field
                    .at(document)
                    .ok_or_else(|| malformed(endpoint))?
                    .clone(),
            ]),
        }
    }

    fn named(
        &self,
        record: &Record,
        file: &serde_json::Value,
        endpoint: &str,
    ) -> Result<(String, String), Error> {
        let (name, location) = match &self.described.naming {
            Naming::Both { name, location } => (
                name.text(file)
                    .ok_or_else(|| malformed(endpoint))?
                    .to_owned(),
                location
                    .text(file)
                    .ok_or_else(|| malformed(endpoint))?
                    .to_owned(),
            ),
            Naming::FromLocation { location } => {
                let held = location
                    .text(file)
                    .ok_or_else(|| malformed(endpoint))?
                    .to_owned();
                (file_name_of(&held).to_owned(), held)
            }
            Naming::Building { name, build } => {
                let held = name.text(file).ok_or_else(|| malformed(endpoint))?;
                let built = build(record, held, file).ok_or_else(|| malformed(endpoint))?;
                (held.to_owned(), built)
            }
        };
        if name.is_empty() || location.is_empty() {
            return Err(malformed(endpoint));
        }
        let path = match self.described.folder.as_ref().and_then(|at| at.text(file)) {
            Some(folder) if !folder.is_empty() => format!("{folder}/{name}"),
            _ => name,
        };
        Ok((path, location))
    }

    fn claimed(&self, file: &serde_json::Value) -> Option<InteropDigest> {
        match &self.described.stated {
            Stated::Nothing => None,
            Stated::Prefixed(field) => interop_of(field.text(file)?),
            Stated::Labelled { algorithm, value } => {
                labelled_interop(algorithm.text(file)?, value.text(file)?)
            }
        }
    }

    fn answered(&self, endpoint: &str, credential: Option<&Credential>) -> Result<String, Error> {
        let (answer, _served) = self.http.send(Method::Get, endpoint, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                endpoint,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let limits = self.http.limits();
        answer
            .into_body()
            .into_with_config()
            .limit(limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| crate::http::transport_failure(endpoint, &reason))
    }
}

fn segments_of(identifier: &str) -> usize {
    identifier
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}

fn is_under_the_record(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains("://")
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

delegating_source!(DescribedSource);
