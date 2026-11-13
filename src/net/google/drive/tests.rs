use std::sync::Arc;

use super::super::tests::{authorizations, login, server, through, tokens, transport};
use super::*;
use crate::net::scripted::reply;
use crate::net::{Google, Offline, Options, Splitting};
use crate::source::recorded::block_on;

const ROOT: &str = "1LabRawFolderOfTheTestAbCdEfGhIjK";
const SUB: &str = "1ScansSubfolderOfTheTestAbCdEfGhI";
const FILE: &str = "1ParticipantsFileOfTheTestAbCdEfG";
const DEEP: &str = "1DeepFileOfTheTestAbCdEfGhIjKlMn";
const ELSEWHERE: &str = "1SomeoneElsesPrivateFileAbCdEfGhI";
const UNLISTED_ITEM: &str = "1ItemOfAnUnlistedFolderAbCdEfGhIj";

fn listed(ids: &[&str]) -> Vec<DriveId> {
    ids.iter().map(|id| DriveId::parse(id).unwrap()).collect()
}

/// a network that sends google tokens to drive's api for the `listed` items, from the login at
/// `credentials`, asked of the token endpoint at `endpoint`.
fn drive_net(listed_ids: &[&str], credentials: &std::path::Path, endpoint: &Url) -> Net {
    let named = credentials.as_os_str().to_owned();
    let google = Google::new(
        Vec::new(),
        Arc::new(move |name: &str| {
            (name == "GOOGLE_APPLICATION_CREDENTIALS").then(|| named.clone())
        }),
    )
    .with_drive(listed(listed_ids))
    .with_token_endpoint(Some(endpoint.clone()));
    Net::new(
        Options {
            offline: Offline::No,
            jobs: 8,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
    .with_google(google)
}

/// a page of a folder's items, as `files.list` sends it.
fn page(ids: &[&str]) -> Vec<u8> {
    let files: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({"id": id, "name": format!("{id}.csv")}))
        .collect();
    serde_json::json!({ "files": files })
        .to_string()
        .into_bytes()
}

/// a google json error of drive's api.
fn said(reason: &str, message: &str) -> Vec<u8> {
    serde_json::json!({"error": {
        "errors": [{"domain": "global", "reason": reason, "message": message}],
        "code": 403,
        "message": message,
    }})
    .to_string()
    .into_bytes()
}

#[test]
fn drive_api_requests_are_located_strictly() {
    let listing = |id: &str| {
        format!(
            "https://www.googleapis.com/drive/v3/files?q=%27{id}%27+in+parents+and+trashed+%3D+false&fields=nextPageToken%2Cfiles%28id%29&pageSize=1000&pageToken=abc&supportsAllDrives=true&includeItemsFromAllDrives=true"
        )
    };
    for (link, id, is_listing) in [
        (
            format!(
                "https://www.googleapis.com/drive/v3/files/{FILE}?alt=media&supportsAllDrives=true"
            ),
            FILE,
            false,
        ),
        (
            format!("https://www.googleapis.com/drive/v3/files/{ROOT}?fields=id%2Cname"),
            ROOT,
            false,
        ),
        (listing(ROOT), ROOT, true),
    ] {
        assert_eq!(
            locate(&Url::parse(&link).unwrap()),
            Some(Asked {
                id: id.to_owned(),
                listing: is_listing,
            }),
            "{link}"
        );
    }
    for link in [
        format!("http://www.googleapis.com/drive/v3/files/{FILE}?alt=media"),
        format!("https://www.googleapis.com:8443/drive/v3/files/{FILE}?alt=media"),
        format!("https://googleapis.com/drive/v3/files/{FILE}?alt=media"),
        format!("https://drive.usercontent.google.com/download?id={FILE}"),
        format!("https://drive.google.com/uc?id={FILE}"),
        format!("https://www.googleapis.com/drive/v2/files/{FILE}?alt=media"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}/export?mimeType=text/csv"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}/permissions"),
        format!("https://www.googleapis.com/upload/drive/v3/files/{FILE}"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}?alt=json"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}?alt=media&key=abc"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}?alt=media&acknowledgeAbuse=true"),
        format!("https://www.googleapis.com/drive/v3/files/{FILE}?q=x"),
        "https://www.googleapis.com/drive/v3/files/short?alt=media".to_owned(),
        "https://www.googleapis.com/drive/v3/files/".to_owned(),
        "https://www.googleapis.com/drive/v3/files".to_owned(),
        "https://www.googleapis.com/drive/v3/files?q=name+contains+%27a%27".to_owned(),
        format!("https://www.googleapis.com/drive/v3/files?q=%27{ROOT}%27+in+parents"),
        format!(
            "https://www.googleapis.com/drive/v3/files?q=%27{ROOT}%27+in+parents+and+trashed+%3D+false+or+%27{ELSEWHERE}%27+in+parents+and+trashed+%3D+false"
        ),
        format!("{}&corpora=allDrives", listing(ROOT)),
        format!(
            "{}&q=%27{ELSEWHERE}%27+in+parents+and+trashed+%3D+false",
            listing(ROOT)
        ),
        format!("{}&access_token=abc", listing(ROOT)),
        "https://www.googleapis.com/storage/v1/b/lab-private/o".to_owned(),
    ] {
        assert_eq!(locate(&Url::parse(&link).unwrap()), None, "{link}");
    }
}

