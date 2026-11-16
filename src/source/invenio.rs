//! invenioRDM records, on zenodo.org or on any other install: a record's files by its id, listed
//! from the install's records api with the size and MD5 it states for each. zenodo.org answers
//! that api in an older form of its own, whose license ids, as `cc-zero`, data.lock has always
//! recorded, so its records are read in that form; every other install answers in invenioRDM's.
//! a link or a DOI that leads to a record page of another install is known by one request for
//! the record at that origin.

use std::collections::BTreeMap;
use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, invalid, malformed};
use crate::account::Account;
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

const ZENODO: &str = "zenodo.org";

/// a record of an invenioRDM install. on zenodo a record id names one version, and a concept id
/// the newest of its versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    /// another install's host, with its port when that is not 443; none for zenodo.org.
    host: Option<String>,
    /// the record's id: digits on zenodo, as `3608990`, and elsewhere as the install makes them,
    /// as `dt797-nsy79`.
    id: String,
}

impl Record {
    /// reads the part after `zenodo:`, a record's number.
    pub(super) fn zenodo(text: &str) -> Result<Record, Error> {
        let id: u64 = text
            .trim()
            .parse()
            .map_err(|_| malformed(&format!("zenodo:{text}"), "zenodo:3608990"))?;
        Ok(Record::on_zenodo(id))
    }

    /// reads the part after `invenio:`, an install's host and a record's id there, as
    /// `data.caltech.edu/dt797-nsy79`. zenodo's records keep their own spelling.
    pub(super) fn parse(text: &str) -> Result<Record, Error> {
        let bad = || {
            malformed(
                &format!("invenio:{text}"),
                "invenio:data.caltech.edu/dt797-nsy79, with the install's host and the record's id",
            )
        };
        let (host, id) = text.trim().split_once('/').ok_or_else(bad)?;
        let host = crate::auth::site::host(host).ok_or_else(bad)?;
        if host == ZENODO {
            return id.parse().map(Record::on_zenodo).map_err(|_| bad());
        }
        if !is_id(id) {
            return Err(bad());
        }
        Ok(Record {
            host: Some(host),
            id: id.to_owned(),
        })
    }

    fn on_zenodo(id: u64) -> Record {
        Record {
            host: None,
            id: id.to_string(),
        }
    }

    /// a pasted zenodo link, and the file in it when the link names one. a DOI zenodo registered
    /// leads to its record through zenodo's `/doi/` page.
    pub(super) fn from_link(url: &Url) -> Option<(Record, Option<String>)> {
        if !matches!(url.host_str()?, "zenodo.org" | "www.zenodo.org") {
            return None;
        }
        let mut parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        if parts.first() == Some(&"doi") {
            let doi = percent_decode_str(&parts[1..].join("/"))
                .decode_utf8()
                .ok()?
                .to_ascii_lowercase();
            let id = doi.strip_prefix("10.5281/zenodo.")?.parse().ok()?;
            return Some((Record::on_zenodo(id), None));
        }
        if parts.first() == Some(&"api") {
            parts.remove(0);
        }
        let (kind, id) = (parts.first()?, parts.get(1)?);
        if !matches!(*kind, "records" | "record") {
            return None;
        }
        let record = Record::on_zenodo(id.parse().ok()?);
        let file = match parts.get(2) {
            Some(&"files") if parts.len() > 3 => {
                let mut segments = parts[3..].to_vec();
                // the API serves a file's bytes at .../files/<name>/content
                if segments.len() > 1 && segments.last() == Some(&"content") {
                    segments.pop();
                }
                Some(
                    percent_decode_str(&segments.join("/"))
                        .decode_utf8_lossy()
                        .into_owned(),
                )
            }
            _ => None,
        };
        Some((record, file))
    }

