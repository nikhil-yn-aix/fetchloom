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
