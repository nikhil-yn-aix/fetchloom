use std::time::Instant;

use super::*;
use crate::digest::Md5;
use crate::error::Kind;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};
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
    let listing = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
    assert_eq!(listing.title.as_deref(), Some("DatasetIris"));
    assert_eq!(listing.license.as_deref(), Some("cc-by-4.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.5281/zenodo.3608990"));
    assert_eq!(listing.access, Access::Public);
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
            etag: None,
            blob: None,
        }]
    );
    assert_eq!(recorded.asked(), [IRIS]);
}

#[test]
fn large_records_need_no_separate_file_requests() {
    let url = "https://zenodo.org/api/records/7793716";
    let recorded = Recorded::default().fixture(url, "zenodo/many.json");
    let listing = block_on(Record::zenodo("7793716").unwrap().resolve(&recorded)).unwrap();
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
    let listing = block_on(Record::zenodo("3608989").unwrap().resolve(&recorded)).unwrap();
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
    let listing = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)).unwrap();
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
    let error = block_on(Record::zenodo("999999999999").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn access_refusals_require_authorization() {
    for status in [401, 403] {
        let recorded = Recorded::default().on(IRIS, status, fixture_bytes("zenodo/refused.json"));
        let error = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.help.is_some());
    }
}

#[test]
fn restricted_records_do_not_look_empty() {
    let url = "https://zenodo.org/api/records/14110909";
    let recorded = Recorded::default().fixture(url, "zenodo/restricted.json");
    let error = block_on(Record::zenodo("14110909").unwrap().resolve(&recorded)).unwrap_err();
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
    let error = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("embargoed"));
    let need = error.need.unwrap();
    assert_eq!(need.account, crate::account::Account::Zenodo);
    assert_eq!(
        need.terms.as_deref(),
        Some("https://zenodo.org/records/3608990")
    );
}

/// zenodo lists the files of a record it holds back only to an account allowed to read
/// them, so the record needs a login.
#[test]
fn a_held_back_record_listed_for_this_account_downloads_and_needs_a_login() {
    for right in ["restricted", "embargoed", "closed"] {
        let body = changed(|value| value["metadata"]["access_right"] = right.into());
        let recorded = Recorded::default().on(IRIS, 200, body);
        let listing = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)).unwrap();
        assert_eq!(listing.files.len(), 1, "{right}");
        assert_eq!(listing.access, Access::Login, "{right}");
    }
}

/// the help goes to fetchloom's issues, since the record's owner cannot change zenodo's answer.
#[test]
fn a_record_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 4] = [
        (
            "a checksum that is no MD5",
            |value| value["files"][0]["checksum"] = "md5:broken".into(),
            "is not an MD5",
        ),
        (
            "no file list",
            |value| {
                value.as_object_mut().unwrap().remove("files");
            },
            "missing field `files`",
        ),
        (
            "an unknown access condition",
            |value| value["metadata"]["access_right"] = "unknown".into(),
            "the access condition is not recognized",
        ),
        (
            "a zero record id",
            |value| value["id"] = 0.into(),
            "the record has no concrete id",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = Recorded::default().on(IRIS, 200, changed(change));
        let Err(error) = block_on(Record::zenodo("3608990").unwrap().resolve(&recorded)) else {
            panic!("{row}: the record was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains(crate::error::ISSUES)),
            "{row}: {:?}",
            error.help
        );
    }
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
#[ignore = "live: talks to zenodo.org"]
fn live_zenodo() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("zenodo:3608990").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "zenodo: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 1);
    assert_eq!(listing.resolved.to_string(), "zenodo:3608990");
    assert_eq!(asked(&work), 1);
}

/// zenodo states CC0 as `cc-zero` in its own form, where invenioRDM's says `cc0-1.0`: a zenodo
/// record keeps the license data.lock has always recorded for it.
#[test]
fn a_zenodo_record_keeps_the_license_id_zenodo_states() {
    let url = "https://zenodo.org/api/records/18149369";
    let recorded = Recorded::default().fixture(url, "zenodo/cc0.json");
    let listing = block_on(Record::zenodo("18149369").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.license.as_deref(), Some("cc-zero"));
    assert_eq!(listing.doi.as_deref(), Some("10.5281/zenodo.18149369"));
    assert_eq!(recorded.asked(), [url]);
}

/// a DOI zenodo registered since it moved to invenioRDM leads to zenodo's `/doi/` page, which
/// names the record after `zenodo.`, so it resolves with no request to that page.
#[test]
fn a_doi_that_leads_to_zenodos_doi_page_names_the_record() {
    let handle = "https://doi.org/api/handles/10.5281%2Fzenodo.18149369";
    let url = "https://zenodo.org/api/records/18149369";
    let recorded = Recorded::default()
        .fixture(handle, "doi/zenodo-new.json")
        .fixture(url, "zenodo/cc0.json");
    let reference = Reference::parse("doi:10.5281/zenodo.18149369")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "zenodo:18149369");
    assert_eq!(recorded.asked(), [handle, url]);
}

