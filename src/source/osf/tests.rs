use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::{Md5, Sha256};
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

const NODE: &str = "https://api.osf.io/v2/nodes/ezcuj/?embed=license&embed=identifiers";
const FILES: &str = "https://api.osf.io/v2/nodes/ezcuj/files/osfstorage/?page%5Bsize%5D=100";

fn expected_file() -> Remote {
    Remote {
        path: "OSC2012.pdf".to_owned(),
        size: Some(216_945),
        sha256: Sha256::from_hex(
            "2450eb9ff3db92a1bff370368b0552b270bd4b5ca0745b773c37d2662f94df8e",
        ),
        md5: Md5::from_hex("44325d4f13b09f3769ede09d7c20a82c"),
        url: Url::parse("https://osf.io/download/sejcv/?version=1").unwrap(),
        named_by_server: false,
    }
}

fn changed_root(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("osf/root.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn with_root(body: Vec<u8>) -> Recorded {
    Recorded::default()
        .fixture(NODE, "osf/embedded.json")
        .on(FILES, 200, body)
}

#[test]
fn lists_every_field_with_metadata_embedded_in_one_request() {
    let recorded = Recorded::default()
        .fixture(NODE, "osf/embedded.json")
        .fixture(FILES, "osf/root.json");
    let listing = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "osf:ezcuj");
    assert_eq!(
        listing.title.as_deref(),
        Some("Reproducibility Project: Psychology")
    );
    assert_eq!(listing.license.as_deref(), Some("CC0 1.0 Universal"));
    assert_eq!(listing.doi.as_deref(), Some("10.17605/OSF.IO/EZCUJ"));
    assert_eq!(listing.files, [expected_file()]);
    assert_eq!(recorded.asked(), [NODE, FILES]);
}

#[test]
fn resolving_the_result_keeps_each_files_stated_version() {
    let recorded = Recorded::default()
        .fixture(NODE, "osf/embedded.json")
        .fixture(FILES, "osf/root.json");
    let listing = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap();
    let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, listing.files);
    assert_eq!(pinned.resolved, listing.resolved);
    assert_eq!(recorded.asked(), [NODE, FILES, NODE, FILES]);
}

#[test]
fn file_guids_resolve_to_one_versioned_file() {
    let node = "https://api.osf.io/v2/nodes/sejcv/?embed=license&embed=identifiers";
    let guid = "https://api.osf.io/v2/guids/sejcv/";
    let recorded = Recorded::default()
        .on(node, 404, fixture_bytes("osf/file-as-node.json"))
        .fixture(guid, "osf/guid.json");
    let listing = block_on(Node::parse("sejcv").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "osf:sejcv");
    assert_eq!(listing.title.as_deref(), Some("OSC2012.pdf"));
    assert_eq!(listing.license, None);
    assert_eq!(listing.doi, None);
    assert_eq!(listing.files, [expected_file()]);
    assert_eq!(recorded.asked(), [node, guid]);
}

#[test]
fn download_links_name_the_file_guid() {
    let parsed = Reference::parse("https://osf.io/download/sejcv/").unwrap();
    assert_eq!(parsed.reference.to_string(), "osf:sejcv");
}

#[test]
fn missing_projects_and_guids_are_unresolved() {
    let node = "https://api.osf.io/v2/nodes/zzzzzzzzzzzz/?embed=license&embed=identifiers";
    let guid = "https://api.osf.io/v2/guids/zzzzzzzzzzzz/";
    let recorded = Recorded::default()
        .on(node, 404, fixture_bytes("osf/missing.json"))
        .on(guid, 404, fixture_bytes("osf/missing-guid.json"));
    let error = block_on(Node::parse("zzzzzzzzzzzz").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [node, guid]);
}

#[test]
fn access_refusals_do_not_trigger_guid_probes() {
    for status in [401, 403] {
        let recorded = Recorded::default().on(NODE, status, fixture_bytes("osf/refused.json"));
        let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(recorded.asked(), [NODE]);
    }
}

#[test]
fn refused_file_lists_require_authorization() {
    let recorded = Recorded::default().fixture(NODE, "osf/embedded.json").on(
        FILES,
        403,
        fixture_bytes("osf/refused.json"),
    );
    let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(recorded.asked(), [NODE, FILES]);
}

#[test]
fn download_links_pin_the_current_version_instead_of_an_old_query() {
    let body = changed_root(|value| {
        value["data"][0]["attributes"]["current_version"] = 2.into();
        value["data"][0]["links"]["download"] =
            "https://osf.io/sejcv/?action=download&version=99".into();
    });
    let recorded = with_root(body);
    let listing = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://osf.io/sejcv/?action=download&version=2"
    );
}

#[test]
fn unversioned_files_are_refused() {
    let body = changed_root(|value| {
        value["data"][0]["attributes"]
            .as_object_mut()
            .unwrap()
            .remove("current_version");
    });
    let recorded = with_root(body);
    let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.help.is_some());
}

#[test]
fn malformed_stated_hashes_are_refused() {
    let body = changed_root(|value| {
        value["data"][0]["attributes"]["extra"]["hashes"]["sha256"] = "broken".into();
    });
    let recorded = with_root(body);
    let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn absent_hashes_do_not_invent_digests() {
    let body = changed_root(|value| {
        value["data"][0]["attributes"]["extra"]
            .as_object_mut()
            .unwrap()
            .remove("hashes");
    });
    let recorded = with_root(body);
    let listing = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files[0].sha256, None);
    assert_eq!(listing.files[0].md5, None);
}

#[test]
fn external_storage_versions_are_not_treated_as_osf_versions() {
    let body = changed_root(|value| value["data"][0]["attributes"]["provider"] = "dropbox".into());
    let recorded = with_root(body);
    let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn signed_download_urls_cannot_enter_the_lock() {
    let body = changed_root(|value| {
        value["data"][0]["links"]["download"] =
            "https://osf.io/download/sejcv/?token=secret".into();
    });
    let recorded = with_root(body);
    let error = block_on(Node::parse("ezcuj").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(!error.message.contains("secret"));
}

#[test]
#[ignore = "live: talks to api.osf.io"]
fn live_osf() {
    let work = Arc::new(crate::work::Work::default());
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: false,
            jobs: 4,
            credentials: Vec::new(),
        },
        work.clone(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    let start = Instant::now();
    let reference = Reference::parse("osf:ezcuj").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "osf: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files, [expected_file()]);
    assert_eq!(listing.resolved.to_string(), "osf:ezcuj");
    assert_eq!(work.counts().requests, 2);
}

#[test]
fn recorded_storage_expiry_is_distinct_from_an_auth_refusal() {
    let url = Url::parse("https://osf.io/download/tfa5d/?version=1").unwrap();
    let recorded =
        Recorded::default().on(url.as_str(), 403, fixture_bytes("osf/expired-storage.xml"));
    let answer = block_on(recorded.get(&url)).unwrap();
    let check = crate::source::fetch_plan(&url).expired.unwrap();
    assert!(check(&answer.text()));
    let denied = answer
        .text()
        .replace("Request has expired.", "The request is not authorized.");
    assert!(!check(&denied));
}
