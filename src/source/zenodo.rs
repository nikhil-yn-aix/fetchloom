//! zenodo: records by id or link, listed through the records API, with an MD5 for every file.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::{Listing, Reference, Remote, malformed};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::Api;

/// a zenodo record. a record id names one version, and a concept id the newest of its versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    id: u64,
}

impl Record {
    /// reads the part after `zenodo:`.
    pub(super) fn parse(text: &str) -> Result<Record, Error> {
        let id = text
            .trim()
            .parse()
            .map_err(|_| malformed(&format!("zenodo:{text}"), "zenodo:3608990"))?;
        Ok(Record { id })
    }

    /// a pasted zenodo link, and the file in it when the link names one.
    pub(super) fn from_link(url: &Url) -> Option<(Record, Option<String>)> {
        if !matches!(url.host_str()?, "zenodo.org" | "www.zenodo.org") {
            return None;
        }
        let mut parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        if parts.first() == Some(&"api") {
            parts.remove(0);
        }
        let (kind, id) = (parts.first()?, parts.get(1)?);
        if !matches!(*kind, "records" | "record") {
            return None;
        }
        let record = Record {
            id: id.parse().ok()?,
        };
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

    /// asks the records API for the record's files.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let response = api.get(&self.url()?).await?;
        let answer: Answer = super::json(self, &response)?;
        if answer.id == 0 {
            return Err(self.invalid("the record has no concrete id"));
        }
        let concept = answer
            .conceptrecid
            .as_deref()
            .and_then(|id| id.parse::<u64>().ok());
        if answer.id != self.id && concept != Some(self.id) {
            return Err(self.invalid("the record or concept id does not match"));
        }
        match answer.metadata.access_right.as_str() {
            "open" => (),
            // zenodo lists a restricted record's files only to an account allowed to read them
            "restricted" | "embargoed" | "closed" if !answer.files.is_empty() => (),
            "restricted" | "embargoed" | "closed" => {
                let page = format!("https://zenodo.org/records/{}", answer.id);
                return Err(Error::new(
                    Kind::AuthRequired,
                    format!("{self} is {}", answer.metadata.access_right),
                )
                .help(format!(
                    "request access at {page}, then sign in with `fetchloom auth login zenodo` and run the same command again"
                ))
                .needs(crate::auth::Account::Zenodo, Some(page)));
            }
            _ => return Err(self.invalid("the access condition is not recognized")),
        }
        let resolved = Record { id: answer.id };
        // the records API embeds all files, including the 1,123 in record 7793716.
        // its separate files endpoint is unpaged, so another request adds no files.
        let files = answer
            .files
            .into_iter()
            .map(|file| file.remote(&resolved))
            .collect::<Result<_, _>>()?;
        Ok(Listing {
            within: Vec::new(),
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Zenodo(resolved),
            title: Some(answer.metadata.title),
            license: answer.metadata.license.map(|license| license.id),
            doi: answer.doi,
            files,
        })
    }

    fn url(&self) -> Result<Url, Error> {
        Url::parse(&format!("https://zenodo.org/api/records/{}", self.id))
            .map_err(|_| self.invalid("the record URL is invalid"))
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("check the record on zenodo.org and report the invalid listing to its owner")
    }
}

#[derive(Deserialize)]
struct Answer {
    id: u64,
    conceptrecid: Option<String>,
    metadata: Metadata,
    doi: Option<String>,
    files: Vec<File>,
}

#[derive(Deserialize)]
struct Metadata {
    title: String,
    license: Option<License>,
    access_right: String,
}

#[derive(Deserialize)]
struct License {
    id: String,
}

#[derive(Deserialize)]
struct File {
    key: String,
    size: u64,
    checksum: String,
}