    /// the record a link on another https host may name, which only its install's answer can
    /// confirm: a record's page, `/records/<id>`, or the page of a DOI the install registered,
    /// `/doi/<doi>`, whose suffix is then the record's id; with that DOI.
    pub(super) fn behind(url: &Url) -> Option<(Record, Option<String>)> {
        let name = url.host_str()?;
        if url.scheme() != "https" || matches!(name, "zenodo.org" | "www.zenodo.org") {
            return None;
        }
        let host = match url.port() {
            Some(port) => format!("{name}:{port}"),
            None => name.to_owned(),
        };
        let host = Some(crate::auth::site::host(&host)?);
        let parts: Vec<&str> = url.path_segments()?.collect();
        let parts = match parts.as_slice() {
            [rest @ .., ""] => rest,
            all => all,
        };
        match parts {
            ["records", id] if is_id(id) => Some((
                Record {
                    host,
                    id: (*id).to_owned(),
                },
                None,
            )),
            ["doi", prefix, suffix] if is_id(suffix) => {
                let doi = super::doi::Doi::bare(&format!("{prefix}/{suffix}"))?;
                Some((
                    Record {
                        host,
                        id: (*suffix).to_owned(),
                    },
                    Some(doi.name().to_owned()),
                ))
            }
            _ => None,
        }
    }

    /// the origin of another install, where every request for its records goes; none for
    /// zenodo.org, which is no origin a person names.
    pub(super) fn origin(&self) -> Option<url::Origin> {
        let host = self.host.as_ref()?;
        Url::parse(&format!("https://{host}/"))
            .ok()
            .map(|url| url.origin())
    }

    /// whether the record is on another install than zenodo.org.
    pub(super) fn is_elsewhere(&self) -> bool {
        self.host.is_some()
    }

