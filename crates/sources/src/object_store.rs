//! The source that fetches from an object store over HTTP or HTTPS.

use std::sync::Arc;
use std::time::Instant;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::Degradation;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, ListingEntry, Revalidated, Served, Source, SourceIdentity, SourceMetadata,
    Validator,
};

use fetchloom_engine::limits::Limits;
use fetchloom_engine::work::WorkCounter;

use crate::http::{HttpBody, HttpSource, Method, check_fetch_status, common_fields, header};
use crate::index;
use crate::origin::Origin;

/// The header an object store carries its version identifier under.
const VERSION_ID_HEADER: &str = "x-amz-version-id";

/// The header an object store states its billing decision under.
const REQUEST_CHARGED_HEADER: &str = "x-amz-request-charged";

/// The value that header carries when the requester is billed.
const REQUESTER_PAYS: &str = "requester";

/// A source reached over an object store's HTTP or HTTPS listing and object
/// API, holding an `HttpSource` it delegates every request to.
pub struct ObjectStoreSource {
    http: HttpSource,
}

impl ObjectStoreSource {
    /// Builds a source holding no connections yet.
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
        }
    }

    /// Removes and returns every fallback performed since the last call.
    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.http.take_degradations()
    }
}

fn metadata_of(
    served: &str,
    answer: &ureq::http::Response<ureq::Body>,
    time_to_first_byte: std::time::Duration,
) -> Result<SourceMetadata, Error> {
    let fields = common_fields(answer);
    let version = header(answer, VERSION_ID_HEADER).filter(|value| !value.is_empty());
    let identity = match version {
        Some(version) => SourceIdentity::ImmutableVersion(version),
        None => crate::http::identity_of(header(answer, "etag").as_deref()),
    };
    let charged =
        header(answer, REQUEST_CHARGED_HEADER).is_some_and(|value| value == REQUESTER_PAYS);
    let cost = if charged {
        Cost {
            egress_charged: Some(true),
            requester_pays: Some(true),
        }
    } else {
        Cost::default()
    };
    Ok(SourceMetadata {
        location: SafeUrl::new(served),
        host: Host::new(Origin::of(served)?.host()),
        size: fields.size,
        content: None,
        interop: None,
        identity,
        last_modified: fields.last_modified,
        supports_ranges: fields.supports_ranges,
        time_to_first_byte,
        retry_after: fields.retry_after,
        cost,
    })
}

impl Source for ObjectStoreSource {
    type Body = HttpBody;

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        let started = Instant::now();
        let (answer, served) = self.http.send(Method::Head, location, None, credential)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                location,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        metadata_of(&served, &answer, time_to_first_byte)
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        let started = Instant::now();
        let (answer, served) =
            self.http
                .send_conditional(Method::Get, location, None, credential, validator)?;
        let time_to_first_byte = started.elapsed();
        let status = answer.status().as_u16();
        if status == 304 {
            return Ok(Revalidated::Unchanged);
        }
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                location,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let metadata = metadata_of(&served, &answer, time_to_first_byte)?;
        Ok(Revalidated::Changed(Box::new(Served {
            metadata,
            body: HttpBody::new(answer.into_body().into_reader()),
        })))
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        let started = Instant::now();
        let (answer, served) = self.http.send(Method::Get, location, range, credential)?;
        let time_to_first_byte = started.elapsed();
        check_fetch_status(location, range, &answer, credential)?;
        let metadata = metadata_of(&served, &answer, time_to_first_byte)?;
        Ok(Served {
            metadata,
            body: HttpBody::new(answer.into_body().into_reader()),
        })
    }

    fn list(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error> {
        let (answer, _served) = self.http.send(Method::Get, location, None, credential)?;
        let status = answer.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                location,
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
            .map_err(|reason| crate::http::transport_failure(location, &reason))?;
        let listed = index::parse(location, status, &body)?;
        let allowed = usize::try_from(limits.listing_entries).unwrap_or(usize::MAX);
        if listed.len() > allowed {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "ask for a narrower prefix, because the index at {} lists more than the {} entries a run reads",
                    SafeUrl::new(location),
                    limits.listing_entries
                ),
            )
            .with_source(location));
        }
        Ok(listed)
    }
}
