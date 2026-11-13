use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::path::Path;

use super::*;
use crate::net::scripted::reply;
use crate::net::{Offline, Options, Splitting};
use crate::source::recorded::block_on;

/// one request a test server took: its request line and head in lowercase, and its body.
#[derive(Clone, Debug)]
pub(super) struct Asked {
    line: String,
    head: String,
    body: String,
}

impl Asked {
    /// the `authorization` this request carried, if any.
    pub(super) fn authorization(&self) -> Option<&str> {
        self.head
            .lines()
            .find_map(|line| line.strip_prefix("authorization:"))
            .map(str::trim)
    }
}

/// a server on this machine that answers each request with what `answer` makes of it and of how
/// many came before it, and keeps every request it took.
pub(super) fn server(
    answer: impl Fn(&Asked, usize) -> Vec<u8> + Send + 'static,
) -> (Url, Arc<Mutex<Vec<Asked>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|length| length.trim().parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; length];
            let _ = stream.read_exact(&mut body);
            let request = Asked {
                line: head.lines().next().unwrap_or_default().to_owned(),
                head: head.clone(),
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            let count = {
                let mut seen = seen.lock().unwrap();
                seen.push(request.clone());
                seen.len() - 1
            };
            let _ = stream.write_all(&answer(&request, count));
        }
    });
    (url, asked)
}

/// a token endpoint that gives `token-0`, `token-1` and so on, each for an hour.
pub(super) fn tokens() -> (Url, Arc<Mutex<Vec<Asked>>>) {
    server(|_, count| {
        reply(
            "200 OK",
            &[("Content-Type", "application/json")],
            format!(
                r#"{{"access_token":"token-{count}","expires_in":3600,"token_type":"Bearer"}}"#
            )
            .as_bytes(),
        )
    })
}

/// a person's login, as gcloud saves it, in `folder`.
pub(super) fn login(folder: &Path) -> std::path::PathBuf {
    let path = folder.join("application_default_credentials.json");
    std::fs::write(
        &path,
        r#"{"type":"authorized_user","client_id":"client.apps.googleusercontent.com","client_secret":"client-secret-of-the-test","refresh_token":"refresh-token-of-the-test"}"#,
    )
    .unwrap();
    path
}

/// a network that sends google tokens for the `listed` buckets, from the credentials at
/// `credentials`, asked of the token endpoint at `endpoint`.
fn google_net(listed: &[&str], credentials: Option<&Path>, endpoint: &Url) -> Net {
    let listed = listed
        .iter()
        .map(|text| GcsBucket::parse(text).unwrap())
        .collect();
    let named = credentials.map(|path| path.as_os_str().to_owned());
    let google = Google::new(
        listed,
        Arc::new(move |name: &str| {
            (name == "GOOGLE_APPLICATION_CREDENTIALS")
                .then(|| named.clone())
                .flatten()
        }),
    )
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

/// a client that carries the requests `send` shapes to a local server. it installs the crypto
/// provider itself, since fetchloom installs it only when it first goes online.
pub(super) fn transport() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// sends a GET of `link` through `net`, carried to `local` with every header `net` gave it.
pub(super) async fn sent(
    net: &Net,
    client: &reqwest::Client,
    local: &Url,
    link: &str,
) -> Result<reqwest::Response, Failure> {
    let url = Url::parse(link).unwrap();
    net.send(&url, |request| {
        let request = request.build().unwrap();
        let target = local.join(request.url().path()).unwrap();
        client.get(target).headers(request.headers().clone())
    })
    .await
}

/// [`sent`], answered.
pub(super) async fn through(
    net: &Net,
    client: &reqwest::Client,
    local: &Url,
    link: &str,
) -> reqwest::Response {
    match sent(net, client, local, link).await {
        Ok(response) => response,
        Err(Failure::Final(error)) => panic!("{link}: {}", error.message),
        Err(Failure::Transient { reason, .. } | Failure::Limited { reason, .. }) => {
            panic!("{link} failed for now: {reason}")
        }
    }
}

pub(super) fn authorizations(asked: &Mutex<Vec<Asked>>) -> Vec<Option<String>> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.authorization().map(str::to_owned))
        .collect()
}

