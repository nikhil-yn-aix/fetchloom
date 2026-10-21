//! A large file fetched as several range requests at once, each part written where it belongs.

use futures_util::future::try_join_all;
use reqwest::Response;
use tracing::{Instrument, Span};

use crate::client::{Ask, Client, Failure, protocol};
use crate::download::content_range;
use crate::error::TransferError;
use crate::url::SafeUrl;
use crate::writer::{BUFFER, Feed};

/// Files longer than this are split when the host has room.
pub(crate) const SPLIT_ABOVE: u64 = 64 << 20;

/// No part is shorter than this, so a file just over the threshold is not cut into slivers.
const PART_AT_LEAST: u64 = 32 << 20;

/// One range of the file and how much of it has been handed to the writer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Part {
    start: u64,
    end: u64,
    done: u64,
}

/// What a split fetch shares among its parts.
pub(crate) struct Split<'a> {
    pub(crate) client: &'a Client,
    pub(crate) url: &'a SafeUrl,
    pub(crate) if_range: Option<&'a str>,
    pub(crate) typed: bool,
    pub(crate) feed: Feed,
}

/// Cuts `length` bytes into at most `streams` parts of at least [`PART_AT_LEAST`] bytes.
pub(crate) fn plan(length: u64, streams: usize) -> Vec<Part> {
    let most = length.div_ceil(PART_AT_LEAST).max(1);
    let count = u64::try_from(streams.max(1)).map_or(most, |streams| streams.min(most));
    let size = length.div_ceil(count);
    (0..count)
        .map(|index| Part {
            start: index * size,
            end: ((index + 1) * size).min(length),
            done: 0,
        })
        .filter(|part| part.start < part.end)
        .collect()
}

/// The bytes from the start of the file that every part before them has finished.
pub(crate) fn prefix(parts: &[Part]) -> u64 {
    let mut kept = 0;
    for part in parts {
        kept = part.start + part.done;
        if part.start + part.done < part.end {
            break;
        }
    }
    kept
}

impl Split<'_> {
    /// Fetches every part at once, the first from `first`, the response that is already open.
    /// Each part retries on its own; the first part to give up stops the others.
    pub(crate) async fn fetch(
        &self,
        parts: &mut [Part],
        first: (Response, Span),
    ) -> Result<(), Failure> {
        let mut first = Some(first);
        try_join_all(parts.iter_mut().map(|part| self.part(part, first.take())))
            .await
            .map(|_| ())
    }

    async fn part(
        &self,
        part: &mut Part,
        mut open: Option<(Response, Span)>,
    ) -> Result<(), Failure> {
        let mut failures = 0;
        while part.start + part.done < part.end {
            let response = match open.take() {
                Some(response) => Ok(response),
                None => self.ask(part, failures + 1).await,
            };
            let pumped = match response {
                Ok((response, span)) => {
                    let start = part.done;
                    let pumped = self.pump(response, part).instrument(span.clone()).await;
                    span.record("bytes", part.done - start);
                    pumped
                }
                Err(failure) => Err(failure),
            };
            if let Err(failure) = pumped {
                failures += 1;
                self.client
                    .pause(failures, failure, self.url)
                    .await
                    .map_err(Failure::last)?;
            }
        }
        Ok(())
    }

    async fn ask(&self, part: &Part, attempt: u32) -> Result<(Response, Span), Failure> {
        let from = part.start + part.done;
        let answer = self
            .client
            .send(&Ask {
                url: self.url,
                from,
                to: Some(part.end - 1),
                if_range: self.if_range,
                attempt,
                typed: self.typed,
            })
            .await?;
        let status = answer.response.status().as_u16();
        let range = content_range(answer.response.headers());
        if status != 206 || range.map(|(start, _)| start) != Some(from) {
            return Err(Failure::last(protocol(
                &answer.url,
                "did not send the range asked for, though it offered ranges",
            )));
        }
        Ok((answer.response, answer.span))
    }

    async fn pump(&self, mut response: Response, part: &mut Part) -> Result<(), Failure> {
        let idle = self.client.config.idle;
        let mut buffer = self.feed.buffer();
        while part.start + part.done + (buffer.len() as u64) < part.end {
            let Ok(chunk) = tokio::time::timeout(idle, response.chunk()).await else {
                self.flush(part, buffer).await?;
                return Err(Failure::again(TransferError::Timeout {
                    host: self.url.host().unwrap_or_default().to_owned(),
                    during: "receiving",
                    limit: idle,
                }));
            };
            let mut bytes = match chunk {
                Ok(Some(bytes)) => bytes,
                Ok(None) => break,
                Err(error) => {
                    self.flush(part, buffer).await?;
                    return Err(self.client.failure(&error, self.url, "receiving"));
                }
            };
            let wanted = part.end - part.start - part.done - buffer.len() as u64;
            bytes.truncate(usize::try_from(wanted).unwrap_or(usize::MAX));
            self.client.counters.received(bytes.len() as u64);
            while !bytes.is_empty() {
                let piece = bytes.split_to((BUFFER - buffer.len()).min(bytes.len()));
                buffer.extend_from_slice(&piece);
                if buffer.len() == BUFFER {
                    buffer = self.flush(part, buffer).await?;
                }
            }
        }
        self.flush(part, buffer).await?;
        if part.start + part.done < part.end {
            return Err(Failure::again(TransferError::Unreachable {
                host: self.url.host().unwrap_or_default().to_owned(),
                reason: "a range ended early".to_owned(),
            }));
        }
        Ok(())
    }

    async fn flush(&self, part: &mut Part, buffer: Vec<u8>) -> Result<Vec<u8>, Failure> {
        if buffer.is_empty() {
            return Ok(buffer);
        }
        let length = buffer.len() as u64;
        self.feed
            .write(part.start + part.done, buffer)
            .await
            .map_err(|_| {
                Failure::last(TransferError::Sink {
                    url: self.url.clone(),
                    source: std::io::Error::other("the writer thread stopped"),
                })
            })?;
        part.done += length;
        Ok(self.feed.buffer())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    #[test]
    fn parts_cover_the_file_once_and_are_never_slivers() {
        for (length, streams, count) in [
            (65 * MIB, 4, 3),
            (200 * MIB, 4, 4),
            (200 * MIB, 1, 1),
            (10, 4, 1),
            (200 * MIB + 3, 3, 3),
        ] {
            let parts = plan(length, streams);
            assert_eq!(parts.len(), count, "{length} {streams}");
            assert_eq!(parts[0].start, 0);
            assert_eq!(parts.last().map(|part| part.end), Some(length));
            assert!(parts.windows(2).all(|pair| pair[0].end == pair[1].start));
        }
    }

    #[test]
    fn the_prefix_stops_at_the_first_unfinished_part() {
        let mut parts = plan(200 * MIB, 4);
        assert_eq!(prefix(&parts), 0);
        parts[0].done = parts[0].end;
        parts[1].done = 5;
        parts[2].done = parts[2].end - parts[2].start;
        assert_eq!(prefix(&parts), parts[1].start + 5);
        for part in &mut parts {
            part.done = part.end - part.start;
        }
        assert_eq!(prefix(&parts), 200 * MIB);
    }
}
