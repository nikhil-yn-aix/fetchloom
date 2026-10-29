use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::{Md5, Sha256};
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

const ID: &str = "dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP";
const DATASET: &str = "https://dataverse.harvard.edu/api/datasets/:persistentId/?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";
const VERSION: &str = "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/5.0?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("dataverse/dataset.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn lists_original_bytes_and_nested_paths_in_one_request() {
    let recorded = Recorded::default().fixture(DATASET, "dataverse/dataset.json");
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0"
    );
    assert_eq!(listing.title.as_deref(), Some("Open Source at Harvard"));
    assert_eq!(listing.version.as_deref(), Some("5.0"));
    assert_eq!(listing.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.7910/DVN/TJCLKP"));
    assert_eq!(
        listing.files,
        vec![
            Remote {
                path: "data/2026-06-24.tsv".to_owned(),
                size: Some(25_314),
                sha256: None,
                md5: Md5::from_hex("0165275ec1437dd2bcb1c6b4b56ef233"),
                url: Url::parse(
                    "https://dataverse.harvard.edu/api/access/datafile/14027661?format=original"
                )
                .unwrap(),
                named_by_server: false,
            },
            Remote {
                path: "code/language.py".to_owned(),
                size: Some(590),
                sha256: None,
                md5: Md5::from_hex("9d61582bcf497c83bbd1ed0eed3c772e"),
                url: Url::parse(
                    "https://dataverse.harvard.edu/api/access/datafile/6867336?format=original"
                )
                .unwrap(),
                named_by_server: false,
            },
            Remote {
                path: "data/primary/primary-data.zip".to_owned(),
                size: Some(276_577),
                sha256: None,
                md5: Md5::from_hex("f24541c5f89a09bc6db2781220cbbe70"),
                url: Url::parse(
                    "https://dataverse.harvard.edu/api/access/datafile/14027662?format=original"
                )
                .unwrap(),
                named_by_server: false,
            },
        ]
    );
    assert_eq!(recorded.asked(), [DATASET]);
}

#[test]
fn resolving_the_result_asks_for_the_published_version() {
    let recorded = Recorded::default()
        .fixture(DATASET, "dataverse/dataset.json")
        .fixture(VERSION, "dataverse/version.json");
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, listing.files);
    assert_eq!(pinned.resolved, listing.resolved);
    assert_eq!(pinned.doi, listing.doi);
    assert_eq!(recorded.asked(), [DATASET, VERSION]);
}

#[test]
fn restricted_files_are_all_named_instead_of_skipped() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][0]["restricted"] = true.into();
        value["data"]["latestVersion"]["files"][1]["restricted"] = true.into();
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("data/2026-06-24.tsv"));
    assert!(error.message.contains("code/language.py"));
    assert!(error.help.is_some());
}

#[test]
fn missing_original_sizes_cannot_be_replaced_with_ingested_sizes() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][0]["dataFile"]
            .as_object_mut()
            .unwrap()
            .remove("originalFileSize");
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn sha256_checksums_are_kept() {
    let hash = "2450eb9ff3db92a1bff370368b0552b270bd4b5ca0745b773c37d2662f94df8e";
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][1]["dataFile"]["checksum"] =
            serde_json::json!({"type": "SHA-256", "value": hash});
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files[1].sha256, Sha256::from_hex(hash));
    assert_eq!(listing.files[1].md5, None);
}

#[test]
fn unknown_checksum_algorithms_are_not_treated_as_md5() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][1]["dataFile"]["checksum"]["type"] = "SHA-1".into();
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files[1].sha256, None);
    assert_eq!(listing.files[1].md5, None);
}

#[test]
fn malformed_known_checksums_are_refused() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][1]["dataFile"]["checksum"]["value"] =
            "broken".into();
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn drafts_cannot_be_pinned_as_published_versions() {
    let body = changed(|value| value["data"]["latestVersion"]["versionState"] = "DRAFT".into());
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.help.is_some());
}

#[test]
fn requested_versions_cannot_silently_change() {
    let url = "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/4.0?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";
    let recorded = Recorded::default().fixture(url, "dataverse/version.json");
    let error = block_on(
        Dataset::parse(&format!("{ID}@4.0"))
            .unwrap()
            .resolve(&recorded),
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn a_response_for_another_dataset_is_refused() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["datasetPersistentId"] = "doi:10.7910/DVN/OTHER".into();
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn missing_datasets_are_unresolved() {
    let url = "https://dataverse.harvard.edu/api/datasets/:persistentId/?persistentId=doi%3A10.7910%2FDVN%2FFETCHLOOM-MISSING-999";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("dataverse/missing.json"));
    let dataset =
        Dataset::parse("dataverse.harvard.edu/doi:10.7910/DVN/FETCHLOOM-MISSING-999").unwrap();
    let error = block_on(dataset.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn access_refusals_require_authorization() {
    for status in [401, 403] {
        let recorded =
            Recorded::default().on(DATASET, status, fixture_bytes("dataverse/refused.json"));
        let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
    }
}

#[test]
fn directory_labels_reach_the_shared_path_check() {
    let body = changed(|value| {
        value["data"]["latestVersion"]["files"][1]["directoryLabel"] = "../code".into();
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let reference = Reference::parse(&format!("dataverse:{ID}"))
        .unwrap()
        .reference;
    let error = block_on(reference.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn invalid_unicode_pids_do_not_panic() {
    assert_eq!(
        Dataset::parse("example.org/é漢/id").unwrap_err().kind,
        Kind::ReferenceInvalid
    );
}

#[test]
fn hosts_cannot_carry_login_details() {
    let error = Dataset::parse("name:secret@example.org/doi:10.7910/DVN/TJCLKP").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceInvalid);
    assert!(!error.message.contains("secret"));
}

#[test]
#[ignore = "live: talks to dataverse.harvard.edu"]
fn live_dataverse() {
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
    )
    .unwrap();
    let start = Instant::now();
    let reference = Reference::parse("dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "dataverse: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 3);
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0"
    );
    assert_eq!(work.counts().requests, 1);
}

#[test]
fn a_dataset_without_file_metadata_is_not_empty() {
    let body = changed(|value| {
        value["data"]["latestVersion"]
            .as_object_mut()
            .unwrap()
            .remove("files");
    });
    let recorded = Recorded::default().on(DATASET, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn access_errors_do_not_suggest_unscoped_credentials() {
    for status in [401, 403] {
        let recorded =
            Recorded::default().on(DATASET, status, fixture_bytes("dataverse/refused.json"));
        let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.help.unwrap().contains("public copy"));
    }
}
