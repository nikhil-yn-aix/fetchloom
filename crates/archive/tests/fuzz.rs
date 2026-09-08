//! Every entry of the hostile corpus, damaged deterministically and driven
//! through the reader, which must name what it refused rather than escape.

use blake3 as _;
use bzip2 as _;
use fetchloom_platform as _;
use flate2 as _;
use lzma_rust2 as _;
use tar as _;
use tempfile as _;
use zip as _;
use zstd as _;

use std::io::{Cursor, Read};

use fetchloom_archive::ArchiveReader;
use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::Archive;
use fetchloom_faults::{Container, Corpus};

struct Rolling(u64);

impl Rolling {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            usize::try_from(self.next() % bound as u64).unwrap_or(0)
        }
    }
}

fn damaged(bytes: &[u8], rolling: &mut Rolling) -> Vec<u8> {
    let mut damaged = bytes.to_vec();
    match rolling.below(4) {
        0 => {
            let at = rolling.below(damaged.len());
            let bit = u8::try_from(rolling.below(8)).unwrap_or(0);
            if let Some(byte) = damaged.get_mut(at) {
                *byte ^= 1 << bit;
            }
        }
        1 => {
            let keep = rolling.below(damaged.len());
            damaged.truncate(keep);
        }
        2 => {
            let at = rolling.below(damaged.len().saturating_add(1));
            let byte = u8::try_from(rolling.below(256)).unwrap_or(0);
            damaged.insert(at, byte);
        }
        _ => {
            let from = rolling.below(damaged.len());
            let to = rolling.below(damaged.len());
            damaged.swap(from, to);
        }
    }
    damaged
}

fn read_through(bytes: Vec<u8>, container: Container, name: &str) -> Result<usize, ErrorKind> {
    let format = match container {
        Container::Tar => ArchiveFormat::Tar,
        Container::Zip => ArchiveFormat::Zip,
    };
    let mut reader = ArchiveReader::new(Cursor::new(bytes), format, name, Limits::default())
        .map_err(|error| error.kind())?;
    let members = reader.members().map_err(|error| error.kind())?;
    let mut read = 0;
    for member in &members {
        let mut body = reader.open(member).map_err(|error| error.kind())?;
        let mut sink = Vec::new();
        body.read_to_end(&mut sink)
            .map_err(|_| ErrorKind::ArchiveUnsupported)?;
        read += sink.len();
    }
    Ok(read)
}

#[test]
fn every_corpus_entry_damaged_a_hundred_ways_is_read_or_named_and_never_anything_else() {
    let corpus = Corpus::build();
    let permitted = [
        ErrorKind::ArchiveUnsupported,
        ErrorKind::ArchiveUnsafePath,
        ErrorKind::ArchiveCollision,
        ErrorKind::ResourceLimit,
        ErrorKind::ArchiveBomb,
        ErrorKind::ArchiveLinkEscape,
    ];
    let mut refused = 0_u32;
    let mut carried = 0_u32;
    for entry in corpus.entries() {
        let mut rolling = Rolling(0x2545_F491_4F6C_DD1D);
        for round in 0..100 {
            let bytes = damaged(entry.bytes(), &mut rolling);
            match read_through(bytes, entry.container(), entry.name()) {
                Ok(_) => carried += 1,
                Err(kind) => {
                    assert!(
                        permitted.contains(&kind),
                        "{} round {round} was refused as {kind:?}, which is not a reader's answer",
                        entry.name()
                    );
                    refused += 1;
                }
            }
        }
    }
    assert!(
        refused > 3_000 && carried > 500,
        "damage produced only one outcome: {refused} refused, {carried} read"
    );
}
