use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const RELEASE: &str =
    "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/releases/v19.3.6";
const LATEST: &str =
    "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/releases/permalink/latest";

fn changed(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("gitlab/release.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn release(text: &str) -> Release {
    Release::parse(text).unwrap()
}

#[test]
fn lists_a_releases_links_at_the_paths_the_release_gives_them() {
    let recorded = Recorded::default().fixture(RELEASE, "gitlab/release.json");
    let listing = block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "gitlab:gitlab-org/gitlab-runner@v19.3.6"
    );
    assert_eq!(listing.title.as_deref(), Some("v19.3.6"));
    assert_eq!(listing.access, Access::Unknown);
    assert_eq!(listing.files.len(), 38);
    assert_eq!(
        listing.files[2],
        Remote {
            path: "release.sha256".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: Url::parse(
                "https://gitlab-runner-downloads.s3.amazonaws.com/v19.3.6/release.sha256"
            )
            .unwrap(),
            named_by_server: false,
            etag: None,
        }
    );
    assert_eq!(
        listing.files[3].path,
        "packages/rpm/gitlab-runner-helper-images.rpm"
    );
    assert!(
        listing
            .files
            .iter()
            .all(|file| file.url.host_str() == Some("gitlab-runner-downloads.s3.amazonaws.com")),
        "each file comes from its link's own address, never from the page gitlab.com shows a program in its place"
    );
    assert_eq!(recorded.asked(), [RELEASE]);
}

#[test]
fn the_newest_release_pins_its_tag() {
    let recorded = Recorded::default()
        .fixture(LATEST, "gitlab/release.json")
        .redirected(LATEST, RELEASE)
        .fixture(RELEASE, "gitlab/release.json");
    let latest = block_on(release("gitlab-org/gitlab-runner").resolve(&recorded)).unwrap();
    assert_eq!(
        latest.resolved.to_string(),
        "gitlab:gitlab-org/gitlab-runner@v19.3.6"
    );
    let pinned = block_on(latest.resolved.resolve(&recorded)).unwrap();
    assert_eq!(pinned.files, latest.files);
    assert_eq!(recorded.asked(), [LATEST, RELEASE]);
}

#[test]
fn a_project_in_nested_groups_is_one_encoded_path() {
    let url = "https://gitlab.com/api/v4/projects/group%2Fsub%2Ftools/releases/v1";
    let body = changed(|value| {
        value["tag_name"] = "v1".into();
        value["assets"]["count"] = 1.into();
        value["assets"]["sources"] = serde_json::json!([]);
        value["assets"]["links"] = serde_json::json!([{
            "name": "data",
            "url": "https://gitlab.com/api/v4/projects/group%2Fsub%2Ftools/packages/generic/tools/v1/rows.csv",
            "direct_asset_url": "https://gitlab.com/group/sub/tools/-/releases/v1/downloads/data/rows%20one.csv",
        }]);
    });
    let recorded = Recorded::default().on(url, 200, body);
    for text in [
        "gitlab:group/sub/tools@v1",
        "https://gitlab.com/group/sub/tools/-/releases/v1",
    ] {
        let reference = Reference::parse(text).unwrap().reference;
        assert_eq!(reference.to_string(), "gitlab:group/sub/tools@v1", "{text}");
        let listing = block_on(reference.resolve(&recorded)).unwrap();
        assert_eq!(listing.files[0].path, "data/rows one.csv");
    }
}

#[test]
fn a_link_without_a_download_link_is_listed_as_unavailable() {
    let body = changed(|value| {
        value["assets"]["links"][0]
            .as_object_mut()
            .unwrap()
            .remove("direct_asset_url");
    });
    let recorded = Recorded::default().on(RELEASE, 200, body);
    let listing = block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 37);
    assert_eq!(listing.unavailable[0].path, "other release artifacts");
}

/// a link whose own address is not https, or holds a secret, is listed as one fetchloom cannot
/// download, so the rest of the release still can be, and the secret is never shown.
#[test]
fn a_link_that_is_not_https_or_holds_a_secret_is_unavailable() {
    let body = changed(|value| {
        value["assets"]["links"][0]["url"] = "http://downloads.example.org/index.html".into();
        value["assets"]["links"][1]["url"] =
            "https://downloads.example.org/release.sha256.asc?X-Amz-Signature=abcdef".into();
        value["assets"]["links"][2]["url"] =
            "https://ada:pw@downloads.example.org/release.sha256".into();
    });
    let recorded = Recorded::default().on(RELEASE, 200, body);
    let listing = block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 35);
    let unavailable: Vec<_> = listing
        .unavailable
        .iter()
        .map(|entry| (entry.path.as_str(), entry.reason.as_str()))
        .collect();
    assert_eq!(
        unavailable,
        [
            ("index.html", "its address is not https"),
            (
                "release.sha256.asc",
                "its address holds a secret, which fetchloom does not keep"
            ),
            (
                "release.sha256",
                "its address holds a secret, which fetchloom does not keep"
            ),
        ]
    );
}

