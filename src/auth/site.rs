//! a website's own credential, for a host no account of its own covers: a token, a header or
//! cookies a person saved with `fetchloom auth login <host>`. it is kept in fetchloom's key store
//! under `site:<host>` and goes to that host alone, as [`crate::net::site`] sends it.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{self, HeaderName};
use serde::{Deserialize, Serialize};

use super::{Origin, store};
use crate::error::{Error, Kind};
use crate::machine::Var;
use crate::net::site::Site;

/// how the key store's ids of websites start.
const PREFIX: &str = "site:";

/// header names a saved header may not take: those that frame a request or belong to one
/// connection, which a server or a proxy would misread, cookies, which `--cookies` keeps by
/// cookie rules, and those fetchloom sets itself to resume and check downloads.
const REFUSED: [&str; 22] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "proxy-authenticate",
    "proxy-authorization",
    "expect",
    "cookie",
    "set-cookie",
    "range",
    "if-range",
    "if-match",
    "if-none-match",
    "if-modified-since",
    "if-unmodified-since",
    "accept-encoding",
    "user-agent",
];

/// what is saved for one host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Saved {
    /// sent as `Authorization: Bearer <token>`.
    Token { token: String },
    /// sent as `<name>: <value>`, as an api that wants `X-API-Key` asks.
    Header { name: String, value: String },
    /// sent by cookie rules, to the host alone.
    Cookies { from: Taken, cookies: Vec<Cookie> },
}

/// where saved cookies came from, so a refusal says how to take them again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Taken {
    /// a cookies.txt file, by its full path.
    File(PathBuf),
    /// a browser's profile, as `--browser` named it, such as `firefox`.
    Browser(String),
}

/// one cookie as a browser keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cookie {
    pub(crate) name: String,
    pub(crate) value: String,
    /// the domain it was set for, `.example.org` when it also covers the hosts below it.
    pub(crate) domain: String,
    pub(crate) path: String,
    pub(crate) secure: bool,
    /// seconds since 1970, or none for a cookie that ends with the browser's session.
    pub(crate) expires: Option<i64>,
}

/// the last second of year 9999, which every system's clock can hold, while a later moment, as
/// one past year 30828 on windows, overflows it.
const LATEST: u64 = 253_402_300_799;

/// the moment `seconds` after 1970 is: 1970 itself for a time before it, and the last second of
/// year 9999 for one past that, so a cookie set to last for ever does.
pub(crate) fn moment(seconds: i64) -> SystemTime {
    let seconds = u64::try_from(seconds).unwrap_or(0);
    UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .filter(|_| seconds <= LATEST)
        .unwrap_or_else(|| UNIX_EPOCH + Duration::from_secs(LATEST))
}

/// `host` without the `:port` a site's host may end with.
pub(crate) fn without_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((name, port))
            if port.chars().all(|c| c.is_ascii_digit())
                && (name.ends_with(']') || !name.contains(':')) =>
        {
            name
        }
        _ => host,
    }
}

impl Saved {
    /// the kind, as `fetchloom auth --json` names it.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Saved::Token { .. } => "token",
            Saved::Header { .. } => "header",
            Saved::Cookies { .. } => "cookies",
        }
    }

    /// what is saved, as a line or a refusal names it: "the token", "the X-API-Key header" or
    /// "the cookies".
    pub(crate) fn what(&self) -> String {
        match self {
            Saved::Token { .. } => "the token".to_owned(),
            Saved::Header { name, .. } => format!("the {name} header"),
            Saved::Cookies { .. } => "the cookies".to_owned(),
        }
    }

    /// the command that saves a new one for `host`, quoted for a shell.
    pub(crate) fn command(&self, host: &str) -> String {
        let quoted = |word: &str| crate::suggested::shell_word(word).into_owned();
        let how = match self {
            Saved::Token { .. } => "--token".to_owned(),
            Saved::Header { name, .. } => format!("--header {}", quoted(name)),
            Saved::Cookies {
                from: Taken::File(path),
                ..
            } => format!("--cookies {}", quoted(&path.display().to_string())),
            Saved::Cookies {
                from: Taken::Browser(browser),
                ..
            } => format!("--browser {}", quoted(browser)),
        };
        format!("fetchloom auth login {} {how}", quoted(host))
    }

    /// how to save a new one for `host`, as a refusal's help says it.
    pub(crate) fn again(&self, host: &str) -> String {
        let command = self.command(host);
        match self {
            Saved::Token { .. } | Saved::Header { .. } => {
                format!("save a current one with `{command}`")
            }
            Saved::Cookies {
                from: Taken::Browser(browser),
                ..
            } => format!(
                "sign in to {} in {} again, then run `{command}`",
                without_port(host),
                browser_name(browser)
            ),
            Saved::Cookies { .. } => format!(
                "sign in to {} in your browser again and export its cookies to the same file, then run `{command}`",
                without_port(host)
            ),
        }
    }

    /// when the soonest of the saved cookies expires, in seconds since 1970: none for a token, a
    /// header, or cookies that all end with the browser's session.
    pub(crate) fn soonest(&self) -> Option<i64> {
        match self {
            Saved::Cookies { cookies, .. } => {
                cookies.iter().filter_map(|cookie| cookie.expires).min()
            }
            Saved::Token { .. } | Saved::Header { .. } => None,
        }
    }

    /// the credential as requests to `host` carry it. none for a header no request can carry.
    pub(crate) fn site(&self, host: &str) -> Option<Site> {
        let (headers, cookies) = match self {
            Saved::Token { token } => (
                vec![(header::AUTHORIZATION, format!("Bearer {}", token.trim()))],
                Vec::new(),
            ),
            Saved::Header { name, value } => (
                vec![(HeaderName::from_bytes(name.as_bytes()).ok()?, value.clone())],
                Vec::new(),
            ),
            Saved::Cookies { cookies, .. } => (
                Vec::new(),
                cookies
                    .iter()
                    .map(|cookie| crate::net::site::Cookie {
                        name: cookie.name.clone(),
                        value: cookie.value.clone(),
                        path: cookie.path.clone(),
                        secure: cookie.secure,
                        expires: cookie.expires.map(moment),
                    })
                    .collect(),
            ),
        };
        Site::new(host, headers, cookies, self.what(), self.again(host))
    }
}

