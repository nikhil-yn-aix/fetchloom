//! openneuro: datasets by accession or openneuro.org link, listed through openneuro's GraphQL
//! API at one snapshot, the one named or the newest, a request for each folder. git-annex keeps a
//! dataset's large files under keys that state their SHA-256, or their MD5 in older datasets,
//! which the downloaded bytes are checked against; they come from openneuro's S3 bucket at the
//! version the snapshot pins. no key goes to openneuro.

use std::collections::{HashSet, VecDeque};
use std::fmt;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::{Access, Listing, Reference, Remote, malformed};
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::net::Api;

const API: &str = "https://openneuro.org/crn/graphql";
const SITE: &str = "https://openneuro.org";
/// the header openneuro's API asks of a query sent by GET, as its guard against requests from
/// other sites.
const PREFLIGHT: &[(&str, &str)] = &[("apollo-require-preflight", "true")];
/// what each query asks of a file or folder.
const FIELDS: &str = "id filename size directory annexed urls";
/// `DataLad`'s own folder at the top of every dataset, which holds none of its data.
const DATALAD: &str = ".datalad";

/// an openneuro dataset, at one snapshot or the newest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    /// the accession, lowercase, as `ds000001`.
    id: String,
    /// the snapshot's tag, as `1.0.0`.
    tag: Option<String>,
}

