//! a bounded prefix queue lets disk writes proceed while a separate thread hashes earlier bytes.

use std::fs::File;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use bytes::Bytes;

use super::hash_range;
use crate::digest::{Hasher, Md5, Sha256};

const QUEUED: usize = 16;

/// how much the pipe gathers before waking the worker. a message per 16 KiB response chunk spent
/// more time waking threads than hashing.
const BATCH: usize = 1024 * 1024;

enum Message {
    Chunks(Vec<Bytes>),
    Range {
        start: u64,
        end: u64,
    },
    Checkpoint {
        received: u64,
        reply: SyncSender<Sha256>,
    },
}

pub(super) struct HashPipe {
    sender: Option<SyncSender<Message>>,
    worker: Option<JoinHandle<io::Result<(Hasher, u64)>>>,
    /// chunks not sent yet. every other message sends them first, so order holds.
    gathered: Vec<Bytes>,
    gathered_bytes: usize,
    /// a small download's hash, kept on the writer's thread instead of a worker.
    inline: Option<Inline>,
}

struct Inline {
    file: File,
    hasher: Hasher,
    received: u64,
}

impl HashPipe {
    pub(super) fn new(hasher: Hasher, file: File, received: u64) -> io::Result<Self> {
        let (sender, inbox) = mpsc::sync_channel(QUEUED);
        let worker = thread::Builder::new()
            .name("fetchloom-hash".to_owned())
            .spawn(move || hash(&file, hasher, received, &inbox))?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            gathered: Vec::new(),
            gathered_bytes: 0,
            inline: None,
        })
    }

    /// hashes on the caller's thread, for downloads too small to be worth a worker.
    pub(super) fn inline(hasher: Hasher, file: File, received: u64) -> Self {
        Self {
            sender: None,
            worker: None,
            gathered: Vec::new(),
            gathered_bytes: 0,
            inline: Some(Inline {
                file,
                hasher,
                received,
            }),
        }
    }

    pub(super) fn update(&mut self, bytes: Bytes) -> io::Result<()> {
        if let Some(inline) = &mut self.inline {
            inline.hasher.update(&bytes);
            inline.received += bytes.len() as u64;
            return Ok(());
        }
        self.gathered_bytes += bytes.len();
        self.gathered.push(bytes);
        if self.gathered_bytes >= BATCH {
            self.send_gathered()?;
        }
        Ok(())
    }

    pub(super) fn range(&mut self, start: u64, end: u64) -> io::Result<()> {
        if let Some(inline) = &mut self.inline {
            same_prefix(inline.received, start)?;
            hash_range(&inline.file, &mut inline.hasher, start, end)?;
            inline.received = end;
            return Ok(());
        }
        self.send_gathered()?;
        self.send(Message::Range { start, end })
    }

    pub(super) fn sha256_so_far(&mut self, received: u64) -> io::Result<Sha256> {
        if let Some(inline) = &self.inline {
            same_prefix(inline.received, received)?;
            return Ok(inline.hasher.sha256_so_far());
        }
        self.send_gathered()?;
        let (reply, result) = mpsc::sync_channel(1);
        self.send(Message::Checkpoint { received, reply })?;
        result.recv().map_err(|_| self.stopped())
    }

    pub(super) fn finish(mut self, received: u64) -> io::Result<(Sha256, Option<Md5>)> {
        self.send_gathered()?;
        let (hasher, reached) = self.join()?;
        same_prefix(reached, received)?;
        Ok(hasher.finish())
    }

    fn send_gathered(&mut self) -> io::Result<()> {
        if self.gathered.is_empty() {
            return Ok(());
        }
        self.gathered_bytes = 0;
        let chunks = std::mem::take(&mut self.gathered);
        self.send(Message::Chunks(chunks))
    }

    fn send(&mut self, message: Message) -> io::Result<()> {
        if self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(message).is_ok())
        {
            Ok(())
        } else {
            Err(self.stopped())
        }
    }

    fn stopped(&mut self) -> io::Error {
        match self.join() {
            Ok(_) => stopped(),
            Err(error) => error,
        }
    }

    fn join(&mut self) -> io::Result<(Hasher, u64)> {
        if let Some(inline) = self.inline.take() {
            return Ok((inline.hasher, inline.received));
        }
        // closing the queue lets the worker drain it before its file handle and claim go away.
        drop(self.sender.take());
        let worker = self.worker.take().ok_or_else(stopped)?;
        worker.join().map_err(|_| stopped())?
    }
}

impl Drop for HashPipe {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.join();
        }
    }
}

fn stopped() -> io::Error {
    io::Error::other("the download hash worker stopped unexpectedly")
}

fn same_prefix(hashed: u64, received: u64) -> io::Result<()> {
    if hashed == received {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the download hash does not cover the recorded prefix",
        ))
    }
}

fn hash(
    file: &File,
    mut hasher: Hasher,
    mut received: u64,
    inbox: &Receiver<Message>,
) -> io::Result<(Hasher, u64)> {
    while let Ok(message) = inbox.recv() {
        match message {
            Message::Chunks(chunks) => {
                for bytes in chunks {
                    hasher.update(&bytes);
                    received += bytes.len() as u64;
                }
            }
            Message::Range { start, end } => {
                same_prefix(received, start)?;
                hash_range(file, &mut hasher, start, end)?;
                received = end;
            }
            Message::Checkpoint {
                received: written,
                reply,
            } => {
                // a fifo barrier excludes later chunks and waits for every earlier byte.
                same_prefix(received, written)?;
                let _ = reply.send(hasher.sha256_so_far());
            }
        }
    }
    Ok((hasher, received))
}

#[cfg(test)]
mod tests;