    /// asks the install's records api for the record's files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let response = api.get(&self.url(&[])?).await?;
        if self.host.is_none() {
            return self.zenodo_listing(super::json(self, &response)?);
        }
        if matches!(response.status, 401 | 403) {
            return Err(self.refused(&response));
        }
        self.listing(super::json(self, &response)?)
    }

    /// the newest version of the record, as its install's `versions/latest` answer names it:
    /// zenodo's id is a number, another install's an id it makes.
    pub(super) async fn latest(&self, api: &impl Api) -> Result<Record, Error> {
        let response = api.get(&self.url(&["versions", "latest"])?).await?;
        let answer: Newest = super::json(self, &response)?;
        let id = match (&self.host, answer.id) {
            (None, serde_json::Value::Number(id)) => {
                id.as_u64().filter(|id| *id > 0).map(|id| id.to_string())
            }
            (Some(_), serde_json::Value::String(id)) => Some(id).filter(|id| is_id(id)),
            _ => None,
        }
        .ok_or_else(|| invalid(self, "its newest version has no id"))?;
        Ok(Record {
            host: self.host.clone(),
            id,
        })
    }

    /// the listing of the record this candidate names when its install's answer, to one request
    /// for the record at its origin, is an invenioRDM record of that id and of `doi`, where a
    /// DOI's page named it; none for another answer. a refusal names the key that may lift it.
    pub(super) async fn recognized(
        &self,
        api: &impl Api,
        doi: Option<&str>,
    ) -> Result<Option<Listing>, Error> {
        let response = api.get(&self.url(&[])?).await?;
        if matches!(response.status, 401 | 403) {
            return Err(self.refused(&response));
        }
        if !(200..=299).contains(&response.status) {
            return Ok(None);
        }
        let Ok(answer) = response.json::<Answer>() else {
            return Ok(None);
        };
        let named = doi.is_none_or(|doi| {
            answer
                .pids
                .doi
                .as_ref()
                .is_some_and(|stated| stated.identifier.eq_ignore_ascii_case(doi))
        });
        if answer.id != self.id || !named {
            return Ok(None);
        }
        self.listing(answer).map(Some)
    }

    /// the listing of another install's answer in invenioRDM's form.
    fn listing(&self, answer: Answer) -> Result<Listing, Error> {
        let parent = answer.parent.as_ref().map(|parent| parent.id.as_str());
        if answer.id != self.id && parent != Some(self.id.as_str()) {
            return Err(invalid(self, "the record or its parent id does not match"));
        }
        if !is_id(&answer.id) {
            return Err(invalid(self, "the record's id is not one"));
        }
        if !answer.files.enabled {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{self} is a record of metadata alone, with no files"),
            )
            .help(format!("check the record at {}", self.page())));
        }
        let entries = answer.files.entries.unwrap_or_default();
        let open = answer.access.files == "public";
        if entries.is_empty() && !open {
            return Err(self.held_back(&answer.access.status));
        }
        if let Some(count) = answer.files.count
            && count > entries.len() as u64
        {
            return Err(invalid(
                self,
                &format!(
                    "it lists {} of the {} it counts",
                    entries.len(),
                    crate::units::count(count, "file", "files")
                ),
            ));
        }
        let resolved = Record {
            host: self.host.clone(),
            id: answer.id,
        };
        let files = entries
            .into_iter()
            .map(|(key, file)| file.remote(&resolved, &key))
            .collect::<Result<_, _>>()?;
        let access = if open && answer.access.record == "public" {
            Access::Public
        } else {
            // the install lists the files of a record it holds back only to a key that may read them
            Access::Login
        };
        Ok(Listing {
            within: Vec::new(),
            access,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Invenio(resolved),
            title: answer.metadata.title,
            license: answer
                .metadata
                .rights
                .into_iter()
                .find_map(|right| right.id),
            doi: answer.pids.doi.map(|doi| doi.identifier),
            files,
        })
    }

    /// the listing of zenodo's answer, in zenodo's own form.
    fn zenodo_listing(&self, answer: ZenodoAnswer) -> Result<Listing, Error> {
        if answer.id == 0 {
            return Err(invalid(self, "the record has no concrete id"));
        }
        let concept = answer.conceptrecid.as_deref();
        if answer.id.to_string() != self.id && concept != Some(self.id.as_str()) {
            return Err(invalid(self, "the record or concept id does not match"));
        }
        let access = match answer.metadata.access_right.as_str() {
            "open" => Access::Public,
            // zenodo lists a restricted record's files only to an account allowed to read them
            "restricted" | "embargoed" | "closed" if !answer.files.is_empty() => Access::Login,
            "restricted" | "embargoed" | "closed" => {
                let page = format!("https://zenodo.org/records/{}", answer.id);
                return Err(Error::new(
                    Kind::AuthRequired,
                    format!("{self} is {}", answer.metadata.access_right),
                )
                .help(format!(
                    "request access at {page}, then sign in with `fetchloom auth login zenodo` and run the same command again"
                ))
                .needs(Account::Zenodo, Some(page)));
            }
            _ => {
                return Err(invalid(self, "the access condition is not recognized"));
            }
        };
        let resolved = Record::on_zenodo(answer.id);
        // the records API embeds all files, including the 1,123 in record 7793716.
        // its separate files endpoint is unpaged, so another request adds no files
        let files = answer
            .files
            .into_iter()
            .map(|file| {
                let key = file.key.clone();
                File {
                    key: file.key,
                    size: file.size,
                    checksum: Some(file.checksum),
                }
                .remote(&resolved, &key)
            })
            .collect::<Result<_, _>>()?;
        Ok(Listing {
            within: Vec::new(),
            access,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Invenio(resolved),
            title: Some(answer.metadata.title),
            license: answer.metadata.license.map(|license| license.id),
            doi: answer.doi,
            files,
        })
    }

    /// another install's refusal of the record, which a key saved for it may lift.
    fn refused(&self, response: &Response) -> Error {
        let host = self.host.as_deref().unwrap_or(ZENODO);
        let command = token_command(host);
        let error = Error::new(
            Kind::AuthRequired,
            format!("{host} refused access to {self}"),
        );
        if response.signed.is_some() {
            return error.help(format!(
                "the key saved for {host} was refused or cannot read it: save a current one with `{command}`, or request access at {}",
                self.page()
            ));
        }
        error
            .help(format!(
                "if you may read the record, save a key with `{command}`, then run the same command again; otherwise request access at {}",
                self.page()
            ))
            .needs_site(host.to_owned())
    }

    /// another install's record whose files it holds back, as its access `status` says.
    fn held_back(&self, status: &str) -> Error {
        let host = self.host.as_deref().unwrap_or(ZENODO);
        Error::new(Kind::AuthRequired, format!("{self} is {status}"))
            .help(format!(
                "request access at {}, then save a key with `{}` and run the same command again",
                self.page(),
                token_command(host)
            ))
            .needs_site(host.to_owned())
    }

    /// the record in the install's records api, with `more` after it, each one encoded segment.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        let host = self.host.as_deref().unwrap_or(ZENODO);
        let mut url = Url::parse(&format!("https://{host}/api/records"))
            .map_err(|_| invalid(self, "the record URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| invalid(self, "the record URL has no path"))?
            .push(&self.id)
            .extend(more);
        Ok(url)
    }

    /// the record's page on its install.
    pub(super) fn page(&self) -> String {
        let host = self.host.as_deref().unwrap_or(ZENODO);
        format!("https://{host}/records/{}", self.id)
    }
}

