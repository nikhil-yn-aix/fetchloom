use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use reqwest::header::{self, HeaderName};
use url::Url;

use super::*;
use crate::net::scripted::{reply, serve};
use crate::net::{Allowed, Lane, Options, Splitting};
use crate::source::recorded::block_on;

fn token(host: &str, value: &str) -> Site {
    Site::new(
        host,
        vec![(header::AUTHORIZATION, format!("Bearer {value}"))],
        Vec::new(),
        "the token".to_owned(),
        format!("sign in again with `fetchloom auth login {host} --token`"),
    )
    .unwrap()
}

fn net(sites: Vec<Site>) -> Net {
    let options = Options {
        offline: crate::net::Offline::No,
        jobs: 1,
        credentials: Vec::new(),
        split: Splitting::Never,
    };
    Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {})).finding_sites(move || sites)
}

/// the value of `name` in a request for `url` that `net` builds on its first origin.
fn sent(net: &Net, url: &str, name: &HeaderName) -> Option<String> {
    let url = Url::parse(url).unwrap();
    let client = net.wire().unwrap().lane(Lane::Shared, &url).unwrap();
    let request = net.request(client, &url, true).build().unwrap();
    let values: Vec<_> = request.headers().get_all(name).iter().collect();
    assert!(values.len() <= 1, "{url}: {values:?}");
    values.first().map(|value| {
        assert!(value.is_sensitive(), "{url}");
        value.to_str().unwrap().to_owned()
    })
}

#[test]
fn a_site_credential_reaches_its_own_host_and_port_alone() {
    let api_key = HeaderName::from_static("x-api-key");
    let keyed = Site::new(
        "api.lab:8443",
        vec![(api_key.clone(), "k-1".to_owned())],
        Vec::new(),
        "the X-API-Key header".to_owned(),
        String::new(),
    )
    .unwrap();
    let net = net(vec![token("data.lab", "t-1"), keyed]);
    for url in [
        "https://data.lab/file",
        "https://DATA.lab:443/file",
        "https://data.lab/deep/er?x=1",
    ] {
        assert_eq!(
            sent(&net, url, &header::AUTHORIZATION).as_deref(),
            Some("Bearer t-1"),
            "{url}"
        );
    }
    for url in [
        "http://data.lab/file",
        "https://data.lab:8443/file",
        "https://sub.data.lab/file",
        "https://data.lab.example/file",
        "https://api.lab:8443/file",
    ] {
        assert_eq!(sent(&net, url, &header::AUTHORIZATION), None, "{url}");
    }
    assert_eq!(
        sent(&net, "https://api.lab:8443/v1/x", &api_key).as_deref(),
        Some("k-1")
    );
    for url in ["https://api.lab/v1/x", "http://api.lab:8443/v1/x"] {
        assert_eq!(sent(&net, url, &api_key), None, "{url}");
    }
}

/// plain http shows a credential to everyone on the way, so it goes only where that is this
/// machine or a network the person listed.
#[test]
fn plain_http_carries_a_credential_to_this_machine_or_an_allowed_origin_alone() {
    let sites = || {
        [
            "127.0.0.1:8080",
            "127.9.9.9:8080",
            "localhost:8080",
            "[::1]:8080",
            "lab.example:8080",
            "elsewhere.example:80",
        ]
        .map(|host| token(host, host))
        .to_vec()
    };
    let allowed = Allowed {
        origins: vec![Url::parse("http://lab.example:8080").unwrap().origin()],
        ..Allowed::default()
    };
    let listed = net(sites()).with_allowed_origins(allowed);
    let unlisted = net(sites());
    for (url, to_listed, to_unlisted) in [
        ("http://127.0.0.1:8080/x", true, true),
        ("http://127.9.9.9:8080/x", true, true),
        ("http://LOCALHOST:8080/x", true, true),
        ("http://[::1]:8080/x", true, true),
        ("http://lab.example:8080/x", true, false),
        ("http://elsewhere.example/x", false, false),
    ] {
        for (net, expected) in [(&listed, to_listed), (&unlisted, to_unlisted)] {
            assert_eq!(
                sent(net, url, &header::AUTHORIZATION).is_some(),
                expected,
                "{url}"
            );
        }
    }
}

/// a server on this machine that redirects to each of `locations` in turn, then answers.
fn redirects(locations: &[&str]) -> (Url, Arc<Mutex<Vec<String>>>) {
    let mut responses: Vec<_> = locations
        .iter()
        .map(|location| reply("302 Found", &[("Location", location)], b""))
        .collect();
    responses.push(reply("200 OK", &[], b""));
    serve(responses)
}

/// the value of `name` in each request the server took.
fn heard(asked: &Mutex<Vec<String>>, name: &str) -> Vec<Option<String>> {
    asked
        .lock()
        .unwrap()
        .iter()
        .map(|head| {
            head.lines().find_map(|line| {
                let (field, value) = line.split_once(':')?;
                field
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_owned())
            })
        })
        .collect()
}

