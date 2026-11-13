use std::ffi::OsString;
use std::io::{Read, Write as _};
use std::net::TcpListener;
use std::time::Duration;

use super::*;
use crate::net::scripted::reply;
use crate::net::{Offline, Options, Splitting};
use crate::source::recorded::block_on;

const SAS: &str = "sv=2022-11-02&sr=c&sp=rl&se=2099-01-01T00%3A00%3A00Z&sig=shared%3D";
const OWN: &str = "sv=2022-11-02&sr=c&sp=rl&sig=own%3D";
/// an account key, the base64 of `secret-key`.
const KEY: &str = "c2VjcmV0LWtleQ==";

/// one request a test server took: its request line, whether it asked for azure's version, and
/// its whole head, lowercase.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Asked {
    line: String,
    versioned: bool,
    head: String,
}

/// a server on this machine that answers each request with what `answer` makes of its head,
/// lowercase, and of how many came before it, and keeps every request it took.
fn server(
    answer: impl Fn(&str, usize) -> Vec<u8> + Send + 'static,
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
            let line = head.lines().next().unwrap_or_default().to_owned();
            let count = {
                let mut seen = seen.lock().unwrap();
                seen.push(Asked {
                    line,
                    versioned: head.contains("x-ms-version: 2020-10-02"),
                    head: head.clone(),
                });
                seen.len() - 1
            };
            let _ = stream.write_all(&answer(&head, count));
        }
    });
    (url, asked)
}

/// azure's answers: 401 to a request without a signature, 200 to one with the signature
/// `signature`, 403 to one with another.
fn blob_host(signature: &'static str) -> (Url, Arc<Mutex<Vec<Asked>>>) {
    server(move |head, _| {
        if !head.contains("sig=") {
            reply("401 Unauthorized", &[], b"")
        } else if head.contains(signature) {
            reply("200 OK", &[], b"blob")
        } else {
            reply("403 Forbidden", &[], b"")
        }
    })
}

/// a network that sends a SAS for the `listed` containers, with the environment `set` gives.
fn azure_net(listed: &[&str], set: &[(&str, &str)]) -> Net {
    azure_net_at(listed, set, None)
}

/// [`azure_net`], asking the instance metadata service at `identity` instead of this machine's.
fn azure_net_at(listed: &[&str], set: &[(&str, &str)], identity: Option<Url>) -> Net {
    let listed = listed
        .iter()
        .map(|text| AzureContainer::parse(text).unwrap())
        .collect();
    let set: Vec<(String, String)> = set
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    let azure = Azure::new(
        listed,
        Arc::new(move |name: &str| {
            set.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }),
    )
    .with_identity_endpoint(identity);
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
    .with_azure(azure)
}

/// a client that carries the requests `send` shapes to a local server. it installs the crypto
/// provider itself, since fetchloom installs it only when it first goes online.
fn transport() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// a GET of `link` through `net`, carried to `local` with the path, query and headers `net` gave
/// it, and the answer, or the error that ended it.
fn through(net: &Net, local: &Url, link: &str) -> Result<reqwest::Response, Error> {
    let client = transport();
    let url = Url::parse(link).unwrap();
    let sent = block_on(net.send(&url, |request| {
        let request = request.build().unwrap();
        let mut target = local.join(request.url().path()).unwrap();
        target.set_query(request.url().query());
        client.get(target).headers(request.headers().clone())
    }));
    match sent {
        Ok(response) => Ok(response),
        Err(Failure::Final(error)) => Err(error),
        Err(Failure::Transient { reason, .. } | Failure::Limited { reason, .. }) => {
            panic!("{link} failed for now: {reason}")
        }
    }
}

fn lines(asked: &Mutex<Vec<Asked>>) -> Vec<String> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|asked| asked.line.clone())
        .collect()
}

fn sent(response: &reqwest::Response) -> Option<&Sent> {
    match response.extensions().get::<Authorized>() {
        Some(Authorized::Azure(sent)) => Some(sent),
        _ => None,
    }
}

