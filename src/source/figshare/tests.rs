use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::Md5;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

const ARTICLE: &str = "https://api.figshare.com/v2/articles/34744689";
const VERSION: &str = "https://api.figshare.com/v2/articles/34744689/versions/1";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("figshare/article.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn lists_every_field_without_requesting_files_twice() {
    let recorded = Recorded::default().fixture(ARTICLE, "figshare/article.json");
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "figshare:34744689@1");
    assert_eq!(
        listing.title.as_deref(),
        Some(
            "Effects of xylooligosaccharide supplementation on methane emissions, rumen microbiome, and serum metabolome in lactating Jersey cows"
        )
    );
    assert_eq!(listing.license.as_deref(), Some("CC BY 4.0"));
    assert_eq!(
        listing.doi.as_deref(),
        Some("10.6084/m9.figshare.34744689.v1")
    );
    assert_eq!(
        listing.files,
        vec![
            Remote {
                path: "labt_a_2743468_sm3772.docx".to_owned(),
                size: Some(158_574),
                sha256: None,
                md5: Md5::from_hex("4adbc16263f40b9f4eb22a52bf94372f"),
                url: Url::parse("https://ndownloader.figshare.com/files/70482987").unwrap(),
                named_by_server: false,
            },
            Remote {
                path: "labt_a_2743468_sm3771.docx".to_owned(),
                size: Some(162_562),
                sha256: None,
                md5: Md5::from_hex("7027c09ac3f0c3f0f59be4d99c9dc4d0"),
                url: Url::parse("https://ndownloader.figshare.com/files/70482990").unwrap(),
                named_by_server: false,
            },
        ]
    );
    assert_eq!(recorded.asked(), [ARTICLE]);
}

#[test]
fn resolving_the_result_asks_for_its_version() {
    let recorded = Recorded::default()
        .fixture(ARTICLE, "figshare/article.json")
        .fixture(VERSION, "figshare/version.json");
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    let pinned = block_on(listing.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, listing.files);
    assert_eq!(pinned.resolved.to_string(), "figshare:34744689@1");
    assert_eq!(recorded.asked(), [ARTICLE, VERSION]);
}

#[test]
fn large_articles_use_the_pinned_files_endpoint() {
    let record = "https://api.figshare.com/v2/articles/34789419";
    let files =
        "https://api.figshare.com/v2/articles/34789419/versions/1/files?page=1&page_size=1000";
    let recorded = Recorded::default()
        .fixture(record, "figshare/large.json")
        .fixture(files, "figshare/large-files.json");
    let listing = block_on(Article::parse("34789419").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "figshare:34789419@1");
    assert_eq!(listing.files.len(), 286);
    assert!(
        listing
            .files
            .iter()
            .any(|file| file.path == "Ardhraka_Korsakow Film/data/sys/lib/jquery.js")
    );
    assert_eq!(recorded.asked(), [record, files]);
}

#[test]
fn a_truncated_article_does_not_drop_files() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("figshare/large.json")).unwrap();
    value["files"].as_array_mut().unwrap().truncate(10);
    let record = "https://api.figshare.com/v2/articles/34789419";
    let files =
        "https://api.figshare.com/v2/articles/34789419/versions/1/files?page=1&page_size=1000";
    let recorded = Recorded::default()
        .on(record, 200, serde_json::to_vec(&value).unwrap())
        .fixture(files, "figshare/large-files.json");
    let listing = block_on(Article::parse("34789419").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 286);
    assert_eq!(recorded.asked(), [record, files]);
}

#[test]
fn computed_hashes_take_precedence_over_supplied_hashes() {
    let body = changed(|value| {
        value["files"][0]["supplied_md5"] = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
    });
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.files[0].md5,
        Md5::from_hex("4adbc16263f40b9f4eb22a52bf94372f")
    );
}

#[test]
fn supplied_hashes_cover_missing_computed_hashes() {
    let body = changed(|value| value["files"][0]["computed_md5"] = "".into());
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.files[0].md5,
        Md5::from_hex("4adbc16263f40b9f4eb22a52bf94372f")
    );
}

