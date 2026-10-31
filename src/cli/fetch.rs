//! many downloads at once: a bar for each running one and one for them all, or plain lines now
//! and then off a terminal, and a stop at the first failure that keeps what already arrived for
//! the next run.

mod again;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::task::JoinSet;

pub(super) use self::again::Again;
use super::Context;
use crate::archive::{self, Packed};
use crate::cache::Cache;
use crate::error::{Error, FileFailure, Kind};
use crate::net::{Event, Expect, Fetch, Fetched, Net};
use crate::output::{Bar, Output, Ticker};
use crate::source::{self, Remote};
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

/// builds a listed download, revalidating unpinned links on later runs. cached members can stand
/// for an archive when `name` identifies one and extraction is enabled.
pub(super) fn listed_job(remote: &Remote, name: &str, extract: bool) -> Job {
    Job {
        fetch: Fetch {
            expect: Expect {
                sha256: remote.sha256,
                md5: remote.md5,
                size: remote.size,
            },
            revalidate: remote.sha256.is_none() && remote.md5.is_none(),
            unpack: extract && archive::by_name(name) == Some(Packed::Archive),
            ..source::fetch_plan(&remote.url)
        },
        label: remote.path.clone(),
        dataset: None,
        again: None,
    }
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
            let bar = context
                .output
                .bar("Downloading", units::count(count as u64, "file", "files"));
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

type Approval = OnceLock<Result<(), Error>>;

/// one download as it runs: its bar, its part of the overall progress, and the question before
/// a large file whose size only its server tells.
struct Watch<'a> {
    job: &'a Job,
    output: &'a Output,
    overall: &'a Overall,
    /// another file must not proceed while the first file is still waiting for an answer.
    approval: &'a Approval,
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
        approval: &'a Approval,
        yes: bool,
    ) -> Watch<'a> {
        Watch {
            job,
            output,
            overall,
            approval,
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
            .get_or_init(|| self.output.bar("Downloading", self.job.label.clone()))
    }

    fn approve(&self, size: &str) -> Result<(), Error> {
        if let Some(answer) = self.approval.get() {
            return answer.clone();
        }
        let decide = || {
            self.approval
                .get_or_init(|| super::ask(self.output, self.yes, &self.job.label, size))
                .clone()
        };
        // a terminal question must not occupy a network worker while it waits for an answer.
        if tokio::runtime::Handle::try_current().is_ok_and(|handle| {
            handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread
        }) {
            tokio::task::block_in_place(decide)
        } else {
            decide()
        }
    }

    fn on(&self, event: &Event) -> Result<(), Error> {
        let label = &self.job.label;
        let bar = self.bar();
        match *event {
            Event::Waiting => {
                bar.message(
                    "Waiting",
                    format!("{label} · another fetchloom is downloading it"),
                );
            }
            Event::Started { total, resumed, .. } => {
                let left = total.map_or(0, |total| total.saturating_sub(resumed));
                if self.job.fetch.expect.size.is_none() {
                    if left > super::ASK_ABOVE {
                        self.approve(&units::size(left))?;
                    }
                    if let Some(total) = total
                        && !self.sized.swap(true, Ordering::Relaxed)
                    {
                        self.overall.ticker.sized(total);
                    }
                    self.unbounded.store(total.is_none(), Ordering::Relaxed);
                    self.received.store(resumed, Ordering::Relaxed);
                }
                bar.message("Downloading", label.clone());
                bar.start(total, resumed);
                self.overall.advance(resumed);
            }
            Event::Bytes(bytes) => {
                bar.advance(bytes);
                self.overall.advance(bytes);
                let received = self.received.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if self.unbounded.load(Ordering::Relaxed) && received > super::ASK_ABOVE {
                    let size = format!("more than {}", units::size(super::ASK_ABOVE));
                    self.approve(&size)?;
                    self.unbounded.store(false, Ordering::Relaxed);
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
    // unknown source sizes share one answer, once headers or received bytes show a large file.
    let approval = Arc::new(Approval::new());
    let listings = again::Listings::default();
    loop {
        while tasks.len() < in_flight
            && let Some((index, job)) = queue.next()
        {
            let (net, cache, output, overall, approval, listings) = (
                net.clone(),
                cache.clone(),
                context.output.clone(),
                overall.clone(),
                approval.clone(),
                listings.clone(),
            );
            let yes = context.yes;
            tasks.spawn(async move {
                let watch = Watch::new(&job, &output, &overall, &approval, yes);
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
                    bar.message("Downloading", format!("{done} of {count} files"));
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
    failed.sort_by_key(|(index, _)| *index);
    let failures = failed
        .iter()
        .map(|(index, error)| FileFailure {
            file: labels.get(*index).cloned().unwrap_or_default(),
            kind: error.kind.name(),
            message: error.message.clone(),
            details: error.details.clone(),
            help: error.help.clone(),
        })
        .collect();
    if failed.len() <= 1 {
        return failed.pop().map(|(_, mut error)| {
            error.failures = failures;
            error
        });
    }
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
    error.failures = failures;
    for (index, failure) in failed.iter().take(NAMED) {
        let label = labels.get(*index).map_or("", String::as_str);
        error = error.detail(if failure.message.starts_with(label) {
            failure.message.clone()
        } else {
            format!("{label}: {}", failure.message)
        });
        if shared_help.is_none()
            && let Some(help) = &failure.help
        {
            error = error.detail(format!("help: {help}"));
        }
    }
    if failed.len() > NAMED {
        error = error.detail(format!("and {} more", failed.len() - NAMED));
    }
    Some(error.help(shared_help.unwrap_or_else(|| {
        "downloaded files stay in the cache. fix the errors above, then run the same command again"
            .to_owned()
    })))
}

/// says which file a failure concerns, and for bytes that changed upstream, how to take them on.
fn explain(error: Error, job: &Job) -> Error {
    // an MD5 mismatch comes with its own help, since the source contradicts itself
    let changed_upstream = error.kind == Kind::IntegrityMismatch
        && error.help.is_none()
        && job.fetch.expect.sha256.is_some()
        && job.dataset.is_some();
    if !changed_upstream {
        return if error.kind == Kind::IntegrityMismatch {
            error.or_help("run the same command again later. if it keeps happening, ask the dataset's owner to check the file and its published size or fingerprint")
        } else {
            error
        };
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
            watched: false,
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
        };
        let approval = Approval::new();
        let watch = Watch::new(&job, &output, &overall, &approval, false);
        watch.on(&unknown()).unwrap();
        watch.on(&Event::Bytes(super::super::ASK_ABOVE)).unwrap();
        let stopped = watch.on(&Event::Bytes(1)).unwrap_err();
        assert_eq!(stopped.kind, Kind::Unconfirmed);
        assert_eq!(
            stopped.message,
            "downloading big.bin needs more than 1.0 GiB, and fetchloom cannot ask for confirmation here"
        );
        assert_eq!(
            stopped.help.as_deref(),
            Some("add --yes to approve this download")
        );
        assert_eq!(
            watch.on(&Event::Bytes(1)).unwrap_err().kind,
            Kind::Unconfirmed
        );
        let fresh = Approval::new();
        let approved = Watch::new(&job, &output, &overall, &fresh, true);
        approved.on(&unknown()).unwrap();
        approved
            .on(&Event::Bytes(2 * super::super::ASK_ABOVE))
            .unwrap();
        let another = Watch::new(&job, &output, &overall, &fresh, false);
        another.on(&unknown()).unwrap();
        another
            .on(&Event::Bytes(2 * super::super::ASK_ABOVE))
            .unwrap();
        let told = Watch::new(&job, &output, &overall, &fresh, false);
        let small = Event::Started {
            total: Some(10),
            resumed: 0,
        };
        told.on(&small).unwrap();
        told.on(&Event::Bytes(2 * super::super::ASK_ABOVE)).unwrap();
    }

    #[test]
    fn an_unknown_drive_size_asks_when_the_server_states_a_large_remainder() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let job = Job {
            fetch: Fetch::new(
                Url::parse("https://drive.google.com/uc?id=1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb")
                    .unwrap(),
            ),
            label: "file.txt".to_owned(),
            dataset: None,
            again: None,
        };
        let overall = Overall {
            bar: None,
            ticker: output.ticker("Downloading", String::new(), std::iter::empty()),
        };
        for (total, resumed, needs_approval) in [
            (super::super::ASK_ABOVE, 0, false),
            (super::super::ASK_ABOVE + 1, 0, true),
            (super::super::ASK_ABOVE + 1, 1, false),
        ] {
            let approval = Approval::new();
            let watch = Watch::new(&job, &output, &overall, &approval, false);
            let result = watch.on(&Event::Started {
                total: Some(total),
                resumed,
            });
            if needs_approval {
                assert_eq!(result.unwrap_err().kind, Kind::Unconfirmed);
            } else {
                result.unwrap();
            }
        }
    }

    #[test]
    fn refusing_a_size_question_does_not_approve_another_file() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let job = |name: &str| Job {
            fetch: Fetch::new(Url::parse(&format!("https://example.org/{name}")).unwrap()),
            label: name.to_owned(),
            dataset: None,
            again: None,
        };
        let first = job("first.bin");
        let second = job("second.bin");
        let overall = Overall {
            bar: None,
            ticker: output.ticker("Downloading", String::new(), std::iter::empty()),
        };
        let approval = Approval::new();
        let event = Event::Started {
            total: Some(super::super::ASK_ABOVE + 1),
            resumed: 0,
        };
        let first = Watch::new(&first, &output, &overall, &approval, false);
        assert_eq!(first.on(&event).unwrap_err().kind, Kind::Unconfirmed);
        let second = Watch::new(&second, &output, &overall, &approval, false);
        assert_eq!(second.on(&event).unwrap_err().kind, Kind::Unconfirmed);
    }

    #[test]
    fn an_unpinned_download_is_not_described_as_locked() {
        let job = Job {
            fetch: Fetch {
                expect: crate::net::Expect {
                    sha256: Some(crate::digest::sha256(b"expected")),
                    ..crate::net::Expect::default()
                },
                ..Fetch::new(Url::parse("https://example.org/data.csv").unwrap())
            },
            label: "data.csv".to_owned(),
            dataset: None,
            again: None,
        };
        let error = explain(
            Error::new(Kind::IntegrityMismatch, "the file differs"),
            &job,
        );
        assert_eq!(error.message, "the file differs");
        assert!(
            error
                .help
                .unwrap()
                .contains("published size or fingerprint")
        );
    }

    #[test]
    fn different_download_failures_keep_their_recovery_steps() {
        let labels = vec!["a.csv".to_owned(), "b.csv".to_owned()];
        let failed = vec![
            (
                0,
                Error::new(Kind::AuthRequired, "access refused").help("set HF_TOKEN"),
            ),
            (
                1,
                Error::new(Kind::ReferenceUnresolved, "file missing").help("check its link"),
            ),
        ];
        let error = together(&labels, failed).unwrap();
        assert_eq!(
            error.details,
            [
                "a.csv: access refused",
                "help: set HF_TOKEN",
                "b.csv: file missing",
                "help: check its link",
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "downloaded files stay in the cache. fix the errors above, then run the same command again"
            )
        );
    }
}
