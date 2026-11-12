//! the optional MD5 lane shares a batch's bytes and an extra download-hashing CPU slot.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use bytes::Bytes;
use md5::Digest as _;

use super::{same_prefix, stopped};
use crate::digest::Hasher;

pub(super) struct Workers {
    limit: usize,
    active: AtomicUsize,
}

impl Workers {
    pub(super) fn new(limit: usize) -> Workers {
        Workers {
            limit: limit.max(1),
            active: AtomicUsize::new(0),
        }
    }

    pub(super) fn enter(self: &Arc<Self>) -> Slot {
        self.active.fetch_add(1, Ordering::Relaxed);
        Slot(Arc::clone(self))
    }

    fn extra(self: &Arc<Self>) -> Option<Slot> {
        self.active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .ok()
            .map(|_| Slot(Arc::clone(self)))
    }

    pub(super) fn crowded(&self) -> bool {
        self.active.load(Ordering::Relaxed) > self.limit
    }
}

pub(super) struct Slot(Arc<Workers>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(super) enum Input {
    Chunks(Arc<Vec<Bytes>>),
    Checkpoint {
        received: u64,
        reply: SyncSender<()>,
    },
    #[cfg(test)]
    Pause {
        ready: SyncSender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    Closing {
        ready: SyncSender<()>,
        release: Receiver<()>,
    },
}

pub(super) type Hashed = io::Result<(md5::Md5, u64)>;
pub(super) type Spawn = fn(Work) -> io::Result<JoinHandle<Hashed>>;

pub(super) struct Work {
    state: md5::Md5,
    received: u64,
    inbox: Receiver<Input>,
    slot: Slot,
}

impl Work {
    fn run(self) -> Hashed {
        let Work {
            mut state,
            mut received,
            inbox,
            slot: _slot,
        } = self;
        #[cfg(test)]
        let mut closing = None;
        while let Ok(input) = inbox.recv() {
            match input {
                Input::Chunks(chunks) => {
                    for bytes in chunks.iter() {
                        state.update(bytes);
                        received += bytes.len() as u64;
                    }
                }
                Input::Checkpoint {
                    received: written,
                    reply,
                } => {
                    same_prefix(received, written)?;
                    let _ = reply.send(());
                }
                #[cfg(test)]
                Input::Pause { ready, release } => super::tests::wait_for_release(&ready, &release),
                #[cfg(test)]
                Input::Closing { ready, release } => closing = Some((ready, release)),
            }
        }
        #[cfg(test)]
        if let Some((ready, release)) = closing {
            super::tests::wait_for_release(&ready, &release);
        }
        Ok((state, received))
    }
}

pub(super) fn spawn(work: Work) -> io::Result<JoinHandle<Hashed>> {
    thread::Builder::new()
        .name("fetchloom-md5".to_owned())
        .spawn(move || work.run())
}

pub(super) struct Lane {
    pub(super) sender: Option<SyncSender<Input>>,
    worker: Option<JoinHandle<Hashed>>,
}

impl Lane {
    pub(super) fn start(
        hasher: &mut Hasher,
        received: u64,
        workers: &Arc<Workers>,
        start: Spawn,
    ) -> Option<Lane> {
        let state = hasher.take_md5()?;
        let Some(slot) = workers.extra() else {
            hasher.restore_md5(state);
            return None;
        };
        let (sender, inbox) = mpsc::sync_channel(1);
        let work = Work {
            state: state.clone(),
            received,
            inbox,
            slot,
        };
        match start(work) {
            Ok(worker) => Some(Lane {
                sender: Some(sender),
                worker: Some(worker),
            }),
            Err(error) => {
                tracing::debug!(%error, "keeping both checksums on one hash worker");
                hasher.restore_md5(state);
                None
            }
        }
    }

    pub(super) fn send(&mut self, input: Input) -> io::Result<()> {
        if self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(input).is_ok())
        {
            Ok(())
        } else {
            match self.join() {
                Ok(_) => Err(stopped()),
                Err(error) => Err(error),
            }
        }
    }

    pub(super) fn checkpoint(&mut self, received: u64) -> io::Result<()> {
        let (reply, result) = mpsc::sync_channel(1);
        self.send(Input::Checkpoint { received, reply })?;
        result.recv().map_err(|_| match self.join() {
            Ok(_) => stopped(),
            Err(error) => error,
        })
    }

    pub(super) fn finish(mut self) -> Hashed {
        self.join()
    }

    fn join(&mut self) -> Hashed {
        drop(self.sender.take());
        self.worker
            .take()
            .ok_or_else(stopped)?
            .join()
            .map_err(|_| stopped())?
    }
}

impl Drop for Lane {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.join();
        }
    }
}