#[test]
fn folder_paths_are_kept() {
    let body = changed(|value| {
        value["folder_structure"] = serde_json::json!({"70482987": "supplements/tables"});
    });
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing.files[0].path,
        "supplements/tables/labt_a_2743468_sm3772.docx"
    );
}

#[test]
fn link_only_files_are_listed_as_unavailable() {
    let body = changed(|value| {
        value["files"][0]["is_link_only"] = true.into();
        value["files"][0]["download_url"] = "https://example.org/elsewhere?id=1".into();
        value["folder_structure"] = serde_json::json!({"70482987": "links"});
    });
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let listing = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap();
    assert_eq!(
        listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["labt_a_2743468_sm3771.docx"]
    );
    assert_eq!(listing.unavailable.len(), 1);
    assert_eq!(
        listing.unavailable[0].path,
        "links/labt_a_2743468_sm3772.docx"
    );
}

#[test]
fn missing_articles_are_unresolved() {
    let url = "https://api.figshare.com/v2/articles/999999999999";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("figshare/missing.json"));
    let error = block_on(Article::parse("999999999999").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn access_refusals_require_authorization() {
    for status in [401, 403] {
        let recorded =
            Recorded::default().on(ARTICLE, status, fixture_bytes("figshare/refused.json"));
        let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
    }
}

#[test]
fn disabled_downloads_do_not_look_empty() {
    let body = changed(|value| value["download_disabled"] = true.into());
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
}

#[test]
fn requested_versions_cannot_silently_change() {
    let url = "https://api.figshare.com/v2/articles/34744689/versions/2";
    let recorded = Recorded::default().fixture(url, "figshare/version.json");
    let error = block_on(Article::parse("34744689@2").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
fn secrets_in_download_urls_never_enter_a_listing() {
    let body = changed(|value| {
        value["files"][0]["download_url"] =
            "https://ndownloader.figshare.com/files/70482987?token=secret".into();
    });
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(!error.message.contains("secret"));
}

#[test]
fn malformed_checksums_are_refused() {
    let body = changed(|value| value["files"][0]["computed_md5"] = "broken".into());
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

#[test]
#[ignore = "live: talks to api.figshare.com"]
fn live_figshare() {
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
    let reference = Reference::parse("figshare:34744689@1").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "figshare: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 2);
    assert_eq!(listing.resolved.to_string(), "figshare:34744689@1");
    assert_eq!(work.counts().requests, 1);
}
#[test]
fn file_pages_continue_until_the_last_page() {
    let first = "https://api.figshare.com/v2/articles/34744689/versions/1/files?page=1&page_size=1";
    let second =
        "https://api.figshare.com/v2/articles/34744689/versions/1/files?page=2&page_size=1";
    let last = "https://api.figshare.com/v2/articles/34744689/versions/1/files?page=3&page_size=1";
    let recorded = Recorded::default()
        .fixture(first, "figshare/files-page1.json")
        .fixture(second, "figshare/files-page2.json")
        .fixture(last, "figshare/files-page3.json");
    let article = Article::parse("34744689@1").unwrap();
    let files = block_on(article.files(&recorded, 1)).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].name, "labt_a_2743468_sm3772.docx");
    assert_eq!(files[1].name, "labt_a_2743468_sm3771.docx");
    assert_eq!(recorded.asked(), [first, second, last]);
}

#[test]
fn repeated_file_pages_are_refused() {
    let first = "https://api.figshare.com/v2/articles/34744689/versions/1/files?page=1&page_size=1";
    let second =
        "https://api.figshare.com/v2/articles/34744689/versions/1/files?page=2&page_size=1";
    let recorded = Recorded::default()
        .fixture(first, "figshare/files-page1.json")
        .fixture(second, "figshare/files-page1.json");
    let error = block_on(Article::parse("34744689@1").unwrap().files(&recorded, 1))
        .err()
        .unwrap();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [first, second]);
}

#[test]
fn embargoed_articles_explain_why_downloads_are_unavailable() {
    let body = changed(|value| value["is_embargoed"] = true.into());
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.help.is_some());
}