impl Dataset {
    /// reads the part after `openneuro:`: an accession, with `@` and a snapshot's tag to pin one.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
        let invalid = || malformed(&format!("openneuro:{text}"), "openneuro:ds000001@1.0.0");
        let text = text.trim();
        let (id, tag) = match text.split_once('@') {
            Some((id, tag)) => (id, Some(tag_of(tag).ok_or_else(invalid)?)),
            None => (text, None),
        };
        Ok(Dataset {
            id: accession(id).ok_or_else(invalid)?,
            tag,
        })
    }

    /// a pasted openneuro.org page of a dataset, or of one of its snapshots, where its DOI
    /// leads.
    pub(super) fn from_link(url: &Url) -> Option<Dataset> {
        if !matches!(url.host_str()?, "openneuro.org" | "www.openneuro.org") {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let (id, tag) = match parts.as_slice() {
            ["datasets", id] => (*id, None),
            ["datasets", id, "versions", tag] | ["datasets", id, "versions", tag, "download"] => {
                (*id, Some(tag_of(tag)?))
            }
            _ => return None,
        };
        Some(Dataset {
            id: accession(id)?,
            tag,
        })
    }

    /// lists the snapshot's files: its top folder, then each folder in it, a request each, or
    /// only the folders that may hold a file the `select` patterns match, when each pattern says
    /// where its files lie, as [`Glob::scope`] does, and a file was found for each.
    pub(super) async fn resolve(&self, api: &impl Api, select: &[Glob]) -> Result<Listing, Error> {
        let tag = match &self.tag {
            Some(tag) => tag.clone(),
            None => self.newest(api).await?,
        };
        let pinned = Dataset {
            id: self.id.clone(),
            tag: Some(tag.clone()),
        };
        let top: Top = pinned
            .ask(
                api,
                &format!(
                    r#"query{{snapshot(datasetId:"{}",tag:"{tag}"){{tag deprecated{{reason}} description{{Name License DatasetDOI}} files{{{FIELDS}}}}}}}"#,
                    pinned.id
                ),
            )
            .await?;
        let snapshot = top
            .snapshot
            .ok_or_else(|| pinned.invalid("the answer holds no snapshot"))?;
        if snapshot.tag.as_deref() != Some(tag.as_str()) {
            return Err(pinned.invalid("the answer names another snapshot"));
        }
        let mut warnings = Vec::new();
        if let Some(deprecated) = snapshot.deprecated {
            warnings.push(
                match deprecated.reason.filter(|reason| !reason.is_empty()) {
                    Some(reason) => format!("the owners of {pinned} deprecated it: {reason}"),
                    None => format!("the owners of {pinned} deprecated it"),
                },
            );
        }
        let scopes: Option<Vec<String>> = if select.is_empty() {
            None
        } else {
            select.iter().map(Glob::scope).collect()
        };
        let mut within = Vec::new();
        let mut found = None;
        if let Some(mut scopes) = scopes {
            let walked = pinned
                .walk(api, &tag, snapshot.files.clone(), select)
                .await?;
            // a pattern that matches nothing is told against all the snapshot holds
            if select
                .iter()
                .all(|glob| walked.files.iter().any(|file| glob.matches(&file.path)))
            {
                scopes.sort();
                scopes.dedup();
                within = scopes;
                found = Some(walked);
            }
        }
        let found = match found {
            Some(found) => found,
            None => pinned.walk(api, &tag, snapshot.files, &[]).await?,
        };
        let description = snapshot.description.unwrap_or_default();
        Ok(Listing {
            within,
            access: Access::Public,
            resolved: Reference::OpenNeuro(pinned),
            title: description.name,
            license: description.license.filter(|license| !license.is_empty()),
            doi: description.doi.map(|doi| {
                let doi = doi.trim();
                doi.strip_prefix("doi:")
                    .or_else(|| doi.strip_prefix("https://doi.org/"))
                    .unwrap_or(doi)
                    .to_owned()
            }),
            files: found.files,
            unavailable: Vec::new(),
            warnings,
        })
    }

    /// the snapshot's files, from the top folder's `entries` down, a request for each folder, or
    /// only the folders that may hold a file the `select` patterns match, when there are some.
    async fn walk(
        &self,
        api: &impl Api,
        tag: &str,
        entries: Vec<Entry>,
        select: &[Glob],
    ) -> Result<Found, Error> {
        let mut found = Found {
            select: select.to_vec(),
            ..Found::default()
        };
        found.add(self, "", entries)?;
        let mut asked = HashSet::new();
        while let Some((tree, path)) = found.folders.pop_front() {
            if !asked.insert(tree.clone()) {
                return Err(self.invalid("a folder lies in itself"));
            }
            if asked.len() + found.files.len() > super::most_files() {
                return Err(self.too_many());
            }
            let folder: Folder = self
                .ask(
                    api,
                    &format!(
                        r#"query{{snapshot(datasetId:"{}",tag:"{tag}"){{files(tree:"{tree}"){{{FIELDS}}}}}}}"#,
                        self.id
                    ),
                )
                .await?;
            let files = folder
                .snapshot
                .ok_or_else(|| self.invalid(&format!("the folder {path} holds no listing")))?
                .files;
            found.add(self, &path, files)?;
        }
        Ok(found)
    }

    /// the tag of the dataset's newest snapshot.
    async fn newest(&self, api: &impl Api) -> Result<String, Error> {
        let latest: Latest = self
            .ask(
                api,
                &format!(
                    r#"query{{dataset(id:"{}"){{latestSnapshot{{tag}}}}}}"#,
                    self.id
                ),
            )
            .await?;
        let tag = latest
            .dataset
            .and_then(|dataset| dataset.latest_snapshot)
            .map(|snapshot| snapshot.tag)
            .ok_or_else(|| {
                Error::new(
                    Kind::ReferenceUnresolved,
                    format!("{self} has no snapshot yet, and only a snapshot can be downloaded"),
                )
                .help(format!("{} says when its owners publish one", self.page()))
            })?;
        tag_of(&tag)
            .ok_or_else(|| self.invalid("the newest snapshot's tag is not one fetchloom can name"))
    }

    /// asks openneuro's API `query`, by GET with the header it asks of one, and reads the
    /// answer's data, or what its errors say.
    async fn ask<T: DeserializeOwned>(&self, api: &impl Api, query: &str) -> Result<T, Error> {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("query", query)
            .finish();
        let url = Url::parse(&format!("{API}?{encoded}"))
            .map_err(|_| self.invalid("the query makes no valid link"))?;
        let response = api.get_with(&url, PREFLIGHT).await?;
        if response.status != 200 {
            return Err(Error::new(
                Kind::NetworkStatus,
                format!("openneuro.org answered {} for {self}", response.status),
            ));
        }
        let answer: Answer<T> = response.json()?;
        if let Some(problem) = answer.errors.first() {
            return Err(self.problem(&problem.message));
        }
        answer
            .data
            .ok_or_else(|| self.invalid("the answer holds no data"))
    }

    /// the error for what openneuro's API said went wrong: a dataset or a snapshot it does not
    /// have, a private dataset, or anything else, in its words, without its stack trace.
    fn problem(&self, message: &str) -> Error {
        let said = message.lines().next().unwrap_or_default().trim();
        if said.contains("does not exist") {
            return Error::new(Kind::ReferenceUnresolved, format!("{self} does not exist"))
                .help(format!(
                    "check the accession at {SITE}/search, then fix it on the command line or as ref in data.toml"
                ));
        }
        if said.contains("do not have access") {
            return Error::new(
                Kind::AuthRequired,
                format!(
                    "{self} is private: openneuro shows it only to the people its owners let in, and fetchloom does not sign in to openneuro"
                ),
            )
            .help("ask its owners to publish a snapshot, or for a copy of the data");
        }
        if said.starts_with("Failed to fetch snapshot") {
            return Error::new(Kind::ReferenceUnresolved, format!("{self} does not exist"))
                .help(format!(
                    "check the snapshots at {SITE}/datasets/{}, or leave the snapshot out for the newest",
                    self.id
                ));
        }
        Error::new(
            Kind::SourceInvalid,
            format!(
                "openneuro could not list {self}: it said \"{}\"",
                said.escape_debug()
            ),
        )
        .help("try again later. if it keeps happening, check the dataset at openneuro.org")
    }

    /// the page that shows the dataset, at its snapshot when it names one.
    fn page(&self) -> String {
        match &self.tag {
            Some(tag) => format!("{SITE}/datasets/{}/versions/{tag}", self.id),
            None => format!("{SITE}/datasets/{}", self.id),
        }
    }

    fn too_many(&self) -> Error {
        super::too_many(
            self,
            &format!("download the folders you need from {}", self.page()),
        )
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.tag {
            Some(tag) => write!(f, "openneuro:{}@{tag}", self.id),
            None => write!(f, "openneuro:{}", self.id),
        }
    }
}

