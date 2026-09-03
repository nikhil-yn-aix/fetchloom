//! The sources reached by a provider's own identifier rather than by a
//! location.

use std::sync::Arc;
use std::time::Instant;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::Degradation;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Listing, ListingEntry, Revalidated, Served, Serves, Source, SourceIdentity,
    SourceMetadata, Validator,
};
use fetchloom_engine::work::WorkCounter;

use crate::http::{HttpBody, HttpSource, Method, check_fetch_status, header};

/// What one provider identifier names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identifier {
    /// The repository, record, or dataset path the provider knows it by.
    pub path: String,
    /// The revision the reference pinned, when it pinned one.
    pub revision: Option<String>,
}

/// Splits a provider reference into the identifier it names and the revision it
/// pinned.
fn identifier_of(rest: &str) -> Identifier {
    match rest.rsplit_once('@') {
        Some((path, revision)) if !path.is_empty() && !revision.is_empty() => Identifier {
            path: path.to_owned(),
            revision: Some(revision.to_owned()),
        },
        _ => Identifier {
            path: rest.to_owned(),
            revision: None,
        },
    }
}

/// Returns the failure a reference this provider cannot read states.
fn unreadable(scheme: &str, reference: &str, wanted: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!("write it as {wanted}, because {scheme}: names {reference} and this build cannot tell what that is"),
    )
    .with_source(reference)
}

/// The provider that serves model and dataset repositories at a revision.
pub struct HuggingFaceSource {
    http: HttpSource,
    origin: String,
}

impl HuggingFaceSource {
    /// Builds a source holding no connections yet.
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: "https://huggingface.co".to_owned(),
        }
    }

    /// Builds a source reaching a stated host, which is how a test drives it
    /// without the public network.
    #[must_use]
    pub fn reaching(origin: impl Into<String>, limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: origin.into(),
        }
    }

    /// Splits a reference into the repository it names, the revision it pinned,
    /// and the file under that repository, when it names one.
    ///
    /// The repository is the first three path segments of a dataset and the
    /// first two of a model, which is the provider's own URL structure rather
    /// than a guess about where a name ends.
    fn parts(reference: &str) -> Result<(Identifier, String), Error> {
        let wanted = "hf:datasets/org/name or hf:org/model";
        let rest = reference
            .strip_prefix("hf:")
            .ok_or_else(|| unreadable("hf", reference, wanted))?;
        let held = identifier_of(rest);
        let segments: Vec<&str> = held
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let owned = if segments.first() == Some(&"datasets") {
            3
        } else {
            2
        };
        if segments.len() < owned {
            return Err(unreadable("hf", reference, wanted));
        }
        Ok((
            Identifier {
                path: segments[..owned].join("/"),
                revision: held.revision,
            },
            segments[owned..].join("/"),
        ))
    }

    /// Returns the location the provider serves one file of a repository at.
    fn resolve(&self, reference: &str) -> Result<String, Error> {
        let (held, file) = Self::parts(reference)?;
        let revision = held.revision.as_deref().unwrap_or("main");
        Ok(format!(
            "{}/{}/resolve/{revision}/{file}",
            self.origin, held.path
        ))
    }
}

/// The provider that serves one deposited record and the files it holds.
pub struct ZenodoSource {
    http: HttpSource,
    origin: String,
}

impl ZenodoSource {
    /// Builds a source holding no connections yet.
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: "https://zenodo.org".to_owned(),
        }
    }

    /// Builds a source reaching a stated host, which is how a test drives it
    /// without the public network.
    #[must_use]
    pub fn reaching(origin: impl Into<String>, limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: origin.into(),
        }
    }

    /// Returns the record identifier a reference names, which is the trailing
    /// digits of a deposit identifier or of a bare number.
    fn record_of(reference: &str) -> Result<String, Error> {
        let rest = reference
            .strip_prefix("zenodo:")
            .ok_or_else(|| unreadable("zenodo", reference, "zenodo:10.5281/zenodo.1234567"))?;
        let digits: String = rest
            .rsplit(['.', '/'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            return Err(unreadable(
                "zenodo",
                reference,
                "zenodo:10.5281/zenodo.1234567",
            ));
        }
        Ok(digits)
    }

    /// Returns the location the provider describes one record at.
    fn resolve(&self, reference: &str) -> Result<String, Error> {
        Ok(format!(
            "{}/api/records/{}",
            self.origin,
            Self::record_of(reference)?
        ))
    }
}

