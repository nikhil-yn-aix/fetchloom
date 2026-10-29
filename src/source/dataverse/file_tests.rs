use super::*;
use crate::digest::Md5;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

/// the files API's answer for one file, at the newest published version of its dataset.
const FILE: &str = "https://dataverse.harvard.edu/api/files/6867336/versions/:latest-published?returnDatasetVersion=true";

fn changed_file(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("dataverse/file.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn file_pages_name_one_file() {
    let cases = [
        (
            "https://dataverse.harvard.edu/file.xhtml?fileId=6867336",
            "dataverse:dataverse.harvard.edu/file:6867336",
        ),
        (
            "https://dataverse.harvard.edu/file.xhtml?fileId=6867336&version=5.0",
            "dataverse:dataverse.harvard.edu/file:6867336@5.0",
        ),
        (
            "https://dataverse.harvard.edu/file.xhtml?persistentId=doi:10.7910/DVN/27245/DMEVUB",
            "dataverse:dataverse.harvard.edu/file:doi:10.7910/DVN/27245/DMEVUB",
        ),
    ];
    for (link, canonical) in cases {
        let parsed = Reference::parse(link).unwrap();
        assert_eq!(parsed.reference.to_string(), canonical, "{link}");
        assert_eq!(parsed.file, None, "{link}");
        assert_eq!(
            Reference::parse(canonical).unwrap().reference,
            parsed.reference
        );
    }
    for other in [
        "dataverse.harvard.edu/file:",
        "dataverse.harvard.edu/file:abc",
        "dataverse.harvard.edu/file:doi:",
    ] {
        assert_eq!(
            Dataset::parse(other).unwrap_err().kind,
            Kind::ReferenceInvalid,
            "{other}"
        );
    }
}

#[test]
fn a_file_is_listed_as_one_file_of_its_dataset_in_one_request() {
    let recorded = Recorded::default().fixture(FILE, "dataverse/file.json");
    let reference = Reference::parse("https://dataverse.harvard.edu/file.xhtml?fileId=6867336")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/file:6867336@5.0"
    );
    assert_eq!(listing.title.as_deref(), Some("Open Source at Harvard"));
    assert_eq!(listing.version.as_deref(), Some("5.0"));
    assert_eq!(listing.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.7910/DVN/TJCLKP"));
    assert_eq!(
        listing.files,
        [Remote {
            path: "code/language.py".to_owned(),
            size: Some(590),
            sha256: None,
            md5: Md5::from_hex("9d61582bcf497c83bbd1ed0eed3c772e"),
            url: Url::parse(
                "https://dataverse.harvard.edu/api/access/datafile/6867336?format=original"
            )
            .unwrap(),
            named_by_server: false,
        }]
    );
    assert_eq!(recorded.asked(), [FILE]);
}

#[test]
fn the_resolved_file_asks_for_the_same_version() {
    let pinned =
        "https://dataverse.harvard.edu/api/files/6867336/versions/5.0?returnDatasetVersion=true";
    let recorded = Recorded::default()
        .fixture(FILE, "dataverse/file.json")
        .fixture(pinned, "dataverse/file.json");
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
    let listing = block_on(file.resolve(&recorded)).unwrap();
    let again = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(again.resolved, listing.resolved);
    assert_eq!(again.files, listing.files);
    assert_eq!(recorded.asked(), [FILE, pinned]);
}

#[test]
fn a_file_named_by_its_persistent_id_resolves_to_its_id() {
    let url = "https://dataverse.harvard.edu/api/files/:persistentId/versions/:latest-published?persistentId=doi%3A10.7910%2FDVN%2F27245%2FDMEVUB&returnDatasetVersion=true";
    let recorded = Recorded::default().fixture(url, "dataverse/file-pid.json");
    let file = Dataset::parse("dataverse.harvard.edu/file:doi:10.7910/DVN/27245/DMEVUB").unwrap();
    let listing = block_on(file.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/file:2498612@1.0"
    );
    assert_eq!(listing.doi.as_deref(), Some("10.7910/DVN/27245"));
    assert_eq!(listing.files.len(), 1);
    assert_eq!(listing.files[0].path, "taa_rep_materials.zip");
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn an_answer_about_another_file_is_refused() {
    let body = changed_file(|value| value["data"]["dataFile"]["id"] = 1.into());
    let recorded = Recorded::default().on(FILE, 200, body);
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
    let error = block_on(file.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn a_restricted_file_is_refused_by_name() {
    let body = changed_file(|value| value["data"]["restricted"] = true.into());
    let recorded = Recorded::default().on(FILE, 200, body);
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
    let error = block_on(file.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(
        error.message.contains("code/language.py"),
        "{}",
        error.message
    );
}

#[test]
fn a_file_missing_from_the_version_asked_for_is_unresolved() {
    let url =
        "https://dataverse.harvard.edu/api/files/6867336/versions/1.0?returnDatasetVersion=true";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("dataverse/file-missing.json"));
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336@1.0").unwrap();
    let error = block_on(file.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
}

#[test]
fn a_file_version_cannot_change_or_be_a_draft() {
    let url =
        "https://dataverse.harvard.edu/api/files/6867336/versions/4.0?returnDatasetVersion=true";
    let recorded = Recorded::default().fixture(url, "dataverse/file.json");
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336@4.0").unwrap();
    assert_eq!(
        block_on(file.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );

    let body =
        changed_file(|value| value["data"]["datasetVersion"]["versionState"] = "DRAFT".into());
    let recorded = Recorded::default().on(FILE, 200, body);
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
    assert_eq!(
        block_on(file.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn a_file_path_cannot_escape_the_dataset() {
    let body = changed_file(|value| value["data"]["directoryLabel"] = "../code".into());
    let recorded = Recorded::default().on(FILE, 200, body);
    let reference = Reference::parse("dataverse:dataverse.harvard.edu/file:6867336")
        .unwrap()
        .reference;
    assert_eq!(
        block_on(reference.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn a_file_pid_answer_must_name_the_requested_file() {
    let url = "https://dataverse.harvard.edu/api/files/:persistentId/versions/1.0?persistentId=doi%3A10.7910%2FDVN%2F27245%2FOTHER&returnDatasetVersion=true";
    let recorded = Recorded::default().fixture(url, "dataverse/file-pid.json");
    let file =
        Dataset::parse("dataverse.harvard.edu/file:doi:10.7910/DVN/27245/OTHER@1.0").unwrap();
    assert_eq!(
        block_on(file.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn a_handle_can_name_a_file() {
    let url = "https://dataverse.harvard.edu/api/files/:persistentId/versions/:latest-published?persistentId=hdl%3A1902.1%2F123%2Ffile&returnDatasetVersion=true";
    let body = changed_file(|value| {
        value["data"]["dataFile"]["persistentId"] = "hdl:1902.1/123/file".into();
    });
    let recorded = Recorded::default().on(url, 200, body);
    let file = Dataset::parse("dataverse.harvard.edu/file:hdl:1902.1/123/file").unwrap();
    let listing = block_on(file.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "dataverse:dataverse.harvard.edu/file:6867336@5.0"
    );
}

#[test]
fn a_numeric_file_id_is_checked_against_the_database_id() {
    let body = changed_file(|value| {
        value["data"]["dataFile"]["id"] = 1.into();
        value["data"]["dataFile"]["persistentId"] = "6867336".into();
    });
    let recorded = Recorded::default().on(FILE, 200, body);
    let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
    assert_eq!(
        block_on(file.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn a_restricted_file_answered_with_success_is_still_refused() {
    let url = "https://dataverse.harvard.edu/api/files/2437082/versions/:latest-published?returnDatasetVersion=true";
    let recorded = Recorded::default().fixture(url, "dataverse/file-restricted.json");
    let file = Dataset::parse("dataverse.harvard.edu/file:2437082").unwrap();
    let error = block_on(file.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(
        error
            .message
            .contains("BF-Rapport MAP Prudence R 1juin 2010.pdf")
    );
    assert!(error.help.unwrap().contains("dataset's owner"));
}

#[test]
fn file_access_errors_do_not_suggest_unscoped_credentials() {
    for status in [401, 403] {
        let recorded =
            Recorded::default().on(FILE, status, fixture_bytes("dataverse/refused.json"));
        let file = Dataset::parse("dataverse.harvard.edu/file:6867336").unwrap();
        let error = block_on(file.resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.help.unwrap().contains("public copy"));
    }
}
