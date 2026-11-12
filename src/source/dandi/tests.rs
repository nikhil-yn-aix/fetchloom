use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const SET: &str = "https://api.dandiarchive.org/api/dandisets/000027/";
const VERSION: &str = "https://api.dandiarchive.org/api/dandisets/000027/versions/0.210831.2033";

fn assets(version: &str) -> String {
    format!("{version}/assets/?metadata=true&order=path&page_size=1000")
}

fn listed(reference: &str, recorded: &Recorded) -> Result<Listing, Error> {
    block_on(Reference::parse(reference)?.reference.resolve(recorded))
}

/// the newest published version, pinned, with each asset's size, the SHA-256 dandi states and
/// the blob's own link on S3. dandi's etag is S3's multipart tag, so no MD5 is taken from it.
#[test]
fn a_published_version_lists_its_assets_with_the_sha256_dandi_states() {
    let recorded = Recorded::default()
        .fixture(SET, "dandi/set.json")
        .fixture(&format!("{VERSION}/info/"), "dandi/info.json")
        .fixture(&assets(VERSION), "dandi/assets.json");
    let listing = listed("dandi:000027", &recorded).unwrap();
    assert_eq!(listing.resolved.to_string(), "dandi:000027@0.210831.2033");
    assert_eq!(
        listing.title.as_deref(),
        Some("Test dataset for testing dandi-cli.")
    );
    assert_eq!(listing.license.as_deref(), Some("CC0-1.0"));
    assert_eq!(
        listing.doi.as_deref(),
        Some("10.48324/dandi.000027/0.210831.2033")
    );
    assert_eq!(listing.access, Access::Public);
    let [file] = listing.files.as_slice() else {
        panic!("{:?}", listing.files);
    };
    assert_eq!(file.path, "sub-RAT123/sub-RAT123.nwb");
    assert_eq!(file.size, Some(18_792));
    assert_eq!(
        file.sha256,
        Sha256::from_hex("1a765509384ea96b7b12136353d9c5b94f23d764ad0431e049197f7875eb352c")
    );
    assert_eq!(file.md5, None);
    assert_eq!(
        file.url.as_str(),
        "https://dandiarchive.s3.amazonaws.com/blobs/2db/af0/2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb3826"
    );
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    assert_eq!(
        recorded.asked(),
        [SET.to_owned(), format!("{VERSION}/info/"), assets(VERSION)]
    );
}

/// a pinned version asks nothing of the dandiset itself, and assets that come in pages are
/// read by each page's next link.
#[test]
fn a_pinned_version_follows_each_page_of_its_assets() {
    let version = "https://api.dandiarchive.org/api/dandisets/001512/versions/0.250624.1820";
    let second = format!("{version}/assets/?metadata=true&order=path&page=2&page_size=2");
    let recorded = Recorded::default()
        .fixture(&format!("{version}/info/"), "dandi/paged-info.json")
        .fixture(&assets(version), "dandi/page-1.json")
        .fixture(&second, "dandi/page-2.json");
    let listing = listed("dandi:001512@0.250624.1820", &recorded).unwrap();
    let paths: Vec<&str> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "sub-mouse6/sub-mouse6_ses-20241205_ecephys.nwb",
            "sub-mouse9/sub-mouse9_ses-20250530_ecephys.nwb",
            "sub-mouse9/sub-mouse9_ses-20250606_ecephys.nwb",
        ]
    );
    assert!(listing.files.iter().all(|file| file.sha256.is_some()));
    assert_eq!(
        recorded.asked(),
        [format!("{version}/info/"), assets(version), second]
    );
}

/// a dandiset dandi never published is read at its draft, which can change, and the listing
/// says so.
#[test]
fn a_dandiset_never_published_is_read_at_its_draft_and_says_so() {
    let draft = "https://api.dandiarchive.org/api/dandisets/000012/versions/draft";
    let recorded = Recorded::default()
        .fixture(
            "https://api.dandiarchive.org/api/dandisets/000012/",
            "dandi/draft-set.json",
        )
        .on(
            &format!("{draft}/info/"),
            200,
            r#"{"version":"draft","name":"a draft","dandiset":{"embargo_status":"OPEN"}}"#,
        )
        .fixture(&assets(draft), "dandi/assets.json");
    let listing = listed("dandi:000012", &recorded).unwrap();
    assert_eq!(listing.resolved.to_string(), "dandi:000012@draft");
    assert_eq!(
        listing.warnings,
        [
            "dandi:000012 has no published version, so its draft is read. a draft can change: data.lock's hashes name the bytes"
        ]
    );
}

/// a zarr asset is a folder of chunks with no bytes of its own, so it is listed as unavailable
/// with why.
#[test]
fn a_zarr_asset_is_listed_without_bytes_and_says_why() {
    let version = "https://api.dandiarchive.org/api/dandisets/000108/versions/draft";
    let recorded = Recorded::default()
        .on(
            &format!("{version}/info/"),
            200,
            r#"{"version":"draft","name":"zarr","dandiset":{"embargo_status":"OPEN"}}"#,
        )
        .fixture(&assets(version), "dandi/zarr.json");
    let listing = listed("dandi:000108@draft", &recorded).unwrap();
    assert_eq!(listing.files, []);
    let [entry] = listing.unavailable.as_slice() else {
        panic!("{:?}", listing.unavailable);
    };
    assert!(
        entry.path.starts_with("sub-MITU01/")
            && std::path::Path::new(&entry.path)
                .extension()
                .is_some_and(|extension| extension == "zarr"),
        "{}",
        entry.path
    );
    assert_eq!(
        entry.reason,
        "it is a zarr folder of chunks, which fetchloom does not download"
    );
}

