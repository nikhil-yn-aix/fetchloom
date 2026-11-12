//! private cloud storage buckets read with google's application default credentials, against
//! a server on this machine that stands in for google's token endpoint and for cloud storage.
#![cfg(debug_assertions)]

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::support::{Result, Sandbox, json, text};

const PARTICIPANTS: &[u8] = b"participant_id\tsex\nsub-01\tF\n";
pub(crate) const EMAIL: &str = "loader@lab-project.iam.gserviceaccount.com";
const KEY: &str = include_str!("../fixtures/sources/google/test-key.pem");
const PUBLIC_KEY: &[u8] = include_bytes!("../fixtures/sources/google/test-key.public.der");
const CLIENT_ID: &str = "client-of-the-test.apps.googleusercontent.com";
pub(crate) const CLIENT_SECRET: &str = "client-secret-of-the-test";
pub(crate) const REFRESH_TOKEN: &str = "refresh-token-of-the-test";
/// the token the stand-in gives a service account, and a login.
const SERVICE_TOKEN: &str = "ya29.service-token-of-the-test";
const LOGIN_TOKEN: &str = "ya29.login-token-of-the-test";
/// a login granted no scope that reads cloud storage, and the token it gets.
const NARROW_LOGIN: &str = "refresh-token-without-the-storage-scope";
const NARROW_TOKEN: &str = "ya29.narrow-token-of-the-test";
/// a login granted cloud storage but not drive, as gcloud's own client grants, and its token.
const CLOUD_LOGIN: &str = "refresh-token-without-the-drive-scope";
const CLOUD_TOKEN: &str = "ya29.cloud-token-of-the-test";
/// the token the stand-in gives a service account that asked for the drive scope.
const DRIVE_SERVICE_TOKEN: &str = "ya29.drive-service-token-of-the-test";
const STORAGE_SCOPE: &str = "https://www.googleapis.com/auth/devstorage.read_only";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
/// the start every signed assertion has: `{"alg":"RS256",` in base64.
const ASSERTION: &str = "eyJhbGciOiJSUzI1NiIs";
const LISTING: &str = "/storage.googleapis.com/storage/v1/b/lab-private/o?prefix=raw%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
const OBJECT: &str = "/storage.googleapis.com/lab-private/raw/a.csv?generation=1700000000000001";

/// one request the stand-in took.
#[derive(Clone, Debug)]
struct Asked {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}

/// a server on this machine that answers with what `answer` makes of each request, one
/// connection at a time, and keeps every request it took.
struct Standin {
    url: String,
    asked: Arc<Mutex<Vec<Asked>>>,
    stop: Arc<AtomicBool>,
    running: Option<JoinHandle<()>>,
}

impl Standin {
    fn start(answer: impl Fn(&Asked) -> Vec<u8> + Send + 'static) -> Result<Standin> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}", listener.local_addr()?);
        let asked = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (asked.clone(), stop.clone());
        let running = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                seen.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(request.clone());
                let _ = stream.write_all(&answer(&request));
                let _ = stream.flush();
            }
        });
        Ok(Standin {
            url,
            asked,
            stop,
            running: Some(running),
        })
    }

    fn requests(&self) -> Vec<Asked> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// each request as its method, target and the token it carried.
    fn sent(&self) -> Vec<(String, String, Option<String>)> {
        self.requests()
            .into_iter()
            .map(|asked| {
                let token = asked
                    .headers
                    .get("authorization")
                    .map(|value| value.trim_start_matches("Bearer ").to_owned());
                (asked.method, asked.target, token)
            })
            .collect()
    }
}

