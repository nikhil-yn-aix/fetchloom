//! a pinned file that no cache, mirror or source gives any more is asked of two public archives:
//! software heritage by its SHA-256, then the wayback machine for a capture of its link in
//! data.lock. the pinned hash decides, as for a mirror, so an archive can withhold a file but
//! never change one. neither is asked about a file this machine could have needed a key for, one
//! on a private network, offline or in mirror-only mode.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use url::Url;

use super::Job;
use crate::cache::Cache;
use crate::cli::push::Keys;
use crate::error::{Error, Kind};
use crate::net::{Api as _, Event, Fetch, Fetched, Net};
use crate::output::{Output, Warning};
use crate::settings::Settings;

/// the files one run asks the archives about at most, so a project whose links all died does not
/// send each of them to two archives.
const MOST: usize = 50;

/// software heritage's raw content by SHA-256.
const HERITAGE: &str = "https://archive.softwareheritage.org/api/1/content/sha256:";
/// the wayback machine, whose captures and their index are on one host.
const WAYBACK: &str = "https://web.archive.org";

/// hosts whose links are themselves the key to a file, so one is never sent to an archive.
const SHARED: [&str; 12] = [
    "drive.google.com",
    "docs.google.com",
    "drive.usercontent.google.com",
    "dropbox.com",
    "www.dropbox.com",
    "dl.dropboxusercontent.com",
    "onedrive.live.com",
    "1drv.ms",
    "sharepoint.com",
    "box.com",
    "app.box.com",
    "dl.boxcloud.com",
];

/// what a run may ask the archives, and how many files it asked about.
#[derive(Default)]
pub(in crate::cli) struct Archived {
    /// the `web_archives` setting.
    on: bool,
    /// origins the settings allow on this machine or its network, which no archive is asked about.
    allowed: Vec<url::Origin>,
    /// where this machine keeps a key, read the first time a file is weighed.
    keys: OnceLock<Keys>,
    settings: Option<Box<Settings>>,
    asked: AtomicUsize,
}

/// why no archive is asked about a file.
#[derive(Debug, PartialEq, Eq)]
enum Unasked {
    Off,
    Offline,
    MirrorsOnly,
    /// its host is this machine, its network or an origin the settings allow there.
    Private(String),
    /// this machine keeps a key for its host, so it may not be public.
    Keyed(String),
    /// the run already asked about the most files one run asks about.
    Most,
}

impl Unasked {
    /// the line an error carries for it: none where an archive could never have helped, offline,
    /// mirror-only or for a private host, whose own errors already say what to do.
    fn said(&self) -> Option<String> {
        match self {
            Unasked::Off => Some("no web archive was asked, since web_archives is off: `fetchloom settings set web_archives true` turns it on".to_owned()),
            Unasked::Keyed(host) => Some(format!(
                "no web archive was asked, since this machine keeps a key for {host}, so the file may not be public"
            )),
            Unasked::Most => Some(format!(
                "no web archive was asked, since this run already asked about {MOST} files, the most one run asks about"
            )),
            Unasked::Offline | Unasked::MirrorsOnly | Unasked::Private(_) => None,
        }
    }
}

impl Archived {
    /// what `settings` let a run ask the archives.
    pub(in crate::cli) fn new(settings: &Settings) -> Archived {
        Archived {
            on: settings.web_archives.value,
            allowed: settings.allowed_origins.value.clone(),
            settings: Some(Box::new(settings.clone())),
            ..Archived::default()
        }
    }

    /// why no archive may be asked about the file at `link`, if none may: the setting, offline,
    /// mirror-only mode, a private host or one this machine keeps a key for, then the run's cap.
    fn unasked(&self, link: &Url, offline: bool, only: bool) -> Option<Unasked> {
        if !self.on {
            return Some(Unasked::Off);
        }
        if offline {
            return Some(Unasked::Offline);
        }
        if only {
            return Some(Unasked::MirrorsOnly);
        }
        let host = link.host_str().unwrap_or_default().to_ascii_lowercase();
        if is_private(link) || self.allowed.contains(&link.origin()) {
            return Some(Unasked::Private(host));
        }
        let keys = self.keys.get_or_init(|| {
            self.settings
                .as_deref()
                .map_or_else(Keys::default, Keys::read)
        });
        if keys.signs_in(link.as_str()) {
            return Some(Unasked::Keyed(host));
        }
        if self.asked.fetch_add(1, Ordering::Relaxed) >= MOST {
            return Some(Unasked::Most);
        }
        None
    }
}

/// whether `link` names this machine or a private network: an address of one, `localhost` or a
/// name such a network uses.
fn is_private(link: &Url) -> bool {
    match link.host() {
        Some(url::Host::Ipv4(ip)) => crate::net::is_private(ip.into()),
        Some(url::Host::Ipv6(ip)) => crate::net::is_private(ip.into()),
        Some(url::Host::Domain(name)) => {
            let name = name.to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost") || crate::net::looks_internal(&name)
        }
        None => true,
    }
}

