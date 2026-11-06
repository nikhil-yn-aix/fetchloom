//! where a request may go. a host a person wrote, on the command line or in data.toml, may be
//! any address, a lab server included. a URL nobody wrote, from a source's listing, a redirect
//! or data.lock alone, may not reach private, loopback or link-local addresses, so a hostile
//! server or a tampered lock cannot reach into a local network.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use hyper_util::client::proxy::matcher::Matcher;
use url::{Host, Url};

use super::Shown;

/// the hosts a person wrote, and the proxies this machine sends requests through.
#[derive(Clone, Default)]
pub(crate) struct Typed(Arc<Mutex<Hosts>>);

#[derive(Default)]
struct Hosts {
    written: HashSet<String>,
    /// a proxy may sit on a private address, and only reaching it is allowed: a URL that asks
    /// for the proxy's host itself, as a redirect could, is refused unless a person wrote it.
    proxies: HashSet<String>,
}

impl Typed {
    #[cfg(debug_assertions)]
    pub(crate) fn add(&self, url: &Url) {
        if let Some(host) = url.host_str() {
            self.add_host(host);
        }
    }

    /// trusts a host a person wrote down, such as a dataverse install on a university network.
    pub(crate) fn add_host(&self, host: &str) {
        self.hosts().written.insert(key(host));
    }

    /// lets the proxy at `host` be reached, wherever its address is.
    pub(crate) fn add_proxy(&self, host: &str) {
        self.hosts().proxies.insert(key(host));
    }

    fn contains(&self, host: &str) -> bool {
        self.hosts().written.contains(&key(host))
    }

    /// whether `host` may resolve to a private address: a person wrote it, or it is a proxy.
    pub(crate) fn may_resolve_privately(&self, host: &str) -> bool {
        let key = key(host);
        let hosts = self.hosts();
        hosts.written.contains(&key) || hosts.proxies.contains(&key)
    }

    fn hosts(&self) -> std::sync::MutexGuard<'_, Hosts> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// refuses a URL whose host is a private address written out, or a proxy's host, unless the
    /// user typed it. names are checked later, on the addresses they resolve to.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        let Some(host) = url.host_str() else {
            return Err(Refused(format!("{} has no host", Shown(url))));
        };
        let hosts = self.hosts();
        let host = key(host);
        if hosts.written.contains(&host) {
            return Ok(());
        }
        if hosts.proxies.contains(&host) {
            return Err(Refused(format!(
                "{} asks for the proxy this machine sends requests through",
                Shown(url)
            )));
        }
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            _ => return Ok(()),
        };
        if is_private(ip) && !hosts.written.contains(&ip.to_string()) {
            return Err(Refused(format!(
                "{} points at the private address {ip}",
                Shown(url)
            )));
        }
        Ok(())
    }
}

/// a host as the sets keep it: lowercase, an IPv6 address without its brackets.
fn key(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase()
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

/// a host name no DNS server knows, which trying again will not change.
#[derive(Debug)]
pub(crate) struct Unknown(pub(crate) String);

impl fmt::Display for Unknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not a known host", self.0)
    }
}

impl std::error::Error for Unknown {}

/// whether a failed lookup said the name does not exist, rather than that DNS could not answer
/// for now. getaddrinfo reports that only through its message on unix: glibc, musl and macos
/// each word it their own way.
fn is_unknown(err: &std::io::Error) -> bool {
    if cfg!(windows) {
        // WSAHOST_NOT_FOUND and WSANO_DATA
        return matches!(err.raw_os_error(), Some(11001 | 11004));
    }
    let text = err.to_string();
    [
        "Name or service not known",
        "No address associated with hostname",
        "Name does not resolve",
        "Name has no usable address",
        "nodename nor servname provided, or not known",
    ]
    .iter()
    .any(|said| text.contains(said))
}

/// the proxies this machine is set to use, chosen for each URL the way reqwest chooses them, and
/// what looking up the hosts sent through them found.
pub(crate) struct Proxies {
    matcher: Matcher,
    /// for each host looked up, why it was refused, if it was.
    looked_up: Mutex<HashMap<String, Option<String>>>,
}

impl Proxies {
    /// the proxies in the environment and, where reqwest reads them, the system settings.
    pub(crate) fn from_system() -> Proxies {
        Proxies {
            matcher: Matcher::from_system(),
            looked_up: Mutex::default(),
        }
    }

