use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::source::recorded::block_on;

#[test]
fn proxy_verdicts_are_cached_and_typed_hosts_skip_preflight() {
    for private in [false, true] {
        let lookups = Arc::new(AtomicUsize::new(0));
        let seen = lookups.clone();
        let proxies = Proxies::with_lookup("http://127.0.0.1:9", move |name, port| {
            assert_eq!(name, "inward.invalid");
            assert_eq!(port, 443);
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
            typed.add_host("INWARD.invalid");
            assert!(proxies.check(&url, &typed).await.is_ok());
            assert_eq!(lookups.load(Ordering::SeqCst), 1);
        });
    }
}

/// a proxy is named by where it is, never by the login its address may hold.
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

/// a value the proxy matcher cannot read would send every request around the proxy, as if the
/// variable were unset, so it is refused, naming the variable and never a login it holds.
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
