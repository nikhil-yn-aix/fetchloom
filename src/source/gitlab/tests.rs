use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};
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
            blob: None,
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

/// a tag gitlab names that no reference can hold is refused, never pinned where it would not
/// read back.
#[test]
fn a_tag_no_reference_can_hold_is_refused() {
    let spaced = changed(|release| release["tag_name"] = "v19 3.6".into());
    let recorded = Recorded::default().on(LATEST, 200, spaced);
    let error = block_on(release("gitlab-org/gitlab-runner").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains("no tag a reference can hold"),
        "{}",
        error.message
    );
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
/// private. a project without a release is pinned by its own files, which the help names.
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
    let releases = "check the releases at https://gitlab.com/gitlab-org/gitlab-runner/-/releases; a tag without a release has no files to list";
    assert_eq!(
        error.help.as_deref(),
        Some(format!("{releases}. to pin the project's own files at that tag instead, run `fetchloom add https://gitlab.com/gitlab-org/gitlab-runner/-/tree/v19.3.6`").as_str())
    );
    let recorded = Recorded::default().on(LATEST, 404, fixture_bytes("gitlab/missing.json"));
    let error = block_on(release("gitlab-org/gitlab-runner").resolve(&recorded)).unwrap_err();
    assert_eq!(
        error.message,
        "https://gitlab.com/gitlab-org/gitlab-runner has no release"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(format!("{releases}. to pin the project's own files at its default branch instead, run `fetchloom add https://gitlab.com/gitlab-org/gitlab-runner`").as_str())
    );
    let elsewhere = Release::parse("//code.videolan.org/videolan/dav1d").unwrap();
    assert_eq!(
        elsewhere.no_release().help.as_deref(),
        Some(
            "check the releases at https://code.videolan.org/videolan/dav1d/-/releases; a tag without a release has no files to list. to pin the project's own files at its default branch instead, run `fetchloom add https://code.videolan.org/videolan/dav1d/-/tree/HEAD`"
        )
    );

    let recorded = Recorded::default().on(RELEASE, 404, fixture_bytes("gitlab/no-project.json"));
    let error =
        block_on(release("gitlab-org/gitlab-runner@v19.3.6").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
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
    let keyed = release("gitlab-org/gitlab-runner")
        .project
        .hidden(true, "releases");
    assert_eq!(keyed.kind, Kind::AuthRequired, "the key may lack access");
    assert!(keyed.message.ends_with("or this key cannot see it"));
    assert!(keyed.need.is_none(), "a key went already");
}

/// a project gitlab.com shows no key it was sent names that key, as hugging face's refusal does,
/// so a person checks the key as well as the path.
#[test]
fn a_hidden_project_names_the_key_that_went() {
    crate::source::TEST_VARIABLES.with_borrow_mut(|set| set.push(("GITLAB_TOKEN", "glpat-test")));
    let error = release("gitlab-org/gitlab-runner")
        .project
        .hidden(true, "releases");
    crate::source::TEST_VARIABLES.with_borrow_mut(Vec::clear);
    assert_eq!(
        error.message,
        "gitlab.com has no project gitlab-org/gitlab-runner, or this key cannot see it"
    );
    assert_eq!(
        error.details,
        ["the key in GITLAB_TOKEN went with the request"]
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "check the path at https://gitlab.com/gitlab-org/gitlab-runner. a private project's releases need a key that can read it: if the key in GITLAB_TOKEN is old or wrong, set GITLAB_TOKEN to a current one from https://gitlab.com/-/user_settings/personal_access_tokens"
        )
    );
    assert!(error.need.is_none(), "a key went already");
}

