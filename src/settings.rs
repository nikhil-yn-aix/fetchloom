//! every setting a run reads, read once when it starts: each value and where it came from, so
//! `fetchloom settings` can show what a run would use and why. a value comes from a `FETCHLOOM_*`
//! environment variable, else from the settings file, else from this machine, else from
//! fetchloom's defaults.

use std::ffi::OsString;
use std::fmt;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Error, Kind};
use crate::machine::Machine;
use crate::source::Bucket;
use crate::units;

/// reads one environment variable, so tests can give each run its own.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// downloads running at once, across all hosts. downloads wait on the network, not the
/// processor, so the number does not follow the core count.
pub(crate) const JOBS: usize = 8;
/// requests running at once to one host, so no server sees fetchloom as a flood.
pub(crate) const HOST_JOBS: usize = 4;
/// how long a request may wait for an answer, or a download stay silent, before it is tried
/// again.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
/// downloads above this size ask first, at a terminal, or need `--yes` without one.
pub(crate) const ASK_ABOVE: u64 = 1 << 30;
/// the least memory a run budgets for its buffers, and what it budgets when the machine does not
/// say how much it has.
pub(crate) const LEAST_MEMORY: u64 = 256 << 20;
/// the most memory a run budgets for its buffers on its own, however much the machine has.
const MOST_MEMORY: u64 = 4 << 30;

/// the settings file's name, in fetchloom's folder beside the credentials file.
const FILE: &str = "settings.toml";
/// the keys the settings file takes, each with the variable that overrides it.
const KEYS: [(&str, &str); 13] = [
    ("cache", "FETCHLOOM_CACHE"),
    ("jobs", "FETCHLOOM_JOBS"),
    ("host_jobs", "FETCHLOOM_HOST_JOBS"),
    ("retries", "FETCHLOOM_RETRIES"),
    ("timeout", "FETCHLOOM_TIMEOUT"),
    ("ask_above", "FETCHLOOM_ASK_ABOVE"),
    ("memory", "FETCHLOOM_MEMORY"),
    ("most_files", "FETCHLOOM_MOST_FILES"),
    ("archive_files", "FETCHLOOM_ARCHIVE_FILES"),
    ("archive_size", "FETCHLOOM_ARCHIVE_SIZE"),
    ("archive_ratio", "FETCHLOOM_ARCHIVE_RATIO"),
    ("allowed_origins", "FETCHLOOM_ALLOWED_ORIGINS"),
    ("s3_buckets", "FETCHLOOM_S3_BUCKETS"),
];
/// the keys that hold a list: an array of text in the settings file, text separated by commas in
/// a variable.
const LISTS: [&str; 2] = ["allowed_origins", "s3_buckets"];

/// where a setting's value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Origin {
    /// fetchloom's own default.
    Default,
    /// sized from this machine: its processors or memory.
    Machine,
    /// a `FETCHLOOM_*` environment variable.
    Environment,
    /// the settings file.
    File,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Origin::Default => "default",
            Origin::Machine => "this machine",
            Origin::Environment => "environment",
            Origin::File => "settings file",
        })
    }
}

/// a setting's value and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Set<T> {
    pub(crate) value: T,
    pub(crate) origin: Origin,
}

impl<T> Set<T> {
    fn default(value: T) -> Set<T> {
        Set {
            value,
            origin: Origin::Default,
        }
    }
}

/// the settings file a run read, if it found one.
#[derive(Clone, Debug, Default)]
pub(crate) struct File {
    /// where the file is, or would be.
    pub(crate) path: Option<PathBuf>,
    /// its values by key, as text, when it was there.
    values: Vec<(String, String)>,
    pub(crate) found: bool,
}

