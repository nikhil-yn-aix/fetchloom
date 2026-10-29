//! the project: the folder holding `data.toml`, its `data.lock`, the data folder, and the small
//! `.fetchloom` folder of facts about what was placed.

pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod state;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use self::lock::Lock;
use self::manifest::Manifest;
use self::state::{Look, State};
use crate::error::{Error, Kind};

const FOLDER: &str = ".fetchloom";

pub(crate) struct Project {
    pub(crate) root: PathBuf,
}

/// the project lock, held while a command changes the project. dropping it lets others in.
pub(crate) struct Held {
    _lock: Option<File>,
}

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
        let _span = tracing::debug_span!("read_manifest").entered();
        let text = fs::read_to_string(self.manifest_path()).map_err(|err| {
            if err.kind() == io::ErrorKind::InvalidData {
                Error::new(Kind::ProjectInvalid, "data.toml is not UTF-8 text")
                    .help("save it as UTF-8, then run the command again")
            } else {
                self.unreadable(manifest::FILE, &err)
            }
        })?;
        let manifest = Manifest::parse(&text)?;
        Ok((text, manifest))
    }

    /// data.lock, or nothing when the project has none yet.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Error> {
        let span = tracing::debug_span!("read_lock", bytes = tracing::field::Empty).entered();
        match fs::read_to_string(self.root.join(lock::FILE)) {
            Ok(text) => {
                span.record("bytes", text.len());
                Lock::parse(&text).map(Some)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::new(
                Kind::ProjectInvalid,
                "data.lock cannot be read: it is not UTF-8 text",
            )
            .help(lock::BROKEN)),
            Err(err) => Err(self.unreadable(lock::FILE, &err)),
        }
    }

    /// the state, and when it was written in nanoseconds since 1970, like the times it records. a
    /// missing or unreadable state is empty.
    pub(crate) fn state(&self) -> (State, u64) {
        let span = tracing::debug_span!("read_state", bytes = tracing::field::Empty).entered();
        let read = File::open(self.root.join(FOLDER).join("state")).and_then(|mut file| {
            let written = Look::of(&file.metadata()?).modified;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            Ok((bytes, written))
        });
        match read {
            Ok((bytes, written)) => {
                span.record("bytes", bytes.len());
                (State::parse(&bytes), written)
            }
            Err(_) => (State::default(), 0),
        }
    }

    /// fetchloom's own folder in the project, which holds no data a person works with.
    pub(crate) fn own_folder(&self) -> PathBuf {
        self.root.join(FOLDER)
    }

    /// replaces a file at the project's root in one step.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        write_atomically(&self.root.join(name), bytes)
            .map_err(|err| Error::io(format_args!("could not write {name}"), &err))
    }

    pub(crate) fn write_state(&self, state: &State) -> Result<(), Error> {
        let _span = tracing::debug_span!("write_state", files = state.files.len()).entered();
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
            Ok(()) => Ok(Hold::Held(Held { _lock: Some(file) })),
            Err(TryLockError::WouldBlock) => Ok(Hold::Busy),
            // a filesystem without locks still works, one fetchloom at a time
            Err(TryLockError::Error(err)) => {
                tracing::debug!(%err, "this filesystem has no file locks");
                Ok(Hold::Held(Held { _lock: None }))
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
        let created = match fs::create_dir(&folder) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
            Err(err) => {
                return Err(Error::io(
                    format_args!("could not create {}", folder.display()),
                    &err,
                ));
            }
        };
        let found = fs::symlink_metadata(&folder)
            .map_err(|err| Error::io(format_args!("could not read {}", folder.display()), &err))?
            .file_type();
        if found.is_symlink() || !found.is_dir() {
            let what = if found.is_symlink() {
                "a link"
            } else {
                "a file"
            };
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "{} is {what}, and fetchloom needs a real folder for project metadata",
                    folder.display()
                ),
            )
            .help("replace .fetchloom with a real folder before running this command"));
        }
        if created {
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
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let own = parent.join(FOLDER);
    let folder = if own.is_dir() { own.as_path() } else { parent };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (temporary, file) = loop {
        let temporary = folder.join(format!(".{name}.{:08x}.tmp", fastrand::u32(..)));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
    };
    // std's rename hands windows a long path in its `\\?\` form. tempfile's persist does not,
    // and fails once a project's metadata passes 260 characters
    let written = fill(file, bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// writes `bytes` into a new file and flushes them to disk.
fn fill(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
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

    #[cfg(windows)]
    #[test]
    fn metadata_links_cannot_redirect_writes() {
        let (_dir, project) = project(&[("data.toml", "")]);
        // FAT32 and exFAT can make neither a hardlink nor a junction.
        if fs::hard_link(project.manifest_path(), project.root.join("link-probe")).is_err() {
            return;
        }
        let outside = tempfile::tempdir().unwrap();
        let link = project.own_folder();
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let held = project.hold();
        let written = project.write_state(&State::default());
        assert!(held.is_err(), "linked .fetchloom accepted a project lock");
        assert!(written.is_err(), "linked .fetchloom accepted project state");
        for error in [held.err().unwrap(), written.unwrap_err()] {
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(
                error.message.contains(&link.display().to_string()),
                "{}",
                error.message
            );
            assert!(error.message.contains("link"), "{}", error.message);
            assert!(error.help.is_some());
        }
        assert_eq!(
            fs::read_dir(outside.path()).unwrap().count(),
            0,
            "metadata writes escaped through .fetchloom"
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

    #[test]
    fn interrupted_metadata_writes_leave_no_staging_file_in_the_root() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        const CHILD: &str = "FETCHLOOM_TEST_METADATA_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let project = Project {
                root: PathBuf::from(root),
            };
            project.write("data.lock", &vec![b'x'; 64 << 20]).unwrap();
            return;
        }
        let (_dir, project) = project(&[("data.lock", "old")]);
        let own = project.inner().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project::tests::interrupted_metadata_writes_leave_no_staging_file_in_the_root",
            ])
            .env(CHILD, &project.root)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let staged_in = |folder: &Path| {
            fs::read_dir(folder).unwrap().flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".data.lock.")
            })
        };
        let started = Instant::now();
        let staged = loop {
            if staged_in(&project.root) || staged_in(&own) {
                break true;
            }
            if child.try_wait().unwrap().is_some() || started.elapsed() > Duration::from_secs(30) {
                break false;
            }
            std::thread::yield_now();
        };
        let _ = child.kill();
        let output = child.wait_with_output().unwrap();
        assert!(
            staged,
            "the write never started: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !staged_in(&project.root),
            "an interrupted metadata write left a temporary in the project root"
        );
        let contents = fs::read(project.root.join("data.lock")).unwrap();
        assert!(
            contents == b"old"
                || (contents.len() == 64 << 20 && contents.iter().all(|byte| *byte == b'x'))
        );
    }

    #[test]
    fn a_lock_can_be_written_before_the_own_folder_exists() {
        let (_dir, project) = project(&[]);
        project.write("data.lock", b"standalone lock").unwrap();
        assert_eq!(
            fs::read(project.root.join("data.lock")).unwrap(),
            b"standalone lock"
        );
        assert!(!project.own_folder().exists());
        assert_eq!(fs::read_dir(&project.root).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn a_deep_project_can_create_and_replace_its_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = dir.path().to_path_buf();
        while root.as_os_str().len() < 300 {
            root.push("a_long_project_folder_name");
        }
        fs::create_dir_all(&root).unwrap();
        let project = Project { root };
        let Hold::Held(_held) = project.hold().unwrap() else {
            panic!("free")
        };
        project.write("data.lock", b"first").unwrap();
        project.write("data.lock", b"second").unwrap();
        project.write_state(&State::default()).unwrap();
        assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"second");
        assert!(project.root.join(FOLDER).join("state").is_file());
    }
}
