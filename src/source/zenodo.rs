//! zenodo: records by id or link, listed through the records API, with an MD5 for every file.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Listing, Var, malformed};
use crate::error::Error;
use crate::net::{Api, Credential};

/// a zenodo record. a record id names exactly one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    id: u64,
}

impl Record {
    /// reads the part after `zenodo:`.
    pub(super) fn parse(text: &str) -> Result<Record, Error> {
        let id = text
            .trim()
            .parse()
            .map_err(|_| malformed(&format!("zenodo:{text}"), "zenodo:3608990"))?;
        Ok(Record { id })
    }

    /// a pasted zenodo link, and the file in it when the link names one.
    pub(super) fn from_link(url: &Url) -> Option<(Record, Option<String>)> {
        if !matches!(url.host_str()?, "zenodo.org" | "www.zenodo.org") {
            return None;
        }
        let mut parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        if parts.first() == Some(&"api") {
            parts.remove(0);
        }
        let (kind, id) = (parts.first()?, parts.get(1)?);
        if !matches!(*kind, "records" | "record") {
            return None;
        }
        let record = Record {
            id: id.parse().ok()?,
        };
        let file = match parts.get(2) {
            Some(&"files") if parts.len() > 3 => {
                let mut segments = parts[3..].to_vec();
                // the API serves a file's bytes at .../files/<name>/content
                if segments.len() > 1 && segments.last() == Some(&"content") {
                    segments.pop();
                }
                Some(
                    percent_decode_str(&segments.join("/"))
                        .decode_utf8_lossy()
                        .into_owned(),
                )
            }
            _ => None,
        };
        Some((record, file))
    }

    /// asks the records API for the record's files.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the zenodo milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("zenodo"))
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "zenodo:{}", self.id)
    }
}

/// `ZENODO_TOKEN`, for restricted records.
pub(super) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let token = var("ZENODO_TOKEN")
        .and_then(|token| token.into_string().ok())
        .filter(|token| !token.trim().is_empty());
    token
        .map(|token| Credential {
            host: "zenodo.org",
            header: "authorization",
            value: format!("Bearer {}", token.trim()),
        })
        .into_iter()
        .collect()
}