/// gitlab's message tells a missing release from a project it does not show, which may be
/// private.
#[test]
fn a_missing_release_and_a_hidden_project_say_which_they_are() {
    let recorded = Recorded::default().on(RELEASE, 404, fixture_bytes("gitlab/missing.json"));
    let error =
        block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "https://gitlab.com/gitlab-org/gitlab-runner has no release at the tag v19.3.6"
    );

    let recorded = Recorded::default().on(RELEASE, 404, fixture_bytes("gitlab/no-project.json"));
    let error =
        block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "gitlab.com has no project gitlab-org/gitlab-runner, or it is private"
    );
    let need = error.need.unwrap();
    assert_eq!(need.account, Account::GitLab);
    assert_eq!(
        need.maybe_missing.as_deref(),
        Some("https://gitlab.com/gitlab-org/gitlab-runner")
    );
    let keyed = release("gitlab-org/gitlab-runner").hidden(true);
    assert!(keyed.message.ends_with("or this key cannot see it"));
    assert!(keyed.need.is_none(), "a key went already");
}

#[test]
fn a_refused_key_offers_the_gitlab_account() {
    let recorded =
        Recorded::default().on(RELEASE, 401, br#"{"message":"401 Unauthorized"}"#.to_vec());
    let error =
        block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(error.need.unwrap().account, Account::GitLab);
}

#[test]
fn an_answer_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 6] = [
        (
            "another tag",
            |value| value["tag_name"] = "v19.3.5".into(),
            "names another tag",
        ),
        (
            "fewer assets than it counts",
            |value| value["assets"]["count"] = 43.into(),
            "lists 42 of the 43 assets it counts",
        ),
        (
            "a download link on another host",
            |value| {
                value["assets"]["links"][0]["direct_asset_url"] =
                    "https://example.org/gitlab-org/gitlab-runner/-/releases/v19.3.6/downloads/index.html"
                        .into();
            },
            "is not this release's on gitlab.com",
        ),
        (
            "another project's download link",
            |value| {
                value["assets"]["links"][0]["direct_asset_url"] =
                    "https://gitlab.com/someone/else/-/releases/v19.3.6/downloads/index.html"
                        .into();
            },
            "is not this release's on gitlab.com",
        ),
        (
            "a download link with a query",
            |value| {
                value["assets"]["links"][0]["direct_asset_url"] =
                    "https://gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6/downloads/index.html?private_token=secret"
                        .into();
            },
            "is not this release's on gitlab.com",
        ),
        (
            "a download link that names no file",
            |value| {
                value["assets"]["links"][0]["direct_asset_url"] =
                    "https://gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6/downloads/"
                        .into();
            },
            "is not this release's on gitlab.com",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = Recorded::default().on(RELEASE, 200, changed(change));
        let Err(error) = block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded))
        else {
            panic!("{row}: the answer was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(
            !error.message.contains("secret"),
            "{row}: {}",
            error.message
        );
        assert!(error.help.is_some(), "{row}");
    }
}

/// as a file's page, which goes to its raw bytes.
#[test]
fn release_pages_name_releases_and_other_links_stay_as_they_were() {
    for (text, expected) in [
        (
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
        ),
        (
            "https://gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6",
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
        ),
        (
            "https://gitlab.com/gitlab-org/gitlab-runner/-/releases/permalink/latest",
            "gitlab:gitlab-org/gitlab-runner",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap().reference;
        assert_eq!(parsed.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, parsed);
    }
    for text in [
        "gitlab:gitlab-runner",
        "gitlab:gitlab-org//gitlab-runner",
        "gitlab:-org/gitlab-runner",
        "gitlab:gitlab-org/gitlab-runner@",
        "gitlab:gitlab-org/gitlab runner",
        "gitlab:",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let releases = "https://gitlab.com/gitlab-org/gitlab-runner/-/releases";
    assert_eq!(
        Reference::parse(releases).unwrap().reference,
        Reference::Url(Url::parse(releases).unwrap())
    );
    assert_eq!(
        Reference::parse("https://gitlab.com/gitlab-org/gitlab-runner/-/blob/main/README.md")
            .unwrap()
            .reference
            .to_string(),
        "https://gitlab.com/gitlab-org/gitlab-runner/-/raw/main/README.md"
    );
}

#[test]
#[ignore = "live: talks to gitlab.com"]
fn live_gitlab() {
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
    let reference = Reference::parse("gitlab:gitlab-org/gitlab-runner@v19.3.6")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "gitlab: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 38);
    assert_eq!(
        listing.resolved.to_string(),
        "gitlab:gitlab-org/gitlab-runner@v19.3.6"
    );
    assert_eq!(work.counts().requests, 1);
}
