use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::Md5;
use crate::error::Kind;
use crate::source::Remote;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const DOI: &str = "10.5281/zenodo.3608990";
const HANDLE: &str = "https://doi.org/api/handles/10.5281%2Fzenodo.3608990";
const RECORD: &str = "https://zenodo.org/api/records/3608990";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("doi/zenodo.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn every_real_handle_url_routes_without_getting_the_landing_page() {
    let cases = [
        (
            "10.5281/zenodo.3608990",
            "https://doi.org/api/handles/10.5281%2Fzenodo.3608990",
            "doi/zenodo.json",
            "zenodo:3608990",
        ),
        (
            "10.6084/m9.figshare.34744689.v1",
            "https://doi.org/api/handles/10.6084%2Fm9.figshare.34744689.v1",
            "doi/figshare.json",
            "figshare:34744689@1",
        ),
        (
            "10.7910/DVN/TJCLKP",
            "https://doi.org/api/handles/10.7910%2Fdvn%2Ftjclkp",
            "doi/dataverse.json",
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
        ),
        (
            "10.17605/OSF.IO/EZCUJ",
            "https://doi.org/api/handles/10.17605%2Fosf.io%2Fezcuj",
            "doi/osf.json",
            "osf:ezcuj",
        ),
        (
            "10.5061/dryad.j1fd7",
            "https://doi.org/api/handles/10.5061%2Fdryad.j1fd7",
            "doi/dryad.json",
            "dryad:10.5061/dryad.j1fd7",
        ),
    ];
    for (doi, url, fixture, target) in cases {
        let recorded = Recorded::default().fixture(url, fixture);
        let reference = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap();
        assert_eq!(reference.to_string(), target);
        assert_eq!(recorded.asked(), [url]);
    }
}

/// doi.org's answer for a kaggle dataset version, recorded on 2026-11-07.
const KAGGLE_HANDLE: &str = r#"{"responseCode":1,"handle":"10.34740/kaggle/dsv/2736749","values":[{"index":100,"type":"HS_ADMIN","data":{"format":"admin","value":{"handle":"10.admin/codata","index":300,"permissions":"111111111111"}},"ttl":86400,"timestamp":"2024-10-10T15:05:22Z"},{"index":1,"type":"URL","data":{"format":"string","value":"https://www.kaggle.com/dsv/2736749"},"ttl":86400,"timestamp":"2024-10-10T15:05:22Z"}]}"#;

#[test]
fn kaggle_dois_follow_kaggle_to_the_dataset_version() {
    let handle = "https://doi.org/api/handles/10.34740%2Fkaggle%2Fdsv%2F2736749";
    let page = "https://www.kaggle.com/dsv/2736749";
    let recorded = Recorded::default()
        .on(handle, 200, KAGGLE_HANDLE)
        .on(page, 200, "<!DOCTYPE html>")
        .redirected(
            page,
            "https://www.kaggle.com/datasets/obulisainaren/retinal-oct-c8/versions/2",
        );
    let doi = Doi::parse("10.34740/kaggle/dsv/2736749").unwrap();
    let target = block_on(doi.locate(&recorded)).unwrap();
    assert_eq!(target.to_string(), "kaggle:obulisainaren/retinal-oct-c8@2");
    assert_eq!(recorded.asked(), [handle, page]);
}

#[test]
fn a_known_site_page_that_is_no_dataset_is_not_blamed_on_the_publisher() {
    let handle = "https://doi.org/api/handles/10.34740%2Fkaggle%2Fdsv%2F2736749";
    let body = KAGGLE_HANDLE.replace(
        "https://www.kaggle.com/dsv/2736749",
        "https://www.kaggle.com/code/someone/notebook",
    );
    let recorded = Recorded::default().on(handle, 200, body);
    let doi = Doi::parse("10.34740/kaggle/dsv/2736749").unwrap();
    let error = block_on(doi.locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.message.contains("kaggle"), "{}", error.message);
    assert!(!error.help.unwrap_or_default().contains("publisher"));
}

#[test]
fn bare_prefixed_and_web_dois_return_the_complete_dataset() {
    for text in [
        DOI,
        "doi:10.5281/zenodo.3608990",
        "https://doi.org/10.5281/zenodo.3608990",
    ] {
        let recorded = Recorded::default()
            .fixture(HANDLE, "doi/zenodo.json")
            .fixture(RECORD, "zenodo/iris.json");
        let reference = Reference::parse(text).unwrap().reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
        assert_eq!(listing.title.as_deref(), Some("DatasetIris"));
        assert_eq!(listing.license.as_deref(), Some("cc-by-4.0"));
        assert_eq!(listing.doi.as_deref(), Some(DOI));
        assert_eq!(
            listing.files,
            [Remote {
                path: "iris.csv".to_owned(),
                size: Some(4177),
                sha256: None,
                md5: Md5::from_hex("395d6abd001abf3760a1a794ef4c453c"),
                url: Url::parse("https://zenodo.org/api/records/3608990/files/iris.csv/content")
                    .unwrap(),
                named_by_server: false,
                etag: None,
            }]
        );
        assert_eq!(recorded.asked(), [HANDLE, RECORD]);
    }
}