impl Drop for Standin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(running) = self.running.take() {
            let _ = running.join();
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Asked> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .get("content-length")
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some(Asked {
        method,
        target,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// a whole response that closes its connection.
fn respond(status: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// cloud storage refusing a request, as it words one without credentials.
fn refused() -> Vec<u8> {
    respond(
        "401 Unauthorized",
        br#"{"error":{"code":401,"message":"Anonymous caller does not have storage.objects.list access to the Google Cloud Storage bucket.","errors":[{"message":"Anonymous caller does not have storage.objects.list access to the Google Cloud Storage bucket.","domain":"global","reason":"required"}]}}"#,
    )
}

/// cloud storage refusing a token without the scope that reads it, as google words that.
fn narrow() -> Vec<u8> {
    let body = br#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED"}}"#;
    let mut response = format!(
        "HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Type: application/json\r\nWWW-Authenticate: Bearer realm=\"https://accounts.google.com/\", error=\"insufficient_scope\", scope=\"https://www.googleapis.com/auth/devstorage.read_only\"\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// base64 with the URL alphabet and no padding, as a JSON web token's parts are written.
fn decode(text: &str) -> Option<Vec<u8>> {
    let (mut bits, mut held, mut bytes) = (0u32, 0, Vec::new());
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from((bits >> held) & 0xff).ok()?);
        }
    }
    Some(bytes)
}

/// what is wrong with a service account's assertion, as google's token endpoint checks it: its
/// signature under the test key's public half, its issuer, audience, scope and hour of life.
fn assertion_fault(assertion: &str) -> Option<String> {
    let parts: Vec<&str> = assertion.split('.').collect();
    let [header, claims, signature] = parts.as_slice() else {
        return Some("not three parts".to_owned());
    };
    let key = ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA256,
        PUBLIC_KEY,
    );
    let signed = format!("{header}.{claims}");
    if key.verify(signed.as_bytes(), &decode(signature)?).is_err() {
        return Some("Invalid JWT Signature.".to_owned());
    }
    let header: serde_json::Value = serde_json::from_slice(&decode(header)?).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decode(claims)?).ok()?;
    let lifetime = claims["exp"]
        .as_u64()?
        .checked_sub(claims["iat"].as_u64()?)?;
    let expected = header["alg"] == "RS256"
        && header["kid"] == "test-key-id"
        && claims["iss"] == EMAIL
        && claims["aud"] == "https://oauth2.googleapis.com/token"
        && [STORAGE_SCOPE, DRIVE_SCOPE].contains(&claims["scope"].as_str().unwrap_or_default())
        && lifetime == 3600;
    (!expected).then(|| format!("unexpected assertion {header} {claims}"))
}

/// the scope a service account's assertion asks for.
fn assertion_scope(assertion: &str) -> Option<String> {
    let claims = decode(assertion.split('.').nth(1)?)?;
    let claims: serde_json::Value = serde_json::from_slice(&claims).ok()?;
    claims["scope"].as_str().map(str::to_owned)
}

/// google's token endpoint: a token for a well signed assertion or the test's login, and
/// `invalid_grant` for anything else.
fn token(asked: &Asked) -> Vec<u8> {
    let form: HashMap<String, String> = url::form_urlencoded::parse(asked.body.as_bytes())
        .into_owned()
        .collect();
    let field = |name: &str| form.get(name).map_or("", String::as_str);
    let fault = match field("grant_type") {
        "urn:ietf:params:oauth:grant-type:jwt-bearer" => assertion_fault(field("assertion")),
        "refresh_token"
            if field("client_id") == CLIENT_ID
                && field("client_secret") == CLIENT_SECRET
                && [REFRESH_TOKEN, NARROW_LOGIN, CLOUD_LOGIN].contains(&field("refresh_token")) =>
        {
            None
        }
        _ => Some("Bad Request".to_owned()),
    };
    if let Some(fault) = fault {
        return respond(
            "400 Bad Request",
            serde_json::json!({"error": "invalid_grant", "error_description": fault})
                .to_string()
                .as_bytes(),
        );
    }
    let token = match (field("grant_type"), field("refresh_token")) {
        ("refresh_token", NARROW_LOGIN) => NARROW_TOKEN,
        ("refresh_token", CLOUD_LOGIN) => CLOUD_TOKEN,
        ("refresh_token", _) => LOGIN_TOKEN,
        _ if assertion_scope(field("assertion")).as_deref() == Some(DRIVE_SCOPE) => {
            DRIVE_SERVICE_TOKEN
        }
        _ => SERVICE_TOKEN,
    };
    respond(
        "200 OK",
        serde_json::json!({"access_token": token, "expires_in": 3599, "token_type": "Bearer"})
            .to_string()
            .as_bytes(),
    )
}

/// the token endpoint and the private bucket `lab-private`, which lists and serves `raw/a.csv`
/// to a request with a token the endpoint gave, and refuses the narrow token for its scope, as
/// google does.
fn google() -> Result<Standin> {
    let page = serde_json::json!({"items": [{
        "name": "raw/a.csv",
        "size": PARTICIPANTS.len().to_string(),
        "generation": "1700000000000001",
    }]})
    .to_string();
    Standin::start(move |asked| {
        if asked.method == "POST" && asked.target == "/token" {
            return token(asked);
        }
        let carried = asked.headers.get("authorization").map(String::as_str);
        if carried == Some(&format!("Bearer {NARROW_TOKEN}")) {
            return narrow();
        }
        let signed_in = [SERVICE_TOKEN, LOGIN_TOKEN]
            .iter()
            .any(|token| carried == Some(&format!("Bearer {token}")));
        match asked.target.as_str() {
            _ if !signed_in => refused(),
            LISTING => respond("200 OK", page.as_bytes()),
            OBJECT => {
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\nETag: \"v1\"\r\n\r\n",
                    PARTICIPANTS.len()
                )
                .into_bytes();
                response.extend_from_slice(PARTICIPANTS);
                response
            }
            _ => respond("404 Not Found", b"{}"),
        }
    })
}

