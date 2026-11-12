//! dandi: dandisets by number or dandiarchive.org link, listed through dandi's versioned assets
//! API at one published version, or the draft, with the SHA-256 dandi states for each asset. an
//! embargoed dandiset, which dandi shows to no one without an account, is refused; no key goes to
//! dandi.

use std::collections::HashMap;
use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, Unavailable, malformed};
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

const API: &str = "https://api.dandiarchive.org/api";
const SITE: &str = "https://dandiarchive.org";
/// where an asset's bytes are, as dandi links them, by the blob's id.
const BLOBS: &str = "https://dandiarchive.s3.amazonaws.com/blobs/";
/// the assets one page of the listing asks for.
const PAGE: usize = 1000;
/// the digest dandi states as a SHA-256 of an asset's bytes. its `dandi:dandi-etag` is S3's
/// multipart tag, no MD5, so it is never read as one.
const SHA256: &str = "dandi:sha2-256";

/// a dandiset, at one version, the draft or the newest dandi published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dandiset {
    /// the six digits dandi numbers it with, as `000027`.
    id: String,
    /// a published version, as `0.210831.2033`, or `draft`.
    version: Option<String>,
}

impl Dandiset {
    /// reads the part after `dandi:`: a dandiset's number, with `@` and a version to pin one.
    pub(super) fn parse(text: &str) -> Result<Dandiset, Error> {
        let invalid = || malformed(&format!("dandi:{text}"), "dandi:000027@0.210831.2033");
        let text = text.trim();
        let (id, version) = match text.split_once('@') {
            Some((id, version)) => (id, Some(version_of(version).ok_or_else(invalid)?)),
            None => (text, None),
        };
        Ok(Dandiset {
            id: number(id).ok_or_else(invalid)?,
            version,
        })
    }

