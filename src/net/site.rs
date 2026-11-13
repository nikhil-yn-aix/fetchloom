//! a website's saved credential as requests carry it: a token, a header or cookies for one host
//! and port. it goes over https, or over plain http only to this machine or an origin
//! `allowed_origins` lists, and only while a request stays on its first origin.

use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

use reqwest::StatusCode;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use url::{Host, Url};

use super::{Net, Shown};
use crate::auth::Account;
use crate::error::{Error, Kind};

/// what one host's saved credential sends.
#[derive(Clone, Debug)]
pub(crate) struct Site {
    /// lowercase, without its port.
    host: String,
    port: u16,
    /// headers sent as they are, such as `authorization` with a bearer token.
    headers: Vec<(HeaderName, HeaderValue)>,
    cookies: Vec<Cookie>,
    /// what is sent, as a refusal names it, such as "the token".
    what: String,
    /// how to save a new one, as a refusal's help says it.
    again: String,
}

/// one saved cookie, kept to its site's host: cookie rules still decide its path, its secure
/// flag and when it expires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) path: String,
    pub(crate) secure: bool,
    /// none for a cookie that ends with the browser's session, which a saved one outlives.
    pub(crate) expires: Option<SystemTime>,
}

impl Site {
    /// the credential for `host`, a host name with `:port` when that is not 443: `headers` and
    /// `cookies`, `what` naming them in a refusal and `again` saying how to save new ones. none
    /// for anything but a plain host, or a header no request can carry.
    pub(crate) fn new(
        host: &str,
        headers: Vec<(HeaderName, String)>,
        cookies: Vec<Cookie>,
        what: String,
        again: String,
    ) -> Option<Site> {
        let (host, port) = super::key_origin(host)?;
        let headers = headers
            .into_iter()
            .map(|(name, value)| {
                let mut value = HeaderValue::from_str(&value).ok()?;
                value.set_sensitive(true);
                Some((name, value))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Site {
            host,
            port,
            headers,
            cookies,
            what,
            again,
        })
    }

    /// whether the credential is for the host and port of `url`.
    fn is_for(&self, url: &Url) -> bool {
        url.host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(&self.host))
            && url.port_or_known_default() == Some(self.port)
    }

