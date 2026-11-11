//! where a request may go. a host a person typed on the command line may be any address, a lab
//! server included, and so may an origin the `allowed_origins` setting lists. a URL nobody typed,
//! from data.toml, data.lock, a source's listing or a redirect, may not reach private, loopback
//! or link-local addresses, so a cloned project, a hostile server or a tampered lock cannot reach
//! into a local network.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use hyper_util::client::proxy::matcher::Matcher;
use url::{Host, Url};

use super::Shown;
use crate::error::Error;

/// the hosts a person typed, the origins a person allowed, and the proxies this machine sends
/// requests through.
#[derive(Clone, Default)]
pub(crate) struct Typed(Arc<Mutex<Hosts>>);

#[derive(Default)]
struct Hosts {
    written: HashSet<String>,
    /// exact origins: a request to another port or scheme of the same host is not allowed.
    origins: HashSet<url::Origin>,
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

    /// lets requests reach `origins`, wherever they are, as the `allowed_origins` setting lists
    /// them.
    pub(crate) fn allow(&self, origins: impl IntoIterator<Item = url::Origin>) {
        self.hosts().origins.extend(origins);
    }

    /// whether a person typed the host of `url` or allowed its origin.
    fn trusts(&self, url: &Url) -> bool {
        let hosts = self.hosts();
        url.host_str()
            .is_some_and(|host| hosts.written.contains(&key(host)))
            || hosts.origins.contains(&url.origin())
    }

    /// whether `host` may resolve to a private address: a person wrote it, an allowed origin is
    /// on it, or it is a proxy.
    pub(crate) fn may_resolve_privately(&self, host: &str) -> bool {
        let key = key(host);
        let hosts = self.hosts();
        hosts.written.contains(&key) || hosts.proxies.contains(&key) || hosts.allows_on(&key)
    }