/// an accession as openneuro writes it: `ds` and six digits, in lowercase.
fn accession(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let digits = lower.strip_prefix("ds")?;
    (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(lower)
}

/// a snapshot's tag, as `1.0.0` or `00001`: letters, digits, `.`, `-` and `_`, which a query
/// carries as they are.
fn tag_of(text: &str) -> Option<String> {
    let usable = (1..=32).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    usable.then(|| text.to_owned())
}

/// what a git-annex key states of its file: its size, and its SHA-256 or MD5 for the backends
/// that name the bytes by one, as `SHA256E-s1757261330--7cfba0….nii.gz` does. a key of another
/// backend states no digest.
#[derive(Debug, PartialEq, Eq)]
struct Key {
    size: Option<u64>,
    sha256: Option<Sha256>,
    md5: Option<Md5>,
}

impl Key {
    fn parse(key: &str) -> Option<Key> {
        let (head, name) = key.split_once("--")?;
        let mut fields = head.split('-');
        let backend = fields.next()?;
        let mut size = None;
        for field in fields {
            match field.split_at_checked(1)? {
                ("s", digits) => size = Some(digits.parse().ok()?),
                // a chunk of a file, not the file, which its digest would not name
                ("S" | "C", _) => {
                    return Some(Key {
                        size: None,
                        sha256: None,
                        md5: None,
                    });
                }
                _ => {}
            }
        }
        let extended = backend.ends_with('E');
        let digest = |length: usize| {
            let hex = name.get(..length)?;
            let rest = &name[length..];
            let ends = rest.is_empty() || (extended && rest.starts_with('.'));
            ends.then_some(hex)
        };
        Some(match backend.trim_end_matches('E') {
            "SHA256" => Key {
                size,
                sha256: Some(Sha256::from_hex(digest(64)?)?),
                md5: None,
            },
            "MD5" => Key {
                size,
                sha256: None,
                md5: Some(Md5::from_hex(digest(32)?)?),
            },
            _ => Key {
                size,
                sha256: None,
                md5: None,
            },
        })
    }
}

/// the files listed so far, the folders still to list, by id and path, and the patterns whose
/// files are wanted, none when every file is.
#[derive(Default)]
struct Found {
    files: Vec<Remote>,
    folders: VecDeque<(String, String)>,
    select: Vec<Glob>,
}

impl Found {
    /// whether the folder at `path` may hold a wanted file, at any depth.
    fn opens(&self, path: &str) -> bool {
        self.select.is_empty() || self.select.iter().any(|glob| glob.may_reach(path))
    }

    /// whether the file at `path` lies in the folders a pattern's scope names, so the listing
    /// holds every file of the folders it names.
    fn keeps(&self, path: &str) -> bool {
        self.select.is_empty() || self.select.iter().any(|glob| glob.scope_holds(path))
    }

    /// adds the entries of the folder at `folder`, the top one for an empty path.
    fn add(&mut self, dataset: &Dataset, folder: &str, entries: Vec<Entry>) -> Result<(), Error> {
        for entry in entries {
            if entry.filename.is_empty() || entry.filename.contains('/') {
                return Err(dataset.invalid(&format!(
                    "an entry in {} has the name \"{}\"",
                    if folder.is_empty() {
                        "the top folder"
                    } else {
                        folder
                    },
                    entry.filename.escape_debug()
                )));
            }
            let path = if folder.is_empty() {
                entry.filename.clone()
            } else {
                format!("{folder}/{}", entry.filename)
            };
            if entry.directory {
                if folder.is_empty() && entry.filename == DATALAD {
                    continue;
                }
                if !is_git_id(&entry.id) {
                    return Err(dataset.invalid(&format!("the folder {path} has no usable id")));
                }
                if self.opens(&path) {
                    self.folders.push_back((entry.id, path));
                }
                continue;
            }
            if self.keeps(&path) {
                self.files.push(file(dataset, path, &entry)?);
            }
        }
        Ok(())
    }
}

/// a file of the listing: one git-annex keeps, by its key, from openneuro's S3 bucket at the
/// version the snapshot pins, or one git keeps, by its object, from openneuro itself.
fn file(dataset: &Dataset, path: String, entry: &Entry) -> Result<Remote, Error> {
    let size = entry
        .size
        .as_ref()
        .and_then(number)
        .ok_or_else(|| dataset.invalid(&format!("the size of {path} is not a number")))?;
    let objects = format!("{SITE}/crn/datasets/{}/objects/", dataset.id);
    let (key, url) = if entry.annexed {
        let key = Key::parse(&entry.id).ok_or_else(|| {
            dataset.invalid(&format!(
                "{path} is kept under \"{}\", which is no git-annex key",
                entry.id.escape_debug()
            ))
        })?;
        if key.size.is_some_and(|stated| stated != size) {
            return Err(dataset.invalid(&format!(
                "{path} is listed at {size} bytes, and its key says otherwise"
            )));
        }
        let bucket = format!("/openneuro.org/{}/", dataset.id);
        let stored = entry
            .urls
            .iter()
            .filter_map(|link| Url::parse(link).ok())
            .filter(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("s3.amazonaws.com")
                    && url.path().starts_with(&bucket)
            })
            .max_by_key(|url| url.query_pairs().any(|(name, _)| name == "versionId"));
        let url = match stored {
            Some(url) => url,
            None => Url::parse(&format!("{objects}{}", entry.id))
                .map_err(|_| dataset.invalid(&format!("{path} has no usable link")))?,
        };
        (key, url)
    } else {
        if !is_git_id(&entry.id) {
            return Err(dataset.invalid(&format!("{path} has no usable git id")));
        }
        let mut url = Url::parse(&format!("{objects}{}", entry.id))
            .map_err(|_| dataset.invalid(&format!("{path} has no usable link")))?;
        url.query_pairs_mut()
            .append_pair("filename", &entry.filename);
        (
            Key {
                size: None,
                sha256: None,
                md5: None,
            },
            url,
        )
    };
    Ok(Remote {
        path,
        size: Some(size),
        sha256: key.sha256,
        md5: key.md5,
        url,
        named_by_server: false,
        etag: None,
    })
}

