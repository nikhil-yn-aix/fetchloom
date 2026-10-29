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
            offline: false,
            jobs: 1,
            credentials: crate::source::credentials(&var),
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
                name.eq_ignore_ascii_case("authorization")
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
fn a_bare_dataverse_token_has_no_host_to_send_it_to() {
    let var = |name: &str| (name == "DATAVERSE_TOKEN").then(|| OsString::from("unscoped-test"));
    assert!(crate::source::credentials(&var).is_empty());
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
