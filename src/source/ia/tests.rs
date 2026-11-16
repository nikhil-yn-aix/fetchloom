use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const IRIS: &str = "https://archive.org/metadata/iris_20240626";
const LENDING: &str = "https://archive.org/metadata/isbn_9781425795511";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("ia/iris.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn item(text: &str) -> Item {
    Item::parse(text).unwrap()
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// the item's data file goes with the size and MD5 archive.org states, its link on archive.org;
/// archive.org's own records of the item, which it remakes whenever the item changes, stay out.
#[test]
fn lists_an_items_files_with_the_size_and_md5_it_states() {
    let recorded = Recorded::default().fixture(IRIS, "ia/iris.json");
    let listing = block_on(item("iris_20240626").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "ia:iris_20240626");
    assert_eq!(listing.title.as_deref(), Some("Iris dataset"));
    assert_eq!(listing.access, Access::Public);
    assert_eq!(
        listing.files,
        [Remote {
            path: "iris.csv".to_owned(),
            size: Some(4753),
            sha256: None,
            md5: Md5::from_hex("635234f87d12fb47b62990b244002e05"),
            url: Url::parse("https://archive.org/download/iris_20240626/iris.csv").unwrap(),
            named_by_server: false,
            etag: None,
        }]
    );
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    assert_eq!(recorded.asked(), [IRIS]);
}

/// a file the item keeps private, as a lending library's book keeps its scans, is left out with
/// a warning that names it, while the files anyone may download are listed.
#[test]
fn a_file_the_item_keeps_private_is_left_out_with_a_reason() {
    let recorded = Recorded::default().fixture(LENDING, "ia/lending.json");
    let listing = block_on(item("isbn_9781425795511").resolve(&recorded)).unwrap();
    assert_eq!(
        paths(&listing),
        [
            "isbn_9781425795511.lcpdf",
            "isbn_9781425795511_encrypted.pdf",
            "isbn_9781425795511_lcp.epub",
            "isbn_9781425795511_scandata.xml",
        ]
    );
    assert_eq!(
        listing.warnings,
        [
            "archive.org keeps 19 files of ia:isbn_9781425795511 private, so they are left out: isbn_9781425795511.epub, isbn_9781425795511.pdf, isbn_9781425795511_chocr.html.gz and 16 more"
        ]
    );

    let body = changed(|value| value["files"][0]["private"] = "true".into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let error = block_on(item("iris_20240626").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "archive.org keeps every file of ia:iris_20240626 private"
    );
}

/// a file in a folder of the item keeps its path, and its link encodes each part of its name.
#[test]
fn a_files_path_keeps_its_folders_and_its_link_encodes_its_name() {
    let body = changed(|value| value["files"][0]["name"] = "tables/data #1%?.csv".into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let listing = block_on(item("iris_20240626").resolve(&recorded)).unwrap();
    assert_eq!(listing.files[0].path, "tables/data #1%?.csv");
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://archive.org/download/iris_20240626/tables/data%20%231%25%3F.csv"
    );
}

/// archive.org answers an identifier it has no item for with an empty answer, telling capitals
/// apart, and keeps a dark item's files from everyone.
#[test]
fn a_missing_or_dark_item_is_unresolved() {
    let missing = "https://archive.org/metadata/IRIS_20240626";
    let recorded = Recorded::default().fixture(missing, "ia/missing.json");
    let error = block_on(item("IRIS_20240626").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "ia:IRIS_20240626 does not exist, or is not public"
    );
    assert!(error.help.unwrap().contains("capitals"));

    let body = changed(|value| value["is_dark"] = true.into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let error = block_on(item("iris_20240626").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "archive.org has taken ia:iris_20240626 down");
}

/// a collection's own files are its logo and records, not the items it gathers.
#[test]
fn a_collection_is_refused_with_how_to_add_its_items() {
    let body = changed(|value| value["metadata"]["mediatype"] = "collection".into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let error = block_on(item("iris_20240626").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceInvalid);
    assert!(
        error
            .help
            .unwrap()
            .contains("https://archive.org/details/iris_20240626"),
    );
}

/// an MD5 that sums the item's files rather than hashing one, as archive.org writes beside a
/// file it marks so, checks nothing and is left out.
#[test]
fn a_summed_md5_is_not_a_files_own() {
    let body = changed(|value| value["files"][0]["summation"] = "md5".into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let listing = block_on(item("iris_20240626").resolve(&recorded)).unwrap();
    assert_eq!(listing.files[0].md5, None);
    assert_eq!(listing.files[0].size, Some(4753));
}

/// the help goes to fetchloom's issues, since the item's uploader cannot change archive.org's
/// answer.
#[test]
fn an_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 3] = [
        (
            "another item",
            |value| value["metadata"]["identifier"] = "iris_20240627".into(),
            "the answer names another item",
        ),
        (
            "an MD5 that is no MD5",
            |value| value["files"][0]["md5"] = "broken".into(),
            "the MD5 of \"iris.csv\" is not an MD5",
        ),
        (
            "a size that is no number",
            |value| value["files"][0]["size"] = "4.7K".into(),
            "the size of \"iris.csv\" is not a number",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = Recorded::default().on(IRIS, 200, changed(change));
        let Err(error) = block_on(item("iris_20240626").resolve(&recorded)) else {
            panic!("{row}: the answer was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains(crate::error::ISSUES)),
            "{row}"
        );
    }
}

/// an escaping name reaches the listing check every source's answer passes.
#[test]
fn an_escaping_name_reaches_the_shared_listing_check() {
    let body = changed(|value| value["files"][0]["name"] = "../iris.csv".into());
    let recorded = Recorded::default().on(IRIS, 200, body);
    let reference = Reference::parse("ia:iris_20240626").unwrap().reference;
    assert_eq!(
        block_on(reference.resolve(&recorded)).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

/// an item's page and its folder of downloads name it; a link to one of its files stays a plain
/// link, since it may name a file inside a zip, and another page of the item is refused.
#[test]
fn identifiers_and_item_links_name_one_item_and_other_forms_are_refused() {
    for text in [
        "ia:iris_20240626",
        "https://archive.org/details/iris_20240626",
        "https://archive.org/details/iris_20240626/",
        "https://www.archive.org/details/iris_20240626?tab=about",
        "https://archive.org/download/iris_20240626",
        "https://archive.org/download/iris_20240626/",
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert_eq!(parsed.reference.to_string(), "ia:iris_20240626", "{text}");
    }
    let long = format!("ia:{}", "a".repeat(101));
    for text in [
        "ia:",
        "ia:-iris",
        "ia:iris/2024",
        "ia:iris 2024",
        long.as_str(),
        "https://archive.org/details/iris_20240626/iris.csv",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let file = "https://archive.org/download/iris_20240626/iris.csv";
    assert_eq!(
        Reference::parse(file).unwrap().reference,
        Reference::Url(Url::parse(file).unwrap())
    );
}

#[test]
#[ignore = "live: talks to archive.org"]
fn live_ia() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("ia:iris_20240626").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "ia: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(paths(&listing), ["iris.csv"]);
    assert_eq!(asked(&work), 1);
}
