//! Contract tests over configuration precedence and what supplies each value.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::path::PathBuf;

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
use fetchloom_engine as _;
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use fetchloom_cli::config::{self, Discovered, Origin};
use fetchloom_cli::settings::{self, Environment};
use fetchloom_cli::surface::{DisplayMode, GlobalFlags};
use tempfile::TempDir;

mod support;

use support::FakeEnvironment;

fn write(directory: &std::path::Path, name: &str, body: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

fn discovered_from(project: Option<&str>, user: Option<&str>) -> (TempDir, Discovered) {
    let temporary = TempDir::new().unwrap();
    let project = project.map(|body| {
        let path = write(temporary.path(), "fetchloom.toml", body);
        config::read(&path).unwrap()
    });
    let user = user.map(|body| {
        let path = write(temporary.path(), "config.toml", body);
        config::read(&path).unwrap()
    });
    (temporary, Discovered { project, user })
}

fn threads_origin(
    flags: &GlobalFlags,
    project: Option<&str>,
    user: Option<&str>,
    environment: &dyn Environment,
) -> (Option<std::num::NonZeroU32>, Origin) {
    let (_temporary, discovered) = discovered_from(project, user);
    let resolved = settings::resolve_all(
        flags,
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        environment,
    )
    .expect("no level supplied a value this build cannot read");
    (resolved.threads.value, resolved.threads.origin)
}

#[test]
fn a_default_is_used_when_no_level_supplies_a_value() {
    let (value, origin) = threads_origin(
        &GlobalFlags::default(),
        None,
        None,
        &FakeEnvironment::default(),
    );
    assert_eq!(value, None);
    assert_eq!(origin, Origin::Default);
}

#[test]
fn a_user_config_beats_a_default() {
    let (value, origin) = threads_origin(
        &GlobalFlags::default(),
        None,
        Some("threads = 9\n"),
        &FakeEnvironment::default(),
    );
    assert_eq!(value, std::num::NonZeroU32::new(9));
    assert_eq!(origin, Origin::UserConfig);
}

#[test]
fn a_project_config_beats_a_user_config() {
    let (value, origin) = threads_origin(
        &GlobalFlags::default(),
        Some("threads = 2\n"),
        Some("threads = 9\n"),
        &FakeEnvironment::default(),
    );
    assert_eq!(value, std::num::NonZeroU32::new(2));
    assert_eq!(origin, Origin::ProjectConfig);
}

#[test]
fn the_environment_beats_a_project_config() {
    let (value, origin) = threads_origin(
        &GlobalFlags::default(),
        Some("threads = 2\n"),
        Some("threads = 9\n"),
        &FakeEnvironment::with(&[("FETCHLOOM_THREADS", "5")]),
    );
    assert_eq!(value, std::num::NonZeroU32::new(5));
    assert_eq!(origin, Origin::Environment);
}

#[test]
fn the_command_line_beats_the_environment() {
    let flags = GlobalFlags {
        threads: Some(7),
        ..GlobalFlags::default()
    };
    let (value, origin) = threads_origin(
        &flags,
        Some("threads = 2\n"),
        Some("threads = 9\n"),
        &FakeEnvironment::with(&[("FETCHLOOM_THREADS", "5")]),
    );
    assert_eq!(value, std::num::NonZeroU32::new(7));
    assert_eq!(origin, Origin::CommandLine);
}

#[test]
fn all_five_levels_are_distinguished_in_one_resolution() {
    let (_temporary, discovered) =
        discovered_from(Some("display = \"none\"\n"), Some("threads = 9\n"));
    let flags = GlobalFlags {
        offline: true,
        ..GlobalFlags::default()
    };
    let resolved = settings::resolve_all(
        &flags,
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::default(),
    )
    .expect("no level supplied a value this build cannot read");

    assert_eq!(resolved.offline.origin, Origin::CommandLine);
    assert_eq!(resolved.threads.origin, Origin::UserConfig);
    assert_eq!(resolved.display.origin, Origin::ProjectConfig);

    let with_environment = settings::resolve_all(
        &GlobalFlags::default(),
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::with(&[("FETCHLOOM_THREADS", "4")]),
    )
    .expect("no level supplied a value this build cannot read");
    assert_eq!(with_environment.threads.origin, Origin::Environment);
    assert_eq!(with_environment.offline.origin, Origin::Default);

    assert!(resolved.offline.value);
    assert_eq!(resolved.threads.value, std::num::NonZeroU32::new(9));
    assert_eq!(resolved.display.value, DisplayMode::None);
    assert_eq!(with_environment.threads.value, std::num::NonZeroU32::new(4));
}

#[test]
fn explain_names_the_origin_of_every_setting() {
    let (_temporary, discovered) = discovered_from(Some("threads = 3\n"), None);
    let resolved = settings::resolve_all(
        &GlobalFlags::default(),
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::default(),
    )
    .expect("no level supplied a value this build cannot read");
    let rows = fetchloom_cli::explain::rows(&resolved, &measured(4));

    let threads = rows.iter().find(|row| row.key == "threads").unwrap();
    assert_eq!(threads.origin, "project config");
    assert_eq!(threads.value, "3");
    let display = rows.iter().find(|row| row.key == "display").unwrap();
    assert_eq!(display.origin, "default");
    assert!(rows.iter().all(|row| !row.origin.is_empty()));
}

#[test]
fn a_setting_no_level_supplied_reports_the_measurement_that_chose_it() {
    let (_temporary, discovered) = discovered_from(None, None);
    let resolved = settings::resolve_all(
        &GlobalFlags::default(),
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::default(),
    )
    .expect("no level supplied a value this build cannot read");
    let rows = fetchloom_cli::explain::rows(&resolved, &measured(4));
    let threads = rows.iter().find(|row| row.key == "threads").unwrap();
    assert_eq!(threads.value, "4");
    assert_eq!(threads.origin, "measured");
}

#[test]
fn an_unknown_key_is_an_error() {
    let temporary = TempDir::new().unwrap();
    let path = write(temporary.path(), "fetchloom.toml", "nonsense = 1\n");
    assert!(config::read(&path).is_err());
}

#[test]
fn every_key_the_contract_names_takes_effect() {
    let temporary = TempDir::new().unwrap();
    for (key, shown) in [
        ("color = \"never\"", "color = never"),
        ("hints = false", "hints = false"),
        ("log = \"debug\"", "log = debug"),
        ("retries = 4", "retries = 4"),
        ("timeout = \"45s\"", "timeout = 45s"),
        (
            "sources = [\"https://lab.edu/data/\"]",
            "sources = https://lab.edu/data/",
        ),
    ] {
        let path = write(temporary.path(), "fetchloom.toml", &format!("{key}\n"));
        assert!(
            config::read(&path).is_ok(),
            "{key} is a contracted key and was refused"
        );

        let output = support::fetchloom_reading_configuration()
            .arg("explain")
            .arg("--config")
            .arg(&path)
            .current_dir(temporary.path())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&output.stdout);
        let collapsed: String = said
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        let wanted: String = shown.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            collapsed.contains(&wanted),
            "{key} parsed but took no effect, because explain says:\n{said}"
        );
    }
}

