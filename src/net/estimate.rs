//! how long downloads should take, judged before any starts: a pure function of the files and
//! bytes each host serves, what this machine remembers of each host's speed, the pacing fetchloom
//! keeps to and its job limits.

use std::time::Duration;

use serde::Serialize;

use crate::units;

/// a host's remembered speed: bytes a second over one stream, and the seconds each file waits
/// for its answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Speed {
    pub(crate) rate: f64,
    pub(crate) overhead: f64,
}

/// the downloads planned from one host, under the name its requests are paced and counted by.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Host {
    pub(crate) files: u64,
    /// the bytes of the files whose size is known.
    pub(crate) bytes: u64,
    /// files whose size only their download tells.
    pub(crate) unknown: u64,
    pub(crate) largest: u64,
    pub(crate) speed: Option<Speed>,
    /// how far apart its requests start, as arXiv asks 3 s.
    pub(crate) spacing: Option<Duration>,
    /// how many of its requests may run at once, as arXiv allows one.
    pub(crate) most: Option<usize>,
}

/// downloads running at once: `jobs` in all, `host_jobs` to one host.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) jobs: usize,
    pub(crate) host_jobs: usize,
}

/// whether a time is an estimate or a floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Bound {
    About,
    AtLeast,
}

/// how long downloads should take, in whole seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Time {
    pub(crate) bound: Bound,
    pub(crate) seconds: u64,
}

/// a floor shorter than this says nothing a person plans around, so it is no time at all.
const FLOOR_SHOWN: f64 = 60.0;

impl Time {
    /// as a person reads it, rounded: `about 3 min`, `at least 5 min`, `under 10 s`.
    pub(crate) fn words(self) -> String {
        let took = Duration::from_secs(self.seconds);
        match self.bound {
            Bound::About => units::about(took),
            Bound::AtLeast => format!("at least {}", units::at_least(took)),
        }
    }
}

/// how long downloads from `hosts` should take under `limits`, or none without grounds.
///
/// each file costs its host's overhead, the wait for its answer, and its bytes at the host's
/// rate. a host runs as many streams at once as the fewest of `host_jobs`, what its source
/// allows, `jobs` and its files, so it takes the longest of three: its work spread over its
/// streams; its largest file, which one stream carries alone; and its pacing, one request per
/// spacing, `files × spacing`. hosts run side by side, so all of them take the longest host, or
/// all their work spread over `jobs` streams when that is longer.
///
/// a host without a remembered speed, or with a file of unknown size, has no estimate, only its
/// pacing as a floor. then the whole is at least the longest part that is known, the other hosts'
/// estimates or any floor, since the rest can only add time; a floor under a minute is no time.
pub(crate) fn time(hosts: &[Host], limits: Limits) -> Option<Time> {
    let jobs = limits.jobs.max(1);
    let (mut longest, mut work, mut known) = (0.0_f64, 0.0_f64, true);
    for host in hosts.iter().filter(|host| host.files > 0) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "an estimate needs no exact count"
        )]
        let (files, bytes, largest) = (host.files as f64, host.bytes as f64, host.largest as f64);
        let floor = host
            .spacing
            .map_or(0.0, |spacing| files * spacing.as_secs_f64());
        longest = longest.max(floor);
        let Some(speed) = host
            .speed
            .filter(|speed| host.unknown == 0 && speed.rate > 0.0)
        else {
            known = false;
            continue;
        };
        let streams = [limits.host_jobs, host.most.unwrap_or(usize::MAX), jobs]
            .into_iter()
            .min()
            .unwrap_or(1)
            .max(1);
        #[expect(clippy::cast_precision_loss, reason = "a few streams")]
        let streams = (streams as f64).min(files);
        let own = files * speed.overhead + bytes / speed.rate;
        let alone = speed.overhead + largest / speed.rate;
        longest = longest.max(own / streams).max(alone);
        work += own;
    }
    #[expect(clippy::cast_precision_loss, reason = "a few streams")]
    let spread = work / jobs as f64;
    let seconds = longest.max(spread);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seconds past u64 saturate"
    )]
    let (rounded, floored) = (seconds.round() as u64, seconds.floor() as u64);
    if known {
        return Some(Time {
            bound: Bound::About,
            seconds: rounded,
        });
    }
    (seconds >= FLOOR_SHOWN).then_some(Time {
        bound: Bound::AtLeast,
        seconds: floored,
    })
}

/// what a run would download: its files, their known bytes, how many sizes only the downloads
/// tell, and how long they should take, as `--json` gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Total {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    pub(crate) sizes_unknown: u64,
    /// none without grounds: the time shows once the downloads start.
    pub(crate) time: Option<Time>,
}

impl Total {
    /// `hosts` added up, with how long they should take under `limits`.
    pub(crate) fn of(hosts: &[Host], limits: Limits) -> Total {
        Total {
            files: hosts.iter().map(|host| host.files).sum(),
            bytes: hosts
                .iter()
                .map(|host| host.bytes)
                .fold(0, u64::saturating_add),
            sizes_unknown: hosts.iter().map(|host| host.unknown).sum(),
            time: hosts
                .iter()
                .any(|host| host.files > 0)
                .then(|| time(hosts, limits))
                .flatten(),
        }
    }

    /// whether any size is known, so the total says something before the downloads start.
    pub(crate) fn is_sized(&self) -> bool {
        self.files > self.sizes_unknown
    }