/// Reads the entries a record description names, taking each file's location
/// and length from what the record stated and nothing else.
fn record_entries(location: &str, body: &str) -> Result<Vec<ListingEntry>, Error> {
    let malformed = || {
        Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name the files instead of the record, because {} answered with a record this build cannot read",
                SafeUrl::new(location)
            ),
        )
        .with_source(location)
    };
    let document: serde_json::Value = serde_json::from_str(body).map_err(|_| malformed())?;
    let files = document
        .get("files")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(malformed)?;
    let mut entries = Vec::new();
    for file in files {
        let Some(key) = file
            .get("key")
            .or_else(|| file.get("filename"))
            .and_then(serde_json::Value::as_str)
        else {
            return Err(malformed());
        };
        let held = file
            .get("links")
            .and_then(|links| links.get("self").or_else(|| links.get("download")))
            .and_then(serde_json::Value::as_str);
        let Some(held) = held else {
            return Err(malformed());
        };
        entries.push(ListingEntry {
            location: SafeUrl::new(held),
            path: key.to_owned(),
            size: file.get("size").and_then(serde_json::Value::as_u64),
        });
    }
    entries.sort_by(|one, other| one.path.as_bytes().cmp(other.path.as_bytes()));
    Ok(entries)
}

macro_rules! delegating_source {
    ($provider:ty, $scheme:literal, $serves:expr) => {
        impl Source for $provider {
            type Body = HttpBody;

            fn serves(&self, reference: &str) -> Option<Serves> {
                reference
                    .starts_with(concat!($scheme, ":"))
                    .then_some($serves)
            }

            fn take_degradations(&self) -> Vec<Degradation> {
                self.http.take_degradations()
            }

            fn probe(
                &self,
                location: &str,
                credential: Option<&Credential>,
            ) -> Result<SourceMetadata, Error> {
                let resolved = self.located(location)?;
                let started = Instant::now();
                let (answer, served) = self.http.send(Method::Head, &resolved, None, credential)?;
                let elapsed = started.elapsed();
                let status = answer.status().as_u16();
                if !(200..300).contains(&status) {
                    return Err(crate::http::status_failure(
                        &resolved,
                        status,
                        header(&answer, "retry-after").as_deref(),
                        credential,
                    ));
                }
                Ok(self.metadata(&served, &answer, elapsed, location))
            }

            fn fetch(
                &self,
                location: &str,
                range: Option<ByteRange>,
                credential: Option<&Credential>,
            ) -> Result<Served<Self::Body>, Error> {
                let resolved = self.located(location)?;
                let started = Instant::now();
                let (answer, served) = self.http.send(Method::Get, &resolved, range, credential)?;
                let elapsed = started.elapsed();
                check_fetch_status(&resolved, range, &answer, credential)?;
                let metadata = self.metadata(&served, &answer, elapsed, location);
                Ok(Served {
                    metadata,
                    body: HttpBody::new(answer.into_body().into_reader()),
                })
            }

            fn revalidate(
                &self,
                location: &str,
                validator: &Validator,
                credential: Option<&Credential>,
            ) -> Result<Revalidated<Self::Body>, Error> {
                let resolved = self.located(location)?;
                let started = Instant::now();
                let (answer, served) = self.http.send_conditional(
                    Method::Get,
                    &resolved,
                    None,
                    credential,
                    validator,
                )?;
                let elapsed = started.elapsed();
                let status = answer.status().as_u16();
                if status == 304 {
                    return Ok(Revalidated::Unchanged);
                }
                if !(200..300).contains(&status) {
                    return Err(crate::http::status_failure(
                        &resolved,
                        status,
                        header(&answer, "retry-after").as_deref(),
                        credential,
                    ));
                }
                let metadata = self.metadata(&served, &answer, elapsed, location);
                Ok(Revalidated::Changed(Box::new(Served {
                    metadata,
                    body: HttpBody::new(answer.into_body().into_reader()),
                })))
            }

            fn list(
                &self,
                location: &str,
                credential: Option<&Credential>,
            ) -> Result<Listing, Error> {
                self.listed(location, credential)
            }
        }
    };
}

