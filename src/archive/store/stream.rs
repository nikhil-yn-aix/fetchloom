//! reusable byte buffers let an archive reader continue while a writer stores a large member.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};

use crate::cache::{NewObject, Staging};

const BLOCK: usize = 256 << 10;
const BUFFERS: usize = 3;

enum Chunk {
    Bytes(Vec<u8>),
    End,
}

pub(super) struct Sending {
    chunks: SyncSender<Chunk>,
    buffers: Receiver<Vec<u8>>,
}

pub(super) struct Receiving {
    prefix: Vec<u8>,
    chunks: Receiver<Chunk>,
    buffers: SyncSender<Vec<u8>>,
}

pub(super) fn channel(prefix: Vec<u8>) -> (Sending, Receiving) {
    let (chunks, received) = mpsc::sync_channel(BUFFERS - 1);
    let (returned, buffers) = mpsc::sync_channel(BUFFERS);
    for _ in 0..BUFFERS {
        let _ = returned.send(Vec::with_capacity(BLOCK));
    }
    (
        Sending { chunks, buffers },
        Receiving {
            prefix,
            chunks: received,
            buffers: returned,
        },
    )
}

impl Sending {
    pub(super) fn finish(self) -> io::Result<()> {
        self.chunks.send(Chunk::End).map_err(|_| stopped())
    }
}

impl Write for Sending {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for part in bytes.chunks(BLOCK) {
            let mut buffer = self.buffers.recv().map_err(|_| stopped())?;
            buffer.clear();
            buffer.extend_from_slice(part);
            self.chunks
                .send(Chunk::Bytes(buffer))
                .map_err(|_| stopped())?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Receiving {
    pub(super) fn store(self, staging: &Arc<Staging>) -> io::Result<Option<NewObject>> {
        let Receiving {
            prefix,
            chunks,
            buffers,
        } = self;
        let mut object = staging.new_object()?;
        object.write_all(&prefix)?;
        drop(prefix);
        while let Ok(chunk) = chunks.recv() {
            match chunk {
                Chunk::Bytes(mut bytes) => {
                    object.write_all(&bytes)?;
                    bytes.clear();
                    let _ = buffers.send(bytes);
                }
                Chunk::End => return Ok(Some(object)),
            }
        }
        Ok(None)
    }
}

fn stopped() -> io::Error {
    io::Error::other("the archive member's writer stopped")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::thread;

    use super::*;

    #[test]
    fn writes_larger_than_one_buffer_reuse_the_same_bounded_storage() {
        let (mut output, input) = channel(Vec::new());
        let worker = thread::spawn(move || {
            let mut pointers = BTreeSet::new();
            let mut bytes = 0;
            loop {
                match input.chunks.recv().unwrap() {
                    Chunk::Bytes(buffer) => {
                        assert!(buffer.len() <= BLOCK);
                        assert!(buffer.iter().all(|byte| *byte == b'a'));
                        bytes += buffer.len();
                        pointers.insert(buffer.as_ptr() as usize);
                        let _ = input.buffers.send(buffer);
                    }
                    Chunk::End => return (bytes, pointers.len()),
                }
            }
        });
        let bytes = vec![b'a'; 17 * BLOCK + 9];
        output.write_all(&bytes).unwrap();
        output.finish().unwrap();
        let (received, allocations) = worker.join().unwrap();
        assert_eq!(received, bytes.len());
        assert!(
            allocations <= BUFFERS,
            "no new buffer is allocated per decoded block"
        );
    }
}