/// a command whose cloud storage and token requests go to `standin`.
fn command(
    sandbox: &Sandbox,
    standin: &Standin,
    folder: &std::path::Path,
    args: &[&str],
) -> Command {
    let mut command = sandbox.command_in(folder, args);
    command.env("FETCHLOOM_TEST_BUCKETS", &standin.url).env(
        "FETCHLOOM_TEST_GOOGLE_TOKEN",
        format!("{}/token", standin.url),
    );
    command
}

/// a service account's key in the sandbox, as the google cloud console downloads it.
pub(crate) fn service_account(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let path = sandbox.folder("keys")?.join("lab.json");
    let key = serde_json::json!({
        "type": "service_account",
        "project_id": "lab-project",
        "private_key_id": "test-key-id",
        "private_key": KEY,
        "client_email": EMAIL,
        "client_id": "100000000000000000001",
        "token_uri": "https://oauth2.googleapis.com/token",
    });
    fs::write(&path, key.to_string())?;
    Ok(path)
}

/// a person's login in a gcloud folder of the sandbox, as `gcloud auth application-default
/// login` saves it.
pub(crate) fn login(sandbox: &Sandbox) -> Result<std::path::PathBuf> {
    let folder = sandbox.folder("gcloud")?;
    let login = serde_json::json!({
        "account": "",
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
        "refresh_token": REFRESH_TOKEN,
        "type": "authorized_user",
        "universe_domain": "googleapis.com",
    });
    fs::write(
        folder.join("application_default_credentials.json"),
        login.to_string(),
    )?;
    Ok(folder)
}

/// every file below `folder`, which no secret may reach.
fn kept_files(folder: &std::path::Path, found: &mut Vec<(String, Vec<u8>)>) -> Result {
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if path.is_dir() {
            kept_files(&path, found)?;
        } else {
            found.push((path.display().to_string(), fs::read(&path)?));
        }
    }
    Ok(())
}

fn assert_no_secret(surfaces: &[(String, Vec<u8>)]) {
    let key_line = KEY.lines().nth(3).unwrap_or_default();
    for (name, bytes) in surfaces {
        let said = String::from_utf8_lossy(bytes);
        for secret in [
            SERVICE_TOKEN,
            DRIVE_SERVICE_TOKEN,
            LOGIN_TOKEN,
            REFRESH_TOKEN,
            CLOUD_LOGIN,
            CLOUD_TOKEN,
            CLIENT_SECRET,
            ASSERTION,
            key_line,
        ] {
            assert!(!said.contains(secret), "{name} holds {secret}: {said}");
        }
    }
}

