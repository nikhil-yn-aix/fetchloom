//! what sources say about requests to their hosts, which the network follows without knowing any
//! source by name. `source::rules` builds the one a run uses.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use url::Url;

use super::google::DriveId;
use super::{Net, Response};
use crate::account::Account;
use crate::error::Error;

/// the answer of a rule that asks the network again before it can say.
pub(crate) type Asking<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// the sources' hooks, each called where a source knows more than the network.
#[derive(Clone, Copy)]
pub(crate) struct Rules {
    /// the name a host's requests are paced and counted under, when its source limits it
    /// together with other hosts, as arXiv's terms do its hosts.
    pub(crate) paced_as: fn(&str) -> Option<&'static str>,
    /// how far apart requests paced under a name start, for hosts that ask for that, as NCBI's,
    /// arXiv's and semantic scholar's do.
    pub(crate) spacing: fn(&str) -> Option<Duration>,
    /// how many requests paced under a name may run at once, for hosts that ask for fewer than
    /// the network allows.
    pub(crate) most_at_once: fn(&str) -> Option<usize>,
    /// the request for a fresh link to a file whose source links it only for a while, by the
    /// stable link data.lock keeps.
    pub(crate) relink: fn(&Url) -> Option<Box<dyn Relink>>,
    /// whether a link such a source gave expires before a moment, or too soon after it to start
    /// a download on.
    pub(crate) link_expired: fn(&Url, SystemTime) -> bool,
    /// a source's own error for a download it refused with a status, when it says more than the
    /// status does.
    pub(crate) refused_download: fn(&Url, u16) -> Option<Error>,
    /// what to do about the git-lfs pointer a link answered with, in its source's words: where it
    /// serves the file the pointer stands for, or why fetchloom cannot download that file.
    pub(crate) lfs_help: fn(&Url) -> Option<String>,
    /// lists a folder `drive_folders` names through drive's api with a token, so the network
    /// hears which items it holds.
    pub(crate) drive_search: for<'a> fn(&'a Net, &'a DriveId) -> Asking<'a, ()>,
    /// the name drive's api gives a file, by its id.
    pub(crate) drive_name: for<'a> fn(&'a Net, &'a str) -> Asking<'a, String>,
    /// the account whose source answered from a host, for a refusal that names no account.
    pub(crate) account_at: fn(&str) -> Option<Account>,
    /// a source's words for a rate limit that stopped a request to a link sent with no key: the
    /// key that lifts it, as gitlab's for its servers, which limit requests without one by
    /// address.
    pub(crate) keyless_limit: fn(&Url, Error) -> Error,
    /// the media type a source serves a link's bytes in, which every request for the link asks
    /// for: github's api answers a blob or a release asset with json about it otherwise.
    pub(crate) accept: fn(&Url) -> Option<&'static str>,
    /// a source's refusal of a link it serves only with its key, given before any request for it
    /// goes out without one, as github's api shows a private repository's files to a key alone.
    pub(crate) keyless: fn(&Url) -> Option<Error>,
}

/// a file a source links only for a while, by the stable link data.lock keeps: the request that
/// lists a fresh link to it, and how that link is found in the answer.
pub(crate) trait Relink: Send + Sync {
    /// the request whose answer lists a fresh link.
    fn asked(&self) -> &Url;

    /// the fresh link to the file among those `answer` lists, or why there is none.
    ///
    /// # Errors
    ///
    /// the failed request, worded for its source, or the file missing from the listing.
    fn found(&self, answer: Result<Response, Error>) -> Result<Url, Error>;

    /// the error for a fresh link the source gave again once it had expired.
    fn stale(&self) -> Error;
}

impl Default for Rules {
    /// no source's rules, as a network a test makes has.
    fn default() -> Rules {
        Rules {
            paced_as: |_| None,
            spacing: |_| None,
            most_at_once: |_| None,
            relink: |_| None,
            link_expired: |_, _| false,
            refused_download: |_, _| None,
            lfs_help: |_| None,
            drive_search: unsearched,
            drive_name: unnamed,
            account_at: |_| None,
            keyless_limit: |_, error| error,
            accept: |_| None,
            keyless: |_| None,
        }
    }
}

impl Rules {
    /// the name requests to `url`'s host are paced and counted under: the host as
    /// [`super::host`] names it, in lowercase, or the name its source gives it and others.
    pub(crate) fn paced(&self, url: &Url) -> String {
        let host = super::host(url).to_ascii_lowercase();
        (self.paced_as)(&host).map_or(host, str::to_owned)
    }
}

fn unsearched<'a>(_: &'a Net, _: &'a DriveId) -> Asking<'a, ()> {
    Box::pin(std::future::ready(Ok(())))
}

fn unnamed<'a>(_: &'a Net, _: &'a str) -> Asking<'a, String> {
    Box::pin(std::future::ready(Ok(String::new())))
}