/// a browser's name as people write it, from what `--browser` took: `firefox:work` is firefox.
fn browser_name(browser: &str) -> &str {
    browser.split_once(':').map_or(browser, |(name, _)| name)
}

/// the host a website's credential is saved for, from what a person typed: a host name or an
/// address, with `:port` when that is not 443. none for a link, a login or anything else.
pub(crate) fn host(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.contains(['/', '\\', '@', '?', '#', '%']) {
        return None;
    }
    super::dataverse_host(typed)
}

/// the header a saved key goes in, from the name a person typed, or why no key may go in it.
pub(crate) fn header_name(typed: &str) -> Result<HeaderName, Error> {
    // what was typed may hold the key itself, as `X-API-Key: sk_…` does, so none of it is told
    let name = HeaderName::from_bytes(typed.trim().as_bytes()).map_err(|_| {
        Error::new(
            Kind::Usage,
            "--header takes a header's name alone, which holds no `:`, space or other character a name cannot",
        )
        .help("give the name, as `--header X-API-Key`, and type its value when asked, where it stays hidden; a script pipes it in with --with-token")
    })?;
    if name.as_str() == "cookie" {
        return Err(
            Error::new(Kind::Usage, "--header does not take Cookie").help(
                "save a site's cookies with `--cookies <file>` or `--browser <name>`, which keep each cookie's path, secure flag and expiry",
            ),
        );
    }
    if REFUSED.contains(&name.as_str()) {
        return Err(Error::new(
            Kind::Usage,
            format!("--header does not take {typed}, which fetchloom or HTTP itself sets on each request"),
        )
        .help("name the header the site reads your key from, such as X-API-Key or Authorization"));
    }
    Ok(name)
}

/// the key store's id for `host`.
fn id(host: &str) -> String {
    format!("{PREFIX}{host}")
}

/// every host with a saved credential, in order.
pub(crate) fn hosts(var: Var<'_>) -> Vec<String> {
    store::saved(var)
        .into_keys()
        .filter_map(|id| id.strip_prefix(PREFIX).map(str::to_owned))
        .collect()
}

/// what is saved for `host`, and where. none when nothing is, or what is cannot be read.
pub(crate) fn find(var: Var<'_>, host: &str) -> Option<(Saved, Origin)> {
    let id = id(host);
    let place = *store::saved(var).get(&id)?;
    let text = store::read(var, &id, place)?;
    let saved = serde_json::from_str(&text).ok()?;
    Some((saved, origin(var, place)))
}

/// where a key in `place` is, as `auth` names it.
fn origin(var: Var<'_>, place: store::Place) -> Origin {
    match place {
        store::Place::Keyring => Origin::Keyring,
        store::Place::File => Origin::File(
            crate::machine::settings_folder(var)
                .unwrap_or_default()
                .join("credentials.json"),
        ),
    }
}

/// the most bytes windows' credential store keeps for an entry, `CRED_MAX_CREDENTIAL_BLOB_SIZE`.
const WINDOWS_ENTRY: usize = 2_560;

/// whether `saved` outgrows windows' credential store, which keeps [`WINDOWS_ENTRY`] bytes an
/// entry and takes the saved text as UTF-16, two bytes a unit.
pub(crate) fn too_big_for_windows(saved: &Saved) -> bool {
    serde_json::to_string(saved)
        .is_ok_and(|text| text.encode_utf16().count().saturating_mul(2) > WINDOWS_ENTRY)
}

/// saves `saved` for `host` in `place`, in place of what was saved for it before. returns
/// where it went.
///
/// # Errors
///
/// when the store or the file cannot take it, as a credential store that holds keys of a
/// limited length refuses a long one.
pub(crate) fn save(
    var: Var<'_>,
    host: &str,
    saved: &Saved,
    place: store::Place,
) -> std::io::Result<Origin> {
    let text = serde_json::to_string(saved).map_err(std::io::Error::other)?;
    store::save(var, &id(host), &text, place)?;
    Ok(origin(var, place))
}

/// removes what is saved for `host`. returns where it was, or none when nothing was.
///
/// # Errors
///
/// when the store or the file cannot be changed.
pub(crate) fn forget(var: Var<'_>, host: &str) -> std::io::Result<Option<Origin>> {
    let id = id(host);
    let Some(place) = store::saved(var).get(&id).copied() else {
        return Ok(None);
    };
    store::forget(var, &id)?;
    Ok(Some(origin(var, place)))
}

/// every website's saved credential, as requests carry them: each to its own host alone.
pub(crate) fn sites(var: Var<'_>) -> Vec<Site> {
    hosts(var)
        .into_iter()
        .filter_map(|host| {
            let (saved, _) = find(var, &host)?;
            saved.site(&host)
        })
        .collect()
}

#[cfg(test)]
mod tests;
