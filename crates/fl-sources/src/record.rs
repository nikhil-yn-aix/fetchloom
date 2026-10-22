//! Fetching a JSON document: a record, a page of a listing, a search answer.

use fl_transfer::client::Client;
use fl_transfer::error::TransferError;
use fl_transfer::fetch::{Page, Request};
use serde_json::Value;

use crate::source::SourceError;

/// The page `request` names and its body read as JSON. A location that is not found makes
/// `reference` unresolved.
pub(crate) async fn json(
    client: &Client,
    request: &Request<'_>,
    reference: &str,
) -> Result<(Page, Value), SourceError> {
    let page = client.get(request).await.map_err(|error| match error {
        TransferError::Status {
            status: 404 | 410, ..
        } => SourceError::Unresolved {
            reference: reference.to_owned(),
        },
        other => SourceError::Transfer(other),
    })?;
    let value = serde_json::from_slice(&page.body).map_err(|error| SourceError::Body {
        url: page.url.clone(),
        reason: error.to_string(),
    })?;
    Ok((page, value))
}