/// a rate limit on a request without a key offers the key that lifts it: gitlab.com's account,
/// or a key saved for another gitlab server, known by its raw links and its api; any other host's
/// limit stays as the network said it.
#[test]
fn a_keyless_rate_limit_offers_the_key_of_the_gitlab_that_set_it() {
    let limited = || Error::new(Kind::NetworkRateLimited, "asked to wait").help("later");
    let at = |link: &str| keyless_limit(&Url::parse(link).unwrap(), limited());
    let com =
        at("https://gitlab.com/lab/survey/-/raw/0bda0ce801be0fa2f464ff845a9d5ceae99aad7d/a.csv");
    assert_eq!(com.need.unwrap().account, Account::GitLab);
    for link in [
        "https://code.example.org/lab/survey/-/raw/main/a.csv",
        "https://code.example.org/api/v4/projects/lab%2Fsurvey/repository/commits/main",
    ] {
        let error = at(link);
        let need = error.need.unwrap();
        assert_eq!(
            need.account,
            Account::Login("code.example.org".to_owned()),
            "{link}"
        );
        assert!(need.site, "{link}");
        assert!(
            error
                .help
                .unwrap()
                .contains("`fetchloom auth login code.example.org --token`"),
            "{link}"
        );
    }
    let other = at("https://example.org/data/a.csv");
    assert!(other.need.is_none());
    assert_eq!(other.help.as_deref(), Some("later"));
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
    let rows: [(&str, Change, &str); 7] = [
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
            "none of the one asset it counts",
            |value| {
                value["assets"]["count"] = 1.into();
                value["assets"]["links"] = serde_json::json!([]);
                value["assets"]["sources"] = serde_json::json!([]);
            },
            "lists 0 of the 1 asset it counts",
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

/// a file's page still goes to its raw bytes.
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

const DAV1D: &str = "https://code.videolan.org/api/v4/projects/videolan%2Fdav1d/releases/1.5.3";

/// a release on another gitlab server is listed from that server's own api, and a link it gives
/// no path in the release, as dav1d's, takes the name its address ends with.
#[test]
fn a_release_on_another_server_is_listed_from_its_own_api() {
    let recorded = Recorded::default().fixture(DAV1D, "gitlab/dav1d-release.json");
    let reference = Reference::parse("gitlab://code.videolan.org/videolan/dav1d@1.5.3")
        .unwrap()
        .reference;
    assert_eq!(
        reference.written_origin(),
        Some(Url::parse("https://code.videolan.org/").unwrap().origin())
    );
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "gitlab://code.videolan.org/videolan/dav1d@1.5.3"
    );
    assert_eq!(listing.title.as_deref(), Some("dav1d 1.5.3"));
    assert_eq!(
        listing.files,
        [Remote {
            path: "dav1d-1.5.3.tar.xz".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: Url::parse(
                "https://downloads.videolan.org/videolan/dav1d/1.5.3/dav1d-1.5.3.tar.xz"
            )
            .unwrap(),
            named_by_server: false,
            etag: None,
            blob: None,
        }]
    );
    assert_eq!(recorded.asked(), [DAV1D]);
}

/// another server's reference is marked by `//` before its host, which no project path on
/// gitlab.com can start with, so a group named like a host stays gitlab.com's; a pasted release
/// page on any https host is read as that server's.
#[test]
fn another_servers_references_are_marked_by_their_host() {
    for (text, expected, origin) in [
        (
            "gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            "gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            Some("https://code.videolan.org"),
        ),
        (
            "gitlab://Code.VideoLAN.org/videolan/dav1d",
            "gitlab://code.videolan.org/videolan/dav1d",
            Some("https://code.videolan.org"),
        ),
        (
            "gitlab://gitlab.example.org:8443/group/sub/tools@v1",
            "gitlab://gitlab.example.org:8443/group/sub/tools@v1",
            Some("https://gitlab.example.org:8443"),
        ),
        (
            "gitlab://gitlab.example.org:443/group/tools@v1",
            "gitlab://gitlab.example.org/group/tools@v1",
            Some("https://gitlab.example.org"),
        ),
        (
            "gitlab://gitlab.com/gitlab-org/gitlab-runner@v19.3.6",
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            None,
        ),
        (
            "gitlab:code.videolan.org/videolan/dav1d",
            "gitlab:code.videolan.org/videolan/dav1d",
            None,
        ),
        (
            "https://code.videolan.org/videolan/dav1d/-/releases/1.5.3",
            "gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            Some("https://code.videolan.org"),
        ),
        (
            "https://gitlab.example.org:8443/group/tools/-/releases/permalink/latest",
            "gitlab://gitlab.example.org:8443/group/tools",
            Some("https://gitlab.example.org:8443"),
        ),
    ] {
        let reference = Reference::parse(text).unwrap().reference;
        assert_eq!(reference.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, reference);
        assert_eq!(
            reference.written_origin(),
            origin.map(|origin| Url::parse(origin).unwrap().origin()),
            "{text}"
        );
    }
    for text in [
        "gitlab://",
        "gitlab://code.videolan.org",
        "gitlab://code.videolan.org/dav1d",
        "gitlab://ada@code.videolan.org/videolan/dav1d",
        "gitlab:///videolan/dav1d",
        "gitlab://code.videolan.org//dav1d",
        "gitlab://code.videolan.org/videolan/dav1d@",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    let plain = "http://gitlab.example.org/group/tools/-/releases/v1";
    assert_eq!(
        Reference::parse(plain).unwrap().reference,
        Reference::Url(Url::parse(plain).unwrap())
    );
}

/// another server's refusal names the sign-in that saves a key for that host alone, never
/// gitlab.com's.
#[test]
fn another_servers_refusal_names_its_own_sign_in() {
    let reference = Reference::parse("gitlab://code.videolan.org/videolan/dav1d@1.5.3")
        .unwrap()
        .reference;
    // a project no key went for may be a typo; a refusal says nothing of the name
    for (status, body, message, kind) in [
        (
            404,
            fixture_bytes("gitlab/no-project.json"),
            "code.videolan.org has no project videolan/dav1d, or it is private",
            Kind::ReferenceUnresolved,
        ),
        (
            401,
            br#"{"message":"401 Unauthorized"}"#.to_vec(),
            "code.videolan.org refused access to gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            Kind::AuthRequired,
        ),
    ] {
        let recorded = Recorded::default().on(DAV1D, status, body);
        let error = block_on(reference.resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, kind, "{status}");
        assert_eq!(error.message, message);
        let help = error.help.unwrap_or_default();
        assert!(
            help.contains("`fetchloom auth login code.videolan.org --token`"),
            "{status}: {help}"
        );
        assert!(!help.contains("GITLAB_TOKEN"), "{status}: {help}");
        let need = error.need.unwrap();
        assert_eq!(need.account, Account::Login("code.videolan.org".to_owned()));
        assert!(need.site, "{status}");
    }
}

#[test]
#[ignore = "live: talks to gitlab.com"]
fn live_gitlab() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("gitlab:gitlab-org/gitlab-runner@v19.3.6")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "gitlab: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 38);
    assert_eq!(
        listing.resolved.to_string(),
        "gitlab:gitlab-org/gitlab-runner@v19.3.6"
    );
    assert_eq!(asked(&work), 1);
}

#[test]
#[ignore = "live: talks to code.videolan.org"]
fn live_gitlab_server() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("gitlab://code.videolan.org/videolan/dav1d@1.5.3")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "gitlab server: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
        start.elapsed()
    );
    assert_eq!(listing.files[0].path, "dav1d-1.5.3.tar.xz");
    assert_eq!(asked(&work), 1);
}