/// a private bucket the person lists: the listing goes without credentials, is refused, and
/// goes again with a token a service account's signed assertion got; the object carries the
/// token from the start. a teammate's sync reads it with a gcloud login. no secret reaches an
/// output, a log, a trace or a file.
#[test]
fn a_listed_private_bucket_is_read_with_application_default_credentials() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = google()?;
    let key = service_account(&sandbox)?;
    let trace = sandbox.folder("traces")?.join("add.json");
    let output = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["-vv", "add", "gs://lab-private/raw/", "--as", "raw"],
    )
    .env("FETCHLOOM_GCS_BUCKETS", "gs://lab-private")
    .env("GOOGLE_APPLICATION_CREDENTIALS", &key)
    .env("FETCHLOOM_TRACE", &trace)
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let service = Some(SERVICE_TOKEN.to_owned());
    assert_eq!(
        standin.sent(),
        [
            ("GET".to_owned(), LISTING.to_owned(), None),
            ("POST".to_owned(), "/token".to_owned(), None),
            ("GET".to_owned(), LISTING.to_owned(), service.clone()),
            ("GET".to_owned(), OBJECT.to_owned(), service),
        ]
    );
    let work = sandbox.work();
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://storage.googleapis.com/lab-private/raw/a.csv?generation=1700000000000001\""),
        "{lock}"
    );
    assert_eq!(fs::read(work.join("data/raw/a.csv"))?, PARTICIPANTS);
    let mut surfaces = vec![
        ("stdout".to_owned(), output.stdout),
        ("stderr".to_owned(), output.stderr),
        ("trace".to_owned(), fs::read(&trace)?),
    ];
    kept_files(&work, &mut surfaces)?;
    kept_files(&sandbox.cache(), &mut surfaces)?;

    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), clone.join(name))?;
    }
    let synced = command(&sandbox, &standin, &clone, &["--json", "sync", "--locked"])
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .env("FETCHLOOM_GCS_BUCKETS", "lab-private")
        .env("CLOUDSDK_CONFIG", login(&sandbox)?)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stdout)?);
    assert_eq!(fs::read(clone.join("data/raw/a.csv"))?, PARTICIPANTS);
    assert_eq!(
        standin.sent()[4..],
        [
            ("GET".to_owned(), OBJECT.to_owned(), None),
            ("POST".to_owned(), "/token".to_owned(), None),
            (
                "GET".to_owned(),
                OBJECT.to_owned(),
                Some(LOGIN_TOKEN.to_owned())
            ),
        ]
    );
    surfaces.push(("sync json".to_owned(), synced.stdout));
    surfaces.push(("sync stderr".to_owned(), synced.stderr));
    kept_files(&clone, &mut surfaces)?;
    kept_files(&sandbox.folder("teammate-cache")?, &mut surfaces)?;
    assert_no_secret(&surfaces);
    Ok(())
}

