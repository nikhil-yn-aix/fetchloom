use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const RELEASE: &str = "https://api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2";
const LATEST: &str = "https://api.github.com/repos/jqlang/jq/releases/latest";
const REPOSITORY: &str = "https://api.github.com/repos/jqlang/jq";
/// what github answers a request without a key once its 60 an hour are used up.
const LIMITED: &[u8] = br#"{"message":"API rate limit exceeded for 203.0.113.7. (But here's the good news: Authenticated requests get a higher rate limit. Check out the documentation for more details.)","documentation_url":"https://docs.github.com/rest/overview/resources-in-the-rest-api#rate-limiting"}"#;

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("github/release.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn release(text: &str) -> Release {
    Release::parse(text).unwrap()
}

#[test]
fn lists_a_releases_files_with_the_sha256_github_states() {
    let recorded = Recorded::default().fixture(RELEASE, "github/release.json");
    let listing = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "github:jqlang/jq@jq-1.8.2");
    assert_eq!(listing.title.as_deref(), Some("jq 1.8.2"));
    assert_eq!(listing.access, Access::Unknown);
    assert_eq!(listing.files.len(), 29);
    assert_eq!(
        listing.files[0],
        Remote {
            path: "jq-1.8.2.tar.gz".to_owned(),
            size: Some(1_959_950),
            sha256: Sha256::from_hex(
                "71b8d6e8f5fe81f6c6d0d110e3892251f6ce76ed095abd315e26e6e1193af3af"
            ),
            md5: None,
            url: Url::parse(
                "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-1.8.2.tar.gz"
            )
            .unwrap(),
            named_by_server: false,
            etag: None,
        }
    );
    assert!(listing.files.iter().all(|file| file.sha256.is_some()));
    assert_eq!(listing.files[28].path, "sha256sum.txt");
    assert_eq!(recorded.asked(), [RELEASE]);
}

#[test]
fn the_latest_release_pins_its_tag() {
    let recorded = Recorded::default()
        .fixture(LATEST, "github/release.json")
        .fixture(RELEASE, "github/release.json");
    let latest = block_on(release("jqlang/jq").resolve(&recorded)).unwrap();
    assert_eq!(latest.resolved.to_string(), "github:jqlang/jq@jq-1.8.2");
    let pinned = block_on(latest.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, latest.files);
    assert_eq!(recorded.asked(), [LATEST, RELEASE]);
}

#[test]
fn a_tag_with_a_slash_stays_one_tag() {
    let url = "https://api.github.com/repos/jqlang/jq/releases/tags/release%2F1.0";
    let body = changed(|value| value["tag_name"] = "release/1.0".into());
    let recorded = Recorded::default().on(url, 200, body);
    for text in [
        "github:jqlang/jq@release/1.0",
        "https://github.com/jqlang/jq/releases/tag/release/1.0",
        "https://github.com/jqlang/jq/releases/tag/release%2F1.0",
    ] {
        let reference = Reference::parse(text).unwrap().reference;
        assert_eq!(
            reference.to_string(),
            "github:jqlang/jq@release/1.0",
            "{text}"
        );
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved, reference, "{text}");
    }
}

/// an asset uploaded before github stated digests has none, and checks only its size until the
/// lock pins its bytes. a digest of another algorithm checks nothing fetchloom keeps.
#[test]
fn an_asset_without_a_sha256_is_listed_without_one() {
    let body = changed(|value| {
        value["assets"][0]["digest"] = serde_json::Value::Null;
        value["assets"][1]["digest"] = "sha512:abcd".into();
    });
    let recorded = Recorded::default().on(RELEASE, 200, body);
    let listing = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap();
    assert_eq!(listing.files[0].sha256, None);
    assert_eq!(listing.files[0].size, Some(1_959_950));
    assert_eq!(listing.files[1].sha256, None);
    assert!(listing.files[2].sha256.is_some());
}

#[test]
fn an_asset_still_uploading_is_listed_as_unavailable() {
    let body = changed(|value| value["assets"][0]["state"] = "starter".into());
    let recorded = Recorded::default().on(RELEASE, 200, body);
    let listing = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 28);
    assert_eq!(listing.unavailable.len(), 1);
    assert_eq!(listing.unavailable[0].path, "jq-1.8.2.tar.gz");
}

/// github answers 404 alike for a missing release and a missing or private repository, so the
/// repository's own answer tells them apart.
#[test]
fn a_missing_release_of_a_repository_github_shows_is_unresolved() {
    for (reference, url, message) in [
        (
            "jqlang/jq@jq-1.8.2",
            RELEASE,
            "https://github.com/jqlang/jq has no release at the tag jq-1.8.2",
        ),
        (
            "jqlang/jq",
            LATEST,
            "https://github.com/jqlang/jq has no published release",
        ),
    ] {
        let recorded = Recorded::default()
            .on(url, 404, fixture_bytes("github/missing.json"))
            .on(REPOSITORY, 200, b"{}".to_vec());
        let error = block_on(release(reference).resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved, "{reference}");
        assert_eq!(error.message, message);
        assert!(error.need.is_none());
        assert_eq!(recorded.asked(), [url, REPOSITORY]);
    }
}

