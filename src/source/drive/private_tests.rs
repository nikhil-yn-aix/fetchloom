use super::*;
use crate::net::DriveAnswered;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const ROOT: &str = "1LabRawFolderOfTheTestAbCdEfGhIjK";
const SUB: &str = "1ScansSubfolderOfTheTestAbCdEfGhI";
const A: &str = "1ParticipantsFileOfTheTestAbCdEfG";
const B: &str = "1ScanTableFileOfTheTestAbCdEfGhIj";
const E: &str = "1EventsFileOfTheTestAbCdEfGhIjKlM";
const DOC: &str = "1NotesDocOfTheTestAbCdEfGhIjKlMnO";
const SHORTCUT_ID: &str = "1ShortcutOfTheTestAbCdEfGhIjKlMnO";
const ACCOUNT: &str = "lab-reader@lab-project.iam.gserviceaccount.com";
const A_MD5: &str = "0b9f5c8474e0dadc9977fa058d4d8b9c";
const A_SHA: &str = "63fa4c0d7a7c84c546d0d50646149cc2546cc9c97ebde21556ab98c290d46083";
const B_MD5: &str = "d8bc141841bc797bff58c38eb3424cbf";
const B_SHA: &str = "bce4b2ed55674df0952ea1740b4eab67eaa79737a7b19cadcb0346faea65340c";

/// drive's api: what `files.get` asks of an item.
fn about_url(id: &str) -> String {
    format!(
        "https://www.googleapis.com/drive/v3/files/{id}?fields=id%2Cname%2CmimeType%2Csize%2Cmd5Checksum%2Csha256Checksum&supportsAllDrives=true"
    )
}

/// drive's api: what `files.list` asks for one page of a folder's items.
fn children_url(id: &str, token: Option<&str>) -> String {
    let mut url = format!(
        "https://www.googleapis.com/drive/v3/files?q=%27{id}%27+in+parents+and+trashed+%3D+false&fields=nextPageToken%2CincompleteSearch%2Cfiles%28id%2Cname%2CmimeType%2Csize%2Cmd5Checksum%2Csha256Checksum%29&pageSize=1000&supportsAllDrives=true&includeItemsFromAllDrives=true"
    );
    if let Some(token) = token {
        url.push_str("&pageToken=");
        url.push_str(token);
    }
    url
}

fn folder_item(id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({"id": id, "name": name, "mimeType": FOLDER})
}

fn file_item(id: &str, name: &str, size: &str, md5: &str, sha256: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id, "name": name, "mimeType": "text/csv", "size": size,
        "md5Checksum": md5, "sha256Checksum": sha256,
    })
}

fn page(items: &[serde_json::Value], next: Option<&str>) -> Vec<u8> {
    let mut page = serde_json::json!({ "files": items });
    if let Some(next) = next {
        page["nextPageToken"] = next.into();
    }
    page.to_string().into_bytes()
}

fn folder(id: &str) -> Item {
    Item::parse(&format!("folders/{id}")).unwrap()
}

/// a private folder as drive's api answers for it: two pages for the root, holding a file, a
/// subfolder, a google doc and a shortcut, then a file without checksums; the subfolder holds
/// one more file.
fn lab() -> Recorded {
    Recorded::default()
        .on(
            &about_url(ROOT),
            200,
            folder_item(ROOT, "lab-raw").to_string(),
        )
        .on(
            &children_url(ROOT, None),
            200,
            page(
                &[
                    file_item(A, "participants.tsv", "28", A_MD5, A_SHA),
                    folder_item(SUB, "scans"),
                    serde_json::json!({"id": DOC, "name": "notes", "mimeType": "application/vnd.google-apps.document"}),
                    serde_json::json!({"id": SHORTCUT_ID, "name": "elsewhere", "mimeType": "application/vnd.google-apps.shortcut"}),
                ],
                Some("page-2"),
            ),
        )
        .on(
            &children_url(ROOT, Some("page-2")),
            200,
            page(
                &[serde_json::json!({"id": E, "name": "events.tsv", "mimeType": "text/plain", "size": "7"})],
                None,
            ),
        )
        .on(
            &children_url(SUB, None),
            200,
            page(&[file_item(B, "scans.tsv", "24", B_MD5, B_SHA)], None),
        )
}

/// drive's page refusing folder `id` to anyone with its link.
fn refused_page(recorded: Recorded, id: &str) -> Recorded {
    recorded.on(
        &format!("https://drive.google.com/embeddedfolderview?id={id}"),
        401,
        fixture_bytes("drive/folder-private.html"),
    )
}

