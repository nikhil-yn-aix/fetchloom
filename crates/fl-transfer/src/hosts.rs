//! Connections per host: one count shared by the scheduler, which starts downloads, and by
//! split downloads, which add range requests only where a host has room.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Notify;

/// How many connections each host has, against one limit for all.
#[derive(Debug)]
pub struct Hosts {
    per_host: usize,
    running: Mutex<HashMap<String, usize>>,
    freed: Notify,
}

/// A connection to one host, counted until it is dropped.
#[derive(Debug)]
pub struct Slot {
    hosts: Arc<Hosts>,
    host: String,
}

impl Hosts {
    /// At most `per_host` connections to one host, and at least one.
    #[must_use]
    pub fn new(per_host: usize) -> Arc<Self> {
        Arc::new(Self {
            per_host: per_host.max(1),
            running: Mutex::default(),
            freed: Notify::new(),
        })
    }

    /// A slot on `host` when it has room. Hosts compare without case.
    #[must_use]
    pub fn try_take(self: &Arc<Self>, host: &str) -> Option<Slot> {
        let host = host.to_ascii_lowercase();
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let count = running.entry(host.clone()).or_default();
        if *count >= self.per_host {
            return None;
        }
        *count += 1;
        Some(Slot {
            hosts: Arc::clone(self),
            host,
        })
    }

    /// Waits until some slot is given back. Create the wait before looking for room, so a slot
    /// freed in between is not missed.
    pub(crate) fn freed(&self) -> tokio::sync::futures::Notified<'_> {
        self.freed.notified()
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut running = self
            .hosts
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = running.get_mut(&self.host) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                running.remove(&self.host);
            }
        }
        drop(running);
        self.hosts.freed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_holds_at_most_its_limit_until_a_slot_goes() {
        let hosts = Hosts::new(2);
        let first = hosts.try_take("a.org").unwrap();
        let _second = hosts.try_take("A.org").unwrap();
        assert!(hosts.try_take("a.org").is_none());
        assert!(hosts.try_take("b.org").is_some());
        drop(first);
        assert!(hosts.try_take("a.org").is_some());
        assert!(Hosts::new(0).try_take("c.org").is_some());
    }
}
