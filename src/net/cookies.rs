use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;
use url::Url;

/// names the site a link is on, by its host and port, when a person saved a credential for it.
type SiteOf = Box<dyn Fn(&Url) -> Option<String> + Send + Sync>;

/// the cookies this run's answers set: one jar any request draws on by cookie rules, and one of
/// its own for each site a person saved a credential for, whose cookies, a renewed session among
/// them, go only where that credential may, through `Net::site_headers`.
#[derive(Default)]
pub(super) struct Cookies {
    state: Mutex<State>,
    site: Option<SiteOf>,
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
        }
    }

    pub(super) fn snapshot(&self, url: &Url) -> (Option<HeaderValue>, u64) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.jar.cookies(url), state.generation)
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
        let site = self.site.as_ref().and_then(|site| site(url));
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let State {
            jar,
            sites,
            generation,
        } = &mut *state;
        let jar = match site {
            Some(site) => sites.entry(site).or_default(),
            None => jar,
        };
        jar.set_cookies(&mut headers, url);
        *generation = generation.wrapping_add(1);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.snapshot(url).0
    }
}
