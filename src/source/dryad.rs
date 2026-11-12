//! dryad: datasets by DOI or datadryad.org link, listed through dryad's API at one published
//! version, with the MD5 or SHA-256 dryad states for each file. anyone may list a dataset, but
//! dryad downloads files only for an API account: the key goes with requests to datadryad.org,
//! and never to the storage a download is sent on to.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::doi::Doi;
use super::{Access, Listing, Reference, Remote, malformed};
use crate::auth::Account;
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::net::Api;

const API: &str = "https://datadryad.org/api/v2";

/// the files one page of a version's listing asks for.
const PAGE: usize = 100;

/// a dryad dataset, named by its DOI, at one published version or the newest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    /// the DOI, lowercased, without `doi:`.
    doi: String,
    /// the number dryad gives the version, counted from 1.
    version: Option<u64>,
}

impl Dataset {
    /// reads the part after `dryad:`: a DOI, with `@` and a version number to pin one.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
        let invalid = || malformed(&format!("dryad:{text}"), "dryad:10.5061/dryad.j1fd7@1");
        let text = text.trim();
        let (doi, version) = match text.rsplit_once('@') {
            Some((doi, number)) => {
                let number = number
                    .parse::<u64>()
                    .ok()
                    .filter(|number| *number > 0)
                    .ok_or_else(invalid)?;
                (doi, Some(number))
            }
            None => (text, None),
        };
        let doi = Doi::bare(doi).ok_or_else(invalid)?;
        Ok(Dataset {
            doi: doi.name().to_owned(),
            version,
        })
    }

    /// a pasted datadryad.org link to a dataset's page, as the DOI registry points to it, or to
    /// the dataset in dryad's API.
    pub(super) fn from_link(url: &Url) -> Option<Dataset> {
        if !matches!(url.host_str()?, "datadryad.org" | "www.datadryad.org") {
            return None;
        }
        let parts: Vec<&str> = url.path_segments()?.collect();
        let named = match parts.as_slice() {
            ["dataset", rest @ ..] | ["stash", "dataset", rest @ ..] => rest.join("/"),
            ["api", "v2", "datasets", doi] => (*doi).to_owned(),
            _ => return None,
        };
        let named = percent_decode_str(&named).decode_utf8().ok()?;
        let doi = Doi::bare(named.strip_prefix("doi:")?)?;
        Some(Dataset {
            doi: doi.name().to_owned(),
            version: None,
        })
    }

    /// lists the version's files: the newest published version, or the one pinned. the license
    /// is the dataset's, since dryad states it for the dataset and not for each version.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let (newest, license) = self.described(api).await?;
        let version = match self.version {
            Some(number) if number != newest.number => self.numbered(api, number).await?,
            _ => newest,
        };
        let files = self.files(api, version.id).await?;
        Ok(Listing {
            within: Vec::new(),
            access: version.access,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Dryad(Dataset {
                doi: self.doi.clone(),
                version: Some(version.number),
            }),
            title: version.title,
            license: license.map(|license| licence_name(&license)),
            doi: Some(self.doi.clone()),
            files,
        })
    }

    /// the dataset's newest published version and its license, as the dataset's own answer
    /// names them.
    async fn described(&self, api: &impl Api) -> Result<(Version, Option<String>), Error> {
        let response = api.get(&self.url(&[])?).await?;
        let answer: Described = super::json(self, &response)?;
        if !answer
            .identifier
            .strip_prefix("doi:")
            .is_some_and(|doi| doi.eq_ignore_ascii_case(&self.doi))
        {
            return Err(self.invalid("the answer names another dataset"));
        }
        let id = answer
            .links
            .version
            .as_ref()
            .and_then(|link| version_id(&link.href))
            .ok_or_else(|| self.invalid("the answer links no version"))?;
        let newest = Version {
            id,
            number: answer.number,
            title: answer.title,
            access: access(answer.visibility.as_deref()),
        };
        Ok((newest, answer.license))
    }

    /// the published version numbered `number`, from the dataset's list of versions.
    async fn numbered(&self, api: &impl Api, number: u64) -> Result<Version, Error> {
        let mut url = self.url(&["versions"])?;
        let mut seen = 0;
        for page in 1..=super::most_files().div_ceil(PAGE) {
            url.set_query(Some(&format!("page={page}&per_page={PAGE}")));
            let response = api.get(&url).await?;
            let answer: Page<Versions> = super::json(self, &response)?;
            let listed = answer
                .embedded
                .map(|embedded| embedded.versions)
                .unwrap_or_default();
            if let Some(found) = listed.iter().find(|version| version.number == number) {
                let id = found
                    .links
                    .version
                    .as_ref()
                    .and_then(|link| version_id(&link.href))
                    .ok_or_else(|| self.invalid("a version links nowhere"))?;
                return Ok(Version {
                    id,
                    number,
                    title: found.title.clone(),
                    access: access(found.visibility.as_deref()),
                });
            }
            seen += listed.len();
            if listed.is_empty() || seen >= answer.total {
                break;
            }
        }
        Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("dryad lists no published version {number} of doi:{}", self.doi),
        )
        .help(format!(
            "check the versions at https://datadryad.org/dataset/doi:{}, then name one of them, or leave the version out for the newest",
            self.doi
        )))
    }

    /// every file of version `id`, a page at a time, until the listing holds as many entries as
    /// dryad says the version has. a listing that ends early is refused, not shortened.
    async fn files(&self, api: &impl Api, id: u64) -> Result<Vec<Remote>, Error> {
        let mut url = Url::parse(&format!("{API}/versions/{id}/files"))
            .map_err(|_| self.invalid("the files URL is invalid"))?;
        let mut files = Vec::new();
        let (mut seen, mut total) = (0, 0);
        let mut pages = super::Pages::new();
        for page in 1.. {
            pages
                .next(page, seen)
                .map_err(|reason| self.invalid(&reason))?;
            url.set_query(Some(&format!("page={page}&per_page={PAGE}")));
            let response = api.get(&url).await?;
            let answer: Page<Files> = super::json(self, &response)?;
            if answer.total > super::most_files() {
                return Err(super::too_many(
                    self,
                    "download the files you need from https://datadryad.org one at a time",
                ));
            }
            total = answer.total;
            let listed = answer
                .embedded
                .map(|embedded| embedded.files)
                .unwrap_or_default();
            if listed.is_empty() {
                break;
            }
            seen += listed.len();
            for file in listed {
                if file.status.as_deref() != Some("deleted") {
                    files.push(file.remote(self)?);
                }
            }
            if seen >= total {
                break;
            }
        }
        if seen < total {
            return Err(self.invalid(&format!(
                "the listing ends after {seen} of the {total} files it counts"
            )));
        }
        Ok(files)
    }

    /// the dataset's URL in dryad's API, with `more` path segments after it.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("{API}/datasets"))
            .map_err(|_| self.invalid("the API URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the API URL has no path"))?
            .push(&format!("doi:{}", self.doi))
            .extend(more);
        Ok(url)
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help(crate::error::report_answer())
    }
}

