//! The single processor pool all CPU work runs on.

use rayon::{ThreadPool, ThreadPoolBuildError, ThreadPoolBuilder};

use crate::threads::ThreadBudget;

#[derive(Debug)]
pub struct Processor {
    pool: ThreadPool,
}

impl Processor {
    /// # Errors
    /// Whatever rayon reports when a pool of that many threads cannot be
    /// built.
    pub fn new(budget: ThreadBudget) -> Result<Self, ThreadPoolBuildError> {
        let pool = ThreadPoolBuilder::new()
            .num_threads(budget.threads().get())
            .build()?;
        Ok(Self { pool })
    }

    pub fn install<Op, Out>(&self, op: Op) -> Out
    where
        Op: FnOnce() -> Out + Send,
        Out: Send,
    {
        self.pool.install(op)
    }
}
