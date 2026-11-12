//! a credential that expires, such as a google access token: kept while it has time left, and
//! fetched again by one caller while the others wait for it, so concurrent downloads share one
//! refresh.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::error::Error;

/// how long before it expires a credential is fetched again, so a request never leaves with one
/// that runs out on the way. one that lives less than twice this is fetched again halfway.
const MARGIN: Duration = Duration::from_secs(300);

/// one credential, fetched when first needed and again shortly before it expires.
pub(crate) struct Lease<T> {
    held: tokio::sync::Mutex<State<T>>,
}

enum State<T> {
    Empty,
    Held {
        value: T,
        renew: Instant,
    },
    /// a fetch failed for a reason another try would meet again, such as a revoked login.
    Failed(Error),
}

impl<T> Default for Lease<T> {
    fn default() -> Lease<T> {
        Lease {
            held: tokio::sync::Mutex::new(State::Empty),
        }
    }
}

impl<T: Clone + PartialEq> Lease<T> {
    /// the credential, fetched by `fetch` when none is held or the one held is due for renewal
    /// at `now`. a caller that comes while another fetches waits for that fetch. `fetch` gives
    /// the credential and how long it lives, counted from before it was asked for. a failed
    /// fetch is the answer for the rest of the run.
    ///
    /// # Errors
    ///
    /// what `fetch` failed with, now or earlier in the run.
    pub(crate) async fn get<F, Fut>(&self, now: impl Fn() -> Instant, fetch: F) -> Result<T, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(T, Duration), Error>>,
    {
        let mut held = self.held.lock().await;
        let asked = now();
        match &*held {
            State::Held { value, renew } if asked < *renew => return Ok(value.clone()),
            State::Failed(error) => return Err(error.clone()),
            State::Held { .. } | State::Empty => {}
        }
        match fetch().await {
            Ok((value, lifetime)) => {
                *held = State::Held {
                    value: value.clone(),
                    renew: renewal(asked, lifetime),
                };
                Ok(value)
            }
            Err(error) => {
                *held = State::Failed(error.clone());
                Err(error)
            }
        }
    }

    /// drops `stale`, which a server refused, unless another caller replaced it already, so the
    /// next caller fetches a new one.
    pub(crate) async fn forget(&self, stale: &T) {
        let mut held = self.held.lock().await;
        if matches!(&*held, State::Held { value, .. } if value == stale) {
            *held = State::Empty;
        }
    }
}

/// when a credential asked for at `asked` that lives `lifetime` is fetched again.
fn renewal(asked: Instant, lifetime: Duration) -> Instant {
    asked + lifetime.saturating_sub(MARGIN.min(lifetime / 2))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::error::Kind;
    use crate::source::recorded::block_on;

    #[test]
    fn a_credential_is_renewed_shortly_before_it_expires() {
        let asked = Instant::now();
        for (lifetime, renew) in [(3599, 3299), (600, 300), (400, 200), (60, 30), (0, 0)] {
            assert_eq!(
                renewal(asked, Duration::from_secs(lifetime)),
                asked + Duration::from_secs(renew),
                "{lifetime}"
            );
        }
    }

    /// a clock that reads `at` seconds past `start`, as the test moves it.
    fn clock(start: Instant, at: &AtomicUsize) -> impl Fn() -> Instant + '_ {
        move || start + Duration::from_secs(at.load(Ordering::SeqCst) as u64)
    }

    #[test]
    fn a_held_credential_is_used_until_its_renewal_then_fetched_again() {
        let lease = Lease::default();
        let start = Instant::now();
        let at = AtomicUsize::new(0);
        let fetched = AtomicUsize::new(0);
        let fetch = || async {
            let count = fetched.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((format!("token-{count}"), Duration::from_secs(3600)))
        };
        block_on(async {
            assert_eq!(
                lease.get(clock(start, &at), fetch).await.unwrap(),
                "token-1"
            );
            at.store(3299, Ordering::SeqCst);
            assert_eq!(
                lease.get(clock(start, &at), fetch).await.unwrap(),
                "token-1"
            );
            at.store(3300, Ordering::SeqCst);
            assert_eq!(
                lease.get(clock(start, &at), fetch).await.unwrap(),
                "token-2"
            );
        });
        assert_eq!(fetched.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn callers_that_come_during_a_fetch_share_it() {
        let lease = Lease::default();
        let start = Instant::now();
        let at = AtomicUsize::new(0);
        let fetched = AtomicUsize::new(0);
        let fetch = || async {
            fetched.fetch_add(1, Ordering::SeqCst);
            // the other callers run while this fetch waits on the network
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            Ok(("shared".to_owned(), Duration::from_secs(3600)))
        };
        let got = block_on(async {
            let get = || lease.get(clock(start, &at), fetch);
            let (a, b, c, d) = tokio::join!(get(), get(), get(), get());
            [a, b, c, d].map(Result::unwrap)
        });
        assert_eq!(got, ["shared"; 4].map(str::to_owned));
        assert_eq!(fetched.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_credential_is_fetched_again_once() {
        let lease = Lease::default();
        let start = Instant::now();
        let at = AtomicUsize::new(0);
        let fetched = AtomicUsize::new(0);
        let fetch = || async {
            let count = fetched.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((format!("token-{count}"), Duration::from_secs(3600)))
        };
        block_on(async {
            let first = lease.get(clock(start, &at), fetch).await.unwrap();
            // two requests that carried the first token are refused; the second finds it
            // replaced already
            lease.forget(&first).await;
            let second = lease.get(clock(start, &at), fetch).await.unwrap();
            lease.forget(&first).await;
            assert_eq!(lease.get(clock(start, &at), fetch).await.unwrap(), second);
            assert_eq!(second, "token-2");
        });
        assert_eq!(fetched.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_fetch_answers_the_rest_of_the_run() {
        let lease: Lease<String> = Lease::default();
        let start = Instant::now();
        let at = AtomicUsize::new(0);
        let fetched = AtomicUsize::new(0);
        let fetch = || async {
            fetched.fetch_add(1, Ordering::SeqCst);
            Err(Error::new(Kind::AuthRequired, "revoked"))
        };
        block_on(async {
            for _ in 0..3 {
                let error = lease.get(clock(start, &at), fetch).await.unwrap_err();
                assert_eq!(error.message, "revoked");
            }
        });
        assert_eq!(fetched.load(Ordering::SeqCst), 1);
    }
}
