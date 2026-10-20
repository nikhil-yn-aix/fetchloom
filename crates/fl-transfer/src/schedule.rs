//! Runs many jobs with a fixed number of workers: a cap on jobs at once, the connections per
//! host of [`Hosts`], and round robin across groups so a large group cannot starve a small one.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::panic::resume_unwind;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::task::JoinSet;

use crate::hosts::{Hosts, Slot};

/// One job waiting to run.
#[derive(Clone, Debug)]
pub struct Queued<J> {
    /// The group it belongs to, such as a dataset. Groups take turns.
    pub group: String,
    /// The host it talks to, compared without regard to case.
    pub host: String,
    /// The job itself, handed to the worker function.
    pub job: J,
}

/// Runs queues of jobs with at most so many at once, and no more against one host than its
/// [`Hosts`] allow. A client's own hosts are the ones to share, so its split downloads see the
/// connections the scheduler holds.
#[derive(Clone, Debug)]
pub struct Scheduler {
    jobs: usize,
    hosts: Arc<Hosts>,
}

struct Waiting<J> {
    index: usize,
    host: String,
    job: J,
}

enum Pick<J> {
    Job(Waiting<J>, Slot),
    Blocked,
    Empty,
}

struct State<J> {
    groups: Vec<VecDeque<Waiting<J>>>,
    cursor: usize,
    left: usize,
}

struct Shared<J, T> {
    state: Mutex<State<J>>,
    results: Mutex<Vec<Option<T>>>,
    hosts: Arc<Hosts>,
}

impl Scheduler {
    /// A scheduler running at most `jobs` at once, at least one, within `hosts`.
    #[must_use]
    pub fn new(jobs: usize, hosts: Arc<Hosts>) -> Self {
        Self {
            jobs: jobs.max(1),
            hosts,
        }
    }

    /// Runs `work` on every queued job and returns the results in queue order.
    ///
    /// Exactly `min(jobs, queue length)` worker tasks run. Each takes the next job in round robin
    /// order across groups whose host has room; jobs of one group start in their queued order,
    /// except that a job whose host is full is passed over for a later job of the same group.
    ///
    /// # Panics
    ///
    /// Resumes the panic of a job that panicked, after stopping the other workers.
    pub async fn run<J, T, F, Fut>(&self, queue: Vec<Queued<J>>, work: F) -> Vec<T>
    where
        J: Send + 'static,
        T: Send + 'static,
        F: Fn(J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let total = queue.len();
        if total == 0 {
            return Vec::new();
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new(queue)),
            results: Mutex::new((0..total).map(|_| None).collect()),
            hosts: Arc::clone(&self.hosts),
        });
        let work = Arc::new(work);
        let mut workers = JoinSet::new();
        for _ in 0..self.jobs.min(total) {
            workers.spawn(worker(shared.clone(), work.clone()));
        }
        while let Some(joined) = workers.join_next().await {
            if let Err(error) = joined
                && error.is_panic()
            {
                resume_unwind(error.into_panic());
            }
        }
        let results = std::mem::take(&mut *lock(&shared.results));
        results.into_iter().flatten().collect()
    }
}

impl<J> State<J> {
    fn new(queue: Vec<Queued<J>>) -> Self {
        let left = queue.len();
        let mut places: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<VecDeque<Waiting<J>>> = Vec::new();
        for (index, queued) in queue.into_iter().enumerate() {
            let place = *places.entry(queued.group).or_insert_with(|| {
                groups.push(VecDeque::new());
                groups.len() - 1
            });
            groups[place].push_back(Waiting {
                index,
                host: queued.host.to_lowercase(),
                job: queued.job,
            });
        }
        Self {
            groups,
            cursor: 0,
            left,
        }
    }

    fn pick(&mut self, hosts: &Arc<Hosts>) -> Pick<J> {
        if self.left == 0 {
            return Pick::Empty;
        }
        let count = self.groups.len();
        for step in 0..count {
            let place = (self.cursor + step) % count;
            let Some((found, slot)) = self.groups[place]
                .iter()
                .enumerate()
                .find_map(|(found, waiting)| Some((found, hosts.try_take(&waiting.host)?)))
            else {
                continue;
            };
            let Some(waiting) = self.groups[place].remove(found) else {
                continue;
            };
            self.cursor = (place + 1) % count;
            self.left -= 1;
            return Pick::Job(waiting, slot);
        }
        Pick::Blocked
    }
}