impl File {
    fn remote(self, record: &Record) -> Result<Remote, Error> {
        let md5 = self
            .checksum
            .strip_prefix("md5:")
            .and_then(Md5::from_hex)
            .ok_or_else(|| {
                record.invalid(&format!("the checksum for \"{}\" is not an MD5", self.key))
            })?;
        let mut url = record.url()?;
        url.path_segments_mut()
            .map_err(|()| record.invalid("the file URL has no path"))?
            .extend(["files", &self.key, "content"]);
        Ok(Remote {
            path: self.key,
            size: Some(self.size),
            sha256: None,
            md5: Some(md5),
            url,
            named_by_server: false,
        })
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "zenodo:{}", self.id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::digest::Md5;
    use crate::error::Kind;
    use crate::source::recorded::{Recorded, block_on, fixture_bytes};
    use crate::source::{Reference, Remote};

    const IRIS: &str = "https://zenodo.org/api/records/3608990";

    fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut value = serde_json::from_slice(&fixture_bytes("zenodo/iris.json")).unwrap();
        change(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn lists_every_field_of_iris_in_one_request() {
        let recorded = Recorded::default().fixture(IRIS, "zenodo/iris.json");
        let listing = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
        assert_eq!(listing.title.as_deref(), Some("DatasetIris"));
        assert_eq!(listing.license.as_deref(), Some("cc-by-4.0"));
        assert_eq!(listing.doi.as_deref(), Some("10.5281/zenodo.3608990"));
        assert_eq!(
            listing.files,
            vec![Remote {
                path: "iris.csv".to_owned(),
                size: Some(4177),
                sha256: None,
                md5: Md5::from_hex("395d6abd001abf3760a1a794ef4c453c"),
                url: Url::parse("https://zenodo.org/api/records/3608990/files/iris.csv/content")
                    .unwrap(),
                named_by_server: false,
            }]
        );
        assert_eq!(recorded.asked(), [IRIS]);
    }

    #[test]
    fn large_records_need_no_separate_file_requests() {
        let url = "https://zenodo.org/api/records/7793716";
        let recorded = Recorded::default().fixture(url, "zenodo/many.json");
        let listing = block_on(Record::parse("7793716").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.files.len(), 1123);
        assert_eq!(listing.files[0].path, "covid19_2023_01_16.json.gz");
        assert_eq!(listing.files[1122].path, "covid19_2022_02_28.json.gz");
        assert_eq!(recorded.asked(), [url]);
    }

    #[test]
    fn concepts_pin_the_concrete_record_when_resolved_again() {
        let concept = "https://zenodo.org/api/records/3608989";
        let recorded = Recorded::default()
            .fixture(concept, "zenodo/concept.json")
            .fixture(IRIS, "zenodo/iris.json");
        let listing = block_on(Record::parse("3608989").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
        let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
        assert_eq!(pinned.files, listing.files);
        assert_eq!(recorded.asked(), [concept, IRIS]);
    }

    #[test]
    fn answers_must_match_the_requested_record_or_concept() {
        for (requested, id, concept, valid) in [
            ("3608990", 3_608_991, Some("3608989"), false),
            ("3608989", 3_608_990, Some("9999999"), false),
            ("3608989", 3_608_990, None, false),
            ("3608989", 3_608_990, Some("not an id"), false),
            ("3608990", 3_608_990, None, true),
            ("3608989", 3_608_990, Some("3608989"), true),
        ] {
            let url = format!("https://zenodo.org/api/records/{requested}");
            let body = changed(|value| {
                value["id"] = id.into();
                value["conceptrecid"] = concept.into();
            });
            let recorded = Recorded::default().on(&url, 200, body);
            let reference = Reference::parse(&format!("zenodo:{requested}"))
                .unwrap()
                .reference;
            let result = block_on(reference.resolve(&recorded));
            if valid {
                assert_eq!(result.unwrap().resolved.to_string(), format!("zenodo:{id}"));
            } else {
                assert_eq!(result.unwrap_err().kind, Kind::SourceInvalid, "{requested}");
            }
            assert_eq!(recorded.asked(), [url]);
        }
    }

    #[test]
    fn file_keys_are_one_encoded_url_segment() {
        let body = changed(|value| value["files"][0]["key"] = "folder/data #1%?.csv".into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let listing = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.files[0].path, "folder/data #1%?.csv");
        assert_eq!(
            listing.files[0].url.as_str(),
            "https://zenodo.org/api/records/3608990/files/folder%2Fdata%20%231%25%3F.csv/content"
        );
    }

    #[test]
    fn missing_records_are_unresolved() {
        let url = "https://zenodo.org/api/records/999999999999";
        let recorded = Recorded::default().on(url, 404, fixture_bytes("zenodo/missing.json"));
        let error =
            block_on(Record::parse("999999999999").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved);
        assert_eq!(recorded.asked(), [url]);
    }

    #[test]
    fn access_refusals_require_authorization() {
        for status in [401, 403] {
            let recorded =
                Recorded::default().on(IRIS, status, fixture_bytes("zenodo/refused.json"));
            let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
            assert_eq!(error.kind, Kind::AuthRequired);
            assert!(error.help.is_some());
        }
    }

    #[test]
    fn restricted_records_do_not_look_empty() {
        let url = "https://zenodo.org/api/records/14110909";
        let recorded = Recorded::default().fixture(url, "zenodo/restricted.json");
        let error = block_on(Record::parse("14110909").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.message.contains("restricted"));
        assert!(error.help.is_some());
    }

    #[test]
    fn embargoed_records_explain_the_access_condition() {
        let body = changed(|value| {
            value["metadata"]["access_right"] = "embargoed".into();
            value["files"] = serde_json::json!([]);
        });
        let recorded = Recorded::default().on(IRIS, 200, body);
        let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.message.contains("embargoed"));
        let need = error.need.unwrap();
        assert_eq!(need.account, crate::auth::Account::Zenodo);
        assert_eq!(
            need.terms.as_deref(),
            Some("https://zenodo.org/records/3608990")
        );
    }

    #[test]
    fn a_restricted_record_listed_for_this_account_downloads() {
        let body = changed(|value| value["metadata"]["access_right"] = "restricted".into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let listing = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.files.len(), 1);
    }

    #[test]
    fn malformed_checksums_are_not_silently_discarded() {
        let body = changed(|value| value["files"][0]["checksum"] = "md5:broken".into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
    }

    #[test]
    fn missing_file_lists_are_not_empty_datasets() {
        let body = changed(|value| {
            value.as_object_mut().unwrap().remove("files");
        });
        let recorded = Recorded::default().on(IRIS, 200, body);
        let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
    }

    #[test]
    fn escaping_paths_reach_the_shared_listing_check() {
        let body = changed(|value| value["files"][0]["key"] = "../iris.csv".into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let reference = Reference::parse("zenodo:3608990").unwrap().reference;
        assert_eq!(
            block_on(reference.resolve(&recorded)).unwrap_err().kind,
            Kind::SourceInvalid
        );
    }

    #[test]
    fn unknown_access_conditions_are_invalid_answers() {
        let body = changed(|value| value["metadata"]["access_right"] = "unknown".into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.help.is_some());
    }

    #[test]
    fn a_zero_record_id_cannot_be_a_version_pin() {
        let body = changed(|value| value["id"] = 0.into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let error = block_on(Record::parse("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
    }

    #[test]
    #[ignore = "live: talks to zenodo.org"]
    fn live_zenodo() {
        let work = Arc::new(crate::work::Work::default());
        let net = crate::net::Net::new(
            crate::net::Options {
                offline: crate::net::Offline::No,
                jobs: 4,
                credentials: Vec::new(),
                split: crate::net::Splitting::WhenSlow,
            },
            work.clone(),
            Arc::default(),
            Arc::new(|_| {}),
        );
        let start = Instant::now();
        let reference = Reference::parse("zenodo:3608990").unwrap().reference;
        let listing = block_on(reference.resolve(&net)).unwrap();
        eprintln!(
            "zenodo: {} files, {} requests, {:?}",
            listing.files.len(),
            work.counts().requests,
            start.elapsed()
        );
        assert_eq!(listing.files.len(), 1);
        assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
        assert_eq!(work.counts().requests, 1);
    }
}