    /// the host as messages name it, with its port when that is not 443.
    fn named(&self) -> String {
        if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl Cookie {
    /// whether the cookie goes with a request for `url` at `now`: its path holds the link's, it
    /// is not secure or the link is https, and it has not expired.
    fn applies(&self, url: &Url, now: SystemTime) -> bool {
        self.on_path(url) && self.expires.is_none_or(|expires| expires > now)
    }

    /// whether the cookie would go with a request for `url`, its expiry aside.
    fn on_path(&self, url: &Url) -> bool {
        (!self.secure || url.scheme() == "https") && path_matches(&self.path, url.path())
    }
}

/// whether a cookie for `cookie`'s path goes to `request`'s, as RFC 6265 matches them: the same
/// path, or one below it.
fn path_matches(cookie: &str, request: &str) -> bool {
    let cookie = if cookie.starts_with('/') { cookie } else { "/" };
    request == cookie
        || request.starts_with(cookie)
            && (cookie.ends_with('/') || request[cookie.len()..].starts_with('/'))
}

/// how a run finds the saved credentials, once the first request needs them.
type Find = Box<dyn FnOnce() -> Vec<Site> + Send>;

/// every website's saved credential, looked for when the first request needs one.
pub(crate) struct Sites(Arc<LazyLock<Vec<Site>, Find>>);

impl Default for Sites {
    fn default() -> Sites {
        Sites(Arc::new(LazyLock::new(Box::new(Vec::new))))
    }
}

impl Sites {
    /// the run's cookie store, which keeps the cookies a site with a saved credential sets in a
    /// jar of that site's own.
    pub(super) fn cookies(&self) -> super::cookies::Cookies {
        let sites = Arc::clone(&self.0);
        super::cookies::Cookies::by_site(move |url| {
            sites.iter().find(|site| site.is_for(url)).map(Site::named)
        })
    }
}

/// says a website's saved credential went with a request, as the answer carries it.
#[derive(Clone, Debug)]
pub(crate) struct Sent {
    host: String,
    what: String,
    again: String,
    /// whether the hop that answered carried it, rather than only a hop before a redirect.
    answered: bool,
    /// whether every saved cookie that would have gone had expired, so nothing went.
    expired: bool,
}

impl Sent {
    /// the error for an answer of `status` to `url` that refused the credential.
    pub(crate) fn refusal(&self, url: &Url, status: StatusCode) -> Error {
        let code = status.as_u16();
        let message = if self.expired {
            format!(
                "{} refused access to {} ({code}), and {} saved for it have expired",
                self.host,
                Shown(url),
                self.what
            )
        } else {
            format!(
                "{} refused {} saved for it ({code}); it may have expired",
                self.host, self.what
            )
        };
        Error::new(Kind::AuthRequired, message)
            .help(format!("{}, then run the same command again", self.again))
    }

    /// the error for a web page that came back from `url` where a file was expected, as a site
    /// sends its sign-in page once a session ends.
    fn signed_out(&self, url: &Url) -> Error {
        let message = if self.expired {
            format!(
                "{} sent a web page, such as its sign-in page, where a file was expected, and {} saved for it have expired",
                self.host, self.what
            )
        } else {
            format!(
                "{} sent a web page, such as its sign-in page, where a file was expected, so {} saved for it may have expired",
                self.host, self.what
            )
        };
        Error::new(Kind::AuthRequired, message)
            .detail(format!("the page came from {}", Shown(url)))
            .help(format!(
                "{}, then run the same command again. if the page itself is the data, ask for it with --web-page, or web_page = true in data.toml",
                self.again
            ))
    }

    /// whether the hop that answered carried the credential.
    pub(crate) fn answered(&self) -> bool {
        self.answered
    }
}

/// the refusal of `response`, a web page from `answered` where a file was expected after a saved
/// credential went for `link`, as a site sends its sign-in page once a session ends. a page the
/// link was sent on to, by another origin or path, is refused whatever was asked; else none when
/// the person `asked` for a page, or the page's own name says it is one.
pub(super) fn sign_in_page(
    response: &reqwest::Response,
    answered: &Url,
    link: &Url,
    asked: bool,
) -> Option<Error> {
    let sent = response.extensions().get::<Sent>()?;
    let page = response.status().is_success() && super::headers::is_html(response.headers());
    let moved = answered.origin() != link.origin()
        || answered.path().trim_end_matches('/') != link.path().trim_end_matches('/');
    (page && (moved || !(asked || names_a_page(answered)))).then(|| sent.signed_out(answered))
}

/// whether the last part of `link`'s path ends in a web page's extension.
fn names_a_page(link: &Url) -> bool {
    link.path_segments()
        .and_then(Iterator::last)
        .and_then(|name| name.rsplit_once('.'))
        .is_some_and(|(_, extension)| {
            ["htm", "html", "xhtml"]
                .iter()
                .any(|page| extension.eq_ignore_ascii_case(page))
        })
}

/// the way in a refusal's `WWW-Authenticate` asks for, in lowercase, such as `basic`.
fn challenge(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::WWW_AUTHENTICATE)?.to_str().ok()?;
    let scheme = value.split_whitespace().next()?;
    Some(scheme.trim_end_matches(',').to_ascii_lowercase())
}

/// the host of `url` as `NO_PROXY` lists it, an IPv6 address without its brackets.
fn bare_host(url: &Url) -> String {
    match url.host() {
        Some(Host::Ipv6(ip)) => ip.to_string(),
        _ => url.host_str().unwrap_or_default().to_owned(),
    }
}

/// whether `url` names this machine: a loopback address, or `localhost`.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_loopback())
        }
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// the cookie header for `cookies` with the run's own cookies for the same link, `run`: each
/// header's cookies win by name over those of the headers after it, and all of them over a
/// saved one, since the site set them since. none when the result cannot be a header.
fn cookie_header(cookies: &[&Cookie], run: &[&HeaderValue]) -> Option<HeaderValue> {
    let name = |pair: &str| {
        pair.split_once('=')
            .map_or(pair, |(first, _)| first)
            .to_owned()
    };
    let mut set: Vec<&str> = Vec::new();
    for value in run {
        for pair in value
            .to_str()
            .unwrap_or_default()
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            if !set.iter().any(|kept| name(kept) == name(pair)) {
                set.push(pair);
            }
        }
    }
    let named = |wanted: &str| set.iter().any(|pair| name(pair) == wanted);
    let mut ordered: Vec<&&Cookie> = cookies.iter().collect();
    // longer paths first, as RFC 6265 orders them
    ordered.sort_by_key(|cookie| std::cmp::Reverse(cookie.path.len()));
    let pairs: Vec<String> = ordered
        .into_iter()
        .filter(|cookie| !named(&cookie.name))
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .chain(set.iter().map(|pair| (*pair).to_owned()))
        .collect();
    let mut value = HeaderValue::from_str(&pairs.join("; ")).ok()?;
    value.set_sensitive(true);
    Some(value)
}

