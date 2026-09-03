//! Whether this run may reach the network at all.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, ErrorKind};
use crate::redact::SafeUrl;

/// Whether the run has forbidden every request.
static FORBIDDEN: AtomicBool = AtomicBool::new(false);

/// Records that this run may issue no request, whatever a reference resolves
/// to and whichever adapter would issue it.
pub fn forbid() {
    FORBIDDEN.store(true, Ordering::SeqCst);
}

/// Returns whether the run has forbidden every request.
#[must_use]
pub fn forbidden() -> bool {
    FORBIDDEN.load(Ordering::SeqCst)
}

/// Refuses a request the run has forbidden, at the point it would be issued.
///
/// # Errors
///
/// Fails with a policy failure when the run forbids network activity.
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
