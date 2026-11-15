//! where each file data.lock pins can still be had, downloading none: at its source, asked for its
//! first byte through a download's own path, or on a mirror, a folder mirror's copy of its size or
//! another mirror's object answered at its size. requests run a bounded number at a time, hosts in
//! turn, under the per-host limits downloads keep, and a host that gives no answer is asked no more.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::task::JoinSet;
use url::Url;

use super::Context;
use super::fetch::Queue;
use super::fetch::mirrors::Look;
use crate::digest::{self, Md5, Sha256};
use crate::error::{Error, Kind};
use crate::mirror::{Wanted, folder, web};
use crate::net::{Expect, Fetch, Net, Probed};
use crate::project::lock::{LockedFile, StatedFile};

/// copies looked at on one folder mirror at once, as sync copies from one.
const LOOKS: usize = 8;

/// what to do when files could not be checked and none gave a hint of its own.
pub(in crate::cli) const AGAIN: &str =
    "ask again later: a source that gave no answer may be busy or down";

/// how soon the trouble of `kind` that kept a file from being checked should be named: a refusal
/// first, since asking again later does not end it, and no answer last.
pub(in crate::cli) fn urgency(kind: Kind) -> u8 {
    match kind {
        Kind::AuthRequired => 0,
        Kind::NetworkUnreachable
        | Kind::NetworkStatus
        | Kind::NetworkRateLimited
        | Kind::NetworkRefused
        | Kind::OfflineMissing => 2,
        _ => 1,
    }
}

/// a file data.lock pins, to look for.
#[derive(Clone, Copy)]
pub(in crate::cli) struct Pinned<'a> {
    pub(in crate::cli) dataset: &'a str,
    path: &'a str,
    pub(in crate::cli) size: u64,
    /// none for a file only its source's MD5 pins, which no mirror keeps.
    sha256: Option<Sha256>,
    md5: Option<Md5>,
    url: &'a str,
    /// for an archive, the list of the files it unpacks to, which a mirror keeps in its place.
    tree: Option<Sha256>,
    /// whether data.toml asks for its link's web page, so a page is the file.
    page: bool,
}

impl<'a> Pinned<'a> {
    pub(in crate::cli) fn locked(dataset: &'a str, file: &'a LockedFile, page: bool) -> Self {
        Pinned {
            dataset,
            path: &file.path,
            size: file.size,
            sha256: Some(file.sha256),
            md5: file.md5,
            url: &file.url,
            tree: file.unpacks_to,
            page,
        }
    }

    /// a file its source's digests pin before a sync downloads it.
    pub(in crate::cli) fn stated(dataset: &'a str, file: &'a StatedFile, page: bool) -> Self {
        Pinned {
            dataset,
            path: &file.path,
            size: file.size,
            sha256: file.sha256,
            md5: file.md5,
            url: &file.url,
            tree: None,
            page,
        }
    }

    /// the name a person knows it by, as `iris/iris.csv`.
    pub(in crate::cli) fn label(&self) -> String {
        format!("{}/{}", self.dataset, self.path)
    }

    /// what a mirror keeps it as, when its SHA-256 is pinned.
    fn wanted(&self) -> Option<Wanted> {
        Some(Wanted {
            digest: self.sha256?,
            size: self.size,
            label: self.label(),
        })
    }

    /// whether a web page may be the file: data.toml asks for one, or its name says it is one.
    fn takes_page(&self) -> bool {
        self.page
            || std::path::Path::new(self.path)
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
        if self.url.is_empty() {
            return Err(Lack::Imported);
        }
        let url = Url::parse(self.url).map_err(|err| {
            Lack::Unknown(Error::new(
                Kind::ProjectInvalid,
                format!("data.lock has a broken link for {}: {err}", self.label()),
            ))
        })?;
        Ok(Fetch {
            expect: Expect {
                sha256: self.sha256,
                md5: self.md5,
                size: Some(self.size),
            },
            page: self.page,
            ..crate::source::fetch_plan(&url)
        })
    }
}

