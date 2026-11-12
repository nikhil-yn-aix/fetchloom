//! figshare: articles by id, optionally pinned to a version, listed through the articles API.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, Unavailable, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::Api;

/// a figshare article, at a version or at its newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Article {
    id: u64,
    version: Option<u32>,
}

impl Article {
    /// reads the part after `figshare:`, such as `34744689` or `34744689@2`.
    pub(super) fn parse(text: &str) -> Result<Article, Error> {
        let bad = || {
            malformed(
                &format!("figshare:{text}"),
                "figshare:34744689 or figshare:34744689@2",
            )
        };
        let (id, version) = match text.trim().split_once('@') {
            Some((id, version)) => (id, Some(version.parse().map_err(|_| bad())?)),
            None => (text.trim(), None),
        };
        Ok(Article {
            id: id.parse().map_err(|_| bad())?,
            version,
        })
    }

    /// a pasted article link, from figshare.com or an institution's figshare portal.
    pub(super) fn from_link(url: &Url) -> Option<Article> {
        if !is_portal(url.host_str()?) {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let after = parts.iter().position(|part| *part == "articles")? + 1;
        let article = match &parts[after..] {
            [kind, rest @ ..]
                if rest.len() >= 2
                    && matches!(
                        *kind,
                        "figure"
                            | "online_resource"
                            | "preprint"
                            | "book"
                            | "conference_contribution"
                            | "media"
                            | "dataset"
                            | "poster"
                            | "journal_contribution"
                            | "presentation"
                            | "thesis"
                            | "software"
                    ) =>
            {
                rest
            }
            other => other,
        };
        // modern links add a type before the title; a numeric title is still a title.
        let (id, version) = match article {
            [_, id, version] => (*id, Some(version.parse().ok()?)),
            [_, id] | [id] => (*id, None),
            _ => return None,
        };
        Some(Article {
            id: id.parse().ok()?,
            version,
        })
    }

    /// asks the articles API for the article's files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let response = api.get(&self.url()?).await?;
        let answer: Answer = super::json(self, &response)?;
        if answer.id != self.id
            || answer.version == 0
            || self
                .version
                .is_some_and(|version| version != answer.version)
        {
            return Err(self.invalid("the article id or version does not match"));
        }
        if answer.download_disabled || answer.is_embargoed {
            return Err(Error::new(
                Kind::AuthRequired,
                format!("{self} does not allow downloads"),
            )
            .help("open the article on figshare.com to check its download conditions"));
        }
        let resolved = Article {
            id: answer.id,
            version: Some(answer.version),
        };
        // the schema promises only ten embedded files, though some articles return more.
        // a shorter list is complete; longer lists need the version's files endpoint.
        let entries = if answer.files.len() < 10 {
            answer.files
        } else {
            resolved.files(api, 1000).await?
        };
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        for file in entries {
            let folder = answer
                .folder_structure
                .get(&file.id.to_string())
                .map(String::as_str);
            if file.is_link_only {
                unavailable.push(Unavailable {
                    path: file.path(folder),
                    reason: "figshare lists it as a link to another site, not a stored file"
                        .to_owned(),
                });
            } else {
                files.push(file.remote(&resolved, folder)?);
            }
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable,
            warnings: Vec::new(),
            resolved: Reference::Figshare(resolved),
            title: Some(answer.title),
            license: answer.license.map(|license| license.name),
            doi: answer.doi,
            files,
        })
    }

    async fn files(&self, api: &impl Api, page_size: usize) -> Result<Vec<File>, Error> {
        let mut url = self.url()?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the files URL has no path"))?
            .push("files");
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        let mut page = 1_u32;
        loop {
            url.query_pairs_mut()
                .clear()
                .append_pair("page", &page.to_string())
                .append_pair("page_size", &page_size.to_string());
            let response = api.get(&url).await?;
            let entries: Vec<File> = super::json(self, &response)?;
            let is_last = entries.is_empty() || entries.len() < page_size;
            if entries.iter().any(|file| !seen.insert(file.id)) {
                return Err(self.invalid("the files endpoint repeated a file"));
            }
            files.extend(entries);
            if files.len() > super::most_files() {
                return Err(super::too_many(
                    self,
                    "select the files you need with --select",
                ));
            }
            if is_last {
                return Ok(files);
            }
            page += 1;
        }
    }

    fn url(&self) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("https://api.figshare.com/v2/articles/{}", self.id))
            .map_err(|_| self.invalid("the article URL is invalid"))?;
        if let Some(version) = self.version {
            url.path_segments_mut()
                .map_err(|()| self.invalid("the article URL has no path"))?
                .extend(["versions", &version.to_string()]);
        }
        Ok(url)
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("check the article on figshare.com and report the invalid listing to its owner")
    }
}

/// whether a host serves figshare portal pages rather than the API or file downloads.
pub(super) fn is_portal(host: &str) -> bool {
    host == "figshare.com"
        || (host.ends_with(".figshare.com")
            && !matches!(host, "ndownloader.figshare.com" | "api.figshare.com"))
}

#[derive(Deserialize)]
struct Answer {
    id: u64,
    version: u32,
    title: String,
    license: Option<License>,
    doi: Option<String>,
    files: Vec<File>,
    #[serde(default)]
    folder_structure: HashMap<String, String>,
    #[serde(default)]
    download_disabled: bool,
    #[serde(default)]
    is_embargoed: bool,
}

#[derive(Deserialize)]
struct License {
    name: String,
}

#[derive(Deserialize)]
struct File {
    id: u64,
    name: String,
    size: u64,
    is_link_only: bool,
    download_url: String,
    computed_md5: Option<String>,
    supplied_md5: Option<String>,
}

impl File {
    /// where the file goes inside the dataset, below the folder figshare files it in.
    fn path(&self, folder: Option<&str>) -> String {
        match folder.filter(|folder| !folder.is_empty()) {
            Some(folder) => format!("{folder}/{}", self.name),
            None => self.name.clone(),
        }
    }

    fn remote(self, article: &Article, folder: Option<&str>) -> Result<Remote, Error> {
        let url = self.download(article)?;
        let md5 = self
            .computed_md5
            .as_deref()
            .filter(|hash| !hash.is_empty())
            .or_else(|| self.supplied_md5.as_deref().filter(|hash| !hash.is_empty()))
            .map(|hash| {
                Md5::from_hex(hash).ok_or_else(|| {
                    article.invalid(&format!("the checksum for \"{}\" is not an MD5", self.name))
                })
            })
            .transpose()?;
        Ok(Remote {
            path: self.path(folder),
            size: Some(self.size),
            sha256: None,
            md5,
            url,
            named_by_server: false,
            etag: None,
        })
    }

    fn download(&self, article: &Article) -> Result<Url, Error> {
        let bad = || {
            article.invalid(&format!(
                "the download URL for \"{}\" is not a stable public link",
                self.name
            ))
        };
        let url = Url::parse(&self.download_url).map_err(|_| bad())?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(bad());
        }
        Ok(url)
    }
}

impl fmt::Display for Article {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "figshare:{}", self.id)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