/// a bucket nobody listed never gets a token, though credentials are on the machine, and a
/// listed one without credentials says how to sign in. neither asks the token endpoint.
#[test]
fn a_bucket_gets_a_token_only_when_listed_and_signed_in() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = google()?;
    let key = service_account(&sandbox)?;
    let unlisted = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["--json", "add", "gs://lab-private/raw/"],
    )
    .env("FETCHLOOM_GCS_BUCKETS", "gs://another-lab")
    .env("GOOGLE_APPLICATION_CREDENTIALS", &key)
    .output()?;
    assert_eq!(
        unlisted.status.code(),
        Some(40),
        "{}",
        text(&unlisted.stdout)?
    );
    let error = &json(&unlisted)?["errors"][0];
    assert_eq!(error["kind"], "auth.required");
    assert_eq!(
        error["message"],
        "gs://lab-private/raw/ is private: google cloud storage refused to list it without credentials"
    );
    assert_eq!(
        error["help"],
        "if you trust gs://lab-private with your google credentials, add it to gcs_buckets in the settings file or FETCHLOOM_GCS_BUCKETS"
    );
    let signed_out = command(
        &sandbox,
        &standin,
        &sandbox.work(),
        &["--json", "add", "gs://lab-private/raw/"],
    )
    .env("FETCHLOOM_GCS_BUCKETS", "gs://lab-private")
    .output()?;
    assert_eq!(signed_out.status.code(), Some(40));
    let error = &json(&signed_out)?["errors"][0];
    assert_eq!(
        error["message"],
        "gs://lab-private/raw/ is private: google cloud storage refused to list it without credentials, and no google application default credentials were found"
    );
    assert_eq!(
        error["help"],
        "run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file"
    );
    assert_eq!(
        standin.sent(),
        [
            ("GET".to_owned(), LISTING.to_owned(), None),
            ("GET".to_owned(), LISTING.to_owned(), None),
        ]
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// a login google no longer takes, as a revoked one, stops the run with what google said and
/// the command that signs in again; one whose token lacks the storage scope says how to grant
/// it. nothing of the login reaches the output.
#[test]
fn a_login_google_refuses_or_without_the_scope_says_how_to_sign_in() -> Result {
    let sandbox = Sandbox::new()?;
    let standin = google()?;
    let folder = login(&sandbox)?;
    let file = folder.join("application_default_credentials.json");
    let saved = fs::read_to_string(&file)?;
    let run = |refresh_token: &str| -> Result<(serde_json::Value, String)> {
        fs::write(&file, saved.replace(REFRESH_TOKEN, refresh_token))?;
        let output = command(
            &sandbox,
            &standin,
            &sandbox.work(),
            &["--json", "-vv", "add", "gs://lab-private/raw/"],
        )
        .env("FETCHLOOM_GCS_BUCKETS", "gs://lab-private")
        .env("CLOUDSDK_CONFIG", &folder)
        .output()?;
        assert_eq!(output.status.code(), Some(40), "{}", text(&output.stdout)?);
        let said = format!("{}{}", text(&output.stdout)?, text(&output.stderr)?);
        Ok((json(&output)?["errors"][0].clone(), said))
    };
    let (revoked, revoked_said) = run("refresh-token-google-revoked")?;
    assert_eq!(revoked["kind"], "auth.required");
    assert_eq!(
        revoked["message"],
        format!("google refused the google login in {}", file.display())
    );
    assert_eq!(
        revoked["details"][0],
        "google said invalid_grant: Bad Request"
    );
    assert_eq!(
        revoked["help"],
        "sign in again with `gcloud auth application-default login`"
    );
    assert_eq!(standin.sent().len(), 2);
    let (narrow, narrow_said) = run(NARROW_LOGIN)?;
    assert_eq!(
        narrow["message"],
        format!(
            "google cloud storage refused the google login in {} for gs://lab-private/raw/: its token lacks the scope that reads cloud storage",
            file.display()
        )
    );
    assert_eq!(
        narrow["help"],
        "a login needs the cloud-platform scope: sign in again with `gcloud auth application-default login`, with no --scopes, or with https://www.googleapis.com/auth/cloud-platform among them"
    );
    assert_eq!(
        standin.sent()[2..],
        [
            ("GET".to_owned(), LISTING.to_owned(), None),
            ("POST".to_owned(), "/token".to_owned(), None),
            (
                "GET".to_owned(),
                LISTING.to_owned(),
                Some(NARROW_TOKEN.to_owned())
            ),
        ]
    );
    for secret in [
        "refresh-token-google-revoked",
        NARROW_LOGIN,
        NARROW_TOKEN,
        CLIENT_SECRET,
    ] {
        assert!(!revoked_said.contains(secret), "{revoked_said}");
        assert!(!narrow_said.contains(secret), "{narrow_said}");
    }
    Ok(())
}

mod drive;
