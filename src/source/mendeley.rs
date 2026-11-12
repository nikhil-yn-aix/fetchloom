//! mendeley data: datasets by id, DOI or data.mendeley.com link, listed at one published version
//! with the SHA-256 mendeley states for each file. the API mendeley documents wants an OAuth
//! client for every caller, so fetchloom reads the public one mendeley's own site reads, which
//! lists published versions to anyone. no key goes to mendeley.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::{Access, Listing, Reference, Remote, Unavailable, malformed};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

const SITE: &str = "https://data.mendeley.com";

/// a mendeley data dataset, at one published version or the newest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    /// the id in the dataset's link, lowercase, as `rscbjbr9sj`.
    id: String,
    /// the version mendeley numbers from 1.
    version: Option<u64>,
}

impl Dataset {
    /// reads the part after `mendeley:`: a dataset id, with `@` and a version to pin one.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
        let invalid = || malformed(&format!("mendeley:{text}"), "mendeley:rscbjbr9sj@2");
        let text = text.trim();
        let (id, version) = match text.split_once('@') {
            Some((id, version)) => (id, Some(version_number(version).ok_or_else(invalid)?)),
            None => (text, None),
        };
        Ok(Dataset {
            id: dataset_id(id).ok_or_else(invalid)?,
            version,
        })
    }

    /// a pasted data.mendeley.com link to a dataset, with the version the link names, as the DOI
    /// registry points to it.
    pub(super) fn from_link(url: &Url) -> Option<Dataset> {
        if url.host_str()? != "data.mendeley.com" {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let (id, version) = match parts.as_slice() {
            ["datasets", id] => (*id, None),
            ["datasets", id, version] => (*id, Some(version_number(version)?)),
            _ => return None,
        };
        Some(Dataset {
            id: dataset_id(id)?,
            version,
        })
    }

    /// lists the version's files, in the dataset's top folder and every folder below it.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let described = self.described(api).await?;
        let version = described.version;
        let folders = self.folders(api, version).await?;
        let mut found = Found::default();
        self.files_in(api, version, "root", "", &mut found).await?;
        for (id, path) in folders {
            self.files_in(api, version, &id, &path, &mut found).await?;
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            unavailable: found.unavailable,
            warnings: Vec::new(),
            resolved: Reference::Mendeley(Dataset {
                id: self.id.clone(),
                version: Some(version),
            }),
            title: described.name,
            license: described
                .data_licence
                .and_then(|licence| licence.short_name.or(licence.full_name)),
            doi: described.doi.and_then(|doi| doi.id),
            files: found.files,
        })
    }

    /// what mendeley says about the version: the one pinned, or the newest it published.
    async fn described(&self, api: &impl Api) -> Result<Described, Error> {
        let mut url = self.url(&[])?;
        if let Some(version) = self.version {
            url.set_query(Some(&format!("version={version}")));
        }
        let described: Described = self.answer(&api.get(&url).await?)?;
        if !described.id.eq_ignore_ascii_case(&self.id) {
            return Err(self.invalid("the answer names another dataset"));
        }
        if self
            .version
            .is_some_and(|version| version != described.version)
        {
            return Err(self.invalid("the answer names another version"));
        }
        if described.version == 0 {
            return Err(self.invalid("the answer names no version"));
        }
        Ok(described)
    }

    /// every folder of the version, with its path from the dataset's top folder.
    async fn folders(&self, api: &impl Api, version: u64) -> Result<Vec<(String, String)>, Error> {
        let url = self.url(&["folders", &version.to_string()])?;
        let folders: Vec<Folder> = self.answer(&api.get(&url).await?)?;
        if folders.len() > super::most_files() {
            return Err(self.too_many());
        }
        let by_id: HashMap<&str, &Folder> = folders
            .iter()
            .map(|folder| (folder.id.as_str(), folder))
            .collect();
        let mut paths = Vec::with_capacity(folders.len());
        for folder in &folders {
            if !is_uuid(&folder.id) {
                return Err(self.invalid("a folder has an id that is no UUID"));
            }
            let mut names = vec![folder.name.as_str()];
            let mut at = folder;
            while let Some(parent) = at.parent_id.as_deref() {
                at = by_id
                    .get(parent)
                    .ok_or_else(|| self.invalid("a folder lies in a folder it does not list"))?;
                names.push(at.name.as_str());
                if names.len() > folders.len() {
                    return Err(self.invalid("its folders hold each other"));
                }
            }
            names.reverse();
            paths.push((folder.id.clone(), names.join("/")));
        }
        Ok(paths)
    }

    /// the files of folder `folder` at `path`, a part at a time, until the listing holds as many
    /// as mendeley counts for the folder.
    async fn files_in(
        &self,
        api: &impl Api,
        version: u64,
        folder: &str,
        path: &str,
        found: &mut Found,
    ) -> Result<(), Error> {
        let mut url = self.url(&["files"])?;
        let mut seen: u64 = 0;
        let mut ids = HashSet::new();
        loop {
            url.set_query(Some(&format!(
                "folder_id={folder}&version={version}&$start={seen}"
            )));
            let response = api.get(&url).await?;
            let listed: Vec<File> = self.answer(&response)?;
            let total = response.total;
            if listed.is_empty() {
                if total.is_some_and(|total| seen < total) {
                    return Err(self.invalid(&format!(
                        "the listing of a folder ends after {seen} of the {} files it counts",
                        total.unwrap_or_default()
                    )));
                }
                return Ok(());
            }
            seen += listed.len() as u64;
            for file in listed {
                if found.files.len() + found.unavailable.len() >= super::most_files() {
                    return Err(self.too_many());
                }
                if !ids.insert(file.id.clone()) {
                    return Err(self.invalid("the listing of a folder repeats a file"));
                }
                found.add(self, path, file)?;
            }
            if total.is_some_and(|total| seen >= total) {
                return Ok(());
            }
        }
    }

    /// reads an answer of mendeley's public API, which tells of a missing dataset or version with
    /// a 200 that holds an error rather than with its status.
    fn answer<T: DeserializeOwned>(&self, response: &Response) -> Result<T, Error> {
        if response.status == 451 {
            return Err(self.embargoed());
        }
        let value: serde_json::Value = super::json(self, response)?;
        if let Some(problem) = value.get("error") {
            return Err(
                match problem.get("status").and_then(serde_json::Value::as_u64) {
                    Some(404 | 410) => Error::new(
                        Kind::ReferenceUnresolved,
                        format!("{self} does not exist, or is not public"),
                    )
                    .help(super::UNKNOWN_HELP),
                    Some(401 | 403) => super::refused("data.mendeley.com", &self.to_string()),
                    Some(451) => self.embargoed(),
                    _ => self.invalid("the answer is an error mendeley does not explain"),
                },
            );
        }
        serde_json::from_value(value)
            .map_err(|err| self.invalid(&format!("the answer could not be read: {err}")))
    }

    /// the dataset's URL in mendeley's public API, with `more` path segments after it.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("{SITE}/public-api/datasets"))
            .map_err(|_| self.invalid("the API URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the API URL has no path"))?
            .push(&self.id)
            .extend(more);
        Ok(url)
    }

    /// the page that shows the dataset, at its version when it names one.
    fn page(&self) -> String {
        match self.version {
            Some(version) => format!("{SITE}/datasets/{}/{version}", self.id),
            None => format!("{SITE}/datasets/{}", self.id),
        }
    }

    fn embargoed(&self) -> Error {
        Error::new(
            Kind::AuthRequired,
            format!("mendeley holds back the files of {self} under an embargo"),
        )
        .help(format!(
            "{} says when the embargo ends; no sign-in lifts it",
            self.page()
        ))
    }

    fn too_many(&self) -> Error {
        super::too_many(
            self,
            &format!("download the files you need from {}", self.page()),
        )
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help(crate::error::report_answer())
    }
}

