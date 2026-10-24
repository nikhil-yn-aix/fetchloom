//! `fl init`: a minimal `data.toml`, and the project's files kept out of git.

use std::fs;
use std::io::Write;
use std::path::Path;

use fl_model::error::ErrorKind;
use fl_model::event::Event;
use serde_json::json;
use toml_edit::{DocumentMut, table, value};

use crate::args::Add;
use crate::ask::Ask;
use crate::commands::Context;
use crate::failure::Failure;
use crate::project::MANIFEST;
use crate::report::Outcome;

const IGNORED: [&str; 2] = ["data/", ".fetchloom/"];

/// `fl init`, then `fl add` of `first` when the walk through `-i` named a first dataset.
pub(crate) async fn run(
    context: &Context<'_>,
    name: Option<&str>,
    first: Option<&str>,
) -> Result<Outcome, Failure> {
    let path = create(context, context.cwd, name)?;
    if let Some(first) = first {
        let add = Add {
            reference: Some(first.to_owned()),
            ..Add::default()
        };
        return super::add::run(context, &add).await;
    }
    Ok(Outcome::ok(
        json!({ "manifest": path.display().to_string() }),
        format!("Started a project in {}.", context.cwd.display()),
    ))
}

/// What the walk of `fl init -i` asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Answers {
    /// The project's name.
    pub(crate) name: Option<String>,
    /// A first dataset to add.
    pub(crate) first: Option<String>,
    /// Where the store should live from now on, when it should move.
    pub(crate) store: Option<String>,
}

/// Asks for the project's name, a first dataset and where the store lives. Empty answers keep
/// the defaults: the folder's name, no dataset, the store at `store`.
pub(crate) fn interview(ask: &dyn Ask, dir: &Path, store: &Path) -> Result<Answers, Failure> {
    let refusal = || {
        Failure::new(
            ErrorKind::Usage,
            "fl init -i asks questions, and there is no terminal to ask on",
        )
        .help("run `fl init --name NAME`, then `fl add REF`")
    };
    let given = |text: String| (!text.is_empty()).then_some(text);
    let name = given(ask.text(
        &format!("Project name (empty for {})", default_name(dir)),
        refusal(),
    )?);
    let store_answer = given(ask.text(
        &format!(
            "Where should downloads be kept? (empty for {})",
            store.display()
        ),
        refusal(),
    )?)
    .map(|answer| {
        if answer.starts_with('~') {
            answer
        } else {
            dir.join(&answer).display().to_string()
        }
    })
    .filter(|answer| Path::new(answer) != store);
    let first = given(ask.text(
        "A first dataset: a DOI, a link, or search words (empty to skip)",
        refusal(),
    )?);
    Ok(Answers {
        name,
        first,
        store: store_answer,
    })
}

/// Sets `[store] local` to `store` in the user configuration file at `file`, keeping the rest
/// of it as written.
pub(crate) fn save_store(file: &Path, store: &str) -> Result<(), Failure> {
    let old = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(Failure::io("read", file, &error)),
    };
    let mut document: DocumentMut = old.parse().map_err(|error: toml_edit::TomlError| {
        Failure::new(
            ErrorKind::LocalInvalidConfig,
            format!("{}: {}", file.display(), error.message()),
        )
    })?;
    document.entry("store").or_insert(table())["local"] = value(store);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|error| Failure::io("create", parent, &error))?;
    }
    crate::project::write(file, &document.to_string())
}

