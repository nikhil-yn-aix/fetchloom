//! Which operation fails, on which call, with which error.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use fetchloom_engine::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    VolumeId,
    FreeSpace,
    FileId,
    Fingerprint,
    VolumeBacking,
    VolumeCapabilities,
    CreateFileExclusive,
    CreateDirectoryExclusive,
    Preallocate,
    Flush,
    ReleaseWritten,
    PublishFile,
    PublishDirectory,
    CloneOrCopy,
    CreateSymlink,
    OwnerToken,
    TryLock,
    Lock,
    TryLockShared,
    LockShared,
    FileIdOf,
    Owns,
}

/// How many more calls happen before the process ends, and how it ends.
type Kill = (u64, fn() -> !);

#[derive(Debug)]
struct Rule {
    successes: u64,
    failures: u64,
    error: Error,
}

#[derive(Debug, Default)]
pub struct Faults {
    rules: Mutex<HashMap<Operation, Vec<Rule>>>,
    delays: Mutex<HashMap<Operation, std::time::Duration>>,
    kills: Mutex<HashMap<Operation, Kill>>,
}

impl Faults {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Mutex::new(HashMap::new()),
            delays: Mutex::new(HashMap::new()),
            kills: Mutex::new(HashMap::new()),
        }
    }

    pub fn delay(&self, operation: Operation, waiting: std::time::Duration) {
        self.delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(operation, waiting);
    }

    /// Ends this process inside the operation, so a kill lands at a position in
    /// the code under test rather than at a position in the test. The caller
    /// supplies the ending, because how a process is killed outright is the
    /// platform question this crate does not answer.
    pub fn kill_at(&self, operation: Operation, after: u64, end: fn() -> !) {
        self.kills
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(operation, (after, end));
    }

    pub fn fail(&self, operation: Operation, after: u64, times: u64, error: Error) {
        let mut rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
        rules.entry(operation).or_default().push(Rule {
            successes: after,
            failures: times,
            error,
        });
    }

    #[must_use]
    pub fn check(&self, operation: Operation) -> Option<Error> {
        let waiting = self
            .delays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&operation)
            .copied();
        if let Some(waiting) = waiting {
            std::thread::sleep(waiting);
        }
        let kill = {
            let mut kills = self.kills.lock().unwrap_or_else(PoisonError::into_inner);
            match kills.get_mut(&operation) {
                Some((0, end)) => Some(*end),
                Some((remaining, _)) => {
                    *remaining -= 1;
                    None
                }
                None => None,
            }
        };
        if let Some(end) = kill {
            end();
        }
        let mut rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
        let slots = rules.get_mut(&operation)?;
        for slot in slots {
            if slot.successes > 0 {
                slot.successes -= 1;
                return None;
            }
            if slot.failures > 0 {
                slot.failures -= 1;
                return Some(slot.error.clone());
            }
        }
        None
    }
}
