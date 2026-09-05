//! Tracks an archive against the entry, expanded-byte, and expansion-ratio
//! limits, both while it is listed and while its bytes move.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;

fn bomb(next_action: String) -> Error {
    Error::new(ErrorKind::ArchiveBomb, next_action)
}

pub struct BombGuard {
    name: String,
    on_disk_bytes: u64,
    entries: u64,
    expanded_bytes: u64,
    limits: Limits,
}

impl BombGuard {
    #[must_use]
    pub fn new(name: impl Into<String>, on_disk_bytes: u64, limits: Limits) -> Self {
        Self {
            name: name.into(),
            on_disk_bytes,
            entries: 0,
            expanded_bytes: 0,
            limits,
        }
    }

    pub(crate) fn observe_entry(&mut self) -> Result<(), Error> {
        self.entries += 1;
        if self.entries > self.limits.archive_entries {
            return Err(bomb(format!(
                "archive \"{}\" holds more than {} entries, {} counted",
                self.name, self.limits.archive_entries, self.entries
            )));
        }
        Ok(())
    }

    pub(crate) fn observe_bytes(&mut self, count: u64) -> Result<(), Error> {
        self.expanded_bytes = self.expanded_bytes.saturating_add(count);
        if self.expanded_bytes > self.limits.expanded_bytes {
            return Err(bomb(format!(
                "archive \"{}\" expands past {} bytes, {} counted",
                self.name, self.limits.expanded_bytes, self.expanded_bytes
            )));
        }
        let denominator = self.on_disk_bytes.max(1);
        let ratio = self.expanded_bytes / denominator;
        if ratio > self.limits.expansion_ratio {
            return Err(bomb(format!(
                "archive \"{}\" expands {ratio} times past its own size, past the limit of {}",
                self.name, self.limits.expansion_ratio
            )));
        }
        Ok(())
    }
}
