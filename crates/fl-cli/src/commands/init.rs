//! `fl init`: a minimal `data.toml`, and the project's files kept out of git.

use std::fs;
use std::io::Write;
use std::path::Path;

use fl_model::document::Status;
use fl_model::error::ErrorKind;
use fl_model::event::Event;
use serde_json::json;
use toml_edit::{DocumentMut, table, value};

use crate::commands::Context;
use crate::failure::Failure;
use crate::project::MANIFEST;
use crate::report::Outcome;

const IGNORED: [&str; 2] = ["data/", ".fetchloom/"];

/// `fl init`.
pub(crate) fn run(context: &Context<'_>, name: Option<&str>) -> Result<Outcome, Failure> {
    let path = create(context, context.cwd, name)?;
    Ok(Outcome {
        status: Status::Ok,
        result: json!({ "manifest": path.display().to_string() }),
        answer: None,
        sentence: format!("Started a project in {}.", context.cwd.display()),
    })
}

/// Writes a new `data.toml` in `dir`, and in a git repository keeps the data directory and
/// `.fetchloom/` out of it. Returns the file written.
pub(crate) fn create(
    context: &Context<'_>,
    dir: &Path,
    name: Option<&str>,
) -> Result<std::path::PathBuf, Failure> {
    let path = dir.join(MANIFEST);
    if path.exists() {
        return Err(Failure::new(
            ErrorKind::ProjectExists,
            format!(
                "`{}` already exists and init never overwrites it",
                path.display()
            ),
        )
        .subject(path.display().to_string())
        .help("add datasets to it with `fl add REF`"));
    }
    fs::create_dir_all(dir).map_err(|error| Failure::io("create", dir, &error))?;
    let name = name.map_or_else(|| default_name(dir), str::to_owned);
    let mut document = DocumentMut::new();
    document["project"] = table();
    document["project"]["name"] = value(name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| Failure::io("create", &path, &error))?;
    file.write_all(document.to_string().as_bytes())
        .map_err(|error| Failure::io("write", &path, &error))?;
    context.emit(Event::Initialized {
        path: path.display().to_string(),
    });
    if dir.ancestors().any(|parent| parent.join(".git").exists()) {
        ignore(dir)?;
        context.emit(Event::Initialized {
            path: format!(
                "{} entries for data/ and .fetchloom/",
                dir.join(".gitignore").display()
            ),
        });
    }
    Ok(path)
}

fn default_name(dir: &Path) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project")
        .to_owned()
}

fn ignore(dir: &Path) -> Result<(), Failure> {
    let path = dir.join(".gitignore");
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(Failure::io("read", &path, &error)),
    };
    let mut text = old.clone();
    for entry in IGNORED {
        if !old.lines().any(|line| line.trim() == entry) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(entry);
            text.push('\n');
        }
    }
    if text != old {
        fs::write(&path, text).map_err(|error| Failure::io("write", &path, &error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_appends_only_missing_entries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".gitignore");
        fs::write(&path, "target\ndata/").unwrap();
        ignore(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "target\ndata/\n.fetchloom/\n"
        );
        ignore(temp.path()).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "target\ndata/\n.fetchloom/\n"
        );
    }
}