/// where a pinned file can still be had, or why nowhere.
#[derive(Debug)]
pub(in crate::cli) enum Found {
    /// the mirror at `index` in the run's list holds it, and `source` says why its source does
    /// not serve it, when its source was asked first.
    Mirror {
        index: usize,
        source: Option<Lack>,
    },
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
    /// its source answered with a file but not its size, so the size could not be checked.
    Unsized(Error),
    /// no answer after the retries, a web page where the file was expected, or another answer.
    Unknown(Error),
    /// it was imported from files, and no mirror holds it, so no source can give it.
    Imported,
}

impl Lack {
    /// what `--json` calls it: `changed`, `gone`, `refused`, `unsized`, `unknown` or `imported`.
    pub(in crate::cli) fn outcome(&self) -> &'static str {
        match self {
            Lack::Changed { .. } => "changed",
            Lack::Gone(_) => "gone",
            Lack::Refused(_) => "refused",
            Lack::Unsized(_) => "unsized",
            Lack::Unknown(_) => "unknown",
            Lack::Imported => "imported",
        }
    }

    /// whether it shows the file is lost: its source has nothing at its link or another file
    /// there, or no source ever had it. a refusal or no answer shows nothing either way.
    pub(in crate::cli) fn lost(&self) -> bool {
        matches!(self, Lack::Changed { .. } | Lack::Gone(_) | Lack::Imported)
    }

    /// the size a changed file has now.
    pub(in crate::cli) fn size(&self) -> Option<u64> {
        match self {
            Lack::Changed { size, .. } => Some(*size),
            _ => None,
        }
    }

    /// why no place serves a file pinned at `pinned` bytes, in a source's words where it gave
    /// them.
    pub(in crate::cli) fn error(self, pinned: u64) -> Error {
        match self {
            Lack::Changed { size, host } => Error::new(
                Kind::IntegrityMismatch,
                format!(
                    "{host} serves {} where data.lock pins {}",
                    sized(size, pinned),
                    sized(pinned, size)
                ),
            ),
            Lack::Gone(error)
            | Lack::Refused(error)
            | Lack::Unsized(error)
            | Lack::Unknown(error) => error,
            Lack::Imported => Error::new(
                Kind::ReferenceUnresolved,
                "it was imported from files, and no mirror holds it",
            ),
        }
    }
}

/// `size` in words, exact to the byte when `other` would read the same once rounded.
fn sized(size: u64, other: u64) -> String {
    if crate::units::size(size) == crate::units::size(other) {
        crate::units::count(size, "byte", "bytes")
    } else {
        crate::units::size(size)
    }
}

