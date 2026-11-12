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

mod resources;

pub(crate) use resources::Level;

use crate::error::{Error, Kind};
use crate::machine::Machine;
use crate::mirror::Location;
use crate::source::{Bucket, DriveId, GcsBucket};
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
const KEYS: [(&str, &str); 20] = [
    ("cache", "FETCHLOOM_CACHE"),
    ("resources", "FETCHLOOM_RESOURCES"),
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
    ("gcs_buckets", "FETCHLOOM_GCS_BUCKETS"),
    ("drive_folders", "FETCHLOOM_DRIVE_FOLDERS"),
    ("mirrors", "FETCHLOOM_MIRRORS"),
    ("mirrors_only", "FETCHLOOM_MIRRORS_ONLY"),
    ("push_to", "FETCHLOOM_PUSH_TO"),
    ("build_floor", "FETCHLOOM_BUILD_FLOOR"),
];
/// the keys that hold a list: an array of text in the settings file, text separated by commas in
/// a variable.
const LISTS: [&str; 5] = [
    "allowed_origins",
    "s3_buckets",
    "gcs_buckets",
    "drive_folders",
    "mirrors",
];
/// the keys that hold true or false.
const BOOLEANS: [&str; 1] = ["mirrors_only"];
/// the keys an empty variable clears rather than leaving to the settings file: who may be
/// reached, which buckets keys sign for, where requests go and where push writes, so blanking one
/// for a run cannot leave the file's choice in force.
const CLEARED: [&str; 7] = [
    "allowed_origins",
    "s3_buckets",
    "gcs_buckets",
    "drive_folders",
    "mirrors",
    "mirrors_only",
    "push_to",
];
/// the variables fetchloom reads besides those of the settings file's keys.
const OTHERS: [&str; 13] = [
    "FETCHLOOM_SETTINGS",
    "FETCHLOOM_NO_CONFIG",
    "FETCHLOOM_OFFLINE",
    "FETCHLOOM_RATE",
    "FETCHLOOM_THREADS",
    "FETCHLOOM_SPLIT",
    "FETCHLOOM_LOG",
    "FETCHLOOM_TRACE",
    "FETCHLOOM_BUILDING",
    "FETCHLOOM_COLORS",
    "FETCHLOOM_NO_HINTS",
    "FETCHLOOM_ACCESSIBLE",
    "FETCHLOOM_KEYRING",
];

/// where a setting's value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// fetchloom's own default.
    Default,
    /// sized from this machine: its processors or memory.
    Machine,
    /// a `FETCHLOOM_*` environment variable.
    Environment,
    /// the settings file.
    File,
    /// an option on the command line, such as `--mirror`, which `fetchloom settings` shows
    /// before what the settings give.
    Flag,
    /// derived from the `resources` level, for a knob no name set.
    Resources(Level),
}