    /// the facts after what downloads: `1.2 GiB · about 3 min`, or without grounds `1.2 GiB · the
    /// time shows once it starts`, with `and 2 of unknown size` when some sizes are not known.
    pub(crate) fn facts(&self) -> String {
        let size = match (self.sizes_unknown, self.is_sized()) {
            (0, _) => units::size(self.bytes),
            (_, false) => "size unknown".to_owned(),
            (unknown, true) => format!(
                "{} and {} of unknown size",
                units::size(self.bytes),
                units::grouped(unknown)
            ),
        };
        let time = self
            .time
            .map_or_else(|| "the time shows once it starts".to_owned(), Time::words);
        format!("{size} · {time}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    /// a MiB, as rates and sizes in bytes a second count it.
    const A_MIB: f64 = 1_048_576.0;
    const LIMITS: Limits = Limits {
        jobs: 8,
        host_jobs: 4,
    };

    fn known(files: u64, bytes: u64, largest: u64, rate: f64, overhead: f64) -> Host {
        Host {
            files,
            bytes,
            largest,
            speed: Some(Speed { rate, overhead }),
            ..Host::default()
        }
    }

    fn about(seconds: u64) -> Time {
        Time {
            bound: Bound::About,
            seconds,
        }
    }

    fn at_least(seconds: u64) -> Time {
        Time {
            bound: Bound::AtLeast,
            seconds,
        }
    }

    /// ten files of 10 MiB at 1 MiB/s, half a second each to answer, over four streams: 105 s of
    /// work in four, 26 s, longer than the largest file alone, 10.5 s.
    #[test]
    fn a_known_host_spreads_its_work_over_its_streams() {
        let host = known(10, 100 * MIB, 10 * MIB, A_MIB, 0.5);
        assert_eq!(time(&[host], LIMITS), Some(about(26)));
    }

    /// one file of 100 MiB and three tiny ones cannot share the large one's stream.
    #[test]
    fn the_largest_file_takes_one_stream_alone() {
        let host = known(4, 100 * MIB + 3, 100 * MIB, A_MIB, 0.0);
        assert_eq!(time(&[host], LIMITS), Some(about(100)));
    }

    /// arXiv's one request at a time, 3 s apart: 100 papers take at least 300 s however fast
    /// each arrives, and with no speed remembered that floor is all there is.
    #[test]
    fn pacing_is_a_floor_even_for_a_host_never_seen() {
        let papers = Host {
            files: 100,
            bytes: 100 * MIB,
            largest: MIB,
            spacing: Some(Duration::from_secs(3)),
            most: Some(1),
            ..Host::default()
        };
        assert_eq!(
            time(std::slice::from_ref(&papers), LIMITS),
            Some(at_least(300))
        );
        let fast = Host {
            speed: Some(Speed {
                rate: 100.0 * A_MIB,
                overhead: 0.1,
            }),
            ..papers.clone()
        };
        assert_eq!(time(&[fast], LIMITS), Some(about(300)));
        let slow = Host {
            speed: Some(Speed {
                rate: A_MIB / 4.0,
                overhead: 0.1,
            }),
            ..papers
        };
        // one stream: 100 × 0.1 s + 400 s
        assert_eq!(time(&[slow], LIMITS), Some(about(410)));
    }

    /// a host fetchloom knows nothing about gives no number, and a floor under a minute none.
    #[test]
    fn no_grounds_give_no_time() {
        let unknown = Host {
            files: 3,
            bytes: 30 * MIB,
            largest: 10 * MIB,
            ..Host::default()
        };
        assert_eq!(time(std::slice::from_ref(&unknown), LIMITS), None);
        let spaced = Host {
            spacing: Some(Duration::from_secs(1)),
            ..unknown.clone()
        };
        assert_eq!(time(&[spaced], LIMITS), None);
        let unsure = Host {
            unknown: 1,
            ..known(3, 20 * MIB, 10 * MIB, A_MIB, 0.0)
        };
        assert_eq!(time(&[unsure], LIMITS), None);
    }

    /// what is known is a floor for the whole once another host has no estimate.
    #[test]
    fn a_host_without_grounds_leaves_the_rest_as_a_floor() {
        let slow = known(1, 120 * MIB, 120 * MIB, A_MIB, 0.0);
        let unknown = Host {
            files: 1,
            bytes: MIB,
            largest: MIB,
            ..Host::default()
        };
        assert_eq!(time(&[slow, unknown], LIMITS), Some(at_least(120)));
    }

    /// two hosts side by side take the longer, unless every stream of the run is busy: three
    /// hosts of 4 streams each with 2 jobs in all share those 2.
    #[test]
    fn hosts_run_side_by_side_within_the_jobs_overall() {
        let a = known(4, 40 * MIB, 10 * MIB, A_MIB, 0.0);
        let b = known(4, 80 * MIB, 20 * MIB, A_MIB, 0.0);
        assert_eq!(time(&[a.clone(), b.clone()], LIMITS), Some(about(20)));
        let few = Limits {
            jobs: 2,
            host_jobs: 4,
        };
        let c = known(4, 40 * MIB, 10 * MIB, A_MIB, 0.0);
        // 160 s of work over 2 streams
        assert_eq!(time(&[a, b, c], few), Some(about(80)));
    }

    #[test]
    fn a_total_says_its_size_and_time_or_that_the_time_shows_later() {
        let host = known(2, 3 * MIB, 2 * MIB, A_MIB, 0.0);
        let total = Total::of(&[host], LIMITS);
        assert_eq!(total.files, 2);
        assert_eq!(total.facts(), "3.0 MiB · under 10 s");
        let unknown = Host {
            files: 3,
            bytes: MIB,
            largest: MIB,
            unknown: 2,
            ..Host::default()
        };
        assert_eq!(
            Total::of(&[unknown], LIMITS).facts(),
            "1.0 MiB and 2 of unknown size · the time shows once it starts"
        );
    }
}
