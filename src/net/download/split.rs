//! one large file over several streams at once. each stream fetches a range and writes it where
//! it belongs, a stream that runs out of work takes half of the largest range left, and a stream
//! is added only while each one has made the whole download faster.

use std::future::{Future, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header;
use tokio::sync::OwnedSemaphorePermit;
use url::Url;

use super::{Event, Listener, local};
use crate::cache::{PartialWriter, Validators};
use crate::error::Kind;
use crate::net::failure::{self, Failure};
use crate::net::headers::{self, if_range};
use crate::net::{IDLE, Net, Shown, host};
use crate::work::Work;

/// the smallest file split across streams.
pub(crate) const SPLIT_ABOVE: u64 = 64 * 1024 * 1024;
/// the most streams one file gets.
pub(crate) const MOST_STREAMS: usize = 8;
/// the smallest half of a range that a stream takes from another.
pub(crate) const SMALLEST_PIECE: u64 = 8 * 1024 * 1024;
/// how long a rate is measured before and after a stream is added.
pub(crate) const WINDOW: Duration = Duration::from_secs(2);
/// how much faster the whole download must get with each added stream before another is added.
const GAIN: f64 = 1.15;
/// failed streams after which a download asks for no more ranges.
const FAILURES: u32 = 3;

/// the file being split: where to ask, what identifies its version, and who hears of progress.
pub(super) struct Job<'a> {
    pub(super) from: &'a Url,
    /// the URL whose host the download's slots belong to.
    pub(super) key: &'a Url,
    pub(super) validators: &'a Validators,
    pub(super) total: u64,
    pub(super) on: Listener<'a>,
}

pub(super) enum Streamed {
    /// every byte arrived. `pieced` says more than one answer brought them.
    Whole {
        writer: Box<PartialWriter>,
        pieced: bool,
    },
    /// a range came from another version of the file, so the download starts over.
    StartOver,
}

/// one stream's place in the file: its next byte and where its share ends.
#[derive(Clone, Copy, Debug)]
struct Share {
    next: u64,
    end: u64,
    /// the stream's answer runs to the end of the file, so its share can grow back over a range
    /// another stream gave up.
    open: bool,
    /// the stream delivered bytes of this share.
    started: bool,
}

/// which stream fetches which bytes, and what nobody fetches yet.
#[derive(Debug, Default)]
struct Board {
    /// by stream, none once the stream stopped.
    shares: Vec<Option<Share>>,
    /// ranges nobody fetches, in order.
    free: Vec<(u64, u64)>,
    /// bytes the streams delivered, for the rate.
    delivered: u64,
    /// a range was asked for.
    pieced: bool,
    /// no more ranges are asked for.
    stopped: bool,
}

impl Board {
    /// the first gap goes to the stream with the answer in hand, and the rest wait.
    fn new(mut gaps: Vec<(u64, u64)>) -> Board {
        let first = (!gaps.is_empty()).then(|| gaps.remove(0));
        Board {
            shares: vec![first.map(|(next, end)| Share {
                next,
                end,
                open: true,
                started: false,
            })],
            free: gaps,
            ..Board::default()
        }
    }

    fn share(&self, id: usize) -> Option<Share> {
        self.shares.get(id).copied().flatten()
    }

    fn left(&self, id: usize) -> u64 {
        self.share(id).map_or(0, |share| share.end - share.next)
    }

    /// the next range for stream `id`: the first free one, else the upper half of the largest
    /// share left, when that half is at least `piece`.
    fn assign(&mut self, id: usize, piece: u64) -> Option<(u64, u64)> {
        let range = if self.free.is_empty() {
            let (victim, left) = self
                .shares
                .iter()
                .enumerate()
                .filter_map(|(index, share)| share.map(|share| (index, share.end - share.next)))
                .max_by_key(|(_, left)| *left)?;
            let half = left / 2;
            if half == 0 || half < piece {
                return None;
            }
            let share = self.shares[victim].as_mut()?;
            let end = share.end;
            share.end -= half;
            (share.end, end)
        } else {
            self.free.remove(0)
        };
        if self.shares.len() <= id {
            self.shares.resize(id + 1, None);
        }
        self.shares[id] = Some(Share {
            next: range.0,
            end: range.1,
            open: false,
            started: false,
        });
        self.pieced = true;
        Some(range)
    }

    /// takes up to `length` bytes that arrived for stream `id`: where they go and how many of
    /// them belong to its share.
    fn deliver(&mut self, id: usize, length: u64) -> (u64, u64) {
        let Some(Some(share)) = self.shares.get_mut(id) else {
            return (0, 0);
        };
        let at = share.next;
        let taken = length.min(share.end - share.next);
        share.next += taken;
        share.started = true;
        // the stream lets its answer go now, so the share cannot grow back
        if share.next == share.end {
            share.open = false;
        }
        self.delivered += taken;
        (at, taken)
    }

