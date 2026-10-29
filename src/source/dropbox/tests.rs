use super::*;
use crate::error::Kind;
use crate::net::Api as _;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

#[test]
fn shared_links_keep_their_keys_and_request_downloads() {
    for (input, expected, name) in [
        (
            "https://www.dropbox.com/s/abc/data.csv?raw=1&rlkey=public&dl=0#preview",
            "https://www.dropbox.com/s/abc/data.csv?rlkey=public&dl=1",
            "data.csv",
        ),
        (
            "https://www.dropbox.com/scl/fi/abc/data.csv?rlkey=public&dl=0",
            "https://www.dropbox.com/scl/fi/abc/data.csv?rlkey=public&dl=1",
            "data.csv",
        ),
        (
            "https://www.dropbox.com/sh/abc/def?dl=0",
            "https://www.dropbox.com/sh/abc/def?dl=1",
            "download",
        ),
        (
            "https://www.dropbox.com/scl/fo/abc/def?rlkey=public&dl=0",
            "https://www.dropbox.com/scl/fo/abc/def?rlkey=public&dl=1",
            "download",
        ),
    ] {
        let parsed = Reference::parse(input).unwrap().reference;
        let listing = block_on(parsed.resolve(&Recorded::default())).unwrap();
        assert_eq!(listing.resolved.to_string(), expected);
        assert_eq!(listing.files.len(), 1);
        assert_eq!(listing.files[0].url.as_str(), expected);
        assert_eq!(listing.files[0].path, name);
        assert!(listing.files[0].named_by_server);
    }
}

#[test]
fn a_dropbox_html_answer_is_not_a_file() {
    let url = Url::parse(
        "https://www.dropbox.com/sh/o4p5od3yzzj7384/AADovh3gpXiPyPoygeB6Cwi1a/readme.md?dl=1",
    )
    .unwrap();
    let (from, divert) = crate::source::fetch_plan(&url);
    let recorded = Recorded::default()
        .fixture(from.as_str(), "dropbox/download-page.html")
        .content_type(from.as_str(), "text/html; charset=utf-8");
    let response = block_on(recorded.get(&from)).unwrap();
    let error = divert.unwrap()(&response.url, response.status, &response.text()).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.message.contains("dropbox"));
    assert!(error.help.unwrap().contains("browser"));
}

#[test]
fn dropbox_access_and_missing_pages_keep_their_error_kinds() {
    let url = Url::parse("https://www.dropbox.com/s/abc/data.csv?dl=1").unwrap();
    let (_, divert) = crate::source::fetch_plan(&url);
    for (status, kind) in [
        (401, Kind::AuthRequired),
        (403, Kind::AuthRequired),
        (404, Kind::ReferenceUnresolved),
        (410, Kind::ReferenceUnresolved),
    ] {
        let recorded = Recorded::default().on(
            url.as_str(),
            status,
            fixture_bytes("dropbox/download-page.html"),
        );
        let response = block_on(recorded.get(&url)).unwrap();
        let error = divert.unwrap()(&response.url, response.status, &response.text()).unwrap_err();
        assert_eq!(error.kind, kind);
        assert!(error.help.is_some());
    }
}

#[test]
fn unrelated_downloads_have_no_dropbox_page_handler() {
    let url = Url::parse("https://example.org/s/abc/data.csv?dl=1").unwrap();
    let (from, divert) = crate::source::fetch_plan(&url);
    assert_eq!(from, url);
    assert!(divert.is_none());
}
