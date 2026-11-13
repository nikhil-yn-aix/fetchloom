//! firefox's cookies: `cookies.sqlite` in the profile it used last, or the one a person names,
//! read as firefox keeps them, in plain text.

use std::path::{Path, PathBuf};

use super::sqlite::{Database, Value};
use super::{Left, Read, Why};
use crate::auth::site::Cookie;
use crate::source::Var;

/// the schema version from which firefox keeps an expiry in milliseconds, not seconds.
const MILLISECONDS: u32 = 16;

/// the folders firefox keeps its profiles in on this system, where it may have them: its own
/// on linux, the XDG one firefox 147 moved new installs to, and those of flatpak and snap.
pub(super) fn roots(var: Var<'_>) -> Vec<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let home = crate::source::home(var);
    if cfg!(windows) {
        let mut roots: Vec<PathBuf> = set("APPDATA")
            .map(|appdata| appdata.join("Mozilla").join("Firefox"))
            .into_iter()
            .collect();
        roots.extend(set("LOCALAPPDATA").map(|local| {
            local.join(r"Packages\Mozilla.Firefox_n80bbvh6b1yt2\LocalCache\Roaming\Mozilla\Firefox")
        }));
        return roots;
    }
    let Some(home) = home else {
        return Vec::new();
    };
    if cfg!(target_os = "macos") {
        return vec![home.join("Library/Application Support/Firefox")];
    }
    let config = set("XDG_CONFIG_HOME")
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    vec![
        config.join("mozilla/firefox"),
        home.join(".mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/config/mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
    ]
}

/// the profile folders under `root` that hold cookies: those `profiles.ini` names, and every
/// folder beside them, as an install keeps them in `Profiles`.
fn profiles(root: &Path) -> Vec<(PathBuf, Option<String>)> {
    let mut found: Vec<(PathBuf, Option<String>)> = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("profiles.ini")) {
        let mut name = None;
        let mut path = None;
        let mut relative = true;
        let mut flush = |name: &mut Option<String>, path: &mut Option<String>, relative: bool| {
            if let Some(folder) = path.take() {
                let folder = if relative {
                    root.join(folder)
                } else {
                    PathBuf::from(folder)
                };
                found.push((folder, name.take()));
            }
            *name = None;
        };
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                flush(&mut name, &mut path, relative);
                relative = true;
            } else if let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "Name" => name = Some(value.trim().to_owned()),
                    "Path" => path = Some(value.trim().to_owned()),
                    "IsRelative" => relative = value.trim() != "0",
                    _ => {}
                }
            }
        }
        flush(&mut name, &mut path, relative);
    }
    for folder in [root.to_path_buf(), root.join("Profiles")] {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !found.iter().any(|(known, _)| *known == path) {
                found.push((path, None));
            }
        }
    }
    found.retain(|(folder, _)| folder.join("cookies.sqlite").is_file());
    found
}

/// the cookie file of the profile `named` asks for, in one of `roots`: its name in
/// `profiles.ini`, or the last part of its folder's name, as `default-release`, or a folder by
/// its absolute path; without a name, the one firefox wrote to last.
pub(super) fn file(roots: &[PathBuf], named: Option<&str>) -> Option<PathBuf> {
    // a name is never a path from where fetchloom runs, which a cloned repo could fill
    if let Some(named) = named.map(Path::new).filter(|named| named.is_absolute()) {
        let file = named.join("cookies.sqlite");
        return file.is_file().then_some(file);
    }
    let candidates = roots.iter().flat_map(|root| profiles(root));
    let matching: Vec<PathBuf> = candidates
        .filter(|(folder, name)| {
            named.is_none_or(|named| {
                let folder_name = folder
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name.as_deref() == Some(named)
                    || folder_name == named
                    || folder_name.ends_with(&format!(".{named}"))
            })
        })
        .map(|(folder, _)| folder.join("cookies.sqlite"))
        .collect();
    super::newest(matching)
}

/// the cookies in firefox's `cookies.sqlite` at `path`. those a container, a private window or
/// another site's frames keep apart are left out.
pub(super) fn read(path: &Path) -> std::io::Result<Read> {
    taken(&Database::open(path)?)
        .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidData, why.0))
}

/// the cookies in `database`, firefox's `cookies.sqlite`, as [`read`] takes them.
pub(super) fn taken(database: &Database) -> Result<Read, super::sqlite::Broken> {
    let rows = database.rows(
        "moz_cookies",
        &[
            "name",
            "value",
            "host",
            "path",
            "expiry",
            "isSecure",
            "originAttributes",
        ],
    )?;
    let milliseconds = database.user_version >= MILLISECONDS;
    let mut read = Read::default();
    for row in rows {
        let text = |at: usize| row[at].text().unwrap_or_default().to_owned();
        let (name, domain) = (text(0), text(2));
        if !text(6).is_empty() {
            read.left.push(Left {
                name,
                domain,
                why: Why::Apart,
            });
            continue;
        }
        let expires = match row[4] {
            Value::Integer(expiry) if milliseconds => Some(expiry / 1000),
            Value::Integer(expiry) => Some(expiry),
            _ => None,
        }
        .filter(|expires| *expires > 0);
        read.found.push(Cookie {
            name,
            value: text(1),
            domain,
            path: text(3),
            secure: row[5].integer().is_some_and(|secure| secure != 0),
            expires,
        });
    }
    Ok(read)
}

#[cfg(test)]
mod tests;