impl File {
    /// the settings file: the one `FETCHLOOM_SETTINGS` names, else `settings.toml` in fetchloom's
    /// folder.
    ///
    /// # Errors
    ///
    /// `usage` when the file named by the variable is missing, or when the file holds a key it does
    /// not take or a value that is not text or a whole number; `local.io` when it cannot be read.
    pub(crate) fn read(var: Var<'_>) -> Result<File, Error> {
        let named = var("FETCHLOOM_SETTINGS")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let path = named
            .clone()
            .or_else(|| crate::auth::store::folder(var).map(|folder| folder.join(FILE)));
        let Some(path) = path else {
            return Ok(File::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && named.is_none() => {
                return Ok(File {
                    path: Some(path),
                    values: Vec::new(),
                    found: false,
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::new(
                    Kind::Usage,
                    format!(
                        "FETCHLOOM_SETTINGS names {}, which does not exist",
                        path.display()
                    ),
                )
                .help("unset FETCHLOOM_SETTINGS, or set it to a settings file that exists"));
            }
            Err(err) => {
                let help = if named.is_some() {
                    "unset FETCHLOOM_SETTINGS, or set it to a settings file fetchloom may read"
                } else {
                    "check its permissions, or set FETCHLOOM_SETTINGS to a settings file fetchloom may read"
                };
                return Err(Error::io(
                    format_args!("could not read the settings file {}", path.display()),
                    &err,
                )
                .or_help(help));
            }
        };
        let values = parse(&path, &text)?;
        Ok(File {
            path: Some(path),
            values,
            found: true,
        })
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(set, _)| set == key)
            .map(|(_, value)| value.as_str())
    }

    /// the cache folder the file names, if any. a relative folder is taken from the file's own
    /// folder, so the file means the same wherever fetchloom starts.
    pub(crate) fn cache(&self) -> Option<PathBuf> {
        let folder = PathBuf::from(self.get("cache")?);
        Some(match self.path.as_deref().and_then(Path::parent) {
            Some(parent) if folder.is_relative() => parent.join(folder),
            _ => folder,
        })
    }
}

/// the settings file's values as text, refusing a key it does not take, so a mistyped key is
/// told rather than quietly ignored.
fn parse(path: &Path, text: &str) -> Result<Vec<(String, String)>, Error> {
    let invalid = |message: String| {
        Error::new(Kind::Usage, message).help(format!(
            "the settings file takes {}, each a number or text",
            KEYS.map(|(key, _)| key).join(", ")
        ))
    };
    let table: toml::Table = toml::from_str(text).map_err(|err| {
        invalid(format!(
            "the settings file {} is not valid TOML: {}",
            path.display(),
            err.message()
        ))
    })?;
    table
        .into_iter()
        .map(|(key, value)| {
            if !KEYS.iter().any(|(known, _)| *known == key) {
                return Err(invalid(format!(
                    "the settings file {} has \"{key}\", which is not a setting",
                    path.display()
                )));
            }
            let text = match value {
                toml::Value::String(text) => text,
                toml::Value::Integer(number) => number.to_string(),
                toml::Value::Array(items) if LISTS.contains(&key.as_str()) => items
                    .into_iter()
                    .map(|item| match item {
                        toml::Value::String(text) => Ok(text),
                        other => Err(invalid(format!(
                            "{key} in the settings file {} holds a {}, not text in quotes",
                            path.display(),
                            other.type_str()
                        ))),
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join(","),
                other => {
                    return Err(invalid(format!(
                        "{key} in the settings file {} is a {}, not a whole number or text in quotes",
                        path.display(),
                        other.type_str()
                    )));
                }
            };
            Ok((key, text))
        })
        .collect()
}

/// every setting a run uses.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) machine: Machine,
    pub(crate) file: File,
    pub(crate) jobs: Set<usize>,
    pub(crate) host_jobs: Set<usize>,
    pub(crate) retries: Set<u32>,
    pub(crate) timeout: Set<Duration>,
    pub(crate) ask_above: Set<u64>,
    /// the memory a run's buffers aim to stay within: download queues share it.
    pub(crate) memory: Set<u64>,
    /// the most files one dataset may list, sized from `memory` unless set.
    pub(crate) most_files: Set<usize>,
    /// the most entries one archive may unpack to.
    pub(crate) archive_files: Set<u64>,
    /// the most bytes one archive may unpack to.
    pub(crate) archive_size: Set<u64>,
    /// the most bytes one archive may unpack to for each compressed byte.
    pub(crate) archive_ratio: Set<u64>,
    /// the origins, such as `http://lab.local:8080`, that may be on this machine or its network
    /// though only data.toml or data.lock names them.
    pub(crate) allowed_origins: Set<Vec<url::Origin>>,
    /// the S3 buckets a request may be signed for with this machine's AWS keys, after S3 refused
    /// it without one.
    pub(crate) s3_buckets: Set<Vec<Bucket>>,
}

impl Settings {
    /// every setting at fetchloom's defaults, with what is sized from `machine` sized from it.
    pub(crate) fn defaults(machine: Machine) -> Settings {
        // a quarter of what the machine gives, but never more than half of it, however small
        let memory = match machine.memory {
            Some(room) => Set {
                value: (room / 4).clamp(LEAST_MEMORY.min(room / 2), MOST_MEMORY),
                origin: Origin::Machine,
            },
            None => Set::default(LEAST_MEMORY),
        };
        Settings {
            machine,
            file: File::default(),
            jobs: Set::default(JOBS),
            host_jobs: Set::default(HOST_JOBS),
            retries: Set::default(crate::net::RETRIES),
            timeout: Set::default(TIMEOUT),
            ask_above: Set::default(ASK_ABOVE),
            most_files: most_files(memory),
            archive_files: Set::default(crate::archive::Limits::default().members),
            archive_size: Set::default(crate::archive::Limits::default().bytes),
            archive_ratio: Set::default(crate::archive::Limits::default().ratio),
            allowed_origins: Set::default(Vec::new()),
            s3_buckets: Set::default(Vec::new()),
            memory,
        }
    }

    /// every setting, from the environment `var` reads, else from `file`, else from `machine`
    /// and the defaults.
    ///
    /// # Errors
    ///
    /// `usage` naming the variable or the file's key and the values it takes, when one holds
    /// something else.
    pub(crate) fn read(var: Var<'_>, file: File, machine: Machine) -> Result<Settings, Error> {
        let defaults = Settings::defaults(machine);
        let given = Given { var, file: &file };
        let memory = size(&given, "memory", defaults.memory)?;
        Ok(Settings {
            machine,
            jobs: count(&given, "jobs", 1..=256, defaults.jobs)?,
            host_jobs: count(&given, "host_jobs", 1..=256, defaults.host_jobs)?,
            retries: count(
                &given,
                "retries",
                0..=crate::net::MOST_RETRIES,
                defaults.retries,
            )?,
            timeout: seconds(&given, "timeout", 1..=3600, defaults.timeout)?,
            ask_above: size(&given, "ask_above", defaults.ask_above)?,
            most_files: count(&given, "most_files", 1..=1_000_000_000, most_files(memory))?,
            archive_files: count(
                &given,
                "archive_files",
                1..=1_000_000_000_000,
                defaults.archive_files,
            )?,
            archive_size: size(&given, "archive_size", defaults.archive_size)?,
            archive_ratio: count(
                &given,
                "archive_ratio",
                1..=1_000_000_000,
                defaults.archive_ratio,
            )?,
            allowed_origins: origins(&given, "allowed_origins")?,
            s3_buckets: buckets(&given, "s3_buckets")?,
            memory,
            file,
        })
    }

    /// the limits every archive unpacks within: as set, with each of the at most four archives
    /// unpacking at once given a quarter of the memory budget to decompress in, never less than
    /// the default.
    pub(crate) fn archive_limits(&self) -> crate::archive::Limits {
        let defaults = crate::archive::Limits::default();
        crate::archive::Limits {
            members: self.archive_files.value,
            bytes: self.archive_size.value,
            ratio: self.archive_ratio.value,
            memory: (self.memory.value / 4).max(defaults.memory),
        }
    }

    /// how many batches each download may queue for its hash worker: the memory budget shared
    /// among the downloads that can run at once, between 1 and 16 batches of a mebibyte.
    pub(crate) fn hash_batches(&self) -> usize {
        let jobs = u64::try_from(self.jobs.value).unwrap_or(u64::MAX).max(1);
        usize::try_from(((self.memory.value / jobs) >> 20).clamp(1, 16)).unwrap_or(16)
    }
}

/// the most files a dataset may list within `memory`, never fewer than fetchloom's own default.
fn most_files(memory: Set<u64>) -> Set<usize> {
    let sized = usize::try_from(memory.value / crate::source::PER_FILE).unwrap_or(usize::MAX);
    if sized <= crate::source::MOST_FILES {
        return Set::default(crate::source::MOST_FILES);
    }
    Set {
        value: sized,
        // sized from what the machine gives, or from memory a person set
        origin: match memory.origin {
            Origin::Default => Origin::Default,
            _ => Origin::Machine,
        },
    }
}

/// where a run looks for its settings: the environment first, then the settings file.
struct Given<'a> {
    var: Var<'a>,
    file: &'a File,
}

impl Given<'_> {
    /// the text `key` holds, where it came from, and how to name it in a message.
    fn get(&self, key: &str) -> Option<(String, Origin, String)> {
        let variable = variable(key);
        if let Some(value) = (self.var)(variable).filter(|value| !value.is_empty()) {
            return Some((
                value.to_string_lossy().into_owned(),
                Origin::Environment,
                variable.to_owned(),
            ));
        }
        let value = self.file.get(key)?;
        let path = self.file.path.as_deref().unwrap_or(Path::new(FILE));
        Some((
            value.to_owned(),
            Origin::File,
            format!("{key} in the settings file {}", path.display()),
        ))
    }
}