impl HuggingFaceSource {
    /// Returns the location one reference is fetched from.
    fn located(&self, reference: &str) -> Result<String, Error> {
        self.resolve(reference)
    }

    /// Returns what the response said about the object, under the reference the
    /// user wrote rather than the location it resolved to.
    fn metadata(
        &self,
        served: &str,
        answer: &ureq::http::Response<ureq::Body>,
        time_to_first_byte: std::time::Duration,
        reference: &str,
    ) -> SourceMetadata {
        let fields = crate::http::common_fields(answer);
        let revision = identifier_of(reference.strip_prefix("hf:").unwrap_or(reference)).revision;
        let identity = match revision {
            Some(revision) if revision != "main" => SourceIdentity::ImmutableVersion(revision),
            _ => crate::http::identity_of(header(answer, "etag").as_deref()),
        };
        SourceMetadata {
            location: SafeUrl::new(served),
            host: Host::of_location(&self.origin),
            size: fields.size,
            content: None,
            interop: None,
            identity,
            last_modified: fields.last_modified,
            supports_ranges: fields.supports_ranges,
            time_to_first_byte,
            retry_after: fields.retry_after,
            cost: fetchloom_engine::seam::source::Cost::default(),
        }
    }

    /// Lists the entries a repository holds at the revision the reference
    /// pinned.
    fn listed(&self, reference: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        let (held, _) = Self::parts(reference)?;
        let revision = held.revision.as_deref().unwrap_or("main");
        let described = if held.path.starts_with("datasets/") {
            held.path.clone()
        } else {
            format!("models/{}", held.path)
        };
        let api = format!("{}/api/{described}/tree/{revision}", self.origin);
        let (answer, _served) = self.http.send(Method::Get, &api, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                &api,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let limits = self.http.limits();
        let body = answer
            .into_body()
            .into_with_config()
            .limit(limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| crate::http::transport_failure(&api, &reason))?;
        let base = match held.revision.as_deref() {
            Some(revision) => format!("hf:{}@{revision}/", held.path),
            None => format!("hf:{}/", held.path),
        };
        let entries = tree_entries(&api, &body, &base)?;
        let limits = self.http.limits();
        bounded(entries, 0, &api, &limits)
    }
}

impl ZenodoSource {
    /// Returns the location one reference is fetched from.
    fn located(&self, reference: &str) -> Result<String, Error> {
        self.resolve(reference)
    }

    /// Returns what the response said about the object, under the reference the
    /// user wrote rather than the location it resolved to.
    fn metadata(
        &self,
        served: &str,
        answer: &ureq::http::Response<ureq::Body>,
        time_to_first_byte: std::time::Duration,
        _reference: &str,
    ) -> SourceMetadata {
        let fields = crate::http::common_fields(answer);
        SourceMetadata {
            location: SafeUrl::new(served),
            host: Host::of_location(&self.origin),
            size: fields.size,
            content: None,
            interop: None,
            identity: crate::http::identity_of(header(answer, "etag").as_deref()),
            last_modified: fields.last_modified,
            supports_ranges: fields.supports_ranges,
            time_to_first_byte,
            retry_after: fields.retry_after,
            cost: fetchloom_engine::seam::source::Cost::default(),
        }
    }

    /// Lists the files one record holds.
    fn listed(&self, reference: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        let api = self.located(reference)?;
        let (answer, _served) = self.http.send(Method::Get, &api, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                &api,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let limits = self.http.limits();
        let body = answer
            .into_body()
            .into_with_config()
            .limit(limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| crate::http::transport_failure(&api, &reason))?;
        bounded(record_entries(&api, &body)?, 0, &api, &limits)
    }
}

/// Reads the entries a repository tree names.
fn tree_entries(location: &str, body: &str, prefix: &str) -> Result<Vec<ListingEntry>, Error> {
    let malformed = || {
        Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name the files instead of the repository, because {} answered with a tree this build cannot read",
                SafeUrl::new(location)
            ),
        )
        .with_source(location)
    };
    let document: serde_json::Value = serde_json::from_str(body).map_err(|_| malformed())?;
    let held = document.as_array().ok_or_else(malformed)?;
    let mut entries = Vec::new();
    for item in held {
        let kind = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("file");
        if kind != "file" {
            continue;
        }
        let Some(path) = item.get("path").and_then(serde_json::Value::as_str) else {
            return Err(malformed());
        };
        entries.push(ListingEntry {
            location: SafeUrl::new(&format!("{prefix}{path}")),
            path: path.to_owned(),
            size: item.get("size").and_then(serde_json::Value::as_u64),
        });
    }
    entries.sort_by(|one, other| one.path.as_bytes().cmp(other.path.as_bytes()));
    Ok(entries)
}

