//! a credential that may expire, such as a google token or a program's AWS keys: kept while it
//! has time left, and fetched again by one caller while the others wait for it.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::error::{Error, Kind};

/// how long before it expires a credential is fetched again, so a request signed with it, such
/// as a download that resumes, still has minutes to go.
const MARGIN: Duration = Duration::from_mins(5);

/// one credential, fetched when first needed and again shortly before it expires.
pub(crate) struct Lease<T> {
    held: tokio::sync::Mutex<State<T>>,
}

enum State<T> {
    Empty,
    /// a credential, and when it is fetched again: never, for one that does not expire.
    Held {
        value: T,
        renew: Option<Instant>,
    },
    /// a fetch failed for a reason another would meet again, such as a revoked login or a
    /// program that exited with an error.
    Failed(Error),
}

impl<T> Default for Lease<T> {
    fn default() -> Lease<T> {
        Lease {
            held: tokio::sync::Mutex::new(State::Empty),
        }
    }
}

impl<T: Clone> Lease<T> {
    /// the credential, fetched by `fetch` when none is held or the one held is due for renewal
    /// at `now`. `fetch` gives it and its lifetime from when it was asked, none if it never ends.
    ///
    /// # Errors
    ///
    /// what `fetch` failed with, now or earlier in the run: a failure is the answer for every
    /// later caller, unless a stop ended the fetch.
    pub(crate) async fn get<F, Fut>(&self, now: impl Fn() -> Instant, fetch: F) -> Result<T, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(T, Option<Duration>), Error>>,
    {
        let mut held = self.held.lock().await;
        let asked = now();
        match &*held {
            State::Held { value, renew } if renew.is_none_or(|renew| asked < renew) => {
                return Ok(value.clone());
            }
            State::Failed(error) => return Err(error.clone()),
            State::Held { .. } | State::Empty => {}
        }
        match fetch().await {
            Ok((value, lifetime)) => {
                *held = State::Held {
                    value: value.clone(),
                    renew: lifetime.and_then(|lifetime| renewal(asked, lifetime)),
                };
                Ok(value)
            }
            Err(error) if error.kind == Kind::Interrupted => Err(error),
            Err(error) => {
                *held = State::Failed(error.clone());
                Err(error)
            }
        }
    }
}

impl<T: PartialEq> Lease<T> {
    /// drops `stale`, which a server refused, unless another caller replaced it already, so the
    /// next caller fetches a new one.
    pub(crate) async fn forget(&self, stale: &T) {
        let mut held = self.held.lock().await;
        if matches!(&*held, State::Held { value, .. } if value == stale) {
            *held = State::Empty;
        }
    }
}

