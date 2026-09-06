//! The router that turns a DOI into the reference of the provider serving it.
//!
//! A DOI resolves to a landing page rather than to files, so nothing here
//! fetches. It reads the registration and answers with the reference an adapter
//! of this build already serves.

use std::sync::Arc;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::work::WorkCounter;

use crate::http::{HttpSource, Method, header};

const DATACITE: &str = "https://api.datacite.org";

#[must_use]
pub fn is_doi(reference: &str) -> bool {
    reference
        .strip_prefix("doi:")
        .is_some_and(|rest| rest.starts_with("10.") && rest.contains('/'))
}

pub struct DoiRouter {
    http: HttpSource,
    origin: String,
}

impl DoiRouter {
    #[must_use]
    pub fn new(limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: DATACITE.to_owned(),
        }
    }

    #[must_use]
    pub fn reaching(origin: impl Into<String>, limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            http: HttpSource::new(limits, work),
            origin: origin.into(),
        }
    }

    /// # Errors
    /// `reference.unresolved` when the reference is not a DOI, when the
    /// registration cannot be read, or when it names a provider no adapter of
    /// this build serves. The `network.*` kinds as the registry answers.
    pub fn route(&self, reference: &str, credential: Option<&Credential>) -> Result<String, Error> {
        let name = reference
            .strip_prefix("doi:")
            .filter(|rest| rest.starts_with("10.") && rest.contains('/') && !rest.ends_with('/'));
        let Some(name) = name else {
            return Err(Error::new(
                ErrorKind::ReferenceUnresolved,
                format!(
                    "write it as doi:10.prefix/suffix, because {reference} is not a name the registry can be asked about"
                ),
            )
            .with_source(reference));
        };
        let endpoint = format!("{}/dois/{name}", self.origin);
        let body = self.registration(&endpoint, credential)?;
        let document: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| unreadable(&endpoint))?;
        let attributes = document
            .get("data")
            .and_then(|data| data.get("attributes"))
            .ok_or_else(|| unreadable(&endpoint))?;
        let landing = attributes
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| unreadable(&endpoint))?;
        let registrant = document
            .get("data")
            .and_then(|data| data.get("relationships"))
            .and_then(|held| held.get("client"))
            .and_then(|held| held.get("data"))
            .and_then(|held| held.get("id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no registrant the registry named");
        routed(name, landing).ok_or_else(|| unserved(reference, landing, registrant))
    }

    fn registration(
        &self,
        endpoint: &str,
        credential: Option<&Credential>,
    ) -> Result<String, Error> {
        let (answer, _served) = self.http.send(Method::Get, endpoint, None, credential)?;
        let status = answer.status().as_u16();
        if status == 404 {
            return Err(Error::new(
                ErrorKind::ReferenceUnresolved,
                format!(
                    "check the name, because {} is registered with no DOI registry this build asks",
                    SafeUrl::new(endpoint)
                ),
            )
            .with_source(endpoint));
        }
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                endpoint,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        answer
            .into_body()
            .into_with_config()
            .limit(self.http.limits().listing_bytes)
            .read_to_string()
            .map_err(|reason| crate::http::transport_failure(endpoint, &reason))
    }
}

fn unreadable(endpoint: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name the provider's own reference instead of the DOI, because {} answered with a registration this build cannot read",
            SafeUrl::new(endpoint)
        ),
    )
    .with_source(endpoint)
}

fn unserved(reference: &str, landing: &str, registrant: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name a location or a provider reference instead, because {reference} is registered by {registrant} and resolves to {}, which no adapter of this build serves. Serving it needs an adapter for that provider's own file listing API, described the way the six in crates/sources/src/described.rs are",
            SafeUrl::new(landing)
        ),
    )
    .with_source(reference)
}

fn host_of(landing: &str) -> Option<&str> {
    let after_scheme = landing.split_once("://")?.1;
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, at)| at);
    Some(host.split(':').next().unwrap_or(host))
}

