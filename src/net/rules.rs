//! what sources say about requests to their hosts, which the network follows without knowing any
//! source by name. `source::rules` builds the one a run uses.

use std::future::Future;
use std::pin::Pin;

use url::Url;

use super::Net;
use super::google::DriveId;
use crate::error::Error;

/// the answer of a rule that asks the network again before it can say.
pub(crate) type Asking<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// the sources' hooks, each called where a source knows more than the network.
#[derive(Clone, Copy)]
pub(crate) struct Rules {
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
            refused_download: |_, _| None,
            drive_search: unsearched,
            drive_name: unnamed,
        }
    }
}

fn unsearched<'a>(_: &'a Net, _: &'a DriveId) -> Asking<'a, ()> {
    Box::pin(std::future::ready(Ok(())))
}

fn unnamed<'a>(_: &'a Net, _: &'a str) -> Asking<'a, String> {
    Box::pin(std::future::ready(Ok(String::new())))
}
