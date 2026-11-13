//! private google drive folders and files, read through drive's api with google's application
//! default credentials, against a server on this machine that stands in for google's token
//! endpoint, for drive's folder pages and download host, and for drive's api, answering as
//! google's api reference documents it.

use std::collections::HashMap;
use std::fs;
use std::process::Output;

use super::{
    Asked, CLOUD_LOGIN, CLOUD_TOKEN, DRIVE_SERVICE_TOKEN, EMAIL, LOGIN_TOKEN, REFRESH_TOKEN,
    Standin, assert_no_secret, kept_files, login, respond, service_account, token,
};
use crate::support::{Fetchloom, Result, Sandbox, json, text};

/// a private folder, `lab-raw`: participants.tsv, a google doc, and the subfolder scans with
/// scans.tsv. drive lists the root in two pages.
const ROOT: &str = "1LabRawFolderOfTheTestAbCdEfGhIjK";
const SUB: &str = "1ScansSubfolderOfTheTestAbCdEfGhI";
const A: &str = "1ParticipantsFileOfTheTestAbCdEfG";
const B: &str = "1ScanTableFileOfTheTestAbCdEfGhIj";
const DOC: &str = "1NotesDocOfTheTestAbCdEfGhIjKlMnO";
/// a private file shared on its own.
const LONE: &str = "1LoneProtocolFileOfTheTestAbCdEfG";
/// another lab's private folder, which holds one file.
const OTHER: &str = "1AnotherLabsFolderOfTheTestAbCdEf";
const C: &str = "1AnotherLabsFileOfTheTestAbCdEfGh";
/// a folder no account of the test was given.
const UNSHARED: &str = "1FolderNotSharedWithTheAccountAb";

const A_BYTES: &[u8] = b"participant_id\tsex\nsub-01\tF\n";
const B_BYTES: &[u8] = b"scan\tsite\nscan-01\tparis\n";
const LONE_BYTES: &[u8] = b"# protocol\nscan twice\n";
const C_BYTES: &[u8] = b"other lab\n";

const FOLDER: &str = "application/vnd.google-apps.folder";

/// how drive's api describes each item of the test, as `files.get` and `files.list` send it.
fn described(id: &str) -> Option<serde_json::Value> {
    let file = |name: &str, kind: &str, bytes: &[u8], md5: &str, sha256: &str| {
        serde_json::json!({
            "id": id, "name": name, "mimeType": kind, "size": bytes.len().to_string(),
            "md5Checksum": md5, "sha256Checksum": sha256,
        })
    };
    Some(match id {
        ROOT => serde_json::json!({"id": ROOT, "name": "lab-raw", "mimeType": FOLDER}),
        SUB => serde_json::json!({"id": SUB, "name": "scans", "mimeType": FOLDER}),
        OTHER => serde_json::json!({"id": OTHER, "name": "other-lab", "mimeType": FOLDER}),
        DOC => serde_json::json!({
            "id": DOC, "name": "notes", "mimeType": "application/vnd.google-apps.document",
        }),
        A => file(
            "participants.tsv",
            "text/tab-separated-values",
            A_BYTES,
            "0b9f5c8474e0dadc9977fa058d4d8b9c",
            "63fa4c0d7a7c84c546d0d50646149cc2546cc9c97ebde21556ab98c290d46083",
        ),
        B => file(
            "scans.tsv",
            "text/tab-separated-values",
            B_BYTES,
            "d8bc141841bc797bff58c38eb3424cbf",
            "bce4b2ed55674df0952ea1740b4eab67eaa79737a7b19cadcb0346faea65340c",
        ),
        LONE => file(
            "protocol.md",
            "text/markdown",
            LONE_BYTES,
            "da76072624e995f1fb535824cee95883",
            "748d7b5fdc5e9e38b9963711523be2b4aaaf997ddb1a1a29bd7f4b09a233ccf4",
        ),
        C => file(
            "c.txt",
            "text/plain",
            C_BYTES,
            "a8a72ffba4fa8f1c93505521b78dded4",
            "cd595abe2569f8c0e56d4fe9db7edb57d1bc2280eaf206b37bc65b440237a59f",
        ),
        _ => return None,
    })
}