#[test]
fn resolving_the_result_skips_the_handle_and_keeps_the_record() {
    let recorded = Recorded::default()
        .fixture(HANDLE, "doi/zenodo.json")
        .fixture(RECORD, "zenodo/iris.json");
    let reference = Reference::parse(DOI).unwrap().reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, listing.files);
    assert_eq!(pinned.resolved, listing.resolved);
    assert_eq!(recorded.asked(), [HANDLE, RECORD, RECORD]);
}

/// a handle is asked for with the DOI's punctuation encoded as text, and the link it holds
/// decides the source, not the registrant the DOI's prefix names.
#[test]
fn a_doi_is_asked_for_as_written_and_goes_where_its_handle_points() {
    for (doi, handle) in [
        (
            "10.1234/another-registrant",
            "https://doi.org/api/handles/10.1234%2Fanother-registrant",
        ),
        (
            "10.1234/a?b#c%d/e",
            "https://doi.org/api/handles/10.1234%2Fa%3Fb%23c%25d%2Fe",
        ),
    ] {
        let body = changed(|value| value["handle"] = doi.into());
        let recorded = Recorded::default().on(handle, 200, body);
        let target = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap();
        assert_eq!(target.to_string(), "zenodo:3608990", "{doi}");
        assert_eq!(recorded.asked(), [handle], "{doi}");
    }
}

#[test]
fn missing_handles_are_unresolved_with_either_http_status() {
    let doi = "10.5281/fetchloom-missing-999999999";
    let url = "https://doi.org/api/handles/10.5281%2Ffetchloom-missing-999999999";
    for status in [200, 404] {
        let recorded = Recorded::default().on(url, status, fixture_bytes("doi/missing.json"));
        let error = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved);
        assert!(error.help.is_some());
        assert_eq!(recorded.asked(), [url]);
    }
}

#[test]
fn unknown_hosts_are_named_without_getting_their_pages() {
    let url = "https://doi.org/api/handles/10.1038%2Fs41586-020-2649-2";
    let recorded = Recorded::default().fixture(url, "doi/unknown-host.json");
    let error = block_on(
        Doi::parse("10.1038/s41586-020-2649-2")
            .unwrap()
            .locate(&recorded),
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.message.contains("www.nature.com"));
    assert!(error.help.is_some());
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn access_refusals_use_the_shared_error() {
    for status in [401, 403] {
        let recorded = Recorded::default().on(HANDLE, status, fixture_bytes("doi/missing.json"));
        let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
    }
}

/// a handle answer fetchloom cannot use is refused for its reason, with help to report it.
#[test]
fn a_handle_answer_that_cannot_be_used_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 4] = [
        (
            "no URL value",
            |value| value["values"] = serde_json::json!([]),
            "the handle has no URL value",
        ),
        (
            "a failed lookup",
            |value| value["responseCode"] = 2.into(),
            "the handles API did not report success",
        ),
        (
            "another DOI's handle",
            |value| value["handle"] = "10.1234/another-doi".into(),
            "the handle names another DOI",
        ),
        (
            "a URL value in base64",
            |value| value["values"][1]["data"]["format"] = "base64".into(),
            "the URL value is not in string format",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = Recorded::default().on(HANDLE, 200, changed(change));
        let Err(error) = block_on(Doi::parse(DOI).unwrap().locate(&recorded)) else {
            panic!("{row}: the handle was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(error.help.is_some(), "{row}");
    }
}

#[test]
fn a_handle_cannot_route_back_to_a_doi() {
    let body = changed(|value| {
        value["values"][1]["data"]["value"] = "https://doi.org/10.5281/zenodo.3608990".into();
    });
    let recorded = Recorded::default().on(HANDLE, 200, body);
    let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.help.is_some());
    assert_eq!(recorded.asked(), [HANDLE]);
}

#[test]
#[ignore = "live: talks to doi.org and repository APIs"]
fn live_doi() {
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
    for (doi, target, requests) in [
        (DOI, "zenodo:3608990", 2),
        ("10.6084/m9.figshare.34744689.v1", "figshare:34744689@1", 2),
        (
            "10.7910/DVN/TJCLKP",
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@",
            2,
        ),
        ("10.17605/OSF.IO/EZCUJ", "osf:ezcuj", 3),
    ] {
        let before = work.counts().requests;
        let start = Instant::now();
        let reference = Reference::parse(doi).unwrap().reference;
        let listing = block_on(reference.resolve(&net)).unwrap();
        let used = work.counts().requests - before;
        eprintln!(
            "doi {doi}: {} files, {used} requests, {:?}",
            listing.files.len(),
            start.elapsed()
        );
        assert_ne!(listing.files, Vec::<Remote>::new());
        assert!(listing.resolved.to_string().starts_with(target));
        assert_eq!(used, requests);
    }
}
