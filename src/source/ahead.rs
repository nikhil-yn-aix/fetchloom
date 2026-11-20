//! requests a walk of folders asks before their turn, a few at once, whose answers it reads in
//! the order it asked them, so it finds, warns of and stops at what it would asking one at a time.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use crate::error::Error;
use crate::net::Response;

/// an answer, or what kept it.
type Answer = Result<Response, Error>;

/// a request asked ahead, until its answer comes.
struct Pending<'a> {
    asking: Pin<Box<dyn Future<Output = Answer> + Send + 'a>>,
    answer: Option<Answer>,
}

impl Pending<'_> {
    /// lets the request make progress, keeping its answer once it comes.
    fn poll(&mut self, cx: &mut std::task::Context<'_>) {
        if self.answer.is_none()
            && let Poll::Ready(answer) = self.asking.as_mut().poll(cx)
        {
            self.answer = Some(answer);
        }
    }
}

/// up to `window` requests at once, each with what the walk keeps beside it, `K`, handed back in
/// the order they were asked.
pub(super) struct Ahead<'a, K> {
    window: usize,
    asked: VecDeque<(K, Pending<'a>)>,
}

impl<'a, K> Ahead<'a, K> {
    pub(super) fn new(window: usize) -> Ahead<'a, K> {
        Ahead {
            window: window.max(1),
            asked: VecDeque::new(),
        }
    }

    /// whether another request may be asked now.
    pub(super) fn has_room(&self) -> bool {
        self.asked.len() < self.window
    }

    /// asks `asking` now, its answer read after every request asked before it.
    pub(super) fn ask(&mut self, kept: K, asking: impl Future<Output = Answer> + Send + 'a) {
        self.asked.push_back((
            kept,
            Pending {
                asking: Box::pin(asking),
                answer: None,
            },
        ));
    }

    /// keeps `answer` in turn, as a request that ends at once: a refusal the walk finds before
    /// asking, told only after every answer asked before it.
    pub(super) fn answered(&mut self, kept: K, answer: Answer) {
        self.ask(kept, std::future::ready(answer));
    }

    /// waits for `asking`, a request the walk needs before its next turn, as a folder's next page,
    /// while the requests asked ahead go on: one that holds its host's slot must keep moving, or
    /// `asking` could wait for that slot forever. `asking` is polled first, so where its host has
    /// one slot free it takes it before any request asked ahead.
    pub(super) async fn meanwhile<T>(&mut self, asking: impl Future<Output = T>) -> T {
        let mut asking = std::pin::pin!(asking);
        std::future::poll_fn(|cx| {
            if let Poll::Ready(answer) = asking.as_mut().poll(cx) {
                return Poll::Ready(answer);
            }
            for (_, pending) in &mut self.asked {
                pending.poll(cx);
            }
            Poll::Pending
        })
        .await
    }

    /// the request asked first, with its answer, once that answer comes, the others making
    /// progress meanwhile; none when nothing is asked.
    pub(super) async fn next(&mut self) -> Option<(K, Answer)> {
        if self.asked.is_empty() {
            return None;
        }
        std::future::poll_fn(|cx| {
            for (_, pending) in &mut self.asked {
                pending.poll(cx);
            }
            match self.asked.front().map(|(_, first)| first.answer.is_some()) {
                Some(true) => Poll::Ready(()),
                _ => Poll::Pending,
            }
        })
        .await;
        let (kept, pending) = self.asked.pop_front()?;
        pending.answer.map(|answer| (kept, answer))
    }
}
