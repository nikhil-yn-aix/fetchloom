//! Which operation fails, on which call, with which error.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use fetchloom_engine::error::Error;

/// A named operation a fault can be scheduled against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operation {
    /// Reading the identifier of the volume a path is on.
    VolumeId,
    /// Reading the identifier of a file within its volume.
    FileId,
    /// Reading the tuple recording that a file is probably unchanged.
    Fingerprint,
    /// Reading what a volume sits on.
    VolumeBacking,
    /// Detecting what a volume can do.
    VolumeCapabilities,
    /// Creating a file that must not already exist.
    CreateFileExclusive,
    /// Creating a directory that must not already exist.
    CreateDirectoryExclusive,
    /// Reserving the full length of a file.
    Preallocate,
    /// Pushing a file's bytes as far as a durability tier requires.
    Flush,
    /// Renaming one file onto its final name.
    PublishFile,
    /// Renaming a staging tree onto a destination.
    PublishDirectory,
    /// Placing a file's bytes at another path.
    CloneOrCopy,
    /// Creating a symbolic link.
    CreateSymlink,
    /// Reading what this process would record about itself as a lock holder.
    OwnerToken,
    /// Taking an advisory lock without waiting.
    TryLock,
    /// Taking an advisory lock and waiting for it.
    Lock,
    /// Taking a lock other readers may hold, without waiting.
    TryLockShared,
    /// Taking a lock other readers may hold, and waiting for it.
    LockShared,
    /// Reading the identity of an open file.
    FileIdOf,
    /// Reading whether a file belongs to this user.
    Owns,
}

#[derive(Debug)]
struct Rule {
    successes: u64,
    failures: u64,
    error: Error,
}

/// The faults scheduled for a run.
#[derive(Debug, Default)]
pub struct Faults {
    rules: Mutex<HashMap<Operation, Vec<Rule>>>,
}

impl Faults {
    /// Starts an empty schedule, in which every operation succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self {
            rules: Mutex::new(HashMap::new()),
        }
    }

    /// Schedules a failure for one operation.
    pub fn fail(&self, operation: Operation, after: u64, times: u64, error: Error) {
        let mut rules = self.rules.lock().unwrap_or_else(PoisonError::into_inner);
        rules.entry(operation).or_default().push(Rule {
            successes: after,
            failures: times,
            error,
        });
    }

    /// Consults the schedule before an operation runs.
    #[must_use]
    pub fn check(&self, operation: Operation) -> Option<Error> {
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