/// the request goes again with it, and later requests for that container carry it from the
/// start. every request asks for azure's version.
#[test]
fn a_sas_goes_to_a_listed_container_once_it_refused_a_request_without_one() {
    let (local, asked) = blob_host("shared%3d");
    let net = azure_net(&["azure:lab/raw"], &[("AZURE_STORAGE_SAS_TOKEN", SAS)]);
    for link in [
        "https://lab.blob.core.windows.net/raw/a.csv",
        "https://lab.blob.core.windows.net/raw/b.csv?versionid=1",
    ] {
        let answer = through(&net, &local, link).unwrap();
        assert_eq!(answer.status(), 200, "{link}");
        assert_eq!(
            sent(&answer),
            Some(&Sent::Sas {
                container: "azure:lab/raw".to_owned(),
                given: Given::Environment,
                expiry: Some("2099-01-01T00:00:00Z".to_owned()),
            })
        );
    }
    let sas = SAS.to_ascii_lowercase();
    assert_eq!(
        lines(&asked),
        [
            "get /raw/a.csv http/1.1".to_owned(),
            format!("get /raw/a.csv?{sas} http/1.1"),
            format!("get /raw/b.csv?versionid=1&{sas} http/1.1"),
        ]
    );
    assert!(asked.lock().unwrap().iter().all(|asked| asked.versioned));
}

/// not to a container nobody listed, one in another account than the environment's SAS is kept
/// to, or a link with a SAS of its own; a refusal says how the request went.
#[test]
fn a_sas_never_goes_where_the_settings_do_not_send_it() {
    let (local, asked) = blob_host("own%3d");
    let net = azure_net(
        &["azure:lab/raw", "azure:other/data"],
        &[
            ("AZURE_STORAGE_SAS_TOKEN", SAS),
            ("AZURE_STORAGE_ACCOUNT", "Lab"),
        ],
    );
    for (link, container) in [
        (
            "https://nobody.blob.core.windows.net/raw/a.csv",
            "azure:nobody/raw",
        ),
        (
            "https://lab.blob.core.windows.net/unlisted/a.csv",
            "azure:lab/unlisted",
        ),
    ] {
        let unlisted = through(&net, &local, link).unwrap();
        assert_eq!(unlisted.status(), 401, "{link}");
        assert_eq!(
            sent(&unlisted),
            Some(&Sent::Anonymous {
                container: container.to_owned(),
                listed: false,
            })
        );
    }
    let elsewhere = through(
        &net,
        &local,
        "https://other.blob.core.windows.net/data/a.csv",
    )
    .unwrap();
    assert_eq!(elsewhere.status(), 401);
    assert_eq!(
        sent(&elsewhere),
        Some(&Sent::Anonymous {
            container: "azure:other/data".to_owned(),
            listed: true,
        })
    );
    let own = format!("https://lab.blob.core.windows.net/raw/a.csv?{OWN}");
    assert_eq!(through(&net, &local, &own).unwrap().status(), 200);
    let own = OWN.to_ascii_lowercase();
    assert_eq!(
        lines(&asked),
        [
            "get /raw/a.csv http/1.1".to_owned(),
            "get /unlisted/a.csv http/1.1".to_owned(),
            "get /data/a.csv http/1.1".to_owned(),
            format!("get /raw/a.csv?{own} http/1.1"),
        ]
    );
}

/// before it reaches any container. a container with a SAS of its own takes none of it, so it
/// does not count.
#[test]
fn a_sas_without_its_account_is_refused_for_containers_in_two_accounts() {
    let (local, asked) = blob_host("shared%3d");
    let net = azure_net(
        &["azure:lab/raw", "azure:lab/scans", "azure:other/data"],
        &[("AZURE_STORAGE_SAS_TOKEN", SAS)],
    );
    let error = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "AZURE_STORAGE_SAS_TOKEN names no account, and azure_containers lists containers in 2 accounts: lab and other"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "set AZURE_STORAGE_ACCOUNT to the account the SAS is for, or give each container its own SAS link in azure_containers"
        )
    );
    assert_eq!(lines(&asked), ["get /raw/a.csv http/1.1"]);
    let own = format!("https://other.blob.core.windows.net/data?{OWN}");
    let net = azure_net(
        &["azure:lab/raw", &own],
        &[("AZURE_STORAGE_SAS_TOKEN", SAS)],
    );
    let answer = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap();
    assert_eq!(answer.status(), 200);
}

