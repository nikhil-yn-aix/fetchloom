use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::{Md5, Sha256};
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

const ID: &str = "dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP";
const DATASET: &str = "https://dataverse.harvard.edu/api/datasets/:persistentId/?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";
const LATEST: &str = "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/:latest-published?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";
const VERSION: &str = "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/5.0?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("dataverse/version.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn lists_original_bytes_and_nested_paths_in_one_request() {
    let recorded = Recorded::default().fixture(LATEST, "dataverse/version.json");
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0"
    );
    assert_eq!(listing.title.as_deref(), Some("Open Source at Harvard"));
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
    assert_eq!(recorded.asked(), [LATEST]);
}

#[test]
fn resolving_the_result_asks_for_the_published_version() {
    let recorded = Recorded::default()
        .fixture(LATEST, "dataverse/version.json")
        .fixture(VERSION, "dataverse/version.json");
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, listing.files);
    assert_eq!(pinned.resolved, listing.resolved);
    assert_eq!(pinned.doi, listing.doi);
    assert_eq!(recorded.asked(), [LATEST, VERSION]);
}

#[test]
fn a_curators_draft_does_not_hide_the_published_version() {
    // with a key, the dataset endpoint answers with the newest version that key can see, which
    // for a curator is the open draft
    let mut draft: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("dataverse/dataset.json")).unwrap();
    draft["data"]["latestVersion"]["versionState"] = "DRAFT".into();
    let recorded = Recorded::default()
        .on(DATASET, 200, serde_json::to_vec(&draft).unwrap())
        .fixture(LATEST, "dataverse/version.json");
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0"
    );
    assert_eq!(recorded.asked(), [LATEST]);
}

#[test]
fn restricted_files_are_all_named_instead_of_skipped() {
    let body = changed(|value| {
        value["data"]["files"][0]["restricted"] = true.into();
        value["data"]["files"][1]["restricted"] = true.into();
    });
    let recorded = Recorded::default().on(LATEST, 200, body.clone());
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("data/2026-06-24.tsv"));
    assert!(error.message.contains("code/language.py"));
    assert!(error.help.is_some());
    // a key for this install may be allowed the files, and its downloads then say if not
    let answer: version::Answer = serde_json::from_slice(&body).unwrap();
    let signed_in = |name: &str| match name {
        "DATAVERSE_URL" => Some("https://dataverse.harvard.edu".into()),
        "DATAVERSE_API_TOKEN" => Some("0123-key".into()),
        _ => None,
    };
    let listing = answer
        .listing(&Dataset::parse(ID).unwrap(), &signed_in)
        .unwrap();
    assert_eq!(listing.files.len(), 3);
}