/// the request goes through each origin of `hops` as written; only the transport is local, so
/// `send` still decides what goes on each hop.
fn through(net: &Net, start: &str, hops: &[&str]) -> Arc<Mutex<Vec<String>>> {
    net.trust(
        std::iter::once(start)
            .chain(hops.iter().copied())
            .map(|url| Url::parse(url).unwrap().origin()),
    );
    let (local, asked) = redirects(hops);
    let _ = rustls::crypto::ring::default_provider().install_default();
    let transport = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let start = Url::parse(start).unwrap();
    let answer = block_on(net.send(&start, |request| {
        let request = request.build().unwrap();
        transport
            .get(local.clone())
            .headers(request.headers().clone())
    }));
    assert!(answer.is_ok(), "the scripted redirects must finish");
    asked
}

#[test]
fn a_site_credential_never_follows_a_redirect_off_its_first_origin() {
    let net = net(vec![token("data.lab", "t-1"), token("other.lab", "t-2")]);
    let asked = through(
        &net,
        "https://data.lab/start",
        &[
            "https://data.lab/same",
            "https://other.lab/away",
            "https://data.lab/back",
        ],
    );
    let token = Some("Bearer t-1".to_owned());
    assert_eq!(
        heard(&asked, "authorization"),
        [token.clone(), token, None, None]
    );
}

fn cookie(name: &str, path: &str, secure: bool, expires: Option<SystemTime>) -> Cookie {
    Cookie {
        name: name.to_owned(),
        value: format!("{name}-value"),
        path: path.to_owned(),
        secure,
        expires,
    }
}

#[test]
fn a_cookie_goes_by_its_path_secure_flag_and_expiry() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let later = Some(now + Duration::from_secs(60));
    let earlier = Some(now - Duration::from_secs(1));
    for (cookie, url, goes) in [
        (cookie("a", "/", false, None), "http://h/x", true),
        (cookie("a", "/data", false, later), "https://h/data", true),
        (cookie("a", "/data", false, later), "https://h/data/x", true),
        (
            cookie("a", "/data/", false, later),
            "https://h/data/x",
            true,
        ),
        (
            cookie("a", "/data", false, later),
            "https://h/database",
            false,
        ),
        (cookie("a", "/data", false, later), "https://h/", false),
        (cookie("a", "", false, later), "https://h/any", true),
        (cookie("a", "/", true, later), "https://h/x", true),
        (cookie("a", "/", true, later), "http://h/x", false),
        (cookie("a", "/", false, earlier), "https://h/x", false),
        (cookie("a", "/", false, Some(now)), "https://h/x", false),
    ] {
        let url = Url::parse(url).unwrap();
        assert_eq!(cookie.applies(&url, now), goes, "{cookie:?} to {url}");
    }
}

#[test]
fn the_runs_own_cookies_join_the_saved_ones_and_win_by_name() {
    let saved = [
        cookie("session", "/", false, None),
        cookie("pref", "/data", false, None),
    ];
    let saved: Vec<&Cookie> = saved.iter().collect();
    let alone = cookie_header(&saved, None).unwrap();
    assert!(alone.is_sensitive());
    assert_eq!(alone, "pref=pref-value; session=session-value");
    let jar = HeaderValue::from_static("session=renewed; csrf=1");
    assert_eq!(
        cookie_header(&saved, Some(&jar)).unwrap(),
        "pref=pref-value; session=renewed; csrf=1"
    );
}

#[test]
fn saved_cookies_never_follow_a_redirect_off_their_first_origin() {
    let session = Site::new(
        "data.lab",
        Vec::new(),
        vec![cookie("session", "/", true, None)],
        "the cookies".to_owned(),
        String::new(),
    )
    .unwrap();
    let net = net(vec![session]);
    let asked = through(
        &net,
        "https://data.lab/start",
        &["https://elsewhere.lab/sso", "https://data.lab/back"],
    );
    assert_eq!(
        heard(&asked, "cookie"),
        [Some("session=session-value".to_owned()), None, None]
    );
}

#[test]
fn a_refusal_names_what_was_saved_and_how_to_save_it_again() {
    let net = net(vec![
        token("data.lab", "t-1"),
        Site::new(
            "old.lab:8443",
            Vec::new(),
            vec![cookie(
                "session",
                "/",
                false,
                Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )],
            "the cookies".to_owned(),
            "sign in to old.lab:8443 in firefox, then run `fetchloom auth login old.lab:8443 --browser firefox`".to_owned(),
        )
        .unwrap(),
    ]);
    let mark = |url: &str| {
        let url = Url::parse(url).unwrap();
        let mut response = reqwest::Response::from(http::Response::new(Vec::<u8>::new()));
        let mut carried = None;
        net.mark_site(&mut response, &url, true, &mut carried);
        response.extensions().get::<Sent>().cloned()
    };
    let url = Url::parse("https://data.lab/file").unwrap();
    let refused = mark(url.as_str())
        .unwrap()
        .refusal(&url, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.kind, Kind::AuthRequired);
    assert_eq!(
        refused.message,
        "data.lab refused the token saved for it (401); it may have expired"
    );
    assert_eq!(
        refused.help.as_deref(),
        Some(
            "sign in again with `fetchloom auth login data.lab --token`, then run the same command again"
        )
    );
    let url = Url::parse("https://old.lab:8443/file?token=abc").unwrap();
    let expired = mark(url.as_str()).unwrap();
    assert_eq!(
        expired.refusal(&url, StatusCode::FORBIDDEN).message,
        "old.lab:8443 refused access to https://old.lab:8443/file?token=*** (403), and the cookies saved for it have expired"
    );
    assert!(mark("https://elsewhere.lab/file").is_none());
}