/// what they hold being what google's listing, asked with the token, put there. what a lock
/// names, or a listing of an unlisted folder, teaches nothing.
#[test]
fn a_drive_token_goes_only_to_listed_items_drive_refused_and_what_they_hold() {
    let drive = Drive::new(listed(&[ROOT, SUB]));
    assert!(!drive.may_carry(ROOT));
    // a listing before the folder was refused went without the token, and proves nothing
    drive.heard(ROOT, &page(&[FILE]));
    assert!(!drive.may_carry(FILE));
    drive.seen().refused.insert(ROOT.to_owned());
    assert!(drive.may_carry(ROOT));
    assert!(!drive.may_carry(FILE));
    drive.heard(ROOT, &page(&[FILE, SUB]));
    assert!(drive.may_carry(FILE));
    // a listed subfolder of a refused folder carries the token, and so does what it holds
    assert!(drive.may_carry(SUB));
    drive.heard(SUB, &page(&[DEEP]));
    assert!(drive.may_carry(DEEP));
    // an item refused without a token is no reason to carry one, unless it is listed
    drive.seen().refused.insert(ELSEWHERE.to_owned());
    assert!(!drive.may_carry(ELSEWHERE));
    drive.heard(ELSEWHERE, &page(&[UNLISTED_ITEM]));
    assert!(!drive.may_carry(UNLISTED_ITEM));
    // ids drive could not have sent are no items
    drive.heard(ROOT, &page(&["../../etc", "short"]));
    assert!(!drive.may_carry("../../etc"));
    assert!(!drive.may_carry("short"));
    assert!(!Drive::default().may_carry(ROOT));
}

/// though its folder is listed and refused: only an answer to the token speaks for the account.
#[test]
fn a_listing_answered_without_the_token_proves_nothing() {
    let (endpoint, _) = tokens();
    let folder = tempfile::tempdir().unwrap();
    let net = drive_net(&[ROOT], &login(folder.path()), &endpoint);
    net.google.drive.seen().refused.insert(ROOT.to_owned());
    let listing = |identity: Option<&str>| Answered {
        asked: Asked {
            id: ROOT.to_owned(),
            listing: true,
        },
        identity: identity.map(str::to_owned),
        account: None,
        lacks_scope: false,
    };
    assert!(net.drive_heard(&listing(None), 200, &page(&[FILE])).is_ok());
    assert!(!net.google.drive.may_carry(FILE));
    assert!(
        net.drive_heard(&listing(Some("the google login")), 404, &page(&[FILE]))
            .is_ok()
    );
    assert!(!net.google.drive.may_carry(FILE));
    assert!(
        net.drive_heard(&listing(Some("the google login")), 200, &page(&[FILE]))
            .is_ok()
    );
    assert!(net.google.drive.may_carry(FILE));
}

/// once drive refused the listed file, after one request to the token endpoint. an item nobody
/// listed never gets it, though drive refused it too.
#[test]
fn a_drive_token_goes_to_drives_api_alone_and_never_follows_a_redirect() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|_, count| match count {
        0 | 2 => reply(
            "302 Found",
            &[("Location", "https://elsewhere.example/a.csv")],
            b"",
        ),
        _ => reply("200 OK", &[], b"bytes"),
    });
    let folder = tempfile::tempdir().unwrap();
    let net = drive_net(&[FILE], &login(folder.path()), &endpoint);
    let client = transport();
    let media = |id: &str| media(id).unwrap().to_string();
    let unrefused = block_on(through(&net, &client, &local, &media(FILE)));
    assert_eq!(unrefused.status(), 200);
    assert_eq!(authorizations(&asked), [None, None]);
    assert!(asked_for_tokens.lock().unwrap().is_empty());
    net.google.drive.seen().refused.insert(FILE.to_owned());
    net.google.drive.seen().refused.insert(ELSEWHERE.to_owned());
    let answer = block_on(through(&net, &client, &local, &media(FILE)));
    assert_eq!(answer.status(), 200);
    let other = block_on(through(&net, &client, &local, &media(ELSEWHERE)));
    assert_eq!(other.status(), 200);
    assert_eq!(
        authorizations(&asked),
        [None, None, Some("bearer token-0".to_owned()), None, None]
    );
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 1);
    assert_eq!(
        other
            .extensions()
            .get::<Answered>()
            .map(|answered| &answered.identity),
        Some(&None)
    );
}

