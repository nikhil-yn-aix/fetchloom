//! how often a host lets fetchloom ask, from the rate limit headers it sends: zenodo's
//! `X-RateLimit-Remaining` and `X-RateLimit-Reset`, hugging face's `RateLimit` and
//! `RateLimit-Policy`, and the `Retry-After` of a 429. requests to a host stop starting while one
//! of its limits is used up, and start again when it resets.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap};

use super::failure::retry_after;

/// the longest wait for a host's limit to reset. a host that asks for longer is not waited for,
/// so the person can come back later.
pub(super) const LONGEST: Duration = Duration::from_secs(120);
/// how far apart answers place the reset of one window, since a `Date` counts whole seconds.
const SAME_WINDOW: Duration = Duration::from_millis(1500);
/// after a notice, a wait that ends within this long of the one it told of passes without
/// another, such as the short waits zenodo's 429s ask for just after its limit resets.
const QUIET: Duration = Duration::from_secs(10);
/// a reset this large is a time in seconds since 1970, and a smaller one seconds from now.
const EPOCH_FROM: u64 = 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Limit {
    /// the policy's name, empty for a host that states one limit.
    pub(super) name: String,
    /// the requests a window allows.
    pub(super) quota: Option<u64>,
    pub(super) window: Option<Duration>,
    pub(super) remaining: u64,
    /// how long after the answer the limit resets.
    pub(super) reset: Duration,
    /// the reset as the host wrote it, in seconds since 1970, which tells windows apart exactly.
    pub(super) stamp: Option<u64>,
}

/// the limits an answer states. a 429 also says that a limit is used up, for at least as long as
/// its `Retry-After` asks. `now` stands in for the host's clock when the answer has no `Date`.
pub(super) fn limits(status: StatusCode, headers: &HeaderMap, now: SystemTime) -> Vec<Limit> {
    let clock = headers
        .get(header::DATE)
        .and_then(|value| httpdate::parse_http_date(value.to_str().ok()?).ok())
        .unwrap_or(now);
    let mut limits: Vec<Limit> = plain(headers, clock)
        .into_iter()
        .chain(structured(headers))
        .collect();
    if status == StatusCode::TOO_MANY_REQUESTS {
        let asked = retry_after(headers);
        let named = limits.iter().any(|limit| limit.remaining == 0);
        for limit in &mut limits {
            if !named || limit.remaining == 0 {
                limit.remaining = 0;
                limit.reset = limit.reset.max(asked.unwrap_or_default());
            }
        }
        if limits.is_empty()
            && let Some(asked) = asked
        {
            limits.push(Limit {
                name: String::new(),
                quota: None,
                window: None,
                remaining: 0,
                reset: asked,
                stamp: None,
            });
        }
    }
    limits
}

/// how long until every limit an answer says is used up resets, when one is.
pub(super) fn used_up(limits: &[Limit]) -> Option<Duration> {
    limits
        .iter()
        .filter(|limit| limit.remaining == 0)
        .map(|limit| limit.reset)
        .max()
}

/// `X-RateLimit-Limit`, `-Remaining` and `-Reset`, as zenodo and many others send them, or the
/// same without the `X-`, from early drafts of the standard.
fn plain(headers: &HeaderMap, clock: SystemTime) -> Option<Limit> {
    let number = |names: [&str; 2]| {
        names
            .iter()
            .find_map(|name| leading_number(headers.get(*name)?.to_str().ok()?))
    };
    let remaining = number(["x-ratelimit-remaining", "ratelimit-remaining"])?;
    let reset = number(["x-ratelimit-reset", "ratelimit-reset"])?;
    let (reset, stamp) = if reset >= EPOCH_FROM {
        let now = clock
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        (Duration::from_secs(reset.saturating_sub(now)), Some(reset))
    } else {
        (Duration::from_secs(reset), None)
    };
    Some(Limit {
        name: String::new(),
        quota: number(["x-ratelimit-limit", "ratelimit-limit"]),
        window: None,
        remaining,
        reset,
        stamp,
    })
}

/// `RateLimit` and `RateLimit-Policy` from the current drafts of the standard, as hugging face
/// sends them: `"api";r=499;t=9` and `"fixed window";"api";q=500;w=300`.
fn structured(headers: &HeaderMap) -> Vec<Limit> {
    let members = |name: &str| -> Vec<(String, Vec<(String, u64)>)> {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(member)
            .collect()
    };
    let policies = members("ratelimit-policy");
    members("ratelimit")
        .into_iter()
        .filter_map(|(name, params)| {
            let remaining = param(&params, "r")?;
            let reset = param(&params, "t")?;
            let policy = policies
                .iter()
                .find(|(policy, _)| *policy == name)
                .map(|(_, params)| params);
            Some(Limit {
                quota: policy.and_then(|params| param(params, "q")),
                window: policy
                    .and_then(|params| param(params, "w"))
                    .map(Duration::from_secs),
                name,
                remaining,
                reset: Duration::from_secs(reset),
                stamp: None,
            })
        })
        .collect()
}

