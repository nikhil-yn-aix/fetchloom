use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use reqwest::header::{self, HeaderName, HeaderValue};
use tokio::sync::Notify;
use url::Url;

use super::Response;
use crate::net::{Authorized, Wire};
use crate::source::Keyed;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Sent {
    url: Url,
    headers: Vec<(HeaderName, HeaderValue)>,
    trusted: bool,
    cookie_generation: u64,
}

impl Sent {
    pub(crate) fn capture(
        wire: &Wire,
        hop: &Url,
        request: &reqwest::RequestBuilder,
    ) -> Option<Self> {
        let request = request.try_clone()?.build().ok()?;
        let mut headers = request.headers().clone();
        let (cookie, cookie_generation) = wire.cookies.snapshot(request.url());
        if !headers.contains_key(header::COOKIE)
            && let Some(cookie) = cookie
        {
            headers.insert(header::COOKIE, cookie);
        }
        let mut headers: Vec<_> = headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        headers.sort_unstable_by(|(a, av), (b, bv)| {
            a.as_str()
                .cmp(b.as_str())
                .then_with(|| av.as_bytes().cmp(bv.as_bytes()))
        });
        Some(Self {
            url: request.url().clone(),
            headers,
            trusted: wire.typed.trusts(hop),
            cookie_generation,
        })
    }

    pub(super) fn current(&self, wire: &Wire) -> bool {
        self.cookie_generation == wire.cookies.generation()
    }

    fn bytes(&self) -> usize {
        self.url.as_str().len()
            + self.headers.capacity() * std::mem::size_of::<(HeaderName, HeaderValue)>()
            + self
                .headers
                .iter()
                .map(|(name, value)| name.as_str().len() + value.len())
                .sum::<usize>()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Key {
    pub(super) sent: Sent,
    pub(super) keyed: Option<Keyed>,
    pub(super) authorized: Option<Authorized>,
    pub(super) drive: Option<Arc<crate::net::DriveAnswered>>,
    pub(super) mirror: bool,
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.sent.hash(state);
        self.mirror.hash(state);
    }
}

impl Key {
    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.sent.bytes()
            + keyed_bytes(self.keyed.as_ref())
            + authorized_bytes(self.authorized.as_ref())
            + self.drive.as_ref().map_or(0, |drive| {
                std::mem::size_of::<crate::net::DriveAnswered>()
                    + 2 * std::mem::size_of::<usize>()
                    + drive.bytes()
            })
    }
}

fn keyed_bytes(keyed: Option<&Keyed>) -> usize {
    match keyed {
        Some(Keyed::Signed {
            bucket,
            key,
            region,
        }) => bucket.to_string().len() + key.len() + region.len(),
        Some(Keyed::Unsigned { bucket, .. }) => bucket.to_string().len(),
        None => 0,
    }
}

fn authorized_bytes(authorized: Option<&Authorized>) -> usize {
    match authorized {
        Some(Authorized::Token {
            bucket, identity, ..
        }) => bucket.len() + identity.len(),
        Some(Authorized::Anonymous { bucket, .. }) => bucket.len(),
        Some(Authorized::Azure(crate::net::Sent::Anonymous { container, .. })) => container.len(),
        Some(Authorized::Azure(crate::net::Sent::Sas {
            container, expiry, ..
        })) => container.len() + expiry.as_ref().map_or(0, String::len),
        None => 0,
    }
}

fn response_bytes(response: &Response) -> usize {
    std::mem::size_of::<Response>()
        + response.body.capacity()
        + response.url.as_str().len()
        + response.next.as_ref().map_or(0, |url| url.as_str().len())
        + response.region.as_ref().map_or(0, String::len)
        + keyed_bytes(response.keyed.as_ref())
        + authorized_bytes(response.authorized.as_ref())
        + response
            .drive
            .as_ref()
            .map_or(0, crate::net::DriveAnswered::bytes)
}