/// a refused SAS is not sent again for that request, and the refusal names where it came from.
#[test]
fn a_containers_own_sas_comes_first_and_a_refused_one_is_the_answer() {
    let (local, asked) = blob_host("own%3d");
    let own = format!("https://lab.blob.core.windows.net/raw?{OWN}");
    let net = azure_net(
        &[&own, "azure:lab/scans"],
        &[("AZURE_STORAGE_SAS_TOKEN", SAS)],
    );
    assert_eq!(
        through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv")
            .unwrap()
            .status(),
        200
    );
    let refused = through(
        &net,
        &local,
        "https://lab.blob.core.windows.net/scans/a.csv",
    )
    .unwrap();
    assert_eq!(refused.status(), 403);
    let error = sent(&refused)
        .unwrap()
        .refusal(&Url::parse("https://lab.blob.core.windows.net/scans/a.csv").unwrap());
    assert_eq!(
        error.message,
        "azure refused the SAS in AZURE_STORAGE_SAS_TOKEN for https://lab.blob.core.windows.net/scans/a.csv"
    );
    let (own, shared) = (OWN.to_ascii_lowercase(), SAS.to_ascii_lowercase());
    assert_eq!(
        lines(&asked),
        [
            "get /raw/a.csv http/1.1".to_owned(),
            format!("get /raw/a.csv?{own} http/1.1"),
            "get /scans/a.csv http/1.1".to_owned(),
            format!("get /scans/a.csv?{shared} http/1.1"),
        ]
    );
}

/// and only the first request asks for azure's version.
#[test]
fn a_sas_never_follows_a_redirect() {
    let (local, asked) = server(|_, count| match count {
        0 => reply("401 Unauthorized", &[], b""),
        1 => reply(
            "302 Found",
            &[("Location", "https://elsewhere.example/raw/a.csv")],
            b"",
        ),
        _ => reply("200 OK", &[], b"blob"),
    });
    let net = azure_net(&["azure:lab/raw"], &[("AZURE_STORAGE_SAS_TOKEN", SAS)]);
    let answer = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap();
    assert_eq!(answer.status(), 200);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3);
    assert!(asked[1].line.contains("sig="), "{asked:?}");
    assert_eq!(
        (asked[2].line.as_str(), asked[2].versioned),
        ("get /raw/a.csv http/1.1", false)
    );
}

/// it fails the request that needs it and asks nothing more.
#[test]
fn an_unreadable_sas_fails_without_repeating_it() {
    let (local, asked) = blob_host("shared%3d");
    let net = azure_net(
        &["azure:lab/raw"],
        &[("AZURE_STORAGE_SAS_TOKEN", "sp=rl&sig=leaked-value")],
    );
    let error = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "AZURE_STORAGE_SAS_TOKEN holds no SAS fetchloom can read"
    );
    assert!(!format!("{error:?}").contains("leaked-value"));
    assert_eq!(lines(&asked), ["get /raw/a.csv http/1.1"]);
}