#[test]
fn a_token_goes_to_cloud_storage_over_https_alone() {
    let google = Google::new(
        vec![GcsBucket::parse("lab-private").unwrap()],
        Arc::new(|_: &str| None),
    );
    for (link, bucket, listed) in [
        (
            "https://storage.googleapis.com/lab-private/raw/a.csv?generation=1",
            "lab-private",
            true,
        ),
        (
            "https://storage.googleapis.com:443/storage/v1/b/lab-private/o?prefix=raw%2F",
            "lab-private",
            true,
        ),
        (
            "https://storage.googleapis.com/download/storage/v1/b/lab-private/o/a.csv?alt=media",
            "lab-private",
            true,
        ),
        (
            "https://STORAGE.googleapis.com/other_bucket/a.csv",
            "other_bucket",
            false,
        ),
    ] {
        let located = google.locate(&Url::parse(link).unwrap()).unwrap();
        assert_eq!(located.bucket(), Some((bucket, listed)), "{link}");
    }
    for link in [
        "http://storage.googleapis.com/lab-private/a.csv",
        "https://storage.googleapis.com:8443/lab-private/a.csv",
        "https://lab-private.storage.googleapis.com/a.csv",
        "https://storage.googleapis.com.lab.example/lab-private/a.csv",
        "https://www.googleapis.com/storage/v1/b/lab-private/o",
        "https://storage.googleapis.com/upload/storage/v1/b/lab-private/o",
        "https://storage.googleapis.com/batch/storage/v1",
        "https://storage.googleapis.com/storage/v1/b",
        "https://storage.googleapis.com/",
        "https://storage.googleapis.com/Lab-Private/a.csv",
        "https://storage.googleapis.com/lab-private/a.csv?X-Goog-Signature=abc&X-Goog-Credential=x",
        "https://storage.googleapis.com/lab-private/a.csv?access_token=abc",
        "https://s3.amazonaws.com/lab-private/a.csv",
    ] {
        assert!(
            google.locate(&Url::parse(link).unwrap()).is_none(),
            "{link}"
        );
    }
}

/// four downloads at once, all with the one token a single request to the token endpoint got. a
/// bucket nobody listed never gets it.
#[test]
fn a_listed_bucket_refused_without_a_token_is_asked_again_with_one() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|request, _| match request.authorization() {
        Some("bearer token-0") => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b"{}"),
    });
    let folder = tempfile::tempdir().unwrap();
    let net = google_net(
        &["gs://lab-private"],
        Some(&login(folder.path())),
        &endpoint,
    );
    let client = transport();
    let link = |name: &str| format!("https://storage.googleapis.com/lab-private/raw/{name}");
    let links = ["a.csv", "b.csv", "c.csv", "d.csv"].map(link);
    let answers = block_on(async {
        let (a, b, c, d) = tokio::join!(
            through(&net, &client, &local, &links[0]),
            through(&net, &client, &local, &links[1]),
            through(&net, &client, &local, &links[2]),
            through(&net, &client, &local, &links[3]),
        );
        [a, b, c, d]
    });
    let identity = format!(
        "the google login in {}",
        folder
            .path()
            .join("application_default_credentials.json")
            .display()
    );
    for answer in &answers {
        assert_eq!(answer.status(), 200);
        assert_eq!(
            answer.extensions().get::<Authorized>(),
            Some(&Authorized::Token {
                bucket: "lab-private".to_owned(),
                identity: identity.clone(),
                lacks_scope: false,
            })
        );
    }
    let sent = authorizations(&asked);
    assert_eq!(
        sent.iter().filter(|sent| sent.is_none()).count(),
        4,
        "{sent:?}"
    );
    assert_eq!(
        sent.iter()
            .filter(|sent| sent.as_deref() == Some("bearer token-0"))
            .count(),
        4,
        "{sent:?}"
    );
    let token_requests = asked_for_tokens.lock().unwrap().clone();
    assert_eq!(token_requests.len(), 1);
    assert_eq!(token_requests[0].line, "post / http/1.1");
    assert!(
        token_requests[0]
            .head
            .contains("content-type: application/x-www-form-urlencoded"),
        "{}",
        token_requests[0].head
    );
    assert!(
        token_requests[0]
            .body
            .starts_with("grant_type=refresh_token&"),
        "{}",
        token_requests[0].body
    );
    // the bucket's later requests carry the token from the start; another bucket never does
    let later = block_on(through(&net, &client, &local, &link("e.csv")));
    assert_eq!(later.status(), 200);
    let other = block_on(through(
        &net,
        &client,
        &local,
        "https://storage.googleapis.com/lab-other/raw/a.csv",
    ));
    assert_eq!(other.status(), 401);
    assert_eq!(
        other.extensions().get::<Authorized>(),
        Some(&Authorized::Anonymous {
            bucket: "lab-other".to_owned(),
            listed: false,
        })
    );
    let sent = authorizations(&asked);
    assert_eq!(sent.len(), 10);
    assert_eq!(sent[8].as_deref(), Some("bearer token-0"));
    assert_eq!(sent[9], None);
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 1);
    assert_eq!(net.work.counts().requests, 11);
}

#[test]
fn a_token_never_follows_a_redirect() {
    let (endpoint, _) = tokens();
    let (local, asked) = server(|_, count| match count {
        0 => reply("401 Unauthorized", &[], b""),
        1 => reply(
            "302 Found",
            &[("Location", "https://elsewhere.example/lab-private/a.csv")],
            b"",
        ),
        _ => reply("200 OK", &[], b"object"),
    });
    let folder = tempfile::tempdir().unwrap();
    let net = google_net(&["lab-private"], Some(&login(folder.path())), &endpoint);
    let client = transport();
    let answer = block_on(through(
        &net,
        &client,
        &local,
        "https://storage.googleapis.com/lab-private/a.csv",
    ));
    assert_eq!(answer.status(), 200);
    assert_eq!(
        authorizations(&asked),
        [None, Some("bearer token-0".to_owned()), None]
    );
}