/// whether `text` is a git object's id: 40 hex digits.
fn is_git_id(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// a size as GraphQL's `BigInt` gives one: a number, or text holding one.
fn number(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

#[derive(Deserialize)]
struct Answer<T> {
    data: Option<T>,
    #[serde(default)]
    errors: Vec<Problem>,
}

#[derive(Deserialize)]
struct Problem {
    message: String,
}

#[derive(Deserialize)]
struct Latest {
    dataset: Option<Newest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Newest {
    latest_snapshot: Option<Tagged>,
}

#[derive(Deserialize)]
struct Tagged {
    tag: String,
}

#[derive(Deserialize)]
struct Top {
    snapshot: Option<Snapshot>,
}

#[derive(Deserialize)]
struct Snapshot {
    tag: Option<String>,
    deprecated: Option<Deprecated>,
    description: Option<Description>,
    #[serde(default)]
    files: Vec<Entry>,
}

#[derive(Deserialize)]
struct Deprecated {
    reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct Description {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "License")]
    license: Option<String>,
    #[serde(rename = "DatasetDOI")]
    doi: Option<String>,
}

#[derive(Deserialize)]
struct Folder {
    snapshot: Option<Listed>,
}

#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    files: Vec<Entry>,
}

#[derive(Clone, Deserialize)]
struct Entry {
    id: String,
    filename: String,
    size: Option<serde_json::Value>,
    #[serde(default)]
    directory: bool,
    #[serde(default)]
    annexed: bool,
    #[serde(default)]
    urls: Vec<String>,
}

#[cfg(test)]
mod tests;