/// a google json error, as drive's api sends one.
fn error(status: &str, code: u16, reason: &str, message: &str) -> Vec<u8> {
    let body = serde_json::json!({"error": {
        "errors": [{"domain": "global", "reason": reason, "message": message}],
        "code": code,
        "message": message,
    }});
    respond(status, body.to_string().as_bytes())
}

/// the page drive's folder view and download host send for an item not shared with anyone who
/// has the link.
fn private_page() -> Vec<u8> {
    let body = b"<html><head><title>Google Drive - Access Denied</title></head><body>You need access</body></html>";
    let mut response = format!(
        "HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// drive's api refusing a token without a drive scope, as google words that.
fn without_the_drive_scope() -> Vec<u8> {
    let body = br#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","errors":[{"message":"Insufficient Permission","domain":"global","reason":"insufficientPermissions"}],"status":"PERMISSION_DENIED"}}"#;
    let mut response = format!(
        "HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Type: application/json\r\nWWW-Authenticate: Bearer realm=\"https://accounts.google.com/\", error=\"insufficient_scope\", scope=\"https://www.googleapis.com/auth/drive.readonly\"\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// drive's api: what a token that reads drive may see, by `files.get` and `files.list`.
fn api(asked: &Asked, rest: &str, query: &HashMap<String, String>) -> Vec<u8> {
    let carried = asked.headers.get("authorization").map(String::as_str);
    if carried == Some(&format!("Bearer {CLOUD_TOKEN}")) {
        return without_the_drive_scope();
    }
    let reads = [DRIVE_SERVICE_TOKEN, LOGIN_TOKEN]
        .iter()
        .any(|token| carried == Some(&format!("Bearer {token}")));
    if !reads {
        return error("401 Unauthorized", 401, "authError", "Invalid Credentials");
    }
    let field = |name: &str| query.get(name).map_or("", String::as_str);
    if let Some(id) = rest.strip_prefix('/') {
        if field("alt") == "media" {
            let bytes = match id {
                A => A_BYTES,
                B => B_BYTES,
                LONE => LONE_BYTES,
                C => C_BYTES,
                _ => {
                    return error(
                        "404 Not Found",
                        404,
                        "notFound",
                        &format!("File not found: {id}."),
                    );
                }
            };
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            )
            .into_bytes();
            response.extend_from_slice(bytes);
            return response;
        }
        return match described(id) {
            Some(item) => respond("200 OK", item.to_string().as_bytes()),
            None => error(
                "404 Not Found",
                404,
                "notFound",
                &format!("File not found: {id}."),
            ),
        };
    }
    let parent = field("q")
        .strip_prefix('\'')
        .and_then(|q| q.strip_suffix("' in parents and trashed = false"))
        .unwrap_or_default();
    let children: &[&str] = match (parent, field("pageToken")) {
        (ROOT, "") => &[A, SUB],
        (ROOT, "page-2-of-lab-raw") => &[DOC],
        (SUB, "") => &[B],
        (OTHER, "") => &[C],
        _ => {
            return error(
                "404 Not Found",
                404,
                "notFound",
                &format!("File not found: {parent}."),
            );
        }
    };
    let mut page = serde_json::json!({
        "files": children.iter().filter_map(|id| described(id)).collect::<Vec<_>>(),
    });
    if (parent, field("pageToken")) == (ROOT, "") {
        page["nextPageToken"] = "page-2-of-lab-raw".into();
    }
    respond("200 OK", page.to_string().as_bytes())
}

/// the token endpoint, drive's folder view and download host, which refuse every item of the
/// test as private, and drive's api.
fn drive() -> Result<Standin> {
    Standin::start(|asked| {
        if asked.method == "POST" && asked.target == "/token" {
            return token(asked);
        }
        let Ok(url) = url::Url::parse(&format!("http://stand.in{}", asked.target)) else {
            return respond("400 Bad Request", b"{}");
        };
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        match url.path() {
            "/drive.google.com/embeddedfolderview" | "/drive.usercontent.google.com/download" => {
                private_page()
            }
            path => match path.strip_prefix("/www.googleapis.com/drive/v3/files") {
                Some(rest) => api(asked, rest, &query),
                None => respond("404 Not Found", b"{}"),
            },
        }
    })
}

/// a command whose drive and token requests go to `standin`.
fn command(
    sandbox: &Sandbox,
    standin: &Standin,
    folder: &std::path::Path,
    args: &[&str],
) -> Fetchloom {
    let mut command = sandbox.command_in(folder, args);
    command.env("FETCHLOOM_TEST_DRIVE", &standin.url).env(
        "FETCHLOOM_TEST_GOOGLE_TOKEN",
        format!("{}/token", standin.url),
    );
    command
}

/// each request as its path, whether it carried a token, and the item it was about.
fn sent(standin: &Standin) -> Vec<(String, Option<String>, String)> {
    standin
        .requests()
        .into_iter()
        .filter_map(|asked| {
            let token = asked
                .headers
                .get("authorization")
                .map(|value| value.trim_start_matches("Bearer ").to_owned());
            let url = url::Url::parse(&format!("http://stand.in{}", asked.target)).ok()?;
            let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
            let about = query
                .get("id")
                .cloned()
                .or_else(|| {
                    let q = query.get("q")?;
                    Some(q.split('\'').nth(1)?.to_owned())
                })
                .or_else(|| url.path().rsplit('/').next().map(str::to_owned))
                .unwrap_or_default();
            Some((url.path().to_owned(), token, about))
        })
        .collect()
}

/// every request with a token went to drive's api about one of `allowed`, and every request to
/// drive's folder view or download host went without one.
fn tokens_only_for(standin: &Standin, allowed: &[&str]) {
    for (path, token, about) in sent(standin) {
        if path == "/token" {
            continue;
        }
        if token.is_some() {
            assert!(
                path.starts_with("/www.googleapis.com/drive/v3/files"),
                "a token went to {path}"
            );
            assert!(
                allowed.contains(&about.as_str()),
                "a token went with a request about {about}"
            );
        }
    }
}

const API_PATH: &str = "/www.googleapis.com/drive/v3/files";

/// adds the private folder `lab-raw` as `raw`, listed, with a service account's key, writing a
/// timeline to `trace`.
fn add_lab(sandbox: &Sandbox, standin: &Standin, trace: &std::path::Path) -> Result<Output> {
    let key = service_account(sandbox)?;
    let output = command(
        sandbox,
        standin,
        &sandbox.work(),
        &[
            "-vv",
            "add",
            &format!("drive:folders/{ROOT}"),
            "--as",
            "raw",
            "--exclude",
            "notes",
        ],
    )
    .env("FETCHLOOM_DRIVE_FOLDERS", ROOT)
    .env("GOOGLE_APPLICATION_CREDENTIALS", &key)
    .env("FETCHLOOM_TRACE", trace)
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(output)
}

/// a private folder the person lists: drive's folder view refuses it without a token, so drive's
/// api lists it, both pages and the subfolder, with the token a service account's signed
/// assertion got for the drive scope, and its files come from drive's api, pinned by the
/// checksums drive states. no secret reaches an output, a log, a trace or a file.
#[test]
fn a_listed_private_folder_is_read_through_drives_api() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = drive()?;
    let trace = sandbox.folder("traces")?.join("add.json");
    let output = add_lab(&sandbox, &standin, &trace)?;
    let service = Some(DRIVE_SERVICE_TOKEN.to_owned());
    let asked = sent(&standin);
    assert_eq!(
        asked[..6],
        [
            (
                "/drive.google.com/embeddedfolderview".to_owned(),
                None,
                ROOT.to_owned()
            ),
            ("/token".to_owned(), None, "token".to_owned()),
            (
                format!("{API_PATH}/{ROOT}"),
                service.clone(),
                ROOT.to_owned()
            ),
            (API_PATH.to_owned(), service.clone(), ROOT.to_owned()),
            (API_PATH.to_owned(), service.clone(), ROOT.to_owned()),
            (API_PATH.to_owned(), service.clone(), SUB.to_owned()),
        ]
    );
    let mut media: Vec<_> = asked[6..].to_vec();
    media.sort();
    assert_eq!(
        media,
        [
            (format!("{API_PATH}/{A}"), service.clone(), A.to_owned()),
            (format!("{API_PATH}/{B}"), service, B.to_owned()),
        ]
    );
    let work = sandbox.work();
    assert_eq!(fs::read(work.join("data/raw/participants.tsv"))?, A_BYTES);
    assert_eq!(fs::read(work.join("data/raw/scans/scans.tsv"))?, B_BYTES);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for wanted in [
        format!("url = \"https://drive.google.com/uc?id={A}\""),
        format!("url = \"https://drive.google.com/uc?id={B}\""),
        "sha256 = \"63fa4c0d7a7c84c546d0d50646149cc2546cc9c97ebde21556ab98c290d46083\"".to_owned(),
        "md5 = \"0b9f5c8474e0dadc9977fa058d4d8b9c\"".to_owned(),
    ] {
        assert!(lock.contains(&wanted), "{wanted} not in {lock}");
    }
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
        ("trace".to_owned(), fs::read(&trace)?),
    ];
    kept_files(&work, &mut surfaces)?;
    kept_files(&sandbox.cache(), &mut surfaces)?;
    assert_no_secret(&surfaces);
    Ok(())
}

/// a teammate's sync, with only data.toml and data.lock, a sharing link in its settings and a
/// login: each file's download is refused without a token, so the listed folder's page is asked,
/// refused too, and drive's api lists the folder once, with one token, before the files come
/// from drive's api. nothing the lock names gets a token unless the listing found it there.
#[test]
fn a_teammate_finds_a_private_folders_files_through_the_same_refusal() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = drive()?;
    add_lab(
        &sandbox,
        &standin,
        &sandbox.folder("traces")?.join("add.json"),
    )?;
    let before = standin.requests().len();
    let (work, clone) = (sandbox.work(), sandbox.folder("clone")?);
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    let synced = command(&sandbox, &standin, &clone, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .env(
            "FETCHLOOM_DRIVE_FOLDERS",
            format!("https://drive.google.com/drive/folders/{ROOT}?usp=sharing"),
        )
        .env("CLOUDSDK_CONFIG", login(&sandbox)?)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stdout)?);
    assert_eq!(fs::read(clone.join("data/raw/participants.tsv"))?, A_BYTES);
    assert_eq!(fs::read(clone.join("data/raw/scans/scans.tsv"))?, B_BYTES);
    let again = &sent(&standin)[before..];
    assert_eq!(
        again.iter().filter(|(path, ..)| path == "/token").count(),
        1,
        "{again:?}"
    );
    let login_token = Some(LOGIN_TOKEN.to_owned());
    for (path, token, about) in [
        ("/drive.usercontent.google.com/download".to_owned(), None, A),
        (
            "/drive.google.com/embeddedfolderview".to_owned(),
            None,
            ROOT,
        ),
        (format!("{API_PATH}/{ROOT}"), login_token.clone(), ROOT),
        (format!("{API_PATH}/{A}"), login_token.clone(), A),
        (format!("{API_PATH}/{B}"), login_token.clone(), B),
    ] {
        let wanted = (path, token, about.to_owned());
        assert!(again.contains(&wanted), "{wanted:?} not in {again:?}");
    }
    tokens_only_for(&standin, &[ROOT, SUB, A, B]);
    let mut surfaces = vec![
        ("sync json".to_owned(), synced.stdout),
        ("sync stderr".to_owned(), synced.stderr),
    ];
    kept_files(&clone, &mut surfaces)?;
    kept_files(&sandbox.folder("teammate-cache")?, &mut surfaces)?;
    assert_no_secret(&surfaces);
    Ok(())
}