    /// stream `id` stopped, and what it did not fetch goes back.
    fn give_back(&mut self, id: usize) {
        let Some(share) = self.shares.get_mut(id).and_then(Option::take) else {
            return;
        };
        if share.next < share.end {
            self.release((share.next, share.end));
        }
    }

    /// a range nobody fetches: a stream whose answer runs on into it takes it, else it waits.
    fn release(&mut self, range: (u64, u64)) {
        if let Some(share) = self
            .shares
            .iter_mut()
            .flatten()
            .find(|share| share.open && share.end == range.0)
        {
            share.end = range.1;
            return;
        }
        let index = self.free.partition_point(|free| free.0 < range.0);
        self.free.insert(index, range);
    }

    /// the server sends stream `id` its share only up to `end`, and the rest goes back.
    fn trim(&mut self, id: usize, end: u64) {
        let Some(Some(share)) = self.shares.get_mut(id) else {
            return;
        };
        if share.next < end && end < share.end {
            let rest = (end, share.end);
            share.end = end;
            self.release(rest);
        }
    }

    /// whether every byte was delivered.
    #[cfg(test)]
    fn finished(&self) -> bool {
        self.free.is_empty()
            && self
                .shares
                .iter()
                .flatten()
                .all(|share| share.next == share.end)
    }
}

enum Ended {
    /// the board has no more work for it.
    Done,
    /// the server does not serve ranges as asked, such as by sending the whole file.
    NoRanges,
    /// a range came from another version of the file.
    Changed,
    /// the writer stopped, and its own error says why.
    WriterGone,
    Failed(Failure),
}

/// whether to add a stream, by the rate the download had before and after the last one.
#[derive(Clone, Copy)]
enum Probe {
    /// measuring the rate of the streams running now.
    Measure { since: Instant, delivered: u64 },
    /// a stream was added and has delivered nothing yet.
    Wait { id: usize, before: f64 },
    /// measuring the rate with the added stream.
    Rise {
        before: f64,
        since: Instant,
        delivered: u64,
    },
    /// the last stream added made the download no faster.
    Settled,
}

struct Streams<'a> {
    job: Job<'a>,
    board: Mutex<Board>,
    sender: tokio::sync::mpsc::Sender<(u64, Bytes)>,
}

