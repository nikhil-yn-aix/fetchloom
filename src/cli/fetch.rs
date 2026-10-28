//! many downloads at once: a bar for each running one and one for them all, or plain lines now
//! and then off a terminal, and a stop at the first failure that keeps what already arrived for
//! the next run.

mod again;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::task::JoinSet;

pub(super) use self::again::Again;
use super::Context;
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{Event, Fetch, Fetched, Net};
use crate::output::{Bar, Output, Ticker};
use crate::units;

/// a file to download, the name the person watching knows it by, and the dataset it belongs to
/// when it is part of a project.
pub(super) struct Job {
    pub(super) fetch: Fetch,
    pub(super) label: String,
    pub(super) dataset: Option<String>,
    /// where to ask again when the file is pinned and its recorded link is dead.
    pub(super) again: Option<Again>,
}

/// how all the downloads of one call go together: a bar for them all on a terminal when there
/// are several, and plain lines now and then where no bar shows.
struct Overall {
    bar: Option<Bar>,
    ticker: Ticker,
}

impl Overall {
    fn new(context: &Context, jobs: &[Job]) -> Overall {
        let count = jobs.len();
        let sizes = || jobs.iter().map(|job| job.fetch.expect.size);
        let bar = (count > 1).then(|| {
            let bar = context.output.bar(format!("downloading {count} files"));
            bar.start(sizes().sum(), 0);
            bar
        });
        let what = match jobs {
            [only] => only.label.clone(),
            _ => units::count(count as u64, "file", "files"),
        };
        let ticker = context.output.ticker("Downloading", what, sizes());
        Overall { bar, ticker }
    }

    fn advance(&self, bytes: u64) {
        if let Some(bar) = &self.bar {
            bar.advance(bytes);
        }
        self.ticker.advance(bytes);
    }
}

/// one download as it runs: its bar, its part of the overall progress, and the question before
/// a large file whose size only its server tells.
struct Watch<'a> {
    job: &'a Job,
    output: &'a Output,
    overall: &'a Overall,
    /// whether this batch asked already, so it asks at most once.
    asked: &'a AtomicBool,
    yes: bool,
    bar: OnceLock<Bar>,
    /// whether the size the server told was added to the overall total.
    sized: AtomicBool,
    /// whether neither the source nor the server told the size, so only the bytes received tell
    /// when the download gets large.
    unbounded: AtomicBool,
    received: AtomicU64,
}

impl<'a> Watch<'a> {
    fn new(
        job: &'a Job,
        output: &'a Output,
        overall: &'a Overall,
        asked: &'a AtomicBool,
        yes: bool,
    ) -> Watch<'a> {
        Watch {
            job,
            output,
            overall,
            asked,
            yes,
            bar: OnceLock::new(),
            sized: AtomicBool::new(false),
            unbounded: AtomicBool::new(false),
            received: AtomicU64::new(0),
        }
    }

    /// the download's bar, made when it is first needed.
    fn bar(&self) -> &Bar {
        self.bar
            .get_or_init(|| self.output.bar(self.job.label.clone()))
    }

    fn on(&self, event: &Event) -> Result<(), Error> {
        let label = &self.job.label;
        let bar = self.bar();
        match *event {
            Event::Waiting => {
                bar.message(format!("waiting for another fetchloom to download {label}"));
            }
            Event::Started { total, resumed, .. } => {
                let left = total.map_or(0, |total| total.saturating_sub(resumed));
                if self.job.fetch.expect.size.is_none() {
                    if left > super::ASK_ABOVE && !self.asked.swap(true, Ordering::Relaxed) {
                        super::confirm(self.output, self.yes, label, left)?;
                    }
                    if let Some(total) = total
                        && !self.sized.swap(true, Ordering::Relaxed)
                    {
                        self.overall.ticker.sized(total);
                    }
                    self.unbounded.store(total.is_none(), Ordering::Relaxed);
                    self.received.store(resumed, Ordering::Relaxed);
                }
                bar.start(total, resumed);
                self.overall.advance(resumed);
            }
            Event::Bytes(bytes) => {
                bar.advance(bytes);
                self.overall.advance(bytes);
                let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if self.unbounded.load(Ordering::Relaxed)
                    && received > super::ASK_ABOVE
                    && !self.asked.swap(true, Ordering::Relaxed)
                {
                    let size = format!("more than {}", units::size(super::ASK_ABOVE));
                    super::ask(self.output, self.yes, label, &size)?;
                }
            }
        }
        Ok(())
    }
}