/// the variable that sets `key`.
pub(crate) fn variable(key: &str) -> &'static str {
    KEYS.iter()
        .find(|(known, _)| *known == key)
        .map_or("FETCHLOOM_SETTINGS", |(_, variable)| variable)
}

/// how to undo a bad value: unset the variable, or remove the key from the file.
fn undo(origin: Origin, key: &str) -> String {
    match origin {
        Origin::File => format!("remove {key} from the settings file"),
        _ => format!("unset {}", variable(key)),
    }
}

/// the whole number `key` holds, within `range`, or `default` when it is unset or empty.
/// surrounding whitespace is accepted.
fn count<T: FromStr + PartialOrd + fmt::Display + Copy>(
    given: &Given<'_>,
    key: &str,
    range: RangeInclusive<T>,
    default: Set<T>,
) -> Result<Set<T>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(default);
    };
    let (low, high) = (range.start(), range.end());
    text.trim()
        .parse::<T>()
        .ok()
        .filter(|number| range.contains(number))
        .map(|value| Set { value, origin })
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!("{named} is \"{text}\", which is not a number from {low} to {high}"),
            )
            .help(format!(
                "{} to use the default of {}, or set it to a number from {low} to {high}",
                undo(origin, key),
                default.value
            ))
        })
}

/// the byte size `key` holds, such as `512M` or `4G`, or `default`.
fn size(given: &Given<'_>, key: &str, default: Set<u64>) -> Result<Set<u64>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(default);
    };
    units::bytes(&text)
        .map(|value| Set { value, origin })
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!("{named} is \"{text}\", which is not a size such as 512M or 4G"),
            )
            .help(format!(
                "{} to use the default of {}, or set it to a size such as 512M or 4G",
                undo(origin, key),
                units::size(default.value)
            ))
        })
}