    /// checks `url` before it is sent, when a proxy will carry it. the proxy resolves the name
    /// itself, so fetchloom's resolver never sees the addresses, and a host nobody wrote is looked
    /// up here first and refused when any of its addresses is private. a name this machine cannot
    /// resolve is left to the proxy, since some networks resolve names only through it. the
    /// proxy itself may be reached wherever it is, since a person set it up.
    pub(crate) async fn check(&self, url: &Url, typed: &Typed) -> Result<(), Refused> {
        let Some(proxy) = url
            .as_str()
            .parse::<http::Uri>()
            .ok()
            .and_then(|uri| self.matcher.intercept(&uri))
        else {
            return Ok(());
        };
        if let Some(host) = proxy.uri().host() {
            typed.add_proxy(host);
        }
        let Some(Host::Domain(name)) = url.host() else {
            return Ok(());
        };
        let name = name.to_ascii_lowercase();
        if typed.contains(&name) {
            return Ok(());
        }
        let known = self.lock().get(&name).cloned();
        let refused = if let Some(refused) = known {
            refused
        } else {
            let port = url.port_or_known_default().unwrap_or(443);
            let refused = private_address(&name, port).await;
            self.lock().insert(name, refused.clone());
            refused
        };
        refused.map_or(Ok(()), |reason| Err(Refused(reason)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Option<String>>> {
        self.looked_up
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// why `name` may not go through a proxy, when this machine resolves it to a private address.
async fn private_address(name: &str, port: u16) -> Option<String> {
    let mut found = tokio::net::lookup_host((name, port)).await.ok()?;
    let address = found.find(|address| is_private(address.ip()))?;
    Some(format!(
        "{name} resolves to the private address {}, and a proxy would reach it",
        address.ip()
    ))
}

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
        let allowed = self.typed.may_resolve_privately(&host);
        let offline = self.offline;
        Box::pin(async move {
            if offline {
                return Err(Box::new(Offline) as Box<dyn std::error::Error + Send + Sync>);
            }
            let found: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), 0)).await {
                Ok(found) => found.collect(),
                Err(err) if is_unknown(&err) => {
                    return Err(
                        Box::new(Unknown(host.clone())) as Box<dyn std::error::Error + Send + Sync>
                    );
                }
                Err(err) => return Err(Box::new(err)),
            };
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
                    .or_else(|| nat64(ip))
                    .is_some_and(|mapped| is_private(IpAddr::V4(mapped)))
        }
    }
}

/// the IPv4 address a NAT64 translator reaches for `ip`: one in the well-known prefix
/// `64:ff9b::/96`, or in the local-use `64:ff9b:1::/48` read as a /96, as translators set it up.
fn nat64(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    let well_known = segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    let local = segments[..3] == [0x64, 0xff9b, 1];
    let [.., a, b, c, d] = ip.octets();
    (well_known || local).then(|| Ipv4Addr::new(a, b, c, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn a_name_nobody_knows_is_told_apart_from_a_dns_hiccup() {
        if cfg!(windows) {
            assert!(is_unknown(&std::io::Error::from_raw_os_error(11001)));
            assert!(!is_unknown(&std::io::Error::from_raw_os_error(11002)));
        } else {
            let said = |text: &str| {
                std::io::Error::other(format!("failed to lookup address information: {text}"))
            };
            assert!(is_unknown(&said("Name or service not known")));
            assert!(is_unknown(&said("Name does not resolve")));
            assert!(is_unknown(&said(
                "nodename nor servname provided, or not known"
            )));
            assert!(!is_unknown(&said("Temporary failure in name resolution")));
            assert!(!is_unknown(&said("Try again")));
        }
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
    fn a_nat64_address_is_judged_by_the_ipv4_address_it_reaches() {
        for text in [
            "64:ff9b::169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::10.0.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b:1::192.168.1.1",
        ] {
            assert!(is_private(ip(text)), "{text} is private");
        }
        for text in ["64:ff9b::8.8.8.8", "64:ff9b:1::188.184.98.157"] {
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

    #[test]
    fn a_proxy_may_be_reached_but_never_requested() {
        let typed = Typed::default();
        typed.add_proxy("127.0.0.1");
        typed.add_proxy("proxy.lab");
        for url in ["http://127.0.0.1:9000/admin", "http://proxy.lab:9000/admin"] {
            let url = Url::parse(url).unwrap();
            assert!(typed.check(&url).is_err(), "{url}");
        }
        assert!(typed.may_resolve_privately("proxy.lab"));
        assert!(!typed.may_resolve_privately("elsewhere.lab"));
        typed.add_host("proxy.lab");
        assert!(
            typed
                .check(&Url::parse("http://proxy.lab:9000/x").unwrap())
                .is_ok(),
            "unless a person wrote it"
        );
    }
}
