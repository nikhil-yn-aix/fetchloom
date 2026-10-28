//! DOIs: doi.org is asked where one points, without following the link, and the host it lands
//! on picks the source. a DOI's prefix is never trusted, since every dataverse install and
//! repository registers its own.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::{Reference, malformed};
use crate::error::{Error, Kind};
use crate::net::Api;

/// a DOI such as `10.5281/zenodo.3608990`, lowercased, since DOIs ignore case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Doi(String);

impl Doi {
    /// a DOI written without a prefix: `10.`, a registrant code, a slash and a suffix.
    pub(super) fn bare(text: &str) -> Option<Doi> {
        let (prefix, suffix) = text.split_once('/')?;
        let registrant = prefix.strip_prefix("10.")?;
        let valid = !registrant.is_empty()
            && registrant.chars().all(|c| c.is_ascii_digit() || c == '.')
            && !suffix.is_empty()
            && !suffix.chars().any(|c| c.is_whitespace() || c.is_control());
        valid.then(|| Doi(text.to_ascii_lowercase()))
    }

    /// reads the part after `doi:`.
    pub(super) fn parse(text: &str) -> Result<Doi, Error> {
        Doi::bare(text.trim())
            .ok_or_else(|| malformed(&format!("doi:{text}"), "doi:10.5281/zenodo.3608990"))
    }

    /// a pasted doi.org link.
    pub(super) fn from_link(url: &Url) -> Option<Doi> {
        if !matches!(url.host_str()?, "doi.org" | "www.doi.org" | "dx.doi.org") {
            return None;
        }
        let path = percent_decode_str(url.path().trim_start_matches('/'))
            .decode_utf8()
            .ok()?;
        Doi::bare(&path)
    }

    /// the DOI without its reference prefix.
    pub(super) fn name(&self) -> &str {
        &self.0
    }

    /// asks doi.org where the DOI points and returns the source there.
    pub(super) async fn locate(&self, api: &impl Api) -> Result<Reference, Error> {
        let mut url = Url::parse("https://doi.org/api/handles/")
            .map_err(|_| self.invalid("the handles API URL is invalid"))?;
        // a DOI is opaque, so its slashes, query marks and fragments belong to one path segment.
        url.path_segments_mut()
            .map_err(|()| self.invalid("the handles API URL has no path"))?
            .pop_if_empty()
            .push(&self.0);
        let response = api.get(&url).await?;
        self.target(super::json(self, &response)?)
    }

    fn target(&self, answer: Handle) -> Result<Reference, Error> {
        match answer.code {
            1 => (),
            100 => return Err(Self::unresolved(&format!("{self} is not registered"))),
            _ => return Err(self.invalid("the handles API did not report success")),
        }
        if !answer.name.eq_ignore_ascii_case(&self.0) {
            return Err(self.invalid("the handle names another DOI"));
        }
        let location = answer
            .values
            .into_iter()
            .find_map(|value| match value {
                HandleValue::Url { data } => Some(data),
                HandleValue::Other => None,
            })
            .ok_or_else(|| self.invalid("the handle has no URL value"))?;
        if location.format != "string" {
            return Err(self.invalid("the URL value is not in string format"));
        }
        let parsed = Reference::parse(&location.value)
            .map_err(|_| self.invalid("the URL value is not a dataset link"))?;
        match parsed.reference {
            Reference::Url(url) => Err(Self::unresolved(&format!(
                "{self} points to {}, which fetchloom cannot list",
                url.host_str().unwrap_or_default()
            ))),
            Reference::Doi(_) => Err(Self::unresolved(&format!(
                "{self} points to another DOI rather than a repository"
            ))),
            reference => Ok(reference),
        }
    }

    fn unresolved(message: &str) -> Error {
        Error::new(Kind::ReferenceUnresolved, message).help(
            "check the DOI in a browser and use a supported dataset link or a direct file URL",
        )
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("doi.org sent an invalid handle for {self}: {reason}"),
        )
        .help("check the DOI in a browser and report the invalid handle to its publisher")
    }
}

#[derive(Deserialize)]
struct Handle {
    #[serde(rename = "responseCode")]
    code: u16,
    #[serde(rename = "handle")]
    name: String,
    #[serde(default)]
    values: Vec<HandleValue>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum HandleValue {
    #[serde(rename = "URL")]
    Url { data: Location },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Location {
    format: String,
    value: String,
}

impl fmt::Display for Doi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "doi:{}", self.0)
    }
}

#[cfg(test)]
mod tests;