#[test]
fn a_private_folder_is_listed_through_drives_api() {
    let recorded = lab();
    let listing = block_on(list(&folder(ROOT), &recorded)).unwrap();
    assert_eq!(listing.title.as_deref(), Some("lab-raw"));
    assert_eq!(listing.access, Access::Login);
    let uc = |id: &str| format!("https://drive.google.com/uc?id={id}");
    let files: Vec<_> = listing
        .files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.size,
                file.url.to_string(),
                file.sha256.map(|sha| sha.hex()),
                file.md5.map(|md5| md5.hex()),
                file.named_by_server,
            )
        })
        .collect();
    assert_eq!(
        files,
        [
            (
                "participants.tsv".to_owned(),
                Some(28),
                uc(A),
                Some(A_SHA.to_owned()),
                Some(A_MD5.to_owned()),
                false
            ),
            ("events.tsv".to_owned(), Some(7), uc(E), None, None, false),
            (
                "scans/scans.tsv".to_owned(),
                Some(24),
                uc(B),
                Some(B_SHA.to_owned()),
                Some(B_MD5.to_owned()),
                false
            ),
        ]
    );
    let unavailable: Vec<_> = listing
        .unavailable
        .iter()
        .map(|entry| (entry.path.as_str(), entry.reason.as_str()))
        .collect();
    assert_eq!(
        unavailable,
        [
            (
                "notes",
                "google docs exports can change between downloads, so a lock cannot pin them"
            ),
            (
                "elsewhere",
                "a shortcut to an item elsewhere in drive: add that item by its own link"
            ),
        ]
    );
    assert_eq!(
        recorded.asked(),
        [
            about_url(ROOT),
            children_url(ROOT, None),
            children_url(ROOT, Some("page-2")),
            children_url(SUB, None),
        ]
    );
}

#[test]
fn a_private_folder_takes_in_at_most_the_files_and_folders_allowed() {
    let error = block_on(list_at_most(&folder(ROOT), &lab(), 2)).unwrap_err();
    assert!(error.message.contains("more than 2"), "{error:?}");
    // drive sending empty pages, each with a token for the next, stops the walk
    let endless = Recorded::default()
        .on(
            &about_url(ROOT),
            200,
            folder_item(ROOT, "lab-raw").to_string(),
        )
        .on(&children_url(ROOT, None), 200, page(&[], Some("next")))
        .on(
            &children_url(ROOT, Some("next")),
            200,
            page(&[], Some("next")),
        );
    let error = block_on(list_at_most(&folder(ROOT), &endless, 5)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "drive's api sent a listing of https://drive.google.com/drive/folders/{ROOT} that fetchloom cannot read: it repeated an earlier page"
        )
    );
}

/// a listing google says is incomplete, a path that would leave the dataset's folder, and a
/// folder met twice are refused rather than pinned, and so is a folder link to a file.
#[test]
fn a_listing_fetchloom_cannot_trust_is_refused() {
    let incomplete = br#"{"files":[],"incompleteSearch":true}"#.to_vec();
    let climbing = page(&[file_item(A, "..", "1", A_MD5, A_SHA)], None);
    let twice = page(&[folder_item(SUB, "a"), folder_item(SUB, "b")], None);
    for (answer, reason) in [
        (incomplete, "drive said its listing is incomplete"),
        (climbing, "the path .. it climbs out of its folder"),
        (twice, "the folder b appears twice"),
    ] {
        let recorded = Recorded::default()
            .on(
                &about_url(ROOT),
                200,
                folder_item(ROOT, "lab-raw").to_string(),
            )
            .on(&children_url(ROOT, None), 200, answer)
            .on(&children_url(SUB, None), 200, page(&[], None));
        let error = block_on(list(&folder(ROOT), &recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{reason}");
        assert!(error.message.ends_with(reason), "{}", error.message);
    }
    let file = Recorded::default().on(
        &about_url(A),
        200,
        file_item(A, "participants.tsv", "28", A_MD5, A_SHA).to_string(),
    );
    let error = block_on(list(&folder(A), &file)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        format!("drive's {A} is a file, not a folder")
    );
}

/// without the network's leave, the refusal says what would let the folder through.
#[test]
fn a_refused_folder_goes_to_drives_api_only_when_the_network_lets_it() {
    let ready = refused_page(lab(), ROOT).private(Private::Ready);
    let listing = block_on(folder(ROOT).resolve(&ready)).unwrap();
    assert_eq!(listing.files.len(), 3);
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:folders/{ROOT}")
    );
    let unlisted = refused_page(lab(), ROOT);
    let error = block_on(folder(ROOT).resolve(&unlisted)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        format!(
            "https://drive.google.com/drive/folders/{ROOT} is not shared with anyone who has the link"
        )
    );
    assert_eq!(
        error.help.as_deref(),
        Some(super::super::trust(ROOT).as_str())
    );
    assert_eq!(
        unlisted.asked(),
        [
            format!("https://drive.google.com/embeddedfolderview?id={ROOT}"),
            format!("drive refused {ROOT}"),
        ]
    );
    let unsigned = refused_page(lab(), ROOT).private(Private::Unsigned);
    let error = block_on(folder(ROOT).resolve(&unsigned)).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "https://drive.google.com/drive/folders/{ROOT} is not shared with anyone who has the link, and no google application default credentials were found"
        )
    );
    assert!(
        error.help.as_deref().unwrap().starts_with(
            "set GOOGLE_APPLICATION_CREDENTIALS to the key file of a service account it is shared with"
        ),
        "{error:?}"
    );
}

