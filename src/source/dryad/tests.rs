use std::time::Instant;

use super::*;
use crate::digest::{Md5, Sha256};
use crate::error::Kind;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};
use crate::source::{Access, Reference, Remote};

const DATASET: &str = "https://datadryad.org/api/v2/datasets/doi:10.5061%2Fdryad.j1fd7";
const VERSIONS: &str =
    "https://datadryad.org/api/v2/datasets/doi:10.5061%2Fdryad.j1fd7/versions?page=1&per_page=100";
const FILES: &str = "https://datadryad.org/api/v2/versions/26724/files?page=1&per_page=100";
const TITLE: &str = "Data from: Data archiving is a good  investment";

fn changed(fixture: &str, change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes(fixture)).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn dataset(text: &str) -> Dataset {
    Dataset::parse(text).unwrap()
}

fn listed(path: &str, size: u64, md5: &str, id: u64) -> Remote {
    Remote {
        path: path.to_owned(),
        size: Some(size),
        sha256: None,
        md5: Md5::from_hex(md5),
        url: Url::parse(&format!("https://datadryad.org/api/v2/files/{id}/download")).unwrap(),
        named_by_server: false,
        etag: None,
        blob: None,
    }
}

#[test]
fn lists_the_newest_version_with_the_md5_dryad_states() {
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .fixture(FILES, "dryad/files.json");
    let listing = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "dryad:10.5061/dryad.j1fd7@1");
    assert_eq!(listing.title.as_deref(), Some(TITLE));
    assert_eq!(listing.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(listing.doi.as_deref(), Some("10.5061/dryad.j1fd7"));
    assert_eq!(listing.access, Access::Public);
    assert_eq!(
        listing.files,
        vec![
            listed(
                "PMC_reuse_of_2007_GEO_datasets.csv",
                1_842_019,
                "1bfc5f0f05efb9c4d8abe042ecd8a66f",
                94868
            ),
            listed(
                "README_for_PMC_reuse_of_2007_GEO_datasets.pdf",
                747_416,
                "8ebe9f0e0eef67d94d46a4f2d3dede56",
                94869
            ),
            listed(
                "tables.csv",
                1176,
                "76330f8572ab638692ce80a14f9ac977",
                94870
            ),
        ]
    );
    assert_eq!(recorded.asked(), [DATASET, FILES]);
}

/// dryad's visibility says who may read a version: anyone, or an account it lets see a
/// restricted one. the hint steers `push`, never a download.
#[test]
fn a_versions_visibility_becomes_its_access() {
    for (visibility, expected) in [
        (Some("public"), Access::Public),
        (Some("restricted"), Access::Login),
        (Some("embargoed"), Access::Unknown),
        (None, Access::Unknown),
    ] {
        let body = changed("dryad/dataset.json", |value| match visibility {
            Some(visibility) => value["visibility"] = visibility.into(),
            None => {
                value.as_object_mut().unwrap().remove("visibility");
            }
        });
        let recorded = Recorded::default()
            .on(DATASET, 200, body)
            .fixture(FILES, "dryad/files.json");
        let listing = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
        assert_eq!(listing.access, expected, "{visibility:?}");
    }
}

#[test]
fn the_pinned_newest_version_lists_the_same_files_and_license() {
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .fixture(FILES, "dryad/files.json");
    let newest = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
    let pinned = block_on(newest.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.resolved, newest.resolved);
    assert_eq!(pinned.files, newest.files);
    assert_eq!(pinned.license, newest.license);
    assert_eq!(recorded.asked(), [DATASET, FILES, DATASET, FILES]);
}

