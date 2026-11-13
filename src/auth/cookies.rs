//! cookies a person signed in with, from a cookies.txt file or a browser's profile, and which of
//! them a website's host takes: those whose domain covers it, unexpired, and that a request
//! header can hold. the rest are named, by name and domain only, with why they were left out.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::site::{Cookie, without_port};
use crate::error::{Error, Kind};
use crate::source::Var;

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
}

/// the browsers `--browser` takes, as its help lists them.
pub(crate) const BROWSERS: &str = "firefox";

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
        let named = match name.trim().to_ascii_lowercase().as_str() {
            "firefox" => Named::Firefox,
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
        }
    }

    /// the cookies `host` takes at `now` from the profile this browser used last, or the one a
    /// person named, with those left out, and the file they came from.
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
        let roots = firefox::roots(var);
        let file = firefox::file(&roots, self.profile.as_deref()).ok_or_else(|| {
            let looked: Vec<String> = roots.iter().map(|root| crate::units::path(root)).collect();
            Error::new(
                Kind::Usage,
                format!("found no {} profile with cookies", self.name()),
            )
            .detail(format!("looked in {}", looked.join(", ")))
            .help(format!(
                "sign in to {} in {} first, or name the profile's folder, as `--browser {}:<folder>`",
                without_port(host),
                self.name(),
                self.name()
            ))
        })?;
        let read = firefox::read(&file).map_err(|err| {
            Error::io(
                format!(
                    "could not read {}'s cookies in {}",
                    self.name(),
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
        Ok((picked, file))
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
        } else if cookie.expires.is_some_and(|expires| moment(expires) <= now) {
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

/// the moment `seconds` after 1970 is.
fn moment(seconds: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
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
mod tests;
