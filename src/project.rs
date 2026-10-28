//! the project: the folder holding `data.toml`, its `data.lock`, the data folder, and the small
//! `.fetchloom` folder of facts about what was placed.

pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod state;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use self::lock::Lock;
use self::manifest::Manifest;
use self::state::State;
use crate::error::{Error, Kind};

const FOLDER: &str = ".fetchloom";

pub(crate) struct Project {
    pub(crate) root: PathBuf,
}

/// the project lock, held while a command changes the project. dropping it lets others in.
pub(crate) struct Held(Option<File>);

pub(crate) enum Hold {
    Held(Held),
    /// another fetchloom is changing this project right now.
    Busy,
}

impl Project {
    /// the nearest folder at or above `start` that holds a data.toml.
    pub(crate) fn find(start: &Path) -> Option<Project> {
        start
            .ancestors()
            .find(|folder| folder.join(manifest::FILE).is_file())
            .map(|root| Project {
                root: root.to_path_buf(),
            })
    }

    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.root.join(manifest::FILE)
    }

    /// data.toml's text and what it says.
    pub(crate) fn manifest(&self) -> Result<(String, Manifest), Error> {
        let text = fs::read_to_string(self.manifest_path())
            .map_err(|err| self.unreadable(manifest::FILE, &err))?;
        let manifest = Manifest::parse(&text)?;
        Ok((text, manifest))
    }

    /// data.lock, or nothing when the project has none yet.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Error> {
        match fs::read_to_string(self.root.join(lock::FILE)) {
            Ok(text) => Lock::parse(&text).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::new(
                Kind::ProjectInvalid,
                "data.lock cannot be read: it is not UTF-8 text",
            )
            .help(lock::BROKEN)),
            Err(err) => Err(self.unreadable(lock::FILE, &err)),
        }
    }

    pub(crate) fn state(&self) -> State {
        fs::read(self.root.join(FOLDER).join("state"))
            .map(|bytes| State::parse(&bytes))
            .unwrap_or_default()
    }

    /// replaces a file at the project's root in one step.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        write_atomically(&self.root.join(name), bytes)
            .map_err(|err| Error::io(format_args!("could not write {name}"), &err))
    }

    pub(crate) fn write_state(&self, state: &State) -> Result<(), Error> {
        let folder = self.inner()?;
        write_atomically(&folder.join("state"), &state.render())
            .map_err(|err| Error::io("could not record what was placed", &err))
    }

    /// takes the project lock, unless another fetchloom holds it.
    pub(crate) fn hold(&self) -> Result<Hold, Error> {
        let folder = self.inner()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(folder.join("lock"))
            .map_err(|err| Error::io("could not open the project lock", &err))?;
        match file.try_lock() {
            Ok(()) => Ok(Hold::Held(Held(Some(file)))),
            Err(TryLockError::WouldBlock) => Ok(Hold::Busy),
            // a filesystem without locks still works, one fetchloom at a time
            Err(TryLockError::Error(err)) => {
                tracing::debug!(%err, "this filesystem has no file locks");
                Ok(Hold::Held(Held(None)))
            }
        }
    }

    /// the data folder, made sure to be a real folder inside the project with no link on the way,
    /// so nothing is ever placed or deleted outside it.
    pub(crate) fn data_dir(&self, manifest: &Manifest) -> Result<PathBuf, Error> {
        let mut folder = self.root.clone();
        for part in manifest.data_dir.split('/') {
            folder.push(part);
            match fs::symlink_metadata(&folder) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(Error::new(Kind::ProjectInvalid, format!("{} is a link, and fetchloom only places files in real folders", folder.display()))
                        .help("replace the link with a folder, or set data_dir in data.toml to another folder"));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(Error::new(
                        Kind::ProjectInvalid,
                        format!(
                            "{} is a file where the data folder should be",
                            folder.display()
                        ),
                    ));
                }
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&folder).map_err(|err| {
                        Error::io(format_args!("could not create {}", folder.display()), &err)
                    })?;
                }
                Err(err) => {
                    return Err(Error::io(
                        format_args!("could not read {}", folder.display()),
                        &err,
                    ));
                }
            }
        }
        Ok(folder)
    }

    /// adds the data folder to the .gitignore of the git repository the project is in, unless it is
    /// there. says whether it was added.
    pub(crate) fn ignore_in_git(&self, data_dir: &str) -> Result<bool, Error> {
        if !self
            .root
            .ancestors()
            .any(|folder| folder.join(".git").exists())
        {
            return Ok(false);
        }
        let path = self.root.join(".gitignore");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(self.unreadable(".gitignore", &err)),
        };
        let entry = format!("/{data_dir}/");
        let present = text.lines().map(str::trim).any(|line| {
            [
                entry.as_str(),
                &entry[1..],
                &entry[..entry.len() - 1],
                &entry[1..entry.len() - 1],
            ]
            .contains(&line)
        });
        if present {
            return Ok(false);
        }
        let mut updated = text;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&entry);
        updated.push('\n');
        self.write(".gitignore", updated.as_bytes())?;
        Ok(true)
    }

    /// the .fetchloom folder, which tells git to ignore it the first time it is made.
    fn inner(&self) -> Result<PathBuf, Error> {
        let folder = self.root.join(FOLDER);
        if !folder.is_dir() {
            fs::create_dir_all(&folder).map_err(|err| {
                Error::io(format_args!("could not create {}", folder.display()), &err)
            })?;
            write_atomically(&folder.join(".gitignore"), b"*\n").map_err(|err| {
                Error::io(format_args!("could not write {}", folder.display()), &err)
            })?;
        }
        Ok(folder)
    }

    fn unreadable(&self, name: &str, err: &io::Error) -> Error {
        Error::io(
            format_args!("could not read {}", self.root.join(name).display()),
            err,
        )
    }
}

