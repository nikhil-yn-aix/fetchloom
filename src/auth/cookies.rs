//! cookies a person signed in with, from a cookies.txt file or a browser's profile, and which of
//! them a website's host takes: those whose domain covers it, unexpired, and that a request
//! header can hold. the rest are named, by name and domain only, with why they were left out.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use super::site::{Cookie, without_port};
use crate::error::{Error, Kind};
use crate::source::Var;

mod chromium;
mod firefox;
mod sqlite;

/// why a cookie was left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Why {
    /// its domain covers other hosts, not this one.
    Elsewhere,
    Expired,
    /// a request header cannot hold its name or value.
    Unsendable,
    /// the browser keeps it apart: for a container, a private window or another site's frames.
    Apart,
    /// it is encrypted with a key fetchloom could not read.
    Locked,
    /// it is kept in the clear, where the browser seals every cookie it keeps.
    Unsealed,
}

/// what a browser's profile holds: its cookies, and those it keeps apart or cannot open.
#[derive(Debug, Default)]
pub(crate) struct Read {
    pub(crate) found: Vec<Cookie>,
    pub(crate) left: Vec<Left>,
}

/// a browser whose profile fetchloom reads cookies from, and the profile a person named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Browser {
    named: Named,
    profile: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    Firefox,
    Chromium(chromium::Chromium),
}

/// the browsers `--browser` takes, as its help lists them.
pub(crate) const BROWSERS: &str = "firefox, chrome, chromium, brave, edge, vivaldi or opera";

impl Browser {
    /// the browser `typed` names, with a profile after a `:`, as `firefox:work` or a profile's
    /// folder.
    ///
    /// # Errors
    ///
    /// for a browser fetchloom does not read.
    pub(crate) fn parse(typed: &str) -> Result<Browser, Error> {
        let (name, profile) = match typed.split_once(':') {
            Some((name, profile)) if !profile.is_empty() => (name, Some(profile.to_owned())),
            _ => (typed.trim_end_matches(':'), None),
        };
        let lowered = name.trim().to_ascii_lowercase();
        let chromium = chromium::BROWSERS
            .iter()
            .find(|browser| browser.name == lowered);
        let named = match (lowered.as_str(), chromium) {
            ("firefox", _) => Named::Firefox,
            (_, Some(browser)) => Named::Chromium(*browser),
            _ => {
                return Err(Error::new(
                    Kind::Usage,
                    format!(
                        "fetchloom does not read cookies from \"{}\"",
                        name.escape_debug()
                    ),
                )
                .help(format!(
                    "name one of {BROWSERS}, or export the site's cookies as cookies.txt with a browser extension and use --cookies <file>"
                )));
            }
        };
        Ok(Browser { named, profile })
    }

    /// the browser as people name it.
    pub(crate) fn name(&self) -> &'static str {
        match self.named {
            Named::Firefox => "firefox",
            Named::Chromium(browser) => browser.name,
        }
    }

    /// the cookies `host` takes at `now` from the profile this browser used last, or the one a
    /// person named, with those left out, and the profile's folder.
    ///
    /// # Errors
    ///
    /// when no profile has cookies, or its file cannot be read.
    pub(crate) fn cookies(
        &self,
        var: Var<'_>,
        host: &str,
        now: SystemTime,
    ) -> Result<(Picked, PathBuf), Error> {
        let name = self.name();
        let profile = self.profile.as_deref();
        let (roots, file) = match self.named {
            Named::Firefox => {
                let roots = firefox::roots(var);
                let file = firefox::file(&roots, profile);
                (roots, file)
            }
            Named::Chromium(browser) => {
                let root = browser.root(var).ok_or_else(|| {
                    Error::new(
                        Kind::Usage,
                        format!("{name} on windows keeps its cookies under app-bound encryption, which only {name} itself may open"),
                    )
                    .help("use --browser firefox, or export the site's cookies as cookies.txt with a browser extension and use --cookies <file>")
                })?;
                let file = chromium::file(&root, profile);
                (vec![root], file)
            }
        };
        let file = file.ok_or_else(|| {
            let looked: Vec<String> = roots.iter().map(|root| crate::units::path(root)).collect();
            Error::new(Kind::Usage, format!("found no {name} profile with cookies"))
                .detail(format!("looked in {}", looked.join(", ")))
                .help(format!(
                    "sign in to {} in {name} first, or name the profile's folder, as `--browser {name}:<folder>`",
                    without_port(host),
                ))
        })?;
        let read = match self.named {
            Named::Firefox => firefox::read(&file),
            Named::Chromium(browser) => {
                let store = super::store::KEYRING.usable(var);
                // chromium's fixed linux key needs no store; any other key is the store's
                let key = |tag: &str| {
                    let fixed = tag == "v10" && !cfg!(target_os = "macos");
                    (fixed || store).then(|| browser.key(tag)).flatten()
                };
                chromium::read(&file, host, &key)
            }
        }
        .map_err(|err| {
            Error::io(
                format!(
                    "could not read {name}'s cookies in {}",
                    crate::units::path(&file)
                ),
                &err,
            )
        })?;
        let mut picked = pick(read.found, host, now);
        picked.left.extend(
            read.left
                .into_iter()
                .filter(|left| covers(&left.domain, host)),
        );
        // chromium keeps its cookies in the profile's `Network` folder
        let mut profile = file.parent().unwrap_or(&file);
        if profile.file_name().is_some_and(|name| name == "Network") {
            profile = profile.parent().unwrap_or(profile);
        }
        Ok((picked, profile.to_path_buf()))
    }
}

