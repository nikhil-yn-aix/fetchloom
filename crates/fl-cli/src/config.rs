//! Settings of a run, merged from defaults, the system and user `config.toml`, the
//! environment and flags, each value remembering the layer it came from.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fl_model::config::MachineConfig;
use fl_model::error::ErrorKind;
use fl_model::manifest::Link;
use fl_transfer::url::SafeUrl;

use crate::args::Global;
use crate::failure::Failure;

/// Where a setting's value came from, lowest precedence first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    /// fetchloom's own default.
    Default,
    /// The system `config.toml` at this path.
    System(PathBuf),
    /// The user `config.toml` at this path.
    User(PathBuf),
    /// This environment variable.
    Environment(&'static str),
    /// This flag.
    Flag(&'static str),
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::System(path) | Self::User(path) => write!(f, "{}", path.display()),
            Self::Environment(name) => write!(f, "${name}"),
            Self::Flag(flag) => f.write_str(flag),
        }
    }
}

/// A value and the layer it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Setting<T> {
    /// The value.
    pub(crate) value: T,
    /// Where it came from.
    pub(crate) from: Layer,
}

impl<T> Setting<T> {
    fn new(value: T, from: Layer) -> Self {
        Self { value, from }
    }

    fn over(&mut self, value: Option<T>, from: Layer) {
        if let Some(value) = value {
            *self = Self::new(value, from);
        }
    }
}

/// Settings of a run.
#[derive(Debug)]
pub(crate) struct Config {
    /// Where the command runs.
    pub(crate) cwd: PathBuf,
    /// The local tier.
    pub(crate) store: Setting<PathBuf>,
    /// How files are placed in projects when a dataset does not say.
    pub(crate) link: Setting<Link>,
    /// Transfers running at once.
    pub(crate) jobs: Setting<usize>,
    /// Transfers running at once against one host.
    pub(crate) per_host: Setting<usize>,
    /// The longest silence tolerated on a connection, in seconds.
    pub(crate) timeout: Setting<u64>,
    /// No network at all.
    pub(crate) offline: Setting<bool>,
    /// The user's configuration directory, holding `sources/` and `credentials.toml`.
    pub(crate) settings: Option<PathBuf>,
    /// A Hugging Face Hub mirror from `HF_ENDPOINT`, used instead of huggingface.co.
    pub(crate) hub: Option<SafeUrl>,
}

/// Reads a configuration file, `None` when it does not exist.
pub(crate) type ReadFile<'a> = &'a dyn Fn(&Path) -> io::Result<Option<String>>;

