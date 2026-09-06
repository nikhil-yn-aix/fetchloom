//! Rewriting a pack against a dictionary trained over exactly what that pack
//! holds.

use fetchloom_engine::compression::{CompressionChoice, Stored};
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;
use serde::Serialize;

use crate::Cache;

/// The level a compacted pack is written at. Compaction is a maintenance
/// command rather than a step on the fetch path, so the rate the append path
/// has to keep up with does not bind it, and a frame decompresses at the same
/// rate whatever level wrote it.
pub const COMPACT_LEVEL: i32 = 19;

/// How large a trained dictionary is asked to be.
pub const DICTIONARY_BYTES: usize = 16 * 1024;

/// A pack holding fewer objects than this trains nothing. zstd refuses to
/// train on one sample, and a handful of samples describes a pack rather than
/// a shape shared across it.
pub const MIN_SAMPLES: usize = 8;

/// A pack holding less content than this trains nothing, because the
/// dictionary it would store is itself [`DICTIONARY_BYTES`] and would cost more
/// than it saves.
pub const MIN_CONTENT_BYTES: u64 = (DICTIONARY_BYTES as u64) * 8;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CompactReport {
    pub packs: u64,
    pub dictionaries: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

impl<P: Platform> Cache<P> {
    /// Rewrites every pack, training one dictionary over the objects each one
    /// holds and recompressing them against it.
    ///
    /// # Errors
    /// `cache.corrupt` when a pack cannot be read, rewritten or published.
    pub fn compact(&self) -> Result<CompactReport, Error> {
        let mut report = CompactReport::default();
        for pack in self.packs()? {
            report.packs += 1;
            report.bytes_before += size_of_pack(&pack);
            if self.compact_pack(&pack)? {
                report.dictionaries += 1;
            }
            report.bytes_after += size_of_pack(&pack);
        }
        Ok(report)
    }

    fn compact_pack(&self, pack: &std::path::Path) -> Result<bool, Error> {
        let entries = self.entries_in(pack)?;
        if entries.is_empty() {
            return Ok(false);
        }
        let mut objects: Vec<(ContentDigest, Vec<u8>)> = Vec::with_capacity(entries.len());
        for (digest, _) in &entries {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut self.read(*digest)?, &mut bytes)
                .map_err(|reason| filesystem_failure(Surface::Cache, pack, &reason))?;
            objects.push((*digest, bytes));
        }
        let dictionary = self.dictionary_over(pack, &objects);
        self.rewrite_pack_with(pack, &objects, dictionary.as_deref())?;
        Ok(dictionary.is_some())
    }

    fn dictionary_over(
        &self,
        pack: &std::path::Path,
        objects: &[(ContentDigest, Vec<u8>)],
    ) -> Option<Vec<u8>> {
        if self.compression() == CompressionChoice::None {
            self.degradations.record(
                format!("{} compacted against a dictionary", pack.display()),
                "it rewritten with none",
                "this run asked for compress none, and a run told never to compress does not get compressed packs from a maintenance command".to_owned(),
            );
            return None;
        }
        let content: u64 = objects.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        if objects.len() < MIN_SAMPLES || content < MIN_CONTENT_BYTES {
            self.degradations.record(
                format!("{} compacted against a dictionary", pack.display()),
                "it rewritten with none",
                format!(
                    "it holds {} objects and {content} bytes, and a dictionary is trained only over at least {MIN_SAMPLES} objects and {MIN_CONTENT_BYTES} bytes, below which the {DICTIONARY_BYTES} byte dictionary costs more than it saves",
                    objects.len()
                ),
            );
            return None;
        }
        let samples: Vec<&[u8]> = objects.iter().map(|(_, bytes)| bytes.as_slice()).collect();
        match zstd::dict::from_samples(&samples, DICTIONARY_BYTES) {
            Ok(dictionary) => Some(dictionary),
            Err(reason) => {
                self.degradations.record(
                    format!("{} compacted against a dictionary", pack.display()),
                    "it rewritten with none",
                    format!(
                        "training over its {} objects was refused: {reason}",
                        objects.len()
                    ),
                );
                None
            }
        }
    }
}

fn size_of_pack(pack: &std::path::Path) -> u64 {
    std::fs::metadata(pack).map_or(0, |found| found.len())
}

#[must_use]
pub(crate) fn stored_for(dictionary: Option<&[u8]>, choice: CompressionChoice) -> Stored {
    match choice {
        CompressionChoice::None => Stored::Raw,
        CompressionChoice::Auto | CompressionChoice::Zstd(_) => Stored::Zstd {
            level: COMPACT_LEVEL,
            stride: 0,
            dictionary: crate::compress::identifier_of(dictionary),
        },
    }
}

pub(crate) fn unreadable(pack: &std::path::Path, reason: &str) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "run cache compact after fetching them again, because every object in {} was compressed against a dictionary that pack no longer states, so none of them can be read: {reason}",
            pack.display()
        ),
    )
}