/// whether the wayback machine may be sent `link`: never one with a query, which may hold a key,
/// nor a share link, which is the key itself.
fn wayback_may_see(link: &Url) -> bool {
    let host = link.host_str().unwrap_or_default().to_ascii_lowercase();
    let shared = SHARED.iter().any(|known| {
        host.strip_suffix(known)
            .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'))
    });
    link.query().is_none() && link.username().is_empty() && link.password().is_none() && !shared
}

/// the file `job` pins from software heritage, else from the wayback machine's first capture of
/// its link since `changed`, when `failed`, the source's own failure, says the file is gone or
/// changed there and the archives may be asked. what they said joins `failed` otherwise.
#[expect(
    clippy::too_many_arguments,
    reason = "each is one part of the download the archives stand in for"
)]
pub(super) async fn recover(
    net: &Net,
    cache: &Cache,
    output: &Output,
    archived: &Archived,
    only: bool,
    job: &Job,
    failed: Error,
    on: &(dyn Fn(Event) -> Result<(), Error> + Sync),
) -> Result<Fetched, Error> {
    let Some(digest) = job.fetch.expect.sha256 else {
        return Err(failed);
    };
    if !matches!(
        failed.kind,
        Kind::ReferenceUnresolved | Kind::IntegrityMismatch | Kind::NetworkUnreachable
    ) {
        return Err(failed);
    }
    let link = &job.fetch.url;
    if let Some(unasked) = archived.unasked(link, net.offline().enabled(), only) {
        return Err(match unasked.said() {
            Some(said) => failed.detail(said),
            None => failed,
        });
    }
    on(Event::Moved)?;
    let mut said = Vec::new();
    let heritage = Url::parse(&format!("{HERITAGE}{}/raw/", digest.hex()))
        .map_err(|err| Error::new(Kind::LocalIo, err.to_string()))?;
    match from(net, cache, job, heritage, on).await {
        Ok(fetched) => {
            told(output, job, "software heritage");
            return Ok(fetched);
        }
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        Err(error) => said.push(format!("software heritage: {}", lacked(&error))),
    }
    if wayback_may_see(link) {
        match capture(
            net,
            link,
            job.again.as_ref().map(|again| again.changed.as_str()),
        )
        .await
        {
            Ok(Some(capture)) => match from(net, cache, job, capture, on).await {
                Ok(fetched) => {
                    told(output, job, "the wayback machine");
                    return Ok(fetched);
                }
                Err(error) if error.kind == Kind::Interrupted => return Err(error),
                Err(error) => said.push(format!("the wayback machine: {}", lacked(&error))),
            },
            Ok(None) => said.push(format!(
                "the wayback machine: no capture of {} that answered 200",
                crate::shown::Shown(link)
            )),
            Err(error) if error.kind == Kind::Interrupted => return Err(error),
            Err(error) => said.push(format!("the wayback machine: {}", error.message)),
        }
    } else {
        said.push(
            "the wayback machine was not asked, since the link holds a query or is a share link"
                .to_owned(),
        );
    }
    Err(said.into_iter().fold(failed, Error::detail))
}

/// what an archive's failure says of the file, in short.
fn lacked(error: &Error) -> String {
    match error.kind {
        Kind::ReferenceUnresolved => "not there".to_owned(),
        Kind::IntegrityMismatch => "a copy with other bytes, so it was not used".to_owned(),
        _ => error.message.clone(),
    }
}

/// says that `job`'s file came from `archive`.
fn told(output: &Output, job: &Job, archive: &str) {
    output.warn(
        Warning::SourceArchived,
        &format!(
            "{} is gone from its link in data.lock or changed there, so it came from {archive}, whose copy hashes to its pin",
            job.label
        ),
    );
}

/// `job`'s file downloaded from `url`, an archive's copy, checked against its pin.
async fn from(
    net: &Net,
    cache: &Cache,
    job: &Job,
    url: Url,
    on: &(dyn Fn(Event) -> Result<(), Error> + Sync),
) -> Result<Fetched, Error> {
    let fetch = Fetch {
        expect: job.fetch.expect,
        revalidate: false,
        unpack: false,
        label: job.fetch.label.clone(),
        ..crate::source::fetch_plan(&url)
    };
    net.download(&fetch, cache, on).await
}