#[test]
fn missing_original_sizes_cannot_be_replaced_with_ingested_sizes() {
    let body = changed(|value| {
        value["data"]["files"][0]["dataFile"]
            .as_object_mut()
            .unwrap()
            .remove("originalFileSize");
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn sha256_checksums_are_kept() {
    let hash = "2450eb9ff3db92a1bff370368b0552b270bd4b5ca0745b773c37d2662f94df8e";
    let body = changed(|value| {
        value["data"]["files"][1]["dataFile"]["checksum"] =
            serde_json::json!({"type": "SHA-256", "value": hash});
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files[1].sha256, Sha256::from_hex(hash));
    assert_eq!(listing.files[1].md5, None);
}

#[test]
fn unknown_checksum_algorithms_are_not_treated_as_md5() {
    let body = changed(|value| {
        value["data"]["files"][1]["dataFile"]["checksum"]["type"] = "SHA-1".into();
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let listing = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files[1].sha256, None);
    assert_eq!(listing.files[1].md5, None);
}

#[test]
fn malformed_known_checksums_are_refused() {
    let body = changed(|value| {
        value["data"]["files"][1]["dataFile"]["checksum"]["value"] = "broken".into();
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn drafts_cannot_be_pinned_as_published_versions() {
    let body = changed(|value| value["data"]["versionState"] = "DRAFT".into());
    let recorded = Recorded::default().on(LATEST, 200, body);
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
        value["data"]["datasetPersistentId"] = "doi:10.7910/DVN/OTHER".into();
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn missing_datasets_are_unresolved() {
    let url = "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/:latest-published?persistentId=doi%3A10.7910%2FDVN%2FFETCHLOOM-MISSING-999";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("dataverse/missing.json"));
    let dataset =
        Dataset::parse("dataverse.harvard.edu/doi:10.7910/DVN/FETCHLOOM-MISSING-999").unwrap();
    let error = block_on(dataset.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn directory_labels_reach_the_shared_path_check() {
    let body = changed(|value| {
        value["data"]["files"][1]["directoryLabel"] = "../code".into();
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
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
            exp: crate::exp::Experiments::default(),
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
        value["data"].as_object_mut().unwrap().remove("files");
    });
    let recorded = Recorded::default().on(LATEST, 200, body);
    let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn access_errors_do_not_suggest_unscoped_credentials() {
    for status in [401, 403] {
        let recorded =
            Recorded::default().on(LATEST, status, fixture_bytes("dataverse/refused.json"));
        let error = block_on(Dataset::parse(ID).unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(
            error
                .help
                .unwrap()
                .contains("`fetchloom auth login dataverse`")
        );
    }
}

/// a key is saved under the install's port when that is not 443, so a download from that port
/// finds it.
#[test]
fn an_install_on_another_port_keeps_its_port_in_the_account() {
    let account = |text: &str| Dataset::parse(text).unwrap().account();
    for (text, host) in [
        ("dv.lab:8443/doi:10.1/X", "dv.lab:8443"),
        ("dv.lab:443/doi:10.1/X", "dv.lab"),
        ("DV.lab/file:1", "dv.lab"),
        ("[::1]:8443/doi:10.1/X", "[::1]:8443"),
    ] {
        assert_eq!(
            account(text),
            crate::auth::Account::Dataverse(host.to_owned()),
            "{text}"
        );
    }
}

#[test]
fn pasted_versions_are_not_discarded() {
    for page in [
        "dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP",
        "citation?persistentId=doi:10.7910/DVN/TJCLKP",
        "file.xhtml?fileId=6867336",
        "file.xhtml?persistentId=doi:10.7910/DVN/27245/DMEVUB",
    ] {
        for version in ["release-1", "", ":draft", ":DRAFT", "DRAFT"] {
            let url = format!("https://dataverse.harvard.edu/{page}&version={version}");
            let error = Reference::parse(&url).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{url}");
            assert!(error.help.is_some(), "{url}");
        }
    }
}

#[test]
fn pasted_published_versions_keep_their_one_request_pin() {
    for (page, endpoint, fixture, pinned) in [
        (
            "dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP&version=5.0",
            VERSION,
            "dataverse/version.json",
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0",
        ),
        (
            "file.xhtml?fileId=6867336&version=5.0",
            "https://dataverse.harvard.edu/api/files/6867336/versions/5.0?returnDatasetVersion=true",
            "dataverse/file.json",
            "dataverse:dataverse.harvard.edu/file:6867336@5.0",
        ),
        (
            "dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP&version=:latest-published",
            LATEST,
            "dataverse/version.json",
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@5.0",
        ),
    ] {
        let recorded = Recorded::default().fixture(endpoint, fixture);
        let reference = Reference::parse(&format!("https://dataverse.harvard.edu/{page}"))
            .unwrap()
            .reference;
        assert_eq!(
            Reference::parse(&reference.to_string()).unwrap().reference,
            reference
        );
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), pinned);
        assert_eq!(recorded.asked(), [endpoint]);
    }
    let doi = Reference::parse(
        "https://doi.org/10.1234/dataset.xhtml?persistentId=doi:10.1/X&version=bad",
    )
    .unwrap();
    assert_eq!(doi.reference.to_string(), "doi:10.1234/dataset.xhtml");
}
