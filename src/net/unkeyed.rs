//! the repositories a run asks without a key for files their source shows to a key alone. a
//! repository answers alike for all its files, so the first such request for one goes alone and
//! its answer holds for the rest: a refusal refuses them unasked, so they spend none of the
//! requests a host allows an address, and a file served lets them all go out.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use tokio::sync::watch;

/// what the run knows of each repository it asked without a key, by the name its source gives it.
#[derive(Default)]
pub(super) struct Unkeyed(Mutex<HashMap<String, Known>>);

enum Known {
    /// a request is out, whose answer the others wait for.
    Asking(watch::Receiver<()>),
    /// it served a file to a request without a key.
    Served,
    /// it refused one.
    Refused,
}

/// what a request for a repository's file does: goes out as any other, goes first while the
/// others wait for its answer, or is refused unasked.
pub(super) enum Turn<'a> {
    Ask,
    First(First<'a>),
    Refused,
}

/// the first request for a repository's file, whose answer decides for the rest. one that ends
/// with neither a file nor a refusal leaves the next request to ask.
pub(super) struct First<'a> {
    unkeyed: &'a Unkeyed,
    repository: String,
    told: watch::Sender<()>,
}

impl Unkeyed {
    /// the turn of a request for a file of `repository`, once a request out for it has answered.
    pub(super) async fn turn(&self, repository: &str) -> Turn<'_> {
        loop {
            let mut asking = {
                let mut known = self.0.lock().unwrap_or_else(PoisonError::into_inner);
                match known.get(repository) {
                    Some(Known::Served) => return Turn::Ask,
                    Some(Known::Refused) => return Turn::Refused,
                    Some(Known::Asking(asking)) => asking.clone(),
                    None => {
                        let (told, asking) = watch::channel(());
                        known.insert(repository.to_owned(), Known::Asking(asking));
                        return Turn::First(First {
                            unkeyed: self,
                            repository: repository.to_owned(),
                            told,
                        });
                    }
                }
            };
            // an error says the first request ended without an answer, as a told one does not
            let _ = asking.changed().await;
        }
    }

    /// that `repository` refused the first request without a key, so the rest are refused.
    pub(super) fn refused(&self, repository: &str) {
        self.answered(repository, Known::Refused);
    }

    /// `now` for `repository` while its first request is out; what it answered stays.
    fn answered(&self, repository: &str, now: Known) {
        let mut known = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(asked @ Known::Asking(_)) = known.get_mut(repository) {
            *asked = now;
        }
    }
}

impl First<'_> {
    /// that the repository served the file, so the others go out too.
    pub(super) fn served(&self) {
        self.unkeyed.answered(&self.repository, Known::Served);
        self.told.send_replace(());
    }
}

impl Drop for First<'_> {
    fn drop(&mut self) {
        let mut known = self
            .unkeyed
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if matches!(known.get(&self.repository), Some(Known::Asking(_))) {
            known.remove(&self.repository);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future, poll_fn};
    use std::pin::{Pin, pin};
    use std::task::Poll;

    use super::*;

    /// `turn`'s answer when it has one at once, none while it waits.
    async fn now<'a, F: Future<Output = Turn<'a>>>(mut turn: Pin<&mut F>) -> Option<Turn<'a>> {
        poll_fn(|cx| match turn.as_mut().poll(cx) {
            Poll::Ready(turn) => Poll::Ready(Some(turn)),
            Poll::Pending => Poll::Ready(None),
        })
        .await
    }

    /// the requests that wait for a first one learn its answer: a refusal refuses them, a file
    /// served lets them go out, and an end with neither lets the next one ask. each repository
    /// waits for its own.
    #[tokio::test]
    async fn the_first_request_answers_for_its_repository() {
        let unkeyed = Unkeyed::default();
        let Some(Turn::First(first)) = now(pin!(unkeyed.turn("lab/secret"))).await else {
            panic!("the first request goes first");
        };
        assert!(matches!(
            now(pin!(unkeyed.turn("lab/other"))).await,
            Some(Turn::First(_))
        ));
        let mut waiting = pin!(unkeyed.turn("lab/secret"));
        assert!(
            now(waiting.as_mut()).await.is_none(),
            "it waits for the first"
        );
        unkeyed.refused("lab/secret");
        drop(first);
        assert!(matches!(waiting.await, Turn::Refused));

        let Some(Turn::First(first)) = now(pin!(unkeyed.turn("lab/public"))).await else {
            panic!("the first request goes first");
        };
        let mut waiting = pin!(unkeyed.turn("lab/public"));
        assert!(
            now(waiting.as_mut()).await.is_none(),
            "it waits for the first"
        );
        first.served();
        assert!(matches!(waiting.await, Turn::Ask));
        unkeyed.refused("lab/public");
        drop(first);
        assert!(
            matches!(now(pin!(unkeyed.turn("lab/public"))).await, Some(Turn::Ask)),
            "a file served stays its answer"
        );

        let Some(Turn::First(first)) = now(pin!(unkeyed.turn("lab/flaky"))).await else {
            panic!("the first request goes first");
        };
        let mut waiting = pin!(unkeyed.turn("lab/flaky"));
        assert!(
            now(waiting.as_mut()).await.is_none(),
            "it waits for the first"
        );
        drop(first);
        assert!(
            matches!(waiting.await, Turn::First(_)),
            "a first request that ended without an answer leaves the next to ask"
        );
    }
}