fn lock<V>(mutex: &Mutex<V>) -> MutexGuard<'_, V> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn worker<J, T, F, Fut>(shared: Arc<Shared<J, T>>, work: Arc<F>)
where
    F: Fn(J) -> Fut,
    Fut: Future<Output = T>,
{
    loop {
        let freed = shared.hosts.freed();
        let pick = lock(&shared.state).pick(&shared.hosts);
        match pick {
            Pick::Empty => return,
            Pick::Blocked => freed.await,
            Pick::Job(waiting, slot) => {
                drop(freed);
                let output = work(waiting.job).await;
                if let Some(result) = lock(&shared.results).get_mut(waiting.index) {
                    *result = Some(output);
                }
                drop(slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::sync::Notify;

    use super::*;

    fn queued(group: &str, host: &str, job: &'static str) -> Queued<&'static str> {
        Queued {
            group: group.to_owned(),
            host: host.to_owned(),
            job,
        }
    }

    #[derive(Default)]
    struct Gauge {
        now: [AtomicUsize; 3],
        most: [AtomicUsize; 3],
        all_now: AtomicUsize,
        all_most: AtomicUsize,
    }

    #[tokio::test]
    async fn limits_hold_and_results_keep_queue_order() {
        let gauge = Arc::new(Gauge::default());
        let queue: Vec<Queued<(usize, usize)>> = (0..20)
            .map(|index| Queued {
                group: format!("g{}", index % 2),
                host: format!("h{}", index % 3),
                job: (index % 3, index),
            })
            .collect();
        let seen = gauge.clone();
        let scheduler = Scheduler::new(8, Hosts::new(2));
        let results = scheduler
            .run(queue, move |(host, index)| {
                let gauge = seen.clone();
                async move {
                    let here = gauge.now[host].fetch_add(1, Ordering::SeqCst) + 1;
                    gauge.most[host].fetch_max(here, Ordering::SeqCst);
                    let all = gauge.all_now.fetch_add(1, Ordering::SeqCst) + 1;
                    gauge.all_most.fetch_max(all, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(3)).await;
                    gauge.now[host].fetch_sub(1, Ordering::SeqCst);
                    gauge.all_now.fetch_sub(1, Ordering::SeqCst);
                    index
                }
            })
            .await;
        assert_eq!(results, (0..20).collect::<Vec<_>>());
        for host in 0..3 {
            assert_eq!(gauge.most[host].load(Ordering::SeqCst), 2);
        }
        assert!(gauge.all_most.load(Ordering::SeqCst) <= 8);
    }

    #[tokio::test]
    async fn groups_take_turns() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let queue = vec![
            queued("A", "a1", "A1"),
            queued("A", "a2", "A2"),
            queued("A", "a3", "A3"),
            queued("A", "a4", "A4"),
            queued("B", "b1", "B1"),
            queued("B", "b2", "B2"),
        ];
        let seen = log.clone();
        let scheduler = Scheduler::new(1, Hosts::new(1));
        scheduler
            .run(queue, move |name| {
                let log = seen.clone();
                async move {
                    lock(&log).push(name);
                }
            })
            .await;
        assert_eq!(*lock(&log), ["A1", "B1", "A2", "B2", "A3", "A4"]);
    }

    #[tokio::test]
    async fn a_full_host_does_not_block_another_host_of_its_group() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        let queue = vec![
            queued("A", "H1", "first"),
            queued("A", "h1", "second"),
            queued("A", "h2", "other"),
        ];
        let (seen, opens) = (log.clone(), gate.clone());
        let scheduler = Scheduler::new(2, Hosts::new(1));
        scheduler
            .run(queue, move |name| {
                let (log, gate) = (seen.clone(), opens.clone());
                async move {
                    lock(&log).push(name);
                    match name {
                        "first" => gate.notified().await,
                        "other" => gate.notify_one(),
                        _ => {}
                    }
                }
            })
            .await;
        assert_eq!(*lock(&log), ["first", "other", "second"]);
    }

    #[tokio::test]
    async fn exactly_min_of_jobs_and_queue_length_workers_run() {
        for (jobs, length, expected) in [(4, 10, 4), (5, 2, 2)] {
            let tasks = Arc::new(Mutex::new(HashSet::new()));
            let queue: Vec<Queued<usize>> = (0..length)
                .map(|index| Queued {
                    group: "g".to_owned(),
                    host: format!("h{index}"),
                    job: index,
                })
                .collect();
            let seen = tasks.clone();
            let scheduler = Scheduler::new(jobs, Hosts::new(1));
            scheduler
                .run(queue, move |_| {
                    let tasks = seen.clone();
                    async move {
                        lock(&tasks).insert(tokio::task::id());
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await;
            assert_eq!(lock(&tasks).len(), expected);
        }
    }

    #[tokio::test]
    async fn zero_limits_count_as_one() {
        let scheduler = Scheduler::new(0, Hosts::new(0));
        let queue = vec![queued("A", "h", "x"), queued("A", "h", "y")];
        let results = scheduler.run(queue, |name| async move { name }).await;
        assert_eq!(results, ["x", "y"]);
    }
}