/// a private file shared on its own, listed by its id: its download is refused without a
/// token, so drive's api names it and sends it, with a login's token.
#[test]
fn a_listed_private_file_is_named_and_sent_by_drives_api() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = drive()?;
    let output = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &[
            "add",
            &format!("https://drive.google.com/file/d/{LONE}/view"),
            "--as",
            "protocol",
        ],
    )
    .env("FETCHLOOM_DRIVE_FOLDERS", LONE)
    .env("CLOUDSDK_CONFIG", login(&sandbox)?)
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let login_token = Some(LOGIN_TOKEN.to_owned());
    assert_eq!(
        sent(&standin),
        [
            (
                "/drive.usercontent.google.com/download".to_owned(),
                None,
                LONE.to_owned()
            ),
            ("/token".to_owned(), None, "token".to_owned()),
            (
                format!("{API_PATH}/{LONE}"),
                login_token.clone(),
                LONE.to_owned()
            ),
            (format!("{API_PATH}/{LONE}"), login_token, LONE.to_owned()),
        ]
    );
    assert_eq!(
        fs::read(sandbox.work().join("data/protocol/protocol.md"))?,
        LONE_BYTES
    );
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
    ];
    kept_files(&sandbox.work(), &mut surfaces)?;
    assert_no_secret(&surfaces);
    Ok(())
}

