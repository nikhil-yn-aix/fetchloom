//! `resources`, one word that trades speed against memory: what a level sets on the knobs it
//! derives from the machine, which a knob set by name always overrides.

use std::fmt;

use crate::machine::Machine;

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

    /// the level `text` names.
    pub(crate) fn parse(text: &str) -> Option<Level> {
        Level::ALL
            .into_iter()
            .find(|level| level.name() == text.trim())
    }

    /// what `self` gives the knobs it derives on `machine`. the default level gives none, so it
    /// leaves every knob at fetchloom's default, and max spends no memory or local worker no
    /// measure showed a gain for.
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::super::{File, Settings, parse};
    use crate::machine::Machine;

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

    /// what each level gives the knobs it derives on a big machine, a small one, a tiny one whose
    /// default budget is below lean's, and one that does not say its memory. `most_files` stays
    /// sized from the memory the default level gives.
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

    /// a knob set by name, in the environment or the settings file, beats the level wherever the
    /// level came from. memory set by name also sizes `most_files`, as it does without a level.
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
    /// `FETCHLOOM_THREADS` would, unless that variable sets the cap itself. the other levels
    /// leave the workers as they are.
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

    /// the default level, set or not, leaves every knob at fetchloom's defaults, and no level
    /// moves a setting it does not derive: limits on what a source may list or an
    /// archive may unpack to, retries, the timeout, the size that asks first, mirrors, buckets
    /// and where push writes.
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
                            &settings.gcs_buckets,
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

    /// a level that is none of the three is a usage error naming them, worded as the other
    /// settings' errors are, from the environment or from the settings file.
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

    /// lean spends less memory but lists as many files as the default level, and unpacks
    /// archives within the same limits, which no level changes.
    #[test]
    fn lean_keeps_most_files_and_the_archive_limits() {
        let big = machine(16, Some(16 * GIB));
        let lean = read(&[("FETCHLOOM_RESOURCES", "lean")], "", big);
        let default = read(&[], "", big);
        assert!(lean.memory.value < default.memory.value);
        assert_eq!(lean.most_files, default.most_files);
        assert_eq!(lean.archive_limits(), default.archive_limits());
    }
}
