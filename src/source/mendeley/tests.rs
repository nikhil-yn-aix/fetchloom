use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Reference, Remote};

const API: &str = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj";
const DATASET: &str = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj?version=2";
const FOLDERS: &str = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj/folders/2";
const ROOT: &str = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj/files?folder_id=root&version=2&$start=0";
const TITLE: &str =
    "Labeled Optical Coherence Tomography (OCT) and Chest X-Ray Images for Classification";
const IMAGES: &str = "c8996286-84fb-4a22-83a6-95e6aaf67e67";
const ANGRY: &str = "0609435c-411c-4740-9956-cf84490a991c";

fn changed(fixture: &str, change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes(fixture)).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn files(folder: &str, version: u64, start: u64) -> String {
    format!("{API}/files?folder_id={folder}&version={version}&$start={start}")
}

/// a file as mendeley lists it, its download link on its public-files path.
fn entry(name: &str, id: &str, size: u64) -> serde_json::Value {
    json!({
        "filename": name,
        "id": id,
        "size": size,
        "status": "COMPLETED",
        "content_details": {
            "sha256_hash": "13efc055629733dbab07877f8b3c9f81097840dbcdaa326a8542322c2281ce36",
            "size": size,
            "download_url": format!("https://data.mendeley.com/public-files/datasets/rscbjbr9sj/files/{id}/file_downloaded"),
        },
    })
}

fn listed(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

/// the version 2 answers: the dataset, no folders, and the three files of its top folder.
fn version_two() -> Recorded {
    Recorded::default()
        .fixture(DATASET, "mendeley/dataset.json")
        .fixture(FOLDERS, "mendeley/folders.json")
        .fixture(ROOT, "mendeley/files.json")
        .total(ROOT, 3)
}

fn dataset(text: &str) -> Dataset {
    Dataset::parse(text).unwrap()
}

#[test]
fn lists_a_version_with_the_sha256_mendeley_states() {
    let recorded = version_two();
    let listing = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "mendeley:rscbjbr9sj@2");
    assert_eq!(listing.title.as_deref(), Some(TITLE));
    assert_eq!(listing.license.as_deref(), Some("CC BY 4.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.17632/rscbjbr9sj.2"));
    assert_eq!(listing.access, Access::Public);
    let file = |path: &str, size, sha256, id| Remote {
        path: path.to_owned(),
        size: Some(size),
        sha256: Sha256::from_hex(sha256),
        md5: None,
        url: Url::parse(&format!(
            "https://data.mendeley.com/public-files/datasets/rscbjbr9sj/files/{id}/file_downloaded"
        ))
        .unwrap(),
        named_by_server: false,
        etag: None,
    };
    assert_eq!(
        listing.files,
        vec![
            file(
                "ChestXRay2017.zip",
                1_235_512_464,
                "13efc055629733dbab07877f8b3c9f81097840dbcdaa326a8542322c2281ce36",
                "f12eaf6d-6023-432f-acc9-80c9d7393433"
            ),
            file(
                "code2017.zip",
                19787,
                "a461639ad3f01684ea5312ec5093906ad774987598f32c7268100b01c714c4a9",
                "4935bcd6-c7f7-4244-a119-baaf7175b2d8"
            ),
            file(
                "OCT2017.tar.gz",
                5_793_183_169,
                "f82f9c7fcf4c2f6bb393fd185d8ddf620bd8c0272f7dc559f0c3c3055d7b8c68",
                "5699a1d8-d1b6-45db-bb92-b61051445347"
            ),
        ]
    );
    assert_eq!(recorded.asked(), [DATASET, FOLDERS, ROOT]);
}

#[test]
fn the_newest_version_is_pinned_and_asked_for_by_number_again() {
    let (folders, root) = (format!("{API}/folders/3"), files("root", 3, 0));
    let three = changed("mendeley/dataset.json", |value| {
        value["version"] = 3.into();
        value["doi"]["id"] = "10.17632/rscbjbr9sj.3".into();
    });
    let pinned = format!("{API}?version=3");
    let recorded = Recorded::default()
        .on(API, 200, three.clone())
        .on(&pinned, 200, three)
        .fixture(&folders, "mendeley/folders.json")
        .fixture(&root, "mendeley/files.json")
        .total(&root, 3);
    let newest = block_on(dataset("rscbjbr9sj").resolve(&recorded)).unwrap();
    assert_eq!(newest.resolved.to_string(), "mendeley:rscbjbr9sj@3");
    assert_eq!(newest.doi.as_deref(), Some("10.17632/rscbjbr9sj.3"));
    let again = block_on(newest.resolved.resolve(&recorded)).unwrap();
    assert_eq!(again.files, newest.files);
    assert_eq!(
        recorded.asked(),
        [API, &folders, &root, &pinned, &folders, &root]
    );
}

