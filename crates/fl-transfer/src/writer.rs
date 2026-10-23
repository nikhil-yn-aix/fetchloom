//! The thread that owns a download's sink: it writes and hashes what the network hands it, so
//! the network never waits on the disk and memory stays bounded.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use fl_model::partial::PartialRecord;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::hash::{Hasher, Hashes};
use crate::sink::Sink;

/// The size of every buffer between the network and the writer.
pub(crate) const BUFFER: usize = 256 * 1024;

/// Buffers waiting for the writer at most.
const DEPTH: usize = 8;

enum Command {
    Write { offset: u64, buffer: Vec<u8> },
    Reset,
    Allocate(u64),
    Save(PartialRecord),
    Hashes(oneshot::Sender<Hashes>),
    Unhashed,
}

/// A handle that hands buffers to the writer. Clones feed the same writer, so the parts of a
/// split download share it and its bound on memory.
#[derive(Clone)]
pub(crate) struct Feed {
    commands: mpsc::Sender<Command>,
    spare: Arc<Mutex<mpsc::UnboundedReceiver<Vec<u8>>>>,
}

/// The writer thread is gone; [`Writer::stopped`] says why.
#[derive(Debug)]
pub(crate) struct Stopped;

impl Feed {
    /// An empty buffer of [`BUFFER`] bytes, reused when the writer has handed one back.
    pub(crate) fn buffer(&self) -> Vec<u8> {
        self.spare
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .unwrap_or_else(|_| Vec::with_capacity(BUFFER))
    }

    /// Hands `buffer` to the writer, to be written at `offset`.
    pub(crate) async fn write(&self, offset: u64, buffer: Vec<u8>) -> Result<(), Stopped> {
        self.commands
            .send(Command::Write { offset, buffer })
            .await
            .map_err(|_| Stopped)
    }
}

/// The network side of a writer thread.
pub(crate) struct Writer<S> {
    feed: Feed,
    thread: Option<JoinHandle<io::Result<(S, Hasher)>>>,
}

impl<S: Sink> Writer<S> {
    /// Starts the thread with `sink`, whose kept bytes `hasher` has already seen. `fresh` is a
    /// hasher that has seen nothing, used after a reset.
    pub(crate) fn start(sink: S, hasher: Hasher, fresh: Hasher) -> Self {
        let (commands, received) = mpsc::channel(DEPTH);
        let (returned, spare) = mpsc::unbounded_channel();
        let thread =
            tokio::task::spawn_blocking(move || run(sink, hasher, &fresh, received, &returned));
        Self {
            feed: Feed {
                commands,
                spare: Arc::new(Mutex::new(spare)),
            },
            thread: Some(thread),
        }
    }

    /// A handle for handing buffers over.
    pub(crate) fn feed(&self) -> Feed {
        self.feed.clone()
    }

    /// An empty buffer of [`BUFFER`] bytes.
    pub(crate) fn buffer(&self) -> Vec<u8> {
        self.feed.buffer()
    }

    /// Hands `buffer` to the writer, to be written at `offset`.
    pub(crate) async fn write(&mut self, offset: u64, buffer: Vec<u8>) -> io::Result<()> {
        self.send(Command::Write { offset, buffer }).await
    }

    /// Drops every byte held, and the hashes with them.
    pub(crate) async fn reset(&mut self) -> io::Result<()> {
        self.send(Command::Reset).await
    }

    /// Makes room for `length` bytes.
    pub(crate) async fn allocate(&mut self, length: u64) -> io::Result<()> {
        self.send(Command::Allocate(length)).await
    }

    /// Saves `record` once everything handed over before it is written.
    pub(crate) async fn save(&mut self, record: PartialRecord) -> io::Result<()> {
        self.send(Command::Save(record)).await
    }

    /// Stops hashing, for writes that arrive out of order.
    pub(crate) async fn unhashed(&mut self) -> io::Result<()> {
        self.send(Command::Unhashed).await
    }

