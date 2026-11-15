//! where each file data.lock pins can still be had, downloading none: on a mirror, a folder mirror's
//! copy of its size or another mirror's object answered at its size, else at its source, asked for
//! its first byte through a download's own path. requests run a bounded number at a time, hosts in
//! turn, under the per-host limits downloads keep, and a host that gives no answer is asked no more.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinSet;
use url::Url;

use super::Context;
use super::fetch::Queue;
use super::fetch::mirrors::{self, Look};
use crate::error::{Error, Kind};
use crate::mirror::Wanted;
use crate::net::{Expect, Fetch, Net, Probed};
use crate::project::lock::LockedFile;

/// copies looked at on one folder mirror at once, as sync copies from one.
const LOOKS: usize = 8;

/// a file data.lock pins, to look for.
pub(in crate::cli) struct Pinned<'a> {
    pub(in crate::cli) dataset: &'a str,
    pub(in crate::cli) file: &'a LockedFile,
    /// whether data.toml asks for its link's web page, so a page is the file.
    pub(in crate::cli) page: bool,
}

impl Pinned<'_> {
    /// the name a person knows it by, as `iris/iris.csv`.
    pub(in crate::cli) fn label(&self) -> String {
        format!("{}/{}", self.dataset, self.file.path)
    }

    fn wanted(&self) -> Wanted {
        Wanted {
            digest: self.file.sha256,
            size: self.file.size,
            label: self.label(),
        }
    }

    /// whether a web page may be the file: data.toml asks for one, or its name says it is one.
    fn takes_page(&self) -> bool {
        self.page
            || std::path::Path::new(&self.file.path)
                .extension()
                .is_some_and(|extension| {
                    ["htm", "html", "xhtml"]
                        .iter()
                        .any(|page| extension.eq_ignore_ascii_case(page))
                })
    }

    /// the request for its first byte at its source, as a sync asks for the file, or why it has
    /// none: an import names no link, and a broken link names nothing.
    fn request(&self) -> Result<Fetch, Lack> {
        if self.file.url.is_empty() {
            return Err(Lack::Imported);
        }
        let url = Url::parse(&self.file.url).map_err(|err| {
            Lack::Unknown(Error::new(
                Kind::ProjectInvalid,
                format!("data.lock has a broken link for {}: {err}", self.label()),
            ))
        })?;
        Ok(Fetch {
            expect: Expect {
                sha256: Some(self.file.sha256),
                md5: self.file.md5,
                size: Some(self.file.size),
            },
            page: self.page,
            ..crate::source::fetch_plan(&url)
        })
    }
}

/// where a pinned file can still be had, or why nowhere.
#[derive(Debug)]
pub(in crate::cli) enum Found {
    /// the mirror at this place in the run's list holds it at its size.
    Mirror(usize),
    /// its source serves it at its size.
    Source,
    Nowhere(Lack),
}

/// why no mirror or source serves a pinned file.
#[derive(Debug)]
pub(in crate::cli) enum Lack {
    /// its source serves a file of `size` bytes in its place, at `host`.
    Changed { size: u64, host: String },
    /// its source has nothing at its link, as 404 or 410 says.
    Gone(Error),
    /// its source refused it, as 401 or 403 says, with the sign-in hint a download gives.
    Refused(Error),
    /// no answer after the retries, a web page where the file was expected, or another answer.
    Unknown(Error),
    /// it was imported from files, and no mirror holds it, so no source can give it.
    Imported,
}

/// how a folder mirror's copy counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum Folders {
    /// a copy of the pinned size, unread.
    Sized,
    /// not at all, as for a caller that hashed the copies itself.
    Skipped,
}

/// what `probed`, the answer at `url`, says of a file pinned at `size` bytes: served, or why
/// not. a web page is the file only where `takes_page` says one may be.
pub(in crate::cli) fn judged(
    probed: Probed,
    size: u64,
    takes_page: bool,
    url: &Url,
) -> Result<(), Lack> {
    let host = crate::net::host(url);
    match probed {
        Probed::File { page: true, .. } if !takes_page => Err(Lack::Unknown(
            Error::new(
                Kind::SourceInvalid,
                format!("{host} answered with a web page, not the file"),
            )
            .help("open the link in a browser: it may ask for a sign-in, or lead to a page about the file"),
        )),
        Probed::File {
            total: Some(total), ..
        } if total == size => Ok(()),
        Probed::File {
            total: Some(total), ..
        } => Err(Lack::Changed {
            size: total,
            host: host.to_owned(),
        }),
        Probed::File { total: None, .. } => Err(Lack::Unknown(Error::new(
            Kind::SourceInvalid,
            format!("{host} answered without saying the file's size"),
        ))),
        Probed::Answered {
            status: 404 | 410,
            error,
        } => Err(Lack::Gone(error)),
        Probed::Answered {
            status: 401 | 403,
            error,
        } => Err(Lack::Refused(error)),
        // a source's own words for a refusal or a missing file, as a sign-in page or drive's answer
        Probed::Answered { error, .. } if error.kind == Kind::AuthRequired => {
            Err(Lack::Refused(error))
        }
        Probed::Answered { error, .. } if error.kind == Kind::ReferenceUnresolved => {
            Err(Lack::Gone(error))
        }
        Probed::Answered { error, .. } | Probed::Unanswered(error) => Err(Lack::Unknown(error)),
    }
}