/// as a 401 refuses an expired or revoked one.
#[test]
fn a_token_drive_refuses_is_replaced_once() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|request, _| {
        match request.authorization() {
        Some("bearer token-1") => reply("200 OK", &[], b"bytes"),
        _ => reply(
            "401 Unauthorized",
            &[("Content-Type", "application/json")],
            br#"{"error":{"errors":[{"domain":"global","reason":"authError","message":"Invalid Credentials","locationType":"header","location":"Authorization"}],"code":401,"message":"Invalid Credentials"}}"#,
        ),
    }
    });
    let folder = tempfile::tempdir().unwrap();
    let net = drive_net(&[FILE], &login(folder.path()), &endpoint);
    net.google.drive.seen().refused.insert(FILE.to_owned());
    let client = transport();
    let link = media(FILE).unwrap().to_string();
    let answer = block_on(through(&net, &client, &local, &link));
    assert_eq!(answer.status(), 200);
    assert_eq!(
        authorizations(&asked),
        [
            Some("bearer token-0".to_owned()),
            Some("bearer token-1".to_owned())
        ]
    );
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 2);
}

const ACCOUNT: &str = "lab-reader@lab-project.iam.gserviceaccount.com";

/// how a request about the root went out with a service account's token.
fn service() -> Answered {
    Answered {
        asked: Asked {
            id: ROOT.to_owned(),
            listing: true,
        },
        identity: Some(format!("the service account {ACCOUNT} in /keys/lab.json")),
        account: Some(ACCOUNT.to_owned()),
        lacks_scope: false,
    }
}

/// how a request about the root went out with a person's login.
fn person() -> Answered {
    Answered {
        identity: Some("the google login in /home/ada/adc.json".to_owned()),
        account: None,
        ..service()
    }
}

fn link() -> String {
    format!("https://drive.google.com/open?id={ROOT}")
}

/// the service account's email to share with, or the gcloud command for a login without the
/// drive scope, which google says in a header or in its words alone.
#[test]
fn a_refusal_names_the_account_and_what_would_let_it_read() {
    let link = link();
    let not_found = said("notFound", &format!("File not found: {ROOT}."));
    let unshared = service().error(404, &not_found);
    assert_eq!(unshared.kind, Kind::AuthRequired);
    assert_eq!(
        unshared.message,
        format!(
            "google drive has no file or folder {link} that the service account {ACCOUNT} in /keys/lab.json can read"
        )
    );
    assert_eq!(
        unshared.help.as_deref(),
        Some(
            format!("share it with {ACCOUNT} as a viewer, then run the same command again")
                .as_str()
        )
    );
    assert_eq!(
        person().error(404, &not_found).help.as_deref(),
        Some(
            "ask its owner to share it with the google account you signed in with, or sign in with one it is shared with"
        )
    );
    let words = said(
        "insufficientPermissions",
        "Request had insufficient authentication scopes.",
    );
    let headed = Answered {
        lacks_scope: true,
        ..person()
    };
    for (answered, body) in [(headed, b"".to_vec()), (person(), words)] {
        let unscoped = answered.error(403, &body);
        assert_eq!(
            unscoped.message,
            format!(
                "google drive refused the google login in /home/ada/adc.json for {link}: its token lacks the drive.readonly scope"
            )
        );
        assert!(
            unscoped.help.as_deref().unwrap().contains(
                "`gcloud auth application-default login --client-id-file=CLIENT_FILE --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform`"
            ),
            "{unscoped:?}"
        );
    }
}

/// drive's api turned off in the account's project says where to turn it on, a rate limit is a
/// failure another try may get past, and google's words reach no terminal unescaped.
#[test]
fn other_refusals_say_what_google_said() {
    let link = link();
    let disabled = service().error(
        403,
        &said(
            "accessNotConfigured",
            "Google Drive API has not been used in project 123456789 before or it is disabled.",
        ),
    );
    assert_eq!(
        disabled.message,
        format!(
            "google drive's api is turned off in the google cloud project of the service account {ACCOUNT} in /keys/lab.json"
        )
    );
    assert_eq!(
        disabled.details,
        [
            "google said: Google Drive API has not been used in project 123456789 before or it is disabled."
        ]
    );
    let limited = said("userRateLimitExceeded", "User Rate Limit Exceeded");
    assert!(matches!(
        service().failure(403, &limited),
        Failure::Transient { .. }
    ));
    assert_eq!(
        service().error(403, &limited).kind,
        Kind::NetworkRateLimited
    );
    let not_found = said("notFound", &format!("File not found: {ROOT}."));
    assert!(matches!(
        service().failure(404, &not_found),
        Failure::Final(_)
    ));
    assert_eq!(
        person().error(401, b"{}").message,
        format!(
            "google drive refused the token of the google login in /home/ada/adc.json for {link}"
        )
    );
    let tokenless = Answered {
        identity: None,
        account: None,
        ..service()
    };
    assert_eq!(
        tokenless.error(403, b"").message,
        format!("google drive's api refused {link} without a token")
    );
    let loud = service().error(
        403,
        &said(
            "accessNotConfigured",
            &format!("\u{1b}[2J{}", "a".repeat(400)),
        ),
    );
    assert!(!loud.details[0].contains('\u{1b}'), "{:?}", loud.details);
    assert!(loud.details[0].chars().count() < 340, "{:?}", loud.details);
}