/// replaces `path` in one step, so a reader sees the old file or the new one, never half.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(folder)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let project = Project {
            root: dir.path().to_path_buf(),
        };
        (dir, project)
    }

    #[test]
    fn the_project_is_the_nearest_folder_with_a_data_toml() {
        let (dir, _) = project(&[("data.toml", ""), ("a/b/c.txt", "")]);
        let found = Project::find(&dir.path().join("a").join("b")).unwrap();
        assert_eq!(found.root, dir.path());
        assert!(Project::find(&tempfile::tempdir().unwrap().path().join("x")).is_none());
    }

    #[test]
    fn one_command_changes_the_project_at_a_time() {
        let (_dir, project) = project(&[("data.toml", "")]);
        let Hold::Held(held) = project.hold().unwrap() else {
            panic!("free")
        };
        assert!(matches!(project.hold().unwrap(), Hold::Busy));
        drop(held);
        assert!(matches!(project.hold().unwrap(), Hold::Held(_)));
        assert_eq!(
            fs::read_to_string(project.root.join(".fetchloom/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn the_data_folder_is_made_and_never_a_file() {
        let (_dir, project) = project(&[("data.toml", ""), ("blocked", "a file")]);
        let manifest = Manifest::parse("").unwrap();
        assert!(project.data_dir(&manifest).unwrap().is_dir());
        let blocked = Manifest::parse("[project]\ndata_dir = \"blocked/inner\"\n").unwrap();
        assert_eq!(
            project.data_dir(&blocked).unwrap_err().kind,
            Kind::ProjectInvalid
        );
    }

    #[test]
    fn gitignore_gains_the_data_folder_once_inside_git() {
        let (dir, project) = project(&[("data.toml", ""), (".gitignore", "target")]);
        assert!(
            !project.ignore_in_git("data").unwrap(),
            "outside git nothing changes"
        );
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(project.ignore_in_git("data").unwrap());
        assert!(!project.ignore_in_git("data").unwrap(), "only once");
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "target\n/data/\n"
        );
    }
}
