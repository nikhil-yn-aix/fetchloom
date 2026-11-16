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
    net_with(google)
}

/// a network that sends google tokens as `google` says.
fn net_with(google: Google) -> Net {
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

/// four downloads at once share the one token a single request to the token endpoint got. a bucket
/// nobody listed never gets it.
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

/// a 401 refuses an expired or revoked token; a second refusal is the answer.
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

/// cloud storage refuses such a token with 403 and `insufficient_scope`: no new token would get
/// further, so the refusal says how to sign in with the scope rather than to ask the owner.
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

/// the rest of the run hears google's refusal without asking again.
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

/// a token answer cut off partway is asked for again, one of 64 KiB is read whole and one past it
/// ends the request, each byte counted as the run's. with no retries, the cut one ends it.
#[test]
fn a_token_answer_is_read_whole_up_to_64_kib() {
    let (endpoint, asked_for_tokens) = server(|_, count| {
        let mut body = format!(
            r#"{{"access_token":"token-{count}","expires_in":3600,"token_type":"Bearer"}}"#
        )
        .into_bytes();
        body.resize(if count == 2 { (64 << 10) + 1 } else { 64 << 10 }, b' ');
        let mut answer = reply("200 OK", &[("Content-Type", "application/json")], &body);
        if count == 0 || count == 3 {
            // the head promises a body that never comes
            answer.truncate(answer.len() - body.len());
        }
        answer
    });
    let (local, _) = server(|request, _| match request.authorization() {
        Some("bearer token-1") => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b""),
    });
    let folder = tempfile::tempdir().unwrap();
    let credentials = login(folder.path());
    let net = |retries| {
        let mut net =
            google_net(&["lab-private"], Some(&credentials), &endpoint).with_retries(retries);
        net.first_wait = Duration::ZERO;
        net
    };
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    let read = net(3);
    let answer = block_on(through(&read, &client, &local, link));
    assert_eq!(answer.status(), 200);
    assert_eq!(read.work.counts().bytes_down, 64 << 10);
    let long = net(3);
    let Err(Failure::Final(error)) = block_on(sent(&long, &client, &local, link)) else {
        panic!("a token answer past 64 KiB was read");
    };
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        "127.0.0.1 sent more than 64 KiB where a token was expected"
    );
    assert_eq!(long.work.counts().bytes_down, (64 << 10) + 1);
    let Err(Failure::Final(error)) = block_on(sent(&net(0), &client, &local, link)) else {
        panic!("a cut-off token answer was read");
    };
    assert_eq!(
        error.message,
        "gave up on 127.0.0.1 after 1 try: the connection to 127.0.0.1 broke partway through the answer"
    );
    assert_eq!(asked_for_tokens.lock().unwrap().len(), 4);
}

/// this machine's service account, as google's metadata server names it.
const MACHINE: &str = "loader@lab-project.iam.gserviceaccount.com";
/// the folder google's metadata server keeps the default service account in, lowercase as the
/// test server keeps a request.
const ACCOUNT: &str = "/computemetadata/v1/instance/service-accounts/default";

/// google's metadata server for a machine whose service account is [`MACHINE`]: its email, and
/// `machine-1`, `machine-2` and so on for each token asked, every answer saying it is google's.
fn machine_server() -> (Url, Arc<Mutex<Vec<Asked>>>) {
    server(|request, count| {
        let target = request.line.split(' ').nth(1).unwrap_or_default();
        let google = [("Metadata-Flavor", "Google")];
        if target == format!("{ACCOUNT}/email") {
            return reply("200 OK", &google, MACHINE.as_bytes());
        }
        if target.starts_with(&format!("{ACCOUNT}/token?")) {
            let token = format!(
                r#"{{"access_token":"machine-{count}","expires_in":3599,"token_type":"Bearer"}}"#
            );
            return reply("200 OK", &google, token.as_bytes());
        }
        reply("404 Not Found", &google, b"")
    })
}

/// a network without application default credentials that sends google tokens for the
/// `listed` buckets, on a machine whose metadata server is `machine`, none for one without.
fn machine_net(listed: &[&str], endpoint: &Url, machine: Option<&Url>) -> Net {
    let listed = listed
        .iter()
        .map(|text| GcsBucket::parse(text).unwrap())
        .collect();
    let google = Google::new(listed, Arc::new(|_: &str| None))
        .with_token_endpoint(Some(endpoint.clone()))
        .with_machine_server(machine.cloned());
    net_with(google)
}

