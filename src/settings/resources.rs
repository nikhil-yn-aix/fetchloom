//! `resources`, one word that trades speed against memory: what a level sets on the knobs it
//! derives from the machine, which a knob set by name overrides, and what `-v` says a run spends.

use std::fmt;

use super::{LEAST_MEMORY, MOST_MEMORY, Origin, Settings};
use crate::machine::Machine;
use crate::threads::Threads;
use crate::units;

/// the memory lean gives a run's buffers at most. no measure found a run slower within it.
const LEAN_MEMORY: u64 = 128 << 20;
/// downloads at once, and at once to one host, at lean.
const LEAN_JOBS: usize = 2;
const LEAN_HOST_JOBS: usize = 2;
/// the most workers one local group runs at lean.
const LEAN_THREADS: usize = 2;
/// downloads at once, and at once to one host, at max: past these a far link with many small
/// files measured little more gain, and a near one none.
const MAX_JOBS: usize = 32;
const MAX_HOST_JOBS: usize = 16;

/// how much of the machine a run may spend for speed: lean spends less memory and fewer
/// connections, max more connections, and default what fetchloom spends without a level.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Level {
    Lean,
    #[default]
    Default,
    Max,
}

/// what a level gives the knobs it derives. none leaves a knob at fetchloom's default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Limits {
    pub(crate) jobs: Option<usize>,
    pub(crate) host_jobs: Option<usize>,
    /// the most workers one local group runs, as `FETCHLOOM_THREADS` sets it.
    pub(crate) threads: Option<usize>,
    pub(crate) memory: Option<u64>,
}

impl Level {
    /// every level, in the order a message names them.
    pub(crate) const ALL: [Level; 3] = [Level::Lean, Level::Default, Level::Max];