#[test]
fn an_older_version_is_found_in_the_versions_list() {
    let body = changed("dryad/dataset.json", |value| {
        value["versionNumber"] = 2.into();
        value["title"] = "a newer title".into();
        value["visibility"] = "restricted".into();
        value["_links"]["stash:version"]["href"] = "/api/v2/versions/30001".into();
    });
    let recorded = Recorded::default()
        .on(DATASET, 200, body)
        .fixture(VERSIONS, "dryad/versions.json")
        .fixture(FILES, "dryad/files.json");
    let listing = block_on(dataset("10.5061/dryad.j1fd7@1").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "dryad:10.5061/dryad.j1fd7@1");
    assert_eq!(listing.title.as_deref(), Some(TITLE));
    assert_eq!(listing.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(listing.access, Access::Public, "the older version's own");
    assert_eq!(listing.files.len(), 3);
    assert_eq!(recorded.asked(), [DATASET, VERSIONS, FILES]);
}

#[test]
fn the_versions_list_is_read_page_by_page() {
    let second = "https://datadryad.org/api/v2/datasets/doi:10.5061%2Fdryad.j1fd7/versions?page=2&per_page=100";
    let files = "https://datadryad.org/api/v2/versions/30001/files?page=1&per_page=100";
    let body = changed("dryad/dataset.json", |value| {
        value["versionNumber"] = 3.into();
        value["_links"]["stash:version"]["href"] = "/api/v2/versions/30002".into();
    });
    let first_page = changed("dryad/versions.json", |value| value["total"] = 2.into());
    let second_page = changed("dryad/versions.json", |value| {
        value["total"] = 2.into();
        let version = &mut value["_embedded"]["stash:versions"][0];
        version["versionNumber"] = 2.into();
        version["_links"]["self"]["href"] = "/api/v2/versions/30001".into();
    });
    let recorded = Recorded::default()
        .on(DATASET, 200, body)
        .on(VERSIONS, 200, first_page)
        .on(second, 200, second_page)
        .fixture(files, "dryad/files.json");
    let listing = block_on(dataset("10.5061/dryad.j1fd7@2").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "dryad:10.5061/dryad.j1fd7@2");
    assert_eq!(recorded.asked(), [DATASET, VERSIONS, second, files]);
}

#[test]
fn a_version_dryad_does_not_list_is_unresolved() {
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .fixture(VERSIONS, "dryad/versions.json");
    let error = block_on(dataset("10.5061/dryad.j1fd7@5").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(
        error.message.contains("no published version 5"),
        "{}",
        error.message
    );
    assert!(error.help.is_some());
    assert_eq!(recorded.asked(), [DATASET, VERSIONS]);
}

#[test]
fn files_are_read_page_by_page_up_to_the_count_dryad_states() {
    let second = "https://datadryad.org/api/v2/versions/26724/files?page=2&per_page=100";
    let first_page = changed("dryad/files.json", |value| value["total"] = 4.into());
    let second_page = changed("dryad/files.json", |value| {
        value["total"] = 4.into();
        let files = value["_embedded"]["stash:files"].as_array_mut().unwrap();
        files.truncate(1);
        files[0]["path"] = "more/rows.csv".into();
        files[0]["_links"]["stash:download"]["href"] = "/api/v2/files/94871/download".into();
    });
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .on(FILES, 200, first_page)
        .on(second, 200, second_page);
    let listing = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 4);
    assert_eq!(listing.files[3].path, "more/rows.csv");
    assert_eq!(
        listing.files[3].url.as_str(),
        "https://datadryad.org/api/v2/files/94871/download"
    );
    assert_eq!(recorded.asked(), [DATASET, FILES, second]);
}

#[test]
fn a_listing_that_ends_before_its_count_is_refused() {
    let second = "https://datadryad.org/api/v2/versions/26724/files?page=2&per_page=100";
    let first_page = changed("dryad/files.json", |value| value["total"] = 5.into());
    let empty = changed("dryad/files.json", |value| {
        value["total"] = 5.into();
        value["_embedded"]["stash:files"] = serde_json::json!([]);
    });
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .on(FILES, 200, first_page)
        .on(second, 200, empty);
    let error = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains("ends after 3 of the 5 files"),
        "{}",
        error.message
    );
    let none = changed("dryad/files.json", |value| {
        value["total"] = 1.into();
        value["_embedded"]["stash:files"] = serde_json::json!([]);
    });
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .on(FILES, 200, none);
    let error = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap_err();
    assert!(
        error
            .message
            .contains("ends after 0 of the 1 file it counts"),
        "{}",
        error.message
    );
}

#[test]
fn sha256_digests_are_kept_and_other_kinds_check_nothing() {
    let sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let body = changed("dryad/files.json", |value| {
        let files = &mut value["_embedded"]["stash:files"];
        files[0]["digestType"] = "sha-256".into();
        files[0]["digest"] = sha256.into();
        files[1]["digestType"] = "sha-1".into();
        files[1]["digest"] = "da39a3ee5e6b4b0d3255bfef95601890afd80709".into();
    });
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .on(FILES, 200, body);
    let listing = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
    let digests: Vec<_> = listing
        .files
        .iter()
        .map(|file| (file.sha256, file.md5))
        .collect();
    assert_eq!(
        digests,
        [
            (Sha256::from_hex(sha256), None),
            (None, None),
            (None, Md5::from_hex("76330f8572ab638692ce80a14f9ac977")),
        ]
    );
}

#[test]
fn deleted_entries_are_not_listed() {
    let body = changed("dryad/files.json", |value| {
        value["_embedded"]["stash:files"][1]["status"] = "deleted".into();
    });
    let recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .on(FILES, 200, body);
    let listing = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["PMC_reuse_of_2007_GEO_datasets.csv", "tables.csv"]);
}

