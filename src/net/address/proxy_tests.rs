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
            typed.add_host("inward.invalid");
            assert!(proxies.check(&url, &typed).await.is_ok());
            assert_eq!(lookups.load(Ordering::SeqCst), 1);
        });
    }
}
