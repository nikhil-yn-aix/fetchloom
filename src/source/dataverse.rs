//! dataverse: datasets on any dataverse install, named by host and persistent id, listed through
//! the native API.

mod version;

use std::fmt;

use url::Url;

use super::{Listing, malformed};
use crate::error::{Error, Kind};
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
        if host.contains('@') {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                "a dataverse host cannot contain login details",
            )
            .help("remove the user name and password from the host"));
        }
        let origin = Url::parse(&format!("https://{host}/")).map_err(|_| bad())?;
        if origin.host_str().is_none()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(bad());
        }
        let (pid, version) = match rest.rsplit_once('@') {
            Some((pid, version)) if is_version(version) => (pid, Some(version.to_owned())),
            _ => (rest, None),
        };
        let pid_ok = ["doi:", "hdl:"].iter().any(|scheme| {
            pid.len() > scheme.len()
                && pid
                    .get(..scheme.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
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
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let response = api.get(&self.url()?).await?;
        let answer: version::Answer = super::json(self, &response)?;
        answer.listing(self)
    }

    fn url(&self) -> Result<Url, Error> {
        let mut url = Url::parse(&format!(
            "https://{}/api/datasets/:persistentId/",
            self.host
        ))
        .map_err(|_| self.invalid("the dataset URL is invalid"))?;
        if let Some(version) = &self.version {
            url.path_segments_mut()
                .map_err(|()| self.invalid("the dataset URL has no path"))?
                .pop_if_empty()
                .extend(["versions", version]);
        }
        url.query_pairs_mut().append_pair("persistentId", &self.pid);
        Ok(url)
    }

    fn file_url(&self, id: u64) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("https://{}/api/access/datafile/{id}", self.host))
            .map_err(|_| self.invalid("the file URL is invalid"))?;
        // dataverse checksums describe the upload, not the ingested tabular conversion.
        url.query_pairs_mut().append_pair("format", "original");
        Ok(url)
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("open the dataset's page and choose a published version with complete file metadata")
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

#[cfg(test)]
mod tests;
