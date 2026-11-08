//! stopping a run cleanly. the first ctrl-c asks every task to wind down and keep what it has.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

use crate::error::{Error, Kind};

pub(crate) struct Stop {
    asked: watch::Sender<bool>,
    /// the run's work was done before any stop was asked.
    finished: AtomicBool,
}

impl Default for Stop {
    fn default() -> Stop {
        Stop {
            asked: watch::Sender::new(false),
            finished: AtomicBool::new(false),
        }
    }
}

impl Stop {
    /// the error work that a stop ended returns.
    pub(crate) fn stopped() -> Error {
        Error::new(Kind::Interrupted, "stopped at your request")
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.is_asked() {
            Err(Stop::stopped())
        } else {
            Ok(())
        }
    }

    pub(crate) fn ask(&self) {
        self.asked.send_replace(true);
    }

    /// resolves once a stop is asked, at once when it already was.
    pub(crate) async fn asked(&self) {
        let mut receiver = self.asked.subscribe();
        let _ = receiver.wait_for(|asked| *asked).await;
    }

    /// whether a stop was asked, for work that runs on its own thread.
    pub(crate) fn is_asked(&self) -> bool {
        *self.asked.borrow()
    }

    /// marks the point past which the run completes its work, whatever is asked. a command marks
    /// it once it last looked for a stop where it could still stop, so a stop asked later, as by
    /// ctrl-c while it prints its last lines, leaves it finished.
    pub(crate) fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    /// fails when a stop was asked and the run had not marked its work as finishing.
    pub(crate) fn check_unfinished(&self) -> Result<(), Error> {
        if self.finished.load(Ordering::SeqCst) {
            Ok(())
        } else {
            self.check()
        }
    }
}