const CALTECH: &str = "https://data.caltech.edu/api/records/dt797-nsy79";

/// a record on another invenioRDM install pins with the size and MD5 the install states, each
/// file at its link in that install's api.
#[test]
fn a_record_on_another_install_pins_with_the_md5_it_states() {
    let recorded = Recorded::default().fixture(CALTECH, "invenio/caltech-record.json");
    let reference = Reference::parse("invenio:data.caltech.edu/dt797-nsy79")
        .unwrap()
        .reference;
    assert_eq!(
        reference.written_origin(),
        Some(Url::parse("https://data.caltech.edu/").unwrap().origin())
    );
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "invenio:data.caltech.edu/dt797-nsy79"
    );
    assert_eq!(listing.access, Access::Public);
    assert_eq!(listing.license.as_deref(), Some("cc-by-4.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.22002/dt797-nsy79"));
    assert!(
        listing
            .title
            .is_some_and(|title| title.starts_with("Software: "))
    );
    assert_eq!(
        listing.files,
        [Remote {
            path: "Micropublication_2026-1.0.0.zip".to_owned(),
            size: Some(4_165_550),
            sha256: None,
            md5: Md5::from_hex("32091236f73a22a5ed5f7aa4f01e1138"),
            url: Url::parse(&format!(
                "{CALTECH}/files/Micropublication_2026-1.0.0.zip/content"
            ))
            .unwrap(),
            named_by_server: false,
            etag: None,
            blob: None,
        }]
    );
    assert_eq!(recorded.asked(), [CALTECH]);
}