/// which places [`find`] asks, in what order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) enum Ask {
    /// as `status --sources` asks: each file's source, then for a file its source does not serve
    /// at its size, the mirrors, a folder mirror's copy by its size and an unpacked archive by the
    /// list of its files a mirror keeps, as push leaves it.
    Sources,
    /// as `drop` asks: the web and bucket mirrors, then the source of a file none holds. folder
    /// mirrors are the caller's, which hashes their copies.
    Elsewhere,
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
        Probed::File { total: None, .. } => Err(Lack::Unsized(Error::new(
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

/// where each of `files` can still be had, in order: at its source or on the first of the run's
/// mirrors, asked in the order sync asks them, that holds it, both as `ask` says. fails at a stop.
pub(in crate::cli) async fn find(
    context: &Context,
    net: &Arc<Net>,
    files: &[Pinned<'_>],
    ask: Ask,
) -> Result<Vec<Found>, Error> {
    let mut found: Vec<Option<Found>> = std::iter::repeat_with(|| None).take(files.len()).collect();
    let mut lacks: Vec<Option<Lack>> = std::iter::repeat_with(|| None).take(files.len()).collect();
    if ask == Ask::Sources {
        at_sources(context, net, files, &mut found, &mut lacks).await?;
    }
    if !left(&found).is_empty() {
        for index in context.mirrors.up(context, net).await? {
            let asked = left(&found);
            if asked.is_empty() {
                break;
            }
            let held = held_on(context, net, files, &asked, index, ask).await?;
            for (at, held) in asked.into_iter().zip(held) {
                if held {
                    let source = lacks[at].take();
                    found[at] = Some(Found::Mirror { index, source });
                }
            }
        }
    }
    if ask == Ask::Elsewhere {
        at_sources(context, net, files, &mut found, &mut lacks).await?;
    }
    Ok(found
        .into_iter()
        .zip(lacks)
        .map(|(found, lack)| {
            found.unwrap_or_else(|| {
                Found::Nowhere(
                    lack.unwrap_or_else(|| Lack::Unknown(Error::new(Kind::LocalIo, "not asked"))),
                )
            })
        })
        .collect())
}

/// the files left out of `found` yet, as `left` lists them.
fn left(found: &[Option<Found>]) -> Vec<usize> {
    (0..found.len()).filter(|at| found[*at].is_none()).collect()
}

/// asks the source of each of `files` that `found` has no place for yet, putting one that serves
/// it in `found` and why one does not in `lacks`. fails at a stop.
async fn at_sources(
    context: &Context,
    net: &Arc<Net>,
    files: &[Pinned<'_>],
    found: &mut [Option<Found>],
    lacks: &mut [Option<Lack>],
) -> Result<(), Error> {
    let asked = left(found);
    let probed = each(context, net, &asked, |at| files[at].request().ok()).await?;
    for (at, probed) in asked.into_iter().zip(probed) {
        let pinned = &files[at];
        let judgement = pinned
            .request()
            .and_then(|_| match (probed, Url::parse(pinned.url)) {
                (Some(probed), Ok(url)) => judged(probed, pinned.size, pinned.takes_page(), &url),
                _ => Err(Lack::Unknown(Error::new(
                    Kind::LocalIo,
                    format!("{} was not asked", pinned.label()),
                ))),
            });
        match judgement {
            Ok(()) => found[at] = Some(Found::Source),
            Err(lack) => lacks[at] = Some(lack),
        }
    }
    Ok(())
}

/// whether the mirror at `index` holds each of `files` at `asked`, in order, looked at as `ask`
/// says. fails at a stop.
async fn held_on(
    context: &Context,
    net: &Arc<Net>,
    files: &[Pinned<'_>],
    asked: &[usize],
    index: usize,
    ask: Ask,
) -> Result<Vec<bool>, Error> {
    let lists = ask == Ask::Sources;
    match context.mirrors.look(index) {
        Look::Folder(_) if ask == Ask::Elsewhere => Ok(vec![false; asked.len()]),
        Look::Folder(root) => Ok(tokio::task::block_in_place(|| {
            crate::place::run_all(asked, context.threads.at_most(LOOKS), |at| {
                let file = &files[*at];
                file.sha256
                    .is_some_and(|sha256| folder::has(root, &sha256, file.size))
                    || file.tree.is_some_and(|tree| {
                        matches!(folder::read_tree(root, &tree, &context.stop),
                            Ok(folder::Tree::Bytes(bytes)) if digest::sha256(&bytes) == tree)
                    })
            })
        })),
        Look::Ask(request) => {
            let probed = each(context, net, asked, |at| {
                files[at].wanted().map(|wanted| request(&wanted))
            })
            .await?;
            let mut held: Vec<bool> = asked
                .iter()
                .zip(probed)
                .map(|(at, probed)| {
                    matches!(probed, Some(Probed::File { total: Some(total), page: false })
                        if total == files[*at].size)
                })
                .collect();
            let location = &context.mirrors.mirrors.list[index];
            let base = context.mirrors.base(index, location).filter(|_| lists);
            for (held, at) in held.iter_mut().zip(asked).filter(|(held, _)| !**held) {
                if let (Some(base), Some(tree)) = (&base, files[*at].tree) {
                    *held = matches!(web::read_tree(net, base, &tree).await?,
                        web::Tree::Bytes(bytes) if digest::sha256(&bytes) == tree);
                }
            }
            Ok(held)
        }
    }
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