/// the error for a download dryad refused: it downloads files only for an API account, and its
/// keys last 10 hours.
pub(super) fn refused_download(url: &Url) -> Option<Error> {
    let is_file = url.host_str() == Some("datadryad.org")
        && url
            .path()
            .strip_prefix("/api/v2/files/")
            .and_then(|rest| rest.strip_suffix("/download"))
            .is_some_and(|id| id.parse::<u64>().is_ok());
    is_file.then(|| {
        Error::new(
            Kind::AuthRequired,
            format!(
                "dryad refused {}: dryad lets only a signed-in API account download files",
                crate::net::Shown(url)
            ),
        )
        .help("get a key on your datadryad.org account page and sign in with `fetchloom auth login dryad`, or set DRYAD_TOKEN; a key dryad gives lasts 10 hours")
        .needs(Account::Dryad, None)
    })
}

/// a license as a lock records it: the SPDX id when dryad links one, as it does for CC0.
fn licence_name(license: &str) -> String {
    license
        .strip_prefix("https://spdx.org/licenses/")
        .and_then(|rest| rest.strip_suffix(".html"))
        .unwrap_or(license)
        .to_owned()
}

/// the id in a link to one version of dryad's API, `/api/v2/versions/26724`.
fn version_id(href: &str) -> Option<u64> {
    href.strip_prefix("/api/v2/versions/")?.parse().ok()
}