/// as a 401 refuses an expired or revoked one; a second refusal is the answer.
#[test]
fn a_token_cloud_storage_refuses_is_replaced_once() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|_, count| match count {
        1 | 3 => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b""),
    });
    let folder = tempfile::tempdir().unwrap();
    let net = google_net(&["lab-private"], Some(&login(folder.path())), &endpoint);
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    let statuses: Vec<u16> = (0..3)
        .map(|_| {
            block_on(through(&net, &client, &local, link))
                .status()
                .as_u16()
        })
        .collect();
    assert_eq!(statuses, [200, 200, 401]);
    assert_eq!(
        authorizations(&asked),
        [
            None,
            Some("bearer token-0".to_owned()),
            Some("bearer token-0".to_owned()),
            Some("bearer token-1".to_owned()),
            Some("bearer token-1".to_owned()),
            Some("bearer token-2".to_owned()),
        ]
    );
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 3);
}

/// refused with 403 and google's `insufficient_scope`: no new token would get further, so the
/// refusal says how to sign in with the scope rather than to ask the owner.
#[test]
fn a_token_without_the_storage_scope_says_how_to_get_one() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|request, _| match request.authorization() {
        Some(_) => reply(
            "403 Forbidden",
            &[(
                "WWW-Authenticate",
                r#"Bearer realm="https://accounts.google.com/", error="insufficient_scope", scope="https://www.googleapis.com/auth/devstorage.read_only""#,
            )],
            b"",
        ),
        None => reply("401 Unauthorized", &[], b""),
    });
    let folder = tempfile::tempdir().unwrap();
    let net = google_net(&["lab-private"], Some(&login(folder.path())), &endpoint);
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    let answer = block_on(through(&net, &client, &local, link));
    assert_eq!(answer.status(), 403);
    let authorized = answer.extensions().get::<Authorized>().unwrap();
    let identity = format!(
        "the google login in {}",
        folder
            .path()
            .join("application_default_credentials.json")
            .display()
    );
    assert_eq!(
        authorized,
        &Authorized::Token {
            bucket: "lab-private".to_owned(),
            identity: identity.clone(),
            lacks_scope: true,
        }
    );
    let error = authorized.refusal(&Url::parse(link).unwrap());
    assert_eq!(
        error.message,
        format!(
            "google cloud storage refused {identity} for {link}: its token lacks the scope that reads cloud storage"
        )
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "a login needs the cloud-platform scope: sign in again with `gcloud auth application-default login`, with no --scopes, or with https://www.googleapis.com/auth/cloud-platform among them"
        )
    );
    assert_eq!(authorizations(&asked).len(), 2);
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 1);
}

/// with no credentials on the machine a listed bucket goes without a token and says so, and
/// credentials that cannot be read stop the request rather than leave it without a token.
#[test]
fn a_listed_bucket_without_usable_credentials_asks_no_token_endpoint() {
    let (endpoint, asked_for_tokens) = tokens();
    let (local, asked) = server(|_, _| reply("401 Unauthorized", &[], b""));
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    let net = google_net(&["lab-private"], None, &endpoint);
    let answer = block_on(through(&net, &client, &local, link));
    assert_eq!(answer.status(), 401);
    assert_eq!(
        answer.extensions().get::<Authorized>(),
        Some(&Authorized::Anonymous {
            bucket: "lab-private".to_owned(),
            listed: true,
        })
    );
    let folder = tempfile::tempdir().unwrap();
    let missing = folder.path().join("missing.json");
    let net = google_net(&["lab-private"], Some(&missing), &endpoint);
    let Err(Failure::Final(error)) = block_on(sent(&net, &client, &local, link)) else {
        panic!("credentials that cannot be read were passed over");
    };
    assert_eq!(error.kind, Kind::Usage);
    assert_eq!(authorizations(&asked), [None, None]);
    assert!(asked_for_tokens.lock().unwrap().is_empty());
}

/// and the rest of the run hears the same without asking again.
#[test]
fn a_refused_login_stops_requests_with_what_google_said() {
    let (endpoint, asked_for_tokens) = server(|_, _| {
        reply(
            "400 Bad Request",
            &[("Content-Type", "application/json")],
            br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        )
    });
    let (local, asked) = server(|_, _| reply("401 Unauthorized", &[], b""));
    let folder = tempfile::tempdir().unwrap();
    let net = google_net(&["lab-private"], Some(&login(folder.path())), &endpoint);
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    for _ in 0..2 {
        let Err(Failure::Final(error)) = block_on(sent(&net, &client, &local, link)) else {
            panic!("a refused login was passed over");
        };
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(
            error.details,
            ["google said invalid_grant: Token has been expired or revoked."]
        );
    }
    assert_eq!(authorizations(&asked), [None]);
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 1);
}