/// downloads every job within the network's limits and returns what each fetched, in the jobs'
/// order.
pub(super) async fn all(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Fetched>, Error> {
    let count = jobs.len();
    let alone = count == 1;
    let overall = Arc::new(Overall::new(context, &jobs));
    let labels: Vec<String> = jobs.iter().map(|job| job.label.clone()).collect();
    let mut results: Vec<Option<Fetched>> = std::iter::repeat_with(|| None).take(count).collect();
    let mut failed = Vec::new();
    let mut tasks = JoinSet::new();
    let mut queue = jobs.into_iter().enumerate();
    // more tasks than download slots keep every slot busy, and few enough keep memory flat
    let in_flight = context.jobs * 2;
    let mut done = 0;
    // a large file whose size the source did not state is asked about once its server tells
    // the size, or once that much arrived when it never does, and only the first one asks
    let asked = Arc::new(AtomicBool::new(false));
    let listings = again::Listings::default();
    loop {
        while tasks.len() < in_flight
            && let Some((index, job)) = queue.next()
        {
            let (net, cache, output, overall, asked, listings) = (
                net.clone(),
                cache.clone(),
                context.output.clone(),
                overall.clone(),
                asked.clone(),
                listings.clone(),
            );
            let yes = context.yes;
            tasks.spawn(async move {
                let watch = Watch::new(&job, &output, &overall, &asked, yes);
                // with no bar for them all, a lone download shows its bar while the server
                // takes its time to answer
                if alone {
                    watch.bar();
                }
                let on = |event: Event| watch.on(&event);
                let fetched = match net.download(&job.fetch, &cache, &on).await {
                    Err(err) => {
                        again::recover(&net, &cache, &output, &listings, &job, err, &on).await
                    }
                    fetched => fetched,
                };
                (index, fetched.map_err(|err| explain(err, &job)))
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        match joined {
            Ok((index, Ok(fetched))) => {
                results[index] = Some(fetched);
                done += 1;
                if let Some(bar) = &overall.bar {
                    bar.message(format!("downloaded {done} of {count} files"));
                }
            }
            Ok((index, Err(err))) if on_its_own(&err) => failed.push((index, err)),
            Ok((_, Err(err))) => {
                tasks.abort_all();
                return Err(err);
            }
            Err(err) => {
                tasks.abort_all();
                return Err(Error::new(
                    Kind::LocalIo,
                    format!("a download stopped unexpectedly: {err}"),
                ));
            }
        }
    }
    if let Some(error) = together(&labels, failed) {
        return Err(error);
    }
    Ok(results.into_iter().flatten().collect())
}

/// how many failed files one error names. the rest are counted.
const NAMED: usize = 10;

/// whether a failure concerns its own file only, so the other downloads go on and every such
/// file is named at the end. a failure of the network, the disk or the person stops them all.
fn on_its_own(error: &Error) -> bool {
    matches!(
        error.kind,
        Kind::ReferenceUnresolved
            | Kind::IntegrityMismatch
            | Kind::AuthRequired
            | Kind::NetworkRefused
            | Kind::SourceInvalid
    )
}

/// one error for the files that failed on their own: the failure itself when it is the only
/// one, else a line naming each file and what happened to it, in the jobs' order.
fn together(labels: &[String], mut failed: Vec<(usize, Error)>) -> Option<Error> {
    if failed.len() <= 1 {
        return failed.pop().map(|(_, error)| error);
    }
    failed.sort_by_key(|(index, _)| *index);
    let (_, first) = failed.first()?;
    let shared_help = failed
        .iter()
        .all(|(_, error)| error.help == first.help)
        .then(|| first.help.clone())
        .flatten();
    let mut error = Error::new(
        first.kind,
        format!("{} files could not be downloaded", failed.len()),
    );
    for (index, failure) in failed.iter().take(NAMED) {
        let label = labels.get(*index).map_or("", String::as_str);
        error = error.detail(if failure.message.starts_with(label) {
            failure.message.clone()
        } else {
            format!("{label}: {}", failure.message)
        });
    }
    if failed.len() > NAMED {
        error = error.detail(format!("and {} more", failed.len() - NAMED));
    }
    Some(error.help(shared_help.unwrap_or_else(|| {
        "the files that arrived are kept, so the same command again downloads only the rest"
            .to_owned()
    })))
}

/// says which file a failure concerns, and for bytes that changed upstream, how to take them on.
fn explain(error: Error, job: &Job) -> Error {
    // an MD5 mismatch comes with its own help, since the source contradicts itself
    let changed_upstream = error.kind == Kind::IntegrityMismatch
        && error.help.is_none()
        && job.fetch.expect.sha256.is_some();
    if !changed_upstream {
        return error;
    }
    let mut error = Error {
        message: format!("{} is not the file that was locked", job.label),
        ..error
    };
    if let Some(dataset) = &job.dataset {
        error.help = Some(format!(
            "if the new version is intended, run `fetchloom lock --upgrade {dataset}`"
        ));
    }
    error
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;
    use crate::output::{Level, Style};

    #[test]
    fn a_download_of_unknown_length_asks_once_it_passes_the_limit() {
        let style = Style {
            color: false,
            unicode: true,
            interactive: false,
        };
        let output = Output::new(style, Level::Quiet, false, false);
        let job = Job {
            fetch: Fetch::new(Url::parse("https://example.org/big.bin").unwrap()),
            label: "big.bin".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = Overall {
            bar: None,
            ticker: output.ticker("Downloading", String::new(), std::iter::empty()),
        };
        let unknown = || Event::Started {
            total: None,
            resumed: 0,
            name: None,
        };
        let asked = AtomicBool::new(false);
        let watch = Watch::new(&job, &output, &overall, &asked, false);
        watch.on(&unknown()).unwrap();
        watch.on(&Event::Bytes(super::super::ASK_ABOVE)).unwrap();
        let stopped = watch.on(&Event::Bytes(1)).unwrap_err();
        assert_eq!(stopped.kind, Kind::Unconfirmed);
        assert_eq!(
            stopped.message,
            "downloading big.bin needs more than 1.0 GiB, and there is no terminal to ask first"
        );
        assert_eq!(
            stopped.help.as_deref(),
            Some("add --yes to download anyway")
        );
        watch.on(&Event::Bytes(1)).unwrap();
        let fresh = AtomicBool::new(false);
        let approved = Watch::new(&job, &output, &overall, &fresh, true);
        approved.on(&unknown()).unwrap();
        approved
            .on(&Event::Bytes(2 * super::super::ASK_ABOVE))
            .unwrap();
        let told = Watch::new(&job, &output, &overall, &fresh, false);
        let small = Event::Started {
            total: Some(10),
            resumed: 0,
            name: None,
        };
        told.on(&small).unwrap();
        told.on(&Event::Bytes(2 * super::super::ASK_ABOVE)).unwrap();
    }
}
