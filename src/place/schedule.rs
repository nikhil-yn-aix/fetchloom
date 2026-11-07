//! limits concurrent namespace mutations per parent without changing the worker ceiling.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};

use crate::threads::Threads;

const PER_PARENT: usize = 4;

pub(super) struct Folders {
    names: HashMap<PathBuf, usize>,
    parent: Vec<usize>,
}

impl Folders {
    pub(super) fn new(count: usize) -> Self {
        Self {
            names: HashMap::new(),
            parent: vec![usize::MAX; count],
        }
    }

    pub(super) fn record(&mut self, index: usize, destination: &Path) {
        let parent = destination.parent().unwrap_or(destination);
        let next = self.names.len();
        let group = *self.names.entry(parent.to_path_buf()).or_insert(next);
        self.parent[index] = group;
    }

    pub(super) fn finish(self, serial: bool) -> Work {
        let limit = if serial { 1 } else { PER_PARENT };
        Work {
            parent: self.parent,
            gates: (0..self.names.len())
                .map(|_| Gate {
                    live: Mutex::new(0),
                    changed: Condvar::new(),
                    limit,
                })
                .collect(),
            serial,
        }
    }
}

pub(super) struct Work {
    parent: Vec<usize>,
    gates: Vec<Gate>,
    serial: bool,
}

impl Work {
    pub(super) fn threads(&self, ceiling: Threads) -> Threads {
        ceiling.at_most(if self.serial {
            1
        } else {
            self.gates.len().saturating_mul(PER_PARENT).max(1)
        })
    }

    pub(super) fn enter(&self, index: usize) -> Option<Permit<'_>> {
        self.parent
            .get(index)
            .and_then(|parent| self.gates.get(*parent))
            .map(Gate::enter)
    }
}

struct Gate {
    live: Mutex<usize>,
    changed: Condvar,
    limit: usize,
}

impl Gate {
    fn enter(&self) -> Permit<'_> {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        while *live >= self.limit {
            live = self
                .changed
                .wait(live)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *live += 1;
        Permit(self)
    }
}

pub(super) struct Permit<'a>(&'a Gate);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *self.0.live.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.changed.notify_one();
    }
}

#[cfg(any(target_os = "macos", windows, test))]
pub(super) fn serializes_name(name: &str) -> bool {
    ["exfat", "vfat", "fat", "fat32", "msdos", "msdosfs"]
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn serializes_magic(magic: i128) -> bool {
    matches!(magic, 0x2011_bab0 | 0x4d44)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn filesystem_classification_is_exact() {
        for name in ["exfat", "EXFAT", "fat32", "MSDOS", "vfat", "msdosfs"] {
            assert!(serializes_name(name), "{name}");
        }
        for name in ["ntfs", "refs", "apfs", "ext4", "xfs", "not-exfat", ""] {
            assert!(!serializes_name(name), "{name}");
        }
        assert!(serializes_magic(0x2011_bab0));
        assert!(serializes_magic(0x4d44));
        for magic in [-1, 0, 0xef53, 0x5846_5342] {
            assert!(!serializes_magic(magic));
        }
    }

    #[test]
    fn parent_and_volume_caps_respect_the_commands_ceiling() {
        for parents in [1, 2, 100] {
            for serial in [false, true] {
                let mut folders = Folders::new(parents);
                for index in 0..parents {
                    folders.record(index, &PathBuf::from(format!("{index}/file")));
                }
                let work = folders.finish(serial);
                let wanted = if serial { 1 } else { parents * PER_PARENT };
                assert_eq!(work.threads(Threads::default()).limit(256), wanted.min(256));
                assert_eq!(
                    work.threads(Threads::default().at_most(2)).limit(256),
                    wanted.min(2)
                );
            }
        }
    }

    #[test]
    fn permits_bound_each_parent_and_leave_no_active_work() {
        let count = 512;
        let mut folders = Folders::new(count);
        for index in 0..count {
            folders.record(index, &PathBuf::from(format!("{}/file-{index}", index % 2)));
        }
        let work = folders.finish(false);
        let live = [AtomicUsize::new(0), AtomicUsize::new(0)];
        let peak = [AtomicUsize::new(0), AtomicUsize::new(0)];
        let indexes: Vec<_> = (0..count).collect();
        let done = super::super::run_all(&indexes, work.threads(Threads::default()), |&index| {
            let _permit = work.enter(index).unwrap();
            let parent = index % 2;
            let now = live[parent].fetch_add(1, Ordering::SeqCst) + 1;
            peak[parent].fetch_max(now, Ordering::SeqCst);
            live[parent].fetch_sub(1, Ordering::SeqCst);
            index
        });
        assert_eq!(done, indexes);
        for parent in 0..2 {
            assert!(peak[parent].load(Ordering::SeqCst) <= PER_PARENT);
            assert_eq!(live[parent].load(Ordering::SeqCst), 0);
            assert_eq!(*work.gates[parent].live.lock().unwrap(), 0);
        }
    }
}
