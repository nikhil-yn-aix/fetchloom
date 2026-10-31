//! stopping a run cleanly. the first ctrl-c asks every task to wind down and keep what it has.

use tokio::sync::watch;

use crate::error::{Error, Kind};

pub(crate) struct Stop(watch::Sender<bool>);

impl Default for Stop {
    fn default() -> Stop {
        Stop(watch::Sender::new(false))
    }
}

impl Stop {
    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.is_asked() {
            Err(Error::new(Kind::Interrupted, "stopped at your request"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn ask(&self) {
        self.0.send_replace(true);
    }

    /// resolves once a stop is asked, at once when it already was.
    pub(crate) async fn asked(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|asked| *asked).await;
    }

    /// whether a stop was asked, for work that runs on its own thread.
    pub(crate) fn is_asked(&self) -> bool {
        *self.0.borrow()
    }
}
