//! One file from a location into a sink: resume what can be resumed, stream through the writer
//! thread, verify every stated digest, retry what may succeed.

use fl_model::digest::Digest;
use fl_model::partial::{PartialRecord, Validator};
use reqwest::header::{self, HeaderMap};
use tracing::Instrument;

use crate::client::{Answer, Ask, Client, Failure, protocol};
use crate::error::TransferError;
use crate::hash::{Hasher, Hashes};
use crate::hosts::Slot;
use crate::retry;
use crate::sink::Sink;
use crate::split::{self, SPLIT_ABOVE, Split};
use crate::url::SafeUrl;
use crate::writer::{BUFFER, Writer, rehash};

/// The progress written into a sink's record at most this many bytes apart.
const RECORD_EVERY: u64 = 64 << 20;

/// One file to fetch.
#[derive(Clone, Debug)]
pub struct Download {
    /// Where it is.
    pub url: SafeUrl,
    /// Its length, when the source states it.
    pub length: Option<u64>,
    /// Every digest the source states, BLAKE3 included when it is pinned.
    pub stated: Vec<Digest>,
    /// The bytes at this location never change: a pinned hash or an immutable version.
    pub stable: bool,
}

/// A finished download.
#[derive(Debug)]
pub struct Downloaded<S> {
    /// The sink, holding every byte.
    pub sink: S,
    /// The file's length.
    pub bytes: u64,
    /// Its hashes, every stated one already compared.
    pub hashes: Hashes,
    /// It continued from bytes an earlier attempt kept.
    pub resumed: bool,
    /// It started again from zero after the server sent other bytes than those kept.
    pub restarted: bool,
}

enum Step {
    Done,
    Again,
    Split,
}

struct Run<'a, S> {
    client: &'a Client,
    download: &'a Download,
    writer: Writer<S>,
    offset: u64,
    saved: u64,
    validator: Validator,
    length: Option<u64>,
    resumed: bool,
    restarted: bool,
    refused_range: bool,
    failures: u32,
}

impl Client {
    /// Fetches `download` into `sink`.
    ///
    /// Bytes an earlier attempt kept in the sink are read back and hashed, then only the rest is
    /// asked for when the bytes are known not to have changed. On failure the sink keeps its
    /// bytes and a record, so a later call continues.
    ///
    /// # Errors
    ///
    /// Returns [`TransferError`] when the bytes cannot be fetched, do not match what was stated,
    /// or cannot be written.
    pub async fn download<S: Sink>(
        &self,
        download: &Download,
        sink: S,
    ) -> Result<Downloaded<S>, TransferError> {
        let span = tracing::debug_span!(
            "download",
            host = download.url.host().unwrap_or_default(),
            bytes = tracing::field::Empty,
            resumed = tracing::field::Empty,
        );
        let finished = async { Run::start(self, download, sink).await?.run().await }
            .instrument(span.clone())
            .await?;
        span.record("bytes", finished.bytes);
        span.record("resumed", finished.resumed);
        Ok(finished)
    }
}

impl<'a, S: Sink> Run<'a, S> {
    async fn start(
        client: &'a Client,
        download: &'a Download,
        sink: S,
    ) -> Result<Self, TransferError> {
        let fresh = Hasher::new(&download.stated);
        let record = sink.record().filter(|record| {
            record.received > 0
                && record.location == download.url.to_string()
                && (download.stable || record.validator != Validator::None)
        });
        let hasher = fresh.clone();
        let (sink, kept) = tokio::task::spawn_blocking(move || {
            let mut sink = sink;
            let mut hasher = hasher;
            let wanted = record.as_ref().map_or(0, |record| record.received);
            let kept = rehash(&mut sink, &mut hasher, wanted)?;
            if kept == 0 || kept < wanted {
                sink.reset()?;
                return Ok::<_, std::io::Error>((sink, None));
            }
            Ok((sink, record.map(|record| (record, hasher, kept))))
        })
        .await
        .map_err(|error| sink_error(download, std::io::Error::other(error)))?
        .map_err(|error| sink_error(download, error))?;
        let (record, hasher, kept) = match kept {
            Some((record, hasher, kept)) => (Some(record), hasher, kept),
            None => (None, fresh.clone(), 0),
        };
        let (validator, length) = record.map_or((Validator::None, download.length), |record| {
            (record.validator, record.length.or(download.length))
        });
        Ok(Self {
            client,
            download,
            writer: Writer::start(sink, hasher, fresh),
            offset: kept,
            saved: kept,
            validator,
            length,
            resumed: false,
            restarted: false,
            refused_range: false,
            failures: 0,
        })
    }