#[test]
fn a_reserved_prefix_key_is_an_error() {
    let temporary = TempDir::new().unwrap();
    let path = write(temporary.path(), "fetchloom.toml", "x-future = 1\n");
    assert!(config::read(&path).is_err());
}

#[test]
fn a_project_file_is_found_by_searching_upward() {
    let temporary = TempDir::new().unwrap();
    let deep = temporary.path().join("one").join("two").join("three");
    std::fs::create_dir_all(&deep).unwrap();
    write(temporary.path(), "fetchloom.toml", "threads = 4\n");

    let found = config::find_project_file(&deep).unwrap();
    assert_eq!(found, temporary.path().join("fetchloom.toml"));
}

#[test]
fn disabling_configuration_reaches_neither_file() {
    let temporary = TempDir::new().unwrap();
    write(temporary.path(), "fetchloom.toml", "threads = 4\n");

    let discovered = config::discover(temporary.path(), None, true).unwrap();
    assert!(discovered.project.is_none());
    assert!(discovered.user.is_none());

    let resolved = settings::resolve_all(
        &GlobalFlags::default(),
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::default(),
    )
    .expect("no level supplied a value this build cannot read");
    assert_eq!(resolved.threads.value, None);
    assert_eq!(resolved.threads.origin, Origin::Default);
}

#[test]
fn a_named_file_disables_the_search() {
    let temporary = TempDir::new().unwrap();
    write(temporary.path(), "fetchloom.toml", "threads = 4\n");
    let named = write(temporary.path(), "other.toml", "threads = 8\n");

    let discovered = config::discover(temporary.path(), Some(&named), false).unwrap();
    let resolved = settings::resolve_all(
        &GlobalFlags::default(),
        &fetchloom_cli::surface::TransferFlags::default(),
        &discovered,
        &FakeEnvironment::default(),
    )
    .expect("no level supplied a value this build cannot read");
    assert_eq!(resolved.threads.value, std::num::NonZeroU32::new(8));
    assert_eq!(resolved.threads.origin, Origin::ProjectConfig);
}

#[test]
fn the_configuration_location_is_not_the_cache_location() {
    let environment = FakeEnvironment::with(&[
        ("HOME", "/home/person"),
        ("XDG_CONFIG_HOME", "/home/person/.config"),
        ("XDG_CACHE_HOME", "/home/person/.cache"),
        ("APPDATA", "C:\\Users\\person\\AppData\\Roaming"),
        ("LOCALAPPDATA", "C:\\Users\\person\\AppData\\Local"),
    ]);
    let cache = fetchloom_cli::settings::default_cache_dir(&environment);
    let configuration = config::user_config_directory().unwrap_or_else(|| PathBuf::from("unset"));
    assert_ne!(cache, configuration);
}

/// What a test's machine is said to have measured, so a row's origin is the
/// level that supplied it rather than the machine the test runs on.
fn measured(threads: u32) -> fetchloom_cli::explain::Measured {
    fetchloom_cli::explain::Measured {
        threads,
        concurrency: 1,
        per_host: 1,
        recorded: Vec::new(),
    }
}
