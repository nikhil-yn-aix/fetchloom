//! Reporting every effective setting and the level that supplied it.

use crate::config::Discovered;
use crate::settings::Settings;

/// One setting as `explain` reports it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Explained {
    /// The setting's name.
    pub key: String,
    /// Its effective value.
    pub value: String,
    /// The level that supplied it.
    pub origin: String,
}

/// Renders one setting's value for reporting.
///
/// A value nothing supplied is shown as a question mark, never as an invented
/// number and never as a language construct the reader has to know.
trait Describe {
    fn describe(&self) -> String;
}

impl Describe for bool {
    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Describe for u32 {
    fn describe(&self) -> String {
        self.to_string()
    }
}

impl Describe for std::path::PathBuf {
    fn describe(&self) -> String {
        self.display().to_string()
    }
}

impl Describe for crate::surface::DisplayMode {
    fn describe(&self) -> String {
        match self {
            Self::Plain => "plain".to_owned(),
            Self::Live => "live".to_owned(),
            Self::None => "none".to_owned(),
        }
    }
}

impl<T: Describe> Describe for Option<T> {
    fn describe(&self) -> String {
        self.as_ref()
            .map_or_else(|| "?".to_owned(), Describe::describe)
    }
}

/// Reports every effective setting with the level that supplied it.
///
/// Takes the resolved settings. Returns one row per setting, in a stable order,
/// each naming the value and the precedence level it came from.
#[must_use]
pub fn rows(settings: &Settings) -> Vec<Explained> {
    macro_rules! row {
        ($name:literal, $field:ident) => {
            Explained {
                key: $name.to_owned(),
                value: Describe::describe(&settings.$field.value),
                origin: settings.$field.origin.to_string(),
            }
        };
    }
    vec![
        row!("offline", offline),
        row!("threads", threads),
        row!("display", display),
        row!("cache.dir", cache_dir),
    ]
}

/// Describes where the configuration files came from.
///
/// Takes the files a run discovered. Returns one line per level, naming the
/// path that was found, or that the level supplied nothing.
#[must_use]
pub fn file_lines(discovered: &Discovered) -> Vec<String> {
    let project = discovered.project.as_ref().map_or_else(
        || "project config: none found".to_owned(),
        |loaded| format!("project config: {}", loaded.path.display()),
    );
    let user = discovered.user.as_ref().map_or_else(
        || "user config: none found".to_owned(),
        |loaded| format!("user config: {}", loaded.path.display()),
    );
    vec![project, user]
}
