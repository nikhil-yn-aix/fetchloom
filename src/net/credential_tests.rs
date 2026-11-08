use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

use super::*;
use crate::source::recorded::block_on;

const OWNERS: [(&str, &str); 5] = [
    ("zenodo.org", "Bearer zenodo-test"),
    ("api.figshare.com", "token figshare-test"),
    ("api.osf.io", "Bearer osf-test"),
    ("huggingface.co", "Bearer hf-test"),
    ("api.kaggle.com", "Bearer kaggle-test"),
];

fn net() -> Net {
    let var = |name: &str| {
        let token = match name {
            "ZENODO_TOKEN" => "zenodo-test",
            "FIGSHARE_TOKEN" => "figshare-test",
            "OSF_TOKEN" => "osf-test",
            "HF_TOKEN" => "hf-test",
            "KAGGLE_API_TOKEN" => "kaggle-test",
            _ => return None,
        };
        Some(OsString::from(token))
    };
    Net::new(
        Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials: crate::auth::credentials(&var),
            split: crate::net::Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
}

#[test]
fn each_sources_credentials_only_reach_its_https_host_on_port_443() {
    let net = net();
    let client = &net.wire().unwrap().client;
    for (host, value) in OWNERS {
        for url in [
            format!("https://{host}/file"),
            format!("https://{host}:443/file"),
            format!("https://{}/file", host.to_ascii_uppercase()),
        ] {
            let url = Url::parse(&url).unwrap();
            let request = net.request(client, &url, true).build().unwrap();
            assert_eq!(request.headers()[header::AUTHORIZATION], value, "{url}");
            assert!(request.headers()[header::AUTHORIZATION].is_sensitive());
        }
        for url in [
            format!("http://{host}/file"),
            format!("http://{host}:443/file"),
            format!("https://{host}:8443/file"),
            format!("https://sub.{host}/file"),
            format!("https://{host}.example.org/file"),
        ] {
            let url = Url::parse(&url).unwrap();
            let request = net.request(client, &url, true).build().unwrap();
            assert!(
                !request.headers().contains_key(header::AUTHORIZATION),
                "{url}"
            );
        }
    }
}

/// a client that carries the requests `send` shapes to a local server instead of their hosts. it
/// is the test's own, so it installs the crypto provider fetchloom installs only when it first
/// goes online.
fn transport() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn server(locations: &[String]) -> (Url, JoinHandle<Vec<Option<String>>>) {
    server_header(locations, "authorization")
}

fn server_header(locations: &[String], header: &str) -> (Url, JoinHandle<Vec<Option<String>>>) {
    let header = header.to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let locations = locations.to_vec();
    let serving = std::thread::spawn(move || {
        let mut credentials = Vec::new();
        for index in 0..=locations.len() {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|read| read == 1)
            {
                head.push(byte[0]);
            }
            credentials.push(String::from_utf8_lossy(&head).lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case(&header)
                    .then(|| value.trim().to_owned())
            }));
            let answer = match locations.get(index) {
                Some(location) => format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n"),
                None => "HTTP/1.1 200 OK\r\n".to_owned(),
            };
            stream
                .write_all(
                    format!("{answer}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes(),
                )
                .unwrap();
        }
        credentials
    });
    (local, serving)
}

#[test]
fn redirects_never_restore_credentials_after_leaving_the_first_origin() {
    for (index, (host, value)) in OWNERS.iter().enumerate() {
        let other = OWNERS[(index + 1) % OWNERS.len()].0;
        for away in [
            format!("https://{other}/away"),
            format!("http://{host}/away"),
            format!("https://{host}:8443/away"),
        ] {
            let net = net();
            let client = transport();
            net.trust([(*host).to_owned(), other.to_owned()]);
            let start = Url::parse(&format!("https://{host}/start")).unwrap();
            let (local, serving) = server(&[
                format!("https://{host}/same-origin"),
                away,
                format!("https://{host}/back"),
            ]);
            // only the transport is local; send still selects credentials and follows each origin.
            let answer = block_on(net.send(&start, |request| {
                let request = request.build().unwrap();
                client.get(local.clone()).headers(request.headers().clone())
            }));
            assert!(answer.is_ok(), "the scripted redirect chain must finish");
            assert_eq!(
                serving.join().unwrap(),
                [
                    Some((*value).to_owned()),
                    Some((*value).to_owned()),
                    None,
                    None
                ],
                "{host}"
            );
        }
    }
}