/// another install's record is named by its host and id; zenodo's keep their own spelling, and a
/// page on another host is a plain link until its install answers for it.
#[test]
fn references_name_a_record_on_zenodo_or_another_install() {
    for (text, expected) in [
        (
            "invenio:data.caltech.edu/dt797-nsy79",
            "invenio:data.caltech.edu/dt797-nsy79",
        ),
        (
            "invenio:Data.Caltech.EDU/dt797-nsy79",
            "invenio:data.caltech.edu/dt797-nsy79",
        ),
        (
            "invenio:rdm.example.org:8443/abcde-12345",
            "invenio:rdm.example.org:8443/abcde-12345",
        ),
        ("invenio:zenodo.org/3608990", "zenodo:3608990"),
        (
            "https://zenodo.org/doi/10.5281/zenodo.18149369",
            "zenodo:18149369",
        ),
        ("zenodo:3608990", "zenodo:3608990"),
    ] {
        let reference = Reference::parse(text).unwrap().reference;
        assert_eq!(reference.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, reference);
    }
    for text in [
        "invenio:",
        "invenio:data.caltech.edu",
        "invenio:data.caltech.edu/",
        "invenio:data.caltech.edu/dt797/nsy79",
        "invenio:ada@data.caltech.edu/dt797-nsy79",
        "invenio:data.caltech.edu/-dt797",
        "invenio:zenodo.org/dt797-nsy79",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let page = "https://data.caltech.edu/records/dt797-nsy79";
    assert_eq!(
        Reference::parse(page).unwrap().reference,
        Reference::Url(Url::parse(page).unwrap())
    );
}

/// a record's page, or a DOI's page the install registered, on another host is known as the
/// record by exactly one request for it at that origin, which lists it too.
#[test]
fn a_record_page_on_another_install_is_known_by_one_request() {
    for page in [
        "https://data.caltech.edu/records/dt797-nsy79",
        "https://data.caltech.edu/records/dt797-nsy79/",
        "https://data.caltech.edu/doi/10.22002/dt797-nsy79",
    ] {
        let recorded = Recorded::default().fixture(CALTECH, "invenio/caltech-record.json");
        let reference = Reference::parse(page).unwrap().reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(
            listing.resolved.to_string(),
            "invenio:data.caltech.edu/dt797-nsy79",
            "{page}"
        );
        assert_eq!(listing.files.len(), 1, "{page}");
        assert_eq!(recorded.asked(), [CALTECH], "{page}");
    }
}

/// a page whose host answers for no such record, or for another one, stays the plain link it
/// was; a link of another form asks nothing.
#[test]
fn a_page_no_install_answers_for_stays_a_plain_link() {
    let page = "https://example.org/records/report.csv";
    let probe = "https://example.org/api/records/report.csv";
    let another = |record: &str| {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("invenio/caltech-record.json")).unwrap();
        value["id"] = record.into();
        serde_json::to_vec(&value).unwrap()
    };
    for (status, body) in [
        (404, fixture_bytes("invenio/missing.json")),
        (200, b"<!DOCTYPE html>".to_vec()),
        (200, another("other-12345")),
    ] {
        let recorded = Recorded::default().on(probe, status, body);
        let reference = Reference::parse(page).unwrap().reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved, reference);
        assert_eq!(listing.files[0].path, "report.csv");
        assert_eq!(recorded.asked(), [probe]);
    }
    let doi = "https://data.caltech.edu/doi/10.22002/dt797-nsy79";
    let other_doi = |value: &mut serde_json::Value| {
        value["pids"]["doi"]["identifier"] = "10.22002/other-12345".into();
    };
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("invenio/caltech-record.json")).unwrap();
    other_doi(&mut value);
    let recorded = Recorded::default().on(CALTECH, 200, serde_json::to_vec(&value).unwrap());
    let reference = Reference::parse(doi).unwrap().reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved, reference, "the record holds another DOI");
    for plain in [
        "https://data.caltech.edu/records/dt797-nsy79/files/a.zip",
        "http://data.caltech.edu/records/dt797-nsy79",
        "https://example.org/data/records.csv",
    ] {
        let recorded = Recorded::default();
        let reference = Reference::parse(plain).unwrap().reference;
        block_on(reference.resolve(&recorded)).unwrap();
        assert!(recorded.asked().is_empty(), "{plain}");
    }
}

const TUWIEN: &str = "https://researchdata.tuwien.ac.at/api/records/hg0hy-kxc20";
const TUWIEN_HANDLE: &str = "https://doi.org/api/handles/10.48436%2Fhg0hy-kxc20";

/// a DOI another install registered leads to its `/doi/` page, so the record is known by one
/// request for it there; one whose install answers for no record stays a page fetchloom cannot
/// list, as any other host's.
#[test]
fn a_doi_of_another_install_resolves_to_its_record() {
    let recorded = Recorded::default()
        .fixture(TUWIEN_HANDLE, "invenio/tuwien-handle.json")
        .fixture(TUWIEN, "invenio/tuwien-record.json");
    let reference = Reference::parse("doi:10.48436/hg0hy-kxc20")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "invenio:researchdata.tuwien.ac.at/hg0hy-kxc20"
    );
    assert_eq!(listing.doi.as_deref(), Some("10.48436/hg0hy-kxc20"));
    assert_eq!(listing.license.as_deref(), Some("gpl-3.0-or-later"));
    assert_eq!(listing.files.len(), 11);
    assert!(
        listing
            .files
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
    assert_eq!(recorded.asked(), [TUWIEN_HANDLE, TUWIEN]);

    let recorded = Recorded::default()
        .fixture(TUWIEN_HANDLE, "invenio/tuwien-handle.json")
        .on(TUWIEN, 404, fixture_bytes("invenio/missing.json"));
    let error = block_on(reference.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "doi:10.48436/hg0hy-kxc20 points to researchdata.tuwien.ac.at, which fetchloom cannot list"
    );
    assert_eq!(recorded.asked(), [TUWIEN_HANDLE, TUWIEN]);
}

