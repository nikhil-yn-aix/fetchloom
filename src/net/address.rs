//! where a request may go. a URL the user typed may reach any address, a lab server included.
//! a URL that came from a file or a server may not reach private, loopback or link-local
//! addresses, so a hostile `data.lock` cannot reach into a local network.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use url::{Host, Url};

/// the hosts the user typed on the command line.
#[derive(Clone, Default)]
pub(crate) struct Typed(Arc<Mutex<HashSet<String>>>);

impl Typed {
    pub(crate) fn add(&self, url: &Url) {
        if let Some(host) = url.host_str() {
            self.hosts().insert(host.to_ascii_lowercase());
        }
    }

    fn contains(&self, host: &str) -> bool {
        self.hosts().contains(&host.to_ascii_lowercase())
    }

    fn hosts(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// refuses a URL whose host is a private address written out, unless the user typed it.
    /// names are checked later, on the addresses they resolve to.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            Some(Host::Domain(_)) => return Ok(()),
            None => return Err(Refused(format!("{url} has no host"))),
        };
        if is_private(ip)
            && !self.contains(&ip.to_string())
            && !url.host_str().is_some_and(|host| self.contains(host))
        {
            return Err(Refused(format!("{url} points at the private address {ip}")));
        }
        Ok(())
    }
}

/// a request fetchloom will not make, with the reason in one sentence.
#[derive(Debug)]
pub(crate) struct Refused(pub(crate) String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// a request made while offline. it never leaves the machine.
#[derive(Debug)]
pub(crate) struct Offline;

impl fmt::Display for Offline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("offline")
    }
}

impl std::error::Error for Offline {}

/// resolves names for the HTTP client, dropping private addresses for hosts nobody typed.
pub(crate) struct Resolver {
    typed: Typed,
    offline: bool,
}

impl Resolver {
    pub(crate) fn new(typed: Typed, offline: bool) -> Resolver {
        Resolver { typed, offline }
    }
}

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let allowed = self.typed.contains(&host);
        let offline = self.offline;
        Box::pin(async move {
            if offline {
                return Err(Box::new(Offline) as Box<dyn std::error::Error + Send + Sync>);
            }
            let found: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let kept: Vec<SocketAddr> = found
                .iter()
                .copied()
                .filter(|address| allowed || !is_private(address.ip()))
                .collect();
            if kept.is_empty() && !found.is_empty() {
                let refused = Refused(format!("{host} resolves only to private addresses"));
                return Err(Box::new(refused) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// addresses that belong to this machine or its local network rather than the internet.
pub(crate) fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            let shared = first == 100 && (second & 0xc0) == 64;
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_documentation()
                || shared
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_private(IpAddr::V4(mapped)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn private_addresses() {
        for text in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_private(ip(text)), "{text} is private");
        }
        for text in [
            "8.8.8.8",
            "188.184.98.157",
            "2001:4860:4860::8888",
            "100.128.0.1",
        ] {
            assert!(!is_private(ip(text)), "{text} is public");
        }
    }

    #[test]
    fn a_private_literal_needs_to_have_been_typed() {
        let typed = Typed::default();
        let metadata = Url::parse("http://169.254.169.254/latest").unwrap();
        assert!(typed.check(&metadata).is_err());
        typed.add(&metadata);
        assert!(typed.check(&metadata).is_ok());
        assert!(
            typed
                .check(&Url::parse("https://zenodo.org/x").unwrap())
                .is_ok()
        );
    }
}