/// who may read a version, as dryad says: anyone when it is public, an account it lets see the
/// version when it is restricted.
fn access(visibility: Option<&str>) -> Access {
    match visibility {
        Some("public") => Access::Public,
        Some("restricted") => Access::Login,
        _ => Access::Unknown,
    }
}

/// the version a listing reads: its id in dryad's API, its number, its title and who may read
/// it.
struct Version {
    id: u64,
    number: u64,
    title: Option<String>,
    access: Access,
}

#[derive(Deserialize)]
struct Described {
    identifier: String,
    #[serde(rename = "versionNumber")]
    number: u64,
    title: Option<String>,
    license: Option<String>,
    visibility: Option<String>,
    #[serde(rename = "_links")]
    links: Links,
}

#[derive(Deserialize)]
struct Links {
    #[serde(rename = "stash:version")]
    version: Option<Link>,
    #[serde(rename = "stash:download")]
    download: Option<Link>,
    #[serde(rename = "self")]
    own: Option<Link>,
}

#[derive(Deserialize)]
struct Link {
    href: String,
}

/// one page of a listing: what it embeds, and how many entries all pages hold.
#[derive(Deserialize)]
struct Page<T> {
    total: usize,
    #[serde(rename = "_embedded")]
    embedded: Option<T>,
}

#[derive(Deserialize)]
struct Versions {
    #[serde(rename = "stash:versions")]
    versions: Vec<Listed>,
}

#[derive(Deserialize)]
struct Listed {
    #[serde(rename = "versionNumber")]
    number: u64,
    title: Option<String>,
    visibility: Option<String>,
    #[serde(rename = "_links")]
    links: VersionLinks,
}

#[derive(Deserialize)]
struct VersionLinks {
    #[serde(rename = "self")]
    version: Option<Link>,
}

#[derive(Deserialize)]
struct Files {
    #[serde(rename = "stash:files")]
    files: Vec<File>,
}

#[derive(Deserialize)]
struct File {
    path: String,
    size: Option<u64>,
    status: Option<String>,
    digest: Option<String>,
    #[serde(rename = "digestType")]
    digest_type: Option<String>,
    #[serde(rename = "_links")]
    links: Links,
}

impl File {
    fn remote(self, dataset: &Dataset) -> Result<Remote, Error> {
        let id = self
            .links
            .download
            .as_ref()
            .and_then(|link| {
                link.href
                    .strip_prefix("/api/v2/files/")?
                    .strip_suffix("/download")?
                    .parse::<u64>()
                    .ok()
            })
            .or_else(|| {
                self.links
                    .own
                    .as_ref()?
                    .href
                    .strip_prefix("/api/v2/files/")?
                    .parse::<u64>()
                    .ok()
            })
            .ok_or_else(|| {
                dataset.invalid(&format!(
                    "\"{}\" has no download link",
                    self.path.escape_debug()
                ))
            })?;
        let url = Url::parse(&format!("{API}/files/{id}/download"))
            .map_err(|_| dataset.invalid("the download URL is invalid"))?;
        let (mut md5, mut sha256) = (None, None);
        if let Some(digest) = self.digest.as_deref() {
            let read = match self.digest_type.as_deref() {
                Some("md5") => Md5::from_hex(digest).map(|digest| md5 = Some(digest)),
                Some("sha-256") => Sha256::from_hex(digest).map(|digest| sha256 = Some(digest)),
                // another algorithm checks nothing fetchloom keeps, and the bytes pin the file
                _ => Some(()),
            };
            if read.is_none() {
                return Err(dataset.invalid(&format!(
                    "the digest of \"{}\" is not hex of its type",
                    self.path.escape_debug()
                )));
            }
        }
        Ok(Remote {
            path: self.path,
            size: self.size,
            sha256,
            md5,
            url,
            named_by_server: false,
            etag: None,
        })
    }
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dryad:{}", self.doi)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