impl Config {
    /// The settings from `global`, the variables `env` reads and the files `read` reads, with
    /// `cwd` as the working directory before `-C`.
    pub(crate) fn new(
        global: &Global,
        cwd: &Path,
        env: impl Fn(&str) -> Option<String>,
        read: ReadFile<'_>,
    ) -> Result<Self, Failure> {
        let cwd = match &global.directory {
            Some(dir) => cwd.join(dir),
            None => cwd.to_owned(),
        };
        let home = home(&env);
        let defaults = MachineDefaults::new(&env);
        let mut store = Setting::new(defaults.store.clone(), Layer::Default);
        let mut link = Setting::new(Link::Auto, Layer::Default);
        let mut jobs = Setting::new(8, Layer::Default);
        let mut per_host = Setting::new(4, Layer::Default);
        let mut timeout = Setting::new(30, Layer::Default);
        let mut offline = Setting::new(false, Layer::Default);
        let user_file = env("FETCHLOOM_CONFIG")
            .map(PathBuf::from)
            .or_else(|| defaults.config.as_ref().map(|dir| dir.join("config.toml")));
        let files = [
            defaults
                .system
                .map(|path| (path.clone(), Layer::System(path))),
            user_file
                .clone()
                .map(|path| (path.clone(), Layer::User(path))),
        ];
        for (path, layer) in files.into_iter().flatten() {
            let Some(machine) = machine(&path, read)? else {
                continue;
            };
            let local = machine
                .store
                .local
                .map(|local| expand(&local, home.as_deref()));
            store.over(local.map(Some), layer.clone());
            link.over(machine.store.link, layer.clone());
            jobs.over(machine.network.jobs, layer.clone());
            per_host.over(machine.network.per_host, layer.clone());
            timeout.over(machine.network.timeout, layer);
        }
        store.over(
            env("FETCHLOOM_STORE").map(|dir| Some(PathBuf::from(dir))),
            Layer::Environment("FETCHLOOM_STORE"),
        );
        offline.over(
            env("FETCHLOOM_OFFLINE").map(|value| value == "1"),
            Layer::Environment("FETCHLOOM_OFFLINE"),
        );
        store.over(global.store.clone().map(Some), Layer::Flag("--store"));
        jobs.over(global.jobs.map(usize::from), Layer::Flag("--jobs"));
        timeout.over(global.timeout, Layer::Flag("--timeout"));
        offline.over(global.offline.then_some(true), Layer::Flag("--offline"));
        let store = match store.value {
            Some(dir) => Setting::new(cwd.join(dir), store.from),
            None => {
                return Err(Failure::new(
                    ErrorKind::LocalInvalidConfig,
                    "no home directory to keep the store in",
                )
                .help("set FETCHLOOM_STORE to a directory for the store"));
            }
        };
        let hub = env("HF_ENDPOINT")
            .map(|endpoint| {
                SafeUrl::parse(&endpoint).map_err(|error| {
                    Failure::new(
                        ErrorKind::LocalInvalidConfig,
                        format!("HF_ENDPOINT is {error}"),
                    )
                    .help("set HF_ENDPOINT to the URL of a Hugging Face Hub, or unset it")
                })
            })
            .transpose()?;
        Ok(Self {
            cwd,
            store,
            link,
            jobs,
            per_host,
            timeout,
            offline,
            settings: user_file.and_then(|file| file.parent().map(Path::to_path_buf)),
            hub,
        })
    }

    /// How the transfer engine connects under these settings.
    pub(crate) fn transfer(&self) -> fl_transfer::client::Config {
        let silence = Duration::from_secs(self.timeout.value);
        fl_transfer::client::Config {
            response: silence,
            idle: silence,
            jobs: self.jobs.value,
            per_host: self.per_host.value,
            offline: self.offline.value,
            ..fl_transfer::client::Config::default()
        }
    }

    /// Every setting as `key`, value and the layer it came from, for `-v`.
    pub(crate) fn shown(&self) -> Vec<(&'static str, String, &Layer)> {
        vec![
            (
                "store",
                self.store.value.display().to_string(),
                &self.store.from,
            ),
            (
                "link",
                match self.link.value {
                    Link::Auto => "auto",
                    Link::Copy => "copy",
                }
                .to_owned(),
                &self.link.from,
            ),
            ("jobs", self.jobs.value.to_string(), &self.jobs.from),
            (
                "per_host",
                self.per_host.value.to_string(),
                &self.per_host.from,
            ),
            (
                "timeout",
                format!("{}s", self.timeout.value),
                &self.timeout.from,
            ),
            (
                "offline",
                self.offline.value.to_string(),
                &self.offline.from,
            ),
        ]
    }
}

/// Reads a file that may not exist.
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn machine(path: &Path, read: ReadFile<'_>) -> Result<Option<MachineConfig>, Failure> {
    let Some(text) = read(path).map_err(|error| Failure::io("read", path, &error))? else {
        return Ok(None);
    };
    MachineConfig::from_toml(&text).map(Some).map_err(|error| {
        let failure = Failure::new(
            error.kind(),
            format!("{}: {}", path.display(), error.message),
        )
        .subject(path.display().to_string());
        match error.help {
            Some(help) => failure.help(help),
            None => failure.help(format!("fix or remove `{}`", path.display())),
        }
    })
}