pub(crate) struct Memo {
    limit: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, Entry>,
    ready: VecDeque<Key>,
    bytes: usize,
}

enum Entry {
    Pending {
        flight: Arc<Flight>,
        bytes: usize,
    },
    Ready {
        response: Box<Response>,
        bytes: usize,
    },
}

#[derive(Default)]
pub(super) struct Flight {
    finished: AtomicBool,
    changed: Notify,
    response: Mutex<Option<(Key, Response)>>,
}

impl Flight {
    pub(super) fn response(&self, key: &Key) -> Option<Response> {
        self.response
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(sent, _)| sent == key)
            .map(|(_, response)| response.clone())
    }

    pub(super) async fn finished(&self) {
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if !self.finished.load(Ordering::Acquire) {
            changed.await;
        }
    }
}

pub(super) enum Found<'a> {
    Ready(Response),
    Waiting(Arc<Flight>),
    Fetching(Claim<'a>),
    Unkept,
}

pub(super) struct Claim<'a> {
    memo: &'a Memo,
    key: Key,
    flight: Arc<Flight>,
}

impl Memo {
    pub(crate) fn new(bytes: u64) -> Self {
        Self {
            limit: usize::try_from(bytes.min(64 << 20)).unwrap_or(usize::MAX),
            state: Mutex::default(),
        }
    }

    pub(crate) fn invalidate(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let old = std::mem::take(&mut *state);
        drop(state);
        drop(old);
    }

    pub(super) fn find(&self, key: Key) -> Found<'_> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = state.entries.get(&key) {
            return match entry {
                Entry::Ready { response, .. } => Found::Ready((**response).clone()),
                Entry::Pending { flight, .. } => Found::Waiting(flight.clone()),
            };
        }
        let bytes = key.bytes() + std::mem::size_of::<Entry>() + std::mem::size_of::<Flight>();
        if !state.make_room(bytes, self.limit) {
            return Found::Unkept;
        }
        let flight = Arc::new(Flight::default());
        state.bytes += bytes;
        state.entries.insert(
            key.clone(),
            Entry::Pending {
                flight: flight.clone(),
                bytes,
            },
        );
        Found::Fetching(Claim {
            memo: self,
            key,
            flight,
        })
    }
}

impl State {
    fn make_room(&mut self, bytes: usize, limit: usize) -> bool {
        if bytes > limit {
            return false;
        }
        while self.bytes > limit - bytes || self.entries.len() >= 4096 {
            let Some(oldest) = self.ready.pop_front() else {
                return false;
            };
            if let Some(Entry::Ready { bytes, .. }) = self.entries.remove(&oldest) {
                self.bytes -= bytes;
            }
        }
        true
    }
}

impl Claim<'_> {
    pub(super) fn keep(self, key: Key, response: &Response) {
        if !(200..300).contains(&response.status) {
            return;
        }
        *self
            .flight
            .response
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((key.clone(), response.clone()));
        let bytes = key.bytes() * 2 + std::mem::size_of::<Entry>() + response_bytes(response);
        let mut state = self
            .memo
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !self.remove(&mut state) {
            return;
        }
        if state.entries.contains_key(&key) || !state.make_room(bytes, self.memo.limit) {
            return;
        }
        state.bytes += bytes;
        state.ready.push_back(key.clone());
        state.entries.insert(
            key,
            Entry::Ready {
                response: Box::new(response.clone()),
                bytes,
            },
        );
    }

    fn remove(&self, state: &mut State) -> bool {
        if let Some(Entry::Pending { flight, bytes }) = state.entries.get(&self.key)
            && Arc::ptr_eq(flight, &self.flight)
        {
            state.bytes -= bytes;
            state.entries.remove(&self.key);
            true
        } else {
            false
        }
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let mut state = self
            .memo
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.remove(&mut state);
        drop(state);
        self.flight.finished.store(true, Ordering::Release);
        self.flight.changed.notify_waiters();
    }
}