    /// The hashes of everything written so far.
    pub(crate) async fn hashes(&mut self) -> io::Result<Hashes> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Hashes(reply)).await?;
        match answer.await {
            Ok(hashes) => Ok(hashes),
            Err(_) => Err(self.stopped().await),
        }
    }

    /// Waits for every write, and hands back the sink and the hashes.
    pub(crate) async fn finish(mut self) -> io::Result<(S, Hashes)> {
        let thread = self.thread.take();
        drop(self);
        let (sink, hasher) = join(thread).await?;
        Ok((sink, hasher.finish()))
    }

    /// The error that stopped the thread.
    pub(crate) async fn stopped(&mut self) -> io::Error {
        match join(self.thread.take()).await {
            Err(error) => error,
            Ok(_) => stopped(),
        }
    }

    async fn send(&mut self, command: Command) -> io::Result<()> {
        if self.feed.commands.send(command).await.is_ok() {
            return Ok(());
        }
        Err(self.stopped().await)
    }
}

async fn join<S>(thread: Option<JoinHandle<io::Result<(S, Hasher)>>>) -> io::Result<(S, Hasher)> {
    thread
        .ok_or_else(stopped)?
        .await
        .map_err(io::Error::other)?
}

fn stopped() -> io::Error {
    io::Error::other("the writer thread stopped")
}

fn run<S: Sink>(
    mut sink: S,
    mut hasher: Hasher,
    fresh: &Hasher,
    mut received: mpsc::Receiver<Command>,
    returned: &mpsc::UnboundedSender<Vec<u8>>,
) -> io::Result<(S, Hasher)> {
    let mut hash = true;
    while let Some(command) = received.blocking_recv() {
        match command {
            Command::Write { offset, mut buffer } => {
                sink.write_at(offset, &buffer)?;
                if hash {
                    hasher.update(&buffer);
                }
                buffer.clear();
                let _ = returned.send(buffer);
            }
            Command::Reset => {
                sink.reset()?;
                hasher = fresh.clone();
            }
            Command::Allocate(length) => sink.allocate(length)?,
            Command::Save(record) => sink.save(&record)?,
            Command::Hashes(reply) => {
                let _ = reply.send(hasher.clone().finish());
            }
            Command::Unhashed => hash = false,
        }
    }
    Ok((sink, hasher))
}

