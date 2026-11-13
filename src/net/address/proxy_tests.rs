use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::source::recorded::block_on;

#[test]
fn proxy_verdicts_are_cached_and_typed_origins_skip_preflight() {
    for private in [false, true] {
        let lookups = Arc::new(AtomicUsize::new(0));
        let seen = lookups.clone();
        let proxies = Proxies::with_lookup("http://127.0.0.1:9", move |name, _| {
            assert_eq!(name, "inward.invalid");
            seen.fetch_add(1, Ordering::SeqCst);
            Box::pin(
                async move { private.then(|| "name resolves to a private address".to_owned()) },
            )
        });
        let typed = Typed::default();
        let url = Url::parse("https://inward.invalid/file").unwrap();
        block_on(async {
            for _ in 0..2 {
                assert_eq!(proxies.check(&url, &typed).await.is_err(), private);
            }
            assert_eq!(lookups.load(Ordering::SeqCst), 1);
            typed.add(&Url::parse("https://INWARD.invalid:8443/typed").unwrap());
            let typed_url = Url::parse("https://inward.invalid:8443/file").unwrap();
            assert!(proxies.check(&typed_url, &typed).await.is_ok());
            assert_eq!(
                lookups.load(Ordering::SeqCst),
                1,
                "a typed origin is not looked up"
            );
            // the host's other origins are not typed, so the verdict on the name still holds
            assert_eq!(proxies.check(&url, &typed).await.is_err(), private);
        });
    }
}

#[test]
fn a_proxy_is_shown_without_its_login() {
    let url = Url::parse("https://data.example.org/iris.csv").unwrap();
    let matcher = Matcher::builder()
        .all("http://name:secret@proxy.example.org:8080".to_owned())
        .build();
    assert_eq!(
        shown_proxy(&matcher, &url).as_deref(),
        Some("http://proxy.example.org:8080")
    );
    assert_eq!(shown_proxy(&Matcher::builder().build(), &url), None);
}

/// the environment a test gives: each variable `pairs` names, and no other.
fn env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs = pairs.to_vec();
    move |name| {
        pairs
            .iter()
            .find(|(set, _)| *set == name)
            .map(|(_, value)| OsString::from(value))
    }
}

/// the matcher would pass such a value over and send requests around the proxy. the refusal names
/// the variable, never its login.
#[test]
fn a_proxy_setting_that_names_no_proxy_is_refused() {
    for (variable, value) in [
        ("HTTPS_PROXY", "htp:/proxy:80"),
        ("https_proxy", "ftp://proxy.lab:21"),
        ("HTTP_PROXY", " http://proxy.lab:8080"),
        ("http_proxy", "http://"),
        ("ALL_PROXY", "proxy lab"),
        ("all_proxy", "htp://name:secret@proxy.lab:80"),
    ] {
        let error = named(&env(&[(variable, value)])).unwrap_err();
        assert_eq!(error.kind, crate::error::Kind::Usage, "{variable}={value}");
        assert!(
            error.message.starts_with(&format!("{variable} is \"")),
            "{}",
            error.message
        );
        assert!(!error.message.contains("secret"), "{}", error.message);
        let help = error.help.as_deref().unwrap_or_default();
        assert!(help.contains(&format!("unset {variable}")), "{help}");
    }
}

/// what the matcher reads is accepted and an empty value is no proxy. of a pair, the variable
/// the matcher reads is the one checked: the uppercase name when it is set.
#[test]
fn a_readable_proxy_setting_is_named_by_its_variable() {
    for value in [
        "http://proxy.lab:8080",
        "proxy.lab:3128",
        "127.0.0.1:18849",
        "https://proxy.lab",
        "socks5://proxy.lab:1080",
        "http://name:secret@proxy.lab:8080",
    ] {
        let found = named(&env(&[("HTTPS_PROXY", value)])).unwrap();
        assert_eq!(found.len(), 1, "{value}");
    }
    assert_eq!(named(&env(&[("HTTPS_PROXY", "")])).unwrap(), Vec::new());
    assert_eq!(named(&env(&[])).unwrap(), Vec::new());
    let both = named(&env(&[
        ("HTTPS_PROXY", "http://upper.lab:1"),
        ("https_proxy", "htp:/lower"),
    ]))
    .unwrap();
    assert_eq!(
        both,
        [("HTTPS_PROXY", http::Uri::from_static("http://upper.lab:1/"))]
    );
}

#[test]
fn a_login_in_a_proxy_setting_is_never_shown() {
    for (value, shown) in [
        ("htp://name:secret@proxy:80", "htp://***@proxy:80"),
        ("name:secret@proxy:80", "***@proxy:80"),
        ("htp:/proxy:80", "htp:/proxy:80"),
        ("http://a@b@proxy", "http://***@proxy"),
    ] {
        assert_eq!(without_login(value), shown);
    }
}

