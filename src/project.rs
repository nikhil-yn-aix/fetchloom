//! the project: the folder holding `data.toml`, its `data.lock`, the data folder, and the small
//! `.fetchloom` folder of facts about what was placed.

pub(crate) mod git;
pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod state;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use self::lock::Lock;
use self::manifest::Manifest;
use self::state::{Look, State};
use crate::cache::names;
use crate::error::{Error, Kind};

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
    /// another fetchloom is changing this project right now: the command its owner record names,
    /// when it can be read.
    Busy(Option<String>),
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
            // flushed before it gets its name, so a power cut leaves a whole record, which the
            // next command reads and takes over, never an empty one, which only a person can
            // tell from another OS's paused writer
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
                    fs::rename(&temporary, path)?;
                }
            }
            #[cfg(test)]
            tests::owner_published(path);
            Ok(())
        })();
        let removed = match fs::remove_file(&temporary) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        };
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
                // the OS lock keeps out every command of this OS, and the record only tells them
                // who holds it, so a full disk need not stop a command. without OS locks the
                // record is the lock itself
                Err(err) if self.lock.is_some() => {
                    tracing::debug!(%err, "could not write the project owner record");
                    return Ok(Hold::Held(self));
                }
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

/// a project fetchloom could not lock, named as the project. a file that tells who holds it but
/// that fetchloom cannot read as its own is named, so a person can delete it once no other
/// fetchloom works there.
fn not_locked(owner: &Path, err: &io::Error) -> Error {
    let error = Error::io("could not lock the project", err);
    if err.kind() == io::ErrorKind::InvalidData {
        return error.help(format!(
            "if no other fetchloom is working in this project, delete {}, then run the command again",
            owner.display()
        ));
    }
    error
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

/// the command the owner record at `path` names, as a waiting command tells it.
fn holder(path: &Path) -> Option<String> {
    let (bytes, _) = read_owner(path).ok()?;
    let owner = serde_json::from_slice::<Owner>(&bytes).ok()?;
    Some(format!("`fetchloom {}` (pid {})", owner.command, owner.pid))
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
                "{} is larger than {}, more than fetchloom ever writes there",
                path.display(),
                crate::units::size(OWNER_CAP)
            ),
        ));
    }
    Ok((bytes, modified))
}

impl Project {
    /// the nearest folder at or above `start` that holds a data.toml, inside the git repository
    /// `start` is in: a data.toml above the repository belongs to another history, so `-C` must
    /// name it. each folder's data.toml is looked for before its `.git`. any entry named data.toml
    /// marks the project, so one that is no file, as a folder, is told as such when read rather
    /// than passed over for a folder above it or no project at all.
    pub(crate) fn find(start: &Path) -> Option<Project> {
        for folder in start.ancestors() {
            if fs::symlink_metadata(folder.join(manifest::FILE)).is_ok() {
                return Some(Project {
                    root: folder.to_path_buf(),
                });
            }
            if is_repository(folder) {
                return None;
            }
        }
        None
    }

    /// where the git repository `start` is in begins: the nearest folder at or above it holding
    /// `.git`.
    pub(crate) fn repository(start: &Path) -> Option<&Path> {
        start.ancestors().find(|folder| is_repository(folder))
    }

    pub(crate) fn manifest_path(&self) -> PathBuf {
        self.root.join(manifest::FILE)
    }

    pub(crate) fn manifest(&self) -> Result<(String, Manifest), Error> {
        let _span = tracing::debug_span!("read_manifest").entered();
        let path = self.manifest_path();
        // a folder or a pipe there would fail the read, or never end it
        if let Ok(found) = fs::metadata(&path)
            && !found.is_file()
        {
            let what = if found.is_dir() {
                "a folder"
            } else {
                "a special file"
            };
            return Err(Error::new(
                Kind::LocalIo,
                format!("{} is {what}, not a file", path.display()),
            )
            .help("move it aside, then put the project's data.toml file there, or run the command in another folder"));
        }
        let text = fs::read_to_string(&path).map_err(|err| {
            if err.kind() == io::ErrorKind::InvalidData {
                Error::new(Kind::ProjectInvalid, "data.toml is not UTF-8 text")
                    .help("save it as UTF-8, then run the command again")
            } else {
                self.unreadable(manifest::FILE, &err)
            }
        })?;
        let manifest = Manifest::parse(&text)
            .map_err(|error| unmerged(manifest::FILE, &text).unwrap_or(error))?;
        Ok((text, manifest))
    }