/// the origins `key` lists, separated by commas: each a scheme, a host and a port when it is not
/// the usual one, without a path. none when it is unset or empty.
fn origins(given: &Given<'_>, key: &str) -> Result<Set<Vec<url::Origin>>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut origins = Vec::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let url = url::Url::parse(entry).ok().filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some_and(|host| !host.is_empty())
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        });
        let Some(url) = url else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{named} holds \"{entry}\", which is not an origin such as http://lab.local:8080"
                ),
            )
            .help(format!(
                "{} to allow none, or list origins as a scheme, a host and a port when it is not the usual one, such as http://lab.local:8080, separated by commas",
                undo(origin, key)
            )));
        };
        origins.push(url.origin());
    }
    Ok(Set {
        value: origins,
        origin,
    })
}

/// the buckets `key` lists, separated by commas: an AWS bucket as `s3://name`, or a bare name,
/// and one at another S3 service as `s3:https://host:port/name`. none when it is unset or empty.
fn buckets(given: &Given<'_>, key: &str) -> Result<Set<Vec<Bucket>>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut buckets = Vec::new();
    for entry in text
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let Some(bucket) = Bucket::parse(entry) else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{named} holds \"{}\", which is not a bucket such as s3://lab-data or s3:https://minio.lab:9000/scans",
                    Bucket::shown(entry)
                ),
            )
            .help(format!(
                "{} to sign for none, or list buckets as s3://name, or as s3:https://host:port/name at another S3 service, separated by commas",
                undo(origin, key)
            )));
        };
        if !buckets.contains(&bucket) {
            buckets.push(bucket);
        }
    }
    Ok(Set {
        value: buckets,
        origin,
    })
}

