//! Whether this run may reach the network at all.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, ErrorKind};
use crate::redact::SafeUrl;

static FORBIDDEN: AtomicBool = AtomicBool::new(false);

pub fn forbid() {
    FORBIDDEN.store(true, Ordering::SeqCst);
}

#[must_use]
pub fn forbidden() -> bool {
    FORBIDDEN.load(Ordering::SeqCst)
}

/// # Errors
/// `policy.offline` when the run was told not to reach the network.
pub fn allowed(location: &str) -> Result<(), Error> {
    if !forbidden() {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::PolicyOffline,
        format!(
            "run the command again without --offline to reach {}",
            SafeUrl::new(location)
        ),
    ))
}
