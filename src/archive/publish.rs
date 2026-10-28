//! publishing what an archive unpacked to. every object waits in `tmp/` until the whole archive
//! passed its checks, then all of them are flushed to disk and put in place, in parallel.

use std::io;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

/// runs `work` on every item, on as many threads as the machine runs at once, and returns the
/// results in no particular order. the first failure stops the rest.
pub(super) fn in_parallel<T: Send, R: Send>(
    items: Vec<T>,
    work: impl Fn(T) -> io::Result<R> + Sync,
) -> io::Result<Vec<R>> {
    let threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(items.len());
    if threads <= 1 {
        return items.into_iter().map(work).collect();
    }
    let queue = Mutex::new(items.into_iter());
    let failed = AtomicBool::new(false);
    let worker = || -> io::Result<Vec<R>> {
        let mut done = Vec::new();
        while !failed.load(Ordering::Relaxed) {
            let next = queue
                .lock()
                .map_err(|_| io::Error::other("a publishing thread failed"))?
                .next();
            let Some(item) = next else { break };
            match work(item) {
                Ok(result) => done.push(result),
                Err(err) => {
                    failed.store(true, Ordering::Relaxed);
                    return Err(err);
                }
            }
        }
        Ok(done)
    };
    thread::scope(|scope| {
        let workers: Vec<_> = (0..threads).map(|_| scope.spawn(worker)).collect();
        let mut results = Vec::new();
        for worker in workers {
            let done = worker
                .join()
                .map_err(|_| io::Error::other("a publishing thread failed"))??;
            results.extend(done);
        }
        Ok(results)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_item_is_worked_once() {
        let mut doubled = in_parallel((0..1000).collect(), |n: u32| Ok(n * 2)).unwrap();
        doubled.sort_unstable();
        assert_eq!(doubled, (0..1000).map(|n| n * 2).collect::<Vec<_>>());
    }

    #[test]
    fn a_failure_is_returned() {
        let result = in_parallel((0..100).collect(), |n: u32| {
            if n == 50 {
                Err(io::Error::other("disk full"))
            } else {
                Ok(n)
            }
        });
        assert_eq!(result.unwrap_err().to_string(), "disk full");
    }
}
