use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

/// gdown's test folder `deep_folder`, with `folder_1/folder_1_1` and `folder_2` inside.
const DEEP: &str = "1oow464Z4Et95RAwtRZn6dLOFb_YYpLwL";
const FOLDER_1: &str = "1TMZoSAu4ecs_Q3GEEWfiyrQIPtj3DJAC";
const FOLDER_1_1: &str = "1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U";
const FOLDER_2: &str = "1RRfB3nFcMtg_ibfCjoU6P6DbsrXkwnPF";

fn page(id: &str) -> String {
    format!("https://drive.google.com/embeddedfolderview?id={id}")
}

fn folder(id: &str) -> Item {
    Item::parse(&format!("folders/{id}")).unwrap()
}

/// drive as it answered for `deep_folder` and each folder in it.
fn deep() -> Recorded {
    Recorded::default()
        .fixture(&page(DEEP), "drive/folder-deep.html")
        .fixture(&page(FOLDER_1), "drive/folder-deep-1.html")
        .fixture(&page(FOLDER_2), "drive/folder-deep-2.html")
        .fixture(&page(FOLDER_1_1), "drive/folder-deep-1-1.html")
}

fn uc(id: &str) -> Url {
    Url::parse(&format!("https://drive.google.com/uc?id={id}")).unwrap()
}

#[test]
fn a_folder_lists_the_files_of_every_folder_inside_it() {
    let recorded = deep();
    let listing = block_on(folder(DEEP).resolve(&recorded)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:folders/{DEEP}")
    );
    assert_eq!(listing.title.as_deref(), Some("deep_folder"));
    let file = |path: &str, id: &str| Remote {
        path: path.to_owned(),
        size: None,
        sha256: None,
        md5: None,
        url: uc(id),
        named_by_server: false,
    };
    assert_eq!(
        listing.files,
        [
            file("file.txt", "15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--"),
            file("folder_1/file.txt", "1T8RH3_FJMzShUfibltqBLCYvQN3QYWFG"),
            file("folder_2/file.txt", "1ioOMsmyR9HRWvwTUFSPb__MoOs4wE42v"),
            file(
                "folder_1/folder_1_1/file.txt",
                "1eGGJOMWkEZCzfp5rT4fbZ3-lJ5ShDUST"
            ),
        ]
    );
    assert_eq!(
        recorded.asked(),
        [page(DEEP), page(FOLDER_1), page(FOLDER_2), page(FOLDER_1_1)]
    );
}

#[test]
fn a_file_shared_before_2021_keeps_its_resource_key() {
    let id = "1LufWJZBSP-qij-4rC_F8BPpf_JFxW2ku";
    let recorded = Recorded::default().fixture(&page(id), "drive/folder-keyed.html");
    let listing = block_on(folder(id).resolve(&recorded)).unwrap();
    let urls: Vec<(&str, &str)> = listing
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.url.as_str()))
        .collect();
    assert_eq!(
        urls,
        [
            (
                "gdown.pptx",
                "https://drive.google.com/uc?id=15umvZKlsJ3094HNg5S4vJsIhxcFlyTeK"
            ),
            (
                "spam.txt",
                "https://drive.google.com/uc?id=0B9P1L--7Wd2vU3VUVlFnbTgtS2c&resourcekey=0-WWs_XOSctfaY_0-sJBKRSQ"
            ),
        ]
    );
}

#[test]
fn google_docs_stay_listed_beside_stored_files() {
    let id = "12zxlvJtuHFV6awc3AINaNHnfvRttPv0i";
    let recorded = Recorded::default().fixture(&page(id), "drive/folder-select-mixed.html");
    let listing = block_on(folder(id).resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 2);
    assert_eq!(
        listing.unavailable,
        [crate::source::Unavailable {
            path: "gdown".to_owned(),
            reason: "google docs exports can change between downloads".to_owned(),
        }]
    );
    assert_eq!(recorded.asked(), [page(id)]);
}

#[test]
fn a_folder_shared_with_some_people_names_what_its_owner_can_do() {
    let recorded =
        Recorded::default().on(&page(DEEP), 401, fixture_bytes("drive/folder-private.html"));
    let err = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::AuthRequired);
    assert_eq!(
        err.message,
        format!(
            "https://drive.google.com/drive/folders/{DEEP} is not shared with anyone who has the link"
        )
    );
    assert!(err.help.unwrap().contains("anyone with the link"));
}