    fn hosts(&self) -> std::sync::MutexGuard<'_, Hosts> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// refuses a URL whose host is a private address written out, or a proxy's host, unless the
    /// user typed it or allowed its origin. names are checked later, on the addresses they
    /// resolve to, except a name an allowed origin is on: it may resolve privately, so its other
    /// origins are refused here.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        let Some(host) = url.host_str() else {
            return Err(Refused(format!("{} has no host", Shown(url))));
        };
        let hosts = self.hosts();
        let host = key(host);
        if hosts.written.contains(&host) || hosts.origins.contains(&url.origin()) {
            return Ok(());
        }
        if hosts.proxies.contains(&host) {
            return Err(Refused(format!(
                "{} asks for the proxy this machine sends requests through",
                Shown(url)
            )));
        }
        if matches!(url.host(), Some(Host::Domain(_))) && hosts.allows_on(&host) {
            return Err(Refused(format!(
                "allowed_origins lists another origin on {host}, not {}",
                url.origin().ascii_serialization()
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

impl super::Net {
    /// lets requests reach the origins the `allowed_origins` setting lists, wherever they are.
    pub(crate) fn with_allowed_origins(self, origins: &[url::Origin]) -> super::Net {
        self.typed.allow(origins.iter().cloned());
        self
    }
}

impl Hosts {
    /// whether an allowed origin is on `host`, given as [`key`] makes it.
    fn allows_on(&self, host: &str) -> bool {
        self.origins.iter().any(|origin| match origin {
            url::Origin::Tuple(_, on, _) => key(&on.to_string()) == host,
            url::Origin::Opaque(_) => false,
        })
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
    /// each variable that names a proxy, with the proxy it names, as the matcher reads them.
    named: Vec<(&'static str, http::Uri)>,
    /// for each host looked up, why it was refused, if it was.
    looked_up: Mutex<HashMap<String, Option<String>>>,
    /// what this run decided about each proxy it checked, by host and port.
    checked: Mutex<HashMap<(String, u16), reach::Verdict>>,
    #[cfg(test)]
    test_lookup: Option<TestLookup>,
    #[cfg(test)]
    test_probe: Option<TestProbe>,
}

/// the proxy variables, each pair in the order the proxy matcher reads it: the lowercase name
/// counts only when the uppercase one is unset.
const VARIABLES: [[&str; 2]; 3] = [
    ["HTTPS_PROXY", "https_proxy"],
    ["HTTP_PROXY", "http_proxy"],
    ["ALL_PROXY", "all_proxy"],
];

/// the proxy each set variable names. a value the matcher cannot read, which it would pass over
/// as if the variable were unset, is refused, since a person who set it relies on the proxy and
/// requests would go around it. an empty value names no proxy.
///
/// # Errors
///
/// `usage`, naming the first variable whose value is no proxy address.
fn named(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Vec<(&'static str, http::Uri)>, Error> {
    let mut named = Vec::new();
    for pair in VARIABLES {
        let Some((name, value)) = pair
            .into_iter()
            .find_map(|name| var(name).map(|value| (name, value)))
        else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        match value.to_str().and_then(proxy_address) {
            Some(uri) => named.push((name, uri)),
            None => return Err(not_proxy(name, &value.to_string_lossy())),
        }
    }
    Ok(named)
}

/// the proxy `value` names, as the matcher reads a proxy variable: an http, https or socks
/// address, or a host and port alone. none when the matcher cannot read it.
fn proxy_address(value: &str) -> Option<http::Uri> {
    let matcher = Matcher::builder().all(value.to_owned()).build();
    let intercept = matcher.intercept(&http::Uri::from_static("http://example.org/"))?;
    Some(intercept.uri().clone())
}

/// the refusal of `value`, the setting of `name` that is no proxy address.
fn not_proxy(name: &str, value: &str) -> Error {
    Error::new(
        crate::error::Kind::Usage,
        format!(
            "{name} is \"{}\", which is not a proxy address",
            without_login(value).escape_debug()
        ),
    )
    .help(format!(
        "set {name} to a proxy, such as http://proxy.example.org:8080, or unset {name} to connect without one. `fetchloom help environment` lists the proxy settings"
    ))
}

/// a proxy setting as messages show it: whatever comes before an `@` is a login, which `***`
/// stands for, whether or not the rest is an address.
fn without_login(value: &str) -> Cow<'_, str> {
    let Some(at) = value.rfind('@') else {
        return Cow::Borrowed(value);
    };
    let start = value.find("://").map_or(0, |scheme| scheme + 3);
    if start > at {
        return Cow::Borrowed(value);
    }
    Cow::Owned(format!("{}***{}", &value[..start], &value[at..]))
}

#[cfg(test)]
pub(super) type LookupFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>;
#[cfg(test)]
type TestLookup = Arc<dyn Fn(String, u16) -> LookupFuture + Send + Sync>;
#[cfg(test)]
type ProbeFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Probe> + Send>>;
#[cfg(test)]
type TestProbe = Arc<dyn Fn(String, u16) -> ProbeFuture + Send + Sync>;

/// the proxy this machine sends `url` through, as its scheme, host and port, without any login
/// its address holds. none when the request goes straight to the host.
pub(crate) fn proxy_of(url: &Url) -> Option<String> {
    shown_proxy(&Matcher::from_system(), url)
}

/// the proxy `matcher` picks for `url`, as [`proxy_of`] shows it.
fn shown_proxy(matcher: &Matcher, url: &Url) -> Option<String> {
    let uri = url.as_str().parse::<http::Uri>().ok()?;
    shown(matcher.intercept(&uri)?.uri())
}

/// a proxy's address as messages show it: its scheme, host and port, without a login.
fn shown(proxy: &http::Uri) -> Option<String> {
    let scheme = proxy.scheme_str().unwrap_or("http");
    let host = proxy.host()?;
    Some(match proxy.port_u16() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    })
}

#[cfg(test)]
mod proxy_tests;
mod reach;

pub(crate) use reach::Carrier;
#[cfg(test)]
use reach::Probe;

impl Proxies {
    /// the proxies in the environment `var` reads and, where reqwest reads them, the system
    /// settings.
    ///
    /// # Errors
    ///
    /// `usage` for a proxy variable whose value is no proxy address.
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Proxies, Error> {
        Ok(Proxies {
            matcher: Matcher::from_system(),
            named: named(var)?,
            looked_up: Mutex::default(),
            checked: Mutex::default(),
            #[cfg(test)]
            test_lookup: None,
            #[cfg(test)]
            test_probe: None,
        })
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
        if typed.trusts(url) {
            return Ok(());
        }
        let known = self.lock().get(&name).cloned();
        let refused = if let Some(refused) = known {
            refused
        } else {
            let port = url.port_or_known_default().unwrap_or(443);
            let refused = self.lookup(&name, port).await;
            self.lock().insert(name, refused.clone());
            refused
        };
        refused.map_or(Ok(()), |reason| Err(Refused(reason)))
    }

    #[cfg(test)]
    pub(super) fn with_lookup(
        proxy: &str,
        lookup: impl Fn(String, u16) -> LookupFuture + Send + Sync + 'static,
    ) -> Proxies {
        Proxies {
            matcher: Matcher::builder().all(proxy.to_owned()).build(),
            named: Vec::new(),
            looked_up: Mutex::default(),
            checked: Mutex::default(),
            test_lookup: Some(Arc::new(lookup)),
            test_probe: None,
        }
    }

    /// the proxy at `proxy`, named by `variable`, whose checks `probe` answers.
    #[cfg(test)]
    fn with_probe(
        variable: &'static str,
        proxy: &str,
        probe: impl Fn(String, u16) -> ProbeFuture + Send + Sync + 'static,
    ) -> Proxies {
        Proxies {
            matcher: Matcher::builder().all(proxy.to_owned()).build(),
            named: proxy_address(proxy)
                .map(|uri| vec![(variable, uri)])
                .unwrap_or_default(),
            looked_up: Mutex::default(),
            checked: Mutex::default(),
            test_lookup: None,
            test_probe: Some(Arc::new(probe)),
        }
    }

    async fn lookup(&self, name: &str, port: u16) -> Option<String> {
        #[cfg(test)]
        if let Some(lookup) = &self.test_lookup {
            return lookup(name.to_owned(), port).await;
        }
        private_address(name, port).await
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

/// addresses that belong to this machine or its local network rather than the internet. the
/// fake addresses clash and mihomo hand out from 198.18.0.0/15 stay public: those tools answer
/// every name with one, so refusing them would refuse every download.
pub(crate) fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [first, second, third, _] = ip.octets();
            let shared = first == 100 && (second & 0xc0) == 64;
            // 192.0.0.0/24 holds oracle cloud's instance metadata at 192.0.0.192
            let protocol = first == 192 && second == 0 && third == 0;
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_documentation()
                || shared
                || protocol
                || first == 0
                || first >= 240
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || documentation(ip)
                || site_local(ip)
                || ip
                    .to_ipv4()
                    .or_else(|| nat64(ip))
                    .or_else(|| six_to_four(ip))
                    .is_some_and(|mapped| is_private(IpAddr::V4(mapped)))
                || local_nat64(ip)
        }
    }
}

// IPv6 documentation classification is still unstable in std.
fn documentation(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    (first == 0x2001 && second == 0x0db8) || (first == 0x3fff && second & 0xf000 == 0)
}

/// whether `ip` sits in `fec0::/10`, the site-local addresses IPv6 gave up but some networks
/// still route.
fn site_local(ip: Ipv6Addr) -> bool {
    ip.segments()[0] & 0xffc0 == 0xfec0
}

/// the IPv4 address a 6to4 address in `2002::/16` carries in its next 32 bits.
fn six_to_four(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let [_, _, a, b, c, d, ..] = ip.octets();
    (ip.segments()[0] == 0x2002).then(|| Ipv4Addr::new(a, b, c, d))
}

/// the IPv4 address a NAT64 translator on the well-known prefix `64:ff9b::/96` reaches for `ip`.
fn nat64(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let [.., a, b, c, d] = ip.octets();
    (ip.segments()[..6] == [0x64, 0xff9b, 0, 0, 0, 0]).then(|| Ipv4Addr::new(a, b, c, d))
}

/// whether `ip` sits in the local-use NAT64 prefix `64:ff9b:1::/48`. a network picks its own prefix
/// length inside it, and with it where the IPv4 address sits, so no address there can be read
/// for sure and every one is treated as private.
fn local_nat64(ip: Ipv6Addr) -> bool {
    ip.segments()[..3] == [0x64, 0xff9b, 1]
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
            // oracle cloud's instance metadata lives at 192.0.0.192
            "192.0.0.192",
            "0.1.2.3",
            "240.0.0.1",
            "255.255.255.254",
            "::169.254.169.254",
            "::10.0.0.1",
            "2002:a9fe:a9fe::1",
            "2002:c000:c0::1",
            "fec0::1",
            "feff::1",
        ] {
            assert!(is_private(ip(text)), "{text} is private");
        }
        for text in [
            "8.8.8.8",
            "188.184.98.157",
            "2001:4860:4860::8888",
            "100.128.0.1",
            "192.0.1.1",
            // clash and mihomo answer every name with a fake address from 198.18.0.0/15
            "198.18.0.1",
            "2002:808:808::1",
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
            // a local-use prefix can place the IPv4 address elsewhere, as at 169.254.169.254 here
            "64:ff9b:1:a9fe:a9:fe00:808:808",
            "64:ff9b:1::188.184.98.157",
        ] {
            assert!(is_private(ip(text)), "{text} is private");
        }
        for text in ["64:ff9b::8.8.8.8", "64:ff9b::188.184.98.157"] {
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

    #[test]
    fn an_allowed_origin_reaches_its_server_and_nothing_else_there() {
        let typed = Typed::default();
        typed.allow(
            ["http://lab.local:8080", "http://127.0.0.1:41234"]
                .map(|origin| Url::parse(origin).unwrap().origin()),
        );
        for (url, allowed) in [
            ("http://lab.local:8080/shards/a.zip", true),
            ("http://LAB.local:8080/a.zip", true),
            ("http://127.0.0.1:41234/a.zip", true),
            ("http://lab.local:9090/a.zip", false),
            ("https://lab.local:8080/a.zip", false),
            ("http://lab.local/a.zip", false),
            ("http://127.0.0.1:41235/a.zip", false),
            ("http://10.0.0.1/a.zip", false),
            ("https://zenodo.org/a.zip", true),
        ] {
            let checked = typed.check(&Url::parse(url).unwrap());
            assert_eq!(checked.is_ok(), allowed, "{url}: {checked:?}");
        }
        assert!(typed.may_resolve_privately("lab.local"));
        assert!(!typed.may_resolve_privately("other.local"));
    }
}

#[cfg(test)]
mod classification;