/// an id a record's page names: letters, digits, `-`, `_` and `.`, a letter or digit first, at
/// most 100 long, as invenioRDM makes `dt797-nsy79` and zenodo `3608990`.
fn is_id(text: &str) -> bool {
    (1..=100).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_alphanumeric())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// the command that saves a key for the install at `host`, for it alone.
fn token_command(host: &str) -> String {
    format!("{} --token", Account::Login(host.to_owned()).command())
}

/// another install's record, in invenioRDM's form.
#[derive(Deserialize)]
struct Answer {
    id: String,
    parent: Option<Parent>,
    #[serde(default)]
    pids: Pids,
    access: RecordAccess,
    files: Files,
    metadata: Metadata,
}

#[derive(Deserialize)]
struct Parent {
    id: String,
}

#[derive(Default, Deserialize)]
struct Pids {
    doi: Option<Pid>,
}

#[derive(Deserialize)]
struct Pid {
    identifier: String,
}

#[derive(Deserialize)]
struct RecordAccess {
    record: String,
    files: String,
    status: String,
}

#[derive(Deserialize)]
struct Files {
    enabled: bool,
    count: Option<u64>,
    /// by name, which sorts them.
    entries: Option<BTreeMap<String, File>>,
}

#[derive(Deserialize)]
struct Metadata {
    title: Option<String>,
    #[serde(default)]
    rights: Vec<Right>,
}

#[derive(Deserialize)]
struct Right {
    id: Option<String>,
}

#[derive(Deserialize)]
struct File {
    key: String,
    size: u64,
    checksum: Option<String>,
}

impl File {
    /// the file `key` names in the record `record`, at its link in the install's api.
    fn remote(self, record: &Record, key: &str) -> Result<Remote, Error> {
        if self.key != key {
            return Err(invalid(
                record,
                &format!(
                    "it lists \"{}\" under another name",
                    self.key.escape_debug()
                ),
            ));
        }
        let md5 = self
            .checksum
            .as_deref()
            .and_then(|checksum| checksum.strip_prefix("md5:"))
            .and_then(Md5::from_hex)
            .ok_or_else(|| {
                invalid(
                    record,
                    &format!("the checksum for \"{}\" is not an MD5", self.key),
                )
            })?;
        let url = record.url(&["files", &self.key, "content"])?;
        Ok(Remote {
            path: self.key,
            size: Some(self.size),
            sha256: None,
            md5: Some(md5),
            url,
            named_by_server: false,
            etag: None,
        })
    }
}

/// the id of the newest version, in either form of the answer.
#[derive(Deserialize)]
struct Newest {
    id: serde_json::Value,
}

/// zenodo's record, in zenodo's own form.
#[derive(Deserialize)]
struct ZenodoAnswer {
    id: u64,
    conceptrecid: Option<String>,
    metadata: ZenodoMetadata,
    doi: Option<String>,
    files: Vec<ZenodoFile>,
}

#[derive(Deserialize)]
struct ZenodoMetadata {
    title: String,
    license: Option<ZenodoLicense>,
    access_right: String,
}

#[derive(Deserialize)]
struct ZenodoLicense {
    id: String,
}

#[derive(Deserialize)]
struct ZenodoFile {
    key: String,
    size: u64,
    checksum: String,
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            None => write!(f, "zenodo:{}", self.id),
            Some(host) => write!(f, "invenio:{host}/{}", self.id),
        }
    }
}

#[cfg(test)]
mod tests;