impl Net {
    /// sends each website's saved credential, from `find`, with the requests for its host and
    /// port, looked for when the first request needs one.
    pub(crate) fn finding_sites(
        mut self,
        find: impl FnOnce() -> Vec<Site> + Send + 'static,
    ) -> Net {
        self.sites = Sites(Arc::new(LazyLock::new(Box::new(find))));
        self
    }

    /// whether a saved credential may go with a request for `url`: over https, or over plain
    /// http to this machine or to an origin `allowed_origins` lists, a network a person vouched
    /// for, when no proxy carries it. anywhere else plain http would show the credential to
    /// everyone on the way, and a proxy reads a plain http request whole.
    pub(super) fn carries(&self, url: &Url) -> bool {
        match url.scheme() {
            "https" => true,
            "http" => {
                self.vouched(url)
                    && self
                        .wire()
                        .is_ok_and(|wire| wire.proxies.carrier(url).is_none())
            }
            _ => false,
        }
    }

    /// whether `url` is on this machine or an origin `allowed_origins` lists.
    fn vouched(&self, url: &Url) -> bool {
        is_loopback(url) || self.typed.allows(url)
    }

    /// the saved credential for the host and port of `url`, when one may go with a request on
    /// its `first_origin`.
    fn site(&self, url: &Url, first_origin: bool) -> Option<&Site> {
        if !first_origin || !self.carries(url) {
            return None;
        }
        self.sites.0.iter().find(|site| site.is_for(url))
    }

    /// the headers a website's saved credential adds to a request for `url` on its
    /// `first_origin`: its own, and a cookie header with its cookies that apply, the ones the
    /// site set this run, which go nowhere else, and the run's shared ones for the link.
    pub(super) fn site_headers(
        &self,
        url: &Url,
        first_origin: bool,
    ) -> Vec<(HeaderName, HeaderValue)> {
        let Some(site) = self.site(url, first_origin) else {
            return Vec::new();
        };
        let mut headers = site.headers.clone();
        let now = SystemTime::now();
        let cookies: Vec<&Cookie> = site
            .cookies
            .iter()
            .filter(|cookie| cookie.applies(url, now))
            .collect();
        let jar = self.wire().ok().map(|wire| &wire.cookies);
        let kept = jar.and_then(|jar| jar.kept(&site.named(), url));
        if !cookies.is_empty() || kept.is_some() {
            let shared = jar.and_then(|jar| jar.snapshot(url).0);
            let run: Vec<&HeaderValue> = kept.iter().chain(&shared).collect();
            if let Some(value) = cookie_header(&cookies, &run) {
                headers.push((header::COOKIE, value));
            }
        }
        // the header names only; the values are the secret
        tracing::debug!(
            host = site.host,
            headers = headers.len(),
            "sending a site's saved credential"
        );
        headers
    }