/// a dataset id as mendeley writes it in links: letters and digits, lowercase.
fn dataset_id(text: &str) -> Option<String> {
    let valid = (4..=32).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_alphanumeric());
    valid.then(|| text.to_ascii_lowercase())
}

/// a version number, counted from 1.
fn version_number(text: &str) -> Option<u64> {
    text.parse::<u64>().ok().filter(|version| *version > 0)
}

/// a UUID as mendeley gives files and folders: hex digits and dashes, which a URL query or path
/// carries as they are.
fn is_uuid(text: &str) -> bool {
    text.len() == 36 && text.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// the files listed so far, and the entries without bytes to download.
#[derive(Default)]
struct Found {
    files: Vec<Remote>,
    unavailable: Vec<Unavailable>,
}

impl Found {
    fn add(&mut self, dataset: &Dataset, folder: &str, file: File) -> Result<(), Error> {
        let path = if folder.is_empty() {
            file.filename
        } else {
            format!("{folder}/{}", file.filename)
        };
        if !is_uuid(&file.id) {
            return Err(dataset.invalid(&format!(
                "\"{}\" has an id that is no UUID",
                path.escape_debug()
            )));
        }
        let Some(details) = file
            .content_details
            .filter(|details| details.download_url.is_some())
        else {
            self.unavailable.push(Unavailable {
                path,
                reason: "mendeley gives no download link for it".to_owned(),
            });
            return Ok(());
        };
        let url = details
            .download_url
            .as_deref()
            .and_then(|link| Url::parse(link).ok())
            .filter(|url| {
                url.as_str()
                    == format!(
                        "{SITE}/public-files/datasets/{}/files/{}/file_downloaded",
                        dataset.id, file.id
                    )
            })
            .ok_or_else(|| {
                dataset.invalid(&format!(
                    "the download link of \"{}\" is not this dataset's",
                    path.escape_debug()
                ))
            })?;
        let sha256 = match details.sha256_hash.as_deref() {
            None => None,
            Some(hex) => Some(Sha256::from_hex(hex).ok_or_else(|| {
                dataset.invalid(&format!(
                    "the SHA-256 of \"{}\" is not hex",
                    path.escape_debug()
                ))
            })?),
        };
        self.files.push(Remote {
            path,
            size: details.size.or(file.size),
            sha256,
            md5: None,
            url,
            named_by_server: false,
            etag: None,
        });
        Ok(())
    }
}

#[derive(Deserialize)]
struct Described {
    id: String,
    version: u64,
    name: Option<String>,
    doi: Option<DoiField>,
    data_licence: Option<Licence>,
}

#[derive(Deserialize)]
struct DoiField {
    id: Option<String>,
}

#[derive(Deserialize)]
struct Licence {
    short_name: Option<String>,
    full_name: Option<String>,
}

#[derive(Deserialize)]
struct Folder {
    id: String,
    name: String,
    parent_id: Option<String>,
}

#[derive(Deserialize)]
struct File {
    filename: String,
    id: String,
    size: Option<u64>,
    content_details: Option<Details>,
}

#[derive(Deserialize)]
struct Details {
    sha256_hash: Option<String>,
    size: Option<u64>,
    download_url: Option<String>,
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mendeley:{}", self.id)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
