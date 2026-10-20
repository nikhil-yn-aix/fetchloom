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
    answered: Mutex<HashMap<String, IpAddr>>,
}

impl Resolver {
    /// A resolver that refuses every name when `offline`, and trusts `trusted` hosts, such as
    /// configured proxies, as if they were typed.
    pub(crate) fn new(offline: bool, trusted: impl IntoIterator<Item = String>) -> Self {
        Self {
            offline,
            typed: Mutex::new(trusted.into_iter().map(|host| lower(&host)).collect()),
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

    /// Whether the caller asked for `host`.
    pub(crate) fn is_typed(&self, host: &str) -> bool {
        self.typed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&lower(host))
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