#[test]
fn a_saved_login_answers_on_any_hop_to_its_own_host_and_nowhere_else() {
    let net = Net::new(
        Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials: vec![
                Credential {
                    host: "urs.earthdata.nasa.gov".to_owned(),
                    header: "authorization",
                    value: "Basic YWRhOnB3".to_owned(),
                    every_hop: true,
                },
                Credential {
                    host: "zenodo.org".to_owned(),
                    header: "authorization",
                    value: "Bearer z".to_owned(),
                    every_hop: false,
                },
            ],
            split: crate::net::Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    net.trust([
        "archive.example".to_owned(),
        "urs.earthdata.nasa.gov".to_owned(),
        "zenodo.org".to_owned(),
    ]);
    // the way an earthdata archive signs a person in: away to the login host and back
    let start = Url::parse("https://archive.example/granule").unwrap();
    let (local, serving) = server(&[
        "http://urs.earthdata.nasa.gov/oauth".to_owned(),
        "https://urs.earthdata.nasa.gov/oauth".to_owned(),
        "https://zenodo.org/not-the-first-origin".to_owned(),
        "https://archive.example/granule?code=1".to_owned(),
    ]);
    let client = transport();
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok());
    assert_eq!(
        serving.join().unwrap(),
        [None, None, Some("Basic YWRhOnB3".to_owned()), None, None],
        "the login goes over https to its host only; the token stays on its first origin"
    );
}

#[test]
fn a_bare_dataverse_token_has_no_host_to_send_it_to() {
    let var = |name: &str| (name == "DATAVERSE_TOKEN").then(|| OsString::from("unscoped-test"));
    assert!(crate::auth::credentials(&var).is_empty());
}

fn keyed(hosts: &[&str]) -> Net {
    let credentials = hosts
        .iter()
        .map(|host| Credential {
            host: (*host).to_owned(),
            header: "authorization",
            value: format!("key {host}"),
            every_hop: false,
        })
        .collect();
    Net::new(
        Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials,
            split: crate::net::Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
}

/// a dataverse on another port gets its key there, while a key for port 443 never reaches
/// another port of the same host, nor the other way round, as curl's CVE-2022-27776 was.
#[test]
fn a_key_for_another_port_reaches_that_port_only() {
    let net = keyed(&["dv.lab:8443", "dv.lab"]);
    let client = &net.wire().unwrap().client;
    let sent = |url: &str| {
        let url = Url::parse(url).unwrap();
        let request = net.request(client, &url, true).build().unwrap();
        request
            .headers()
            .get(header::AUTHORIZATION)
            .map(|value| value.to_str().unwrap().to_owned())
    };
    for url in ["https://dv.lab:8443/api/x", "https://DV.LAB:8443/api/x"] {
        assert_eq!(sent(url).as_deref(), Some("key dv.lab:8443"), "{url}");
    }
    for url in ["https://dv.lab/api/x", "https://dv.lab:443/api/x"] {
        assert_eq!(sent(url).as_deref(), Some("key dv.lab"), "{url}");
    }
    for url in [
        "https://dv.lab:9443/x",
        "http://dv.lab:8443/x",
        "http://dv.lab/x",
        "https://other.lab:8443/x",
        "https://sub.dv.lab:8443/x",
    ] {
        assert_eq!(sent(url), None, "{url}");
    }
}

#[test]
fn a_redirect_to_another_port_of_the_same_host_carries_no_key() {
    for (start, away, key) in [
        (
            "https://dv.lab:8443/start",
            "https://dv.lab/away",
            "key dv.lab:8443",
        ),
        (
            "https://dv.lab/start",
            "https://dv.lab:8443/away",
            "key dv.lab",
        ),
    ] {
        let net = keyed(&["dv.lab:8443", "dv.lab"]);
        net.trust(["dv.lab".to_owned()]);
        let client = transport();
        let start = Url::parse(start).unwrap();
        let (local, serving) = server(&[away.to_owned(), start.join("back").unwrap().to_string()]);
        let answer = block_on(net.send(&start, |request| {
            let request = request.build().unwrap();
            client.get(local.clone()).headers(request.headers().clone())
        }));
        assert!(answer.is_ok());
        assert_eq!(
            serving.join().unwrap(),
            [Some(key.to_owned()), None, None],
            "{start}"
        );
    }
}

#[test]
fn a_redirect_cannot_pick_up_another_sources_credentials() {
    let net = net();
    net.trust([
        "example.org".to_owned(),
        "zenodo.org".to_owned(),
        "api.figshare.com".to_owned(),
    ]);
    let start = Url::parse("https://example.org/shared-file").unwrap();
    let (local, serving) = server(&[
        "https://zenodo.org/file".to_owned(),
        "https://api.figshare.com/file".to_owned(),
        "https://example.org/back".to_owned(),
    ]);
    let client = transport();
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok());
    assert_eq!(serving.join().unwrap(), [None, None, None, None]);
}