/// the whole seconds `key` holds, within `range`, or `default`.
fn seconds(
    given: &Given<'_>,
    key: &str,
    range: RangeInclusive<u64>,
    default: Set<Duration>,
) -> Result<Set<Duration>, Error> {
    let as_seconds = Set {
        value: default.value.as_secs(),
        origin: default.origin,
    };
    count(given, key, range, as_seconds).map(|set| Set {
        value: Duration::from_secs(set.value),
        origin: set.origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(memory: Option<u64>) -> Machine {
        Machine {
            processors: 4,
            memory,
            open_files: Some(1024),
        }
    }

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

    fn read(pairs: &[(&str, &str)], memory: Option<u64>) -> Result<Settings, Error> {
        Settings::read(&env(pairs), File::default(), machine(memory))
    }

    fn file(text: &str) -> Result<File, Error> {
        let path = PathBuf::from("/home/ada/.config/fetchloom/settings.toml");
        parse(&path, text).map(|values| File {
            path: Some(path),
            values,
            found: true,
        })
    }

    #[test]
    fn unset_variables_keep_the_defaults() {
        let settings = read(&[], None).unwrap();
        assert_eq!(settings.jobs, Set::default(JOBS));
        assert_eq!(settings.host_jobs, Set::default(HOST_JOBS));
        assert_eq!(settings.timeout, Set::default(TIMEOUT));
        assert_eq!(settings.ask_above, Set::default(ASK_ABOVE));
        assert_eq!(settings.memory, Set::default(LEAST_MEMORY));
        let empty = read(&[("FETCHLOOM_JOBS", ""), ("FETCHLOOM_MEMORY", "")], None).unwrap();
        assert_eq!(empty.jobs, Set::default(JOBS));
    }

    #[test]
    fn the_memory_budget_is_a_quarter_of_what_the_machine_gives_within_bounds() {
        for (room, budget) in [
            (Some(100 << 20), 50 << 20),
            (Some(600 << 20), LEAST_MEMORY),
            (Some(2 << 30), 512 << 20),
            (Some(64 << 30), MOST_MEMORY),
        ] {
            let settings = read(&[], room).unwrap();
            assert_eq!(settings.memory.value, budget, "{room:?}");
            assert_eq!(settings.memory.origin, Origin::Machine);
        }
        let set = read(&[("FETCHLOOM_MEMORY", "64G")], Some(1 << 30)).unwrap();
        assert_eq!(
            set.memory,
            Set {
                value: 64 << 30,
                origin: Origin::Environment
            }
        );
    }

    #[test]
    fn variables_override_and_name_their_origin() {
        let settings = read(
            &[
                ("FETCHLOOM_JOBS", " 64 "),
                ("FETCHLOOM_HOST_JOBS", "16"),
                ("FETCHLOOM_TIMEOUT", "300"),
                ("FETCHLOOM_ASK_ABOVE", "50G"),
            ],
            None,
        )
        .unwrap();
        let set = |value| Set {
            value,
            origin: Origin::Environment,
        };
        assert_eq!(settings.jobs, set(64));
        assert_eq!(settings.host_jobs, set(16));
        assert_eq!(settings.ask_above.value, 50 << 30);
        assert_eq!(
            settings.timeout,
            Set {
                value: Duration::from_secs(300),
                origin: Origin::Environment
            }
        );
    }

    #[test]
    fn the_settings_file_sets_what_the_environment_leaves_and_names_its_errors() {
        let file = file("jobs = 32\nmemory = \"8G\"\ntimeout = 120\ncache = \"cache\"\n").unwrap();
        assert_eq!(
            file.cache(),
            Some(PathBuf::from("/home/ada/.config/fetchloom/cache"))
        );
        let settings =
            Settings::read(&env(&[("FETCHLOOM_JOBS", "4")]), file, machine(None)).unwrap();
        assert_eq!(
            settings.jobs,
            Set {
                value: 4,
                origin: Origin::Environment
            }
        );
        assert_eq!(
            settings.memory,
            Set {
                value: 8 << 30,
                origin: Origin::File
            }
        );
        assert_eq!(settings.timeout.value, Duration::from_secs(120));
        let error = Settings::read(&env(&[]), self::file("jobs = 0\n").unwrap(), machine(None))
            .unwrap_err();
        assert_eq!(
            error.message,
            "jobs in the settings file /home/ada/.config/fetchloom/settings.toml is \"0\", which is not a number from 1 to 256"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "remove jobs from the settings file to use the default of 8, or set it to a number from 1 to 256"
            )
        );
        let unknown = self::file("job = 3\n").unwrap_err();
        assert_eq!(unknown.kind, Kind::Usage);
        assert_eq!(
            unknown.message,
            "the settings file /home/ada/.config/fetchloom/settings.toml has \"job\", which is not a setting"
        );
        let wrong = self::file("jobs = true\n").unwrap_err();
        assert_eq!(
            wrong.message,
            "jobs in the settings file /home/ada/.config/fetchloom/settings.toml is a boolean, not a whole number or text in quotes"
        );
        assert!(self::file("jobs = [").is_err());
    }

    #[test]
    fn the_most_files_a_dataset_lists_follow_the_memory_budget() {
        let gib = 1 << 30;
        let most = |memory: Option<u64>, pairs: &[(&str, &str)]| {
            Settings::read(&env(pairs), File::default(), machine(memory))
                .unwrap()
                .most_files
        };
        // 32 GiB available: a 4 GiB budget, at 2 KiB a file
        assert_eq!(
            most(Some(32 * gib), &[]),
            Set {
                value: 2_097_152,
                origin: Origin::Machine
            }
        );
        // a small pod never lists fewer than fetchloom always allowed
        assert_eq!(
            most(Some(100 << 20), &[]),
            Set::default(crate::source::MOST_FILES)
        );
        assert_eq!(most(None, &[]).value, 131_072);
        assert_eq!(most(None, &[("FETCHLOOM_MEMORY", "32G")]).value, 16_777_216);
        assert_eq!(
            most(Some(32 * gib), &[("FETCHLOOM_MOST_FILES", "5000000")]),
            Set {
                value: 5_000_000,
                origin: Origin::Environment
            }
        );
        let error = Settings::read(
            &env(&[("FETCHLOOM_MOST_FILES", "0")]),
            File::default(),
            machine(None),
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "FETCHLOOM_MOST_FILES is \"0\", which is not a number from 1 to 1000000000"
        );
    }

    #[test]
    fn archives_unpack_within_their_settings_and_a_share_of_the_memory() {
        let defaults = crate::archive::Limits::default();
        let limits = |pairs: &[(&str, &str)]| {
            Settings::read(&env(pairs), File::default(), machine(None))
                .unwrap()
                .archive_limits()
        };
        assert_eq!(limits(&[]), defaults);
        let raised = limits(&[
            ("FETCHLOOM_ARCHIVE_FILES", "50000000"),
            ("FETCHLOOM_ARCHIVE_SIZE", "64T"),
            ("FETCHLOOM_ARCHIVE_RATIO", "5000"),
            ("FETCHLOOM_MEMORY", "16G"),
        ]);
        assert_eq!(
            raised,
            crate::archive::Limits {
                members: 50_000_000,
                bytes: 64 << 40,
                ratio: 5_000,
                memory: 4 << 30,
            }
        );
        // a small budget never takes decoders below what fetchloom always allowed
        assert_eq!(
            limits(&[("FETCHLOOM_MEMORY", "64M")]).memory,
            defaults.memory
        );
    }

    #[test]
    fn a_bad_value_is_a_usage_error_naming_the_variable_and_what_it_takes() {
        for (name, value, said, help) in [
            (
                "FETCHLOOM_HOST_JOBS",
                "0",
                "FETCHLOOM_HOST_JOBS is \"0\", which is not a number from 1 to 256",
                "unset FETCHLOOM_HOST_JOBS to use the default of 4, or set it to a number from 1 to 256",
            ),
            (
                "FETCHLOOM_TIMEOUT",
                "soon",
                "FETCHLOOM_TIMEOUT is \"soon\", which is not a number from 1 to 3600",
                "unset FETCHLOOM_TIMEOUT to use the default of 30, or set it to a number from 1 to 3600",
            ),
            (
                "FETCHLOOM_MEMORY",
                "4gb",
                "FETCHLOOM_MEMORY is \"4gb\", which is not a size such as 512M or 4G",
                "unset FETCHLOOM_MEMORY to use the default of 256.0 MiB, or set it to a size such as 512M or 4G",
            ),
        ] {
            let error = read(&[(name, value)], None).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{name}");
            assert_eq!(error.message, said, "{name}");
            assert_eq!(error.help.as_deref(), Some(help), "{name}");
        }
    }

    #[test]
    fn each_download_queues_its_share_of_the_memory_budget() {
        for (jobs, memory, batches) in [
            ("8", "256M", 16),
            ("8", "64M", 8),
            ("64", "256M", 4),
            ("256", "256M", 1),
            ("1", "4G", 16),
        ] {
            let settings = read(
                &[("FETCHLOOM_JOBS", jobs), ("FETCHLOOM_MEMORY", memory)],
                None,
            )
            .unwrap();
            assert_eq!(settings.hash_batches(), batches, "{jobs} jobs, {memory}");
        }
    }

    #[test]
    fn retries_come_from_the_setting_within_its_range() {
        let retries = |value: &str| read(&[("FETCHLOOM_RETRIES", value)], None);
        assert_eq!(read(&[], None).unwrap().retries.value, crate::net::RETRIES);
        assert_eq!(retries("").unwrap().retries.value, crate::net::RETRIES);
        assert_eq!(retries("0").unwrap().retries.value, 0);
        assert_eq!(retries(" 12 ").unwrap().retries.value, 12);
        assert_eq!(
            retries("20").unwrap().retries.value,
            crate::net::MOST_RETRIES
        );
        for bad in ["21", "-1", "many", "1.5"] {
            let error = retries(bad).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{bad}");
            assert_eq!(
                error.message,
                format!("FETCHLOOM_RETRIES is \"{bad}\", which is not a number from 0 to 20")
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_RETRIES to use the default of 4, or set it to a number from 0 to 20"
                )
            );
        }
    }

    #[test]
    fn allowed_origins_are_exact_origins_from_the_environment_or_the_file() {
        let shown = |settings: &Settings| -> Vec<String> {
            settings
                .allowed_origins
                .value
                .iter()
                .map(url::Origin::ascii_serialization)
                .collect()
        };
        assert_eq!(
            read(&[], None).unwrap().allowed_origins,
            Set::default(vec![])
        );
        let set = read(
            &[(
                "FETCHLOOM_ALLOWED_ORIGINS",
                " http://LAB.local:8080, https://10.0.0.5:443/ ,",
            )],
            None,
        )
        .unwrap();
        assert_eq!(shown(&set), ["http://lab.local:8080", "https://10.0.0.5"]);
        assert_eq!(set.allowed_origins.origin, Origin::Environment);
        let file = file("allowed_origins = [\"http://lab.local:8080\", \"http://[::1]:9000\"]\n");
        let set = Settings::read(&env(&[]), file.unwrap(), machine(None)).unwrap();
        assert_eq!(shown(&set), ["http://lab.local:8080", "http://[::1]:9000"]);
        assert_eq!(set.allowed_origins.origin, Origin::File);
        for entry in [
            "lab.local:8080",
            "http://lab.local:8080/data",
            "ftp://lab.local",
            "http://ada:secret@lab.local",
            "http://lab.local/?all=1",
        ] {
            let error = read(&[("FETCHLOOM_ALLOWED_ORIGINS", entry)], None).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{entry}");
            assert_eq!(
                error.message,
                format!(
                    "FETCHLOOM_ALLOWED_ORIGINS holds \"{entry}\", which is not an origin such as http://lab.local:8080"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_ALLOWED_ORIGINS to allow none, or list origins as a scheme, a host and a port when it is not the usual one, such as http://lab.local:8080, separated by commas"
                )
            );
        }
        let wrong = self::file("allowed_origins = [8080]\n").unwrap_err();
        assert_eq!(
            wrong.message,
            "allowed_origins in the settings file /home/ada/.config/fetchloom/settings.toml holds a integer, not text in quotes"
        );
        let elsewhere = self::file("jobs = [32]\n").unwrap_err();
        assert!(
            elsewhere.message.contains("is a array"),
            "{}",
            elsewhere.message
        );
    }

    #[test]
    fn s3_buckets_name_aws_buckets_or_buckets_at_another_service() {
        let shown = |settings: &Settings| -> Vec<String> {
            settings
                .s3_buckets
                .value
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(read(&[], None).unwrap().s3_buckets, Set::default(vec![]));
        let set = read(
            &[(
                "FETCHLOOM_S3_BUCKETS",
                " lab-raw, s3://lab.data/ ,s3:https://MINIO.lab:9000/scans/,s3:http://10.0.0.5:80/old-scans,",
            )],
            None,
        )
        .unwrap();
        assert_eq!(
            shown(&set),
            [
                "s3://lab-raw",
                "s3://lab.data",
                "s3:https://minio.lab:9000/scans",
                "s3:http://10.0.0.5/old-scans"
            ]
        );
        assert_eq!(set.s3_buckets.origin, Origin::Environment);
        let file = file("s3_buckets = [\"s3://lab-raw\"]\n").unwrap();
        let set = Settings::read(&env(&[]), file, machine(None)).unwrap();
        assert_eq!(shown(&set), ["s3://lab-raw"]);
        assert_eq!(set.s3_buckets.origin, Origin::File);
        for (entry, said) in [
            ("s3://Lab-Raw", "s3://Lab-Raw"),
            ("gs://lab-raw", "gs://lab-raw"),
            ("s3://lab-raw/prefix/", "s3://lab-raw/prefix/"),
            (
                "s3:https://minio.lab/scans/raw",
                "s3:https://minio.lab/scans/raw",
            ),
            (
                "s3:https://minio.lab/scans?x=1",
                "s3:https://minio.lab/scans?x=1",
            ),
            ("s3:ftp://minio.lab/scans", "s3:ftp://minio.lab/scans"),
            (
                "s3:https://ada:hunter2@minio.lab/scans",
                "s3:https://***@minio.lab/scans",
            ),
        ] {
            let error = read(&[("FETCHLOOM_S3_BUCKETS", entry)], None).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{entry}");
            assert_eq!(
                error.message,
                format!(
                    "FETCHLOOM_S3_BUCKETS holds \"{said}\", which is not a bucket such as s3://lab-data or s3:https://minio.lab:9000/scans"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_S3_BUCKETS to sign for none, or list buckets as s3://name, or as s3:https://host:port/name at another S3 service, separated by commas"
                )
            );
        }
    }
}
