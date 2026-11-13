//! dataverse: datasets on any dataverse install, named by host and persistent id, listed through
//! the native API.

mod file;
mod version;

use std::fmt;

use url::Url;

use super::{Listing, malformed};
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

/// a dataset or one of its files, at a version or at its newest published one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    /// the install's host, with its port when it is not the usual one.
    host: String,
    item: Item,
    version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Dataset(String),
    File(String),
}

impl Dataset {
    /// reads the part after `dataverse:`, such as
    /// `dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@2.0`.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
        let bad = || {
            malformed(
                &format!("dataverse:{text}"),
                "dataverse:host/doi:10.7910/DVN/TJCLKP or dataverse:host/file:6867336, with @version to pin one",
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
            Some((_, version)) if version.eq_ignore_ascii_case(":draft") => {
                return Err(Error::new(
                    Kind::ReferenceInvalid,
                    "a dataverse draft changes until it is published, so it cannot be pinned",
                )
                .help("name a published version, such as @1.0, or none for the newest"));
            }
            Some((pid, version)) if is_version(version) => (pid, Some(version.to_owned())),
            _ => (rest, None),
        };
        let item = match pid.strip_prefix("file:") {
            Some(id) if is_number(id) || is_pid(id) => Item::File(id.to_owned()),
            None if is_pid(pid) => Item::Dataset(pid.to_owned()),
            _ => return Err(bad()),
        };
        Ok(Dataset {
            host: host.to_ascii_lowercase(),
            item,
            version,
        })
    }

    /// a dataset or file page on any install, without fetching its HTML.
    pub(super) fn from_link(url: &Url) -> Option<Result<Dataset, Error>> {
        let page = url.path_segments()?.next_back()?;
        if !matches!(page, "dataset.xhtml" | "citation" | "file.xhtml") {
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
        let id = if page == "file.xhtml" {
            format!(
                "file:{}",
                query("fileId").or_else(|| query("persistentId"))?
            )
        } else {
            query("persistentId")?
        };
        let mut reference = format!("{host}/{id}");
        if let Some(version) = query("version") {
            if !is_version(&version) {
                return Some(Err(malformed(
                    url.as_str(),
                    "a dataverse link with version=1.0 or version=:latest-published",
                )));
            }
            reference.push('@');
            reference.push_str(&version);
        }
        Some(Dataset::parse(&reference))
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

    /// the install's origin: https, at its host and port, where every request for it goes.
    pub(super) fn origin(&self) -> Option<url::Origin> {
        Url::parse(&format!("https://{}/", self.host))
            .ok()
            .map(|url| url.origin())
    }

    /// the account a key for this install is saved under: its host, with its port when that is
    /// not 443, as the key only goes there.
    pub(super) fn account(&self) -> crate::auth::Account {
        let host = crate::auth::dataverse_host(&self.host)
            .unwrap_or_else(|| self.host_name().to_ascii_lowercase());
        crate::auth::Account::Dataverse(host)
    }

    /// asks the install's native API for the dataset's files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        if let Item::File(id) = &self.item {
            return file::resolve(self, id, api).await;
        }
        let response = api.get(&self.url()?).await?;
        let answer: version::Answer = self.answer(&response)?;
        answer.listing(self, &super::env)
    }

    fn answer<T: serde::de::DeserializeOwned>(&self, response: &Response) -> Result<T, Error> {
        super::json(self, response).map_err(|error| {
            if error.kind == Kind::AuthRequired {
                let account = self.account();
                let command = account.command();
                error.help(format!(
                    "if you have access, sign in with `{command}` and run the same command again; otherwise ask the dataset's owner for access"
                )).needs(account, None)
            } else {
                error
            }
        })
    }

    /// `/api/datasets/:persistentId/versions/<version>`, else the newest published version, since
    /// the dataset alone answers with the newest one the key may see, a draft for its curators.
    fn url(&self) -> Result<Url, Error> {
        let Item::Dataset(pid) = &self.item else {
            return Err(self.invalid("a file has no dataset listing URL"));
        };
        let mut url = Url::parse(&format!(
            "https://{}/api/datasets/:persistentId/",
            self.host
        ))
        .map_err(|_| self.invalid("the dataset URL is invalid"))?;
        let version = self.version.as_deref().unwrap_or(":latest-published");
        url.path_segments_mut()
            .map_err(|()| self.invalid("the dataset URL has no path"))?
            .pop_if_empty()
            .extend(["versions", version]);
        url.query_pairs_mut().append_pair("persistentId", pid);
        Ok(url)
    }

    fn file_url(&self, id: u64) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("https://{}/api/access/datafile/{id}", self.host))
            .map_err(|_| self.invalid("the file URL is invalid"))?;
        // dataverse checksums describe the upload, not the ingested tabular conversion
        url.query_pairs_mut().append_pair("format", "original");
        Ok(url)
    }

    /// the error for an answer with a version not yet published, which the person avoids by
    /// choosing a published one.
    fn unpublished(&self) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: the version is not published"),
        )
        .help("open the dataset's page and choose a published version")
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help(crate::error::report_answer())
    }
}

fn is_number(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && text.parse::<u64>().is_ok_and(|id| id > 0)
}

fn is_pid(text: &str) -> bool {
    ["doi:", "hdl:"].iter().any(|scheme| {
        text.len() > scheme.len()
            && text
                .get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

/// a dataverse version: `2.0`, `3`, or a name such as `:latest-published`.
fn is_version(text: &str) -> bool {
    !text.is_empty()
        && (text.chars().all(|c| c.is_ascii_digit() || c == '.') || text.starts_with(':'))
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dataverse:{}/", self.host)?;
        match &self.item {
            Item::Dataset(pid) => f.write_str(pid)?,
            Item::File(id) => write!(f, "file:{id}")?,
        }
        if let Some(version) = &self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod file_tests;