/// Feeds `hasher` the first `length` bytes of `sink`, reading them back in order. Returns how
/// many bytes were there, which is less than `length` when the sink holds fewer.
pub(crate) fn rehash<S: Sink>(sink: &mut S, hasher: &mut Hasher, length: u64) -> io::Result<u64> {
    let mut buffer = vec![0; BUFFER];
    let mut offset = 0;
    while offset < length {
        let want = usize::try_from(length - offset).map_or(BUFFER, |left| left.min(BUFFER));
        let read = match sink.read_at(offset, &mut buffer[..want]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Full;

    impl Sink for Full {
        fn record(&self) -> Option<PartialRecord> {
            None
        }

        fn allocate(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }

        fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> io::Result<()> {
            Err(io::Error::other("the disk is full"))
        }

        fn read_at(&mut self, _offset: u64, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn reset(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn hashes_after_a_failed_write_report_the_sink_error() {
        let mut writer = Writer::start(Full, Hasher::new(&[]), Hasher::new(&[]));
        writer.write(0, vec![1; 8]).await.unwrap();
        let error = writer.hashes().await.unwrap_err();
        assert_eq!(error.to_string(), "the disk is full");
        let later = writer.write(0, vec![1; 8]).await.unwrap_err();
        assert_eq!(later.to_string(), "the writer thread stopped");
        let again = writer.allocate(8).await.unwrap_err();
        assert_eq!(again.to_string(), "the writer thread stopped");
    }

    #[derive(Default)]
    struct Probe {
        bytes: Vec<u8>,
        allocated: Option<u64>,
        saved: Option<PartialRecord>,
    }

    impl Sink for Probe {
        fn record(&self) -> Option<PartialRecord> {
            None
        }

        fn allocate(&mut self, length: u64) -> io::Result<()> {
            self.allocated = Some(length);
            Ok(())
        }

        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
            let start = usize::try_from(offset).map_err(io::Error::other)?;
            if self.bytes.len() < start + bytes.len() {
                self.bytes.resize(start + bytes.len(), 0);
            }
            self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }

        fn read_at(&mut self, _offset: u64, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }

        fn reset(&mut self) -> io::Result<()> {
            self.bytes.clear();
            Ok(())
        }

        fn save(&mut self, record: &PartialRecord) -> io::Result<()> {
            self.saved = Some(record.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn every_command_reaches_the_sink_and_buffers_come_back_empty() {
        let record = PartialRecord {
            location: "http://a/b".to_owned(),
            length: Some(8),
            validator: fl_model::partial::Validator::None,
            received: 4,
        };
        let mut writer = Writer::start(Probe::default(), Hasher::new(&[]), Hasher::new(&[]));
        assert!(writer.buffer().capacity() >= 256 * 1024);
        assert!(writer.feed().buffer().capacity() >= 256 * 1024);
        writer.allocate(8).await.unwrap();
        writer.write(0, vec![1; 4]).await.unwrap();
        writer.save(record.clone()).await.unwrap();
        writer.unhashed().await.unwrap();
        writer.write(4, vec![2; 4]).await.unwrap();
        let (sink, hashes) = writer.finish().await.unwrap();
        assert_eq!(sink.allocated, Some(8));
        assert_eq!(sink.saved, Some(record));
        assert_eq!(sink.bytes, [1, 1, 1, 1, 2, 2, 2, 2]);
        assert_eq!(hashes.blake3, *blake3::hash(&[1; 4]).as_bytes());
    }

    #[tokio::test]
    async fn a_returned_buffer_is_reused() {
        let mut writer = Writer::start(Probe::default(), Hasher::new(&[]), Hasher::new(&[]));
        let mut buffer = writer.buffer();
        buffer.extend_from_slice(&[9; 16]);
        let address = buffer.as_ptr();
        writer.write(0, buffer).await.unwrap();
        writer.hashes().await.unwrap();
        let back = writer.buffer();
        assert!(back.is_empty());
        assert_eq!(back.as_ptr(), address);
    }

    struct Script {
        reads: std::collections::VecDeque<io::Result<usize>>,
        asked: Vec<usize>,
    }

    impl Script {
        fn new(reads: Vec<io::Result<usize>>) -> Self {
            Self {
                reads: reads.into(),
                asked: Vec::new(),
            }
        }
    }

    impl Sink for Script {
        fn record(&self) -> Option<PartialRecord> {
            None
        }

        fn allocate(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }

        fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn read_at(&mut self, _offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
            self.asked.push(buffer.len());
            let read = self.reads.pop_front().unwrap_or(Ok(0))?;
            buffer[..read].fill(1);
            Ok(read)
        }

        fn reset(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn rehash_reads_exactly_the_length_asked_for() {
        let mut sink = Script::new(vec![Ok(7), Ok(3)]);
        let mut hasher = Hasher::new(&[]);
        assert_eq!(rehash(&mut sink, &mut hasher, 10).unwrap(), 10);
        assert_eq!(sink.asked, [10, 3]);
        assert_eq!(hasher.finish().blake3, *blake3::hash(&[1; 10]).as_bytes());

        let mut sink = Script::new(vec![Ok(10), Ok(10)]);
        assert_eq!(rehash(&mut sink, &mut Hasher::new(&[]), 10).unwrap(), 10);
        assert_eq!(sink.asked, [10]);

        let mut sink = Script::new(vec![Ok(4)]);
        assert_eq!(rehash(&mut sink, &mut Hasher::new(&[]), 10).unwrap(), 4);
        assert_eq!(rehash(&mut sink, &mut Hasher::new(&[]), 0).unwrap(), 0);
    }

    #[test]
    fn rehash_retries_an_interrupted_read_and_fails_on_any_other_error() {
        let mut sink = Script::new(vec![Err(io::ErrorKind::Interrupted.into()), Ok(5)]);
        assert_eq!(rehash(&mut sink, &mut Hasher::new(&[]), 5).unwrap(), 5);
        assert_eq!(sink.asked, [5, 5]);

        let mut sink = Script::new(vec![Err(io::ErrorKind::PermissionDenied.into()), Ok(5)]);
        let error = rehash(&mut sink, &mut Hasher::new(&[]), 5).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