#[test]
fn a_repository_github_does_not_show_may_be_private() {
    let recorded = Recorded::default()
        .on(RELEASE, 404, fixture_bytes("github/missing.json"))
        .on(REPOSITORY, 404, fixture_bytes("github/missing.json"));
    let error = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "github has no repository jqlang/jq, or it is private"
    );
    let need = error.need.unwrap();
    assert_eq!(need.account, Account::GitHub);
    assert_eq!(
        need.maybe_missing.as_deref(),
        Some("https://github.com/jqlang/jq")
    );
    let error = release("jqlang/jq").hidden(true);
    assert_eq!(
        error.message,
        "github has no repository jqlang/jq, or this key cannot see it"
    );
    assert!(error.need.is_none(), "a key went already");
}

/// without a key github allows 60 requests an hour; once they are used up, the error says so
/// and how a key lifts it, whether github answers 403 or 429.
#[test]
fn the_rate_limit_says_what_a_key_allows() {
    for status in [403, 429] {
        let recorded = Recorded::default().on(RELEASE, status, LIMITED.to_vec());
        let error = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRateLimited, "{status}");
        assert_eq!(
            error.message,
            "github allows 60 requests an hour without a key, and they are used up"
        );
        let help = error.help.as_deref().unwrap();
        assert!(help.contains("GH_TOKEN or GITHUB_TOKEN"), "{help}");
        assert!(help.contains("fetchloom auth login github"), "{help}");
        assert_eq!(error.need.unwrap().account, Account::GitHub);
        assert_eq!(recorded.asked(), [RELEASE]);
    }
    let keyed = limited(true);
    assert_eq!(keyed.message, "github's rate limit for this key is used up");
    assert!(keyed.need.is_none(), "a key went already");
}

#[test]
fn a_refusal_that_is_no_rate_limit_offers_the_github_account() {
    let recorded = Recorded::default().on(
        RELEASE,
        403,
        br#"{"message":"Resource protected by organization SAML enforcement."}"#.to_vec(),
    );
    let error = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(error.need.unwrap().account, Account::GitHub);
}

/// an answer fetchloom cannot trust is refused for its reason, with help.
#[test]
fn an_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 6] = [
        (
            "another tag",
            |value| value["tag_name"] = "jq-1.8.1".into(),
            "names another tag",
        ),
        (
            "no tag",
            |value| value["tag_name"] = "".into(),
            "names no tag",
        ),
        (
            "a download link on another host",
            |value| {
                value["assets"][0]["browser_download_url"] =
                    "https://example.org/jqlang/jq/releases/download/jq-1.8.2/jq-1.8.2.tar.gz"
                        .into();
            },
            "is not on github.com",
        ),
        (
            "a plain http download link",
            |value| {
                value["assets"][0]["browser_download_url"] =
                    "http://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-1.8.2.tar.gz".into();
            },
            "is not on github.com",
        ),
        (
            "a SHA-256 that is not hex",
            |value| value["assets"][0]["digest"] = "sha256:broken".into(),
            "is not hex",
        ),
        (
            "a digest that names no algorithm",
            |value| {
                value["assets"][0]["digest"] =
                    "71b8d6e8f5fe81f6c6d0d110e3892251f6ce76ed095abd315e26e6e1193af3af".into();
            },
            "names no algorithm",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = Recorded::default().on(RELEASE, 200, changed(change));
        let Err(error) = block_on(release("jqlang/jq@jq-1.8.2").resolve(&recorded)) else {
            panic!("{row}: the answer was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(error.help.is_some(), "{row}");
    }
}

/// a release's page and the latest release's are read as releases. every other github.com link
/// keeps what it did: a release file's own link and a source archive download as they are, and
/// a file's page goes to its raw bytes.
#[test]
fn release_pages_name_releases_and_other_links_stay_as_they_were() {
    for (text, expected) in [
        ("github:jqlang/jq@jq-1.8.2", "github:jqlang/jq@jq-1.8.2"),
        (
            "https://github.com/jqlang/jq/releases/tag/jq-1.8.2",
            "github:jqlang/jq@jq-1.8.2",
        ),
        (
            "https://github.com/jqlang/jq/releases/latest",
            "github:jqlang/jq",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap().reference;
        assert_eq!(parsed.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, parsed);
    }
    for text in [
        "github:jqlang",
        "github:jqlang/jq/extra",
        "github:jqlang/jq@",
        "github:jq lang/jq",
        "github:jqlang/jq@a tag",
        "github:",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    for link in [
        "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-amd64",
        "https://github.com/jqlang/jq/archive/refs/tags/jq-1.8.2.tar.gz",
        "https://github.com/jqlang/jq/releases",
    ] {
        assert_eq!(
            Reference::parse(link).unwrap().reference,
            Reference::Url(Url::parse(link).unwrap()),
            "{link}"
        );
    }
    assert_eq!(
        Reference::parse("https://github.com/jqlang/jq/blob/master/README.md")
            .unwrap()
            .reference
            .to_string(),
        "https://raw.githubusercontent.com/jqlang/jq/master/README.md"
    );
}

#[test]
#[ignore = "live: talks to api.github.com"]
fn live_github() {
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
    let reference = Reference::parse("github:jqlang/jq@jq-1.8.2")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "github: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 29);
    assert_eq!(listing.resolved.to_string(), "github:jqlang/jq@jq-1.8.2");
    assert!(listing.files.iter().all(|file| file.sha256.is_some()));
    assert_eq!(work.counts().requests, 1);
}
