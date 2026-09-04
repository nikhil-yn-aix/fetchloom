//! The one line a run may print about something the user could have done
//! differently.

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub key: String,
    pub line: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub declined_provider: Option<String>,
    pub projected_gain: Option<Duration>,
    pub placement: Option<String>,
    pub restarted_from_zero: bool,
    pub cache_unusable: bool,
}

const WORTH_SAYING: Duration = Duration::from_secs(120);

impl Observed {
    #[must_use]
    pub fn hint(&self) -> Option<Hint> {
        if let (Some(provider), Some(gain), Some(placement)) = (
            self.declined_provider.as_ref(),
            self.projected_gain,
            self.placement.as_ref(),
        ) && gain >= WORTH_SAYING
        {
            return Some(Hint {
                key: format!("credential:{provider}"),
                line: format!(
                    "setting {placement} would have made this transfer about {} shorter",
                    spoken(gain)
                ),
            });
        }
        if self.cache_unusable {
            return Some(Hint {
                key: "cache-unusable".to_owned(),
                line: "nothing this run fetched was kept, because the cache could not be opened; \
                       set FETCHLOOM_CACHE_DIR to a writable directory to reuse it next time"
                    .to_owned(),
            });
        }
        if self.restarted_from_zero {
            return Some(Hint {
                key: "restarted-from-zero".to_owned(),
                line: "this transfer restarted from zero because the source states nothing that \
                       identifies its bytes; a source that does can resume instead"
                    .to_owned(),
            });
        }
        None
    }
}

fn spoken(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 120 {
        format!("{seconds} seconds")
    } else if seconds < 7200 {
        format!("{} minutes", seconds / 60)
    } else {
        format!("{} hours", seconds / 3600)
    }
}

static TAKEN: std::sync::Mutex<Option<Observed>> = std::sync::Mutex::new(None);

pub fn record(change: impl FnOnce(&mut Observed)) {
    let mut held = TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    change(held.get_or_insert_with(Observed::default));
}

#[must_use]
pub fn taken() -> Observed {
    TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_default()
}

fn said_at(cache: &std::path::Path, key: &str) -> std::path::PathBuf {
    let digest = fetchloom_engine::hashing::hash_bytes(key.as_bytes()).to_string();
    let named = digest.rsplit(':').next().unwrap_or(&digest).to_owned();
    cache.join("meta").join("hints").join(named)
}

#[must_use]
pub fn already_said(cache: &std::path::Path, key: &str) -> bool {
    if !cache.is_dir() {
        return true;
    }
    said_at(cache, key).exists()
}

pub fn remember(cache: &std::path::Path, key: &str) {
    let at = said_at(cache, key);
    if let Some(parent) = at.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(at, b"");
}

#[cfg(test)]
mod tests {
    use super::{Observed, spoken};
    use std::time::Duration;

    fn offered() -> Observed {
        Observed {
            declined_provider: Some("Example Storage".to_owned()),
            projected_gain: Some(Duration::from_secs(600)),
            placement: Some("FETCHLOOM_TOKEN_HOST_EXAMPLE".to_owned()),
            ..Observed::default()
        }
    }

    #[test]
    fn a_hint_never_states_a_fact_about_fetchloom() {
        let forbidden = [
            "fetchloom is",
            "fetchloom can",
            "fetchloom supports",
            "did you know",
            "tip:",
            "try fetchloom",
            "new in",
            "feature",
            "upgrade",
            "faster than",
        ];
        let every = [
            offered().hint(),
            Observed {
                cache_unusable: true,
                ..Observed::default()
            }
            .hint(),
            Observed {
                restarted_from_zero: true,
                ..Observed::default()
            }
            .hint(),
        ];
        for hint in every.into_iter().flatten() {
            let said = hint.line.to_lowercase();
            for phrase in forbidden {
                assert!(
                    !said.contains(phrase),
                    "the hint states a fact about Fetchloom rather than an action: {said}"
                );
            }
            assert!(
                said.contains("would have") || said.contains("set ") || said.contains("can resume"),
                "the hint names no action the user could take: {said}"
            );
        }
    }

    #[test]
    fn a_run_that_earned_nothing_gets_no_hint() {
        assert_eq!(Observed::default().hint(), None);
    }

    #[test]
    fn a_difference_below_the_threshold_earns_no_hint() {
        let mut small = offered();
        small.projected_gain = Some(Duration::from_secs(5));
        assert_eq!(small.hint(), None, "a trivial difference produced a hint");
    }

    #[test]
    fn at_most_one_hint_is_ever_returned() {
        let everything = Observed {
            declined_provider: Some("Example".to_owned()),
            projected_gain: Some(Duration::from_secs(600)),
            placement: Some("FETCHLOOM_TOKEN_X".to_owned()),
            restarted_from_zero: true,
            cache_unusable: true,
        };
        let hint = everything.hint();
        assert!(hint.is_some());
        assert!(
            hint.unwrap_or_else(|| unreachable!())
                .key
                .starts_with("credential:"),
            "the run did not return the one hint that could have changed it most"
        );
    }

    #[test]
    fn a_hint_names_the_measured_difference_rather_than_a_word_for_it() {
        let said = offered().hint().unwrap_or_else(|| unreachable!()).line;
        assert!(
            said.contains("10 minutes"),
            "the hint said something other than the number: {said}"
        );
        for vague in ["much", "far", "significantly", "a lot"] {
            assert!(!said.contains(vague), "the hint hedged: {said}");
        }
    }

    #[test]
    fn a_spoken_duration_is_a_number_and_a_unit() {
        assert_eq!(spoken(Duration::from_secs(30)), "30 seconds");
        assert_eq!(spoken(Duration::from_secs(600)), "10 minutes");
        assert_eq!(spoken(Duration::from_secs(7200)), "2 hours");
    }
}
