//! when a request's connection opened, so its wait for an answer counts from then: a connection
//! gets the connect time to open, and the answer the run's timeout after it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::time::Instant;

tokio::task_local! {
    /// what the request this task sends learns of its connection.
    static OPENING: Arc<Opening>;
}

/// what a request learned of its connection: whether a new one began to open for it, and when
/// it opened. a request on a connection already open learns nothing.
#[derive(Default)]
pub(super) struct Opening {
    state: Mutex<(bool, Option<Instant>)>,
}

impl Opening {
    /// `send`, with what it learns of its connection kept here.
    pub(super) fn watching<F: Future>(
        self: &Arc<Self>,
        send: F,
    ) -> impl Future<Output = F::Output> {
        OPENING.scope(Arc::clone(self), send)
    }

    /// the moment by which an answer must begin, for a request sent at `sent` that may stay
    /// silent for `idle`: `idle` after its connection opened, or after it was sent on one already
    /// open. none while a connection is still opening, which the connect time bounds instead.
    pub(super) fn answer_by(&self, sent: Instant, idle: Duration) -> Option<Instant> {
        match *self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            (_, Some(opened)) => Some(opened + idle),
            (true, None) => None,
            (false, None) => Some(sent + idle),
        }
    }

    /// whether a new connection began to open for the request and has not opened yet.
    pub(super) fn connecting(&self) -> bool {
        let (began, opened) = *self.state.lock().unwrap_or_else(PoisonError::into_inner);
        began && opened.is_none()
    }

    fn began(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0 = true;
    }

    fn opened(&self) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).1 = Some(Instant::now());
    }
}

/// the layer under reqwest's connector that tells the request opening a connection when it began
/// and when it opened.
#[derive(Clone, Copy)]
pub(super) struct Watching;

impl<S> tower_layer::Layer<S> for Watching {
    type Service = Watched<S>;

    fn layer(&self, inner: S) -> Watched<S> {
        Watched(inner)
    }
}

#[derive(Clone)]
pub(super) struct Watched<S>(S);

impl<S, R> tower_service::Service<R> for Watched<S>
where
    S: tower_service::Service<R>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        // the pool may finish a connection in a task of its own, which then tells no request
        let opening = OPENING.try_with(Arc::clone).ok();
        if let Some(opening) = &opening {
            opening.began();
        }
        let connecting = self.0.call(request);
        Box::pin(async move {
            let connected = connecting.await;
            if let Some(opening) = opening
                && connected.is_ok()
            {
                opening.opened();
            }
            connected
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the answer's wait counts from when the connection opened, or from the request on one
    /// already open, and not at all while a connection is still opening.
    #[tokio::test(start_paused = true)]
    async fn an_answer_is_waited_for_from_when_its_connection_opened() {
        let idle = Duration::from_secs(5);
        let sent = Instant::now();
        let reused = Opening::default();
        assert_eq!(reused.answer_by(sent, idle), Some(sent + idle));
        let fresh = Opening::default();
        fresh.began();
        assert_eq!(fresh.answer_by(sent, idle), None);
        tokio::time::advance(Duration::from_secs(40)).await;
        fresh.opened();
        assert_eq!(
            fresh.answer_by(sent, idle),
            Some(sent + Duration::from_secs(45))
        );
    }

    /// a request is connecting only between its connection beginning to open and opening, so a
    /// stop then names the connection, and one after names the wait for an answer.
    #[test]
    fn a_request_is_connecting_until_its_connection_opens() {
        let reused = Opening::default();
        assert!(!reused.connecting());
        let fresh = Opening::default();
        fresh.began();
        assert!(fresh.connecting());
        fresh.opened();
        assert!(!fresh.connecting());
    }
}