/// once azure refused one without a credential: a `SharedKey` authorization with the time in
/// `x-ms-date`. the key itself is never sent, and another account's container gets no signature.
#[test]
fn an_account_key_signs_requests_to_its_own_account_alone() {
    let (local, asked) = server(|head, _| {
        if head.contains("authorization: sharedkey lab:") {
            reply("200 OK", &[], b"blob")
        } else {
            reply("401 Unauthorized", &[], b"")
        }
    });
    let net = azure_net(
        &["azure:lab/raw", "azure:other/data"],
        &[("AZURE_STORAGE_KEY", KEY), ("AZURE_STORAGE_ACCOUNT", "lab")],
    );
    let answer = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(
        sent(&answer),
        Some(&Sent::Key {
            container: "azure:lab/raw".to_owned(),
            account: "lab".to_owned(),
        })
    );
    let elsewhere = through(
        &net,
        &local,
        "https://other.blob.core.windows.net/data/a.csv",
    )
    .unwrap();
    assert_eq!(elsewhere.status(), 401);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert!(
        !asked[0].head.contains("authorization"),
        "{}",
        asked[0].head
    );
    assert!(
        asked[1].head.contains("\r\nx-ms-date: "),
        "{}",
        asked[1].head
    );
    assert!(
        !asked[2].head.contains("authorization"),
        "{}",
        asked[2].head
    );
    let key = KEY.to_ascii_lowercase();
    assert!(asked.iter().all(|asked| !asked.head.contains(&key)));
}

/// an az cli in `folder` that gives `printed-token-<n>` on its nth run, and the file that counts
/// its runs, a line each.
#[cfg(unix)]
fn az(folder: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let runs = folder.join("runs");
    let az = folder.join("az");
    std::fs::write(
        &az,
        format!(
            "#!/bin/sh\nprintf 'ran\\n' >> '{runs}'\nrun=$(wc -l < '{runs}')\nprintf '{{\"accessToken\": \"printed-token-%s\", \"expires_on\": 4102444800}}' $run\n",
            runs = runs.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&az, std::fs::Permissions::from_mode(0o755)).unwrap();
    runs
}

/// its token goes in `authorization`, for the container's later requests too, from one run of
/// the az cli, and never to a container nobody listed.
#[cfg(unix)]
#[test]
fn the_az_clis_login_reads_a_listed_container_once_refused() {
    let folder = tempfile::tempdir().unwrap();
    let runs = az(folder.path());
    let (local, asked) = server(|head, _| {
        if head.contains("authorization: bearer printed-token-1\r\n") {
            reply("200 OK", &[], b"blob")
        } else {
            reply("401 Unauthorized", &[], b"")
        }
    });
    let net = azure_net(
        &["azure:lab/raw"],
        &[("PATH", folder.path().to_str().unwrap())],
    );
    for link in [
        "https://lab.blob.core.windows.net/raw/a.csv",
        "https://lab.blob.core.windows.net/raw/b.csv",
    ] {
        assert_eq!(through(&net, &local, link).unwrap().status(), 200, "{link}");
    }
    let unlisted = through(
        &net,
        &local,
        "https://nobody.blob.core.windows.net/raw/a.csv",
    );
    assert_eq!(unlisted.unwrap().status(), 401);
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "ran\n");
    let heads: Vec<bool> = asked
        .lock()
        .unwrap()
        .iter()
        .map(|asked| asked.head.contains("authorization"))
        .collect();
    assert_eq!(heads, [false, true, true, false]);
}

/// as a 401 turns an expired one away, and the request goes again with the new one.
#[cfg(unix)]
#[test]
fn a_token_azure_turns_away_is_asked_for_again_once() {
    let folder = tempfile::tempdir().unwrap();
    let runs = az(folder.path());
    let (local, asked) = server(|head, _| {
        if head.contains("authorization: bearer printed-token-2\r\n") {
            reply("200 OK", &[], b"blob")
        } else {
            reply("401 Unauthorized", &[], b"")
        }
    });
    let net = azure_net(
        &["azure:lab/raw"],
        &[("PATH", folder.path().to_str().unwrap())],
    );
    let answer = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(std::fs::read_to_string(&runs).unwrap(), "ran\nran\n");
    assert_eq!(asked.lock().unwrap().len(), 3);
}

/// the client id of a user-assigned identity, as azure writes one.
const CLIENT_ID: &str = "0b4f5c1e-1a2b-4c3d-8e9f-000000000001";