    /// the name the settings take.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Level::Lean => "lean",
            Level::Default => "default",
            Level::Max => "max",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|level| level.name() == text.trim())
    }

    /// what `self` gives the knobs it derives on `machine`. default gives none, and max spends no
    /// memory or local worker no measure showed a gain for.
    pub(crate) fn limits(self, machine: Machine) -> Limits {
        match self {
            Level::Lean => Limits {
                jobs: Some(LEAN_JOBS),
                host_jobs: Some(LEAN_HOST_JOBS),
                threads: Some(machine.processors.clamp(1, LEAN_THREADS)),
                memory: Some(super::budget(machine).value.min(LEAN_MEMORY)),
            },
            Level::Default => Limits::default(),
            Level::Max => Limits {
                jobs: Some(MAX_JOBS),
                host_jobs: Some(MAX_HOST_JOBS),
                ..Limits::default()
            },
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// what a run spends, as `-v` tells it when the run starts: the numbers its settings come to on
/// this machine, and what holds each that fetchloom's own defaults do not.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Spending {
    pub(crate) level: Level,
    /// downloads at once.
    pub(crate) jobs: usize,
    /// downloads at once to one host, never more than `jobs`.
    pub(crate) host_jobs: usize,
    /// the most workers one local step runs.
    pub(crate) workers: usize,
    /// the budget download buffers aim to stay within.
    pub(crate) memory: u64,
    /// workers a download hashes on: a second one takes a required MD5 beside the SHA-256.
    pub(crate) lanes: usize,
    /// a line for each of these that a level, a setting or the machine holds where it is,
    /// saying what does, as `buffers: a quarter of the 2.0 GiB available`.
    pub(crate) reasons: Vec<String>,
}

impl Settings {
    /// what a run on these settings spends, with `threads` its cap on local workers and `online`
    /// the processors the system has, whether or not this process may use them all.
    pub(crate) fn spending(&self, threads: Threads, online: Option<usize>) -> Spending {
        let processors = self.machine.processors.max(1);
        let jobs = self.jobs.value;
        let workers = threads.limit(processors);
        let hashers = threads.limit(self.hash_workers());
        let lanes = hashers.clamp(1, 2);
        let level = || self.threads.map(|set| set.origin.to_string());
        let mut reasons = Vec::new();
        let mut why = |what: &str, reason: Option<String>| {
            if let Some(reason) = reason {
                reasons.push(format!("{what}: {reason}"));
            }
        };
        why("downloads", named(self.jobs.origin, "jobs"));
        why(
            "to one host",
            if self.host_jobs.value > jobs {
                Some("no more than the downloads at once".to_owned())
            } else {
                named(self.host_jobs.origin, "host_jobs")
            },
        );
        why(
            "local workers",
            if workers < processors {
                Some(level().unwrap_or_else(|| "FETCHLOOM_THREADS".to_owned()))
            } else {
                online.filter(|&online| processors < online).map(|online| {
                    format!("this run may use {processors} of the {online} processors")
                })
            },
        );
        why("buffers", self.buffers());
        why(
            "hash lanes",
            (lanes < 2).then(|| match level() {
                Some(level) if self.hash_workers() < processors => level,
                _ if processors > 1 => "FETCHLOOM_THREADS".to_owned(),
                _ => "one processor".to_owned(),
            }),
        );
        Spending {
            level: self.resources.value,
            jobs,
            host_jobs: self.host_jobs.value.min(jobs),
            workers,
            memory: self.memory.value,
            lanes,
            reasons,
        }
    }

    /// what holds the memory budget where it is.
    fn buffers(&self) -> Option<String> {
        let budget = self.memory.value;
        match (self.memory.origin, self.machine.memory) {
            (Origin::Machine, Some(room)) => {
                let place = if self.machine.memory_in_cgroup {
                    "this cgroup leaves"
                } else {
                    "available"
                };
                let room_text = format!("the {} {place}", units::size(room));
                let quarter = room / 4;
                Some(if quarter > MOST_MEMORY {
                    format!(
                        "{} at most, less than a quarter of {room_text}",
                        units::size(budget)
                    )
                } else if budget == room / 2 && quarter < budget {
                    format!("half of {room_text}")
                } else if quarter < budget {
                    format!(
                        "{} at least, more than a quarter of {room_text}",
                        units::size(LEAST_MEMORY)
                    )
                } else {
                    format!("a quarter of {room_text}")
                })
            }
            (Origin::Default, None) => {
                Some("this system does not say how much memory it has".to_owned())
            }
            (origin, _) => named(origin, "memory"),
        }
    }
}

/// what set `key` when neither fetchloom's default nor the machine did: a level, its variable,
/// the settings file or the command line.
fn named(origin: Origin, key: &str) -> Option<String> {
    match origin {
        Origin::Default | Origin::Machine => None,
        Origin::Environment => Some(super::variable(key).to_owned()),
        Origin::File => Some(format!("{key} in the settings file")),
        Origin::Flag => Some("the command line".to_owned()),
        Origin::Resources(level) => Some(format!("resources = {level}")),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::super::{File, Settings, parse};
    use super::{Level, Spending};
    use crate::machine::Machine;
    use crate::threads::Threads;

    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(set, _)| set == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn file(text: &str) -> File {
        let path = PathBuf::from("/home/ada/.config/fetchloom/settings.toml");
        File {
            values: parse(&path, text).unwrap(),
            path: Some(path),
            found: true,
            bypassed: None,
        }
    }

    fn read(pairs: &[(&str, &str)], text: &str, machine: Machine) -> Settings {
        Settings::read(&env(pairs), file(text), machine).unwrap()
    }

    /// a machine of `processors` and `memory`, as `Machine::read` would find it.
    fn machine(processors: usize, memory: Option<u64>) -> Machine {
        Machine {
            processors,
            memory,
            open_files: Some(1024),
            ..Machine::default()
        }
    }

    /// each knob a level derives, as a run would use it, with where its value came from, as
    /// `--json` names it: `jobs`, `host_jobs`, `memory` and `most_files`.
    fn knobs(settings: &Settings) -> [(u64, &'static str); 4] {
        [
            (settings.jobs.value as u64, settings.jobs.origin.name()),
            (
                settings.host_jobs.value as u64,
                settings.host_jobs.origin.name(),
            ),
            (settings.memory.value, settings.memory.origin.name()),
            (
                settings.most_files.value as u64,
                settings.most_files.origin.name(),
            ),
        ]
    }

    /// the machines are big, small, tiny with a default budget below lean's, and one of unknown
    /// memory. `most_files` stays sized from the default level's memory.
    #[test]
    fn a_level_derives_each_knob_from_the_machine() {
        let big = machine(16, Some(16 * GIB));
        let small = machine(1, Some(GIB));
        let tiny = machine(1, Some(200 * MIB));
        let unknown = machine(4, None);
        let defaults = [(8, "default"), (4, "default")];
        let lean = [(2, "resources"), (2, "resources")];
        let max = [(32, "resources"), (16, "resources")];
        for (level, machine, [jobs, host_jobs], memory, most_files) in [
            (
                "",
                big,
                defaults,
                (4 * GIB, "machine"),
                (2_097_152, "machine"),
            ),
            (
                "default",
                big,
                defaults,
                (4 * GIB, "machine"),
                (2_097_152, "machine"),
            ),
            (
                "lean",
                big,
                lean,
                (128 * MIB, "resources"),
                (2_097_152, "machine"),
            ),
            (
                "lean",
                small,
                lean,
                (128 * MIB, "resources"),
                (131_072, "machine"),
            ),
            (
                "lean",
                tiny,
                lean,
                (100 * MIB, "resources"),
                (100_000, "default"),
            ),
            (
                "lean",
                unknown,
                lean,
                (128 * MIB, "resources"),
                (131_072, "default"),
            ),
            (
                "max",
                big,
                max,
                (4 * GIB, "machine"),
                (2_097_152, "machine"),
            ),
            (
                "max",
                small,
                max,
                (256 * MIB, "machine"),
                (131_072, "machine"),
            ),
        ] {
            let settings = read(&[("FETCHLOOM_RESOURCES", level)], "", machine);
            assert_eq!(
                knobs(&settings),
                [jobs, host_jobs, memory, most_files],
                "{level} {machine:?}"
            );
        }
    }

    /// memory set by name also sizes `most_files`, as it does without a level.
    #[test]
    fn a_knob_set_by_name_beats_a_level_from_the_environment_or_the_file() {
        let big = machine(16, Some(16 * GIB));
        let lean = ("FETCHLOOM_RESOURCES", "lean");
        let max = ("FETCHLOOM_RESOURCES", "max");
        let derived = (2, "resources");
        let budget = (128 * MIB, "resources");
        for (named, text, expected) in [
            (
                &[("FETCHLOOM_JOBS", "6")][..],
                "resources = \"lean\"\n",
                [(6, "environment"), derived, budget, (2_097_152, "machine")],
            ),
            (
                &[],
                "resources = \"lean\"\njobs = 3\n",
                [(3, "file"), derived, budget, (2_097_152, "machine")],
            ),
            (
                &[max],
                "host_jobs = 3\n",
                [
                    (32, "resources"),
                    (3, "file"),
                    (4 * GIB, "machine"),
                    (2_097_152, "machine"),
                ],
            ),
            (
                &[lean, ("FETCHLOOM_MEMORY", "1G")],
                "",
                [derived, derived, (GIB, "environment"), (524_288, "machine")],
            ),
            (
                &[lean],
                "memory = \"64M\"\nmost_files = 7\n",
                [derived, derived, (64 * MIB, "file"), (7, "file")],
            ),
        ] {
            let settings = read(named, text, big);
            assert_eq!(knobs(&settings), expected, "{named:?} {text:?}");
        }
    }

    /// lean caps each local group at two workers, or one on a machine of one processor, as
    /// `FETCHLOOM_THREADS` would. the other levels leave the workers as they are.
    #[test]
    fn lean_caps_local_workers_unless_threads_are_set_by_name() {
        let lean = [("FETCHLOOM_RESOURCES", "lean")];
        for (named, processors, expected) in [
            (&lean[..], 16, Some((2, "resources"))),
            (&lean, 1, Some((1, "resources"))),
            (
                &[("FETCHLOOM_RESOURCES", "lean"), ("FETCHLOOM_THREADS", "8")],
                16,
                None,
            ),
            (&[("FETCHLOOM_RESOURCES", "max")], 16, None),
            (&[("FETCHLOOM_RESOURCES", "default")], 16, None),
            (&[], 16, None),
        ] {
            let settings = read(named, "", machine(processors, Some(GIB)));
            let threads = settings.threads.map(|set| (set.value, set.origin.name()));
            assert_eq!(threads, expected, "{named:?} on {processors}");
        }
    }

    /// no level derives the limits on listings and archives, retries, the timeout, the size that
    /// asks first, mirrors, buckets or where push writes.
    #[test]
    fn the_default_level_keeps_the_defaults_and_no_level_moves_what_it_does_not_derive() {
        let text = "mirrors = [\"/mnt/lab/mirror\"]\ns3_buckets = [\"s3://lab-raw\"]\npush_to = \"/mnt/lab/push\"\n";
        for machine in [
            machine(16, Some(16 * GIB)),
            machine(2, Some(GIB)),
            machine(4, None),
        ] {
            let unset = read(&[], text, machine);
            for named in ["", "default", "lean", "max"] {
                let leveled = read(&[("FETCHLOOM_RESOURCES", named)], text, machine);
                let untouched = |settings: &Settings| {
                    format!(
                        "{:?}",
                        (
                            settings.retries,
                            settings.timeout,
                            settings.ask_above,
                            settings.most_files,
                            settings.archive_limits(),
                            &settings.allowed_origins,
                            &settings.s3_buckets,
                            (&settings.gcs_buckets, &settings.azure_containers),
                            &settings.mirrors,
                            settings.mirrors_only,
                            &settings.push_to,
                            settings.build_floor,
                        )
                    )
                };
                assert_eq!(
                    untouched(&leveled),
                    untouched(&unset),
                    "{named} {machine:?}"
                );
                if matches!(named, "" | "default") {
                    let defaults = Settings::defaults(machine);
                    assert_eq!(
                        (
                            leveled.jobs,
                            leveled.host_jobs,
                            leveled.memory,
                            leveled.threads
                        ),
                        (
                            defaults.jobs,
                            defaults.host_jobs,
                            defaults.memory,
                            defaults.threads
                        ),
                        "{named} {machine:?}"
                    );
                }
            }
        }
    }

    /// the error names the three levels, worded as other settings' errors, from either source.
    #[test]
    fn a_level_that_is_none_of_the_three_is_a_usage_error() {
        let big = machine(16, Some(16 * GIB));
        let error =
            Settings::read(&env(&[("FETCHLOOM_RESOURCES", "big")]), file(""), big).unwrap_err();
        assert_eq!(error.kind, crate::error::Kind::Usage);
        assert_eq!(
            error.message,
            "FETCHLOOM_RESOURCES is \"big\", which is not lean, default or max"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_RESOURCES to use the default level, or set it to lean, default or max"
            )
        );
        let error = Settings::read(&env(&[]), file("resources = \"fast\"\n"), big).unwrap_err();
        assert_eq!(
            error.message,
            "resources in the settings file /home/ada/.config/fetchloom/settings.toml is \"fast\", which is not lean, default or max"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "remove resources from the settings file to use the default level, or set it to lean, default or max"
            )
        );
    }

    /// no level changes `most_files` or the archive limits, though lean spends less memory.
    #[test]
    fn lean_keeps_most_files_and_the_archive_limits() {
        let big = machine(16, Some(16 * GIB));
        let lean = read(&[("FETCHLOOM_RESOURCES", "lean")], "", big);
        let default = read(&[], "", big);
        assert!(lean.memory.value < default.memory.value);
        assert_eq!(lean.most_files, default.most_files);
        assert_eq!(lean.archive_limits(), default.archive_limits());
    }

    /// what a run on `pairs` and the settings file `text` spends on `machine`, its local worker
    /// cap set as a run sets it: from `FETCHLOOM_THREADS`, then at most the level's.
    fn spent(pairs: &[(&str, &str)], text: &str, machine: Machine, online: usize) -> Spending {
        let settings = read(pairs, text, machine);
        let mut threads = Threads::read(&env(pairs)).unwrap();
        if let Some(cap) = settings.threads {
            threads = threads.at_most(cap.value);
        }
        settings.spending(threads, Some(online))
    }

    /// what a run spends at `level`: downloads at once and to one host, local workers and hash
    /// lanes, the memory budget, and what holds each number fetchloom's defaults do not.
    fn spends(level: Level, counts: [usize; 4], memory: u64, reasons: &[&str]) -> Spending {
        let [jobs, host_jobs, workers, lanes] = counts;
        Spending {
            level,
            jobs,
            host_jobs,
            workers,
            memory,
            lanes,
            reasons: reasons.iter().map(|&reason| reason.to_owned()).collect(),
        }
    }

    /// variables, the settings file and a level, with the processors and memory they take.
    type Case<'a> = (&'a [(&'a str, &'a str)], &'a str, Machine, usize, Spending);

    fn check(cases: Vec<Case<'_>>) {
        for (pairs, text, machine, online, expected) in cases {
            assert_eq!(
                spent(pairs, text, machine, online),
                expected,
                "{pairs:?} {text:?}"
            );
        }
    }

    /// downloads to one host never pass downloads at once.
    #[test]
    fn a_run_names_the_level_or_setting_behind_each_number() {
        let roomy = machine(8, Some(8 * GIB));
        let quarter = "buffers: a quarter of the 8.0 GiB available";
        check(vec![
            (
                &[],
                "",
                roomy,
                8,
                spends(Level::Default, [8, 4, 8, 2], 2 * GIB, &[quarter]),
            ),
            (
                &[("FETCHLOOM_RESOURCES", "max")],
                "",
                roomy,
                8,
                spends(
                    Level::Max,
                    [32, 16, 8, 2],
                    2 * GIB,
                    &[
                        "downloads: resources = max",
                        "to one host: resources = max",
                        quarter,
                    ],
                ),
            ),
            (
                &[("FETCHLOOM_RESOURCES", "lean")],
                "",
                roomy,
                8,
                spends(
                    Level::Lean,
                    [2, 2, 2, 1],
                    128 * MIB,
                    &[
                        "downloads: resources = lean",
                        "to one host: resources = lean",
                        "local workers: resources = lean",
                        "buffers: resources = lean",
                        "hash lanes: resources = lean",
                    ],
                ),
            ),
            (
                &[
                    ("FETCHLOOM_JOBS", "3"),
                    ("FETCHLOOM_HOST_JOBS", "5"),
                    ("FETCHLOOM_THREADS", "1"),
                    ("FETCHLOOM_MEMORY", "64M"),
                ],
                "",
                roomy,
                8,
                spends(
                    Level::Default,
                    [3, 3, 1, 1],
                    64 * MIB,
                    &[
                        "downloads: FETCHLOOM_JOBS",
                        "to one host: no more than the downloads at once",
                        "local workers: FETCHLOOM_THREADS",
                        "buffers: FETCHLOOM_MEMORY",
                        "hash lanes: FETCHLOOM_THREADS",
                    ],
                ),
            ),
            (
                &[],
                "host_jobs = 2\n",
                roomy,
                8,
                spends(
                    Level::Default,
                    [8, 2, 8, 2],
                    2 * GIB,
                    &["to one host: host_jobs in the settings file", quarter],
                ),
            ),
        ]);
    }

    /// the machine limits are the usable processors and the memory the system or a cgroup leaves.
    #[test]
    fn a_run_names_the_machine_limits_behind_each_number() {
        let in_cgroup = |processors, memory| Machine {
            memory_in_cgroup: true,
            ..machine(processors, Some(memory))
        };
        check(vec![
            (
                &[],
                "",
                machine(12, Some(64 * GIB)),
                32,
                spends(
                    Level::Default,
                    [8, 4, 12, 2],
                    4 * GIB,
                    &[
                        "local workers: this run may use 12 of the 32 processors",
                        "buffers: 4.0 GiB at most, less than a quarter of the 64.0 GiB available",
                    ],
                ),
            ),
            (
                &[("FETCHLOOM_RESOURCES", "lean")],
                "",
                in_cgroup(2, GIB),
                24,
                spends(
                    Level::Lean,
                    [2, 2, 2, 1],
                    128 * MIB,
                    &[
                        "downloads: resources = lean",
                        "to one host: resources = lean",
                        "local workers: this run may use 2 of the 24 processors",
                        "buffers: resources = lean",
                        "hash lanes: resources = lean",
                    ],
                ),
            ),
            (
                &[],
                "",
                in_cgroup(4, 300 * MIB),
                4,
                spends(
                    Level::Default,
                    [8, 4, 4, 2],
                    150 * MIB,
                    &["buffers: half of the 300.0 MiB this cgroup leaves"],
                ),
            ),
            (
                &[],
                "",
                machine(4, Some(800 * MIB)),
                4,
                spends(
                    Level::Default,
                    [8, 4, 4, 2],
                    256 * MIB,
                    &[
                        "buffers: 256.0 MiB at least, more than a quarter of the 800.0 MiB available",
                    ],
                ),
            ),
            (
                &[],
                "",
                machine(4, None),
                4,
                spends(
                    Level::Default,
                    [8, 4, 4, 2],
                    256 * MIB,
                    &["buffers: this system does not say how much memory it has"],
                ),
            ),
            (
                &[],
                "",
                in_cgroup(1, 2 * GIB),
                1,
                spends(
                    Level::Default,
                    [8, 4, 1, 1],
                    512 * MIB,
                    &[
                        "buffers: a quarter of the 2.0 GiB this cgroup leaves",
                        "hash lanes: one processor",
                    ],
                ),
            ),
        ]);
    }
}