/// the raw capture the wayback machine keeps of `link` that answered 200: the first since
/// `changed`, when the dataset was pinned, else the last before it, or the last of all when the
/// time is unknown. the index is asked through its documented `from`, `to` and `limit`.
async fn capture(net: &Net, link: &Url, changed: Option<&str>) -> Result<Option<Url>, Error> {
    let asks = match changed.and_then(digits) {
        Some(at) => vec![("from", at.clone(), "1"), ("to", at, "-1")],
        None => vec![("", String::new(), "-1")],
    };
    for (bound, at, limit) in asks {
        let mut index = Url::parse(&format!("{WAYBACK}/cdx/search/cdx"))
            .map_err(|err| Error::new(Kind::LocalIo, err.to_string()))?;
        {
            let mut query = index.query_pairs_mut();
            query
                .append_pair("url", link.as_str())
                .append_pair("output", "json")
                .append_pair("fl", "timestamp")
                .append_pair("filter", "statuscode:200");
            if !bound.is_empty() {
                query.append_pair(bound, &at);
            }
            query.append_pair("limit", limit);
        }
        let answer = net.get(&index).await?;
        if answer.status != 200 || answer.body.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let rows: Vec<Vec<String>> = answer.json()?;
        if let Some(stamp) = rows
            .iter()
            .skip(1)
            .find_map(|row| row.first().filter(|stamp| is_stamp(stamp)))
        {
            return Url::parse(&format!("{WAYBACK}/web/{stamp}id_/{link}"))
                .map(Some)
                .map_err(|err| Error::new(Kind::LocalIo, err.to_string()));
        }
    }
    Ok(None)
}

/// a wayback timestamp: 14 digits, `yyyymmddhhmmss`.
fn is_stamp(text: &str) -> bool {
    text.len() == 14 && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// `changed`, a time as data.lock writes it, `2024-01-02T03:04:05Z`, as the wayback machine's
/// digits.
fn digits(changed: &str) -> Option<String> {
    let digits: String = changed
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    (digits.len() >= 8).then_some(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn asking() -> Archived {
        Archived {
            on: true,
            allowed: vec![url("http://lab.example:8080").origin()],
            keys: OnceLock::from(Keys::default()),
            ..Archived::default()
        }
    }

    /// a link on this machine, a private network, or an origin the settings allow there names a
    /// file no archive may hold, and a run asks about at most [`MOST`] files.
    #[test]
    fn archives_are_asked_only_about_public_files_and_at_most_so_many() {
        let archived = asking();
        for (link, unasked) in [
            ("http://127.0.0.1:8000/a.csv", "127.0.0.1"),
            ("http://[::1]/a.csv", "[::1]"),
            ("http://10.1.2.3/a.csv", "10.1.2.3"),
            ("http://localhost/a.csv", "localhost"),
            ("http://intranet/a.csv", "intranet"),
            ("http://files.corp.internal/a.csv", "files.corp.internal"),
            ("http://lab.example:8080/a.csv", "lab.example"),
        ] {
            assert_eq!(
                archived.unasked(&url(link), false, false),
                Some(Unasked::Private(unasked.to_owned())),
                "{link}"
            );
        }
        let public = url("https://data.example.org/a.csv");
        assert_eq!(
            archived.unasked(&public, true, false),
            Some(Unasked::Offline)
        );
        assert_eq!(
            archived.unasked(&public, false, true),
            Some(Unasked::MirrorsOnly)
        );
        let off = Archived {
            on: false,
            ..asking()
        };
        assert_eq!(off.unasked(&public, false, false), Some(Unasked::Off));
        for _ in 0..MOST {
            assert_eq!(archived.unasked(&public, false, false), None);
        }
        assert_eq!(archived.unasked(&public, false, false), Some(Unasked::Most));
    }

    /// a link holding a query may hold a key, and a share link is one, so neither is sent.
    #[test]
    fn the_wayback_machine_never_sees_a_query_or_a_share_link() {
        for (link, sent) in [
            ("https://data.example.org/files/iris.csv", true),
            ("https://data.example.org/get?id=7", false),
            ("https://data.example.org/files/iris.csv?token=x", false),
            ("https://www.dropbox.com/s/abc/iris.csv", false),
            ("https://dl.dropboxusercontent.com/s/abc/iris.csv", false),
            ("https://drive.google.com/file/d/abc/view", false),
            ("https://lab.sharepoint.com/sites/x/iris.csv", false),
            ("https://lab.app.box.com/s/abc", false),
            ("https://onedrive.live.com/download/abc", false),
            ("https://user:pass@data.example.org/iris.csv", false),
            ("https://notdropbox.com/iris.csv", true),
        ] {
            assert_eq!(wayback_may_see(&url(link)), sent, "{link}");
        }
    }

    #[test]
    fn the_time_data_lock_writes_becomes_wayback_digits() {
        assert_eq!(
            digits("2024-01-02T03:04:05Z").as_deref(),
            Some("20240102030405")
        );
        assert_eq!(digits("2024-01-02").as_deref(), Some("20240102"));
        assert_eq!(digits(""), None);
        assert!(is_stamp("20240102030405"));
        assert!(!is_stamp("2024010203040"));
    }
}
