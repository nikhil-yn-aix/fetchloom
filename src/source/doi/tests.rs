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
fn all_four_real_handle_urls_route_without_getting_the_landing_page() {
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
    ];
    for (doi, url, fixture, target) in cases {
        let recorded = Recorded::default().fixture(url, fixture);
        let reference = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap();
        assert_eq!(reference.to_string(), target);
        assert_eq!(recorded.asked(), [url]);
    }
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

#[test]
fn the_landing_url_chooses_the_source_instead_of_the_registrant() {
    let doi = "10.1234/another-registrant";
    let url = "https://doi.org/api/handles/10.1234%2Fanother-registrant";
    let body = changed(|value| value["handle"] = doi.into());
    let recorded = Recorded::default().on(url, 200, body);
    let target = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap();
    assert_eq!(target.to_string(), "zenodo:3608990");
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn doi_punctuation_is_encoded_as_identifier_text() {
    let doi = "10.1234/a?b#c%d/e";
    let url = "https://doi.org/api/handles/10.1234%2Fa%3Fb%23c%25d%2Fe";
    let body = changed(|value| value["handle"] = doi.into());
    let recorded = Recorded::default().on(url, 200, body);
    let target = block_on(Doi::parse(doi).unwrap().locate(&recorded)).unwrap();
    assert_eq!(target.to_string(), "zenodo:3608990");
    assert_eq!(recorded.asked(), [url]);
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

#[test]
fn handles_without_url_values_are_invalid() {
    let body = changed(|value| value["values"] = serde_json::json!([]));
    let recorded = Recorded::default().on(HANDLE, 200, body);
    let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.help.is_some());
}

#[test]
fn unexpected_handle_codes_are_invalid() {
    let body = changed(|value| value["responseCode"] = 2.into());
    let recorded = Recorded::default().on(HANDLE, 200, body);
    let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn handles_for_another_doi_are_invalid() {
    let body = changed(|value| value["handle"] = "10.1234/another-doi".into());
    let recorded = Recorded::default().on(HANDLE, 200, body);
    let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn url_values_must_be_strings_in_string_format() {
    let body = changed(|value| value["values"][1]["data"]["format"] = "base64".into());
    let recorded = Recorded::default().on(HANDLE, 200, body);
    let error = block_on(Doi::parse(DOI).unwrap().locate(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
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
            split: true,
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