    /// a pasted dandiarchive.org page of a dandiset, at the version it names, as the DOI
    /// registry points to it, and the folder its file browser's `location` names.
    pub(super) fn from_link(url: &Url) -> Option<(Dandiset, Option<String>)> {
        if !matches!(url.host_str()?, "dandiarchive.org" | "www.dandiarchive.org") {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let (id, version, files) = match parts.as_slice() {
            ["dandiset", id] => (*id, None, false),
            ["dandiset", id, version] => (*id, Some(*version), false),
            ["dandiset", id, version, "files"] => (*id, Some(*version), true),
            _ => return None,
        };
        let version = match version {
            Some(version) => Some(version_of(version)?),
            None => None,
        };
        let folder = files.then(|| location(url)).flatten();
        Some((
            Dandiset {
                id: number(id)?,
                version,
            },
            folder,
        ))
    }

    /// lists the version's assets: the one pinned, else the newest published, else the draft.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let mut warnings = Vec::new();
        let version = match &self.version {
            Some(version) => version.clone(),
            None => self.newest(api, &mut warnings).await?,
        };
        let pinned = Dandiset {
            id: self.id.clone(),
            version: Some(version.clone()),
        };
        let info: Info = pinned.answer(&api.get(&pinned.url("info/")?).await?)?;
        if let Some(status) = info.dandiset.and_then(|dandiset| dandiset.embargo_status) {
            pinned.open(&status)?;
        }
        if info.version != version {
            return Err(pinned.invalid("the answer names another version"));
        }
        let (files, unavailable) = pinned.assets(api).await?;
        let metadata = info.metadata.unwrap_or_default();
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            resolved: Reference::Dandi(pinned),
            title: info.name,
            license: metadata.license.into_iter().next().map(|license| {
                license
                    .strip_prefix("spdx:")
                    .map_or_else(|| license.clone(), str::to_owned)
            }),
            doi: metadata.doi,
            files,
            unavailable,
            warnings,
        })
    }

    /// the newest version dandi published, or the draft of a dandiset it never published, with
    /// a warning, since a draft can change.
    async fn newest(&self, api: &impl Api, warnings: &mut Vec<String>) -> Result<String, Error> {
        let described: Described = self.answer(&api.get(&self.url("")?).await?)?;
        self.open(described.embargo_status.as_deref().unwrap_or("OPEN"))?;
        if let Some(published) = described.most_recent_published_version {
            return Ok(published.version);
        }
        let Some(draft) = described.draft_version else {
            return Err(self.invalid("the answer names no version"));
        };
        warnings.push(format!(
            "{self} has no published version, so its draft is read. a draft can change: data.lock's hashes name the bytes"
        ));
        Ok(draft.version)
    }

    /// every asset of the version, a page at a time: the blobs, and the zarr folders, which have
    /// no bytes of their own to download.
    async fn assets(&self, api: &impl Api) -> Result<(Vec<Remote>, Vec<Unavailable>), Error> {
        let first = self.url(&format!(
            "assets/?metadata=true&order=path&page_size={PAGE}"
        ))?;
        let pages_at = self.url("assets/")?;
        let mut next = Some(first);
        let mut pages = super::Pages::new();
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        while let Some(url) = next.take() {
            pages
                .next(url.to_string())
                .map_err(|reason| self.invalid(&reason))?;
            let page: Page = self.answer(&api.get(&url).await?)?;
            if page
                .count
                .is_some_and(|count| count > super::most_files() as u64)
            {
                return Err(self.too_many());
            }
            if page.results.is_empty() && page.next.is_some() {
                return Err(self.invalid("an empty page names a next one"));
            }
            for asset in page.results {
                match self.asset(asset)? {
                    Ok(file) => files.push(file),
                    Err(entry) => unavailable.push(entry),
                }
                if files.len() + unavailable.len() > super::most_files() {
                    return Err(self.too_many());
                }
            }
            next = match page.next {
                None => None,
                Some(link) => Some(
                    Url::parse(&link)
                        .ok()
                        .filter(|url| {
                            url.scheme() == "https"
                                && url.host_str() == pages_at.host_str()
                                && url.path() == pages_at.path()
                        })
                        .ok_or_else(|| {
                            self.invalid("a page's next page is not one of this version's")
                        })?,
                ),
            };
        }
        Ok((files, unavailable))
    }

    /// one asset as a file to download, or, for a zarr folder, as an entry without bytes.
    fn asset(&self, asset: Asset) -> Result<Result<Remote, Unavailable>, Error> {
        let path = asset.path;
        let named = || path.escape_debug().to_string();
        if asset.zarr.is_some() {
            return Ok(Err(Unavailable {
                path,
                reason: "it is a zarr folder of chunks, which fetchloom does not download"
                    .to_owned(),
            }));
        }
        let Some(blob) = asset.blob.filter(|blob| is_uuid(blob)) else {
            return Err(self.invalid(&format!("\"{}\" is neither a blob nor a zarr", named())));
        };
        let metadata = asset.metadata.unwrap_or_default();
        let sha256 = match metadata.digest.get(SHA256) {
            None => None,
            Some(hex) => Some(Sha256::from_hex(hex).ok_or_else(|| {
                self.invalid(&format!("the SHA-256 of \"{}\" is not hex", named()))
            })?),
        };
        // the blob's own link on S3, which never changes; dandi's download link redirects there
        let stored = format!("{BLOBS}{}/{}/{blob}", &blob[..3], &blob[3..6]);
        let url = metadata
            .content_url
            .iter()
            .find(|link| **link == stored)
            .map_or_else(
                || format!("{API}/assets/{}/download/", asset.id),
                Clone::clone,
            );
        let url = Url::parse(&url)
            .ok()
            .filter(|_| is_uuid(&asset.id))
            .ok_or_else(|| self.invalid(&format!("\"{}\" has no usable link", named())))?;
        Ok(Ok(Remote {
            path,
            size: Some(asset.size),
            sha256,
            md5: None,
            url,
            named_by_server: false,
            etag: None,
        }))
    }

    /// refuses a dandiset that is not open to anyone without an account.
    fn open(&self, status: &str) -> Result<(), Error> {
        match status {
            "OPEN" => Ok(()),
            "UNEMBARGOING" => Err(Error::new(
                Kind::AuthRequired,
                format!("{self} is leaving its embargo, and dandi opens its data once that ends"),
            )
            .help(format!(
                "run the same command again later; {} says when it is open",
                self.page()
            ))),
            _ => Err(self.embargoed()),
        }
    }

    /// reads an answer of dandi's API: a missing dandiset or version as unresolved, and a
    /// refusal as the embargo dandi refuses a dandiset under when it shows it to nobody.
    fn answer<T: serde::de::DeserializeOwned>(&self, response: &Response) -> Result<T, Error> {
        match response.status {
            401 | 403 => Err(self.embargoed()),
            404 if self.version.is_some() => Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} does not exist"),
            )
            .help(format!(
                "check the versions at {SITE}/dandiset/{}, or leave the version out for the newest",
                self.id
            ))),
            _ => super::json(self, response),
        }
    }

    /// the version's URL in dandi's API, with `more` after it, or the dandiset's for none.
    fn url(&self, more: &str) -> Result<Url, Error> {
        let path = match &self.version {
            Some(version) if !more.is_empty() => {
                format!("{API}/dandisets/{}/versions/{version}/{more}", self.id)
            }
            _ => format!("{API}/dandisets/{}/", self.id),
        };
        Url::parse(&path).map_err(|_| self.invalid("the API URL is invalid"))
    }

    /// the page that shows the dandiset, at its version when it names one.
    fn page(&self) -> String {
        match &self.version {
            Some(version) => format!("{SITE}/dandiset/{}/{version}", self.id),
            None => format!("{SITE}/dandiset/{}", self.id),
        }
    }

    fn embargoed(&self) -> Error {
        Error::new(
            Kind::AuthRequired,
            format!(
                "{self} is embargoed: dandi shows it only to the people its owners let in, and fetchloom does not sign in to dandi"
            ),
        )
        .help(format!(
            "ask its owners for the data, or wait until the embargo ends; {SITE}/dandiset/{} shows it once it is open",
            self.id
        ))
    }

    fn too_many(&self) -> Error {
        super::too_many(
            self,
            &format!(
                "select the folders you need with --select, as {} shows them",
                self.page()
            ),
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

impl fmt::Display for Dandiset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(version) => write!(f, "dandi:{}@{version}", self.id),
            None => write!(f, "dandi:{}", self.id),
        }
    }
}