impl Streams<'_> {
    fn board(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Running<'a> = Vec<Pin<Box<dyn Future<Output = (usize, Ended)> + Send + 'a>>>;

/// writes what the streams send, each piece at its place, on a blocking thread.
fn write_at_places(
    mut writer: PartialWriter,
    mut receiver: tokio::sync::mpsc::Receiver<(u64, Bytes)>,
) -> tokio::task::JoinHandle<io::Result<PartialWriter>> {
    tokio::task::spawn_blocking(move || {
        while let Some((at, bytes)) = receiver.blocking_recv() {
            writer.write_at(at, &bytes)?;
        }
        writer.save()?;
        Ok(writer)
    })
}

impl Net {
    /// fetches the rest of the file over as many streams as make it faster, starting with the
    /// answer in hand. clears `allowed` when this file should not be split again.
    pub(super) async fn split(
        &self,
        job: Job<'_>,
        writer: PartialWriter,
        response: reqwest::Response,
        allowed: &mut bool,
    ) -> Result<Streamed, Failure> {
        let (from, total) = (job.from, job.total);
        let gaps = writer.gaps(total);
        let (sender, receiver) = tokio::sync::mpsc::channel::<(u64, Bytes)>(16);
        let writing = write_at_places(writer, receiver);
        let streams = Streams {
            job,
            board: Mutex::new(Board::new(gaps)),
            sender,
        };
        let shared = &streams;
        let mut running: Running<'_> = vec![Box::pin(async move {
            (0, self.split_stream(shared, 0, Some(response)).await)
        })];
        let mut probe = Probe::Measure {
            since: Instant::now(),
            delivered: 0,
        };
        let mut tick = tokio::time::interval(self.split.window / 8);
        let (mut failures, mut outcome) = (0, None);
        let (mut interrupted, mut start_over) = (false, false);
        while !running.is_empty() {
            tokio::select! {
                (index, (id, ended)) = poll_fn(|cx| {
                    for (index, stream) in running.iter_mut().enumerate() {
                        if let Poll::Ready(ended) = stream.as_mut().poll(cx) {
                            return Poll::Ready((index, ended));
                        }
                    }
                    Poll::Pending
                }) => {
                    drop(running.swap_remove(index));
                    match ended {
                        Ended::Done => {}
                        Ended::NoRanges => shared.board().stopped = true,
                        Ended::Changed => {
                            start_over = true;
                            break;
                        }
                        Ended::WriterGone => break,
                        Ended::Failed(Failure::Final(error)) if error.kind == Kind::Interrupted => {
                            interrupted = true;
                            break;
                        }
                        Ended::Failed(failure) => {
                            failures += 1;
                            if failures >= FAILURES || matches!(failure, Failure::Final(_)) {
                                shared.board().stopped = true;
                            }
                            tracing::debug!(id, "a stream of a split download failed");
                            outcome = Some(failure);
                        }
                    }
                }
                _ = tick.tick() => {
                    if let Some((id, permits)) = self.consider(shared, &mut probe, running.len()) {
                        running.push(Box::pin(async move {
                            let _permits = permits;
                            (id, self.split_stream(shared, id, None).await)
                        }));
                    }
                }
                () = self.stop.asked() => {
                    interrupted = true;
                    break;
                }
            }
        }
        drop(running);
        let (stopped, pieced) = {
            let board = streams.board();
            (board.stopped, board.pieced)
        };
        drop(streams);
        let writer = writing
            .await
            .map_err(|err| Failure::Final(local(&io::Error::other(err))))?
            .map_err(|err| Failure::Final(local(&err)))?;
        if interrupted {
            return Err(Failure::Final(failure::interrupted(from)));
        }
        if start_over || stopped {
            *allowed = false;
        }
        if start_over {
            return Ok(Streamed::StartOver);
        }
        if writer.written() == total {
            return Ok(Streamed::Whole {
                writer: Box::new(writer),
                pieced,
            });
        }
        Err(outcome.unwrap_or_else(|| {
            failure::transient(format!("{} stopped sending parts of the file", host(from)))
        }))
    }

    /// fetches the shares the board gives stream `id`, starting with `answer` when the stream
    /// has one, until the board has nothing left for it.
    async fn split_stream(
        &self,
        shared: &Streams<'_>,
        id: usize,
        mut answer: Option<reqwest::Response>,
    ) -> Ended {
        loop {
            let Some(share) = shared.board().share(id) else {
                return Ended::Done;
            };
            let response = match answer.take() {
                Some(response) => response,
                None => match self.part(shared, id, share.next, share.end).await {
                    Ok(response) => response,
                    Err(ended) => {
                        shared.board().give_back(id);
                        return ended;
                    }
                },
            };
            if let Err(ended) = self.drain(shared, id, response).await {
                shared.board().give_back(id);
                return ended;
            }
            let mut board = shared.board();
            let next = if board.stopped {
                None
            } else {
                board.assign(id, self.split.piece)
            };
            if next.is_none() {
                board.give_back(id);
                return Ended::Done;
            }
        }
    }

    /// asks for bytes `start` up to `end` of the file, for stream `id`, with `If-Range` so that
    /// a server holding another version sends the whole file instead.
    async fn part(
        &self,
        shared: &Streams<'_>,
        id: usize,
        start: u64,
        end: u64,
    ) -> Result<reqwest::Response, Ended> {
        let job = &shared.job;
        let check = if_range(job.validators).map(str::to_owned);
        let shape = |request: reqwest::RequestBuilder| {
            let request = request.header(header::RANGE, format!("bytes={start}-{}", end - 1));
            match &check {
                Some(check) => request.header(header::IF_RANGE, check),
                None => request,
            }
        };
        Work::add(&self.work.requests, 1);
        let response = self
            .send_apart(job.from, shape)
            .await
            .map_err(Ended::Failed)?;
        let status = response.status();
        let head = response.headers();
        tracing::debug!(url = %Shown(job.from), status = status.as_u16(), start, end, "part");
        let changed = headers::changed(job.validators, &headers::validators(head));
        match status {
            StatusCode::PARTIAL_CONTENT
                if changed || headers::total_length(head, status) != Some(job.total) =>
            {
                Err(Ended::Changed)
            }
            StatusCode::PARTIAL_CONTENT if headers::range_start(head) == Some(start) => {
                if let Some(last) = headers::range_end(head) {
                    shared.board().trim(id, last.saturating_add(1));
                }
                Ok(response)
            }
            StatusCode::OK | StatusCode::RANGE_NOT_SATISFIABLE if changed => Err(Ended::Changed),
            StatusCode::RANGE_NOT_SATISFIABLE => Err(Ended::Changed),
            StatusCode::OK | StatusCode::PARTIAL_CONTENT => Err(Ended::NoRanges),
            status => Err(Ended::Failed(failure::refusal(job.from, status, head))),
        }
    }

    /// moves an answer's bytes to the writer until the stream's share is done.
    async fn drain(
        &self,
        shared: &Streams<'_>,
        id: usize,
        mut response: reqwest::Response,
    ) -> Result<(), Ended> {
        let url = shared.job.from;
        let broke = |reason: String| Ended::Failed(failure::transient(reason));
        while shared.board().left(id) > 0 {
            let next = tokio::select! {
                next = tokio::time::timeout(IDLE, response.chunk()) => next,
                () = self.stop.asked() => {
                    return Err(Ended::Failed(Failure::Final(failure::interrupted(url))));
                }
            };
            let chunk = match next {
                Err(_) => {
                    return Err(broke(format!(
                        "{} sent nothing for {} s",
                        host(url),
                        IDLE.as_secs()
                    )));
                }
                Ok(Err(err)) => {
                    return Err(broke(format!(
                        "the connection to {} broke: {err}",
                        host(url)
                    )));
                }
                Ok(Ok(None)) => {
                    return Err(broke(format!(
                        "{} closed the connection before the end of a part",
                        host(url)
                    )));
                }
                Ok(Ok(Some(chunk))) => chunk,
            };
            self.throttle(url, chunk.len())
                .await
                .map_err(Ended::Failed)?;
            Work::add(&self.work.bytes_down, chunk.len() as u64);
            let (at, taken) = shared.board().deliver(id, chunk.len() as u64);
            let Ok(length) = usize::try_from(taken) else {
                continue;
            };
            if length == 0 {
                continue;
            }
            (shared.job.on)(Event::Bytes(taken))
                .map_err(|err| Ended::Failed(Failure::Final(err)))?;
            if shared
                .sender
                .send((at, chunk.slice(..length)))
                .await
                .is_err()
            {
                return Err(Ended::WriterGone);
            }
        }
        Ok(())
    }

    /// adds a stream when the rate rule allows it: the slots and work it takes. a stream is
    /// added while each added stream made the download at least [`GAIN`] times faster, and only
    /// when the host has a free slot that no other file waits for.
    fn consider(
        &self,
        shared: &Streams<'_>,
        probe: &mut Probe,
        running: usize,
    ) -> Option<(usize, (OwnedSemaphorePermit, OwnedSemaphorePermit))> {
        let now = Instant::now();
        let (delivered, started, gone) = {
            let board = shared.board();
            let id = match *probe {
                Probe::Wait { id, .. } => id,
                _ => 0,
            };
            (
                board.delivered,
                board.share(id).is_some_and(|share| share.started),
                board.share(id).is_none(),
            )
        };
        let rate = |since: Instant, from: u64| {
            let seconds = now.duration_since(since).as_secs_f64();
            #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
            let bytes = delivered.saturating_sub(from) as f64;
            bytes / seconds.max(f64::EPSILON)
        };
        let window = self.split.window;
        let (before, added) = match *probe {
            Probe::Measure {
                since,
                delivered: from,
            } if now.duration_since(since) >= window => {
                let before = rate(since, from);
                (before, self.add_stream(shared, running))
            }
            Probe::Wait { before, .. } if started => {
                *probe = Probe::Rise {
                    before,
                    since: now,
                    delivered,
                };
                return None;
            }
            Probe::Wait { .. } if gone => {
                *probe = Probe::Measure {
                    since: now,
                    delivered,
                };
                return None;
            }
            Probe::Rise {
                before,
                since,
                delivered: from,
            } if now.duration_since(since) >= window => {
                let after = rate(since, from);
                if after < before * GAIN {
                    tracing::debug!(
                        streams = running,
                        "another stream made the download no faster"
                    );
                    *probe = Probe::Settled;
                    return None;
                }
                (after, self.add_stream(shared, running))
            }
            _ => return None,
        };
        *probe = match &added {
            Some((id, _)) => Probe::Wait { id: *id, before },
            None => Probe::Measure {
                since: now,
                delivered,
            },
        };
        added
    }

    /// a new stream with a range of its own, when there are fewer than [`MOST_STREAMS`], the
    /// host has a free slot and a range is large enough to share.
    fn add_stream(
        &self,
        shared: &Streams<'_>,
        running: usize,
    ) -> Option<(usize, (OwnedSemaphorePermit, OwnedSemaphorePermit))> {
        if running >= MOST_STREAMS {
            return None;
        }
        let mut board = shared.board();
        if board.stopped {
            return None;
        }
        let permits = self.try_permit(shared.job.key)?;
        let id = board.shares.len();
        board.assign(id, self.split.piece)?;
        tracing::debug!(id, "added a stream to a split download");
        Some((id, permits))
    }
}

#[cfg(test)]
mod tests;
