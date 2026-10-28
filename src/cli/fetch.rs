//! many downloads at once: a bar for each running one and one for them all, or plain lines now
//! and then off a terminal, and a stop at the first failure that keeps what already arrived for
//! the next run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::task::JoinSet;

use super::Context;
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{Event, Fetch, Fetched, Net};
use crate::output::{Bar, Ticker};
use crate::units;

/// a file to download, the name the person watching knows it by, and the dataset it belongs to
/// when it is part of a project.
pub(super) struct Job {
    pub(super) fetch: Fetch,
    pub(super) label: String,
    pub(super) dataset: Option<String>,
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

/// downloads every job within the network's limits and returns what each fetched, in the jobs'
/// order.
pub(super) async fn all(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    jobs: Vec<Job>,
) -> Result<Vec<Fetched>, Error> {
    let count = jobs.len();
    let overall = Arc::new(Overall::new(context, &jobs));
    let mut results: Vec<Option<Fetched>> = std::iter::repeat_with(|| None).take(count).collect();
    let mut tasks = JoinSet::new();
    let mut queue = jobs.into_iter().enumerate();
    // more tasks than download slots keep every slot busy, and few enough keep memory flat
    let in_flight = context.jobs * 2;
    let mut done = 0;
    // a file whose size only its headers tell is asked about there, once per run
    let asked = Arc::new(AtomicBool::new(false));
    loop {
        while tasks.len() < in_flight
            && let Some((index, job)) = queue.next()
        {
            let (net, cache, output, overall, asked) = (
                net.clone(),
                cache.clone(),
                context.output.clone(),
                overall.clone(),
                asked.clone(),
            );
            let yes = context.yes;
            tasks.spawn(async move {
                let bar: OnceLock<Bar> = OnceLock::new();
                let sized = AtomicBool::new(false);
                let on = |event: Event| -> Result<(), Error> {
                    let bar = bar.get_or_init(|| output.bar(job.label.clone()));
                    match event {
                        Event::Waiting => bar.message(format!(
                            "waiting for another fetchloom to download {}",
                            job.label
                        )),
                        Event::Started { total, resumed, .. } => {
                            let left = total.map_or(0, |total| total.saturating_sub(resumed));
                            if job.fetch.expect.size.is_none() {
                                if left > super::ASK_ABOVE && !asked.swap(true, Ordering::Relaxed) {
                                    super::confirm(&output, yes, &job.label, left)?;
                                }
                                if let Some(total) = total
                                    && !sized.swap(true, Ordering::Relaxed)
                                {
                                    overall.ticker.sized(total);
                                }
                            }
                            bar.start(total, resumed);
                            overall.advance(resumed);
                        }
                        Event::Bytes(bytes) => {
                            bar.advance(bytes);
                            overall.advance(bytes);
                        }
                    }
                    Ok(())
                };
                let fetched = net.download(&job.fetch, &cache, &on).await;
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
    Ok(results.into_iter().flatten().collect())
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
