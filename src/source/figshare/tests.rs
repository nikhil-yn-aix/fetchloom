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
fn link_only_files_are_refused_by_name() {
    let body = changed(|value| value["files"][0]["is_link_only"] = true.into());
    let recorded = Recorded::default().on(ARTICLE, 200, body);
    let error = block_on(Article::parse("34744689").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.message.contains("labt_a_2743468_sm3772.docx"));
    assert!(error.help.is_some());
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