impl Origin {
    /// how `--json` names it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::Machine => "machine",
            Origin::Environment => "environment",
            Origin::File => "file",
            Origin::Flag => "flag",
            Origin::Resources(_) => "resources",
        }
    }

    /// whether a person set the value by its name, in a variable or the settings file.
    fn is_named(self) -> bool {
        matches!(self, Origin::Environment | Origin::File)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Origin::Default => "default",
            Origin::Machine => "this machine",
            Origin::Environment => "environment",
            Origin::File => "settings file",
            Origin::Flag => "command line",
            Origin::Resources(level) => return write!(f, "resources = {level}"),
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
    /// its values by key, when it was there.
    values: Vec<(String, Value)>,
    pub(crate) found: bool,
    /// what left the file unread for this run, `--no-config` or `FETCHLOOM_NO_CONFIG`.
    pub(crate) bypassed: Option<&'static str>,
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
        let (path, named) = located(var);
        let Some(path) = path else {
            return Ok(File::default());
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !named => {
                return Ok(File {
                    path: Some(path),
                    ..File::default()
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
                let help = if named {
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
            bypassed: None,
        })
    }

    /// the settings file left unread, as `by` asks: where it is, or would be, and whether it is
    /// there, so a run goes on the variables and defaults past a file that would stop it.
    pub(crate) fn bypassed(var: Var<'_>, by: &'static str) -> File {
        let (path, _) = located(var);
        File {
            found: path.as_deref().is_some_and(Path::is_file),
            path,
            values: Vec::new(),
            bypassed: Some(by),
        }
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.values
            .iter()
            .find(|(set, _)| set == key)
            .map(|(_, value)| value)
    }

    /// the cache folder the file names, if any. a relative folder is taken from the file's own
    /// folder, so the file means the same wherever fetchloom starts.
    pub(crate) fn cache(&self) -> Option<PathBuf> {
        let Value::Text(folder) = self.get("cache")? else {
            return None;
        };
        let folder = PathBuf::from(folder);
        Some(match self.path.as_deref().and_then(Path::parent) {
            Some(parent) if folder.is_relative() => parent.join(folder),
            _ => folder,
        })
    }
}

/// where the settings file is: the one `FETCHLOOM_SETTINGS` names, which must then be there, else
/// `settings.toml` in fetchloom's folder, or none without a folder for it.
fn located(var: Var<'_>) -> (Option<PathBuf>, bool) {
    let named = var("FETCHLOOM_SETTINGS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let is_named = named.is_some();
    let path = named.or_else(|| crate::auth::store::folder(var).map(|folder| folder.join(FILE)));
    (path, is_named)
}

/// a value in the settings file: text, or for a key that holds a list, the list's items.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Text(String),
    List(Vec<String>),
}

/// the settings file's values, refusing a key it does not take, so a mistyped key is told rather
/// than quietly ignored. a message names where the file stops being TOML but never quotes it,
/// since a value may hold a login.
fn parse(path: &Path, text: &str) -> Result<Vec<(String, Value)>, Error> {
    let invalid = |message: String| {
        Error::new(Kind::Usage, message).help(format!(
            "the settings file takes {}, each a number or text",
            KEYS.map(|(key, _)| key).join(", ")
        ))
    };
    let table: toml::Table = toml::from_str(text).map_err(|err| {
        let at = err.span().map_or_else(String::new, |span| {
            let (line, column) = line_and_column(text, span.start);
            format!(" at line {line}, column {column}")
        });
        invalid(format!(
            "the settings file {} is not valid TOML{at}: {}",
            path.display(),
            err.message()
        ))
    })?;
    table
        .into_iter()
        .map(|(key, value)| {
            if !KEYS.iter().any(|(known, _)| *known == key) {
                let error = invalid(format!(
                    "the settings file {} has \"{key}\", which is not a setting",
                    path.display()
                ));
                return Err(match crate::name::closest(&key, KEYS.map(|(key, _)| key)) {
                    Some(meant) => error.help(format!("did you mean `{meant}`?")),
                    None => error,
                });
            }
            let value = match value {
                toml::Value::String(text) => Value::Text(text),
                toml::Value::Integer(number) => Value::Text(number.to_string()),
                toml::Value::Boolean(yes) if BOOLEANS.contains(&key.as_str()) => {
                    Value::Text(yes.to_string())
                }
                toml::Value::Array(items) if LISTS.contains(&key.as_str()) => Value::List(
                    items
                        .into_iter()
                        .map(|item| match item {
                            toml::Value::String(text) => Ok(text),
                            other => Err(invalid(format!(
                                "{key} in the settings file {} holds a {}, not text in quotes",
                                path.display(),
                                other.type_str()
                            ))),
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                other => {
                    return Err(invalid(format!(
                        "{key} in the settings file {} is a {}, not a whole number or text in quotes",
                        path.display(),
                        other.type_str()
                    )));
                }
            };
            Ok((key, value))
        })
        .collect()
}

/// every setting a run uses.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub(crate) machine: Machine,
    pub(crate) file: File,
    /// how much of the machine a run spends for speed, which sets each knob no name sets.
    pub(crate) resources: Set<Level>,
    pub(crate) jobs: Set<usize>,
    pub(crate) host_jobs: Set<usize>,
    pub(crate) retries: Set<u32>,
    pub(crate) timeout: Set<Duration>,
    pub(crate) ask_above: Set<u64>,
    /// the memory a run's buffers aim to stay within: download queues share it.
    pub(crate) memory: Set<u64>,
    /// the memory budget as set by name, else as the default level gives it, which what no level
    /// changes is sized from: `most_files` and the archive limits.
    steady_memory: u64,
    /// the most workers one local group runs, as the level sets it when `FETCHLOOM_THREADS`
    /// does not, which then caps every group as that variable would.
    pub(crate) threads: Option<Set<usize>>,
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
    /// the cloud storage buckets a request may carry a token of this machine's google
    /// credentials for, after cloud storage refused it without one.
    pub(crate) gcs_buckets: Set<Vec<GcsBucket>>,
    /// the google drive folders and files a request to drive's api may carry a token of this
    /// machine's google credentials for, after drive refused them without one.
    pub(crate) drive_folders: Set<Vec<DriveId>>,
    /// the folders and web servers asked for pinned files before their sources, in order.
    pub(crate) mirrors: Set<Vec<Location>>,
    /// whether requests may go to the mirrors alone, never to a source.
    pub(crate) mirrors_only: Set<bool>,
    /// the folder mirror `fetchloom push` writes to when `--to` names none.
    pub(crate) push_to: Set<Option<PathBuf>>,
    /// the least free space a recipe's step may leave on the disk holding the cache, or none for
    /// the larger of 1 GiB and 1% of that disk.
    pub(crate) build_floor: Set<Option<u64>>,
}

impl Settings {
    /// every setting at fetchloom's defaults, with what is sized from `machine` sized from it.
    pub(crate) fn defaults(machine: Machine) -> Settings {
        let memory = budget(machine);
        Settings {
            machine,
            file: File::default(),
            resources: Set::default(Level::Default),
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
            gcs_buckets: Set::default(Vec::new()),
            drive_folders: Set::default(Vec::new()),
            mirrors: Set::default(Vec::new()),
            mirrors_only: Set::default(false),
            push_to: Set::default(None),
            build_floor: Set::default(None),
            memory,
            steady_memory: memory.value,
            threads: None,
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
        let resources = level(&given)?;
        let limits = resources.value.limits(machine);
        let memory = size(
            &given,
            "memory",
            derived(limits.memory, resources.value, defaults.memory),
        )?;
        let steady = if memory.origin.is_named() {
            memory
        } else {
            defaults.memory
        };
        // a level caps local workers only where FETCHLOOM_THREADS does not
        let threads = limits
            .threads
            .filter(|_| var("FETCHLOOM_THREADS").is_none_or(|value| value.is_empty()))
            .map(|value| Set {
                value,
                origin: Origin::Resources(resources.value),
            });
        Ok(Settings {
            machine,
            resources,
            jobs: count(
                &given,
                "jobs",
                1..=256,
                derived(limits.jobs, resources.value, defaults.jobs),
            )?,
            host_jobs: count(
                &given,
                "host_jobs",
                1..=256,
                derived(limits.host_jobs, resources.value, defaults.host_jobs),
            )?,
            retries: count(
                &given,
                "retries",
                0..=crate::net::MOST_RETRIES,
                defaults.retries,
            )?,
            timeout: seconds(&given, "timeout", 1..=3600, defaults.timeout)?,
            ask_above: size(&given, "ask_above", defaults.ask_above)?,
            most_files: count(&given, "most_files", 1..=1_000_000_000, most_files(steady))?,
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
            gcs_buckets: gcs_buckets(&given, "gcs_buckets")?,
            drive_folders: drive_folders(&given, "drive_folders")?,
            mirrors: mirrors(&given, "mirrors")?,
            mirrors_only: boolean(&given, "mirrors_only", false)?,
            push_to: push_folder(&given, "push_to")?,
            build_floor: floor(&given, "build_floor")?,
            memory,
            steady_memory: steady.value,
            threads,
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
            memory: (self.steady_memory / 4).max(defaults.memory),
        }
    }

    /// the CPU budget for checksum lanes; lean must not retain an extra lane's byte batches.
    pub(crate) fn hash_workers(&self) -> usize {
        if matches!(
            self.threads,
            Some(Set {
                origin: Origin::Resources(Level::Lean),
                ..
            })
        ) {
            1
        } else {
            self.machine.processors
        }
    }

    /// how many batches each download may queue for its hash worker: the memory budget shared
    /// among the downloads that can run at once, between 1 and 16 batches of a mebibyte.
    pub(crate) fn hash_batches(&self) -> usize {
        let jobs = u64::try_from(self.jobs.value).unwrap_or(u64::MAX).max(1);
        usize::try_from(((self.memory.value / jobs) >> 20).clamp(1, 16)).unwrap_or(16)
    }
}

/// the memory budget `machine` gives a run without a level: a quarter of what it has, but never
/// more than half of it, however small, nor more than 4 GiB.
fn budget(machine: Machine) -> Set<u64> {
    match machine.memory {
        Some(room) => Set {
            value: (room / 4).clamp(LEAST_MEMORY.min(room / 2), MOST_MEMORY),
            origin: Origin::Machine,
        },
        None => Set::default(LEAST_MEMORY),
    }
}

/// `fallback`, a knob's value without a level, or what `level` gives it as `value`.
fn derived<T>(value: Option<T>, level: Level, fallback: Set<T>) -> Set<T> {
    value.map_or(fallback, |value| Set {
        value,
        origin: Origin::Resources(level),
    })
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
    /// the text `key` holds, where it came from, and how to name it in a message. a list in the
    /// settings file reads as its items separated by commas.
    fn get(&self, key: &str) -> Option<(String, Origin, String)> {
        self.list(key)
            .map(|(items, origin, named)| (items.join(","), origin, named))
    }

    /// the items `key` lists, where they came from, and how to name it in a message: text
    /// separated by commas, or the items of a list in the settings file as they are.
    fn list(&self, key: &str) -> Option<(Vec<String>, Origin, String)> {
        if let Some((value, named)) = self.environment(key) {
            return Some((split(&value.to_string_lossy()), Origin::Environment, named));
        }
        let items = match self.file.get(key)? {
            Value::Text(text) => split(text),
            Value::List(items) => items.clone(),
        };
        Some((
            items,
            Origin::File,
            format!("{key} in the settings file {}", self.file_path().display()),
        ))
    }

    /// the text `key` holds as it is, never split at commas, since a folder's name may hold one,
    /// with where it came from and how to name it in a message.
    fn text(&self, key: &str) -> Option<(OsString, Origin, String)> {
        if let Some((value, named)) = self.environment(key) {
            return Some((value, Origin::Environment, named));
        }
        let Value::Text(text) = self.file.get(key)? else {
            return None;
        };
        Some((
            OsString::from(text),
            Origin::File,
            format!("{key} in the settings file {}", self.file_path().display()),
        ))
    }

    /// what the variable for `key` holds, and its name: a nonempty value, or an empty one for a
    /// key an empty variable clears.
    fn environment(&self, key: &str) -> Option<(OsString, String)> {
        let variable = variable(key);
        (self.var)(variable)
            .filter(|value| !value.is_empty() || CLEARED.contains(&key))
            .map(|value| (value, variable.to_owned()))
    }

    fn file_path(&self) -> &Path {
        self.file.path.as_deref().unwrap_or(Path::new(FILE))
    }
}

/// the line and column, each from one, of byte `offset` in `text`.
fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let mut end = offset.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    (line, column)
}

/// text separated by commas, as a list's variable holds it, each item trimmed and empty ones
/// left out.
fn split(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// the variable that sets `key`.
pub(crate) fn variable(key: &str) -> &'static str {
    KEYS.iter()
        .find(|(known, _)| *known == key)
        .map_or("FETCHLOOM_SETTINGS", |(_, variable)| variable)
}

/// every variable fetchloom reads.
fn known() -> impl Iterator<Item = &'static str> {
    KEYS.iter().map(|(_, variable)| *variable).chain(OTHERS)
}

/// the names among `names` that fetchloom does not read but that are one typo, a character
/// added, dropped, changed or swapped with its neighbour, from a variable it does, each with that
/// variable. any other name is left alone, since a wrapper or another version may read it.
pub(crate) fn misnamed(names: impl IntoIterator<Item = OsString>) -> Vec<(String, &'static str)> {
    names
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter_map(|name| {
            // windows finds a variable by its name in any case
            let compared = if cfg!(windows) {
                name.to_ascii_uppercase()
            } else {
                name.clone()
            };
            if known().any(|known| known == compared) {
                return None;
            }
            let meant = known().find(|known| crate::name::one_typo(&compared, known))?;
            Some((name, meant))
        })
        .collect()
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

/// the byte size `key` holds, such as `20G`, or none when it is unset or empty, for the default the
/// disk holding the cache sets.
fn floor(given: &Given<'_>, key: &str) -> Result<Set<Option<u64>>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(Set::default(None));
    };
    units::bytes(&text)
        .map(|value| Set {
            value: Some(value),
            origin,
        })
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!("{named} is \"{text}\", which is not a size such as 512M or 4G"),
            )
            .help(format!(
                "{} to use the default, the larger of 1.0 GiB and 1% of the disk holding the cache, or set it to a size such as 512M or 4G",
                undo(origin, key)
            ))
        })
}

/// the origins `key` lists, separated by commas: each a scheme, a host and a port when it is not
/// the usual one, without a path. none when it is unset or empty.
fn origins(given: &Given<'_>, key: &str) -> Result<Set<Vec<url::Origin>>, Error> {
    let Some((entries, origin, named)) = given.list(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut origins = Vec::new();
    for entry in entries
        .iter()
        .map(|entry| entry.trim())
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
    let Some((entries, origin, named)) = given.list(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut buckets = Vec::new();
    for entry in entries
        .iter()
        .map(|entry| entry.trim())
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

/// the cloud storage buckets `key` lists, separated by commas: `gs://name`, or a bare name. none
/// when it is unset or empty.
fn gcs_buckets(given: &Given<'_>, key: &str) -> Result<Set<Vec<GcsBucket>>, Error> {
    let Some((entries, origin, named)) = given.list(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut buckets = Vec::new();
    for entry in entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
    {
        let Some(bucket) = GcsBucket::parse(entry) else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{named} holds \"{}\", which is not a bucket such as gs://lab-data",
                    crate::net::mask_query(entry)
                ),
            )
            .help(format!(
                "{} to send google credentials to none, or list buckets as gs://name, separated by commas",
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

/// the google drive folders and files `key` lists, separated by commas: ids, or the links drive
/// gives when they are shared. none when it is unset or empty.
fn drive_folders(given: &Given<'_>, key: &str) -> Result<Set<Vec<DriveId>>, Error> {
    let Some((entries, origin, named)) = given.list(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let mut ids = Vec::new();
    for entry in entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
    {
        let Some(id) = DriveId::parse(entry) else {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "{named} holds \"{}\", which is not a google drive folder or file: its id or the link drive gives when you share it",
                    crate::net::mask_query(entry)
                ),
            )
            .help(format!(
                "{} to send google credentials to drive for nothing, or list ids or sharing links, separated by commas",
                undo(origin, key)
            )));
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(Set { value: ids, origin })
}

/// the mirrors `key` lists, in order: folders, and http or https addresses. a relative folder in
/// the settings file is taken from the file's folder, as `cache` is, and one in a variable from
/// the working folder. none when it is unset or empty.
fn mirrors(given: &Given<'_>, key: &str) -> Result<Set<Vec<Location>>, Error> {
    let Some((entries, origin, named)) = given.list(key) else {
        return Ok(Set::default(Vec::new()));
    };
    let base = match origin {
        Origin::File => given.file_path().parent(),
        _ => None,
    };
    let mut mirrors = Vec::new();
    for entry in entries
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
    {
        let location = Location::parse(entry, base).map_err(|refusal| {
            Error::new(
                Kind::Usage,
                format!(
                    "{named} holds \"{}\", which is no mirror: {}",
                    crate::net::mask_query(entry),
                    refusal.reason
                ),
            )
            .help(format!(
                "{}, or {} to ask no mirror",
                refusal.instead,
                undo(origin, key)
            ))
        })?;
        if !mirrors.contains(&location) {
            mirrors.push(location);
        }
    }
    Ok(Set {
        value: mirrors,
        origin,
    })
}

/// the folder `key` names for `fetchloom push`, which writes folders only: a web address is
/// refused, as a mirror's is read. a relative folder in the settings file is taken from the
/// file's folder, and one in a variable from the working folder. none when it is unset or empty.
fn push_folder(given: &Given<'_>, key: &str) -> Result<Set<Option<PathBuf>>, Error> {
    let Some((value, origin, named)) = given.text(key) else {
        return Ok(Set::default(None));
    };
    // a web address is text, so a name that is not is a folder
    let Some(text) = value.to_str() else {
        return Ok(Set {
            value: Some(PathBuf::from(value)),
            origin,
        });
    };
    if text.trim().is_empty() {
        return Ok(Set {
            value: None,
            origin,
        });
    }
    let base = match origin {
        Origin::File => given.file_path().parent(),
        _ => None,
    };
    let shown = crate::net::mask_query(text.trim());
    match Location::parse(text, base) {
        Ok(Location::Folder(folder)) => Ok(Set {
            value: Some(folder),
            origin,
        }),
        Ok(Location::Web(_)) => Err(Error::new(
            Kind::Usage,
            format!("{named} is \"{shown}\", which is a web address, and push writes to folders"),
        )
        .help(format!(
            "set it to a folder, such as /mnt/lab/fetchloom, and copy that folder to a server or a bucket with a tool such as rclone; or {}",
            undo(origin, key)
        ))),
        Err(refusal) => Err(Error::new(
            Kind::Usage,
            format!("{named} is \"{shown}\", which is no folder: {}", refusal.reason),
        )
        .help(format!("{}, or {}", refusal.instead, undo(origin, key)))),
    }
}

/// the `resources` level, lean, default or max, or the default level when it is unset or empty.
fn level(given: &Given<'_>) -> Result<Set<Level>, Error> {
    let Some((text, origin, named)) = given.get("resources") else {
        return Ok(Set::default(Level::Default));
    };
    let words = || {
        let [lean, default, max] = Level::ALL.map(Level::name);
        format!("{lean}, {default} or {max}")
    };
    Level::parse(&text)
        .map(|value| Set { value, origin })
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!("{named} is \"{text}\", which is not {}", words()),
            )
            .help(format!(
                "{} to use the default level, or set it to {}",
                undo(origin, "resources"),
                words()
            ))
        })
}

/// true or false, as `key` holds it: `true` or `1`, `false` or `0`, or `default` when it is unset
/// or empty.
fn boolean(given: &Given<'_>, key: &str, default: bool) -> Result<Set<bool>, Error> {
    let Some((text, origin, named)) = given.get(key) else {
        return Ok(Set::default(default));
    };
    let value = match text.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "true" | "1" => true,
        "false" | "0" => false,
        _ => {
            return Err(Error::new(
                Kind::Usage,
                format!("{named} is \"{text}\", which is not true or false"),
            )
            .help(format!(
                "{} to use the default of {default}, or set it to true or false",
                undo(origin, key)
            )));
        }
    };
    Ok(Set { value, origin })
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
            bypassed: None,
        })
    }

    /// `build_floor` is a size from a variable or the file, and unset means the default the disk
    /// holding the cache sets.
    #[test]
    fn the_build_floor_is_a_size_or_left_to_the_cache_disk() {
        assert_eq!(read(&[], None).unwrap().build_floor, Set::default(None));
        assert_eq!(
            read(&[("FETCHLOOM_BUILD_FLOOR", "20G")], None)
                .unwrap()
                .build_floor,
            Set {
                value: Some(20 << 30),
                origin: Origin::Environment
            }
        );
        let set = Settings::read(
            &env(&[]),
            file("build_floor = \"500M\"\n").unwrap(),
            machine(None),
        )
        .unwrap();
        assert_eq!(
            set.build_floor,
            Set {
                value: Some(500 << 20),
                origin: Origin::File
            }
        );
        let error = read(&[("FETCHLOOM_BUILD_FLOOR", "lots")], None).unwrap_err();
        assert_eq!(
            error.message,
            "FETCHLOOM_BUILD_FLOOR is \"lots\", which is not a size such as 512M or 4G"
        );
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

    /// a settings file that is not TOML names the line and column where reading stopped, and
    /// never the text there, which may hold a login.
    /// a settings file that is there but cannot be read stops the run, saying why, rather than
    /// reading as no file and leaving its settings out. here `FETCHLOOM_SETTINGS` names a folder.
    #[test]
    fn a_settings_file_that_cannot_be_read_is_an_error_not_a_missing_file() {
        let folder = tempfile::tempdir().unwrap();
        let named = folder.path().to_str().unwrap();
        let Err(error) = File::read(&env(&[("FETCHLOOM_SETTINGS", named)])) else {
            panic!("a folder was read as the settings file");
        };
        assert_eq!(error.kind, Kind::LocalIo);
        let reading = format!("could not read the settings file {named}");
        // windows refuses to open a folder as a file, and says so as a denied permission
        if cfg!(windows) {
            assert!(error.message.starts_with(&reading), "{}", error.message);
        } else {
            assert_eq!(
                error.message,
                format!("{reading}: it is a folder, not a file")
            );
            assert_eq!(
                error.help.as_deref(),
                Some("unset FETCHLOOM_SETTINGS, or set it to a settings file fetchloom may read")
            );
        }
    }

    #[test]
    fn a_settings_file_that_is_not_toml_names_where_reading_stopped() {
        let error = file("jobs = 16\n[extra\n").unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "the settings file /home/ada/.config/fetchloom/settings.toml is not valid TOML at line 2, column 7: unclosed table, expected `]`"
        );
        let error =
            file("jobs = 16\nmirrors = [\"https://ada:hunter2@lab.example/m\"\n").unwrap_err();
        assert!(error.message.contains(" at line "), "{}", error.message);
        assert!(!error.message.contains("hunter2"), "{}", error.message);
    }

    /// a key one typo away from a setting is told that setting; any other key gets the list of
    /// settings.
    #[test]
    fn a_mistyped_key_is_told_the_setting_it_is_near() {
        let typo = file("jbos = 16\n").unwrap_err();
        assert_eq!(
            typo.message,
            "the settings file /home/ada/.config/fetchloom/settings.toml has \"jbos\", which is not a setting"
        );
        assert_eq!(typo.help.as_deref(), Some("did you mean `jobs`?"));
        let unknown = file("colour = \"red\"\n").unwrap_err();
        assert_eq!(unknown.kind, Kind::Usage);
        assert_eq!(
            unknown.help.as_deref(),
            Some(
                "the settings file takes cache, resources, jobs, host_jobs, retries, timeout, ask_above, memory, most_files, archive_files, archive_size, archive_ratio, allowed_origins, s3_buckets, gcs_buckets, drive_folders, mirrors, mirrors_only, push_to, build_floor, each a number or text"
            )
        );
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
    fn lean_keeps_required_checksums_on_existing_workers() {
        for (level, processors, ceiling, expected) in [
            ("default", 12, 8, 8),
            ("default", 12, 2, 2),
            ("lean", 12, 2, 1),
            ("max", 12, 8, 8),
            ("max", 1, 8, 1),
        ] {
            let mut settings = read(&[("FETCHLOOM_RESOURCES", level)], None).unwrap();
            settings.machine.processors = processors;
            let cap = crate::threads::Threads::default().at_most(ceiling);
            assert_eq!(
                cap.limit(settings.hash_workers()),
                expected,
                "{level} on {processors} CPUs"
            );
        }
        let mut explicit = read(
            &[("FETCHLOOM_RESOURCES", "lean"), ("FETCHLOOM_THREADS", "8")],
            None,
        )
        .unwrap();
        explicit.machine.processors = 12;
        let cap = crate::threads::Threads::default().at_most(8);
        assert_eq!(
            cap.limit(explicit.hash_workers()),
            8,
            "a named ceiling overrides the level"
        );
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

    #[test]
    fn mirrors_come_from_the_variable_or_else_the_file_in_their_order() {
        let folder = |path: &str| Location::Folder(PathBuf::from(path));
        let set = read(
            &[(
                "FETCHLOOM_MIRRORS",
                " /mnt/lab/fetchloom , https://data.corp.internal/fetchloom,",
            )],
            None,
        )
        .unwrap();
        assert_eq!(set.mirrors.origin, Origin::Environment);
        assert_eq!(
            set.mirrors.value,
            [
                folder("/mnt/lab/fetchloom"),
                Location::parse("https://data.corp.internal/fetchloom", None).unwrap(),
            ]
        );
        // a folder whose name holds a comma keeps it in the file's list
        let listed = file("mirrors = [\"mirror\", \"/mnt/a,b\"]\nmirrors_only = true\n").unwrap();
        let set = Settings::read(&env(&[]), listed.clone(), machine(None)).unwrap();
        assert_eq!(set.mirrors.origin, Origin::File);
        assert_eq!(
            set.mirrors.value,
            [
                folder("/home/ada/.config/fetchloom/mirror"),
                folder("/mnt/a,b")
            ]
        );
        assert_eq!(
            set.mirrors_only,
            Set {
                value: true,
                origin: Origin::File
            }
        );
        let set = Settings::read(
            &env(&[
                ("FETCHLOOM_MIRRORS", "/usb"),
                ("FETCHLOOM_MIRRORS_ONLY", "0"),
            ]),
            listed,
            machine(None),
        )
        .unwrap();
        assert_eq!(set.mirrors.value, [folder("/usb")]);
        assert!(!set.mirrors_only.value);
        assert_eq!(read(&[], None).unwrap().mirrors, Set::default(Vec::new()));
        assert_eq!(read(&[], None).unwrap().mirrors_only, Set::default(false));
    }

    #[test]
    fn a_mirror_that_is_no_folder_or_web_address_is_a_usage_error() {
        let error = read(&[("FETCHLOOM_MIRRORS", "file:///mnt/lab")], None).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "FETCHLOOM_MIRRORS holds \"file:///mnt/lab\", which is no mirror: a file:// address names a folder"
        );
        assert_eq!(
            error.help.as_deref(),
            Some("write the folder path itself, or unset FETCHLOOM_MIRRORS to ask no mirror")
        );
        let error = Settings::read(
            &env(&[]),
            file("mirrors = [\"https://ada:hunter2@lab.example/m\"]\n").unwrap(),
            machine(None),
        )
        .unwrap_err();
        assert!(!error.message.contains("hunter2"), "{}", error.message);
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("`fetchloom auth login lab.example`")
                    && help.ends_with("remove mirrors from the settings file to ask no mirror")),
            "{:?}",
            error.help
        );
        let error = read(&[("FETCHLOOM_MIRRORS_ONLY", "yes")], None).unwrap_err();
        assert_eq!(
            error.message,
            "FETCHLOOM_MIRRORS_ONLY is \"yes\", which is not true or false"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "unset FETCHLOOM_MIRRORS_ONLY to use the default of false, or set it to true or false"
            )
        );
        let wrong = file("mirrors_only = [true]\n").unwrap_err();
        assert_eq!(wrong.kind, Kind::Usage);
    }

    /// an empty or blank variable for where requests go or where push writes clears what the
    /// settings file names there, so a run meant to reach the sources cannot take the file's
    /// mirror. other settings still fall back to the file.
    #[test]
    fn an_empty_variable_clears_the_files_mirrors_and_push_folder() {
        let filed = file(
            "mirrors = [\"/mnt/lab\"]\nmirrors_only = true\npush_to = \"/mnt/lab\"\njobs = 32\n",
        )
        .unwrap();
        for empty in ["", " "] {
            let set = Settings::read(
                &env(&[
                    ("FETCHLOOM_MIRRORS", empty),
                    ("FETCHLOOM_MIRRORS_ONLY", empty),
                    ("FETCHLOOM_PUSH_TO", empty),
                    ("FETCHLOOM_JOBS", ""),
                ]),
                filed.clone(),
                machine(None),
            )
            .unwrap();
            assert_eq!(
                set.mirrors,
                Set {
                    value: Vec::new(),
                    origin: Origin::Environment
                },
                "{empty:?}"
            );
            assert_eq!(
                set.mirrors_only,
                Set {
                    value: false,
                    origin: Origin::Environment
                },
                "{empty:?}"
            );
            assert_eq!(
                set.push_to,
                Set {
                    value: None,
                    origin: Origin::Environment
                },
                "{empty:?}"
            );
            assert_eq!(
                set.jobs,
                Set {
                    value: 32,
                    origin: Origin::File
                }
            );
        }
    }

    #[test]
    fn gcs_buckets_name_cloud_storage_buckets() {
        let shown = |settings: &Settings| -> Vec<String> {
            settings
                .gcs_buckets
                .value
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(read(&[], None).unwrap().gcs_buckets, Set::default(vec![]));
        let set = read(
            &[(
                "FETCHLOOM_GCS_BUCKETS",
                " lab-raw, gs://lab.data/ ,GS://lab_scans,lab-raw,",
            )],
            None,
        )
        .unwrap();
        assert_eq!(
            shown(&set),
            ["gs://lab-raw", "gs://lab.data", "gs://lab_scans"]
        );
        assert_eq!(set.gcs_buckets.origin, Origin::Environment);
        let filed = file("gcs_buckets = [\"gs://lab-raw\"]\n").unwrap();
        let set = Settings::read(&env(&[]), filed.clone(), machine(None)).unwrap();
        assert_eq!(shown(&set), ["gs://lab-raw"]);
        assert_eq!(set.gcs_buckets.origin, Origin::File);
        // a blank variable clears what the file lists, so no request carries a token for it
        let cleared = Settings::read(
            &env(&[("FETCHLOOM_GCS_BUCKETS", " ")]),
            filed,
            machine(None),
        )
        .unwrap();
        assert_eq!(
            cleared.gcs_buckets,
            Set {
                value: Vec::new(),
                origin: Origin::Environment
            }
        );
        for entry in [
            "gs://Lab-Raw",
            "s3://lab-raw",
            "gs://lab-raw/prefix/",
            "https://storage.googleapis.com/lab-raw",
            "gs://ab",
        ] {
            let error = read(&[("FETCHLOOM_GCS_BUCKETS", entry)], None).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{entry}");
            assert_eq!(
                error.message,
                format!(
                    "FETCHLOOM_GCS_BUCKETS holds \"{entry}\", which is not a bucket such as gs://lab-data"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_GCS_BUCKETS to send google credentials to none, or list buckets as gs://name, separated by commas"
                )
            );
        }
    }

    #[test]
    fn drive_folders_name_drive_ids() {
        const LAB: &str = "1AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        const SCANS: &str = "0B-ScansFolder_2019";
        const NOTES: &str = "1notesFileIdOfTheLab";
        let shown = |settings: &Settings| -> Vec<String> {
            settings
                .drive_folders
                .value
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        assert_eq!(read(&[], None).unwrap().drive_folders, Set::default(vec![]));
        let listed = format!(
            " {LAB}, https://drive.google.com/drive/folders/{SCANS}?usp=sharing ,drive:{NOTES},https://drive.google.com/file/d/{LAB}/view,"
        );
        let set = read(&[("FETCHLOOM_DRIVE_FOLDERS", listed.as_str())], None).unwrap();
        assert_eq!(shown(&set), [LAB, SCANS, NOTES]);
        assert_eq!(set.drive_folders.origin, Origin::Environment);
        let filed = file(&format!("drive_folders = [\"drive:folders/{SCANS}\"]\n")).unwrap();
        let set = Settings::read(&env(&[]), filed.clone(), machine(None)).unwrap();
        assert_eq!(shown(&set), [SCANS]);
        assert_eq!(set.drive_folders.origin, Origin::File);
        // a blank variable clears what the file lists, so nothing on drive gets a token
        let cleared = Settings::read(
            &env(&[("FETCHLOOM_DRIVE_FOLDERS", " ")]),
            filed,
            machine(None),
        )
        .unwrap();
        assert_eq!(
            cleared.drive_folders,
            Set {
                value: Vec::new(),
                origin: Origin::Environment
            }
        );
        for entry in [
            "lab-data",
            "gs://lab-private",
            "drive:folders/short",
            "https://example.org/drive/folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345",
            "1AbCdEfGhIjKlMnOp/QrStUvWxYz012345",
        ] {
            let error = read(&[("FETCHLOOM_DRIVE_FOLDERS", entry)], None).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{entry}");
            assert_eq!(
                error.message,
                format!(
                    "FETCHLOOM_DRIVE_FOLDERS holds \"{entry}\", which is not a google drive folder or file: its id or the link drive gives when you share it"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_DRIVE_FOLDERS to send google credentials to drive for nothing, or list ids or sharing links, separated by commas"
                )
            );
        }
    }

    /// an empty or blank `FETCHLOOM_S3_BUCKETS` clears the buckets the settings file lists, so no
    /// request is signed for them.
    #[test]
    fn an_empty_variable_clears_the_files_s3_buckets() {
        let filed = file("s3_buckets = [\"s3://lab-raw\"]\n").unwrap();
        for empty in ["", " "] {
            let set = Settings::read(
                &env(&[("FETCHLOOM_S3_BUCKETS", empty)]),
                filed.clone(),
                machine(None),
            )
            .unwrap();
            assert_eq!(
                set.s3_buckets,
                Set {
                    value: Vec::new(),
                    origin: Origin::Environment
                },
                "{empty:?}"
            );
        }
    }

    /// a variable one typo from one fetchloom reads, a character added, dropped, changed or
    /// swapped with its neighbour, is named with the one it is near; known names and names
    /// further off are left alone.
    #[test]
    fn a_name_one_typo_from_a_variable_is_named_with_it() {
        // put together, since the docs check takes each whole FETCHLOOM_ name in src for a setting
        let prefixed = |rest: &str| format!("FETCHLOOM_{rest}");
        let names = [
            prefixed("OFLINE"),
            prefixed("SPILT"),
            prefixed("MIRROR"),
            prefixed("JOBZ"),
            "FETCHLOM_CACHE".to_owned(),
            prefixed("OFFLINEE"),
            "FETCHLOOM_JOBS".to_owned(),
            "FETCHLOOM_MIRRORS".to_owned(),
            "FETCHLOOM_TEST_TICKER".to_owned(),
            prefixed("SOMETHING_ELSE"),
            prefixed("JBSO"),
            "PATH".to_owned(),
        ];
        assert_eq!(
            misnamed(names.map(OsString::from)),
            [
                (prefixed("OFLINE"), "FETCHLOOM_OFFLINE"),
                (prefixed("SPILT"), "FETCHLOOM_SPLIT"),
                (prefixed("MIRROR"), "FETCHLOOM_MIRRORS"),
                (prefixed("JOBZ"), "FETCHLOOM_JOBS"),
                ("FETCHLOM_CACHE".to_owned(), "FETCHLOOM_CACHE"),
                (prefixed("OFFLINEE"), "FETCHLOOM_OFFLINE"),
            ]
        );
    }

    /// every variable the environment page documents is one fetchloom reads, so setting it is
    /// never taken for a typo.
    #[test]
    fn every_documented_variable_is_known() {
        let page = include_str!("../docs/user/reference/environment.md");
        let documented: Vec<&str> = page
            .lines()
            .filter_map(|line| line.strip_prefix("| `FETCHLOOM_"))
            .filter_map(|rest| rest.split('`').next())
            .collect();
        assert!(documented.len() > 20, "{documented:?}");
        for name in documented {
            let name = format!("FETCHLOOM_{name}");
            assert!(known().any(|known| known == name), "{name}");
        }
    }

    #[test]
    fn push_to_is_a_folder_from_the_variable_or_else_the_file() {
        assert_eq!(read(&[], None).unwrap().push_to, Set::default(None));
        let set = read(&[("FETCHLOOM_PUSH_TO", " /mnt/lab/fetchloom ")], None).unwrap();
        assert_eq!(
            set.push_to,
            Set {
                value: Some(PathBuf::from("/mnt/lab/fetchloom")),
                origin: Origin::Environment
            }
        );
        // a relative folder is taken from the settings file's folder, as mirrors are
        let filed = file("push_to = \"mirror\"\n").unwrap();
        let set = Settings::read(&env(&[]), filed, machine(None)).unwrap();
        assert_eq!(
            set.push_to,
            Set {
                value: Some(PathBuf::from("/home/ada/.config/fetchloom/mirror")),
                origin: Origin::File
            }
        );
    }

    /// push writes folders only; a bucket or a server gets the folder through rclone.
    #[test]
    fn push_to_a_web_address_is_a_usage_error() {
        let error = read(
            &[("FETCHLOOM_PUSH_TO", "https://data.lab.example/fetchloom")],
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(
            error.message,
            "FETCHLOOM_PUSH_TO is \"https://data.lab.example/fetchloom\", which is a web address, and push writes to folders"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "set it to a folder, such as /mnt/lab/fetchloom, and copy that folder to a server or a bucket with a tool such as rclone; or unset FETCHLOOM_PUSH_TO"
            )
        );
        let error = Settings::read(
            &env(&[]),
            file("push_to = \"file:///mnt/lab\"\n").unwrap(),
            machine(None),
        )
        .unwrap_err();
        assert_eq!(
            error.help.as_deref(),
            Some("write the folder path itself, or remove push_to from the settings file")
        );
    }
}
