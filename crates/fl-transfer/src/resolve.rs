//! The DNS hook every connection goes through: offline refusal, the address policy for names
//! nobody typed, and the address that answered last tried first.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

use crate::address::is_public;

/// Why a name was not resolved. It travels inside reqwest's error, so the client can tell it
/// from a failed lookup.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum Refusal {
    /// Offline, so no name is looked up.
    #[error("offline, so `{0}` is not looked up")]
    Offline(String),
    /// Every address of a name nobody typed is private or reserved.
    #[error("`{0}` resolves only to private or reserved addresses")]
    Private(String),
}

/// Resolves names for the client.
#[derive(Debug, Default)]
pub(crate) struct Resolver {
    offline: bool,
    typed: Mutex<HashSet<String>>,
    pinned: Mutex<HashMap<String, HashSet<u16>>>,
    answered: Mutex<HashMap<String, IpAddr>>,
}

impl Resolver {
    /// A resolver that refuses every name when `offline`, and trusts `trusted` hosts, such as
    /// configured proxies, as if they were typed.
    pub(crate) fn new(offline: bool, trusted: impl IntoIterator<Item = String>) -> Self {
        Self {
            offline,
            typed: Mutex::new(trusted.into_iter().map(|host| lower(&host)).collect()),
            pinned: Mutex::default(),
            answered: Mutex::default(),
        }
    }

    /// Records a host the caller asked for, whose private addresses are allowed.
    pub(crate) fn typed(&self, host: &str) {
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(lower(host));
    }

    /// Records a host and port someone configured, whose private addresses are allowed on
    /// that port only.
    pub(crate) fn pin(&self, host: &str, port: u16) {
        self.pinned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(lower(host))
            .or_default()
            .insert(port);
    }

    /// Whether the caller asked for `host`, or configured it on some port.
    pub(crate) fn is_typed(&self, host: &str) -> bool {
        let host = lower(host);
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&host)
            || self
                .pinned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .contains_key(&host)
    }

    /// Whether private addresses of `host` may be reached on `port`: the caller asked for the
    /// host, or configured it on that port.
    pub(crate) fn allows(&self, host: &str, port: u16) -> bool {
        let host = lower(host);
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&host)
            || self
                .pinned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&host)
                .is_some_and(|ports| ports.contains(&port))
    }

    /// Whether `host` was configured on other ports only, and the address it last answered
    /// from is not known to be public, so a request to `port` must not go out.
    pub(crate) fn pinned_elsewhere(&self, host: &str, port: u16) -> bool {
        let host = lower(host);
        let pinned = self
            .pinned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&host)
            .is_some_and(|ports| !ports.contains(&port));
        pinned
            && !self.allows(&host, port)
            && !self
                .answered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&host)
                .is_some_and(|ip| is_public(*ip))
    }

    /// Records that `address` answered for `host`, so later connections try it first.
    pub(crate) fn answered(&self, host: &str, address: IpAddr) {
        self.answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(lower(host), address);
    }

    /// Applies the policy to the addresses `host` resolved to.
    fn choose(&self, host: &str, found: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, Refusal> {
        let mut chosen = if self.is_typed(host) {
            found
        } else {
            let public: Vec<SocketAddr> = found
                .into_iter()
                .filter(|address| is_public(address.ip()))
                .collect();
            if public.is_empty() {
                return Err(Refusal::Private(host.to_owned()));
            }
            public
        };
        let last = self
            .answered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&lower(host))
            .copied();
        if let Some(index) = last.and_then(|ip| chosen.iter().position(|a| a.ip() == ip)) {
            chosen[..=index].rotate_right(1);
        }
        Ok(chosen)
    }
}

/// The resolver as reqwest holds it.
#[derive(Debug, Clone)]
pub(crate) struct Shared(pub(crate) Arc<Resolver>);

impl Resolve for Shared {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = Arc::clone(&self.0);
        let host = name.as_str().to_owned();
        Box::pin(async move {
            if resolver.offline {
                return Err(Refusal::Offline(host).into());
            }
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let chosen = resolver.choose(&host, found)?;
            let addrs: Addrs = Box::new(chosen.into_iter());
            Ok(addrs)
        })
    }
}

fn lower(host: &str) -> String {
    host.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addresses(texts: &[&str]) -> Vec<SocketAddr> {
        texts
            .iter()
            .map(|text| SocketAddr::new(text.parse().unwrap(), 443))
            .collect()
    }

    #[test]
    fn names_nobody_typed_lose_private_addresses_and_typed_ones_keep_them() {
        let resolver = Resolver::new(false, ["Proxy.Lab".to_owned()]);
        let found = addresses(&["10.0.0.1", "93.184.215.14"]);
        assert_eq!(
            resolver.choose("cdn.org", found.clone()),
            Ok(addresses(&["93.184.215.14"]))
        );
        assert_eq!(
            resolver.choose("inside.lab", addresses(&["10.0.0.1", "127.0.0.1"])),
            Err(Refusal::Private("inside.lab".to_owned()))
        );
        resolver.typed("Inside.Lab");
        assert_eq!(
            resolver.choose("inside.lab", found.clone()),
            Ok(found.clone())
        );
        assert_eq!(resolver.choose("proxy.lab", found.clone()), Ok(found));
    }

    #[test]
    fn the_address_that_answered_last_is_tried_first() {
        let resolver = Resolver::new(false, []);
        let found = addresses(&["1.1.1.1", "1.0.0.1", "8.8.8.8"]);
        resolver.answered("dns.org", "8.8.8.8".parse().unwrap());
        assert_eq!(
            resolver.choose("DNS.org", found),
            Ok(addresses(&["8.8.8.8", "1.1.1.1", "1.0.0.1"]))
        );
    }

    #[tokio::test]
    async fn offline_refuses_every_name() {
        let shared = Shared(Arc::new(Resolver::new(true, [])));
        let name: Name = "zenodo.org".parse().unwrap();
        let error = shared.resolve(name).await.err().unwrap();
        assert_eq!(
            error.to_string(),
            "offline, so `zenodo.org` is not looked up"
        );
    }
}