/// Writes a new `data.toml` in `dir`, and in a git repository keeps the data directory and
/// `.fetchloom/` out of it. Returns the file written.
pub(crate) fn create(
    context: &Context<'_>,
    dir: &Path,
    name: Option<&str>,
) -> Result<std::path::PathBuf, Failure> {
    refuse_existing(dir)?;
    let path = dir.join(MANIFEST);
    fs::create_dir_all(dir).map_err(|error| Failure::io("create", dir, &error))?;
    let text = minimal(dir, name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| Failure::io("create", &path, &error))?;
    file.write_all(text.as_bytes())
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

/// Fails when `dir` already holds a `data.toml`, which init never overwrites.
pub(crate) fn refuse_existing(dir: &Path) -> Result<(), Failure> {
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
    Ok(())
}

/// The text of a new `data.toml` in `dir`: the project's name, the folder's when none is given.
pub(crate) fn minimal(dir: &Path, name: Option<&str>) -> String {
    let name = name.map_or_else(|| default_name(dir), str::to_owned);
    let mut document = DocumentMut::new();
    document["project"] = table();
    document["project"]["name"] = value(name);
    document.to_string()
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

    #[test]
    fn a_new_manifest_is_named_for_its_folder_or_project_when_it_has_none() {
        assert_eq!(
            minimal(Path::new("work").join("survey").as_path(), None),
            "[project]\nname = \"survey\"\n"
        );
        assert_eq!(
            minimal(Path::new("/"), None),
            "[project]\nname = \"project\"\n"
        );
        assert_eq!(
            minimal(Path::new("survey"), Some("birds")),
            "[project]\nname = \"birds\"\n"
        );
    }

    #[test]
    fn a_gitignore_that_cannot_be_read_is_an_error_and_a_missing_one_is_made() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".gitignore")).unwrap();
        let error = ignore(temp.path()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LocalIo);
        let other = tempfile::tempdir().unwrap();
        ignore(other.path()).unwrap();
        assert_eq!(
            fs::read_to_string(other.path().join(".gitignore")).unwrap(),
            "data/\n.fetchloom/\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_dataset_from_the_walk_is_added_to_the_new_project() {
        let fixture = crate::commands::harness::Fixture::new();
        let raw = fixture.cwd.join("raw");
        fs::create_dir_all(&raw).unwrap();
        fs::write(raw.join("a.txt"), b"hello").unwrap();
        let context = fixture.context(&crate::ask::Refuse);
        let outcome = run(&context, Some("demo"), Some("./raw")).await.unwrap();
        let manifest = fs::read_to_string(fixture.cwd.join(MANIFEST)).unwrap();
        assert!(manifest.contains("name = \"demo\""), "{manifest}");
        assert!(manifest.contains("[datasets.raw]"), "{manifest}");
        assert!(manifest.contains("ref = \"./raw\""), "{manifest}");
        assert!(
            outcome.sentence.contains("Added raw"),
            "{}",
            outcome.sentence
        );
    }

    struct Typed(std::sync::Mutex<Vec<&'static str>>);

    impl Ask for Typed {
        fn confirm(&self, _question: &str, refusal: Failure) -> Result<bool, Failure> {
            Err(refusal)
        }

        fn pick(
            &self,
            _prompt: &str,
            _items: &[String],
            refusal: Failure,
        ) -> Result<Option<usize>, Failure> {
            Err(refusal)
        }

        fn text(&self, _prompt: &str, _refusal: Failure) -> Result<String, Failure> {
            Ok(self.0.lock().unwrap().remove(0).to_owned())
        }
    }

    #[test]
    fn empty_answers_keep_every_default() {
        let typed = Typed(std::sync::Mutex::new(vec!["", "", ""]));
        let answers = interview(&typed, Path::new("survey"), Path::new("/store")).unwrap();
        assert_eq!(answers, Answers::default());
    }

    #[test]
    fn answers_are_taken_and_the_same_store_is_no_move() {
        let typed = Typed(std::sync::Mutex::new(vec![
            "birds",
            "/elsewhere",
            "zenodo:1",
        ]));
        let answers = interview(&typed, Path::new("survey"), Path::new("/store")).unwrap();
        assert_eq!(
            answers,
            Answers {
                name: Some("birds".to_owned()),
                first: Some("zenodo:1".to_owned()),
                store: Some("/elsewhere".to_owned()),
            }
        );
        let typed = Typed(std::sync::Mutex::new(vec!["", "/store", ""]));
        let answers = interview(&typed, Path::new("survey"), Path::new("/store")).unwrap();
        assert_eq!(answers.store, None);
    }

    #[test]
    fn without_a_terminal_the_interview_names_the_flags_instead() {
        let error =
            interview(&crate::ask::Refuse, Path::new("survey"), Path::new("/s")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert!(error.to_string().contains("no terminal"), "{error}");
    }

    #[test]
    fn saving_the_store_keeps_the_rest_of_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("fetchloom").join("config.toml");
        save_store(&file, "/first").unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[store]\nlocal = \"/first\"\n"
        );
        fs::write(
            &file,
            "# mine\n[network]\njobs = 4\n\n[store]\nlocal = \"/first\"\n",
        )
        .unwrap();
        save_store(&file, "/second").unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "# mine\n[network]\njobs = 4\n\n[store]\nlocal = \"/second\"\n"
        );
        fs::write(&file, "[store\n").unwrap();
        let error = save_store(&file, "/third").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LocalInvalidConfig);
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        let error = save_store(&folder, "/fourth").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LocalIo);
    }
}
