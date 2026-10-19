//! A bounded pool of scoped threads.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

/// Applies `work` to every item on at most `jobs` threads and returns the results in item order.
///
/// A panic in `work` reaches the caller once every thread has stopped.
pub(crate) fn map<T, R>(items: &[T], jobs: usize, work: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let jobs = jobs.clamp(1, items.len().max(1));
    if jobs == 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..jobs {
            let sender = sender.clone();
            let (next, work) = (&next, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    if sender.send((index, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
    });
    drop(sender);
    let mut results: Vec<(usize, R)> = receiver.into_iter().collect();
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_in_item_order_whatever_the_number_of_jobs() {
        let items: Vec<u64> = (0..1000).collect();
        for jobs in [0, 1, 4, 2000] {
            let doubled = map(&items, jobs, |item| item * 2);
            assert_eq!(
                doubled,
                items.iter().map(|item| item * 2).collect::<Vec<_>>()
            );
        }
        assert!(map(&[] as &[u64], 4, |item| *item).is_empty());
    }
}
