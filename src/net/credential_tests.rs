use std::ffi::OsString;
use std::sync::Mutex;

use super::scripted::{reply, serve};
use super::*;
use crate::source::recorded::block_on;

const OWNERS: [(&str, &str); 8] = [
    ("zenodo.org", "Bearer zenodo-test"),
    ("api.figshare.com", "token figshare-test"),
    ("api.osf.io", "Bearer osf-test"),
    ("huggingface.co", "Bearer hf-test"),
    ("api.kaggle.com", "Bearer kaggle-test"),
    ("datadryad.org", "Bearer dryad-test"),
    ("api.github.com", "Bearer github-test"),
    ("gitlab.com", "Bearer gitlab-test"),
];

fn net() -> Net {
    let var = |name: &str| {
        let token = match name {
            "ZENODO_TOKEN" => "zenodo-test",
            "FIGSHARE_TOKEN" => "figshare-test",
            "OSF_TOKEN" => "osf-test",
            "HF_TOKEN" => "hf-test",
            "KAGGLE_API_TOKEN" => "kaggle-test",
            "DRYAD_TOKEN" => "dryad-test",
            "GH_TOKEN" => "github-test",
            "GITLAB_TOKEN" => "gitlab-test",
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

/// a client of `net`'s own, to build requests with as `send` does.
fn any_client(net: &Net) -> &reqwest::Client {
    let any = Url::parse("https://example.org/").unwrap();
    net.wire().unwrap().lane(Lane::Shared, &any).unwrap()
}

/// trusts the origin of each of `urls`, as a person typing them would.
fn trust_all<'a>(net: &Net, urls: impl IntoIterator<Item = &'a str>) {
    net.trust(
        urls.into_iter()
            .map(|url| Url::parse(url).unwrap().origin()),
    );
}

#[test]
fn each_sources_credentials_only_reach_its_https_host_on_port_443() {
    let net = net();
    let client = any_client(&net);
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

/// a semantic scholar key goes to its API alone: never to the links on S3 the API gives for
/// dataset files, to its website, or over http.
#[test]
fn a_semantic_scholar_key_goes_to_its_api_alone() {
    let var = |name: &str| (name == "S2_API_KEY").then(|| OsString::from("s2-test"));
    let net = Net::new(
        Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials: crate::auth::credentials(&var),
            split: crate::net::Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    let client = any_client(&net);
    let api = Url::parse("https://api.semanticscholar.org/datasets/v1/release/latest").unwrap();
    let request = net.request(client, &api, true).build().unwrap();
    assert_eq!(request.headers()["x-api-key"], "s2-test");
    assert!(request.headers()["x-api-key"].is_sensitive());
    for url in [
        "https://ai2-s2ag.s3.amazonaws.com/staging/2026-11-10/tldrs/part.gz",
        "https://www.semanticscholar.org/paper/204e3073870fae3d05bcbc2f6a8e263d9b72e776",
        "http://api.semanticscholar.org/datasets/v1/release/latest",
    ] {
        let url = Url::parse(url).unwrap();
        let request = net.request(client, &url, true).build().unwrap();
        assert!(!request.headers().contains_key("x-api-key"), "{url}");
    }
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

/// a server on this machine that redirects to each of `locations` in turn, then answers.
fn server(locations: &[String]) -> (Url, Arc<Mutex<Vec<String>>>) {
    let mut responses: Vec<_> = locations
        .iter()
        .map(|location| reply("302 Found", &[("Location", location)], b""))
        .collect();
    responses.push(reply("200 OK", &[], b""));
    serve(responses)
}

/// the value of `header` in each request the server took.
fn sent(asked: &Mutex<Vec<String>>, header: &str) -> Vec<Option<String>> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|head| {
            head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case(header)
                    .then(|| value.trim().to_owned())
            })
        })
        .collect()
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
            let start = Url::parse(&format!("https://{host}/start")).unwrap();
            trust_all(&net, [start.as_str(), away.as_str()]);
            let (local, asked) = server(&[
                format!("https://{host}/same-origin"),
                away,
                format!("https://{host}/back"),
            ]);
            // only the transport is local; send still selects credentials and follows each origin
            let answer = block_on(net.send(&start, |request| {
                let request = request.build().unwrap();
                client.get(local.clone()).headers(request.headers().clone())
            }));
            assert!(answer.is_ok(), "the scripted redirect chain must finish");
            assert_eq!(
                sent(&asked, "authorization"),
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
    // the way an earthdata archive signs a person in: away to the login host and back
    let hops = [
        "http://urs.earthdata.nasa.gov/oauth",
        "https://urs.earthdata.nasa.gov/oauth",
        "https://zenodo.org/not-the-first-origin",
        "https://archive.example/granule?code=1",
    ];
    trust_all(&net, hops);
    let start = Url::parse("https://archive.example/granule").unwrap();
    let (local, asked) = server(&hops.map(str::to_owned));
    let client = transport();
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok());
    assert_eq!(
        sent(&asked, "authorization"),
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

/// a key for port 443 never reaches another port of its host, as curl's CVE-2022-27776 let one.
#[test]
fn a_key_for_another_port_reaches_that_port_only() {
    let net = keyed(&["dv.lab:8443", "dv.lab"]);
    let client = any_client(&net);
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
        trust_all(&net, [start, away]);
        let client = transport();
        let start = Url::parse(start).unwrap();
        let (local, asked) = server(&[away.to_owned(), start.join("back").unwrap().to_string()]);
        let answer = block_on(net.send(&start, |request| {
            let request = request.build().unwrap();
            client.get(local.clone()).headers(request.headers().clone())
        }));
        assert!(answer.is_ok());
        assert_eq!(
            sent(&asked, "authorization"),
            [Some(key.to_owned()), None, None],
            "{start}"
        );
    }
}

/// the key goes to github's API alone, though release files sit on github.com.
#[test]
fn a_github_key_never_reaches_github_com_or_its_storage() {
    let net = net();
    let client = any_client(&net);
    for url in [
        "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-1.8.2.tar.gz",
        "https://release-assets.githubusercontent.com/github-production-release-asset/1",
        "https://objects.githubusercontent.com/github-production-release-asset-2e65be/1",
        "https://raw.githubusercontent.com/jqlang/jq/master/README.md",
    ] {
        let url = Url::parse(url).unwrap();
        let request = net.request(client, &url, true).build().unwrap();
        assert!(
            !request.headers().contains_key(header::AUTHORIZATION),
            "{url}"
        );
    }
}

#[test]
fn a_redirect_cannot_pick_up_another_sources_credentials() {
    let net = net();
    let hops = [
        "https://zenodo.org/file",
        "https://api.figshare.com/file",
        "https://example.org/back",
    ];
    trust_all(&net, hops);
    let start = Url::parse("https://example.org/shared-file").unwrap();
    let (local, asked) = server(&hops.map(str::to_owned));
    let client = transport();
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok());
    assert_eq!(sent(&asked, "authorization"), [None, None, None, None]);
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
    trust_all(
        net,
        std::iter::once(start).chain(hops.iter().map(String::as_str)),
    );
    let start = Url::parse(start).unwrap();
    let client = transport();
    let (local, asked) = server(hops);
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    }));
    assert!(answer.is_ok(), "the local redirect chain must finish");
    sent(&asked, header)
}

