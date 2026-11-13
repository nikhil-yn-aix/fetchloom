//! where a request may go. only the origins a person typed, named as a mirror or listed in
//! `allowed_origins` may reach private addresses, so a cloned project, a hostile server or a
//! tampered lock cannot reach into a local network.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hyper_util::client::proxy::matcher::Matcher;
use url::{Host, Url};

use super::Shown;
use crate::error::Error;

/// the origins a person typed, named or allowed, and the proxies this machine sends requests
/// through.
#[derive(Clone, Default)]
pub(crate) struct Typed(Arc<Mutex<Hosts>>);

#[derive(Default)]
struct Hosts {
    /// exact origins: another port or scheme of the same host is not trusted.
    origins: HashSet<url::Origin>,
    /// a proxy may sit on a private address, and only reaching it is allowed: a URL that asks
    /// for the proxy's host itself, as a redirect could, is refused unless its origin is trusted.
    proxies: HashSet<String>,
    /// the `allowed_origins` setting, which a refusal quotes.
    allowed: Allowed,
    /// names this run's lookups for a trusted origin found on a private address.
    private: HashSet<String>,
}

/// the `allowed_origins` setting as refusals and hints quote it: the origins it lists and where
/// a person changes it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Allowed {
    pub(crate) origins: Vec<url::Origin>,
    /// set by `FETCHLOOM_ALLOWED_ORIGINS`, which wins over the settings file while it is set.
    pub(crate) by_variable: bool,
    /// the settings file, which keeps the setting for every run.
    pub(crate) file: Option<PathBuf>,
}

impl Allowed {
    /// how to let a later run reach `origin` too: the setting's whole value with `origin` added,
    /// for the settings file and one run's variable, or for the variable alone while it sets it.
    pub(crate) fn remedy(&self, origin: &url::Origin) -> String {
        let mut listed: Vec<String> = self
            .origins
            .iter()
            .map(url::Origin::ascii_serialization)
            .collect();
        let origin = origin.ascii_serialization();
        if !listed.contains(&origin) {
            listed.push(origin);
        }
        let variable = format!("`FETCHLOOM_ALLOWED_ORIGINS={}`", listed.join(","));
        if self.by_variable {
            return format!("set {variable}");
        }
        let quoted: Vec<String> = listed
            .iter()
            .map(|origin| format!("\"{origin}\""))
            .collect();
        let file = self
            .file
            .as_deref()
            .map_or_else(|| "the settings file".to_owned(), crate::units::path);
        format!(
            "put `allowed_origins = [{}]` in {file}, or set {variable} for one run",
            quoted.join(", ")
        )
    }
}

/// which addresses the lookups of one HTTP client may keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// any address of a host a trusted origin is on, for requests to a trusted origin.
    Trusted,
    /// public addresses alone, for every other request.
    Public,
}

impl Typed {
    /// trusts the origin of `url`, as a test server standing in for a source needs.
    #[cfg(debug_assertions)]
    pub(crate) fn add(&self, url: &Url) {
        self.allow([url.origin()]);
    }

    /// lets the proxy at `host` be reached, wherever its address is.
    pub(crate) fn add_proxy(&self, host: &str) {
        self.hosts().proxies.insert(key(host));
    }

    /// lets requests reach `origins`, wherever they are: those the `allowed_origins` setting
    /// lists, and those of the links and mirrors a person gives.
    pub(crate) fn allow(&self, origins: impl IntoIterator<Item = url::Origin>) {
        self.hosts().origins.extend(origins);
    }

    /// lets requests reach the origins `allowed` lists, and keeps it for refusals to quote.
    fn set_allowed(&self, allowed: Allowed) {
        let mut hosts = self.hosts();
        hosts.origins.extend(allowed.origins.iter().cloned());
        hosts.allowed = allowed;
    }