/// a dandiset's number: six digits.
fn number(text: &str) -> Option<String> {
    (text.len() == 6 && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| text.to_owned())
}

/// a version as dandi names one: `draft`, or a published version, `0.` then a date and a time,
/// as `0.210831.2033`.
fn version_of(text: &str) -> Option<String> {
    let published = text
        .strip_prefix("0.")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(date, time)| {
            date.len() == 6
                && time.len() == 4
                && date
                    .bytes()
                    .chain(time.bytes())
                    .all(|byte| byte.is_ascii_digit())
        });
    (published || text == "draft").then(|| text.to_owned())
}

/// the folder a file browser's link opens, from its `location`, without a final `/`.
fn location(url: &Url) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == "location")
        .map(|(_, value)| value.trim_matches('/').to_owned())
        .filter(|folder| !folder.is_empty())
}

/// a UUID as dandi gives blobs and assets, which a URL path carries as it is.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

#[derive(Deserialize)]
struct Described {
    embargo_status: Option<String>,
    most_recent_published_version: Option<Named>,
    draft_version: Option<Named>,
}

#[derive(Deserialize)]
struct Named {
    version: String,
}

#[derive(Deserialize)]
struct Info {
    version: String,
    name: Option<String>,
    dandiset: Option<Status>,
    metadata: Option<VersionMetadata>,
}

#[derive(Deserialize)]
struct Status {
    embargo_status: Option<String>,
}

#[derive(Default, Deserialize)]
struct VersionMetadata {
    #[serde(default)]
    license: Vec<String>,
    doi: Option<String>,
}

#[derive(Deserialize)]
struct Page {
    count: Option<u64>,
    next: Option<String>,
    #[serde(default)]
    results: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    #[serde(rename = "asset_id")]
    id: String,
    blob: Option<String>,
    zarr: Option<String>,
    path: String,
    size: u64,
    metadata: Option<AssetMetadata>,
}

#[derive(Default, Deserialize)]
struct AssetMetadata {
    #[serde(default)]
    digest: HashMap<String, String>,
    #[serde(default, rename = "contentUrl")]
    content_url: Vec<String>,
}

#[cfg(test)]
mod tests;