    async fn run(mut self) -> Result<Downloaded<S>, TransferError> {
        match self.drive().await {
            Ok(Step::Split) => self.finish_split().await,
            Ok(_) => self.finish().await,
            Err(error) => {
                let _ = self.writer.finish().await;
                Err(error)
            }
        }
    }

    async fn drive(&mut self) -> Result<Step, TransferError> {
        loop {
            let if_range = match &self.validator {
                Validator::StrongEtag(etag) if self.offset > 0 => Some(etag.clone()),
                _ => None,
            };
            let ask = Ask {
                url: &self.download.url,
                from: self.offset,
                to: None,
                if_range: if_range.as_deref(),
                attempt: self.failures + 1,
            };
            let step = match self.client.send(&ask).await {
                Ok(answer) => self.receive(answer).await,
                Err(failure) => Err(failure),
            };
            match step {
                Ok(Step::Again) => {}
                Ok(Step::Split) => return Ok(Step::Split),
                Ok(Step::Done) => match self.verify().await? {
                    Step::Again => {}
                    _ => return Ok(Step::Done),
                },
                Err(failure) => self.recover(failure).await?,
            }
        }
    }

    async fn receive(&mut self, answer: Answer) -> Result<Step, Failure> {
        let status = answer.response.status().as_u16();
        let url = answer.url.clone();
        match status {
            206 if self.offset > 0 => self.resume(answer).await,
            200 => {
                if self.offset > 0 {
                    tracing::warn!(
                        "{url} ignored the range asked for, so the download starts again"
                    );
                    self.restart().await?;
                }
                self.validator = validator(answer.response.headers());
                self.length = answer.response.content_length().or(self.download.length);
                if let Some(length) = self.length {
                    self.writer
                        .allocate(length)
                        .await
                        .map_err(|error| self.sink(error))?;
                }
                if let Some(slots) = self.spare_slots(&answer) {
                    return self.split(answer, slots).await;
                }
                self.stream(answer).await
            }
            416 if self.offset > 0 => self.unsatisfiable(answer.response.headers(), &url).await,
            401 | 403 if answer.credentialed => {
                Err(Failure::last(TransferError::AuthRejected { url, status }))
            }
            401 | 403 => Err(Failure::last(TransferError::AuthRequired { url, status })),
            _ if retry::transient_status(status) => {
                let retry_after = answer
                    .response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| retry::retry_after(value, self.client.timer.now()));
                Err(Failure {
                    error: TransferError::Status { url, status },
                    transient: true,
                    retry_after,
                })
            }
            _ => Err(Failure::last(TransferError::Status { url, status })),
        }
    }

    async fn resume(&mut self, answer: Answer) -> Result<Step, Failure> {
        let headers = answer.response.headers();
        let Some((start, total)) = content_range(headers) else {
            return Err(Failure::last(protocol(
                &answer.url,
                "sent a range without saying which",
            )));
        };
        if start != self.offset {
            return Err(Failure::last(protocol(
                &answer.url,
                "sent another range than the one asked for",
            )));
        }
        let now = validator(headers);
        let weak = matches!(
            self.validator,
            Validator::WeakEtag(_) | Validator::LastModified(_)
        );
        if weak && now != self.validator {
            tracing::warn!(
                "{} changed since the bytes kept, so the download starts again",
                answer.url
            );
            self.restart().await?;
            return Ok(Step::Again);
        }
        self.length = total.or(self.length);
        self.resumed = true;
        self.client.counters.resume();
        self.stream(answer).await
    }

    async fn unsatisfiable(&mut self, headers: &HeaderMap, url: &SafeUrl) -> Result<Step, Failure> {
        if content_range_length(headers) == Some(self.offset) {
            self.length = Some(self.offset);
            return Ok(Step::Done);
        }
        if self.refused_range {
            return Err(Failure::last(protocol(
                url,
                "refused the range asked for twice",
            )));
        }
        self.refused_range = true;
        self.restart().await?;
        Ok(Step::Again)
    }

    async fn stream(&mut self, answer: Answer) -> Result<Step, Failure> {
        let span = answer.span.clone();
        let start = self.offset;
        let streamed = self.stream_body(answer).instrument(span.clone()).await;
        span.record("bytes", self.offset.saturating_sub(start));
        streamed
    }

    async fn stream_body(&mut self, mut answer: Answer) -> Result<Step, Failure> {
        let mut buffer = self.writer.buffer();
        loop {
            let idle = self.client.config.idle;
            let Ok(chunk) = tokio::time::timeout(idle, answer.response.chunk()).await else {
                self.flush(buffer).await?;
                return Err(Failure::again(TransferError::Timeout {
                    host: answer.url.host().unwrap_or_default().to_owned(),
                    during: "receiving",
                    limit: idle,
                }));
            };
            match chunk {
                Ok(Some(mut bytes)) => {
                    self.client.counters.received(bytes.len() as u64);
                    while !bytes.is_empty() {
                        let room = BUFFER - buffer.len();
                        let piece = bytes.split_to(room.min(bytes.len()));
                        buffer.extend_from_slice(&piece);
                        if buffer.len() == BUFFER {
                            buffer = self.flush(buffer).await?;
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.flush(buffer).await?;
                    return Err(self.client.failure(&error, &answer.url, "receiving"));
                }
            }
        }
        self.flush(buffer).await?;
        match self.length {
            Some(length) if length != self.offset => {
                Err(Failure::again(TransferError::Unreachable {
                    host: answer.url.host().unwrap_or_default().to_owned(),
                    reason: format!("the body ended after {} of {length} bytes", self.offset),
                }))
            }
            _ => Ok(Step::Done),
        }
    }

    async fn flush(&mut self, buffer: Vec<u8>) -> Result<Vec<u8>, Failure> {
        if buffer.is_empty() {
            return Ok(buffer);
        }
        let start = self.offset;
        self.offset += buffer.len() as u64;
        self.writer
            .write(start, buffer)
            .await
            .map_err(|error| self.sink(error))?;
        if self.offset - self.saved >= RECORD_EVERY {
            self.save().await.map_err(Failure::last)?;
        }
        Ok(self.writer.buffer())
    }

    fn spare_slots(&self, answer: &Answer) -> Option<Vec<Slot>> {
        let stable = self.download.stable || matches!(self.validator, Validator::StrongEtag(_));
        let ranges = answer
            .response
            .headers()
            .get(header::ACCEPT_RANGES)
            .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"bytes"));
        if self.offset > 0 || !stable || !ranges || self.length? <= SPLIT_ABOVE {
            return None;
        }
        let hosts = self.client.hosts();
        let host = self.download.url.host()?;
        let slots: Vec<Slot> = (1..self.client.config.per_host)
            .map_while(|_| hosts.try_take(host))
            .collect();
        (!slots.is_empty()).then_some(slots)
    }

    async fn split(&mut self, answer: Answer, slots: Vec<Slot>) -> Result<Step, Failure> {
        let length = self.offset.max(self.length.unwrap_or_default());
        self.writer
            .unhashed()
            .await
            .map_err(|error| self.sink(error))?;
        let mut parts = split::plan(length, 1 + slots.len());
        let if_range = match &self.validator {
            Validator::StrongEtag(etag) => Some(etag.clone()),
            _ => None,
        };
        let fetched = Split {
            client: self.client,
            url: &self.download.url,
            if_range: if_range.as_deref(),
            feed: self.writer.feed(),
        }
        .fetch(&mut parts, (answer.response, answer.span))
        .await;
        drop(slots);
        self.offset = split::prefix(&parts);
        match fetched {
            Ok(()) => Ok(Step::Split),
            Err(failure) => {
                self.save().await.map_err(Failure::last)?;
                Err(Failure::last(failure.error))
            }
        }
    }

    async fn finish_split(self) -> Result<Downloaded<S>, TransferError> {
        let Self {
            download,
            writer,
            offset: length,
            restarted,
            ..
        } = self;
        let (sink, _) = writer
            .finish()
            .await
            .map_err(|error| sink_error(download, error))?;
        let stated = download.stated.clone();
        let (sink, hashes, read) = tokio::task::spawn_blocking(move || {
            let mut sink = sink;
            let mut hasher = Hasher::new(&stated);
            let read = rehash(&mut sink, &mut hasher, length)?;
            Ok::<_, std::io::Error>((sink, hasher.finish(), read))
        })
        .await
        .map_err(|error| sink_error(download, std::io::Error::other(error)))?
        .map_err(|error| sink_error(download, error))?;
        let url = &download.url;
        let expected = download.length.unwrap_or(length);
        if read != length || expected != length {
            return Err(TransferError::Length {
                url: url.clone(),
                expected,
                received: read,
            });
        }
        hashes
            .check(&download.stated)
            .map_err(|source| TransferError::Mismatch {
                url: url.clone(),
                source,
            })?;
        Ok(Downloaded {
            sink,
            bytes: length,
            hashes,
            resumed: false,
            restarted,
        })
    }

    async fn verify(&mut self) -> Result<Step, TransferError> {
        let url = &self.download.url;
        if let Some(expected) = self.download.length
            && expected != self.offset
        {
            return Err(TransferError::Length {
                url: url.clone(),
                expected,
                received: self.offset,
            });
        }
        let hashes = self
            .writer
            .hashes()
            .await
            .map_err(|error| sink_error(self.download, error))?;
        match hashes.check(&self.download.stated) {
            Ok(()) => Ok(Step::Done),
            Err(_) if self.resumed && !self.restarted && !self.download.stable => {
                tracing::warn!("{url} does not match after resuming, so the download starts again");
                self.restart().await.map_err(|failure| failure.error)?;
                Ok(Step::Again)
            }
            Err(source) => Err(TransferError::Mismatch {
                url: url.clone(),
                source,
            }),
        }
    }

    async fn recover(&mut self, failure: Failure) -> Result<(), TransferError> {
        self.failures += 1;
        if self.offset > 0 {
            if self.download.stable || self.validator != Validator::None {
                self.save().await?;
            } else {
                self.restart().await.map_err(|failure| failure.error)?;
            }
        }
        self.client
            .pause(self.failures, failure, &self.download.url)
            .await
    }

    async fn restart(&mut self) -> Result<(), Failure> {
        self.writer
            .reset()
            .await
            .map_err(|error| self.sink(error))?;
        self.offset = 0;
        self.saved = 0;
        self.restarted = true;
        self.resumed = false;
        Ok(())
    }

    async fn save(&mut self) -> Result<(), TransferError> {
        let record = PartialRecord {
            location: self.download.url.to_string(),
            length: self.length,
            validator: self.validator.clone(),
            received: self.offset,
        };
        self.writer
            .save(record)
            .await
            .map_err(|error| sink_error(self.download, error))?;
        self.saved = self.offset;
        Ok(())
    }

    async fn finish(self) -> Result<Downloaded<S>, TransferError> {
        let bytes = self.offset;
        let (resumed, restarted) = (self.resumed, self.restarted);
        let download = self.download;
        let (sink, hashes) = self
            .writer
            .finish()
            .await
            .map_err(|error| sink_error(download, error))?;
        Ok(Downloaded {
            sink,
            bytes,
            hashes,
            resumed,
            restarted,
        })
    }

    fn sink(&self, error: std::io::Error) -> Failure {
        Failure::last(sink_error(self.download, error))
    }
}

