//! stopping a run cleanly. the first ctrl-c asks every task to wind down and keep what it has.

use tokio::sync::watch;

pub(crate) struct Stop(watch::Sender<bool>);

impl Default for Stop {
    fn default() -> Stop {
        Stop(watch::Sender::new(false))
    }
}

impl Stop {
    pub(crate) fn ask(&self) {
        self.0.send_replace(true);
    }

    /// resolves once a stop is asked, at once when it already was.
    pub(crate) async fn asked(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|asked| *asked).await;
    }
}
