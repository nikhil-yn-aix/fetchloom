use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;
use url::Url;

/// names the site a link is on, by its host and port, when a person saved a credential for it.
type SiteOf = Box<dyn Fn(&Url) -> Option<String> + Send + Sync>;

/// whether a request for a link may carry a cookie, as `site::carried` judges.
type Carries = Box<dyn Fn(&Url) -> bool + Send + Sync>;

/// whether a link's host has a saved credential, on any port, or a key, as a `.netrc` login or a
/// source's token.
type Guarded = Box<dyn Fn(&Url) -> bool + Send + Sync>;

/// the cookies this run's answers set: one jar a request draws on by cookie rules where a saved
/// credential may go, each cookie in it for the host that set it alone, and one of its own for
/// each site a person saved a credential for, whose cookies, a renewed session among them, go
/// only where that credential may, through `Net::site_headers`.
#[derive(Default)]
pub(super) struct Cookies {
    state: Mutex<State>,
    site: Option<SiteOf>,
    /// where the shared jar's cookies may go: where a saved credential may, so a session a host
    /// with a `.netrc` login set never crosses plain http off this machine or a proxy.
    carries: Option<Carries>,
    /// the hosts whose cookies only an answer a credential could go with may set.
    guarded: Option<Guarded>,
}

#[derive(Default)]
struct State {
    jar: Jar,
    /// each site's own cookies, by the site's name.
    sites: HashMap<String, Jar>,
    generation: u64,
}

impl Cookies {
    /// a store that keeps the cookies a link sets in the jar of the site `site` names for it.
    pub(super) fn by_site(
        site: impl Fn(&Url) -> Option<String> + Send + Sync + 'static,
    ) -> Cookies {
        Cookies {
            state: Mutex::default(),
            site: Some(Box::new(site)),
            carries: None,
            guarded: None,
        }
    }

    /// the store, its shared jar sending a cookie only with a request `carries` allows.
    pub(super) fn carrying(
        self,
        carries: impl Fn(&Url) -> bool + Send + Sync + 'static,
    ) -> Cookies {
        Cookies {
            carries: Some(Box::new(carries)),
            ..self
        }
    }

    /// the store, taking no cookie for a host `guarded` names from an answer a credential could
    /// not go with, as `carrying` judges.
    pub(super) fn guarding(
        self,
        guarded: impl Fn(&Url) -> bool + Send + Sync + 'static,
    ) -> Cookies {
        Cookies {
            guarded: Some(Box::new(guarded)),
            ..self
        }
    }

    /// whether a credential could go with a request for `url`, and so the cookies of its answer.
    fn carried(&self, url: &Url) -> bool {
        self.carries.as_ref().is_none_or(|carries| carries(url))
    }

    /// the shared jar's cookies for a request for `url`, when it may carry them, and how many
    /// times the jar has changed.
    pub(super) fn snapshot(&self, url: &Url) -> (Option<HeaderValue>, u64) {
        let carried = self.carried(url);
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let cookies = if carried {
            state.jar.cookies(url)
        } else {
            None
        };
        (cookies, state.generation)
    }

    /// the cookies the site named `site` set this run that go with a request for `url`.
    pub(super) fn kept(&self, site: &str, url: &Url) -> Option<HeaderValue> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.sites.get(site)?.cookies(url)
    }

    pub(super) fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
    }

    #[cfg(test)]
    pub(super) fn add_cookie_str(&self, cookie: &str, url: &Url) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.jar.add_cookie_str(cookie, url);
        state.generation = state.generation.wrapping_add(1);
    }
}

impl CookieStore for Cookies {
    fn set_cookies(&self, headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let mut headers = headers.peekable();
        if headers.peek().is_none() {
            return;
        }
        // whoever is on the way can answer plain http in the host's name, so a session it sets
        // for a signed-in host would make the host's own requests run as someone else
        if !self.carried(url) && self.guarded.as_ref().is_some_and(|guarded| guarded(url)) {
            tracing::debug!(
                host = url.host_str(),
                "an answer no credential could go with set cookies for a signed-in host, so they are left out"
            );
            return;
        }
        let site = self.site.as_ref().and_then(|site| site(url));
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let State {
            jar,
            sites,
            generation,
        } = &mut *state;
        if let Some(site) = site {
            sites
                .entry(site)
                .or_default()
                .set_cookies(&mut headers, url);
        } else {
            let host_only: Vec<HeaderValue> = headers.filter_map(host_only).collect();
            jar.set_cookies(&mut host_only.iter(), url);
        }
        *generation = generation.wrapping_add(1);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.snapshot(url).0
    }
}

/// `cookie`, a `Set-Cookie` value, without its `Domain` attribute, so it goes back to the host
/// that set it alone. the jar knows no public suffix list, so a domain such as `Domain=me` would
/// let one host set a cookie for every host under it. none for a value that is not text.
fn host_only(cookie: &HeaderValue) -> Option<HeaderValue> {
    let text = std::str::from_utf8(cookie.as_bytes()).ok()?;
    let mut parts = text.split(';');
    let pair = parts.next()?;
    let attributes = parts.filter(|attribute| {
        let name = attribute
            .split_once('=')
            .map_or(*attribute, |(name, _)| name);
        !name.trim().eq_ignore_ascii_case("domain")
    });
    let kept: Vec<&str> = std::iter::once(pair).chain(attributes).collect();
    HeaderValue::from_bytes(kept.join(";").as_bytes()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a cookie a site with a saved credential sets stays in that site's own jar, out of the
    /// shared one, while another host's cookie goes to the shared jar for that host alone.
    #[test]
    fn a_saved_sites_cookies_stay_in_its_own_jar() {
        let saved = "data.example.org";
        let cookies =
            Cookies::by_site(move |url| (url.host_str() == Some(saved)).then(|| saved.to_owned()));
        let link = |text: &str| Url::parse(text).unwrap();
        let set = |cookie: &str, url: &Url| {
            let cookie = HeaderValue::from_str(cookie).unwrap();
            cookies.set_cookies(&mut std::iter::once(&cookie), url);
        };
        let (site, other) = (
            link("https://data.example.org/files"),
            link("https://other.example.org/files"),
        );
        set("session=one; Domain=example.org; Path=/", &site);
        set("visit=two; Domain=example.org; Path=/", &other);
        assert_eq!(cookies.kept(saved, &site).unwrap(), "session=one");
        assert_eq!(cookies.snapshot(&site).0, None);
        assert_eq!(cookies.snapshot(&other).0.unwrap(), "visit=two");
        assert_eq!(cookies.snapshot(&link("https://www.example.org/")).0, None);
        assert_eq!(cookies.generation(), 2);
    }
}
