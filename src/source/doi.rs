//! DOIs: doi.org is asked where one points, without following the link, and the host it lands
//! on picks the source. a DOI's prefix is never trusted, since every dataverse install and
//! repository registers its own.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Reference, malformed};
use crate::error::Error;
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

    pub(super) fn name(&self) -> &str {
        &self.0
    }

    /// asks doi.org where the DOI points and returns the source there.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "routing arrives with the DOI milestone"
    )]
    pub(super) async fn locate(&self, _api: &impl Api) -> Result<Reference, Error> {
        Err(super::not_yet("DOI"))
    }
}

impl fmt::Display for Doi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "doi:{}", self.0)
    }
}