/// with no application default credentials, a listed bucket's refusal asks this machine's
/// metadata server, through a client of its own with `Metadata-Flavor: Google` and nothing else,
/// for its service account and a token of the storage scope, once for the run.
#[test]
fn a_listed_bucket_without_credentials_is_read_as_the_machines_service_account() {
    let (endpoint, asked_for_tokens) = tokens();
    let (machine, asked_machine) = machine_server();
    let (local, asked) = server(|request, _| match request.authorization() {
        Some("bearer machine-1") => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b"{}"),
    });
    let net = machine_net(&["lab-private"], &endpoint, Some(&machine));
    let client = transport();
    let link = |name: &str| format!("https://storage.googleapis.com/lab-private/raw/{name}");
    for name in ["a.csv", "b.csv"] {
        let answer = block_on(through(&net, &client, &local, &link(name)));
        assert_eq!(answer.status(), 200);
        assert_eq!(
            answer.extensions().get::<Authorized>(),
            Some(&Authorized::Token {
                bucket: "lab-private".to_owned(),
                identity: format!("this machine's service account {MACHINE}"),
                lacks_scope: false,
            })
        );
    }
    let other = block_on(through(
        &net,
        &client,
        &local,
        "https://storage.googleapis.com/lab-other/raw/a.csv",
    ));
    assert_eq!(other.status(), 401);
    let token = Some("bearer machine-1".to_owned());
    assert_eq!(authorizations(&asked), [None, token.clone(), token, None]);
    let machine_asked = asked_machine.lock().unwrap().clone();
    let lines: Vec<&str> = machine_asked
        .iter()
        .map(|asked| asked.line.as_str())
        .collect();
    assert_eq!(
        lines,
        [
            format!("get {ACCOUNT}/email http/1.1"),
            format!(
                "get {ACCOUNT}/token?scopes=https%3a%2f%2fwww.googleapis.com%2fauth%2fdevstorage.read_only http/1.1"
            ),
        ]
    );
    for asked in &machine_asked {
        assert!(
            asked.head.contains("\r\nmetadata-flavor: google\r\n"),
            "{}",
            asked.head
        );
        assert!(asked.authorization().is_none(), "{}", asked.head);
        assert!(!asked.head.contains("cookie"), "{}", asked.head);
    }
    assert!(asked_for_tokens.lock().unwrap().is_empty());
}

/// a credentials file comes before the machine's metadata server, which is then never asked.
#[test]
fn application_default_credentials_come_before_the_machines_metadata_server() {
    let (endpoint, _) = tokens();
    let (machine, asked_machine) = machine_server();
    let (local, _) = server(|request, _| match request.authorization() {
        Some("bearer token-0") => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b"{}"),
    });
    let folder = tempfile::tempdir().unwrap();
    let named = login(folder.path()).into_os_string();
    let google = Google::new(
        vec![GcsBucket::parse("lab-private").unwrap()],
        Arc::new(move |name: &str| {
            (name == "GOOGLE_APPLICATION_CREDENTIALS").then(|| named.clone())
        }),
    )
    .with_token_endpoint(Some(endpoint))
    .with_machine_server(Some(machine));
    let net = net_with(google);
    let link = "https://storage.googleapis.com/lab-private/raw/a.csv";
    let answer = block_on(through(&net, &transport(), &local, link));
    assert_eq!(answer.status(), 200);
    assert!(asked_machine.lock().unwrap().is_empty());
}

/// off google cloud, where nothing answers at the metadata server's address or another cloud's
/// service answers without saying it is google's, there are no credentials: the refusal says
/// none were found, and the address is asked once a run.
#[test]
fn a_machine_without_googles_metadata_server_has_no_credentials() {
    let (endpoint, _) = tokens();
    let (local, asked) = server(|_, _| reply("401 Unauthorized", &[], b""));
    let (elsewhere, asked_elsewhere) = server(|_, _| reply("404 Not Found", &[], b""));
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap()
    };
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    for machine in [Some(&elsewhere), Some(&closed), None] {
        let net = machine_net(&["lab-private"], &endpoint, machine);
        for _ in 0..2 {
            let answer = block_on(through(&net, &client, &local, link));
            assert_eq!(answer.status(), 401);
            assert_eq!(
                answer.extensions().get::<Authorized>(),
                Some(&Authorized::Anonymous {
                    bucket: "lab-private".to_owned(),
                    listed: true,
                })
            );
        }
    }
    assert_eq!(asked_elsewhere.lock().unwrap().len(), 1);
    assert_eq!(authorizations(&asked), vec![None; 6]);
}

