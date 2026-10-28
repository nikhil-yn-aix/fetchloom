//! dataverse: datasets on any dataverse install, named by host and persistent id, listed through
//! the native API.

use std::fmt;

use url::Url;

use super::{Listing, malformed};
use crate::error::Error;
use crate::net::Api;

/// a dataset on one dataverse install, at a version or at its newest published one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    /// the install's host, with its port when it is not the usual one.
    host: String,
    /// the persistent id, such as `doi:10.7910/DVN/TJCLKP`.
    pid: String,
    version: Option<String>,
}

impl Dataset {
    /// reads the part after `dataverse:`, such as
    /// `dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@2.0`.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
        let bad = || {
            malformed(
                &format!("dataverse:{text}"),
                "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
            )
        };
        let (host, rest) = text.trim().split_once('/').ok_or_else(bad)?;
        let (pid, version) = match rest.rsplit_once('@') {
            Some((pid, version)) if is_version(version) => (pid, Some(version.to_owned())),
            _ => (rest, None),
        };
        let pid_ok = ["doi:", "hdl:"].iter().any(|scheme| {
            pid.len() > scheme.len() && pid[..scheme.len()].eq_ignore_ascii_case(scheme)
        });
        if host.is_empty() || host.contains(['/', '?', '#']) || !pid_ok {
            return Err(bad());
        }
        Ok(Dataset {
            host: host.to_ascii_lowercase(),
            pid: pid.to_owned(),
            version,
        })
    }

    /// a pasted dataset page on any install: `.../dataset.xhtml?persistentId=doi:...`.
    pub(super) fn from_link(url: &Url) -> Option<Dataset> {
        let page = url.path_segments()?.next_back()?;
        if !matches!(page, "dataset.xhtml" | "citation") {
            return None;
        }
        let query = |key: &str| {
            url.query_pairs()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.into_owned())
        };
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str()?),
            None => url.host_str()?.to_owned(),
        };
        Dataset::parse(&format!("{host}/{}", query("persistentId")?))
            .ok()
            .map(|dataset| Dataset {
                version: query("version").filter(|version| is_version(version)),
                ..dataset
            })
    }

    /// the install's host name, without its port.
    pub(super) fn host_name(&self) -> &str {
        match self.host.strip_prefix('[') {
            Some(bracketed) => bracketed
                .split_once(']')
                .map_or(bracketed, |(host, _)| host),
            None => self
                .host
                .split_once(':')
                .map_or(self.host.as_str(), |(host, _)| host),
        }
    }

    /// asks the install's native API for the dataset's files.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the dataverse milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("dataverse"))
    }
}

/// a dataverse version: `2.0`, `3`, or a name such as `:draft`.
fn is_version(text: &str) -> bool {
    !text.is_empty()
        && (text.chars().all(|c| c.is_ascii_digit() || c == '.') || text.starts_with(':'))
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dataverse:{}/{}", self.host, self.pid)?;
        if let Some(version) = &self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}
