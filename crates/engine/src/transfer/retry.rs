//! Waiting between attempts: backoff, server-supplied retry guidance, and the
//! loop that spends attempts until one succeeds or the budget runs out.

use std::sync::Mutex;
use std::time::Duration;

use crate::error::{Error, ErrorKind};
use crate::event::{Event, EventPayload, Sequence};
use crate::limits::Limits;
use crate::seam::observer::Observer;
use crate::seam::source::SourceMetadata;
use crate::tuning::{Answer, Controller};

use super::Pause;

#[must_use]
pub fn backoff(limits: &Limits, attempt: u32, fraction: f64) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    let full = Duration::from_secs(1u64 << exponent).min(limits.retry_ceiling);
    full.mul_f64(fraction.clamp(0.0, 1.0))
}

#[must_use]
pub fn honors(limits: &Limits, retry_after: Duration) -> bool {
    retry_after <= limits.retry_ceiling
}

pub struct Retry<'a, P> {
    pub limits: &'a Limits,
    pub pause: &'a P,
    pub observer: &'a dyn Observer,
    pub sequence: &'a Sequence,
    pub controller: &'a Mutex<Controller>,
    pub host: crate::reference::Host,
}

impl<P: Pause> Retry<'_, P> {
    /// # Errors
    /// Whatever the work reported on its last attempt, once the attempts are
    /// spent or the failure is one that is never retried.
    pub fn until_spent<T>(
        &self,
        mut work: impl FnMut(u32) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let failure = match work(attempt) {
                Ok(done) => {
                    if attempt == 1 {
                        self.answered(Answer::Clean);
                    }
                    return Ok(done);
                }
                Err(failure) => failure,
            };
            self.answered(if failure.retry_after().is_some() {
                Answer::RateLimited
            } else {
                Answer::Faltered
            });
            let asked_past_the_ceiling = failure
                .retry_after()
                .is_some_and(|wait| !honors(self.limits, wait));
            if !failure.retryable()
                || asked_past_the_ceiling
                || attempt >= self.limits.retry_attempts
            {
                return Err(failure.with_attempts(attempt));
            }
            self.observer.emit(&Event::new(
                self.sequence,
                EventPayload::TransferRetry {
                    host: self.host.clone(),
                    attempt,
                    reason: failure.next_action().to_owned(),
                },
            ));
            let backing_off = backoff(self.limits, attempt, self.pause.fraction());
            let asked = failure.retry_after().unwrap_or_default();
            self.pause.sleep(backing_off.max(asked));
        }
    }

    fn answered(&self, answer: Answer) {
        self.controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .answered(answer);
    }
}

#[derive(Debug, Default)]
pub struct SleepingPause;

impl Pause for SleepingPause {
    fn fraction(&self) -> f64 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        f64::from(nanos) / f64::from(u32::MAX)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[must_use]
pub fn body_failure(location: &str, reason: &std::io::Error) -> Error {
    let quiet = matches!(
        reason.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) || reason.to_string().to_lowercase().contains("timeout");
    let kind = if quiet {
        ErrorKind::NetworkTimeout
    } else {
        ErrorKind::NetworkRefused
    };
    Error::new(
        kind,
        format!("try the source again, because the body stopped arriving: {reason}"),
    )
    .with_source(location)
    .with_retryable(true)
}

pub(super) fn ended_early(
    arrived: &Result<(), Error>,
    metadata: &SourceMetadata,
    location: &str,
    have: u64,
) -> Result<(), Error> {
    let short = metadata.size.is_some_and(|expected| have < expected);
    let timed_out = arrived
        .as_ref()
        .err()
        .is_some_and(|failure| failure.kind() == ErrorKind::NetworkTimeout);
    if !short && arrived.is_ok() {
        return Ok(());
    }
    if short && !timed_out {
        return Err(Error::new(
            ErrorKind::IntegrityTruncated,
            format!(
                "fetch the rest, because the source ended the body after {have} of the {} bytes it said the object holds",
                metadata.size.unwrap_or_default()
            ),
        )
        .with_source(location)
        .with_retryable(true));
    }
    match arrived {
        Ok(()) => Ok(()),
        Err(failure) => Err(failure.clone()),
    }
}

pub(super) fn taking(length: u64, rate: u64) -> Duration {
    let nanos = u128::from(length) * 1_000_000_000 / u128::from(rate.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}
