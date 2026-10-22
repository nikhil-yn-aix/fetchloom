//! Settings of a run, from flags and the environment: where the project is, where the store
//! is, and where user adapters and credentials live.

use std::path::{Path, PathBuf};

use fl_model::error::ErrorKind;
use fl_transfer::url::SafeUrl;

use crate::args::Global;
use crate::failure::Failure;

/// Settings of a run.
#[derive(Debug)]
pub(crate) struct Config {
    /// Where the command runs.
    pub(crate) cwd: PathBuf,
    /// The local tier.
    pub(crate) store: PathBuf,
    /// The user's configuration directory, holding `sources/` and `credentials.toml`.
    pub(crate) settings: Option<PathBuf>,
    /// How to connect.
    pub(crate) transfer: fl_transfer::client::Config,
    /// A Hugging Face Hub mirror from `HF_ENDPOINT`, used instead of huggingface.co.
    pub(crate) hub: Option<SafeUrl>,
}

impl Config {
    /// The settings from `global` and the variables `env` reads, with `cwd` as the working
    /// directory before `-C`.
    pub(crate) fn new(
        global: &Global,
        cwd: &Path,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, Failure> {
        let cwd = match &global.directory {
            Some(dir) => cwd.join(dir),
            None => cwd.to_owned(),
        };
        let store = env("FETCHLOOM_STORE")
            .map(PathBuf::from)
            .or_else(|| default_store(&env))
            .ok_or_else(|| {
                Failure::new(
                    ErrorKind::LocalInvalidConfig,
                    "no home directory to keep the store in",
                )
                .help("set FETCHLOOM_STORE to a directory for the store")
            })?;
        let config_dir = env("FETCHLOOM_CONFIG")
            .map(|file| {
                PathBuf::from(file)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
            })
            .or_else(|| default_config(&env));
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
            settings: config_dir,
            transfer: fl_transfer::client::Config::default(),
            hub,
        })
    }
}

fn default_store(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if cfg!(windows) {
        env("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("fetchloom"))
    } else {
        env("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|dir| dir.join("fetchloom"))
    }
}

fn default_config(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if cfg!(windows) {
        env("APPDATA").map(|dir| PathBuf::from(dir).join("fetchloom"))
    } else {
        env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|dir| dir.join("fetchloom"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
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
        )
        .unwrap();
        assert_eq!(config.store, PathBuf::from("/fast/store"));
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
        )
        .unwrap();
        assert_eq!(config.cwd, Path::new("/work").join("sub"));
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
        )
        .unwrap();
        if cfg!(windows) {
            assert_eq!(config.store, Path::new("/local").join("fetchloom"));
            assert_eq!(
                config.settings,
                Some(Path::new("/roaming").join("fetchloom"))
            );
        } else {
            assert_eq!(config.store, PathBuf::from("/home/a/.cache/fetchloom"));
            assert_eq!(
                config.settings,
                Some(PathBuf::from("/home/a/.config/fetchloom"))
            );
        }
    }

    #[test]
    fn no_home_and_no_store_is_an_error_with_help() {
        let failure = Config::new(&Global::default(), Path::new("/work"), with(&[])).unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::LocalInvalidConfig);
        assert!(failure.0.help.unwrap().contains("FETCHLOOM_STORE"));
    }
}