#[test]
fn an_open_link_drive_refuses_is_what_drives_api_says_it_is() {
    let open = |id: &str| format!("https://drive.google.com/open?id={id}");
    let refused = |recorded: Recorded, id: &str| {
        let sign_in = format!(
            "https://accounts.google.com/v3/signin/identifier?continue=https://drive.google.com/open?id%3D{id}"
        );
        recorded
            .on(&open(id), 200, fixture_bytes("drive/sign-in.html"))
            .redirected(&open(id), &sign_in)
            .private(Private::Ready)
    };
    let item = |id: &str| Item::parse(&format!("open/{id}")).unwrap();
    let listing = block_on(item(ROOT).resolve(&refused(lab(), ROOT))).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        format!("drive:folders/{ROOT}")
    );
    assert_eq!(listing.files.len(), 3);
    let file = Recorded::default().on(
        &about_url(A),
        200,
        file_item(A, "participants.tsv", "28", A_MD5, A_SHA).to_string(),
    );
    let listing = block_on(item(A).resolve(&refused(file, A))).unwrap();
    assert_eq!(listing.resolved.to_string(), format!("drive:{A}"));
    assert_eq!(listing.files.len(), 1);
    assert!(listing.files[0].named_by_server);
}

/// a refusal of drive's api is told as the network says the request went: whose token, and
/// whom to share with. a google doc has no bytes for a lone file to download.
#[test]
fn drives_refusals_and_documents_say_what_to_do() {
    let unshared = Recorded::default()
        .on(
            &about_url(ROOT),
            404,
            br#"{"error":{"errors":[{"domain":"global","reason":"notFound","message":"File not found: 1LabRawFolderOfTheTestAbCdEfGhIjK."}],"code":404,"message":"File not found: 1LabRawFolderOfTheTestAbCdEfGhIjK."}}"#.to_vec(),
        )
        .drive(
            &about_url(ROOT),
            DriveAnswered::service_account(ROOT, ACCOUNT),
        );
    let error = block_on(list(&folder(ROOT), &unshared)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.help.as_deref(),
        Some(
            format!("share it with {ACCOUNT} as a viewer, then run the same command again")
                .as_str()
        )
    );
    let document = Recorded::default().on(
        &about_url(DOC),
        200,
        serde_json::json!({"id": DOC, "name": "notes", "mimeType": "application/vnd.google-apps.document"})
            .to_string(),
    );
    let error = block_on(file(&document, DOC)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        format!(
            "drive's {DOC} is a google doc, sheet or slides, which drive keeps as no file, with no bytes of its own to download"
        )
    );
}

/// a private folder of the tester's, read for real: set `FETCHLOOM_TEST_LIVE_DRIVE_FOLDER` to its
/// id, and `GOOGLE_APPLICATION_CREDENTIALS` to a key of a service account it is shared with, or
/// sign in with `gcloud auth application-default login --client-id-file=...` and the drive scope.
/// it lists the folder through drive's api and checks its first file against drive's checksums.
#[test]
#[ignore = "live: talks to google drive with application default credentials"]
fn live_private_drive() {
    let Some(id) = std::env::var("FETCHLOOM_TEST_LIVE_DRIVE_FOLDER")
        .ok()
        .filter(|id| is_id(id))
    else {
        eprintln!(
            "skipped: set FETCHLOOM_TEST_LIVE_DRIVE_FOLDER to the id of a private drive folder"
        );
        return;
    };
    if !matches!(crate::auth::google::find(&crate::machine::env), Ok(Some(_))) {
        eprintln!("skipped: no google application default credentials on this machine");
        return;
    }
    let google = crate::net::Google::new(Vec::new(), std::sync::Arc::new(crate::machine::env))
        .with_drive(vec![DriveId::parse(&id).unwrap()]);
    let (net, work) = crate::source::recorded::live(Vec::new());
    let net = net.with_google(google);
    let listing = block_on(folder(&id).resolve(&net)).unwrap();
    assert!(!listing.files.is_empty(), "{listing:?}");
    let remote = &listing.files[0];
    let folder = tempfile::tempdir().unwrap();
    let cache = crate::cache::Cache::open(folder.path().join("cache")).unwrap();
    let fetch = crate::net::Fetch {
        expect: crate::net::Expect {
            sha256: remote.sha256,
            md5: remote.md5,
            size: remote.size,
        },
        ..crate::source::fetch_plan(&remote.url)
    };
    let fetched = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap();
    assert_eq!(Some(fetched.size), remote.size);
    eprintln!(
        "drive: {} files, {} unavailable, {} downloaded as {}, {} requests",
        listing.files.len(),
        listing.unavailable.len(),
        remote.path,
        fetched.digest.short(),
        work.counts().requests
    );
}