#[test]
fn folders_become_paths_below_the_top_folder() {
    let folders = json!([
        {"id": ANGRY, "name": "Angry", "parent_id": IMAGES},
        {"id": IMAGES, "name": "Image Dataset"},
    ]);
    let (images, angry) = (files(IMAGES, 2, 0), files(ANGRY, 2, 0));
    let recorded = Recorded::default()
        .fixture(DATASET, "mendeley/dataset.json")
        .on(FOLDERS, 200, listed(&folders))
        .on(ROOT, 200, b"[]".to_vec())
        .total(ROOT, 0)
        .on(
            &images,
            200,
            listed(&json!([entry(
                "about.txt",
                "11111111-1111-1111-1111-111111111111",
                5
            )])),
        )
        .total(&images, 1)
        .on(
            &angry,
            200,
            listed(&json!([entry(
                "0001.jpg",
                "22222222-2222-2222-2222-222222222222",
                7
            )])),
        )
        .total(&angry, 1);
    let listing = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        ["Image Dataset/Angry/0001.jpg", "Image Dataset/about.txt"]
    );
    assert_eq!(recorded.asked(), [DATASET, FOLDERS, ROOT, &angry, &images]);
}

#[test]
fn a_folders_files_are_read_part_by_part_up_to_its_count() {
    let second = files("root", 2, 3);
    let recorded = version_two().total(ROOT, 4).on(
        &second,
        200,
        listed(&json!([entry(
            "notes.txt",
            "33333333-3333-3333-3333-333333333333",
            9
        )])),
    );
    let recorded = recorded.total(&second, 4);
    let listing = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 4);
    assert_eq!(listing.files[3].path, "notes.txt");
    assert_eq!(recorded.asked(), [DATASET, FOLDERS, ROOT, &second]);
}

#[test]
fn without_a_count_parts_are_read_until_one_is_empty() {
    let second = files("root", 2, 3);
    let recorded = Recorded::default()
        .fixture(DATASET, "mendeley/dataset.json")
        .fixture(FOLDERS, "mendeley/folders.json")
        .fixture(ROOT, "mendeley/files.json")
        .on(&second, 200, b"[]".to_vec());
    let listing = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 3);
    assert_eq!(recorded.asked(), [DATASET, FOLDERS, ROOT, &second]);
}

#[test]
fn a_folder_listing_that_ends_short_or_repeats_itself_is_refused() {
    let second = files("root", 2, 3);
    for (row, part, reason) in [
        ("short", b"[]".to_vec(), "ends after 3 of the 6 files"),
        (
            "repeated",
            fixture_bytes("mendeley/files.json"),
            "repeats a file",
        ),
    ] {
        let recorded = version_two()
            .total(ROOT, 6)
            .on(&second, 200, part)
            .total(&second, 6);
        let error = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
    }
}

#[test]
fn a_file_without_a_download_link_is_listed_as_unavailable() {
    let body = changed("mendeley/files.json", |value| {
        value[0].as_object_mut().unwrap().remove("content_details");
    });
    let recorded = version_two().on(ROOT, 200, body).total(ROOT, 3);
    let listing = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 2);
    assert_eq!(listing.unavailable.len(), 1);
    assert_eq!(listing.unavailable[0].path, "ChestXRay2017.zip");
}

/// mendeley tells of a missing dataset or version with a 200 that holds an error, as well as
/// with a 404.
#[test]
fn missing_datasets_and_versions_are_unresolved() {
    let nine = format!("{API}?version=9");
    for (status, body) in [
        (200, fixture_bytes("mendeley/noversion.json")),
        (404, fixture_bytes("mendeley/missing.json")),
    ] {
        let recorded = Recorded::default().on(&nine, status, body);
        let error = block_on(dataset("rscbjbr9sj@9").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved, "{status}");
        assert_eq!(
            error.message,
            "mendeley:rscbjbr9sj@9 does not exist, or is not public"
        );
        assert_eq!(recorded.asked(), [nine.as_str()]);
    }
}

#[test]
fn an_embargo_says_no_sign_in_lifts_it() {
    for (status, body) in [
        (451, b"{}".to_vec()),
        (
            200,
            br#"{"error":{"message":"dataset under embargo","status":451}}"#.to_vec(),
        ),
    ] {
        let recorded = version_two().on(ROOT, status, body);
        let error = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired, "{status}");
        assert!(error.message.contains("embargo"), "{}", error.message);
        assert!(error.need.is_none());
        assert_eq!(
            error.help.as_deref(),
            Some(
                "https://data.mendeley.com/datasets/rscbjbr9sj/2 says when the embargo ends; no sign-in lifts it"
            )
        );
    }
}

