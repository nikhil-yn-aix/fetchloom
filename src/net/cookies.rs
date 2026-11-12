use std::sync::{Mutex, PoisonError};

use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::HeaderValue;
use url::Url;

#[derive(Default)]
pub(super) struct Cookies {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    jar: Jar,
    generation: u64,
}

impl Cookies {
    pub(super) fn snapshot(&self, url: &Url) -> (Option<HeaderValue>, u64) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.jar.cookies(url), state.generation)
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.jar.set_cookies(&mut headers, url);
        state.generation = state.generation.wrapping_add(1);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        self.snapshot(url).0
    }
}