fn expand(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

fn home(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env(name).map(PathBuf::from)
}

/// Where things live when nothing says otherwise.
struct MachineDefaults {
    store: Option<PathBuf>,
    config: Option<PathBuf>,
    system: Option<PathBuf>,
}

impl MachineDefaults {
    fn new(env: &impl Fn(&str) -> Option<String>) -> Self {
        if cfg!(windows) {
            Self {
                store: env("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("fetchloom")),
                config: env("APPDATA").map(|dir| PathBuf::from(dir).join("fetchloom")),
                system: env("PROGRAMDATA")
                    .map(|dir| PathBuf::from(dir).join("fetchloom").join("config.toml")),
            }
        } else {
            let home = env("HOME").map(PathBuf::from);
            Self {
                store: env("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .or_else(|| home.as_ref().map(|home| home.join(".cache")))
                    .map(|dir| dir.join("fetchloom")),
                config: env("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| home.map(|home| home.join(".config")))
                    .map(|dir| dir.join("fetchloom")),
                system: Some(PathBuf::from("/etc/fetchloom/config.toml")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    const NO_FILES: ReadFile<'static> = &|_| Ok(None);

    fn system_file() -> PathBuf {
        if cfg!(windows) {
            Path::new("/pd").join("fetchloom").join("config.toml")
        } else {
            PathBuf::from("/etc/fetchloom/config.toml")
        }
    }

    #[test]
    fn the_environment_names_the_store_and_the_config_file() {
        let config = Config::new(
            &Global::default(),
            Path::new("/work"),
            with(&[
                ("FETCHLOOM_STORE", "/fast/store"),
                ("FETCHLOOM_CONFIG", "/etc/fl/config.toml"),
            ]),
            NO_FILES,
        )
        .unwrap();
        assert_eq!(config.store.value, Path::new("/fast/store"));
        assert_eq!(config.store.from, Layer::Environment("FETCHLOOM_STORE"));
        assert_eq!(config.settings, Some(PathBuf::from("/etc/fl")));
        assert_eq!(config.cwd, PathBuf::from("/work"));
    }

    #[test]
    fn a_directory_flag_moves_the_working_directory() {
        let global = Global {
            directory: Some(PathBuf::from("sub")),
            ..Global::default()
        };
        let config = Config::new(
            &global,
            Path::new("/work"),
            with(&[("FETCHLOOM_STORE", "s")]),
            NO_FILES,
        )
        .unwrap();
        assert_eq!(config.cwd, Path::new("/work").join("sub"));
        assert_eq!(config.store.value, Path::new("/work").join("sub").join("s"));
    }

    #[test]
    fn the_store_defaults_under_the_platform_cache() {
        let config = Config::new(
            &Global::default(),
            Path::new("/work"),
            with(&[
                ("LOCALAPPDATA", "/local"),
                ("HOME", "/home/a"),
                ("APPDATA", "/roaming"),
            ]),
            NO_FILES,
        )
        .unwrap();
        assert_eq!(config.store.from, Layer::Default);
        if cfg!(windows) {
            assert_eq!(config.store.value, Path::new("/local").join("fetchloom"));
            assert_eq!(
                config.settings,
                Some(Path::new("/roaming").join("fetchloom"))
            );
        } else {
            assert_eq!(
                config.store.value,
                PathBuf::from("/home/a/.cache/fetchloom")
            );
            assert_eq!(
                config.settings,
                Some(PathBuf::from("/home/a/.config/fetchloom"))
            );
        }
    }

    #[test]
    fn no_home_and_no_store_is_an_error_with_help() {
        let failure =
            Config::new(&Global::default(), Path::new("/work"), with(&[]), NO_FILES).unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::LocalInvalidConfig);
        assert!(failure.0.help.unwrap().contains("FETCHLOOM_STORE"));
    }

    #[test]
    fn flags_beat_the_environment_which_beats_user_which_beats_system() {
        let files: HashMap<PathBuf, &str> = HashMap::from([
            (
                system_file(),
                "[store]\nlocal = \"/sys/store\"\nlink = \"copy\"\n[network]\njobs = 2\nper_host = 1\ntimeout = 5\n",
            ),
            (
                PathBuf::from("/u/config.toml"),
                "[store]\nlocal = \"~/mine\"\n[network]\njobs = 3\n",
            ),
        ]);
        let read = |path: &Path| Ok(files.get(path).map(|text| (*text).to_owned()));
        let env = with(&[
            ("FETCHLOOM_CONFIG", "/u/config.toml"),
            ("PROGRAMDATA", "/pd"),
            ("HOME", "/home/a"),
            ("USERPROFILE", "/home/a"),
            ("FETCHLOOM_OFFLINE", "1"),
        ]);
        let config = Config::new(&Global::default(), Path::new("/w"), &env, &read).unwrap();
        let user = Layer::User(PathBuf::from("/u/config.toml"));
        let system = Layer::System(system_file());
        assert_eq!(
            config.store,
            Setting::new(Path::new("/home/a").join("mine"), user.clone())
        );
        assert_eq!(config.link, Setting::new(Link::Copy, system.clone()));
        assert_eq!(config.jobs, Setting::new(3, user));
        assert_eq!(config.per_host, Setting::new(1, system.clone()));
        assert_eq!(config.timeout, Setting::new(5, system));
        assert_eq!(
            config.offline,
            Setting::new(true, Layer::Environment("FETCHLOOM_OFFLINE"))
        );
        let transfer = config.transfer();
        assert_eq!(transfer.jobs, 3);
        assert_eq!(transfer.per_host, 1);
        assert_eq!(transfer.idle, Duration::from_secs(5));
        assert!(transfer.offline);

        let global = Global {
            store: Some(PathBuf::from("/flag")),
            jobs: Some(9),
            timeout: Some(7),
            ..Global::default()
        };
        let config = Config::new(&global, Path::new("/w"), &env, &read).unwrap();
        assert_eq!(
            config.store,
            Setting::new(PathBuf::from("/flag"), Layer::Flag("--store"))
        );
        assert_eq!(config.jobs, Setting::new(9, Layer::Flag("--jobs")));
        assert_eq!(config.timeout, Setting::new(7, Layer::Flag("--timeout")));
        let shown = config.shown();
        assert_eq!(shown[2], ("jobs", "9".to_owned(), &Layer::Flag("--jobs")));
        assert_eq!(shown[4].1, "7s");
    }

    #[test]
    fn a_bad_config_file_names_the_file_the_key_and_the_closest_key() {
        let read = |_: &Path| Ok(Some("[network]\njbos = 2\n".to_owned()));
        let failure = Config::new(
            &Global::default(),
            Path::new("/w"),
            with(&[
                ("FETCHLOOM_CONFIG", "/u/config.toml"),
                ("FETCHLOOM_STORE", "s"),
            ]),
            &read,
        )
        .unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::LocalInvalidConfig);
        assert!(
            failure.to_string().contains("unknown key `jbos`"),
            "{failure}"
        );
        assert_eq!(
            failure.0.help.as_deref(),
            Some("the closest valid key is `jobs`")
        );
    }

    #[test]
    fn a_tilde_is_the_home_directory_only_at_the_start() {
        let home = Path::new("/h");
        assert_eq!(expand("~", Some(home)), Path::new("/h"));
        assert_eq!(expand("~/a", Some(home)), Path::new("/h").join("a"));
        assert_eq!(expand("~a", Some(home)), Path::new("~a"));
        assert_eq!(expand("~/a", None), Path::new("~/a"));
        assert_eq!(expand("/x/~", Some(home)), Path::new("/x/~"));
    }

    #[test]
    fn layers_name_where_a_value_came_from() {
        assert_eq!(Layer::Default.to_string(), "default");
        assert_eq!(
            Layer::Environment("FETCHLOOM_STORE").to_string(),
            "$FETCHLOOM_STORE"
        );
        assert_eq!(Layer::Flag("--jobs").to_string(), "--jobs");
        assert_eq!(Layer::User(PathBuf::from("c.toml")).to_string(), "c.toml");
    }
}
