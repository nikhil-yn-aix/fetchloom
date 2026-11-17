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
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::pin::{Pin, pin};
    use std::sync::Arc;
    use std::task::Poll;

    use url::Url;

    use super::*;
    use crate::cache::Cache;
    use crate::error::{Error, Kind};
    use crate::net::{Event, Fetch, Net, Offline, Options, Splitting};

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

    /// a repository's first file asked without a key lets the rest of its files go out once its
    /// bytes start, not after they end, as a repository made public since its pin serves them all.
    #[tokio::test]
    async fn a_first_file_served_lets_the_rest_go_while_its_bytes_come() {
        let body: Vec<u8> = (0..=250u8).cycle().take(1 << 16).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = Url::parse(&format!("http://{address}/blobs/first")).unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let sent = body.clone();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                request.push(byte[0]);
            }
            let (first, rest) = sent.split_at(sent.len() / 2);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                sent.len()
            );
            let _ = stream.write_all(first);
            let _ = stream.flush();
            // the rest waits until the test has seen the next file's turn
            let _ = held.recv();
            let _ = stream.write_all(rest);
        });
        let options = Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::Never,
        };
        let mut net = Net::new(options, Arc::default(), Arc::default(), Arc::new(|_| {}));
        net.typed(&url);
        net.rules.keyless = |url| {
            url.path().starts_with("/blobs/").then(|| {
                (
                    "lab/public".to_owned(),
                    Error::new(Kind::AuthRequired, "lab/public shows its files to a key"),
                )
            })
        };
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let (started, on_started) = tokio::sync::oneshot::channel();
        let started = Mutex::new(Some(started));
        let on = |event: Event| {
            if matches!(event, Event::Started { .. })
                && let Some(started) = started.lock().unwrap().take()
            {
                let _ = started.send(());
            }
            Ok(())
        };
        let fetch = Fetch::new(url);
        let second = async {
            on_started.await.unwrap();
            let turn = now(pin!(net.unkeyed.turn("lab/public"))).await;
            let _ = release.send(());
            turn
        };
        let (fetched, turn) = tokio::join!(net.download(&fetch, &cache, &on), second);
        assert!(
            matches!(turn, Some(Turn::Ask)),
            "the next file goes out while the first one's bytes come"
        );
        assert_eq!(fetched.unwrap().size, body.len() as u64);
    }
}
