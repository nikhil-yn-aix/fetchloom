//! the project: the folder holding `data.toml`, its `data.lock`, the data folder, and the small
//! `.fetchloom` folder of facts about what was placed.

pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod state;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use self::lock::Lock;
use self::manifest::Manifest;
use self::state::{Look, State};
use crate::error::{Error, Kind};
use crate::exp::Experiments;

const FOLDER: &str = ".fetchloom";

/// the most an owner record is read for. one is a line of JSON naming a command and a process.
const OWNER_CAP: u64 = 64 << 10;

pub(crate) struct Project {
    pub(crate) root: PathBuf,
}

/// the project lock, held while a command changes the project. dropping it lets others in.
pub(crate) struct Held {
    lock: Option<File>,
    owner: Option<PathBuf>,
}

pub(crate) enum Hold {
    Held(Held),
    /// another fetchloom is changing this project right now.
    Busy,
    /// the owner record blocks this command; tell the user who holds it and how to recover.
    Owner(String),
}

#[derive(Deserialize, Serialize)]
struct Owner {
    os: String,
    pid: u32,
    started: String,
    command: String,
}

impl Held {
    fn publish_owner(&mut self, path: &Path, record: &[u8]) -> io::Result<()> {
        let (temporary, file) = loop {
            let temporary = path.with_file_name(format!(
                "owner.{}.{:016x}.tmp",
                std::process::id(),
                fastrand::u64(..)
            ));
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
        let published = (|| {
            fill(file, record)?;
            // windows and WSL do not share OS locks, so an empty reservation could be
            // reclaimed while its writer is still publishing.
            match fs::hard_link(&temporary, path) {
                Ok(()) => self.owner = Some(path.to_path_buf()),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(err),
                Err(_) => {
                    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
                    self.owner = Some(path.to_path_buf());
                    drop(file);
                    write_atomically(path, record)?;
                }
            }
            #[cfg(test)]
            tests::owner_published(path);
            Ok(())
        })();
        let removed = fs::remove_file(&temporary);
        published.and(removed)
    }

    fn claim(mut self, path: &Path, command: &str, now: SystemTime) -> io::Result<Hold> {
        let owner = Owner {
            os: std::env::consts::OS.to_owned(),
            pid: std::process::id(),
            started: lock::timestamp(now),
            command: command.to_owned(),
        };
        let record = serde_json::to_vec(&owner).map_err(io::Error::other)?;
        loop {
            match self.publish_owner(path, &record) {
                Ok(()) => return Ok(Hold::Held(self)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(err),
            }
            let (bytes, modified) = match read_owner(path) {
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let owner = serde_json::from_slice::<Owner>(&bytes).ok();
            // an empty reservation may belong to a paused writer on another OS.
            let stale = self.lock.is_some()
                && owner
                    .as_ref()
                    .is_some_and(|owner| owner.os == std::env::consts::OS);
            if stale {
                remove_stale_owner(path, &bytes, modified)?;
                continue;
            }
            let who = owner.map_or_else(
                || "another fetchloom".to_owned(),
                |owner| {
                    format!(
                        "`fetchloom {}` ({}, pid {}, started {})",
                        owner.command, owner.os, owner.pid, owner.started
                    )
                },
            );
            return Ok(Hold::Owner(format!(
                "for {who} working in this project; if that process is gone, delete {}",
                path.display()
            )));
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // remove the owner before the OS lock is released with the guard's fields.
        if let Some(path) = &self.owner
            && let Err(err) = fs::remove_file(path)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(%err, "could not remove the project owner");
        }
    }
}

fn remove_stale_owner(path: &Path, bytes: &[u8], modified: SystemTime) -> io::Result<()> {
    let (current, written) = match read_owner(path) {
        Ok(read) => read,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if current.as_slice() != bytes || written != modified {
        return Ok(());
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn read_owner(path: &Path) -> io::Result<(Vec<u8>, SystemTime)> {
    // a cloned or planted owner could be a link to a device or a fifo, which never ends
    let file = crate::cache::read_unfollowed(path)?;
    let modified = file.metadata()?.modified()?;
    let mut bytes = Vec::new();
    file.take(OWNER_CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > OWNER_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} holds more than {}, too much for an owner record",
                path.display(),
                crate::units::size(OWNER_CAP)
            ),
        ));
    }
    Ok((bytes, modified))
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
    pub(crate) fn lock(&self, exp: &Experiments) -> Result<Option<Lock>, Error> {
        Ok(self.lock_and_text(exp)?.map(|(lock, _)| lock))
    }

    /// data.lock with the text it was read from, or nothing when the project has none yet.
    pub(crate) fn lock_and_text(&self, exp: &Experiments) -> Result<Option<(Lock, String)>, Error> {
        let span = tracing::debug_span!("read_lock", bytes = tracing::field::Empty).entered();
        match fs::read_to_string(self.root.join(lock::FILE)) {
            Ok(text) => {
                span.record("bytes", text.len());
                Ok(Some((Lock::parse(&text, exp)?, text)))
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
        // a planted link to a device or a fifo would never end; it reads as no state at all
        let read = crate::cache::read_unfollowed(&self.root.join(FOLDER).join("state")).and_then(
            |mut file| {
                let written = Look::of(&file.metadata()?).modified;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((bytes, written))
            },
        );
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

    /// saves exact metadata before a command changes it, without exposing recovery text in logs.
    /// on unix only the owner may read the new file; elsewhere it inherits the folder's access.
    ///
    /// # Errors
    /// fails before the caller changes metadata if the recovery copy cannot be written and flushed.
    pub(crate) fn backup(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, Error> {
        let folder = self.inner()?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let (path, file) = loop {
            let path = folder.join(format!("{name}.recovery.{:016x}", fastrand::u64(..)));
            match options.open(&path) {
                Ok(file) => break (path, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(Error::io(
                        format_args!("could not save a recovery copy of {name}"),
                        &err,
                    ));
                }
            }
        };
        let flushed = fill(file, bytes).and_then(|()| sync_directory(&folder));
        if let Err(err) = flushed {
            let _ = fs::remove_file(&path);
            return Err(Error::io(
                format_args!("could not save a recovery copy of {name}"),
                &err,
            ));
        }
        Ok(path)
    }

    pub(crate) fn write_state(&self, state: &State, exp: &Experiments) -> Result<(), Error> {
        let _span = tracing::debug_span!("write_state", files = state.files.len()).entered();
        let folder = self.inner()?;
        let path = folder.join("state");
        let written = if exp.on("PROJECT_STREAM_STATE") {
            write_atomically_with(&path, |file| {
                let mut out = BufWriter::with_capacity(256 << 10, file);
                state.write(&mut out).map_err(io::Error::other)?;
                out.flush()?;
                out.get_ref().sync_all()
            })
        } else {
            write_atomically(&path, &state.render())
        };
        written.map_err(|err| Error::io("could not record what was placed", &err))
    }

    /// takes the project lock and owner record for `command`, unless another fetchloom holds it.
    pub(crate) fn hold(&self, command: &str) -> Result<Hold, Error> {
        self.hold_at(command, SystemTime::now())
    }

    fn hold_at(&self, command: &str, now: SystemTime) -> Result<Hold, Error> {
        let folder = self.inner()?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(folder.join("lock"))
            .map_err(|err| Error::io("could not open the project lock", &err))?;
        let lock = match file.try_lock() {
            Ok(()) => Some(file),
            Err(TryLockError::WouldBlock) => return Ok(Hold::Busy),
            Err(TryLockError::Error(err)) => {
                tracing::debug!(%err, "this filesystem has no file locks");
                None
            }
        };
        Held { lock, owner: None }
            .claim(&folder.join("owner"), command, now)
            .map_err(|err| Error::io("could not claim the project owner", &err))
    }

    /// the data folder, made sure to be a real folder inside the project with no link on the way,
    /// so nothing is ever placed or deleted outside it.
    pub(crate) fn data_dir(&self, manifest: &Manifest) -> Result<PathBuf, Error> {
        self.data_folder(manifest, true)
    }

    /// where the data folder is, refused as [`Project::data_dir`] refuses it, but with no missing
    /// folder created, so a plan made from it changes nothing.
    pub(crate) fn planned_data_dir(&self, manifest: &Manifest) -> Result<PathBuf, Error> {
        self.data_folder(manifest, false)
    }

    fn data_folder(&self, manifest: &Manifest, create: bool) -> Result<PathBuf, Error> {
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
                    )
                    .help("move that file to keep it, or set data_dir in [project] in data.toml to another folder"));
                }
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound && !create => {
                    return Ok(self.root.join(&manifest.data_dir));
                }
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
        let Some(updated) = self.ignored_in_git(data_dir)? else {
            return Ok(false);
        };
        self.write(".gitignore", updated.as_bytes())?;
        Ok(true)
    }

    /// the .gitignore that [`Project::ignore_in_git`] would write, or none when it writes nothing.
    pub(crate) fn ignored_in_git(&self, data_dir: &str) -> Result<Option<String>, Error> {
        if !self
            .root
            .ancestors()
            .any(|folder| folder.join(".git").exists())
        {
            return Ok(None);
        }
        let path = self.root.join(".gitignore");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(self.unreadable(".gitignore", &err)),
        };
        // git reads [, *, ? and \ in a rule as a pattern, so a folder named data[1] needs them
        // escaped to be ignored itself
        let literal: String = data_dir
            .chars()
            .flat_map(|c| {
                let escape = matches!(c, '[' | ']' | '*' | '?' | '\\').then_some('\\');
                escape.into_iter().chain([c])
            })
            .collect();
        let entry = format!("/{literal}/");
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
            return Ok(None);
        }
        let mut updated = text;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push_str(&entry);
        updated.push('\n');
        Ok(Some(updated))
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
            sync_directory(&self.root).map_err(|err| {
                Error::io("could not flush the new project metadata folder", &err)
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
    write_atomically_with(path, |file| fill(file, bytes))
}

fn write_atomically_with(
    path: &Path,
    write: impl FnOnce(File) -> io::Result<()>,
) -> io::Result<()> {
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
    let written = write(file)
        .and_then(|()| fs::rename(&temporary, path))
        .and_then(|()| {
            let flushed = sync_directory(parent).and_then(|()| {
                if folder == parent {
                    Ok(())
                } else {
                    sync_directory(folder)
                }
            });
            flushed.map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "{} was replaced, but its directory changes could not be flushed: {err}",
                        path.display()
                    ),
                )
            })
        });
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// flushes directory entries on unix, where flushing a file does not publish its name durably.
/// other platforms keep their existing publication behavior.
///
/// # Errors
/// returns an error when the directory cannot be opened or flushed.
#[cfg_attr(
    not(unix),
    expect(
        clippy::unnecessary_wraps,
        reason = "unix directory flush errors share this platform-independent interface"
    )
)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        #[cfg(test)]
        tests::syncing_directory(path)?;
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// writes `bytes` into a new file and flushes them to disk.
fn fill(mut file: File, bytes: &[u8]) -> io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    thread_local! {
        static OWNER_PUBLISHED: std::cell::Cell<Option<fn(&Path)>> = const {
            std::cell::Cell::new(None)
        };
    }

    pub(super) fn owner_published(path: &Path) {
        if let Some(observe) = OWNER_PUBLISHED.take() {
            observe(path);
        }
    }

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
    fn state_writers_publish_the_same_complete_bytes() {
        let (_dir, project) = project(&[]);
        let mut state = State::default();
        for index in 0..2048 {
            state.files.insert(
                format!("data/é/{index:05}"),
                state::Placed {
                    dataset: "é".to_owned(),
                    sha256: crate::digest::sha256(b"data"),
                    method: crate::place::Method::Copy,
                    look: Look {
                        size: 4,
                        modified: 42,
                    },
                    writable: true,
                    checked: 43,
                },
            );
        }
        let expected = state.render();
        assert!(expected.len() > 256 << 10);
        for exp in [
            Experiments::default(),
            Experiments::default().with("PROJECT_STREAM_STATE", "1"),
        ] {
            project.write_state(&state, &exp).unwrap();
            assert_eq!(
                fs::read(project.own_folder().join("state")).unwrap(),
                expected
            );
            let read = project.state().0;
            assert_eq!(read.files, state.files);
            assert_eq!(read.synced, state.synced);
        }
    }

    #[test]
    fn a_streaming_metadata_failure_keeps_the_previous_file() {
        let (_dir, project) = project(&[("data.lock", "previous")]);
        let path = project.root.join("data.lock");
        let failure = write_atomically_with(&path, |mut file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        });
        assert!(failure.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        let files = fs::read_dir(&project.root)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name(), "data.lock");
    }

    #[test]
    fn the_project_is_the_nearest_folder_with_a_data_toml() {
        let (dir, _) = project(&[("data.toml", ""), ("a/b/c.txt", "")]);
        let found = Project::find(&dir.path().join("a").join("b")).unwrap();
        assert_eq!(found.root, dir.path());
        assert!(Project::find(&tempfile::tempdir().unwrap().path().join("x")).is_none());
    }

    /// `hold_at`, waiting out `Busy`: a test spawning a process at that moment shares the open
    /// lock file until its child runs its program, and a unix file lock lasts as long as any
    /// process holds that file. fetchloom spawns nothing while it holds a project, so only the
    /// tests wait it out.
    fn waiting(project: &Project, command: &str, now: SystemTime) -> Hold {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match project.hold_at(command, now).unwrap() {
                Hold::Busy if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                hold => return hold,
            }
        }
    }

    #[test]
    fn one_command_changes_the_project_at_a_time() {
        let (_dir, project) = project(&[("data.toml", "")]);
        let Hold::Held(held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert!(matches!(project.hold("sync").unwrap(), Hold::Busy));
        drop(held);
        assert!(
            matches!(waiting(&project, "sync", SystemTime::now()), Hold::Held(_)),
            "the lock is free once its holder is dropped"
        );
        assert_eq!(
            fs::read_to_string(project.root.join(".fetchloom/.gitignore")).unwrap(),
            "*\n"
        );
    }

    fn owner_record(os: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "os": os,
            "pid": 42,
            "started": "2026-10-30T12:00:00Z",
            "command": "sync",
        }))
        .unwrap()
    }

    #[test]
    fn claiming_publishes_a_complete_owner_without_temporary_files() {
        let (_dir, project) = project(&[]);
        if !crate::cache::tests::hardlinks(&project.root) {
            return;
        }
        OWNER_PUBLISHED.set(Some(|path| {
            let bytes = fs::read(path).unwrap();
            assert!(
                !bytes.is_empty(),
                "an owner must be complete when it appears"
            );
            let owner: Owner = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(owner.os, std::env::consts::OS);
            assert_eq!(owner.pid, std::process::id());
            assert_eq!(owner.command, "sync");
        }));
        let Hold::Held(held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert!(
            OWNER_PUBLISHED.get().is_none(),
            "publication was not observed"
        );
        let path = project.own_folder().join("owner");
        let record = read_owner(&path).unwrap();
        let competing = Held {
            lock: None,
            owner: None,
        };
        assert!(matches!(
            competing.claim(&path, "remove", SystemTime::now()).unwrap(),
            Hold::Owner(_)
        ));
        assert_eq!(read_owner(&path).unwrap(), record);
        let mut files: Vec<_> = fs::read_dir(project.own_folder())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        assert_eq!(files, [".gitignore", "lock", "owner"]);
        drop(held);
        assert!(!path.exists());
    }

    #[test]
    fn a_stale_owner_replaced_before_removal_is_kept() {
        let os = if cfg!(windows) { "linux" } else { "windows" };
        for (replacement, elapsed) in [
            (owner_record(os), Duration::ZERO),
            (Vec::new(), Duration::from_secs(20)),
        ] {
            let (_dir, project) = project(&[(".fetchloom/owner", "")]);
            let path = project.own_folder().join("owner");
            let (bytes, modified) = read_owner(&path).unwrap();
            write_atomically(&path, &replacement).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified + elapsed)
                .unwrap();
            let fresh = read_owner(&path).unwrap();

            remove_stale_owner(&path, &bytes, modified).unwrap();

            assert_eq!(read_owner(&path).unwrap(), fresh);
        }
    }

    #[test]
    fn another_os_owner_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let os = if cfg!(windows) { "linux" } else { "windows" };
        let record = owner_record(os);
        fs::write(&path, &record).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let now = modified + Duration::from_hours(24);
        let Hold::Owner(message) = waiting(&project, "remove", now) else {
            panic!("another os owns the project")
        };
        assert_eq!(
            message,
            format!(
                "for `fetchloom sync` ({os}, pid 42, started 2026-10-30T12:00:00Z) working in this project; if that process is gone, delete {}",
                path.display()
            )
        );
        assert_eq!(fs::read(&path).unwrap(), record);
        fs::remove_file(&path).unwrap();
        assert!(matches!(project.hold("remove").unwrap(), Hold::Held(_)));
    }

    #[test]
    fn same_os_owner_is_taken_over_under_the_os_lock() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        fs::write(&path, owner_record(std::env::consts::OS)).unwrap();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234);
        let Hold::Held(held) = waiting(&project, "remove", now) else {
            panic!("the os lock proves this owner is stale")
        };
        assert!(held.lock.is_some());
        let owner: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(owner["os"], std::env::consts::OS);
        assert_eq!(owner["pid"], std::process::id());
        assert_eq!(owner["started"], lock::timestamp(now));
        assert_eq!(owner["command"], "remove");
    }

    #[test]
    fn same_os_owner_without_an_os_lock_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let record = owner_record(std::env::consts::OS);
        fs::write(&path, &record).unwrap();
        let held = Held {
            lock: None,
            owner: None,
        };
        assert!(matches!(
            held.claim(&path, "remove", std::time::SystemTime::now())
                .unwrap(),
            Hold::Owner(_)
        ));
        assert_eq!(fs::read(path).unwrap(), record);
    }

    #[test]
    fn an_active_empty_reservation_does_not_expire() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let started = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(started)
            .unwrap();
        let first = Held {
            lock: None,
            owner: Some(path.clone()),
        };
        for lock in [None, Some(File::open(&path).unwrap())] {
            let second = Held { lock, owner: None }
                .claim(&path, "remove", started + Duration::from_secs(12))
                .unwrap();
            assert!(
                matches!(second, Hold::Owner(_)),
                "an active empty reservation was stolen"
            );
            assert_eq!(fs::read(&path).unwrap(), b"");
        }
        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn a_fresh_empty_owner_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        for now in [
            modified - std::time::Duration::from_secs(1),
            modified,
            modified + std::time::Duration::from_secs(10),
        ] {
            assert!(matches!(waiting(&project, "sync", now), Hold::Owner(_)));
            assert_eq!(fs::read(&path).unwrap(), b"");
        }
    }

    #[test]
    fn a_nonempty_invalid_owner_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "{")]);
        let path = project.own_folder().join("owner");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let now = modified + std::time::Duration::from_secs(60);
        let Hold::Owner(message) = waiting(&project, "sync", now) else {
            panic!("an invalid nonempty owner needs manual recovery")
        };
        assert!(message.contains(&path.display().to_string()));
        assert_eq!(fs::read(path).unwrap(), b"{");
    }

    #[test]
    fn an_owner_too_large_to_be_one_is_not_read_whole() {
        let large = "x".repeat(2 << 20);
        let (_dir, project) = project(&[(".fetchloom/owner", &large)]);
        let path = project.own_folder().join("owner");
        let err = read_owner(&path).err().map(|err| err.kind());
        assert_eq!(err, Some(io::ErrorKind::InvalidData));
        assert!(project.hold("sync").is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), large.len() as u64);
    }

    #[test]
    fn a_linked_owner_is_never_followed() {
        let (_dir, project) = project(&[(".fetchloom/elsewhere", "{}")]);
        let target = project.own_folder().join("elsewhere");
        let path = project.own_folder().join("owner");
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &path).is_ok();
        // making a symlink on windows needs developer mode or admin
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &path).is_ok();
        if !made {
            return;
        }
        let err = read_owner(&path).err().map(|err| err.kind());
        assert_eq!(err, Some(io::ErrorKind::InvalidInput));
        assert!(project.hold("sync").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"{}");
    }

    #[test]
    fn a_linked_state_reads_as_none() {
        let (_dir, project) = project(&[]);
        project
            .write_state(&State::default(), &Experiments::default())
            .unwrap();
        let path = project.own_folder().join("state");
        assert_ne!(project.state().1, 0, "a plain state is read");
        let target = project.own_folder().join("elsewhere");
        fs::rename(&path, &target).unwrap();
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &path).is_ok();
        // making a symlink on windows needs developer mode or admin
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &path).is_ok();
        if !made {
            return;
        }
        assert_eq!(project.state().1, 0);
    }

    #[test]
    fn releasing_the_hold_removes_its_owner() {
        let (_dir, project) = project(&[]);
        let path = project.own_folder().join("owner");
        let Hold::Held(held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert!(path.is_file());
        drop(held);
        assert!(!path.exists());
        assert!(matches!(project.hold("sync").unwrap(), Hold::Held(_)));
        assert!(!path.exists());
    }

    #[test]
    fn an_error_releases_the_owner() {
        let (_dir, project) = project(&[]);
        let path = project.inner().unwrap().join("owner");
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1234);
        // a forked test child can retain an OS lock until exec; owner cleanup needs no OS lock.
        let claim = || {
            Held {
                lock: None,
                owner: None,
            }
            .claim(&path, "sync", now)
            .unwrap()
        };
        let fail = || -> Result<(), Error> {
            let Hold::Held(_held) = claim() else {
                panic!("free")
            };
            assert!(path.is_file());
            Err(Error::new(Kind::ProjectInvalid, "invalid project"))
        };
        let result = fail();
        assert_eq!(result.unwrap_err().kind, Kind::ProjectInvalid);
        assert!(!path.exists());
        assert!(matches!(claim(), Hold::Held(_)));
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

        let held = project.hold("sync");
        let written = project.write_state(&State::default(), &Experiments::default());
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
    fn a_data_folder_named_like_a_pattern_is_ignored_as_itself() {
        let (dir, project) = project(&[("data.toml", "")]);
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(project.ignore_in_git("datasets[1]").unwrap());
        assert!(!project.ignore_in_git("datasets[1]").unwrap(), "only once");
        assert_eq!(
            fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
            "/datasets\\[1\\]/\n"
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
        let Hold::Held(_held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        project.write("data.lock", b"first").unwrap();
        project.write("data.lock", b"second").unwrap();
        project
            .write_state(&State::default(), &Experiments::default())
            .unwrap();
        assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"second");
        assert!(project.root.join(FOLDER).join("state").is_file());
    }

    #[test]
    fn invalid_utf8_metadata_has_a_project_error_and_a_recovery_step() {
        let (_dir, project) = project(&[]);
        for name in [manifest::FILE, lock::FILE] {
            fs::write(project.root.join(name), [0xff, 0xfe]).unwrap();
        }
        let manifest = project.manifest().unwrap_err();
        assert_eq!(manifest.kind, Kind::ProjectInvalid);
        assert!(manifest.message.contains("UTF-8"));
        assert!(manifest.help.unwrap().contains("UTF-8"));
        let lock = project.lock_and_text(&Experiments::default()).unwrap_err();
        assert_eq!(lock.kind, Kind::ProjectInvalid);
        assert!(lock.message.contains("UTF-8"));
        assert_eq!(lock.help.as_deref(), Some(lock::BROKEN));
    }

    #[test]
    fn missing_and_unreadable_metadata_are_not_treated_as_parse_errors() {
        let (_dir, project) = project(&[]);
        assert_eq!(project.manifest().unwrap_err().kind, Kind::LocalIo);
        assert_eq!(project.lock(&Experiments::default()).unwrap(), None);
        fs::create_dir(project.root.join(manifest::FILE)).unwrap();
        fs::create_dir(project.root.join(lock::FILE)).unwrap();
        assert_eq!(project.manifest().unwrap_err().kind, Kind::LocalIo);
        assert_eq!(
            project.lock(&Experiments::default()).unwrap_err().kind,
            Kind::LocalIo
        );
    }

    #[test]
    fn state_round_trips_and_a_broken_state_does_not_vouch_for_files() {
        use crate::place::Method;
        use crate::project::state::{Placed, Synced};

        let (_dir, project) = project(&[]);
        let (empty, written) = project.state();
        assert_eq!(empty.files.len(), 0);
        assert_eq!(written, 0);
        let mut state = State::default();
        state.files.insert(
            "data/set/a".to_owned(),
            Placed {
                dataset: "set".to_owned(),
                sha256: crate::digest::sha256(b"a"),
                method: Method::Copy,
                look: Look {
                    size: 1,
                    modified: 42,
                },
                writable: true,
                checked: 43,
            },
        );
        state.synced = Some(Synced {
            manifest: crate::digest::sha256(b"manifest"),
            lock: crate::digest::sha256(b"lock"),
            complete: true,
        });
        project
            .write_state(&state, &Experiments::default())
            .unwrap();
        let (read, written) = project.state();
        assert_eq!(read.files, state.files);
        assert_eq!(read.synced, state.synced);
        let path = project.own_folder().join("state");
        assert_eq!(written, Look::of(&fs::metadata(&path).unwrap()).modified);
        fs::write(&path, b"broken state").unwrap();
        let (read, _) = project.state();
        assert_eq!(read.files.len(), 0);
        assert_eq!(read.synced, None);
    }

    #[test]
    fn files_blocking_metadata_or_data_folders_are_preserved() {
        let (_dir, project) = project(&[
            (".fetchloom", "keep metadata blocker"),
            ("data", "keep data blocker"),
        ]);
        let error = project
            .write_state(&State::default(), &Experiments::default())
            .unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(error.help.unwrap().contains("real folder"));
        let error = project.data_dir(&Manifest::parse("").unwrap()).unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(error.help.unwrap().contains("move that file"));
        assert_eq!(
            fs::read(project.own_folder()).unwrap(),
            b"keep metadata blocker"
        );
        assert_eq!(
            fs::read(project.root.join("data")).unwrap(),
            b"keep data blocker"
        );
    }

    #[test]
    fn failed_atomic_replacement_leaves_the_destination_and_no_staging_files() {
        let (_dir, project) = project(&[("data.lock/keep", "untouched")]);
        let error = project.write("data.lock", b"replacement").unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(error.message.contains("data.lock"));
        assert_eq!(
            fs::read(project.root.join("data.lock/keep")).unwrap(),
            b"untouched"
        );
        assert_eq!(fs::read_dir(&project.root).unwrap().count(), 1);
    }

    #[test]
    fn stale_owner_cleanup_is_idempotent_and_reports_unreadable_owners() {
        let (_dir, project) = project(&[(".fetchloom/owner", "stale")]);
        let path = project.own_folder().join("owner");
        let (bytes, modified) = read_owner(&path).unwrap();
        remove_stale_owner(&path, &bytes, modified).unwrap();
        assert!(!path.exists());
        remove_stale_owner(&path, &bytes, modified).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"not an owner").unwrap();
        assert!(remove_stale_owner(&path, &bytes, modified).is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"not an owner");
    }

    #[test]
    fn gitignore_recognizes_equivalent_entries_in_nested_projects() {
        for entry in ["/inputs/raw/", "inputs/raw/", "/inputs/raw", "inputs/raw"] {
            let (dir, _) = project(&[(".git", "gitdir: elsewhere"), ("child/data.toml", "")]);
            let project = Project {
                root: dir.path().join("child"),
            };
            let text = format!("# existing\n  {entry}  \n");
            fs::write(project.root.join(".gitignore"), &text).unwrap();
            assert!(!project.ignore_in_git("inputs/raw").unwrap());
            assert_eq!(
                fs::read_to_string(project.root.join(".gitignore")).unwrap(),
                text
            );
        }
        let (dir, _) = project(&[(".git", "gitdir: elsewhere"), ("child/data.toml", "")]);
        let project = Project {
            root: dir.path().join("child"),
        };
        assert!(project.ignore_in_git("inputs/raw").unwrap());
        assert_eq!(
            fs::read(project.root.join(".gitignore")).unwrap(),
            b"/inputs/raw/\n"
        );
        fs::write(project.root.join(".gitignore"), [0xff]).unwrap();
        assert_eq!(
            project.ignore_in_git("inputs/raw").unwrap_err().kind,
            Kind::LocalIo
        );
        assert_eq!(fs::read(project.root.join(".gitignore")).unwrap(), [0xff]);
    }

    #[cfg(unix)]
    #[test]
    fn data_and_metadata_symlinks_never_create_files_outside_the_project() {
        use std::os::unix::fs::symlink;

        let (dir, project) = project(&[]);
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, project.root.join("data")).unwrap();
        symlink(&outside, project.own_folder()).unwrap();
        let manifest = Manifest::parse("[project]\ndata_dir = 'data/child'\n").unwrap();
        assert_eq!(
            project.data_dir(&manifest).unwrap_err().kind,
            Kind::ProjectInvalid
        );
        assert_eq!(
            project
                .write_state(&State::default(), &Experiments::default())
                .unwrap_err()
                .kind,
            Kind::ProjectInvalid
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    thread_local! {
        static DIRECTORY_SYNCS: std::cell::RefCell<Vec<PathBuf>> = const {
            std::cell::RefCell::new(Vec::new())
        };
        static DIRECTORY_SYNC_ERROR: std::cell::Cell<Option<io::ErrorKind>> = const {
            std::cell::Cell::new(None)
        };
    }

    #[cfg(unix)]
    pub(super) fn syncing_directory(path: &Path) -> io::Result<()> {
        DIRECTORY_SYNCS.with_borrow_mut(|synced| synced.push(path.to_path_buf()));
        match DIRECTORY_SYNC_ERROR.take() {
            Some(kind) => Err(kind.into()),
            None => Ok(()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_replacements_flush_both_changed_directories() {
        for own in [false, true] {
            let (_dir, project) = project(&[("data.lock", "before")]);
            if own {
                fs::create_dir(project.own_folder()).unwrap();
            }
            DIRECTORY_SYNCS.with_borrow_mut(Vec::clear);
            project.write("data.lock", b"after").unwrap();
            let mut expected = vec![project.root.clone()];
            if own {
                expected.push(project.own_folder());
            }
            assert_eq!(DIRECTORY_SYNCS.with_borrow_mut(std::mem::take), expected);
            assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"after");
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_flush_failure_reports_the_published_file() {
        let (_dir, project) = project(&[("data.lock", "before")]);
        DIRECTORY_SYNC_ERROR.set(Some(io::ErrorKind::Other));
        let result = project.write("data.lock", b"after");
        let unconsumed = DIRECTORY_SYNC_ERROR.take();
        assert!(
            unconsumed.is_none(),
            "the containing directory was not flushed"
        );
        let error = result.unwrap_err();
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(error.message.contains("was replaced"), "{error}");
        assert!(error.message.contains("flush"), "{error}");
        assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"after");
        assert_eq!(fs::read_dir(&project.root).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_directory_creation_flushes_its_parent() {
        let (_dir, project) = project(&[]);
        DIRECTORY_SYNCS.with_borrow_mut(Vec::clear);
        project.inner().unwrap();
        assert_eq!(
            DIRECTORY_SYNCS.with_borrow_mut(std::mem::take),
            [project.own_folder(), project.root.clone()]
        );
    }
}
