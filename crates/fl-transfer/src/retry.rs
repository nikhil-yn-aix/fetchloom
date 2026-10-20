//! When to try a request again, and how long to wait first.

use std::time::{Duration, SystemTime};

/// Attempts made at most for one request, the first included.
pub(crate) const ATTEMPTS: u32 = 5;

/// The longest wait between attempts, and the longest `Retry-After` waited out.
pub(crate) const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// What to do after a failed attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Next {
    /// Wait this long, then try again.
    Wait(Duration),
    /// No attempts are left.
    GiveUp,
    /// The host asked for a longer wait than is waited out.
    TooLong(Duration),
}

/// Whether a status may succeed when asked again: request timeout, too many requests, and the
/// server errors that mean busy or broken for now.
pub(crate) const fn transient_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// The decision after `failures` failed attempts, the host's `Retry-After` taken as a floor:
/// full jitter, a random wait between zero and `min(2^(failures - 1), 60)` seconds.
pub(crate) fn next(failures: u32, retry_after: Option<Duration>, rng: &mut fastrand::Rng) -> Next {
    if let Some(asked) = retry_after.filter(|asked| *asked > LONGEST_WAIT) {
        return Next::TooLong(asked);
    }
    if failures >= ATTEMPTS {
        return Next::GiveUp;
    }
    let ceiling = 1_u64
        .checked_shl(failures.saturating_sub(1))
        .unwrap_or(u64::MAX)
        .min(LONGEST_WAIT.as_secs());
    let jitter = Duration::from_millis(rng.u64(0..=ceiling * 1000));
    Next::Wait(jitter.max(retry_after.unwrap_or_default()))
}

/// How long a `Retry-After` value asks to wait: delay seconds or an HTTP date, read against
/// `now`. A date in the past asks for no wait; text that is neither is ignored.
pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = httpdate::parse_http_date(value).ok()?;
    Some(date.duration_since(now).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_are_full_jitter_under_a_doubling_ceiling() {
        let mut rng = fastrand::Rng::with_seed(7);
        for failures in 1..ATTEMPTS {
            let ceiling = Duration::from_secs(1 << (failures - 1));
            let waits: Vec<Duration> = (0..200)
                .map(|_| match next(failures, None, &mut rng) {
                    Next::Wait(wait) => wait,
                    other => panic!("{other:?}"),
                })
                .collect();
            assert!(waits.iter().all(|wait| *wait <= ceiling), "{failures}");
            assert!(waits.iter().any(|wait| *wait < ceiling / 4), "{failures}");
            assert!(
                waits.iter().any(|wait| *wait > ceiling * 3 / 4),
                "{failures}"
            );
        }
        assert_eq!(next(ATTEMPTS, None, &mut rng), Next::GiveUp);
    }

    #[test]
    fn retry_after_is_a_floor_up_to_sixty_seconds() {
        let mut rng = fastrand::Rng::with_seed(7);
        for _ in 0..50 {
            let Next::Wait(wait) = next(1, Some(Duration::from_secs(30)), &mut rng) else {
                panic!("no wait");
            };
            assert!(wait >= Duration::from_secs(30));
        }
        assert_eq!(
            next(1, Some(LONGEST_WAIT), &mut rng),
            Next::Wait(LONGEST_WAIT)
        );
        let over = Duration::from_secs(61);
        assert_eq!(next(1, Some(over), &mut rng), Next::TooLong(over));
        assert_eq!(next(ATTEMPTS, Some(over), &mut rng), Next::TooLong(over));
    }

    #[test]
    fn retry_after_reads_seconds_and_dates() {
        let now = httpdate::parse_http_date("Wed, 21 Oct 2026 07:28:00 GMT").unwrap();
        for (value, expected) in [
            ("120", Some(120)),
            (" 0 ", Some(0)),
            ("Wed, 21 Oct 2026 07:29:30 GMT", Some(90)),
            ("Wednesday, 21-Oct-26 07:28:10 GMT", Some(10)),
            ("Wed Oct 21 07:28:05 2026", Some(5)),
            ("Tue, 20 Oct 2026 07:28:00 GMT", Some(0)),
            ("soon", None),
            ("-5", None),
        ] {
            assert_eq!(
                retry_after(value, now),
                expected.map(Duration::from_secs),
                "{value}"
            );
        }
    }

    #[test]
    fn only_busy_and_broken_statuses_are_transient() {
        let transient: Vec<u16> = (100..600)
            .filter(|status| transient_status(*status))
            .collect();
        assert_eq!(transient, [408, 429, 500, 502, 503, 504]);
    }
}