/// the proxies a test environment sets, chosen by a matcher built from the same variables.
fn proxies(pairs: &[(&'static str, &'static str)]) -> Proxies {
    let var = env(pairs);
    let first = |pair: [&str; 2]| {
        pair.iter()
            .find_map(|name| var(name))
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    Proxies {
        matcher: Matcher::builder()
            .https(first(["HTTPS_PROXY", "https_proxy"]))
            .http(first(["HTTP_PROXY", "http_proxy"]))
            .all(first(["ALL_PROXY", "all_proxy"]))
            .no(first(["NO_PROXY", "no_proxy"]))
            .build(),
        named: named(&var).unwrap(),
        looked_up: Mutex::default(),
        checked: Mutex::default(),
        test_lookup: None,
        test_probe: None,
    }
}

/// the variable is the one for the link's scheme, else `ALL_PROXY`, and the address shows no login.
/// a proxy no variable names came from the system's settings.
#[test]
fn a_proxy_is_named_by_the_variable_that_set_it() {
    let https = Url::parse("https://data.example.org/iris.csv").unwrap();
    let http = Url::parse("http://data.example.org/iris.csv").unwrap();
    let named = |proxies: &Proxies, url: &Url| {
        proxies
            .carrier(url)
            .map(|carrier| (carrier.shown, carrier.variable))
    };
    let both = proxies(&[
        ("HTTPS_PROXY", "http://name:secret@secure.lab:3128"),
        ("http_proxy", "plain.lab:8080"),
    ]);
    assert_eq!(
        named(&both, &https),
        Some(("http://secure.lab:3128".to_owned(), Some("HTTPS_PROXY")))
    );
    assert_eq!(
        named(&both, &http),
        Some(("http://plain.lab:8080".to_owned(), Some("http_proxy")))
    );
    let all = proxies(&[("ALL_PROXY", "http://all.lab:1080")]);
    for url in [&https, &http] {
        assert_eq!(
            named(&all, url),
            Some(("http://all.lab:1080".to_owned(), Some("ALL_PROXY")))
        );
    }
    let passed = proxies(&[
        ("HTTPS_PROXY", "http://secure.lab:3128"),
        ("NO_PROXY", "example.org"),
    ]);
    assert_eq!(named(&passed, &https), None);
    let system = Proxies::with_lookup("http://system.lab:8080", |_, _| Box::pin(async { None }));
    assert_eq!(
        named(&system, &https),
        Some(("http://system.lab:8080".to_owned(), None))
    );
}

/// the proxy is checked once a request through it has waited three seconds, and the check gives up
/// five later, not after each try's connect timeout. later requests are told at once.
#[tokio::test(start_paused = true)]
async fn a_proxy_that_drops_every_packet_is_found_in_seconds() {
    let proxies = Proxies::with_probe("HTTPS_PROXY", "http://192.0.2.1:8080", |_, _| {
        Box::pin(std::future::pending())
    });
    let url = Url::parse("https://data.example.org/iris.csv").unwrap();
    let carrier = proxies.carrier(&url).unwrap();
    let start = tokio::time::Instant::now();
    let reason = proxies.watch(&carrier).await;
    assert_eq!(reason, "no connection to it opened within 5 s");
    assert_eq!(start.elapsed(), Duration::from_secs(8));
    let again = tokio::time::Instant::now();
    assert_eq!(proxies.watch(&carrier).await, reason);
    assert_eq!(proxies.unreachable(&carrier).await, Some(reason));
    assert_eq!(again.elapsed(), Duration::ZERO);
}

/// a proxy that answers its check, or a request, is left alone: the request goes on however
/// long its answer takes, and the proxy is not checked again this run.
#[tokio::test(start_paused = true)]
async fn a_proxy_that_answers_leaves_its_requests_alone() {
    for answered in [false, true] {
        let checks = Arc::new(AtomicUsize::new(0));
        let counted = checks.clone();
        let proxies = Proxies::with_probe("HTTPS_PROXY", "http://proxy.lab:8080", move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Probe::Opened })
        });
        let url = Url::parse("https://data.example.org/iris.csv").unwrap();
        let carrier = proxies.carrier(&url).unwrap();
        if answered {
            proxies.reached(&carrier);
        }
        let watched = tokio::time::timeout(Duration::from_hours(1), proxies.watch(&carrier));
        assert!(watched.await.is_err(), "the watch never ends");
        assert_eq!(proxies.unreachable(&carrier).await, None);
        assert_eq!(checks.load(Ordering::SeqCst), usize::from(!answered));
    }
}

/// a lookup of the proxy's name that failed for now says nothing about the proxy, so the
/// request goes on as before, and the next one checks again.
#[tokio::test(start_paused = true)]
async fn an_unsure_check_is_made_again() {
    let checks = Arc::new(AtomicUsize::new(0));
    let counted = checks.clone();
    let proxies = Proxies::with_probe("HTTPS_PROXY", "http://proxy.lab:8080", move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Probe::Unsure })
    });
    let url = Url::parse("https://data.example.org/iris.csv").unwrap();
    let carrier = proxies.carrier(&url).unwrap();
    assert_eq!(proxies.unreachable(&carrier).await, None);
    assert_eq!(proxies.unreachable(&carrier).await, None);
    assert_eq!(checks.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_connection_says_why_in_the_same_words_everywhere() {
    use std::io::{Error, ErrorKind};
    for (kind, said) in [
        (ErrorKind::ConnectionRefused, "it refused the connection"),
        (
            ErrorKind::HostUnreachable,
            "this machine has no route to it",
        ),
        (
            ErrorKind::NetworkUnreachable,
            "this machine has no route to it",
        ),
        (ErrorKind::TimedOut, "no connection to it opened in time"),
    ] {
        assert_eq!(reach::why(&Error::from(kind)), said, "{kind:?}");
    }
}