fn verification_net(credentials: Vec<Credential>) -> Net {
    Net::new(
        Options {
            offline: Offline::No,
            jobs: 1,
            credentials,
            split: Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
}

fn redirect_headers(net: &Net, start: &str, hops: &[String], header: &str) -> Vec<Option<String>> {
    let start = Url::parse(start).unwrap();
    let hosts = std::iter::once(start.clone())
        .chain(hops.iter().map(|hop| Url::parse(hop).unwrap()))
        .filter_map(|url| url.host_str().map(str::to_owned));
    net.trust(hosts);
    let client = transport();
    let (local, serving) = server_header(hops, header);
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok(), "the local redirect chain must finish");
    serving.join().unwrap()
}

#[test]
fn wave1_configured_dataverse_keys_do_not_return_after_an_origin_change() {
    for (owner, other_port) in [("dv.lab", "dv.lab:8443"), ("dv.lab:8443", "dv.lab")] {
        let configured = format!("https://{owner}");
        let var = |name: &str| match name {
            "DATAVERSE_API_TOKEN" => Some(OsString::from("wave1-dataverse-sentinel")),
            "DATAVERSE_URL" => Some(OsString::from(&configured)),
            _ => None,
        };
        for away in [
            "https://elsewhere.lab:8443/away".to_owned(),
            format!("https://{other_port}/away"),
            format!("http://{owner}/away"),
        ] {
            let credentials = crate::auth::credentials(&var);
            assert_eq!(credentials.len(), 1);
            let net = verification_net(credentials);
            let start = format!("https://{owner}/start");
            let hops = [
                format!("https://{owner}/same"),
                away,
                format!("https://{owner}/back"),
            ];
            assert_eq!(
                redirect_headers(&net, &start, &hops, "x-dataverse-key"),
                [
                    Some("wave1-dataverse-sentinel".to_owned()),
                    Some("wave1-dataverse-sentinel".to_owned()),
                    None,
                    None
                ],
                "{start}: {hops:?}",
            );
        }
    }
}

#[test]
fn wave1_netrc_logins_return_only_to_their_own_https_host_and_port() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("netrc");
    for (owner, other_port) in [
        ("login.lab", "login.lab:8443"),
        ("login.lab:8443", "login.lab"),
    ] {
        std::fs::write(&path, format!("machine {owner} login ada password pw\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let var = |name: &str| (name == "NETRC").then(|| path.clone().into_os_string());
        let credentials = crate::auth::credentials(&var);
        assert_eq!(credentials.len(), 1);
        let net = verification_net(credentials);
        let hops = [
            format!("https://{owner}/login"),
            format!("https://{other_port}/other-port"),
            format!("http://{owner}/downgrade"),
            "https://other.lab/foreign".to_owned(),
            "https://sub.login.lab/subdomain".to_owned(),
            format!("https://{owner}/return"),
        ];
        let value = Some("Basic YWRhOnB3".to_owned());
        assert_eq!(
            redirect_headers(&net, "https://archive.lab/start", &hops, "authorization"),
            [None, value.clone(), None, None, None, None, value],
            "{owner}",
        );
    }
}

#[test]
fn wave1_osf_exceptions_keep_their_own_scheme_host_and_port_boundaries() {
    let token = "wave1-osf-sentinel";
    for start in [
        "https://api.osf.io/start",
        "https://unrelated.example/start",
    ] {
        let var = |name: &str| (name == "OSF_TOKEN").then(|| OsString::from(token));
        let credentials = crate::auth::credentials(&var);
        assert_eq!(credentials.len(), 3);
        let net = verification_net(credentials);
        let hops: Vec<_> = [
            "https://osf.io/download",
            "https://files.osf.io/download",
            "https://api.osf.io/back",
            "http://osf.io/downgrade",
            "https://files.osf.io:8443/other-port",
            "https://sub.files.osf.io/subdomain",
            "https://files.osf.io.evil.example/foreign",
            "https://files.osf.io/back",
        ]
        .iter()
        .map(|hop| (*hop).to_owned())
        .collect();
        let value = Some(format!("Bearer {token}"));
        let first = start
            .starts_with("https://api.osf.io/")
            .then(|| format!("Bearer {token}"));
        assert_eq!(
            redirect_headers(&net, start, &hops, "authorization"),
            [
                first,
                value.clone(),
                value.clone(),
                None,
                None,
                None,
                None,
                None,
                value
            ],
            "{start}",
        );
    }
}