/// dandi shows an embargoed dandiset to nobody without an account, so its refusal, and an
/// embargo the answer states, are told as an embargo. one leaving its embargo says to wait.
#[test]
fn an_embargoed_dandiset_is_refused_as_such() {
    let refused = |recorded: Recorded, reference: &str| listed(reference, &recorded).unwrap_err();
    let hidden = refused(
        Recorded::default().on(SET, 401, fixture_bytes("dandi/embargoed.json")),
        "dandi:000027",
    );
    assert_eq!(hidden.kind, Kind::AuthRequired);
    assert_eq!(
        hidden.message,
        "dandi:000027 is embargoed: dandi shows it only to the people its owners let in, and fetchloom does not sign in to dandi"
    );
    for (status, said) in [
        ("EMBARGOED", "is embargoed"),
        ("UNEMBARGOING", "is leaving its embargo"),
    ] {
        let stated = refused(
            Recorded::default().on(
                &format!("{VERSION}/info/"),
                200,
                format!(
                    r#"{{"version":"0.210831.2033","dandiset":{{"embargo_status":"{status}"}}}}"#
                ),
            ),
            "dandi:000027@0.210831.2033",
        );
        assert_eq!(stated.kind, Kind::AuthRequired, "{status}");
        assert!(stated.message.contains(said), "{}", stated.message);
    }
}

/// a dandiset that does not exist, or a version it does not have, is unresolved, and the help
/// says where to check.
#[test]
fn a_missing_dandiset_or_version_is_unresolved() {
    let missing = listed(
        "dandi:000027",
        &Recorded::default().on(SET, 404, fixture_bytes("dandi/missing.json")),
    )
    .unwrap_err();
    assert_eq!(missing.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        missing.message,
        "dandi:000027 does not exist, or is not public"
    );
    let unversioned = listed(
        "dandi:000027@0.200101.0000",
        &Recorded::default().on(
            "https://api.dandiarchive.org/api/dandisets/000027/versions/0.200101.0000/info/",
            404,
            "<h1>Not Found</h1>",
        ),
    )
    .unwrap_err();
    assert_eq!(unversioned.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        unversioned.message,
        "dandi:000027@0.200101.0000 does not exist"
    );
    assert!(
        unversioned
            .help
            .as_deref()
            .is_some_and(|help| help.contains("https://dandiarchive.org/dandiset/000027")),
        "{:?}",
        unversioned.help
    );
}

/// an answer that names another version, a page that leads off this version's listing or back
/// to itself, and an asset that is neither a blob nor a zarr, or whose SHA-256 is no hex, are
/// refused with why.
#[test]
fn a_listing_that_cannot_be_trusted_says_why() {
    let info = format!("{VERSION}/info/");
    let page =
        |next: &str, asset: &str| format!(r#"{{"count":1,"next":{next},"results":[{asset}]}}"#);
    let blob = r#"{"asset_id":"1c095f5f-d1e2-45db-b807-fdcfea08c6de","blob":"2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb3826","zarr":null,"path":"a.nwb","size":1}"#;
    let ours = r#"{"version":"0.210831.2033","dandiset":{"embargo_status":"OPEN"}}"#;
    for (answer, listing, said) in [
        (
            r#"{"version":"0.210831.2034"}"#,
            page("null", blob),
            "the answer names another version",
        ),
        (
            ours,
            page(
                "\"https://elsewhere.example/api/dandisets/000027/versions/0.210831.2033/assets/\"",
                blob,
            ),
            "a page's next page is not one of this version's",
        ),
        (
            ours,
            page(&format!("\"{}\"", assets(VERSION)), blob),
            "a page names an earlier page as the next",
        ),
        (
            ours,
            page(
                "null",
                r#"{"asset_id":"1c095f5f-d1e2-45db-b807-fdcfea08c6de","blob":null,"zarr":null,"path":"a.nwb","size":1}"#,
            ),
            "\"a.nwb\" is neither a blob nor a zarr",
        ),
        (
            ours,
            page(
                "null",
                r#"{"asset_id":"1c095f5f-d1e2-45db-b807-fdcfea08c6de","blob":"2dbaf0fd-5003-4a0a-b4c0-bc8cdbdb3826","zarr":null,"path":"a.nwb","size":1,"metadata":{"digest":{"dandi:sha2-256":"not hex"}}}"#,
            ),
            "the SHA-256 of \"a.nwb\" is not hex",
        ),
    ] {
        let recorded =
            Recorded::default()
                .on(&info, 200, answer)
                .on(&assets(VERSION), 200, listing);
        let error = listed("dandi:000027@0.210831.2033", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{said}");
        assert_eq!(
            error.message,
            format!("dandi:000027@0.210831.2033 sent an invalid listing: {said}")
        );
    }
}

#[test]
#[ignore = "live: talks to api.dandiarchive.org"]
fn live_dandi() {
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
    let reference = Reference::parse("dandi:000027").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "dandi: {} at {}, {} files, {} requests, {:?}",
        listing.title.as_deref().unwrap_or_default(),
        listing.resolved,
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 1);
    assert_eq!(
        listing.files[0].sha256,
        Sha256::from_hex("1a765509384ea96b7b12136353d9c5b94f23d764ad0431e049197f7875eb352c")
    );
    assert_eq!(work.counts().requests, 3);
}