fn under(host: &str, domain: &str) -> bool {
    host.eq_ignore_ascii_case(domain)
        || host
            .to_ascii_lowercase()
            .ends_with(&format!(".{}", domain.to_ascii_lowercase()))
}

fn persistent_id(landing: &str) -> Option<&str> {
    let query = landing.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let value = pair.strip_prefix("persistentId=")?;
        value.starts_with("doi:").then_some(value)
    })
}

fn figshare_article(name: &str) -> Option<&str> {
    let after = name.rsplit_once("figshare.")?.1;
    let digits = after.split_once(".v").map_or(after, |(before, _)| before);
    digits
        .chars()
        .all(|letter| letter.is_ascii_digit())
        .then_some(digits)
        .filter(|held| !held.is_empty())
}

fn routed(name: &str, landing: &str) -> Option<String> {
    let host = host_of(landing)?;
    if let Some(persistent) = persistent_id(landing) {
        return Some(format!("dataverse:{host}/{persistent}"));
    }
    if under(host, "zenodo.org") {
        return Some(format!("zenodo:{name}"));
    }
    if under(host, "figshare.com") {
        return Some(format!("figshare:{}", figshare_article(name)?));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{figshare_article, is_doi, routed};

    #[test]
    fn a_doi_is_a_prefix_and_a_suffix_and_nothing_else_is_one() {
        assert!(is_doi("doi:10.5281/zenodo.20546670"));
        assert!(is_doi("doi:10.7910/DVN/OMV93V"));
        assert!(!is_doi("doi:10.5281"), "a prefix alone is not a DOI");
        assert!(!is_doi("doi:notadoi/x"), "a suffix with no 10. prefix");
        assert!(!is_doi("zenodo:10.5281/zenodo.1"));
        assert!(!is_doi("https://doi.org/10.5281/zenodo.1"));
    }

    #[test]
    fn a_landing_page_decides_the_provider_and_a_multiplier_keeps_its_host() {
        assert_eq!(
            routed(
                "10.5281/zenodo.20546670",
                "https://zenodo.org/doi/10.5281/zenodo.20546670"
            )
            .as_deref(),
            Some("zenodo:10.5281/zenodo.20546670")
        );
        assert_eq!(
            routed(
                "10.7910/DVN/OMV93V",
                "https://dataverse.harvard.edu/citation?persistentId=doi:10.7910/DVN/OMV93V"
            )
            .as_deref(),
            Some("dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V"),
            "a Dataverse DOI lost the installation that holds it"
        );
        assert_eq!(
            routed(
                "10.5072/FK2/ABCDEF",
                "https://data.some.edu/dataset.xhtml?persistentId=doi:10.5072/FK2/ABCDEF"
            )
            .as_deref(),
            Some("dataverse:data.some.edu/doi:10.5072/FK2/ABCDEF"),
            "one adapter has to reach every Dataverse installation"
        );
        assert_eq!(
            routed(
                "10.6084/m9.figshare.29575358.v1",
                "https://figshare.com/articles/online_resource/A_Title/29575358/1"
            )
            .as_deref(),
            Some("figshare:29575358")
        );
    }

    #[test]
    fn a_landing_page_no_adapter_serves_is_not_guessed_at() {
        assert_eq!(
            routed("10.1000/xyz", "https://example.org/some/landing/page"),
            None
        );
        assert_eq!(routed("10.1000/xyz", "not a location at all"), None);
    }

    #[test]
    fn a_figshare_article_is_the_digits_the_doi_states_without_its_version() {
        assert_eq!(
            figshare_article("10.6084/m9.figshare.29575358.v1"),
            Some("29575358")
        );
        assert_eq!(
            figshare_article("10.6084/m9.figshare.29575358"),
            Some("29575358")
        );
        assert_eq!(figshare_article("10.5281/zenodo.1"), None);
        assert_eq!(figshare_article("10.6084/m9.figshare.notdigits"), None);
    }
}