#[test]
fn numeric_titles_and_large_versions_keep_the_article_identity() {
    for (path, expected) in [
        ("dataset/2024/1234", "figshare:1234"),
        ("journal_contribution/2024/1234", "figshare:1234"),
        ("online_resource/2024/1234", "figshare:1234"),
        ("dataset/title/34744689/10000", "figshare:34744689@10000"),
        (
            "dataset/2024/34744689/4294967295",
            "figshare:34744689@4294967295",
        ),
        ("title/34744689/10000", "figshare:34744689@10000"),
        ("title/34744689/1", "figshare:34744689@1"),
        ("code/34744689/1", "figshare:34744689@1"),
        ("paper/34744689/1", "figshare:34744689@1"),
        ("fileset/34744689/1", "figshare:34744689@1"),
        ("metadata/34744689/1", "figshare:34744689@1"),
        ("title/34744689", "figshare:34744689"),
        ("2024/1234", "figshare:1234"),
        ("34744689", "figshare:34744689"),
    ] {
        for host in ["figshare.com", "tandf.figshare.com"] {
            let url = format!("https://{host}/articles/{path}");
            let parsed = Reference::parse(&url).unwrap();
            assert_eq!(parsed.reference.to_string(), expected, "{url}");
            assert_eq!(
                Reference::parse(expected).unwrap().reference,
                parsed.reference
            );
        }
    }
    for path in [
        "dataset/title/34744689/4294967296",
        "dataset/title/34744689/not-a-version",
        "dataset/title/34744689/1/2",
    ] {
        let error = Reference::parse(&format!("https://figshare.com/articles/{path}")).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{path}");
    }
}

#[test]
fn parsed_versions_request_only_the_named_article_version() {
    for (path, id, version, endpoint) in [
        (
            "dataset/2024/1234",
            1234,
            1,
            "https://api.figshare.com/v2/articles/1234",
        ),
        (
            "dataset/title/34744689/10000",
            34_744_689,
            10000,
            "https://api.figshare.com/v2/articles/34744689/versions/10000",
        ),
    ] {
        let body = changed(|value| {
            value["id"] = id.into();
            value["version"] = version.into();
        });
        let recorded = Recorded::default().on(endpoint, 200, body);
        let reference = Reference::parse(&format!("https://figshare.com/articles/{path}"))
            .unwrap()
            .reference;
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(
            listing.resolved.to_string(),
            format!("figshare:{id}@{version}")
        );
        assert_eq!(listing.files.len(), 2);
        assert_eq!(recorded.asked(), [endpoint]);
    }
}

#[test]
fn figshare_portal_pages_are_not_downloads() {
    for host in ["tandf.figshare.com", "www.figshare.com", "figshare.com"] {
        for path in [
            "/collections/example/12345",
            "/account/settings",
            "/articles/dataset/title/not-an-id",
        ] {
            let url = format!("https://{host}{path}");
            let error = Reference::parse(&url).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{url}");
            assert!(error.message.contains("figshare"), "{url}");
        }
    }
}

#[test]
fn figshare_portal_downloads_stay_plain_links() {
    for url in [
        "https://ndownloader.figshare.com/files/70482987",
        "https://api.figshare.com/v2/articles/34744689",
        "https://tandf.figshare.com/ndownloader/files/70482987",
        "https://figshare.com/ndownloader/files/70482987",
        "https://figshare.com.example.org/data.csv",
        "https://notfigshare.com/data.csv",
    ] {
        let parsed = Reference::parse(url).unwrap();
        assert_eq!(parsed.reference, Reference::Url(Url::parse(url).unwrap()));
    }
}

#[test]
fn figshare_portal_dois_do_not_fetch_unsupported_pages() {
    let handle = "https://doi.org/api/handles/10.6084%2Fm9.figshare.34744689.v1";
    let mut body: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("doi/figshare.json")).unwrap();
    body["values"][1]["data"]["value"] =
        "https://tandf.figshare.com/collections/example/12345".into();
    let recorded = Recorded::default().on(handle, 200, serde_json::to_vec(&body).unwrap());
    let reference = Reference::parse("doi:10.6084/m9.figshare.34744689.v1")
        .unwrap()
        .reference;
    let error = block_on(reference.resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(recorded.asked(), [handle]);
}