/// a list member's name, the last quoted string in it, and its numeric parameters.
fn member(text: &str) -> (String, Vec<(String, u64)>) {
    let mut name = String::new();
    let mut params = Vec::new();
    for part in text.split(';').map(str::trim) {
        if let Some(quoted) = part
            .strip_prefix('"')
            .and_then(|part| part.strip_suffix('"'))
        {
            quoted.clone_into(&mut name);
        } else if let Some((key, value)) = part.split_once('=')
            && let Ok(value) = value.trim().parse()
        {
            params.push((key.trim().to_ascii_lowercase(), value));
        }
    }
    (name, params)
}

fn param(params: &[(String, u64)], key: &str) -> Option<u64> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| *value)
}

/// the number a header value starts with, as in `133` or `100;w=60`.
fn leading_number(text: &str) -> Option<u64> {
    let text = text.trim_start();
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

/// what the hosts said about their limits, and the requests on their way to each.
#[derive(Default)]
pub(super) struct Paces {
    hosts: Mutex<HashMap<String, Arc<Mutex<Pace>>>>,
}

#[derive(Debug, Default)]
struct Pace {
    /// by policy name.
    limits: HashMap<String, Known>,
    /// requests sent and not yet answered, which the counts the host sent last may not include.
    pending: u64,
    /// the reset the last notice was about, so the requests waiting for one reset tell of it
    /// once.
    announced: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct Known {
    quota: Option<u64>,
    window: Option<Duration>,
    remaining: u64,
    reset: Instant,
    stamp: Option<u64>,
}

/// a wait for a host's limit to reset.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Hold {
    pub(super) until: Instant,
    pub(super) quota: Option<u64>,
    pub(super) window: Option<Duration>,
    /// this is the first request to wait for the reset, which tells the person watching.
    pub(super) announce: bool,
}

/// a request on its way to a host, counted until its answer comes or it fails.
#[derive(Debug)]
pub(super) struct Ticket {
    pace: Option<Arc<Mutex<Pace>>>,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(pace) = self.pace.take() {
            let mut pace = pace.lock().unwrap_or_else(PoisonError::into_inner);
            pace.pending = pace.pending.saturating_sub(1);
        }
    }
}

impl Paces {
    /// a ticket for a request to `host` when its limits allow one at `now`, else the wait. a limit
    /// used up for longer than [`LONGEST`] is left to the host to enforce.
    pub(super) fn admit(&self, host: &str, now: Instant) -> Result<Ticket, Hold> {
        let shared = {
            let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
            hosts.entry(host.to_owned()).or_default().clone()
        };
        let mut pace = shared.lock().unwrap_or_else(PoisonError::into_inner);
        pace.limits.retain(|_, known| known.reset > now);
        let pending = pace.pending;
        let blocking = pace
            .limits
            .values()
            .filter(|known| known.remaining <= pending && known.reset - now <= LONGEST)
            .max_by_key(|known| known.reset)
            .copied();
        let Some(known) = blocking else {
            pace.pending += 1;
            drop(pace);
            return Ok(Ticket { pace: Some(shared) });
        };
        let announce = pace
            .announced
            .is_none_or(|announced| known.reset > announced + QUIET);
        if announce {
            pace.announced = Some(known.reset);
        }
        Err(Hold {
            until: known.reset,
            quota: known.quota,
            window: known.window,
            announce,
        })
    }

    /// records the limits the answer to the ticket's request stated, at `at`.
    pub(super) fn heard(mut ticket: Ticket, limits: Vec<Limit>, at: Instant) {
        let Some(shared) = ticket.pace.take() else {
            return;
        };
        let mut pace = shared.lock().unwrap_or_else(PoisonError::into_inner);
        pace.pending = pace.pending.saturating_sub(1);
        for limit in limits {
            pace.learn(limit, at);
        }
    }
}

impl Pace {
    /// takes a limit an answer stated: a new window replaces the old, answers from one window
    /// keep the lowest count, and an answer from an earlier window changes nothing.
    fn learn(&mut self, limit: Limit, at: Instant) {
        let fresh = Known {
            quota: limit.quota,
            window: limit.window,
            remaining: limit.remaining,
            reset: at + limit.reset,
            stamp: limit.stamp,
        };
        let Some(known) = self.limits.get_mut(&limit.name) else {
            self.limits.insert(limit.name, fresh);
            return;
        };
        let order = match (known.stamp, fresh.stamp) {
            (Some(old), Some(new)) => new.cmp(&old),
            _ if fresh.reset > known.reset + SAME_WINDOW => Ordering::Greater,
            _ if fresh.reset + SAME_WINDOW < known.reset => Ordering::Less,
            _ => Ordering::Equal,
        };
        match order {
            Ordering::Greater => *known = fresh,
            Ordering::Equal => {
                known.remaining = known.remaining.min(fresh.remaining);
                known.reset = known.reset.max(fresh.reset);
                known.quota = fresh.quota.or(known.quota);
                known.window = fresh.window.or(known.window);
            }
            Ordering::Less => {}
        }
    }
}

#[cfg(test)]
mod tests;