/// a folder nobody listed never gets a token, though credentials are on the machine: only the
/// listed folder is searched for it, with the token going to requests about that folder alone.
/// one listed without credentials says how to sign in for drive. neither changes data.toml.
#[test]
fn a_folder_gets_a_token_only_when_listed_and_signed_in() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = drive()?;
    let key = service_account(&sandbox)?;
    let add = |listed: Option<&str>, key: Option<&std::path::Path>| -> Result<serde_json::Value> {
        let mut command = command(
            &sandbox,
            &standin,
            &sandbox.work(),
            &["--json", "add", &format!("drive:folders/{ROOT}")],
        );
        if let Some(listed) = listed {
            command.env("FETCHLOOM_DRIVE_FOLDERS", listed);
        }
        if let Some(key) = key {
            command.env("GOOGLE_APPLICATION_CREDENTIALS", key);
        }
        let output = command.output()?;
        assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
        Ok(json(&output)?["errors"][0].clone())
    };
    let unlisted = add(None, Some(&key))?;
    assert_eq!(unlisted["kind"], "auth.required");
    assert_eq!(
        unlisted["message"],
        format!(
            "https://drive.google.com/drive/folders/{ROOT} is not shared with anyone who has the link"
        )
    );
    assert_eq!(
        unlisted["help"],
        format!(
            "ask its owner to share it with anyone who has the link. if it is shared with your google account or a service account of yours, and you trust it with your google credentials, add {ROOT} to drive_folders in the settings file or FETCHLOOM_DRIVE_FOLDERS"
        )
    );
    assert_eq!(
        sent(&standin),
        [(
            "/drive.google.com/embeddedfolderview".to_owned(),
            None,
            ROOT.to_owned()
        )]
    );
    let elsewhere = add(Some(OTHER), Some(&key))?;
    assert_eq!(elsewhere["message"], unlisted["message"]);
    tokens_only_for(&standin, &[OTHER, C]);
    assert!(
        sent(&standin)
            .iter()
            .any(|(path, token, about)| path == API_PATH && token.is_some() && about == OTHER)
    );
    let signed_out = add(Some(ROOT), None)?;
    assert_eq!(
        signed_out["message"],
        format!(
            "https://drive.google.com/drive/folders/{ROOT} is not shared with anyone who has the link, and no google application default credentials were found"
        )
    );
    assert_eq!(
        signed_out["help"],
        "set GOOGLE_APPLICATION_CREDENTIALS to the key file of a service account it is shared with, or sign in with an oauth client of your own: `gcloud auth application-default login --client-id-file=CLIENT_FILE --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform`. how: https://nikhil-yn-aix.github.io/fetchloom/user/sources/google-drive.html"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// a login without a drive scope, as gcloud's own client gives, says so and names the command
/// that grants one; a service account drive does not share the folder with says whom to share
/// it with. neither token reaches the output.
#[test]
fn a_refusal_names_the_account_and_what_to_do() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = drive()?;
    let folder = login(&sandbox)?;
    let file = folder.join("application_default_credentials.json");
    let saved = fs::read_to_string(&file)?;
    fs::write(&file, saved.replace(REFRESH_TOKEN, CLOUD_LOGIN))?;
    let narrow = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["--json", "add", &format!("drive:folders/{ROOT}")],
    )
    .env("FETCHLOOM_DRIVE_FOLDERS", ROOT)
    .env("CLOUDSDK_CONFIG", &folder)
    .output()?;
    assert_eq!(narrow.status.code(), Some(40), "{}", text(&narrow.stdout)?);
    let error = &json(&narrow)?["errors"][0];
    assert_eq!(
        error["message"],
        format!(
            "google drive refused the google login in {} for https://drive.google.com/open?id={ROOT}: its token lacks the drive.readonly scope",
            file.display()
        )
    );
    assert_eq!(
        error["help"],
        "sign in again with an oauth client of your own, since google keeps gcloud's own from drive: `gcloud auth application-default login --client-id-file=CLIENT_FILE --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform`. how: https://nikhil-yn-aix.github.io/fetchloom/user/sources/google-drive.html"
    );
    let key = service_account(&sandbox)?;
    let unshared = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["--json", "add", &format!("drive:folders/{UNSHARED}")],
    )
    .env("FETCHLOOM_DRIVE_FOLDERS", UNSHARED)
    .env("GOOGLE_APPLICATION_CREDENTIALS", &key)
    .output()?;
    assert_eq!(
        unshared.status.code(),
        Some(40),
        "{}",
        text(&unshared.stdout)?
    );
    let error = &json(&unshared)?["errors"][0];
    assert_eq!(
        error["message"],
        format!(
            "google drive has no file or folder https://drive.google.com/open?id={UNSHARED} that the service account {EMAIL} in {} can read",
            key.display()
        )
    );
    assert_eq!(
        error["help"],
        format!(
            "share it with {EMAIL} as a viewer, then run `fetchloom --json add drive:folders/{UNSHARED}` again"
        )
    );
    fs::write(
        &file,
        saved.replace(REFRESH_TOKEN, "refresh-token-google-revoked"),
    )?;
    let revoked = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["--json", "add", &format!("drive:folders/{ROOT}")],
    )
    .env("FETCHLOOM_DRIVE_FOLDERS", ROOT)
    .env("CLOUDSDK_CONFIG", &folder)
    .output()?;
    let error = &json(&revoked)?["errors"][0];
    assert_eq!(
        error["message"],
        format!("google refused the google login in {}", file.display())
    );
    assert_eq!(
        error["help"],
        "sign in again with an oauth client of your own, as drive needs: `gcloud auth application-default login --client-id-file=CLIENT_FILE --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform`. how: https://nikhil-yn-aix.github.io/fetchloom/user/sources/google-drive.html"
    );
    let said = format!(
        "{}{}{}{}",
        text(&narrow.stdout)?,
        text(&narrow.stderr)?,
        text(&unshared.stdout)?,
        text(&unshared.stderr)?
    );
    for secret in [CLOUD_LOGIN, CLOUD_TOKEN, DRIVE_SERVICE_TOKEN] {
        assert!(!said.contains(secret), "{said}");
    }
    Ok(())
}