    /// whether `origin` is on a private address: its host is one written out, or a name this
    /// run's lookups for a trusted origin found on one. nothing is looked up for it.
    pub(crate) fn on_private_address(&self, origin: &url::Origin) -> bool {
        let url::Origin::Tuple(_, host, _) = origin else {
            return false;
        };
        match host {
            Host::Ipv4(ip) => is_private(IpAddr::V4(*ip)),
            Host::Ipv6(ip) => is_private(IpAddr::V6(*ip)),
            Host::Domain(name) => self.hosts().private.contains(&key(name)),
        }
    }

    /// the refusal of `host`'s private address, `reason`, which `allowed_origins` can lift.
    fn refused(&self, reason: String, host: &str) -> Refused {
        self.hosts().refused(reason, &key(host))
    }

    /// whether the origin of `url` is trusted.
    pub(crate) fn trusts(&self, url: &Url) -> bool {
        self.hosts().origins.contains(&url.origin())
    }

    /// whether the `allowed_origins` setting lists the origin of `url`.
    pub(crate) fn allows(&self, url: &Url) -> bool {
        self.hosts().allowed.origins.contains(&url.origin())
    }

    /// whether `host` may resolve to a private address for a client that keeps `reach`: a proxy
    /// may for every client, a host a trusted origin is on only for the trusted client.
    pub(crate) fn may_resolve_privately(&self, host: &str, reach: Reach) -> bool {
        let key = key(host);
        let hosts = self.hosts();
        hosts.proxies.contains(&key) || (reach == Reach::Trusted && hosts.trusts_on(&key))
    }

    fn hosts(&self) -> std::sync::MutexGuard<'_, Hosts> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// refuses a URL whose host is a private address written out, or a proxy's host, unless its
    /// origin is trusted. names are checked later, on the addresses they resolve to.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        let Some(host) = url.host_str() else {
            return Err(Refused::new(format!("{} has no host", Shown(url))));
        };
        let hosts = self.hosts();
        if hosts.origins.contains(&url.origin()) {
            return Ok(());
        }
        let host = key(host);
        if hosts.proxies.contains(&host) {
            return Err(hosts.refused(
                "it asks for the proxy this machine sends requests through".to_owned(),
                &host,
            ));
        }
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            _ => return Ok(()),
        };
        if is_private(ip) {
            return Err(hosts.refused(format!("it points at the private address {ip}"), &host));
        }
        Ok(())
    }
}

impl super::Net {
    /// lets requests reach the origins the `allowed_origins` setting lists, wherever they are,
    /// and has refusals quote the setting.
    pub(crate) fn with_allowed_origins(self, allowed: Allowed) -> super::Net {
        self.typed.set_allowed(allowed);
        self
    }

    /// whether `origin` is on a private address, as far as its host or this run's lookups say.
    pub(crate) fn on_private_address(&self, origin: &url::Origin) -> bool {
        self.typed.on_private_address(origin)
    }
}

impl Hosts {
    /// the refusal of `host`'s private address, `reason`, naming the origins trusted on `host`,
    /// given as [`key`] makes it, and quoting `allowed_origins`, which can lift it.
    fn refused(&self, reason: String, host: &str) -> Refused {
        Refused {
            reason,
            trusted_on_host: self.origins_on(host),
            allowed: Some(self.allowed.clone()),
        }
    }

    /// whether a trusted origin is on `host`, given as [`key`] makes it.
    fn trusts_on(&self, host: &str) -> bool {
        self.origins.iter().any(|origin| is_on(origin, host))
    }

    /// the trusted origins on `host`, given as [`key`] makes it, sorted as messages show them.
    fn origins_on(&self, host: &str) -> Vec<String> {
        let mut on: Vec<String> = self
            .origins
            .iter()
            .filter(|origin| is_on(origin, host))
            .map(url::Origin::ascii_serialization)
            .collect();
        on.sort();
        on
    }
}

