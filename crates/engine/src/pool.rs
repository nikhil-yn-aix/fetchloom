//! The single processor pool all CPU work runs on.

use rayon::{ThreadPool, ThreadPoolBuildError, ThreadPoolBuilder};

use crate::threads::ThreadBudget;

/// The engine's own rayon thread pool, sized to a thread budget.
///
/// Every processor task enters through `install`, never through rayon's
/// implicit global pool, so a dependency calling `rayon::join` or `scope`
/// outside `install` cannot silently spin up a second pool sized to the raw
/// core count.
pub struct Processor {
    pool: ThreadPool,
}

impl Processor {
    /// Builds a processor pool sized to the given thread budget.
    ///
    /// # Errors
    ///
    /// Fails when the platform cannot start the requested number of threads.
    pub fn new(budget: ThreadBudget) -> Result<Self, ThreadPoolBuildError> {
        let pool = ThreadPoolBuilder::new()
            .num_threads(budget.threads().get())
            .build()?;
        Ok(Self { pool })
    }

    /// Runs `op` on this pool.
    ///
    /// Any `rayon::join`, `scope`, or parallel iterator called inside `op`
    /// operates within this pool rather than rayon's implicit global pool.
    pub fn install<Op, Out>(&self, op: Op) -> Out
    where
        Op: FnOnce() -> Out + Send,
        Out: Send,
    {
        self.pool.install(op)
    }
}