#[test]
fn missing_datasets_are_unresolved() {
    let url = "https://datadryad.org/api/v2/datasets/doi:10.5061%2Fdryad.nothere";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("dryad/missing.json"));
    let error = block_on(dataset("10.5061/dryad.nothere").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn refusals_name_the_dryad_account() {
    for status in [401, 403] {
        let recorded = Recorded::default().on(DATASET, status, fixture_bytes("dryad/refused.json"));
        let error = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(error.need.unwrap().account, Account::Dryad);
    }
}

#[test]
fn an_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, &str, Change, &str); 6] = [
        (
            "another dataset",
            "dryad/dataset.json",
            |value| value["identifier"] = "doi:10.5061/dryad.other".into(),
            "names another dataset",
        ),
        (
            "no version link",
            "dryad/dataset.json",
            |value| {
                value["_links"]
                    .as_object_mut()
                    .unwrap()
                    .remove("stash:version");
            },
            "links no version",
        ),
        (
            "a version link to another host",
            "dryad/dataset.json",
            |value| {
                value["_links"]["stash:version"]["href"] =
                    "https://example.org/api/v2/versions/26724".into();
            },
            "links no version",
        ),
        (
            "a digest that is not hex",
            "dryad/files.json",
            |value| value["_embedded"]["stash:files"][0]["digest"] = "broken".into(),
            "is not hex of its type",
        ),
        (
            "a sha-256 named as md5",
            "dryad/files.json",
            |value| {
                value["_embedded"]["stash:files"][0]["digest"] =
                    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into();
            },
            "is not hex of its type",
        ),
        (
            "a file without a link",
            "dryad/files.json",
            |value| value["_embedded"]["stash:files"][2]["_links"] = serde_json::json!({}),
            "\"tables.csv\" has no download link",
        ),
    ];
    for (row, fixture, change, reason) in rows {
        let (described, files) = if fixture == "dryad/dataset.json" {
            (changed(fixture, change), fixture_bytes("dryad/files.json"))
        } else {
            (
                fixture_bytes("dryad/dataset.json"),
                changed(fixture, change),
            )
        };
        let recorded = Recorded::default()
            .on(DATASET, 200, described)
            .on(FILES, 200, files);
        let Err(error) = block_on(dataset("10.5061/dryad.j1fd7").resolve(&recorded)) else {
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
        ("dryad:10.6078/D1H13D", "dryad:10.6078/d1h13d"),
        (
            "https://www.datadryad.org/dataset/doi:10.5061/dryad.j1fd7",
            "dryad:10.5061/dryad.j1fd7",
        ),
        (
            "https://datadryad.org/dataset/doi%3A10.5061%2Fdryad.j1fd7",
            "dryad:10.5061/dryad.j1fd7",
        ),
        (
            "https://datadryad.org/api/v2/datasets/doi%3A10.5061%2Fdryad.j1fd7",
            "dryad:10.5061/dryad.j1fd7",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap().reference;
        assert_eq!(parsed.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, parsed);
    }
    for text in [
        "dryad:10.5061/dryad.j1fd7@0",
        "dryad:10.5061/dryad.j1fd7@",
        "dryad:10.5061/dryad.j1fd7@latest",
        "dryad:dryad.j1fd7",
        "dryad:",
        "https://datadryad.org/dataset/10.5061/dryad.j1fd7",
        "https://datadryad.org/search?q=iris",
        "https://datadryad.org/downloads/file_stream/94868",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
}

#[test]
fn a_refused_download_asks_for_a_dryad_api_account() {
    let file = Url::parse("https://datadryad.org/api/v2/files/94868/download").unwrap();
    for status in [401, 403] {
        let error = crate::source::refused_download(&file, status).unwrap();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(error.need.unwrap().account, Account::Dryad);
        let help = error.help.unwrap();
        assert!(help.contains("DRYAD_TOKEN"), "{help}");
        assert!(help.contains("fetchloom auth login dryad"), "{help}");
    }
    assert!(crate::source::refused_download(&file, 404).is_none());
    for other in [
        "https://datadryad.org/api/v2/files/94868",
        "https://datadryad.org/api/v2/files/x/download",
        "https://datadryad.org.example.org/api/v2/files/94868/download",
        "https://datadryad.org/downloads/file_stream/94868",
    ] {
        let url = Url::parse(other).unwrap();
        assert!(
            crate::source::refused_download(&url, 401).is_none(),
            "{other}"
        );
    }
}

#[test]
#[ignore = "live: talks to datadryad.org"]
fn live_dryad() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("dryad:10.5061/dryad.j1fd7@1")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "dryad: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 3);
    assert_eq!(listing.resolved.to_string(), "dryad:10.5061/dryad.j1fd7@1");
    assert!(listing.files.iter().all(|file| file.md5.is_some()));
    assert_eq!(asked(&work), 2);
}

/// dryad's answers for doi:10.5061/dryad.j1fd7 whose version 26724 lists its files on pages of
/// `counts[i]` files each, every page stating `totals[i]` files in all, each answer taking a
/// second of a paused clock.
fn paged(counts: &[usize], totals: &[usize]) -> Recorded {
    let template: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("dryad/files.json")).unwrap();
    let entry = template["_embedded"]["stash:files"][0].clone();
    let mut recorded = Recorded::default()
        .fixture(DATASET, "dryad/dataset.json")
        .holding(std::time::Duration::from_secs(1));
    let mut id = 100_000;
    for (index, (count, total)) in counts.iter().zip(totals).enumerate() {
        let mut page = template.clone();
        page["total"] = (*total).into();
        page["count"] = (*count).into();
        let files: Vec<serde_json::Value> = (0..*count)
            .map(|_| {
                id += 1;
                let mut file = entry.clone();
                file["path"] = format!("data/f{id}.csv").into();
                file["_links"]["stash:download"]["href"] =
                    format!("/api/v2/files/{id}/download").into();
                file
            })
            .collect();
        page["_embedded"]["stash:files"] = files.into();
        let url = format!(
            "https://datadryad.org/api/v2/versions/26724/files?page={}&per_page=100",
            index + 1
        );
        recorded = recorded.on(&url, 200, serde_json::to_vec(&page).unwrap());
    }
    recorded.otherwise(404)
}

