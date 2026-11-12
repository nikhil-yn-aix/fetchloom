//! keys a program gave, held for the run until shortly before they expire. one request at a time
//! runs the program: the others wait for its answer and share it, and an answer that failed
//! fails every later request the same way, so no request goes out without a key, or with
//! another, once the program was chosen.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::program::Issued;
use crate::error::{Error, Kind};
use crate::net::sign::Keys;

/// how long before keys expire the program runs again, so a request signed with them, such as a
/// download that resumes, still has minutes to go.
const AHEAD: Duration = Duration::from_mins(5);

/// the keys a program last gave, or why it gave none.
#[derive(Default)]
pub(crate) struct Lease {
    held: tokio::sync::Mutex<Option<Result<Held, Error>>>,
}

struct Held {
    keys: Arc<Keys>,
    /// when the program runs again, none for keys that never expire.
    renew: Option<SystemTime>,
}

impl Lease {
    /// the keys for a request made at `now`: those held, until shortly before they expire, else
    /// what `run` gives, which no other request runs meanwhile.
    ///
    /// # Errors
    ///
    /// what `run` failed with, kept for every later request unless a stop ended it.
    pub(crate) async fn keys(
        &self,
        now: SystemTime,
        run: impl Future<Output = Result<Issued, Error>>,
    ) -> Result<Arc<Keys>, Error> {
        let mut held = self.held.lock().await;
        match &*held {
            Some(Ok(held)) if held.renew.is_none_or(|renew| now < renew) => {
                return Ok(held.keys.clone());
            }
            Some(Err(error)) => return Err(error.clone()),
            _ => {}
        }
        match run.await {
            Ok(issued) => {
                let keys = Arc::new(issued.keys);
                *held = Some(Ok(Held {
                    keys: keys.clone(),
                    renew: issued.expires.map(|expires| renew(now, expires)),
                }));
                Ok(keys)
            }
            Err(error) if error.kind == Kind::Interrupted => Err(error),
            Err(error) => {
                *held = Some(Err(error.clone()));
                Err(error)
            }
        }
    }
}

/// when keys got at `now` that expire at `expires` are asked for again: [`AHEAD`] before they
/// expire, or halfway for keys that hold less than twice that.
fn renew(now: SystemTime, expires: SystemTime) -> SystemTime {
    let left = expires.duration_since(now).unwrap_or_default();
    expires - AHEAD.min(left / 2)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::source::recorded::block_on;
    use crate::stop::Stop;

    /// `minutes` after a fixed moment.
    fn at(minutes: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000 + minutes * 60)
    }

    /// a run that gives the key `id`, which expires at `expires`, and counts itself in `runs`.
    async fn giving(
        runs: &AtomicUsize,
        id: &str,
        expires: Option<SystemTime>,
    ) -> Result<Issued, Error> {
        runs.fetch_add(1, Ordering::SeqCst);
        // a program's run lets other requests go on meanwhile
        tokio::task::yield_now().await;
        Ok(Issued {
            keys: Keys {
                id: id.to_owned(),
                secret: "secret".to_owned(),
                token: None,
            },
            expires,
        })
    }

    #[test]
    fn keys_are_asked_for_again_only_shortly_before_they_expire() {
        let runs = AtomicUsize::new(0);
        let id = |lease: &Lease, now, given, expires| {
            block_on(lease.keys(now, giving(&runs, given, expires)))
                .unwrap()
                .id
                .clone()
        };
        // keys that hold an hour serve every request until five minutes before they expire
        let hour = Lease::default();
        assert_eq!(id(&hour, at(0), "first", Some(at(60))), "first");
        assert_eq!(id(&hour, at(54), "second", Some(at(120))), "first");
        assert_eq!(id(&hour, at(55), "second", Some(at(120))), "second");
        assert_eq!(id(&hour, at(114), "third", Some(at(180))), "second");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        // keys that hold four minutes are asked for again halfway
        let short = Lease::default();
        assert_eq!(id(&short, at(0), "first", Some(at(4))), "first");
        assert_eq!(id(&short, at(1), "second", Some(at(6))), "first");
        assert_eq!(id(&short, at(2), "second", Some(at(6))), "second");
        assert_eq!(runs.load(Ordering::SeqCst), 4);
        // keys without an expiry are never asked for again
        let lasting = Lease::default();
        assert_eq!(id(&lasting, at(0), "first", None), "first");
        assert_eq!(id(&lasting, at(100_000), "second", None), "first");
        assert_eq!(runs.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn requests_at_once_share_one_run() {
        let lease = Arc::new(Lease::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let (release, released) = tokio::sync::watch::channel(false);
        let ids = block_on(async {
            let asking: Vec<_> = (0..8)
                .map(|_| {
                    let (lease, runs, mut released) =
                        (lease.clone(), runs.clone(), released.clone());
                    tokio::spawn(async move {
                        let run = async {
                            let given = giving(&runs, "shared", Some(at(60))).await;
                            let _ = released.wait_for(|released| *released).await;
                            given
                        };
                        lease.keys(at(0), run).await.map(|keys| keys.id.clone())
                    })
                })
                .collect();
            // every request reaches the lease before the run that started first gives its keys
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            release.send_replace(true);
            let mut ids = Vec::new();
            for asked in asking {
                ids.push(asked.await.unwrap().unwrap());
            }
            ids
        });
        assert_eq!(ids, vec!["shared"; 8]);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_run_fails_every_later_request_and_a_stopped_one_none() {
        let runs = AtomicUsize::new(0);
        let lease = Lease::default();
        let stopped = block_on(lease.keys(at(0), async { Err(Stop::stopped()) }));
        assert_eq!(stopped.unwrap_err().kind, Kind::Interrupted);
        let failed = Error::new(Kind::AuthRequired, "the program exited with code 3");
        let first = block_on(lease.keys(at(0), async { Err(failed.clone()) }));
        assert_eq!(first.unwrap_err().message, failed.message);
        let later = block_on(lease.keys(at(1), giving(&runs, "other", None)));
        assert_eq!(later.unwrap_err().message, failed.message);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