    /// marks `response`, the answer to `hop`, with what a website's saved credential sent: on this
    /// hop, or kept in `carried` from an earlier one.
    pub(super) fn mark_site(
        &self,
        response: &mut reqwest::Response,
        hop: &Url,
        first_origin: bool,
        carried: &mut Option<Sent>,
    ) {
        let here = self.site(hop, first_origin).and_then(|site| {
            let now = SystemTime::now();
            let sent = !site.headers.is_empty()
                || site.cookies.iter().any(|cookie| cookie.applies(hop, now));
            let expired = !sent && site.cookies.iter().any(|cookie| cookie.on_path(hop));
            (sent || expired).then(|| Sent {
                host: site.named(),
                what: site.what.clone(),
                again: site.again.clone(),
                answered: true,
                expired,
            })
        });
        let marked = here.or_else(|| {
            carried.clone().map(|earlier| Sent {
                answered: false,
                ..earlier
            })
        });
        if let Some(sent) = marked {
            response.extensions_mut().insert(sent.clone());
            *carried = Some(sent);
        }
    }

    /// the refusal of a 401 or 403 from `url`, a website no source speaks for, when a request for
    /// `asked` carried no credential there: it offers `auth login` for the host and the ways in it
    /// takes, or says why no key goes over plain http there. none for a source's host, whose own
    /// refusal says more, and for a host a redirect led to that a website's key cannot reach.
    pub(super) fn refused(
        &self,
        asked: &Url,
        url: &Url,
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Option<Error> {
        let code = status.as_u16();
        let host = url.host_str()?.to_ascii_lowercase();
        if crate::source::refused_download(url, code).is_some()
            || Account::from_host(&host).is_some()
        {
            return None;
        }
        let named = match url.port_or_known_default() {
            Some(443) | None => host,
            Some(port) => format!("{host}:{port}"),
        };
        let error = Error::new(
            Kind::AuthRequired,
            format!(
                "{} refused access to {} ({code})",
                super::host(url),
                Shown(url)
            ),
        );
        if !self.carries(url) {
            let proxy = self
                .vouched(url)
                .then(|| self.wire().ok()?.proxies.carrier(url));
            return Some(error.help(match proxy.flatten() {
                Some(proxy) => format!(
                    "a proxy, here {}, reads a plain http request whole, so fetchloom sends no key through one: add {} to NO_PROXY to send the request straight there, or ask the site for an https link",
                    proxy.shown,
                    bare_host(url)
                ),
                None => "fetchloom sends a key over plain http only to this machine or to an origin allowed_origins lists, since anyone on the way could read it there: ask the site for an https link".to_owned(),
            }));
        }
        let way = challenge(headers);
        let account = Account::Login(named.clone());
        let command = account.command();
        // a server that asks for a username and password takes the login a person saves for
        // its host, as curl and wget would send from .netrc, on any hop that reaches it
        if way.as_deref() == Some("basic") {
            return Some(
                error
                    .help(format!("if it needs a login, sign in with `{command}`, then run the same command again"))
                    .needs(account, None),
            );
        }
        if asked.origin() != url.origin() {
            return None;
        }
        let help = if way.as_deref() == Some("bearer") {
            format!(
                "if it needs a token, save it with `{command} --token`, then run the same command again"
            )
        } else {
            format!(
                "if it needs a sign-in, save it with `{command}` and --browser <name> to take the cookies of a browser signed in there, --cookies <file>, --token or --header <name>, then run the same command again"
            )
        };
        Some(error.help(help).needs_site(named))
    }

    /// the hosts a website's credential goes to, for tests.
    #[cfg(test)]
    pub(crate) fn site_hosts(&self) -> Vec<String> {
        self.sites.0.iter().map(Site::named).collect()
    }
}

#[cfg(test)]
mod tests;
