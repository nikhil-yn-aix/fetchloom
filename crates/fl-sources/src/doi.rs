//! The DOI router: a DOI is looked up at its resolver, and the host of the page it lands on
//! picks the source. Registrant prefixes are never used.

use std::sync::Arc;

use fl_model::reference::Reference;
use fl_transfer::client::Client;
use fl_transfer::error::TransferError;
use fl_transfer::fetch::Request;
use fl_transfer::url::SafeUrl;
use url::Url;

use crate::source::{Source, SourceError};
use crate::template::escape;

/// Redirects followed from the resolver to a landing page at most.
const HOPS: usize = 10;

/// The reference of the source whose landing page `doi` resolves to, through the resolver at
/// `base`. Each redirect is asked for once, and a landing page is never fetched once a source
/// knows it.
pub(crate) async fn route(
    client: &Client,
    base: &Url,
    doi: &str,
    sources: &[Arc<dyn Source>],
) -> Result<Reference, SourceError> {
    let reference = format!("doi:{doi}");
    let mut path = String::new();
    escape(doi, &mut path);
    let mut at = base.join(&path).map_err(|error| SourceError::Invalid {
        reference: reference.clone(),
        reason: error.to_string(),
    })?;
    for _ in 0..HOPS {
        let url = SafeUrl::parse(at.as_str()).map_err(|error| SourceError::Invalid {
            reference: reference.clone(),
            reason: error.to_string(),
        })?;
        let page = client
            .get(&Request {
                url: &url,
                follow: false,
                typed: false,
            })
            .await
            .map_err(|error| match error {
                TransferError::Status {
                    status: 404 | 410, ..
                } => SourceError::Unresolved {
                    reference: reference.clone(),
                },
                other => SourceError::Transfer(other),
            })?;
        let Some(location) = page.header("location").filter(|_| page.status / 100 == 3) else {
            return Err(SourceError::NoLanding {
                reference,
                host: at.host_str().unwrap_or_default().to_owned(),
                landing: at.to_string(),
            });
        };
        at = at.join(location).map_err(|_| SourceError::Body {
            url: page.url.clone(),
            reason: "redirected to a location that is not a URL".to_owned(),
        })?;
        if let Some(found) = sources
            .iter()
            .rev()
            .find_map(|source| source.landing(at.as_str()))
        {
            return Ok(found);
        }
    }
    Err(SourceError::NoLanding {
        reference,
        host: at.host_str().unwrap_or_default().to_owned(),
        landing: at.to_string(),
    })
}