/// whether `origin` is on `host`, given as [`key`] makes it.
fn is_on(origin: &url::Origin, host: &str) -> bool {
    match origin {
        url::Origin::Tuple(_, on, _) => key(&on.to_string()) == host,
        url::Origin::Opaque(_) => false,
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
pub(crate) struct Refused {
    reason: String,
    /// for a private address, the origins trusted on the same host, as messages show them: a
    /// person may have expected one of them to cover this URL too.
    pub(crate) trusted_on_host: Vec<String>,
    /// the `allowed_origins` setting, for a refusal that listing the origin there lifts.
    pub(crate) allowed: Option<Allowed>,
}

impl Refused {
    /// a refusal no setting lifts, such as a link without a host.
    pub(crate) fn new(reason: impl Into<String>) -> Refused {
        Refused {
            reason: reason.into(),
            trusted_on_host: Vec::new(),
            allowed: None,
        }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
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

/// whether a failed lookup said the name does not exist, rather than that DNS could not answer.
/// on unix only getaddrinfo's message says so, worded differently by glibc, musl and macos.
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

/// the proxy each set variable names, none for an empty one. a value the matcher cannot read,
/// which it would treat as unset, is refused, so requests never go around a proxy a person set.
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

    /// checks `url` before a proxy carries it: the proxy resolves names itself, so an untrusted
    /// host is looked up here first and refused when any address is private. a name this machine
    /// cannot resolve is left to the proxy, since some networks resolve names only through it.
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
            self.lock().insert(name.clone(), refused.clone());
            refused
        };
        refused.map_or(Ok(()), |reason| Err(typed.refused(reason, &name)))
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

/// resolves names for one HTTP client, keeping the private addresses only that its `reach`
/// allows.
pub(crate) struct Resolver {
    typed: Typed,
    offline: bool,
    reach: Reach,
}

impl Resolver {
    pub(crate) fn new(typed: Typed, offline: bool, reach: Reach) -> Resolver {
        Resolver {
            typed,
            offline,
            reach,
        }
    }
}

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let allowed = self.typed.may_resolve_privately(&host, self.reach);
        let typed = self.typed.clone();
        let offline = self.offline;
        let reach = self.reach;
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
                let refused =
                    typed.refused(format!("{host} resolves only to private addresses"), &host);
                return Err(Box::new(refused) as Box<dyn std::error::Error + Send + Sync>);
            }
            // noted so an add can say its typed origin needs allowed_origins later, with no
            // lookup of its own
            if reach == Reach::Trusted && kept.iter().any(|address| is_private(address.ip())) {
                typed.hosts().private.insert(key(&host));
            }
            Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// addresses of this machine or its local network rather than the internet. 198.18.0.0/15 stays
/// public: clash and mihomo answer every name with a fake address there.
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

// IPv6 documentation classification is still unstable in std
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

/// whether `ip` sits in the local-use NAT64 prefix `64:ff9b:1::/48`, where each network places the
/// IPv4 address itself, so every address there counts as private.
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

    /// the private rows cover each range a request could reach a local or metadata host by,
    /// ipv4 carried in ipv6 included; the public rows sit just outside them.
    #[test]
    fn reserved_and_metadata_ranges_are_private_and_the_rest_public() {
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
        for reach in [Reach::Trusted, Reach::Public] {
            assert!(typed.may_resolve_privately("proxy.lab", reach));
            assert!(!typed.may_resolve_privately("elsewhere.lab", reach));
        }
        typed.add(&Url::parse("http://proxy.lab:9000/x").unwrap());
        assert!(
            typed
                .check(&Url::parse("http://proxy.lab:9000/y").unwrap())
                .is_ok(),
            "unless a person typed its origin"
        );
    }

    /// the host's other origins go by the other client and cannot reach this machine's network.
    #[test]
    fn a_trusted_name_resolves_privately_for_the_trusted_client_alone() {
        let typed = Typed::default();
        typed.add(&Url::parse("http://LAB.local:8080/a.zip").unwrap());
        assert!(typed.may_resolve_privately("lab.local", Reach::Trusted));
        assert!(!typed.may_resolve_privately("lab.local", Reach::Public));
        assert!(!typed.may_resolve_privately("other.local", Reach::Trusted));
        let on = |host| typed.refused(String::new(), host).trusted_on_host;
        assert_eq!(on("Lab.Local"), ["http://lab.local:8080"]);
        assert_eq!(on("other.local"), Vec::<String>::new());
    }

    /// the refusal quotes the settings file's line with what it lists already and one run's
    /// variable, or the variable alone while it sets the list, since it then wins over the file.
    #[test]
    fn a_refusal_quotes_the_whole_setting_that_lifts_it() {
        let origin = Url::parse("http://10.0.0.5:9001/a.zip").unwrap().origin();
        let lab = Url::parse("http://lab.local:8080").unwrap().origin();
        let file = Some(PathBuf::from("/home/ada/.config/fetchloom/settings.toml"));
        let remedy = |origins: Vec<url::Origin>, by_variable, file| {
            Allowed {
                origins,
                by_variable,
                file,
            }
            .remedy(&origin)
        };
        assert_eq!(
            remedy(Vec::new(), false, file.clone()),
            "put `allowed_origins = [\"http://10.0.0.5:9001\"]` in /home/ada/.config/fetchloom/settings.toml, or set `FETCHLOOM_ALLOWED_ORIGINS=http://10.0.0.5:9001` for one run"
        );
        assert_eq!(
            remedy(vec![lab.clone()], false, None),
            "put `allowed_origins = [\"http://lab.local:8080\", \"http://10.0.0.5:9001\"]` in the settings file, or set `FETCHLOOM_ALLOWED_ORIGINS=http://lab.local:8080,http://10.0.0.5:9001` for one run"
        );
        assert_eq!(
            remedy(vec![lab], true, file),
            "set `FETCHLOOM_ALLOWED_ORIGINS=http://lab.local:8080,http://10.0.0.5:9001`"
        );
    }

    /// another port or scheme of the host is a url nobody typed: a private address written out is
    /// refused at once, and a name is left to the addresses it resolves to.
    #[test]
    fn a_trusted_origin_trusts_nothing_else_on_its_host() {
        let typed = Typed::default();
        typed.allow([Url::parse("http://lab.local:8080").unwrap().origin()]);
        typed.add(&Url::parse("http://10.0.0.5:9000/fetchloom/").unwrap());
        typed.add(&Url::parse("https://[fd00::5]:8443/data/a.zip").unwrap());
        for (url, trusted, checked) in [
            ("http://lab.local:8080/shards/a.zip", true, true),
            ("http://LAB.local:8080/a.zip", true, true),
            ("http://lab.local:9090/a.zip", false, true),
            ("https://lab.local:8080/a.zip", false, true),
            ("http://lab.local/a.zip", false, true),
            ("http://10.0.0.5:9000/fetchloom/objects/ab/cd", true, true),
            ("http://10.0.0.5:9001/a.zip", false, false),
            ("https://10.0.0.5:9000/a.zip", false, false),
            ("https://[fd00::5]:8443/b.zip", true, true),
            ("https://[fd00::5]/b.zip", false, false),
            ("http://10.0.0.1/a.zip", false, false),
            ("https://zenodo.org/a.zip", false, true),
        ] {
            let url = Url::parse(url).unwrap();
            assert_eq!(typed.trusts(&url), trusted, "{url}");
            let checked_url = typed.check(&url);
            assert_eq!(checked_url.is_ok(), checked, "{url}: {checked_url:?}");
        }
        let refused = |url: &str| typed.check(&Url::parse(url).unwrap()).unwrap_err();
        assert_eq!(
            refused("http://10.0.0.5:9001/a.zip").trusted_on_host,
            ["http://10.0.0.5:9000"]
        );
        assert_eq!(
            refused("https://[fd00::5]/b.zip").trusted_on_host,
            ["https://[fd00::5]:8443"]
        );
        assert_eq!(
            refused("http://10.0.0.1/a.zip").trusted_on_host,
            Vec::<String>::new()
        );
    }
}

#[cfg(test)]
mod classification;