#[test]
fn configured_dataverse_keys_do_not_return_after_an_origin_change() {
    for (owner, other_port) in [("dv.lab", "dv.lab:8443"), ("dv.lab:8443", "dv.lab")] {
        let configured = format!("https://{owner}");
        let var = |name: &str| match name {
            "DATAVERSE_API_TOKEN" => Some(OsString::from("dataverse-sentinel")),
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
                    Some("dataverse-sentinel".to_owned()),
                    Some("dataverse-sentinel".to_owned()),
                    None,
                    None
                ],
                "{start}: {hops:?}",
            );
        }
    }
}

#[test]
fn netrc_logins_return_only_to_their_own_https_host_and_port() {
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
fn osf_exceptions_keep_their_own_scheme_host_and_port_boundaries() {
    let token = "osf-sentinel";
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

/// AWS keys in a test's own environment, with `more` settings beside them.
fn aws_keys(
    more: Vec<(&'static str, String)>,
) -> impl Fn(&str) -> Option<OsString> + Send + Sync + 'static {
    let mut pairs = vec![
        ("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEWXYZ".to_owned()),
        ("AWS_SECRET_ACCESS_KEY", "do-not-send".to_owned()),
        ("AWS_SESSION_TOKEN", "session-of-the-test".to_owned()),
    ];
    pairs.extend(more);
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
    }
}

/// a network that signs for the `listed` buckets with the keys `var` finds.
fn signing_net(
    listed: &[&str],
    var: impl Fn(&str) -> Option<OsString> + Send + Sync + 'static,
) -> Net {
    let listed = listed
        .iter()
        .map(|text| crate::source::Bucket::parse(text).unwrap())
        .collect();
    Net::new(
        Options {
            offline: crate::net::Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
    .with_signing(crate::source::Signing::new(listed, Arc::new(var)))
}

/// checks a request head's signature as S3 does: the canonical request rebuilt from what
/// arrived, signed with `secret` for the scope its authorization names.
fn verify(base: &Url, head: &str, secret: &str) {
    let mut lines = head.lines();
    let target = lines.next().unwrap().split(' ').nth(1).unwrap();
    let url = base.join(target).unwrap();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let header = |name: &str| {
        headers.iter().find(|(key, _)| key == name).map_or_else(
            || panic!("no {name} in {head}"),
            |(_, value)| value.as_str(),
        )
    };
    let authorization = header("authorization");
    let (credential, rest) = authorization
        .strip_prefix("AWS4-HMAC-SHA256 Credential=")
        .and_then(|rest| rest.split_once(", SignedHeaders="))
        .unwrap();
    let (names, _) = rest.split_once(", Signature=").unwrap();
    let scope: Vec<&str> = credential.split('/').collect();
    assert!(names.split(';').any(|name| name == "host"), "{head}");
    assert!(
        headers
            .iter()
            .filter(|(name, _)| name.starts_with("x-amz-"))
            .all(|(name, _)| names.split(';').any(|signed| signed == name)),
        "every x-amz- header is signed: {head}"
    );
    let signed: Vec<(&str, &str)> = names.split(';').map(|name| (name, header(name))).collect();
    let keys = crate::net::sign::Keys {
        id: scope[0].to_owned(),
        secret: secret.to_owned(),
        token: None,
    };
    let expected = crate::net::sign::authorization(
        &keys,
        &crate::net::sign::Scope {
            time: header("x-amz-date"),
            region: scope[2],
            service: scope[3],
        },
        &crate::net::sign::Request {
            method: "GET",
            url: &url,
            headers: &signed,
            payload: header("x-amz-content-sha256"),
        },
    );
    assert_eq!(authorization, expected, "{head}");
}

#[test]
fn a_listed_bucket_refused_without_a_key_is_asked_again_once_signed() {
    let (base, asked) = serve(vec![
        reply(
            "403 Forbidden",
            &[("x-amz-bucket-region", "eu-west-1")],
            b"<Error><Code>AccessDenied</Code></Error>",
        ),
        reply("200 OK", &[], b"<ListBucketResult/>"),
        reply("200 OK", &[], b"object"),
    ]);
    let origin = base.origin().ascii_serialization();
    let net = signing_net(
        &[&format!("s3:{origin}/lab-raw")],
        aws_keys(vec![("AWS_ENDPOINT_URL_S3", origin.clone())]),
    );
    net.typed(&base);
    let listing = base
        .join("lab-raw/?list-type=2&prefix=raw%2F&encoding-type=url")
        .unwrap();
    let response = block_on(net.get(&listing)).unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        response.keyed,
        Some(crate::source::Keyed::Signed {
            bucket: crate::source::Bucket::parse(&format!("s3:{origin}/lab-raw")).unwrap(),
            key: "\u{2026}WXYZ from AWS_ACCESS_KEY_ID".to_owned(),
            region: "eu-west-1".to_owned(),
        })
    );
    // the bucket's later requests go signed from the start
    let object = base
        .join("lab-raw/raw/a%20b+c.csv?versionId=v%2B1")
        .unwrap();
    assert_eq!(block_on(net.get(&object)).unwrap().status, 200);
    let authorizations = sent(&asked, "authorization");
    assert_eq!(authorizations[0], None);
    let heads = asked.lock().unwrap().clone();
    assert_eq!(heads.len(), 3);
    for head in &heads[1..] {
        verify(&base, head, "do-not-send");
        assert!(
            head.contains("x-amz-security-token: session-of-the-test"),
            "{head}"
        );
        assert!(!head.contains("do-not-send"), "{head}");
    }
    assert_eq!(net.work.counts().requests, 3);
}

#[test]
fn only_a_listed_buckets_requests_are_signed_and_never_a_presigned_link() {
    for (start, listed) in [
        ("https://other-lab.s3.amazonaws.com/a.csv", "s3://lab-raw"),
        (
            "https://lab-raw.s3.amazonaws.com/a.csv?X-Amz-Credential=x&X-Amz-Signature=abc",
            "s3://lab-raw",
        ),
        ("https://minio.lab.example/scans/a.csv", "s3://scans"),
    ] {
        let net = signing_net(
            &[listed],
            aws_keys(vec![(
                "AWS_ENDPOINT_URL_S3",
                "https://minio.lab.example".to_owned(),
            )]),
        );
        let (local, asked) = serve(vec![reply("403 Forbidden", &[], b"")]);
        let client = transport();
        let start = Url::parse(start).unwrap();
        let Ok(answer) = block_on(net.send(&start, |request| {
            let request = request.build().unwrap();
            client.get(local.clone()).headers(request.headers().clone())
        })) else {
            panic!("{start} was not answered");
        };
        assert_eq!(answer.status(), 403, "{start}");
        assert_eq!(sent(&asked, "authorization"), [None], "{start}");
        let keyed = answer.extensions().get::<crate::source::Keyed>();
        if start.host_str() == Some("other-lab.s3.amazonaws.com") {
            assert_eq!(
                keyed,
                Some(&crate::source::Keyed::Unsigned {
                    bucket: crate::source::Bucket::parse("s3://other-lab").unwrap(),
                    listed: false,
                })
            );
        } else {
            assert_eq!(keyed, None, "{start}");
        }
    }
}

#[test]
fn a_signature_never_follows_a_redirect() {
    let net = signing_net(
        &["s3:https://minio.lab.example/scans"],
        aws_keys(vec![(
            "AWS_ENDPOINT_URL_S3",
            "https://minio.lab.example".to_owned(),
        )]),
    );
    let (local, asked) = serve(vec![
        reply("403 Forbidden", &[], b""),
        reply("302 Found", &[("Location", "/scans/moved.csv")], b""),
        reply("200 OK", &[], b""),
    ]);
    let client = transport();
    let start = Url::parse("https://minio.lab.example/scans/a.csv").unwrap();
    let Ok(answer) = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        client.get(local.clone()).headers(request.headers().clone())
    })) else {
        panic!("the redirect chain was not answered");
    };
    assert_eq!(answer.status(), 200);
    let authorizations = sent(&asked, "authorization");
    assert_eq!(authorizations.len(), 3);
    assert_eq!(authorizations[0], None);
    assert!(authorizations[1].is_some());
    assert_eq!(authorizations[2], None);
    assert_eq!(sent(&asked, "x-amz-security-token")[2], None);
}