/// where each of `files` can still be had, in order: on the first of the run's mirrors that holds
/// it, asked in the order sync asks them, with folder mirrors as `folders` says, else at its
/// source. fails at a stop.
pub(in crate::cli) async fn find(
    context: &Context,
    net: &Arc<Net>,
    files: &[Pinned<'_>],
    folders: Folders,
) -> Result<Vec<Found>, Error> {
    let mut found: Vec<Option<Found>> = std::iter::repeat_with(|| None).take(files.len()).collect();
    let left = |found: &[Option<Found>]| -> Vec<usize> {
        (0..found.len()).filter(|at| found[*at].is_none()).collect()
    };
    for index in context.mirrors.up(context, net).await? {
        let asked = left(&found);
        if asked.is_empty() {
            break;
        }
        let held: Vec<bool> = match mirrors::look(&context.mirrors.mirrors.list[index]) {
            Look::Folder(_) if folders == Folders::Skipped => continue,
            Look::Folder(root) => tokio::task::block_in_place(|| {
                crate::place::run_all(&asked, context.threads.at_most(LOOKS), |at| {
                    let file = files[*at].file;
                    crate::mirror::folder::has(root, &file.sha256, file.size)
                })
            }),
            Look::Ask(request) => {
                let probed = each(context, net, &asked, |at| {
                    Some(request(&files[at].wanted()))
                })
                .await?;
                asked
                    .iter()
                    .zip(probed)
                    .map(|(at, probed)| {
                        matches!(probed, Some(Probed::File { total: Some(total), page: false })
                            if total == files[*at].file.size)
                    })
                    .collect()
            }
        };
        for (at, held) in asked.into_iter().zip(held) {
            if held {
                found[at] = Some(Found::Mirror(index));
            }
        }
    }
    let mut asked = Vec::new();
    for at in left(&found) {
        match files[at].request() {
            Ok(_) => asked.push(at),
            Err(lack) => found[at] = Some(Found::Nowhere(lack)),
        }
    }
    let probed = each(context, net, &asked, |at| files[at].request().ok()).await?;
    for (at, probed) in asked.into_iter().zip(probed) {
        let pinned = &files[at];
        let judgement = match (probed, Url::parse(&pinned.file.url)) {
            (Some(probed), Ok(url)) => judged(probed, pinned.file.size, pinned.takes_page(), &url),
            _ => Err(Lack::Unknown(Error::new(
                Kind::LocalIo,
                format!("{} was not asked", pinned.label()),
            ))),
        };
        found[at] = Some(match judgement {
            Ok(()) => Found::Source,
            Err(lack) => Found::Nowhere(lack),
        });
    }
    Ok(found
        .into_iter()
        .map(|found| {
            found.unwrap_or_else(|| {
                Found::Nowhere(Lack::Unknown(Error::new(Kind::LocalIo, "not asked")))
            })
        })
        .collect())
}

/// what each later request to `from`'s origin gets in place of an answer, once `error` ended one
/// there before any came, when it would end them too: a refusal to ask the origin is theirs as
/// well, and after no answer, or a name DNS cannot find, asking again would wait for nothing.
fn silenced(error: &Error, from: &Url) -> Option<Error> {
    match error.kind {
        Kind::NetworkRefused | Kind::OfflineMissing => Some(error.clone()),
        Kind::NetworkUnreachable | Kind::NetworkRateLimited | Kind::NetworkStatus => {
            let unasked = Error::new(
                error.kind,
                format!(
                    "not asked, since {} gave no answer for another file",
                    crate::net::host(from)
                ),
            );
            Some(match error.help.as_deref() {
                Some(help) => unasked.help(help),
                None => unasked,
            })
        }
        _ => None,
    }
}

/// the answer to the request `request` makes for each of `items`, in their order, asked a bounded
/// number at a time with their hosts taking turns, each request made when its turn comes; an item
/// without one is not asked. once an origin gives no answer, the rest of its items are not asked
/// and share its error. fails at a stop.
async fn each(
    context: &Context,
    net: &Arc<Net>,
    items: &[usize],
    request: impl Fn(usize) -> Option<Fetch>,
) -> Result<Vec<Option<Probed>>, Error> {
    let mut answers: Vec<Option<Probed>> =
        std::iter::repeat_with(|| None).take(items.len()).collect();
    let mut queue = Queue::grouped((0..items.len()).collect(), |_, at: &usize| {
        request(items[*at]).map_or_else(String::new, |fetch| {
            crate::net::host(&fetch.from).to_ascii_lowercase()
        })
    });
    let silent: Arc<Mutex<HashMap<String, Error>>> = Arc::default();
    // more tasks than request slots keep every slot busy, and few enough keep memory flat
    let in_flight = context.settings.jobs.value.max(1) * 2;
    let mut tasks = JoinSet::new();
    loop {
        while tasks.len() < in_flight
            && let Some((_, at)) = queue.next()
        {
            let Some(fetch) = request(items[at]) else {
                continue;
            };
            let (net, silent) = (Arc::clone(net), Arc::clone(&silent));
            tasks.spawn(async move {
                let origin = fetch.from.origin().ascii_serialization();
                let known = silent
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&origin)
                    .cloned();
                if let Some(error) = known {
                    return (at, Ok(Probed::Unanswered(error)));
                }
                let probed = net.probe(&fetch).await;
                if let Ok(Probed::Unanswered(error)) = &probed
                    && let Some(unasked) = silenced(error, &fetch.from)
                {
                    silent
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .entry(origin)
                        .or_insert(unasked);
                }
                (at, probed)
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        match joined {
            Ok((at, Ok(probed))) => answers[at] = Some(probed),
            Ok((_, Err(error))) => {
                tasks.abort_all();
                return Err(error);
            }
            Err(err) => {
                tasks.abort_all();
                return Err(Error::new(
                    Kind::LocalIo,
                    format!("a check of a source stopped unexpectedly: {err}"),
                ));
            }
        }
    }
    Ok(answers)
}

#[cfg(test)]
mod tests;