/// when a credential asked for at `asked` that lives `lifetime` is fetched again: [`MARGIN`]
/// before it expires, or halfway for a shorter one. none past any time a clock can hold.
fn renewal(asked: Instant, lifetime: Duration) -> Option<Instant> {
    asked.checked_add(lifetime.saturating_sub(MARGIN.min(lifetime / 2)))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::source::recorded::block_on;
    use crate::stop::Stop;

    /// a clock that reads `minutes` after `start`.
    fn at(start: Instant, minutes: u64) -> impl Fn() -> Instant {
        move || start + Duration::from_mins(minutes)
    }

    /// a fetch that gives `value`, which lives `lives`, or for ever when none, and counts itself
    /// in `fetches`.
    async fn giving(
        fetches: &AtomicUsize,
        value: &str,
        lives: Option<Duration>,
    ) -> Result<(String, Option<Duration>), Error> {
        fetches.fetch_add(1, Ordering::SeqCst);
        // a fetch lets other requests go on meanwhile
        tokio::task::yield_now().await;
        Ok((value.to_owned(), lives))
    }

    fn minutes(minutes: u64) -> Duration {
        Duration::from_mins(minutes)
    }

    #[test]
    fn a_credential_is_fetched_again_only_shortly_before_it_expires() {
        let start = Instant::now();
        let fetches = AtomicUsize::new(0);
        let got = |lease: &Lease<String>, minute, value, lives| {
            block_on(lease.get(at(start, minute), || giving(&fetches, value, lives))).unwrap()
        };
        // one that lives an hour serves every caller until five minutes before it expires
        let hour = Lease::default();
        assert_eq!(got(&hour, 0, "first", Some(minutes(60))), "first");
        assert_eq!(got(&hour, 54, "second", Some(minutes(65))), "first");
        assert_eq!(got(&hour, 55, "second", Some(minutes(65))), "second");
        assert_eq!(got(&hour, 114, "third", Some(minutes(65))), "second");
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        // one that lives four minutes is fetched again halfway
        let short = Lease::default();
        assert_eq!(got(&short, 0, "first", Some(minutes(4))), "first");
        assert_eq!(got(&short, 1, "second", Some(minutes(4))), "first");
        assert_eq!(got(&short, 2, "second", Some(minutes(4))), "second");
        assert_eq!(fetches.load(Ordering::SeqCst), 4);
        // one that never expires, or lives past any time the clock holds, is never fetched again
        for lives in [None, Some(Duration::MAX)] {
            let lasting = Lease::default();
            assert_eq!(got(&lasting, 0, "first", lives), "first");
            assert_eq!(got(&lasting, 100_000, "second", lives), "first");
        }
        assert_eq!(fetches.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn callers_at_once_share_one_fetch() {
        let lease = Arc::new(Lease::default());
        let fetches = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::watch::channel(false);
        let start = Instant::now();
        let values = block_on(async {
            let asking: Vec<_> = (0..8)
                .map(|_| {
                    let (lease, fetches, mut released) =
                        (lease.clone(), fetches.clone(), released.clone());
                    tokio::spawn(async move {
                        let fetch = || async move {
                            let given = giving(&fetches, "shared", Some(minutes(60))).await;
                            let _ = released.wait_for(|released| *released).await;
                            given
                        };
                        lease.get(at(start, 0), fetch).await
                    })
                })
                .collect();
            // every caller reaches the lease before the fetch that started first gives its value
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            release.send_replace(true);
            let mut values = Vec::new();
            for asked in asking {
                values.push(asked.await.unwrap().unwrap());
            }
            values
        });
        assert_eq!(values, vec!["shared"; 8]);
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_credential_is_fetched_again_once() {
        let lease = Lease::default();
        let start = Instant::now();
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            let count = fetches.fetch_add(1, Ordering::SeqCst) + 1;
            Ok((format!("token-{count}"), Some(Duration::from_hours(1))))
        };
        block_on(async {
            let first = lease.get(at(start, 0), fetch).await.unwrap();
            // two requests that carried the first token are refused; the second finds it
            // replaced already
            lease.forget(&first).await;
            let second = lease.get(at(start, 0), fetch).await.unwrap();
            lease.forget(&first).await;
            assert_eq!(lease.get(at(start, 0), fetch).await.unwrap(), second);
            assert_eq!(second, "token-2");
        });
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_fetch_answers_every_later_caller_and_a_stopped_one_none() {
        let start = Instant::now();
        let fetches = AtomicUsize::new(0);
        let lease: Lease<String> = Lease::default();
        let stopped = block_on(lease.get(at(start, 0), || async { Err(Stop::stopped()) }));
        assert_eq!(stopped.unwrap_err().kind, Kind::Interrupted);
        let failed = Error::new(Kind::AuthRequired, "the program exited with code 3");
        let first = block_on(lease.get(at(start, 0), || async { Err(failed.clone()) }));
        assert_eq!(first.unwrap_err().message, failed.message);
        let later = block_on(lease.get(at(start, 1), || giving(&fetches, "other", None)));
        assert_eq!(later.unwrap_err().message, failed.message);
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
    }
}