/// the newest of `files` by when it last changed, as the profile a browser used last.
fn newest(files: Vec<PathBuf>) -> Option<PathBuf> {
    files
        .into_iter()
        .filter_map(|file| {
            let changed = std::fs::metadata(&file)
                .and_then(|meta| meta.modified())
                .ok()?;
            Some((changed, file))
        })
        .max_by_key(|(changed, _)| *changed)
        .map(|(_, file)| file)
}

/// a cookie left out, named without its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Left {
    pub(crate) name: String,
    pub(crate) domain: String,
    pub(crate) why: Why,
}

/// the cookies a host takes, and those left out.
#[derive(Debug, Default)]
pub(crate) struct Picked {
    pub(crate) kept: Vec<Cookie>,
    pub(crate) left: Vec<Left>,
}

/// whether a cookie for `domain` goes to `host`, as RFC 6265 matches a domain: the host itself,
/// or a host below a domain written with a leading dot. an address matches only itself.
pub(crate) fn covers(domain: &str, host: &str) -> bool {
    let host = without_port(host).to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    match domain.strip_prefix('.') {
        Some(parent) => {
            host == parent
                || (host.ends_with(&format!(".{parent}"))
                    && host.parse::<std::net::IpAddr>().is_err())
        }
        None => host == domain,
    }
}

/// whether a request's cookie header can hold `cookie`: a name without `=`, `;`, spaces or
/// control characters, and a value without `;` or control characters, which would end it early
/// or break the header.
fn sendable(cookie: &Cookie) -> bool {
    let plain = |c: char| !c.is_control() && c != ';';
    !cookie.name.is_empty()
        && cookie
            .name
            .chars()
            .all(|c| plain(c) && c != '=' && !c.is_whitespace())
        && cookie.value.chars().all(plain)
}

/// the cookies of `found` that go to `host` at `now`, and the rest, each with why. of cookies
/// with one name, domain and path, the last found wins, as in a browser's jar.
pub(crate) fn pick(found: Vec<Cookie>, host: &str, now: SystemTime) -> Picked {
    let mut kept: BTreeMap<(String, String, String), (usize, Cookie)> = BTreeMap::new();
    let mut left = Vec::new();
    for (at, cookie) in found.into_iter().enumerate() {
        let why = if !covers(&cookie.domain, host) {
            Some(Why::Elsewhere)
        } else if cookie
            .expires
            .is_some_and(|expires| super::site::moment(expires) <= now)
        {
            Some(Why::Expired)
        } else if !sendable(&cookie) {
            Some(Why::Unsendable)
        } else {
            None
        };
        if let Some(why) = why {
            left.push(Left {
                name: cookie.name,
                domain: cookie.domain,
                why,
            });
        } else {
            let key = (
                cookie.name.clone(),
                cookie.domain.to_ascii_lowercase(),
                cookie.path.clone(),
            );
            kept.insert(key, (at, cookie));
        }
    }
    let mut kept: Vec<(usize, Cookie)> = kept.into_values().collect();
    kept.sort_by_key(|(at, _)| *at);
    Picked {
        kept: kept.into_iter().map(|(_, cookie)| cookie).collect(),
        left,
    }
}

/// the cookies a cookies.txt file holds, as curl's `-b` and yt-dlp read them and browser
/// extensions write them: a cookie a line, seven fields apart by tabs, `#` starting a comment
/// unless it starts `#HttpOnly_`, and an expiry of 0 for a cookie that ends with the browser's
/// session. also how many lines were neither a cookie, a comment nor blank.
pub(crate) fn netscape(text: &str) -> (Vec<Cookie>, usize) {
    let mut cookies = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let (domain, below, path, secure, expires, name, value) = match fields.as_slice() {
            [domain, below, path, secure, expires, name, value] => {
                (*domain, *below, *path, *secure, *expires, *name, *value)
            }
            // curl takes a cookie whose value is empty without its last tab
            [domain, below, path, secure, expires, name] => {
                (*domain, *below, *path, *secure, *expires, *name, "")
            }
            _ => {
                skipped += 1;
                continue;
            }
        };
        let flag = |text: &str| text.eq_ignore_ascii_case("true");
        let Ok(expires) = expires.trim().parse::<i64>() else {
            skipped += 1;
            continue;
        };
        let bare = domain.trim().trim_start_matches('.');
        if bare.is_empty() {
            skipped += 1;
            continue;
        }
        cookies.push(Cookie {
            name: name.to_owned(),
            value: value.to_owned(),
            domain: if flag(below) {
                format!(".{bare}")
            } else {
                bare.to_owned()
            },
            path: if path.starts_with('/') {
                path.to_owned()
            } else {
                "/".to_owned()
            },
            secure: flag(secure),
            expires: (expires > 0).then_some(expires),
        });
    }
    (cookies, skipped)
}

#[cfg(test)]
mod profile_tests;
#[cfg(test)]
mod tests;