#[test]
fn a_refusal_names_no_account_to_sign_in_to() {
    for status in [401, 403] {
        let recorded = Recorded::default().on(DATASET, status, b"{}".to_vec());
        let error = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert!(error.need.is_none());
    }
}

/// an answer fetchloom cannot trust is refused for its reason, with help.
#[test]
fn an_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, &str, Change, &str); 8] = [
        (
            "another dataset",
            "dataset",
            |value| value["id"] = "zzzzzzzzzz".into(),
            "names another dataset",
        ),
        (
            "another version",
            "dataset",
            |value| value["version"] = 3.into(),
            "names another version",
        ),
        (
            "an error it does not explain",
            "dataset",
            |value| *value = json!({"error": {"message": "busy", "status": 500}}),
            "an error mendeley does not explain",
        ),
        (
            "a folder below a folder it does not list",
            "folders",
            |value| *value = json!([{"id": ANGRY, "name": "Angry", "parent_id": IMAGES}]),
            "a folder lies in a folder it does not list",
        ),
        (
            "folders that hold each other",
            "folders",
            |value| {
                *value = json!([
                    {"id": ANGRY, "name": "Angry", "parent_id": IMAGES},
                    {"id": IMAGES, "name": "Image Dataset", "parent_id": ANGRY},
                ]);
            },
            "its folders hold each other",
        ),
        (
            "a file id that is no UUID",
            "files",
            |value| value[0]["id"] = "../../x".into(),
            "has an id that is no UUID",
        ),
        (
            "another dataset's download link",
            "files",
            |value| {
                value[0]["content_details"]["download_url"] = "https://data.mendeley.com/public-files/datasets/zzzzzzzzzz/files/f12eaf6d-6023-432f-acc9-80c9d7393433/file_downloaded".into();
            },
            "is not this dataset's",
        ),
        (
            "a SHA-256 that is not hex",
            "files",
            |value| value[0]["content_details"]["sha256_hash"] = "broken".into(),
            "is not hex",
        ),
    ];
    for (row, answer, change, reason) in rows {
        let mut bodies = [
            fixture_bytes("mendeley/dataset.json"),
            fixture_bytes("mendeley/folders.json"),
            fixture_bytes("mendeley/files.json"),
        ];
        let at = ["dataset", "folders", "files"]
            .iter()
            .position(|name| *name == answer)
            .unwrap();
        bodies[at] = changed(&format!("mendeley/{answer}.json"), change);
        let [described, folders, root] = bodies;
        let recorded = Recorded::default()
            .on(DATASET, 200, described)
            .on(FOLDERS, 200, folders)
            .on(ROOT, 200, root)
            .total(ROOT, 3);
        let Err(error) = block_on(dataset("rscbjbr9sj@2").resolve(&recorded)) else {
            panic!("{row}: the answer was used");
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
fn ids_and_links_name_one_dataset_and_other_forms_are_refused() {
    for (text, expected) in [
        ("mendeley:RSCBJBR9SJ", "mendeley:rscbjbr9sj"),
        (
            "https://data.mendeley.com/datasets/rscbjbr9sj/2/",
            "mendeley:rscbjbr9sj@2",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap().reference;
        assert_eq!(parsed.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, parsed);
    }
    for text in [
        "mendeley:rscbjbr9sj@0",
        "mendeley:rscbjbr9sj@x",
        "mendeley:rscbjbr9sj@",
        "mendeley:rs",
        "mendeley:rscb-jbr9sj",
        "mendeley:",
        "https://data.mendeley.com/datasets/rscbjbr9sj/0",
        "https://data.mendeley.com/datasets/rscbjbr9sj/2/files/f12eaf6d-6023-432f-acc9-80c9d7393433",
        "https://data.mendeley.com/research-data/?search=oct",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let file = "https://data.mendeley.com/public-files/datasets/rscbjbr9sj/files/4935bcd6-c7f7-4244-a119-baaf7175b2d8/file_downloaded";
    assert_eq!(
        Reference::parse(file).unwrap().reference,
        Reference::Url(Url::parse(file).unwrap()),
        "a file's download link stays a plain link"
    );
}

#[test]
#[ignore = "live: talks to data.mendeley.com"]
fn live_mendeley() {
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
    let reference = Reference::parse("mendeley:rscbjbr9sj@2").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "mendeley: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 3);
    assert_eq!(listing.resolved.to_string(), "mendeley:rscbjbr9sj@2");
    assert!(listing.files.iter().all(|file| file.sha256.is_some()));
    assert_eq!(work.counts().requests, 3);
}