/// a stand-in for an instance metadata service, which gives `identity-token` to a request that
/// says `metadata: true`, as azure's does, and 400 to any other.
fn metadata_service() -> (Url, Arc<Mutex<Vec<Asked>>>) {
    server(|head, _| {
        if head.contains("\r\nmetadata: true\r\n") {
            reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                br#"{"access_token":"identity-token","client_id":"0b4f5c1e-1a2b-4c3d-8e9f-000000000001","expires_in":"86399","expires_on":"4102444800","ext_expires_in":"86399","not_before":"1794524399","resource":"https://storage.azure.com/","token_type":"Bearer"}"#,
            )
        } else {
            reply("400 Bad Request", &[], b"")
        }
    })
}

/// when `AZURE_TOKEN_CREDENTIALS` asks for it, with a token for the identity `AZURE_CLIENT_ID`
/// names. the service's address stays refused to any other request.
#[test]
fn the_machines_managed_identity_reads_a_listed_container_once_refused() {
    let (identity, identity_asked) = metadata_service();
    let (local, asked) = server(|head, _| {
        if head.contains("authorization: bearer identity-token\r\n") {
            reply("200 OK", &[], b"blob")
        } else {
            reply("401 Unauthorized", &[], b"")
        }
    });
    let net = azure_net_at(
        &["azure:lab/raw"],
        &[
            ("AZURE_TOKEN_CREDENTIALS", "ManagedIdentityCredential"),
            ("AZURE_CLIENT_ID", CLIENT_ID),
        ],
        Some(identity.join("metadata/identity/oauth2/token").unwrap()),
    );
    let answer = through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(
        sent(&answer),
        Some(&Sent::Token {
            container: "azure:lab/raw".to_owned(),
            by: SignedIn::Managed,
        })
    );
    assert_eq!(asked.lock().unwrap().len(), 2);
    let identity_asked = identity_asked.lock().unwrap().clone();
    assert_eq!(identity_asked.len(), 1);
    assert_eq!(
        identity_asked[0].line,
        format!(
            "get /metadata/identity/oauth2/token?api-version=2018-02-01&resource=https%3a%2f%2fstorage.azure.com%2f&client_id={CLIENT_ID} http/1.1"
        )
    );
    let project = Url::parse("http://169.254.169.254/metadata/identity/oauth2/token").unwrap();
    let refused = block_on(crate::net::Api::get(&net, &project)).unwrap_err();
    assert_eq!(refused.kind, Kind::NetworkRefused);
}

/// a machine without the identity asked for says so in azure's words, and one where no metadata
/// service answers, as off azure, says so at once rather than trying again.
#[test]
fn a_managed_identity_that_cannot_be_had_says_why() {
    let (identity, _) = server(|_, _| {
        reply(
            "400 Bad Request",
            &[("Content-Type", "application/json")],
            br#"{"error":"invalid_request","error_description":"Identity not found"}"#,
        )
    });
    let (local, asked) = blob_host("never");
    let ask = |identity: Url| {
        let net = azure_net_at(
            &["azure:lab/raw"],
            &[
                ("AZURE_TOKEN_CREDENTIALS", "ManagedIdentityCredential"),
                ("AZURE_CLIENT_ID", CLIENT_ID),
            ],
            Some(identity),
        );
        through(&net, &local, "https://lab.blob.core.windows.net/raw/a.csv").unwrap_err()
    };
    let missing = ask(identity.join("metadata/identity/oauth2/token").unwrap());
    assert_eq!(missing.kind, Kind::AuthRequired);
    assert_eq!(
        missing.message,
        format!(
            "this machine has no managed identity with the client id {CLIENT_ID}, or azure refused it"
        )
    );
    assert_eq!(
        missing.details,
        ["azure said invalid_request: Identity not found"]
    );
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let nowhere = Url::parse(&format!("http://{}/token", closed.local_addr().unwrap())).unwrap();
    drop(closed);
    let unanswered = ask(nowhere);
    assert!(
        unanswered
            .message
            .starts_with("this machine's managed identity could not be asked"),
        "{}",
        unanswered.message
    );
    assert_eq!(lines(&asked).len(), 2);
}