/// another install's refusal, or a record whose files it holds back, names the key saved for
/// that host alone, never zenodo's; one reached through a DOI names the DOI too, so a sign-in
/// offered for the host can say what chose it.
#[test]
fn another_installs_refusal_names_its_own_sign_in() {
    let refused = Recorded::default().on(
        CALTECH,
        403,
        br#"{"status": 403, "message": "Permission denied."}"#.to_vec(),
    );
    let reference = Reference::parse("invenio:data.caltech.edu/dt797-nsy79")
        .unwrap()
        .reference;
    let error = block_on(reference.resolve(&refused)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "data.caltech.edu refused access to invenio:data.caltech.edu/dt797-nsy79"
    );
    let help = error.help.clone().unwrap_or_default();
    assert!(
        help.contains("`fetchloom auth login data.caltech.edu --token`"),
        "{help}"
    );
    assert!(!help.contains("zenodo"), "{help}");
    let need = error.need.unwrap();
    assert_eq!(need.account, Account::Login("data.caltech.edu".to_owned()));
    assert!(need.site);

    let restricted = "https://researchdata.tuwien.ac.at/api/records/c3nk3-d6m20";
    let recorded = Recorded::default().fixture(restricted, "invenio/tuwien-restricted.json");
    let reference = Reference::parse("invenio:researchdata.tuwien.ac.at/c3nk3-d6m20")
        .unwrap()
        .reference;
    let error = block_on(reference.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "invenio:researchdata.tuwien.ac.at/c3nk3-d6m20 is restricted"
    );
    assert!(
        error
            .help
            .unwrap_or_default()
            .contains("`fetchloom auth login researchdata.tuwien.ac.at --token`")
    );
    assert!(error.need.is_some_and(|need| need.site));

    let recorded = Recorded::default()
        .fixture(TUWIEN_HANDLE, "invenio/tuwien-handle.json")
        .on(TUWIEN, 401, b"{}".to_vec());
    let reference = Reference::parse("doi:10.48436/hg0hy-kxc20")
        .unwrap()
        .reference;
    let error = block_on(reference.resolve(&recorded)).unwrap_err();
    let need = error.need.unwrap();
    assert_eq!(
        need.account,
        Account::Login("researchdata.tuwien.ac.at".to_owned())
    );
    assert_eq!(need.via.as_deref(), Some("doi:10.48436/hg0hy-kxc20"));
}

/// the help goes to fetchloom's issues, since the record's owner cannot change the install's
/// answer.
#[test]
fn another_installs_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 3] = [
        (
            "another record",
            |value| {
                value["id"] = "other-12345".into();
                value["parent"]["id"] = "other-00000".into();
            },
            "the record or its parent id does not match",
        ),
        (
            "a checksum that is no MD5",
            |value| {
                value["files"]["entries"]["Micropublication_2026-1.0.0.zip"]["checksum"] =
                    "sha256:abc".into();
            },
            "is not an MD5",
        ),
        (
            "fewer files than it counts",
            |value| value["files"]["count"] = 2.into(),
            "it lists 1 of the 2 files it counts",
        ),
    ];
    for (row, change, reason) in rows {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("invenio/caltech-record.json")).unwrap();
        change(&mut value);
        let recorded = Recorded::default().on(CALTECH, 200, serde_json::to_vec(&value).unwrap());
        let reference = Reference::parse("invenio:data.caltech.edu/dt797-nsy79")
            .unwrap()
            .reference;
        let Err(error) = block_on(reference.resolve(&recorded)) else {
            panic!("{row}: the answer was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
    }
}

#[test]
#[ignore = "live: talks to data.caltech.edu"]
fn live_invenio() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("https://data.caltech.edu/records/dt797-nsy79")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "invenio: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(
        listing.resolved.to_string(),
        "invenio:data.caltech.edu/dt797-nsy79"
    );
    assert_eq!(asked(&work), 1);
}