#[test]
fn a_subfolder_drive_refuses_fails_the_whole_listing() {
    let recorded = Recorded::default()
        .fixture(&page(DEEP), "drive/folder-deep.html")
        .on(
            &page(FOLDER_1),
            401,
            fixture_bytes("drive/folder-private.html"),
        );
    let err = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::AuthRequired);
    assert!(
        err.message.starts_with(&format!(
            "the folder folder_1 in https://drive.google.com/drive/folders/{DEEP} "
        )),
        "{}",
        err.message
    );
}

#[test]
fn a_deleted_folder_is_unresolved() {
    let recorded =
        Recorded::default().on(&page(DEEP), 404, fixture_bytes("drive/folder-missing.html"));
    let err = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::ReferenceUnresolved);
}

#[test]
fn a_folder_link_to_a_file_holds_no_files() {
    // drive answers a file's id with an empty folder page named after the file
    let id = "1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb";
    let recorded = Recorded::default().fixture(&page(id), "drive/folder-of-a-file.html");
    let err = block_on(folder(id).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::ReferenceUnresolved);
    assert!(err.message.ends_with("holds no files"), "{}", err.message);
}

#[test]
fn a_folder_reached_twice_is_refused() {
    // folder_1 answers with the page of deep_folder, which lists folder_1 again
    let recorded = Recorded::default()
        .fixture(&page(DEEP), "drive/folder-deep.html")
        .fixture(&page(FOLDER_1), "drive/folder-deep.html");
    let err = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::SourceInvalid);
}

#[test]
fn a_page_of_another_shape_is_refused() {
    let recorded =
        Recorded::default().on(&page(DEEP), 200, "<html><body>something else</body></html>");
    let err = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(err.kind, Kind::SourceInvalid);
}

#[test]
fn an_empty_folder_says_it_holds_no_files() {
    let recorded = Recorded::default().fixture(&page(DEEP), "drive/folder-empty.html");
    let error = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.message.contains("holds no files"));
}

#[test]
fn relative_file_links_are_resolved_on_drive() {
    let body = String::from_utf8(fixture_bytes("drive/folder-deep-1-1.html"))
        .unwrap()
        .replace("https://drive.google.com/file/d/", "/file/d/");
    let recorded = Recorded::default().on(&page(FOLDER_1_1), 200, body);
    let listing = block_on(folder(FOLDER_1_1).resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 1);
    assert_eq!(
        listing.files[0].url,
        uc("1eGGJOMWkEZCzfp5rT4fbZ3-lJ5ShDUST")
    );
}

#[test]
fn a_folder_allows_the_file_limit_but_not_one_more() {
    for (count, allowed) in [(100_000, true), (100_001, false)] {
        let mut body = String::from("<html><title>many</title><div class=\"flip-entries\">");
        for number in 0..count {
            use std::fmt::Write as _;
            write!(&mut body, "<div class=\"flip-entry\"><a href=\"https://drive.google.com/file/d/1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb/view\"><div class=\"flip-entry-title\">file-{number}.txt</div></a></div>").unwrap();
        }
        body.push_str("</div></html>");
        let recorded = Recorded::default().on(&page(DEEP), 200, body);
        let reference = Reference::Drive(folder(DEEP));
        let listing = block_on(reference.resolve(&recorded));
        if allowed {
            assert_eq!(listing.unwrap().files.len(), count);
        } else {
            assert_eq!(listing.unwrap_err().kind, Kind::SourceInvalid);
        }
    }
}

#[test]
fn unsafe_folder_names_are_refused_before_reading_them() {
    let body = String::from_utf8(fixture_bytes("drive/folder-deep.html"))
        .unwrap()
        .replace(">folder_1<", ">..<");
    let recorded = Recorded::default().on(&page(DEEP), 200, body);
    let error = block_on(folder(DEEP).resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [page(DEEP)]);
}

#[test]
fn unavailable_entries_count_toward_the_listing_limit() {
    for (count, allowed) in [(100_000, true), (100_001, false)] {
        let mut body = String::from("<html><title>docs</title><div class=\"flip-entries\">");
        for number in 0..count {
            use std::fmt::Write as _;
            write!(&mut body, "<div class=\"flip-entry\"><a href=\"https://docs.google.com/document/d/1TFYNzuZJTgNGzGmjraZ58ZVOh9_YoKeBnU-opWgXQL4/edit\"><div class=\"flip-entry-title\">doc-{number}</div></a></div>").unwrap();
        }
        body.push_str("</div></html>");
        let recorded = Recorded::default().on(&page(DEEP), 200, body);
        let result = block_on(Reference::Drive(folder(DEEP)).resolve(&recorded));
        if allowed {
            let listing = result.unwrap();
            assert_eq!(listing.files, [] as [Remote; 0]);
            assert_eq!(listing.unavailable.len(), count);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(error.message.contains("more than 100000"));
        }
    }
}