/// the listing or error of `recorded`'s dataset, the seconds of a paused clock it took, and the
/// requests it asked.
fn paged_resolve(recorded: &Recorded) -> (Result<Listing, Error>, u64, usize) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    runtime.block_on(async {
        let start = tokio::time::Instant::now();
        let listed = dataset("10.5061/dryad.j1fd7").resolve(recorded).await;
        (listed, start.elapsed().as_secs(), recorded.asked().len())
    })
}

/// a version of 1,000 files on ten pages is the same listing whether its pages go one at a time
/// or four at once once the first says how many there are, in five rounds of requests rather
/// than eleven, with no request more.
#[test]
fn pages_dryad_counts_go_together_in_fewer_rounds() {
    let (counts, totals) = ([100; 10], [1_000; 10]);
    let (alone, alone_rounds, alone_asked) = paged_resolve(&paged(&counts, &totals));
    let (together, together_rounds, together_asked) =
        paged_resolve(&paged(&counts, &totals).pages(4));
    let (alone, together) = (alone.unwrap(), together.unwrap());
    assert_eq!(alone.files.len(), 1_000);
    assert_eq!(format!("{alone:?}"), format!("{together:?}"));
    assert_eq!((alone_asked, together_asked), (11, 11));
    assert_eq!((alone_rounds, together_rounds), (11, 5));
}

/// a case of pages: what it shows, the files on each page, the count each page states, and the
/// most files the run lists, when it lists fewer than the default.
type Case = (&'static str, Vec<usize>, Vec<usize>, Option<usize>);

/// pages asked together end as pages one at a time do when the count changes on the way, a page
/// comes back short or empty, a page is missing, or the count passes the most files a run lists,
/// asking at most the three pages beside the one the walk one at a time stops at.
#[test]
fn pages_asked_together_end_as_pages_one_at_a_time_do() {
    let full = [100; 10];
    let cases: [Case; 6] = [
        (
            "the count falls",
            full.to_vec(),
            [1_000, 1_000, 250, 250, 250, 250, 250, 250, 250, 250].to_vec(),
            None,
        ),
        (
            "the count grows",
            [100; 11].to_vec(),
            [
                1_000, 1_100, 1_100, 1_100, 1_100, 1_100, 1_100, 1_100, 1_100, 1_100, 1_100,
            ]
            .to_vec(),
            None,
        ),
        (
            "an empty page before the count",
            [100, 100, 100, 0, 100, 100].to_vec(),
            [1_000; 6].to_vec(),
            None,
        ),
        (
            "a short page before the count",
            [100, 100, 40].to_vec(),
            [1_000; 3].to_vec(),
            None,
        ),
        (
            "a missing page",
            [100; 5].to_vec(),
            [1_000; 5].to_vec(),
            None,
        ),
        (
            "past the most files",
            full.to_vec(),
            [1_000; 10].to_vec(),
            Some(500),
        ),
    ];
    for (case, counts, totals, most) in cases {
        let made = || {
            let recorded = paged(&counts, &totals);
            match most {
                Some(most) => recorded.most(most),
                None => recorded,
            }
        };
        let (alone, _, alone_asked) = paged_resolve(&made());
        let (together, _, together_asked) = paged_resolve(&made().pages(4));
        let told = |listed: &Result<Listing, Error>| match listed {
            Ok(listing) => format!("{listing:?}"),
            Err(error) => format!("{:?} {}", error.kind, error.message),
        };
        assert_eq!(told(&alone), told(&together), "{case}");
        assert!(
            together_asked <= alone_asked + 3,
            "{case}: one at a time asked {alone_asked}, together {together_asked}"
        );
    }
}