/// Refuses a listing longer than the bound a run holds itself to.
fn bounded(
    entries: Vec<ListingEntry>,
    skipped: u64,
    location: &str,
    limits: &Limits,
) -> Result<Listing, Error> {
    let allowed = usize::try_from(limits.listing_entries).unwrap_or(usize::MAX);
    if entries.len() > allowed {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "ask for a narrower reference, because {} lists more than the {} entries a run reads",
                SafeUrl::new(location),
                limits.listing_entries
            ),
        )
        .with_source(location));
    }
    Ok(Listing { entries, skipped })
}

delegating_source!(HuggingFaceSource, "hf", Serves::Container);
delegating_source!(ZenodoSource, "zenodo", Serves::Container);

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "test assertions, where the value that was absent is the message"
    )]

    use super::{HuggingFaceSource, Identifier, ZenodoSource, identifier_of, record_entries};

    #[test]
    fn a_revision_is_read_from_the_reference_and_defaults_to_none() {
        assert_eq!(
            identifier_of("datasets/org/name@abc123"),
            Identifier {
                path: "datasets/org/name".to_owned(),
                revision: Some("abc123".to_owned()),
            }
        );
        assert_eq!(
            identifier_of("datasets/org/name"),
            Identifier {
                path: "datasets/org/name".to_owned(),
                revision: None,
            }
        );
    }

    #[test]
    fn a_zenodo_record_is_the_trailing_digits_and_never_a_guess() {
        assert_eq!(
            ZenodoSource::record_of("zenodo:10.5281/zenodo.1234567").unwrap(),
            "1234567"
        );
        assert_eq!(
            ZenodoSource::record_of("zenodo:1234567").unwrap(),
            "1234567"
        );
        assert!(ZenodoSource::record_of("zenodo:no-digits-at-all").is_err());
        assert!(ZenodoSource::record_of("hf:datasets/org/name").is_err());
    }

    #[test]
    fn a_repository_with_no_path_is_refused_rather_than_guessed() {
        let source = HuggingFaceSource::new(
            fetchloom_engine::limits::Limits::default(),
            std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        );
        assert!(source.resolve("hf:").is_err());
        assert!(source.resolve("https://host/object").is_err());
    }

    #[test]
    fn a_record_states_its_files_and_a_record_that_does_not_is_refused() {
        let entries = record_entries(
            "https://zenodo.org/api/records/1",
            r#"{"files":[{"key":"b.csv","size":20,"links":{"self":"https://zenodo.org/api/files/x/b.csv"}},{"key":"a.csv","size":10,"links":{"self":"https://zenodo.org/api/files/x/a.csv"}}]}"#,
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].path, "a.csv",
            "entries are ordered by path bytes"
        );
        assert_eq!(entries[0].size, Some(10));

        assert!(
            record_entries("https://zenodo.org/api/records/1", r#"{"no_files":[]}"#).is_err(),
            "a record naming no files was read as an empty one"
        );
        assert!(record_entries("https://zenodo.org/api/records/1", "not json at all").is_err());
    }

    #[test]
    fn a_file_a_record_states_no_location_for_is_refused_rather_than_skipped() {
        assert!(
            record_entries(
                "https://zenodo.org/api/records/1",
                r#"{"files":[{"key":"a.csv","size":10}]}"#,
            )
            .is_err(),
            "a file with no location was skipped rather than refused"
        );
    }

    #[test]
    fn a_size_a_record_does_not_state_is_absent_rather_than_zero() {
        let entries = record_entries(
            "https://zenodo.org/api/records/1",
            r#"{"files":[{"key":"a.csv","links":{"self":"https://zenodo.org/x"}}]}"#,
        )
        .unwrap();
        assert_eq!(
            entries[0].size, None,
            "an unstated size was written as a number"
        );
    }
}