fn sink_error(download: &Download, source: std::io::Error) -> TransferError {
    TransferError::Sink {
        url: download.url.clone(),
        source,
    }
}

/// What the response says identifies its bytes.
fn validator(headers: &HeaderMap) -> Validator {
    let text = |name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    match (text(header::ETAG), text(header::LAST_MODIFIED)) {
        (Some(etag), _) if etag.starts_with("W/") => Validator::WeakEtag(etag),
        (Some(etag), _) => Validator::StrongEtag(etag),
        (None, Some(modified)) => Validator::LastModified(modified),
        (None, None) => Validator::None,
    }
}

/// The first byte and the total length a `Content-Range: bytes a-b/total` names.
pub(crate) fn content_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()))
}

/// The length a `Content-Range: bytes */total` names.
fn content_range_length(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get(header::CONTENT_RANGE)?.to_str().ok()?;
    value.strip_prefix("bytes */")?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().unwrap());
        }
        map
    }

    #[test]
    fn validators_prefer_the_etag_and_tell_weak_from_strong() {
        assert_eq!(
            validator(&headers(&[("etag", "\"a\""), ("last-modified", "x")])),
            Validator::StrongEtag("\"a\"".to_owned())
        );
        assert_eq!(
            validator(&headers(&[("etag", "W/\"a\"")])),
            Validator::WeakEtag("W/\"a\"".to_owned())
        );
        assert_eq!(
            validator(&headers(&[("last-modified", "x")])),
            Validator::LastModified("x".to_owned())
        );
        assert_eq!(validator(&headers(&[])), Validator::None);
    }

    #[test]
    fn content_ranges_are_read() {
        let range = |value| content_range(&headers(&[("content-range", value)]));
        assert_eq!(range("bytes 5-9/10"), Some((5, Some(10))));
        assert_eq!(range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(range("bytes x-9/10"), None);
        assert_eq!(range("items 5-9/10"), None);
        let length = |value| content_range_length(&headers(&[("content-range", value)]));
        assert_eq!(length("bytes */10"), Some(10));
        assert_eq!(length("bytes 0-1/10"), None);
    }
}