    /// data.lock, or nothing when the project has none yet.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Error> {
        Ok(self.lock_and_text()?.map(|(lock, _)| lock))
    }

    /// data.lock with the text it was read from, or nothing when the project has none yet.
    pub(crate) fn lock_and_text(&self) -> Result<Option<(Lock, String)>, Error> {
        self.lock_by(|text| Lock::read(text, &self.root))
    }

    /// [`Project::lock_and_text`] of only the datasets `keep` accepts by name, reading only their
    /// lists.
    pub(crate) fn lock_only(
        &self,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<(Lock, String)>, Error> {
        self.lock_by(|text| Lock::read_only(text, &self.root, keep))
    }

    fn lock_by(
        &self,
        read: impl FnOnce(&str) -> Result<Lock, Error>,
    ) -> Result<Option<(Lock, String)>, Error> {
        let span = tracing::debug_span!("read_lock", bytes = tracing::field::Empty).entered();
        match fs::read_to_string(self.root.join(lock::FILE)) {
            Ok(text) => {
                span.record("bytes", text.len());
                let lock =
                    read(&text).map_err(|error| unmerged(lock::FILE, &text).unwrap_or(error))?;
                Ok(Some((lock, text)))
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
        self.state_by(State::parse)
    }

    /// [`Project::state`] with only the records of the paths `keep` accepts.
    pub(crate) fn state_only(&self, keep: &dyn Fn(&str) -> bool) -> (State, u64) {
        self.state_by(|bytes| State::parse_only(bytes, keep))
    }

    fn state_by(&self, parse: impl FnOnce(&[u8]) -> State) -> (State, u64) {
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
                (parse(&bytes), written)
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
            .map_err(|err| self.refused(format_args!("could not write {name}"), &err))
    }

    /// writes the parts of the file lists `lock` keeps beside data.lock that are not there yet,
    /// and returns their paths below the project. a part's name changes with its list, so one
    /// already there holds the same list.
    pub(crate) fn write_lists(&self, lock: &Lock) -> Result<Vec<String>, Error> {
        let parts = lock.lists();
        if parts.is_empty() {
            return Ok(Vec::new());
        }
        let folder = self.root.join(lock::lists::FOLDER);
        fs::create_dir_all(&folder).map_err(|err| {
            self.refused(format_args!("could not make {}", lock::lists::FOLDER), &err)
        })?;
        let mut written = Vec::new();
        for (part, text) in parts {
            let path = self
                .root
                .join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
            if path.is_file() {
                continue;
            }
            let bytes = lock::lists::compress(&text)
                .map_err(|err| Error::io(format_args!("could not compress {part}"), &err))?;
            self.write(&part, &bytes)?;
            written.push(part);
        }
        Ok(written)
    }

    /// removes the parts of file lists `lock` no longer names, and their folder once it is
    /// empty. other files there stay. a part that cannot be removed is left for the next write.
    pub(crate) fn sweep_lists(&self, lock: &Lock) {
        let folder = self.root.join(lock::lists::FOLDER);
        let Ok(entries) = fs::read_dir(&folder) else {
            return;
        };
        let named: std::collections::HashSet<String> =
            lock.lists().into_iter().map(|(part, _)| part).collect();
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if lock::lists::is_part(&file)
                && !named.contains(&format!("{}/{file}", lock::lists::FOLDER))
                && let Err(err) = fs::remove_file(entry.path())
            {
                tracing::debug!(%err, file, "could not remove a file list data.lock no longer names");
            }
        }
        // only an empty folder goes
        let _ = fs::remove_dir(&folder);
    }

    /// fails, naming the folder, when the project folder is read-only for this user, so a
    /// command finds out before it does work it could not record. elsewhere than unix the first
    /// write finds out instead.
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unnecessary_wraps,
            clippy::unused_self,
            reason = "unix asks the folder; other platforms learn it from the first write"
        )
    )]
    pub(crate) fn check_writable(&self) -> Result<(), Error> {
        #[cfg(unix)]
        if let Err(err) = rustix::fs::access(&self.root, rustix::fs::Access::WRITE_OK) {
            let err = io::Error::from(err);
            if read_only(&self.root, &err) {
                return Err(Error::new(
                    Kind::LocalIo,
                    format!(
                        "the project folder {} is read-only, so nothing in it can change",
                        self.root.display()
                    ),
                )
                .help("make that folder writable for you, then run the command again"));
            }
        }
        Ok(())
    }

    /// the error for `doing`, which failed with `err`: one naming the project folder when that
    /// folder is read-only for this user, as that is what to fix.
    fn refused(&self, doing: impl std::fmt::Display, err: &io::Error) -> Error {
        if read_only(&self.root, err) {
            return Error::new(
                Kind::LocalIo,
                format!(
                    "{doing}: the project folder {} is read-only",
                    self.root.display()
                ),
            )
            .help("make that folder writable for you, then run the command again");
        }
        Error::io(doing, err)
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

    pub(crate) fn write_state(&self, state: &State) -> Result<(), Error> {
        let _span = tracing::debug_span!("write_state", files = state.files.len()).entered();
        let folder = self.inner()?;
        let path = folder.join("state");
        write_atomically(&path, &state.render()).map_err(|err| {
            Error::io(
                format_args!(
                    "could not save the list of placed files in {}",
                    path.display()
                ),
                &err,
            )
        })
    }

    /// takes the project lock and owner record for `command`, unless another fetchloom holds it.
    pub(crate) fn hold(&self, command: &str) -> Result<Hold, Error> {
        self.hold_at(command, SystemTime::now())
    }

    fn hold_at(&self, command: &str, now: SystemTime) -> Result<Hold, Error> {
        self.hold_with(command, now, names)
    }

    /// [`Project::hold_at`], with `named` telling whether `.fetchloom/lock` still names the file
    /// locked. a lock that loses its name is opened again, a few times at most, since only a
    /// command that set out to start the project and did not removes it, and only once.
    fn hold_with(
        &self,
        command: &str,
        now: SystemTime,
        named: impl Fn(&Path, &File) -> io::Result<bool>,
    ) -> Result<Hold, Error> {
        const TRIES: usize = 8;
        let opening = |err: io::Error| Error::io("could not lock the project", &err);
        for _ in 0..TRIES {
            let folder = self.inner()?;
            let path = folder.join("lock");
            let file = match crate::cache::open_unfollowed(&path, true) {
                Ok(file) => file,
                // a command that set out to start this project, and did not, removed the folder
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                // fetchloom never makes a link there, and a lock on what one names keeps nobody out
                Err(err) if err.kind() == io::ErrorKind::InvalidInput => {
                    return Err(Error::new(
                        Kind::ProjectInvalid,
                        format!(
                            "{} is a link or a special file, and the project lock must be a plain file",
                            path.display()
                        ),
                    )
                    .help(format!(
                        "remove {}, which fetchloom did not make; the next command makes a plain one",
                        path.display()
                    )));
                }
                Err(err) => return Err(opening(err)),
            };
            let lock = match file.try_lock() {
                // such a command also removes the lock, and a lock on a file with no name keeps
                // nobody out
                Ok(()) if !named(&path, &file).map_err(opening)? => continue,
                Ok(()) => Some(file),
                Err(TryLockError::WouldBlock) => {
                    return Ok(Hold::Busy(holder(&folder.join("owner"))));
                }
                Err(TryLockError::Error(err)) => {
                    tracing::debug!(%err, "this filesystem has no file locks");
                    None
                }
            };
            let owner = folder.join("owner");
            return Held { lock, owner: None }
                .claim(&owner, command, now)
                .map_err(|err| not_locked(&owner, &err));
        }
        Err(Error::new(
            Kind::LocalIo,
            format!(
                "could not lock the project in {}: its lock file kept being removed or replaced",
                self.own_folder().display()
            ),
        )
        .help("check that nothing but fetchloom changes the .fetchloom folder, then run the command again"))
    }

    /// ends `held`, the hold of a command that set out to start this project and did not, so its
    /// folder is as it was. `data`, the data folder it did not find there, goes when it holds only
    /// empty folders. `.fetchloom` goes when `own` says the command made it and it holds only
    /// fetchloom's own records, the state among them only once no data folder is left for it to
    /// describe. a folder holding anything else stays, with all it holds.
    pub(crate) fn unstart(&self, mut held: Held, data: Option<&Path>, own: bool) {
        if self.remove_start(data, own, &mut held.owner) {
            drop(held);
            let _ = fs::remove_dir(self.own_folder());
        }
    }

    /// [`Project::unstart`] for a run that quits at once, whose hold ends with the process.
    pub(crate) fn unstart_quitting(&self, data: Option<&Path>) {
        let mut owner = Some(self.own_folder().join("owner"));
        if self.remove_start(data, true, &mut owner) {
            let _ = fs::remove_dir(self.own_folder());
        }
    }

    /// removes what a command that started this project made, as [`Project::unstart`] says, all
    /// but `.fetchloom` itself, and says whether that is left empty. `owner` is taken only then.
    fn remove_start(&self, data: Option<&Path>, own: bool, owner: &mut Option<PathBuf>) -> bool {
        if let Some(data) = data {
            remove_empty_folders(data);
        }
        if !own {
            return false;
        }
        let folder = self.own_folder();
        let described = data.is_none_or(Path::exists);
        let Ok(entries) = fs::read_dir(&folder) else {
            return false;
        };
        for entry in entries {
            let ours = entry.is_ok_and(|entry| match entry.file_name().to_str() {
                Some(".gitignore" | "lock" | "owner") => true,
                Some("state") => !described,
                _ => false,
            });
            if !ours {
                return false;
            }
        }
        if let Some(owner) = owner.take() {
            let _ = fs::remove_file(owner);
        }
        for name in [".gitignore", "state"] {
            let _ = fs::remove_file(folder.join(name));
        }
        // the lock goes last and while it is still held. a command that waited for it opens its
        // name again, and finds the lock it takes there to be the only one
        let _ = fs::remove_file(folder.join("lock"));
        true
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
                    // data_dir must stay inside the project, so it cannot take the data elsewhere
                    return Err(Error::new(Kind::ProjectInvalid, format!("{} is a link, and fetchloom only places files in real folders", folder.display()))
                        .help("replace the link with a real folder; to keep the data on another disk, move the whole project folder to that disk"));
                }
                Ok(metadata) if !metadata.is_dir() => {
                    return Err(Error::new(
                        Kind::ProjectInvalid,
                        format!(
                            "{} is a file where the data folder should be",
                            folder.display()
                        ),
                    )
                    .help("move that file to keep it, or set data_dir in [project] in data.toml to another folder inside the project"));
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
        if Project::repository(&self.root).is_none() {
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
                return Err(
                    self.refused(format_args!("could not create {}", folder.display()), &err)
                );
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

/// whether `folder` is where a git repository begins: it holds `.git`, as a folder or as the file
/// a submodule or worktree has.
fn is_repository(folder: &Path) -> bool {
    folder.join(".git").exists()
}

/// the error for project file `name` when git left a merge it could not finish in its `text`:
/// both sides between conflict lines at the start of a line, `<<<<<<<`, `|||||||`, `=======`
/// and `>>>>>>>`. none when no such line is there.
fn unmerged(name: &str, text: &str) -> Option<Error> {
    let line = lock::merge::markers::first(text)?;
    Some(
        Error::new(
            Kind::ProjectInvalid,
            format!("{name} has an unfinished git merge conflict"),
        )
        .detail(format!("{name}:{}", line + 1))
        .help(format!(
            "run `{}`, which merges both sides without downloading",
            crate::cli::command("lock")
        )),
    )
}

/// removes `folder` and the folders in it, when none of them holds anything but empty folders,
/// and says whether it did. a link is never followed.
fn remove_empty_folders(folder: &Path) -> bool {
    if !fs::symlink_metadata(folder).is_ok_and(|found| found.is_dir()) {
        return false;
    }
    let Ok(entries) = fs::read_dir(folder) else {
        return false;
    };
    let mut empty = true;
    for entry in entries {
        empty &= entry.is_ok_and(|entry| remove_empty_folders(&entry.path()));
    }
    empty && fs::remove_dir(folder).is_ok()
}

/// whether `err` means `folder` takes no new entries from this user: the folder, or the disk it
/// is on, is read-only. a refused rename into a folder says nothing about which side refused, so
/// the folder is asked. elsewhere than unix only a read-only disk is known for sure.
fn read_only(folder: &Path, err: &io::Error) -> bool {
    match err.kind() {
        io::ErrorKind::ReadOnlyFilesystem => true,
        #[cfg(unix)]
        io::ErrorKind::PermissionDenied => {
            rustix::fs::access(folder, rustix::fs::Access::WRITE_OK).is_err()
        }
        _ => {
            let _ = folder;
            false
        }
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
    #[cfg(test)]
    tests::flushing_file();
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
        static FILE_FLUSHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    pub(super) fn owner_published(path: &Path) {
        if let Some(observe) = OWNER_PUBLISHED.take() {
            observe(path);
        }
    }

    pub(super) fn flushing_file() {
        FILE_FLUSHES.set(FILE_FLUSHES.get() + 1);
    }

    /// a power cut must leave the owner record whole, so the next command can read it and take
    /// it over: its bytes are flushed once, before it gets its name, and its folder is not.
    #[test]
    fn the_owner_record_is_flushed_once_before_it_is_named() {
        let (_dir, project) = project(&[]);
        project.inner().unwrap();
        FILE_FLUSHES.set(0);
        #[cfg(unix)]
        DIRECTORY_SYNCS.with_borrow_mut(Vec::clear);
        OWNER_PUBLISHED.set(Some(|_| {
            assert_eq!(FILE_FLUSHES.get(), 1, "the record got its name unflushed");
        }));
        let held = project.hold("sync");
        OWNER_PUBLISHED.set(None);
        let Hold::Held(held) = held.unwrap() else {
            panic!("free")
        };
        assert!(project.own_folder().join("owner").is_file());
        assert_eq!(FILE_FLUSHES.get(), 1);
        #[cfg(unix)]
        assert_eq!(
            DIRECTORY_SYNCS.with_borrow_mut(std::mem::take),
            Vec::<PathBuf>::new()
        );
        drop(held);
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
    fn the_project_is_the_nearest_folder_with_a_data_toml() {
        let (dir, _) = project(&[("data.toml", ""), ("a/b/c.txt", "")]);
        let found = Project::find(&dir.path().join("a").join("b")).unwrap();
        assert_eq!(found.root, dir.path());
        assert!(Project::find(&tempfile::tempdir().unwrap().path().join("x")).is_none());
    }

    /// data.toml belongs to the git repository that commits it, so the search ends at the first
    /// folder holding `.git`, as a folder or as the file a submodule or worktree has, once that
    /// folder's own data.toml is checked.
    #[test]
    fn a_git_repository_ends_the_search_for_data_toml() {
        let (dir, _) = project(&[
            ("data.toml", ""),
            ("folder/.git/HEAD", ""),
            ("folder/sub/x.txt", ""),
            ("file/.git", "gitdir: ../.git/worktrees/file\n"),
            ("file/sub/x.txt", ""),
            ("own/.git/HEAD", ""),
            ("own/data.toml", ""),
            ("own/sub/x.txt", ""),
        ]);
        for repository in ["folder", "file"] {
            let inner = dir.path().join(repository);
            for start in [inner.clone(), inner.join("sub")] {
                assert!(Project::find(&start).is_none(), "{}", start.display());
            }
            assert_eq!(
                Project::repository(&inner.join("sub")),
                Some(inner.as_path())
            );
        }
        let own = dir.path().join("own");
        assert_eq!(Project::find(&own.join("sub")).unwrap().root, own);
        assert_eq!(Project::find(dir.path()).unwrap().root, dir.path());
    }

    #[test]
    fn one_command_changes_the_project_at_a_time() {
        let (_dir, project) = project(&[("data.toml", "")]);
        let Hold::Held(held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert!(matches!(
            project.hold("sync").unwrap(),
            Hold::Busy(_) | Hold::Owner(_)
        ));
        drop(held);
        assert!(!project.own_folder().join("owner").exists());
        assert_eq!(
            fs::read_to_string(project.root.join(".fetchloom/.gitignore")).unwrap(),
            "*\n"
        );
    }

    /// a command that waits names the one holding the project, so a forgotten terminal is found.
    #[test]
    fn a_waiting_command_learns_which_command_holds_the_project() {
        let (_dir, project) = project(&[("data.toml", "")]);
        let Hold::Held(held) = project.hold("add").unwrap() else {
            panic!("free")
        };
        // without OS locks the owner record itself answers, through `Hold::Owner`
        if let Hold::Busy(holder) = project.hold("sync").unwrap() {
            let expected = format!("`fetchloom add` (pid {})", std::process::id());
            assert_eq!(holder.as_deref(), Some(expected.as_str()));
        }
        drop(held);
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
        let Hold::Owner(message) = project.hold_at("remove", now).unwrap() else {
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
        assert!(!path.exists());
    }

    #[test]
    fn same_os_owner_is_taken_over_under_the_os_lock() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        fs::write(&path, owner_record(std::env::consts::OS)).unwrap();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234);
        let Hold::Held(held) = project.hold_at("remove", now).unwrap() else {
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
        let written = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        for now in [
            written - Duration::from_secs(1),
            written,
            written + Duration::from_secs(10),
        ] {
            let (_dir, project) = project(&[(".fetchloom/owner", "")]);
            let path = project.own_folder().join("owner");
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(written)
                .unwrap();
            assert!(matches!(
                project.hold_at("sync", now).unwrap(),
                Hold::Owner(_)
            ));
            assert_eq!(fs::read(path).unwrap(), b"");
        }
    }

    #[test]
    fn a_nonempty_invalid_owner_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "{")]);
        let path = project.own_folder().join("owner");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let now = modified + std::time::Duration::from_secs(60);
        let Hold::Owner(message) = project.hold_at("sync", now).unwrap() else {
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

    /// a link at `.fetchloom/lock`, which fetchloom never makes, is refused rather than followed,
    /// whether it names a file or nothing: no lock on what it names would keep anyone out.
    #[test]
    fn a_linked_project_lock_is_refused_and_never_followed() {
        let (_dir, project) = project(&[(".fetchloom/elsewhere", "")]);
        let folder = project.own_folder();
        let path = folder.join("lock");
        for target in [folder.join("elsewhere"), folder.join("nothing")] {
            let _ = fs::remove_file(&path);
            #[cfg(unix)]
            let made = std::os::unix::fs::symlink(&target, &path).is_ok();
            // making a symlink on windows needs developer mode or admin
            #[cfg(windows)]
            let made = std::os::windows::fs::symlink_file(&target, &path).is_ok();
            if !made {
                return;
            }
            let (answer, answered) = std::sync::mpsc::channel();
            let holder = Project {
                root: project.root.clone(),
            };
            std::thread::spawn(move || {
                let refused = holder
                    .hold("sync")
                    .err()
                    .map(|error| (error.kind, error.message, error.help));
                let _ = answer.send(refused);
            });
            let (kind, message, help) = answered
                .recv_timeout(Duration::from_secs(60))
                .expect("holding a linked lock never returned")
                .expect("a linked lock was held");
            assert_eq!(kind, Kind::ProjectInvalid);
            assert!(message.contains(&path.display().to_string()), "{message}");
            assert!(help.is_some());
        }
        assert!(!folder.join("nothing").exists());
        assert!(!folder.join("owner").exists());
    }

    /// a lock whose name never seems to hold it, as on a filesystem whose names and open files
    /// disagree, ends the hold with an error rather than trying forever.
    #[test]
    fn a_lock_that_never_keeps_its_name_is_given_up() {
        let (_dir, project) = project(&[]);
        let (answer, answered) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let given_up = project
                .hold_with("sync", SystemTime::now(), |_, _| Ok(false))
                .err()
                .map(|error| (error.kind, error.message));
            let _ = answer.send(given_up);
        });
        let (kind, message) = answered
            .recv_timeout(Duration::from_secs(60))
            .expect("holding a lock that never kept its name never returned")
            .expect("a lock that never kept its name was held");
        assert_eq!(kind, Kind::LocalIo);
        assert!(message.contains("could not lock the project"), "{message}");
    }

    #[test]
    fn a_linked_state_reads_as_none() {
        let (_dir, project) = project(&[]);
        project.write_state(&State::default()).unwrap();
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
        let (_dir, project) = project(&[("data.lock", "old")]);
        let own = project.inner().unwrap();
        let staged_in = |folder: &Path| {
            fs::read_dir(folder).unwrap().flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".data.lock.")
            })
        };
        let path = project.root.join("data.lock");
        let failure = write_atomically_with(&path, |mut file| {
            file.write_all(b"partial")?;
            assert!(staged_in(&own));
            assert!(!staged_in(&project.root));
            Err(io::ErrorKind::Interrupted.into())
        });
        assert_eq!(failure.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(path).unwrap(), b"old");
        assert!(!staged_in(&own));
        assert!(!staged_in(&project.root));
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
        project.write_state(&State::default()).unwrap();
        assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"second");
        assert!(project.root.join(FOLDER).join("state").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_project_folder_is_named_when_a_write_fails() {
        use std::os::unix::fs::PermissionsExt;
        for own in [false, true] {
            let (_dir, project) = project(&[("data.lock", "before")]);
            if own {
                project.inner().unwrap();
            }
            fs::set_permissions(&project.root, fs::Permissions::from_mode(0o555)).unwrap();
            // a user who may write anywhere, such as root, cannot show this
            let writable = fs::write(project.root.join("probe"), b"").is_ok();
            let written = project.write("data.lock", b"after");
            let checked = project.check_writable();
            fs::set_permissions(&project.root, fs::Permissions::from_mode(0o755)).unwrap();
            if writable {
                return;
            }
            let shown = format!("{} is read-only", project.root.display());
            for error in [written.unwrap_err(), checked.unwrap_err()] {
                assert!(error.message.contains(&shown), "{}", error.message);
                assert!(error.help.unwrap().contains("writable"));
            }
            assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"before");
        }
    }

    /// a command that set out to start a project and did not leaves the folder as it found it,
    /// but never removes what it did not make.
    #[test]
    fn a_project_never_started_leaves_its_folder_as_it_was() {
        let (dir, project) = project(&[]);
        let data = project.root.join("data");
        let Hold::Held(held) = project.hold("add").unwrap() else {
            panic!("free")
        };
        fs::create_dir_all(data.join("iris").join("empty")).unwrap();
        project.unstart(held, Some(&data), true);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);

        let Hold::Held(held) = project.hold("add").unwrap() else {
            panic!("free")
        };
        fs::create_dir_all(data.join("iris")).unwrap();
        fs::write(data.join("iris").join("mine.csv"), b"mine").unwrap();
        fs::write(project.own_folder().join("notes"), b"mine").unwrap();
        project.unstart(held, Some(&data), true);
        assert_eq!(
            fs::read(data.join("iris").join("mine.csv")).unwrap(),
            b"mine"
        );
        assert_eq!(
            fs::read(project.own_folder().join("notes")).unwrap(),
            b"mine"
        );
        assert!(
            !project.own_folder().join("owner").exists(),
            "the hold ended all the same"
        );
        assert!(matches!(project.hold("add").unwrap(), Hold::Held(_)));
    }

    /// a lock taken on a file that lost its name keeps nobody out, as when a command that started
    /// no project removed `.fetchloom` while another waited, so the name is opened again.
    #[test]
    fn a_lock_file_that_lost_its_name_is_not_taken() {
        let (_dir, project) = project(&[]);
        let path = project.inner().unwrap().join("lock");
        let waiting = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(names(&path, &waiting).unwrap());
        fs::remove_file(&path).unwrap();
        // fat and exfat on windows keep a removed file's name until its last handle closes, so
        // there no lock can lose its name while it is held
        if path.exists() {
            return;
        }
        assert!(!names(&path, &waiting).unwrap());
        let Hold::Held(held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert!(names(&path, held.lock.as_ref().unwrap()).unwrap());
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
        let lock = project.lock_and_text().unwrap_err();
        assert_eq!(lock.kind, Kind::ProjectInvalid);
        assert!(lock.message.contains("UTF-8"));
        assert_eq!(lock.help.as_deref(), Some(lock::BROKEN));
    }

    /// git leaves both sides of a merge it could not finish in the file, between its conflict
    /// lines. fetchloom names that, and how to finish it, rather than the TOML mistake those
    /// lines make, in either of git's conflict styles and either line ending.
    #[test]
    fn an_unfinished_merge_in_data_toml_or_data_lock_says_how_to_finish_it() {
        let both = "[datasets.core]\nref = \"zenodo:1\"\n<<<<<<< HEAD\n\n[datasets.alpha]\nref = \"zenodo:2\"\n=======\n\n[datasets.beta]\nref = \"zenodo:3\"\n>>>>>>> beta\n";
        let base =
            "version = 1\r\n\r\n<<<<<<< ours\r\n||||||| base\r\n=======\r\n>>>>>>> theirs\r\n";
        let (_dir, project) = project(&[(manifest::FILE, both), (lock::FILE, base)]);
        let help = "run `fetchloom lock`, which merges both sides without downloading";
        let manifest = project.manifest().unwrap_err();
        assert_eq!(manifest.kind, Kind::ProjectInvalid);
        assert_eq!(
            manifest.message,
            "data.toml has an unfinished git merge conflict"
        );
        assert_eq!(manifest.details, ["data.toml:3"]);
        assert_eq!(manifest.help.as_deref(), Some(help));
        let lock = project.lock_and_text().unwrap_err();
        assert_eq!(lock.kind, Kind::ProjectInvalid);
        assert_eq!(
            lock.message,
            "data.lock has an unfinished git merge conflict"
        );
        assert_eq!(lock.details, ["data.lock:3"]);
        assert_eq!(lock.help.as_deref(), Some(help));
        fs::write(project.manifest_path(), "[datasets.x\n<<<<<<<<\n").unwrap();
        let mistake = project.manifest().unwrap_err();
        assert!(
            mistake.message.starts_with("data.toml is not valid TOML"),
            "{}",
            mistake.message
        );
    }

    #[test]
    fn missing_and_unreadable_metadata_are_not_treated_as_parse_errors() {
        let (_dir, project) = project(&[]);
        assert_eq!(project.manifest().unwrap_err().kind, Kind::LocalIo);
        assert_eq!(project.lock().unwrap(), None);
        fs::create_dir(project.root.join(manifest::FILE)).unwrap();
        fs::create_dir(project.root.join(lock::FILE)).unwrap();
        assert_eq!(project.manifest().unwrap_err().kind, Kind::LocalIo);
        assert_eq!(project.lock().unwrap_err().kind, Kind::LocalIo);
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
                adopted: false,
            },
        );
        state.synced = Some(Synced {
            manifest: crate::digest::sha256(b"manifest"),
            lock: crate::digest::sha256(b"lock"),
            complete: true,
        });
        project.write_state(&state).unwrap();
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
        let error = project.write_state(&State::default()).unwrap_err();
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
            project.write_state(&State::default()).unwrap_err().kind,
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
