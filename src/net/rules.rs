//! what sources say about requests to their hosts, which the network follows without knowing any
//! source by name. `source::rules` builds the one a run uses.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use url::Url;

use super::Net;
use super::google::DriveId;
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
    /// a source's own error for a download it refused with a status, when it says more than the
    /// status does.
    pub(crate) refused_download: fn(&Url, u16) -> Option<Error>,
    /// lists a folder `drive_folders` names through drive's api with a token, so the network
    /// hears which items it holds.
    pub(crate) drive_search: for<'a> fn(&'a Net, &'a DriveId) -> Asking<'a, ()>,
    /// the name drive's api gives a file, by its id.
    pub(crate) drive_name: for<'a> fn(&'a Net, &'a str) -> Asking<'a, String>,
}

impl Default for Rules {
    /// no source's rules, as a network a test makes has.
    fn default() -> Rules {
        Rules {
            paced_as: |_| None,
            spacing: |_| None,
            most_at_once: |_| None,
            refused_download: |_, _| None,
            drive_search: unsearched,
            drive_name: unnamed,
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
