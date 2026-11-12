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
fn azure_net(listed: &[&str], set: &[(&'static str, &'static str)]) -> Net {
    let listed = listed
        .iter()
        .map(|text| AzureContainer::parse(text).unwrap())
        .collect();
    let set = set.to_vec();
    let azure = Azure::new(
        listed,
        Arc::new(move |name: &str| {
            set.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }),
    );
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

/// a client that carries the requests `send` shapes to a server on this machine instead of
/// their hosts. it is the test's own, so it installs the crypto provider itself.
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

/// a listed container is asked without the SAS first; once refused, the request goes again
/// with it, and later requests for that container carry it from the start. every request asks
/// for azure's version.
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

/// a container nobody listed, a container in another account than the one the environment's
/// SAS is kept to, and a link that carries a SAS of its own get no SAS of the settings, and a
/// refusal says how the request went.
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

/// the environment's SAS without `AZURE_STORAGE_ACCOUNT` is refused, before it reaches any
/// container, when the listed containers it would go to are in more than one account. a
/// container with a SAS of its own takes none of it, so it does not count.
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

/// a container's entry may carry a SAS of its own, which it gets before the environment's. a
/// SAS azure refuses is not sent again for that request, and the refusal names where it came
/// from.
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

/// the SAS goes with the request to the container's own host alone, never on to where a
/// redirect sends it, and only that first request asks for azure's version.
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

/// an environment that holds no SAS fetchloom can read fails the request that would need it,
/// without repeating what it holds, and asks nothing more.
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

/// a key of the account `AZURE_STORAGE_ACCOUNT` names signs a listed container's requests in that
/// account once azure refused one without a credential: a `SharedKey` authorization of the
/// account, with the time in `x-ms-date`. the key itself is never sent, and a container in
/// another account gets no signature.
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
