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
    let crate::net::Fetch { from, divert, .. } = crate::source::fetch_plan(&url);
    let recorded = Recorded::default().fixture(from.as_str(), "dropbox/download-page.html");
    let response = block_on(recorded.get(&from)).unwrap();
    let error = divert.unwrap()(&response.url, response.status, &response.text()).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.message.contains("dropbox"));
    assert!(error.help.unwrap().contains("browser"));
}

/// each page dropbox answers a download with instead of the file keeps the kind its status
/// means and says why, and a status no case handles names it.
#[test]
fn each_dropbox_refusal_page_keeps_its_kind_and_says_why() {
    let url = Url::parse("https://www.dropbox.com/s/abc/data.csv?dl=1").unwrap();
    let divert = crate::source::fetch_plan(&url).divert;
    let refused = "dropbox refused access to the shared file or folder";
    let missing = "dropbox could not find the shared file or folder";
    for (status, kind, message) in [
        (401, Kind::AuthRequired, refused),
        (403, Kind::AuthRequired, refused),
        (404, Kind::ReferenceUnresolved, missing),
        (410, Kind::ReferenceUnresolved, missing),
        (
            418,
            Kind::NetworkStatus,
            "dropbox answered 418 instead of the shared file or folder",
        ),
    ] {
        let recorded = Recorded::default().on(
            url.as_str(),
            status,
            fixture_bytes("dropbox/download-page.html"),
        );
        let response = block_on(recorded.get(&url)).unwrap();
        let error = divert.unwrap()(&response.url, response.status, &response.text()).unwrap_err();
        assert_eq!(error.kind, kind, "{status}");
        assert_eq!(error.message, message);
        assert!(error.help.is_some());
    }
}

#[test]
fn unrelated_downloads_have_no_dropbox_page_handler() {
    let url = Url::parse("https://example.org/s/abc/data.csv?dl=1").unwrap();
    let crate::net::Fetch { from, divert, .. } = crate::source::fetch_plan(&url);
    assert_eq!(from, url);
    assert!(divert.is_none());
}

/// the shared pdf the dropbox page shows, read for real: the link names the file, dropbox sends
/// it, and the bytes are the ones pinned here from a download on 2026-11-13, 775,543 bytes.
#[test]
#[ignore = "live: talks to www.dropbox.com and dl.dropboxusercontent.com"]
fn live_dropbox() {
    let link = "https://www.dropbox.com/s/3y96pnutxum3p4v/0569.pdf?dl=1";
    let sha256 = crate::digest::Sha256::from_hex(
        "86c4de5593a6058b67cefe76ccff78d98ba20fe1606cbd7d62b11f77b39aee59",
    );
    let work = std::sync::Arc::new(crate::work::Work::default());
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        },
        work.clone(),
        std::sync::Arc::default(),
        std::sync::Arc::new(|_| {}),
    );
    let start = std::time::Instant::now();
    let reference = Reference::parse(link).unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    assert_eq!(listing.resolved.to_string(), link);
    let [remote] = listing.files.as_slice() else {
        panic!("one file: {listing:?}");
    };
    assert_eq!(
        (
            remote.path.as_str(),
            remote.named_by_server,
            remote.size,
            remote.sha256,
            remote.md5
        ),
        ("0569.pdf", true, None, None, None)
    );
    assert_eq!(remote.url.as_str(), link);
    let folder = tempfile::tempdir().unwrap();
    let cache = crate::cache::Cache::open(folder.path().join("cache")).unwrap();
    let fetch = crate::net::Fetch {
        expect: crate::net::Expect {
            sha256,
            md5: None,
            size: Some(775_543),
        },
        ..crate::source::fetch_plan(&remote.url)
    };
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    eprintln!(
        "dropbox: {} as {:?}, {} bytes, {}, {} requests, {:?}",
        listing.resolved,
        fetched.name,
        fetched.size,
        fetched.digest.short(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(fetched.name.as_deref(), Some("0569.pdf"));
    assert_eq!((Some(fetched.digest), fetched.size), (sha256, 775_543));
}