/// google's metadata server on a machine without a service account, or one that gives no token,
/// stops the bucket's requests with what it said.
#[test]
fn a_machine_without_a_service_account_says_so() {
    let (endpoint, _) = tokens();
    let (local, asked) = server(|_, _| reply("401 Unauthorized", &[], b""));
    let google = [("Metadata-Flavor", "Google")];
    let (bare, _) = server(move |_, _| reply("404 Not Found", &google, b"Not Found\n"));
    let (refusing, _) = server(move |request, _| {
        if request.line.contains("/email ") {
            reply("200 OK", &google, MACHINE.as_bytes())
        } else {
            reply("403 Forbidden", &google, b"forbidden")
        }
    });
    let (elsewhere, asked_elsewhere) = machine_server();
    let moved = format!("{elsewhere}computeMetadata/v1/instance/service-accounts/default/email");
    let (sending, _) = server(move |_, _| {
        reply(
            "302 Found",
            &[("Metadata-Flavor", "Google"), ("Location", &moved)],
            b"",
        )
    });
    let client = transport();
    let link = "https://storage.googleapis.com/lab-private/a.csv";
    for (machine, message) in [
        (
            &bare,
            "this machine has no service account: google's metadata server named none".to_owned(),
        ),
        (
            &refusing,
            format!(
                "google's metadata server gave no token for this machine's service account {MACHINE}"
            ),
        ),
        (
            &sending,
            "google's metadata server answered 302 for this machine's service account".to_owned(),
        ),
    ] {
        let net = machine_net(&["lab-private"], &endpoint, Some(machine));
        let Err(Failure::Final(error)) = block_on(sent(&net, &client, &local, link)) else {
            panic!("the bucket was asked without the machine's account");
        };
        assert_eq!(
            (error.kind, error.message.as_str()),
            (Kind::AuthRequired, message.as_str())
        );
    }
    assert_eq!(authorizations(&asked), [None, None, None]);
    assert!(asked_elsewhere.lock().unwrap().is_empty());
}

/// using the machine's service account trusts nothing: a link to the metadata server, by its
/// address or by the name google gives it, is still refused before any request.
#[test]
fn a_link_to_the_metadata_server_stays_refused() {
    let (endpoint, _) = tokens();
    let (machine, asked_machine) = machine_server();
    let (local, _) = server(|request, _| match request.authorization() {
        Some("bearer machine-1") => reply("200 OK", &[], b"object"),
        _ => reply("401 Unauthorized", &[], b"{}"),
    });
    let mut net = machine_net(&["lab-private"], &endpoint, Some(&machine));
    net.lookups = Some(Arc::new(|host| {
        assert_eq!(host, "metadata.google.internal");
        Ok(vec![std::net::SocketAddr::from(([169, 254, 169, 254], 80))])
    }));
    let link = "https://storage.googleapis.com/lab-private/raw/a.csv";
    let answer = block_on(through(&net, &transport(), &local, link));
    assert_eq!(answer.status(), 200);
    let folder = tempfile::tempdir().unwrap();
    let cache = crate::cache::Cache::open(folder.path().join("cache")).unwrap();
    for (host, reason) in [
        (
            "169.254.169.254",
            "it points at the private address 169.254.169.254",
        ),
        (
            "metadata.google.internal",
            "metadata.google.internal resolves only to private addresses",
        ),
    ] {
        let url = Url::parse(&format!(
            "http://{host}/computeMetadata/v1/instance/service-accounts/default/token"
        ))
        .unwrap();
        let fetch = crate::net::Fetch::new(url.clone());
        let error = block_on(net.download(&fetch, &cache, &|_| Ok(()))).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused, "{}", error.message);
        assert_eq!(
            error.message,
            format!("fetchloom will not follow {url}: {reason}")
        );
    }
    assert_eq!(asked_machine.lock().unwrap().len(), 2);
}
