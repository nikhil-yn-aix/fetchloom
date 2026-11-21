//! the project: the folder holding `data.toml`, its `data.lock`, the data folder, and the small
//! `.fetchloom` folder of facts about what was placed.

pub(crate) mod git;
pub(crate) mod lock;
pub(crate) mod manifest;
pub(crate) mod overlap;
pub(crate) mod state;

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use self::lock::Lock;
use self::manifest::Manifest;
use self::state::{Folder, Look, State, Synced, Untold};
use crate::cache::names;
use crate::error::{Error, Kind};
use crate::volume::Volume;
use crate::volume::stall::naming;

const FOLDER: &str = ".fetchloom";

/// the most an owner record is read for. one is a line of JSON naming a command and a process.
const OWNER_CAP: u64 = 64 << 10;

/// the most of a DESCRIPTION file read to find its `Package:` field, far past any real one.
const DESCRIPTION_READ: u64 = 1 << 20;

/// the most of a .gitignore fetchloom reads to look for a line hiding a folder, many times what a
/// person writes.
const GITIGNORE_CAP: u64 = 1 << 20;
/// the most bytes of data.lock a read takes, far past what fetchloom writes: a planted file is
/// refused before it fills memory.
const LOCK_MOST: u64 = 1 << 30;
/// the most bytes of data.toml a read takes: far more than any written by hand or by `fetchloom
/// add`, and far less than would take a machine's memory, so a planted one is refused first.
const MANIFEST_MOST: u64 = 256 << 20;
/// in fetchloom's folder, the data.lock a command writes after data.toml, below a [`Pending`]
/// line, from before the first write until the second is done.
const PENDING: &str = "pending-lock";

/// the first line of a [`PENDING`] record: the data.toml the run wrote, by its SHA-256, and the
/// project folder it wrote it in, so a record a clone or a copy brought along finishes nothing.
#[derive(Serialize, Deserialize)]
struct Pending {
    manifest: String,
    folder: Option<Folder>,
}

pub(crate) struct Project {
    pub(crate) root: PathBuf,
    /// the folders its data folder may be a link into on this machine.
    pub(crate) data_links: DataLinks,
    /// the folder an output's capture pins in this run, as the project names it: on a data folder
    /// link's first follow its files are this project's, as the person claims them and a capture
    /// only reads them.
    pub(crate) captured: Option<String>,
    /// where each data folder link led when the state last recorded it, read once a run and kept
    /// as the run writes the state, so following a link reads no state again.
    followed: std::sync::Mutex<Option<BTreeMap<String, String>>>,
    /// the id this run gives a state that names none, as one a copy or a moved root brought.
    fresh: std::sync::OnceLock<String>,
}

/// the project lock, held while a command changes the project. dropping it lets others in.
pub(crate) struct Held {
    lock: Option<File>,
    owner: Option<PathBuf>,
    /// an owner record fetchloom could not read, or one written for another folder, which taking
    /// the hold removed as stale.
    removed: Option<PathBuf>,
    /// the project's folder, which the owner record this hold writes names.
    here: Option<Folder>,
}

pub(crate) enum Hold {
    Held(Held),
    /// another fetchloom is changing this project right now: the command its owner record names,
    /// when it can be read.
    Busy(Option<String>),
    /// the owner record blocks this command; tell the user who holds it and how to recover.
    Owner(String),
    /// another system's owner record, naming `who`, holds the project at `path`, for a folder this
    /// system cannot match with its own: no process on this machine holds it, and one elsewhere,
    /// as across a network share, this machine cannot check, so a wait might never end.
    Unchecked {
        who: String,
        path: PathBuf,
    },
}

#[derive(Deserialize, Serialize)]
struct Owner {
    os: String,
    pid: u32,
    started: String,
    command: String,
    /// the folder the record was written for, so one a clone or a copy carries holds nobody out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<Folder>,
}

/// the commands that hold a project, the only ones an owner record names.
const HOLDING: [&str; 7] = ["add", "drop", "exclude", "lock", "path", "remove", "sync"];

/// the systems fetchloom is built for, which an owner record names besides this one.
const SYSTEMS: [&str; 3] = ["linux", "macos", "windows"];

impl Owner {
    /// the owner record `bytes` hold, when its every word is one fetchloom writes: a clone can
    /// carry a record, which must neither put its own text in a message nor pass for one.
    fn read(bytes: &[u8]) -> Option<Owner> {
        let owner = serde_json::from_slice::<Owner>(bytes).ok()?;
        let os = owner.os == std::env::consts::OS || SYSTEMS.contains(&owner.os.as_str());
        let command = HOLDING.contains(&owner.command.as_str());
        let started = owner.started.len() <= 40
            && owner
                .started
                .bytes()
                .all(|byte| byte.is_ascii_digit() || b"-:.TZ".contains(&byte));
        (os && command && started).then_some(owner)
    }
}

impl Held {
    /// the owner record fetchloom could not read, or one written for another folder, that taking
    /// this hold removed, told once.
    pub(crate) fn removed_unread(&mut self) -> Option<PathBuf> {
        self.removed.take()
    }

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
            // flushed before it gets its name, so a power cut leaves a whole record for the next
            // command to take over, never an empty one, which only a person can tell from another
            // OS's paused writer
            fill(file, record)?;
            // windows and WSL do not share OS locks, so an empty reservation could be
            // reclaimed while its writer is still publishing
            match naming(path, || fs::hard_link(&temporary, path)) {
                Ok(()) => self.owner = Some(path.to_path_buf()),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(err),
                Err(_) => {
                    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
                    self.owner = Some(path.to_path_buf());
                    drop(file);
                    naming(path, || fs::rename(&temporary, path))?;
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
        debug_assert!(
            HOLDING.contains(&command),
            "{command} holds the project, so HOLDING lists it"
        );
        let owner = Owner {
            os: std::env::consts::OS.to_owned(),
            pid: std::process::id(),
            started: crate::units::timestamp(now),
            command: command.to_owned(),
            folder: self.here.clone(),
        };
        let record = serde_json::to_vec(&owner).map_err(io::Error::other)?;
        loop {
            match self.publish_owner(path, &record) {
                Ok(()) => return Ok(Hold::Held(self)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                // with the OS lock held the record only tells others who holds it, so a full disk
                // need not stop a command. without OS locks the record is the lock itself
                Err(err) if self.lock.is_some() => {
                    tracing::debug!(%err, "could not write the project owner record");
                    return Ok(Hold::Held(self));
                }
                Err(err) => return Err(err),
            }
            let (bytes, modified) = match read_owner(path) {
                Ok(read) => read,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                // a link, a special file, or more than any fetchloom writes: no record of one
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData
                    ) =>
                {
                    match fs::remove_file(path) {
                        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                        _ => {}
                    }
                    self.removed = Some(path.to_path_buf());
                    continue;
                }
                Err(err) => return Err(err),
            };
            let owner = Owner::read(&bytes);
            // a record no fetchloom writes, as a clone or a crash can leave, holds nobody out. an
            // empty one is a reservation, which may belong to a paused writer on another OS
            if owner.is_none() && !bytes.is_empty() {
                remove_stale_owner(path, &bytes, modified)?;
                self.removed = Some(path.to_path_buf());
                continue;
            }
            // a record written for another folder, as a clone or a copy carries, or naming none, as
            // one written by hand, does neither
            let written = |owner: &Owner, untold: Untold| {
                let folder = owner
                    .folder
                    .as_ref()
                    .map(|folder| folder.read_on(&owner.os));
                Folder::written_here(folder.as_ref(), self.here.as_ref(), untold)
            };
            let elsewhere = owner
                .as_ref()
                .is_some_and(|owner| !written(owner, Untold::Here));
            if elsewhere {
                remove_stale_owner(path, &bytes, modified)?;
                self.removed = Some(path.to_path_buf());
                continue;
            }
            let stale = self.lock.is_some()
                && owner
                    .as_ref()
                    .is_some_and(|owner| owner.os == std::env::consts::OS);
            if stale {
                remove_stale_owner(path, &bytes, modified)?;
                continue;
            }
            let unchecked = owner.as_ref().is_some_and(|owner| {
                owner.os != std::env::consts::OS && !written(owner, Untold::Elsewhere)
            });
            let who = owner.map_or_else(
                || "another fetchloom".to_owned(),
                |owner| {
                    format!(
                        "`fetchloom {}` ({}, pid {}, started {})",
                        owner.command, owner.os, owner.pid, owner.started
                    )
                },
            );
            if unchecked {
                return Ok(Hold::Unchecked {
                    who,
                    path: path.to_path_buf(),
                });
            }
            return Ok(Hold::Owner(format!(
                "for {who} working in this project; if that process is gone, delete {}",
                path.display()
            )));
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // remove the owner before the OS lock is released with the guard's fields
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

/// the command the owner record at `path` names, as a waiting command tells it.
fn holder(path: &Path) -> Option<String> {
    let (bytes, _) = read_owner(path).ok()?;
    let owner = Owner::read(&bytes)?;
    Some(format!("`fetchloom {}` (pid {})", owner.command, owner.pid))
}

fn read_owner(path: &Path) -> io::Result<(Vec<u8>, SystemTime)> {
    // a cloned or planted owner could be a link to a device or a fifo, which never ends
    let file = crate::volume::read_unfollowed(path)?;
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
    /// the nearest folder at or above `start` holding a data.toml, inside `start`'s git repository,
    /// since a data.toml above it belongs to another history. any entry named data.toml marks the
    /// project, so one that is no file is told as such when read rather than passed over.
    /// the project at `root`, whose data folder may be a link into no folder.
    pub(crate) fn at(root: PathBuf) -> Project {
        Project {
            root,
            data_links: DataLinks::default(),
            captured: None,
            followed: std::sync::Mutex::new(None),
            fresh: std::sync::OnceLock::new(),
        }
    }

    /// the id a state of this project takes when it names none: one for the whole run, so each
    /// marker the run writes and the state it saves name the same, and none from two runs alike.
    pub(crate) fn fresh_id(&self) -> &str {
        self.fresh
            .get_or_init(|| format!("{:032x}", fastrand::u128(..)))
    }

    pub(crate) fn find(start: &Path) -> Option<Project> {
        for folder in start.ancestors() {
            if fs::symlink_metadata(folder.join(manifest::FILE)).is_ok() {
                return Some(Project::at(folder.to_path_buf()));
            }
            if is_repository(folder) {
                return None;
            }
        }
        None
    }

    /// whether the project is an R package: its DESCRIPTION file names one. one that is a link or
    /// a special file, as a clone can hold to a fifo or a terminal a read would wait on, makes none.
    pub(crate) fn is_r_package(&self) -> bool {
        let mut text = Vec::new();
        crate::volume::read_unfollowed(&self.root.join("DESCRIPTION"))
            .and_then(|file| file.take(DESCRIPTION_READ).read_to_end(&mut text))
            .is_ok()
            && String::from_utf8_lossy(&text)
                .lines()
                .any(|line| line.starts_with("Package:"))
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
        let text = self.manifest_text()?;
        let manifest = Project::parse_manifest(&text)?;
        Ok((text, manifest))
    }

    /// data.toml's text, not yet parsed, read as [`open_own`] opens it and up to
    /// [`MANIFEST_MOST`] bytes.
    pub(crate) fn manifest_text(&self) -> Result<String, Error> {
        let help = "move it aside, then put the project's data.toml file there, or run the command in another folder";
        let (file, found) = open_own(&self.manifest_path())
            .map_err(|err| self.refusal(manifest::FILE, &err, help))?;
        if found.len() > MANIFEST_MOST {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "data.toml is {}, more than the {} fetchloom reads of one",
                    crate::units::size(found.len()),
                    crate::units::size(MANIFEST_MOST)
                ),
            )
            .help(help));
        }
        let mut text = String::with_capacity(usize::try_from(found.len()).unwrap_or_default());
        match file.take(MANIFEST_MOST).read_to_string(&mut text) {
            Ok(_) => Ok(text),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::new(
                Kind::ProjectInvalid,
                "data.toml is not UTF-8 text",
            )
            .help("save it as UTF-8, then run the command again")),
            Err(err) => Err(self.unreadable(manifest::FILE, &err)),
        }
    }

    /// [`Manifest::parse`] of data.toml's `text`, naming an unfinished merge in place of the
    /// parser's error. in a container, the read is refused before it begins when it would
    /// overrun the room, and what the manifest holds after is counted against every later read of
    /// data.lock.
    pub(crate) fn parse_manifest(text: &str) -> Result<Manifest, Error> {
        let _span = tracing::debug_span!("read_manifest").entered();
        let weighed = lock::limited();
        let manifest = Manifest::parse_weighed(text, weighed)
            .map_err(|error| unmerged(manifest::FILE, text).unwrap_or(error))?;
        if weighed {
            lock::hold(manifest::held(text, &manifest));
        }
        Ok(manifest)
    }

    /// data.lock, or nothing when the project has none yet.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Error> {
        Ok(self.lock_and_text()?.map(|(lock, _)| lock))
    }

    /// data.lock with the text it was read from, or nothing when the project has none yet. a
    /// dataset whose file list is not beside it waits, as [`Lock::read_waiting`] keeps it.
    pub(crate) fn lock_and_text(&self) -> Result<Option<(Lock, String)>, Error> {
        self.lock_only(&|_| true)
    }

    /// [`Project::lock_and_text`] of only the datasets `keep` accepts by name, reading only their
    /// lists.
    pub(crate) fn lock_only(
        &self,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<(Lock, String)>, Error> {
        self.lock_by(|text| Lock::read_waiting(text, &self.root, keep))
    }

    /// [`Project::lock_only`] with of dataset `name` only the files whose paths `row` takes, as
    /// [`Lock::read_rows`] reads it: none when that would not answer as a whole read does, or
    /// data.lock does not read, which [`Project::lock_only`] then says why.
    pub(crate) fn lock_rows(
        &self,
        keep: &dyn Fn(&str) -> bool,
        name: &str,
        row: &dyn Fn(&str) -> bool,
    ) -> Option<Lock> {
        let _span = tracing::debug_span!("read_lock").entered();
        let text = self.lock_text().ok()??;
        Lock::read_rows(&text, &self.root, keep, name, row)
    }

    fn lock_by(
        &self,
        read: impl Fn(&str) -> Result<Lock, Error>,
    ) -> Result<Option<(Lock, String)>, Error> {
        let span = tracing::debug_span!("read_lock", bytes = tracing::field::Empty).entered();
        let Some(text) = self.lock_text()? else {
            return Ok(None);
        };
        self.lock_of(text, read, &span).map(Some)
    }

    /// [`Project::lock_and_text`] of data.lock's `text`, read already.
    pub(crate) fn lock_from(&self, text: String) -> Result<(Lock, String), Error> {
        let span = tracing::debug_span!("read_lock", bytes = tracing::field::Empty).entered();
        self.lock_of(
            text,
            |text| Lock::read_waiting(text, &self.root, &|_| true),
            &span,
        )
    }

    fn lock_of(
        &self,
        mut text: String,
        read: impl Fn(&str) -> Result<Lock, Error>,
        span: &tracing::span::EnteredSpan,
    ) -> Result<(Lock, String), Error> {
        #[cfg(debug_assertions)]
        crate::stop::hold("FETCHLOOM_TEST_HOLD_LOCK_READ");
        let mut lock = read(&text);
        // a reader holds nothing, and a writer replaces data.lock before it sweeps the lists the
        // old one named: a list gone from under the text read first is looked for again under
        // the text that replaced it
        let missed = lock.as_ref().map_or(true, |lock| !lock.waiting.is_empty());
        if missed
            && let Ok(Some(now)) = self.lock_text()
            && now != text
        {
            text = now;
            lock = read(&text);
        }
        span.record("bytes", text.len());
        let lock = lock.map_err(|error| unmerged(lock::FILE, &text).unwrap_or(error))?;
        Ok((lock, text))
    }

    /// data.lock's text, or none when the project has none. it is opened once, as [`open_own`]
    /// opens it, and refused before a byte is read when it is a link or no plain file, or larger
    /// than [`LOCK_MOST`] or the room a container leaves: gc reads every registered project's, so
    /// a planted one must not hang or fill another person's run.
    pub(crate) fn lock_text(&self) -> Result<Option<String>, Error> {
        let help = format!(
            "move it aside, then restore data.lock from git, or run `{}`, which resolves every dataset again",
            crate::suggested::command("lock")
        );
        let (file, found) = match open_own(&self.root.join(lock::FILE)) {
            Ok(opened) => opened,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(self.refusal(lock::FILE, &err, &help)),
        };
        if found.len() > LOCK_MOST {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "data.lock is {}, more than the {} fetchloom reads of one",
                    crate::units::size(found.len()),
                    crate::units::size(LOCK_MOST)
                ),
            )
            .help(lock::recovery()));
        }
        lock::fits_file(found.len())?;
        // sized up front, as fs::read_to_string sizes it, so a big data.lock is copied once
        let mut text = String::with_capacity(usize::try_from(found.len()).unwrap_or_default());
        match file.take(LOCK_MOST).read_to_string(&mut text) {
            Ok(_) => Ok(Some(text)),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::new(
                Kind::ProjectInvalid,
                "data.lock cannot be read: it is not UTF-8 text",
            )
            .help(lock::recovery())),
            Err(err) => Err(self.unreadable(lock::FILE, &err)),
        }
    }

    /// the state, and when it was written in nanoseconds since 1970, like the times it records. a
    /// missing or unreadable state is empty.
    pub(crate) fn state(&self) -> (State, u64) {
        self.read_opened(self.open_state(), State::read_whole)
    }

    /// [`Project::state`] with only the records at or below `folders` that `keep` accepts. `keep`
    /// must accept no path outside `folders`, since a read passes over parts that cannot hold one.
    pub(crate) fn state_within(
        &self,
        folders: &[String],
        keep: &dyn Fn(&str) -> bool,
    ) -> (State, u64) {
        self.read_opened(self.open_state(), |file, _| {
            State::read_within(file, folders, keep)
        })
    }

    /// the settled sync the state records, read from what follows its records alone, or none.
    pub(crate) fn synced(&self) -> Option<Synced> {
        self.state_within(&[], &|_| false).0.synced
    }

    /// the datasets `fetchloom drop` freed on this machine, read as [`Project::synced`] reads.
    pub(crate) fn dropped(&self) -> std::collections::BTreeSet<String> {
        self.state_within(&[], &|_| false).0.dropped
    }

    /// the state file and how it looks, opened without following a link: a planted link to a
    /// device or a fifo would never end, and reads as no state at all.
    pub(crate) fn open_state(&self) -> io::Result<(fs::File, Look)> {
        let file = crate::volume::read_unfollowed(&self.root.join(FOLDER).join("state"))?;
        let found = Look::of(&file.metadata()?);
        Ok((file, found))
    }

    /// the state as `read` makes it from the opened file and its length, and when it was written.
    /// a missing or unreadable state is empty, and so is one written for another folder, as a
    /// clone, a copy or a moved root of the project brings it: every file is then checked by its
    /// hash. what it still tells this one comes back in the same read as [`State::copied`], and
    /// only the dataset folders it marked for git stay, since a copy brings their .gitignore files
    /// too: whose a marker is, its id says.
    pub(crate) fn read_opened(
        &self,
        opened: io::Result<(fs::File, Look)>,
        read: impl FnOnce(&mut fs::File, u64) -> io::Result<(State, u64)>,
    ) -> (State, u64) {
        static TOLD: std::sync::Once = std::sync::Once::new();
        let span = tracing::debug_span!("read_state", bytes = tracing::field::Empty).entered();
        let (state, written) = opened
            .and_then(|(mut file, found)| {
                let (state, bytes) = read(&mut file, found.size)?;
                span.record("bytes", bytes);
                Ok((state, found.modified))
            })
            .unwrap_or_default();
        let ours = Folder::written_here(
            state.folder.as_ref(),
            Folder::of(&self.root).as_ref(),
            Untold::Elsewhere,
        );
        if !state.was_read() || ours {
            return (state, written);
        }
        TOLD.call_once(|| {
            tracing::debug!(folder = %self.root.display(), "the state was written for another folder, so it is not used");
        });
        let copied = crate::project::state::Copied {
            id: state.id,
            files: state
                .files
                .into_iter()
                .map(|(path, placed)| (path, placed.dataset))
                .collect(),
            told: state.told,
        };
        let mut kept = State::default();
        kept.markers = state.markers;
        kept.copied = copied;
        (kept, 0)
    }

    /// fetchloom's own folder in the project, which holds no data a person works with.
    pub(crate) fn own_folder(&self) -> PathBuf {
        self.root.join(FOLDER)
    }

    /// replaces a file at the project's root in one step.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        if name == lock::FILE {
            self.keeps_waiting(bytes)?;
        }
        write_atomically(&self.root.join(name), bytes)
            .map_err(|err| self.refused(format_args!("could not write {name}"), &err))
    }

    /// records, before data.toml becomes `manifest`, that data.lock becomes `lock` after it, so a
    /// run cut short between the two writes leaves the next holder of the project what to finish.
    pub(crate) fn write_pending(&self, manifest: &[u8], lock: &str) -> Result<(), Error> {
        let path = self.inner()?.join(PENDING);
        let refused = |err| self.refused(format_args!("could not write {}", path.display()), &err);
        let head = serde_json::to_string(&Pending {
            manifest: crate::digest::sha256(manifest).hex(),
            folder: Folder::of(&self.root),
        })
        .map_err(|err| refused(err.into()))?;
        write_atomically(&path, format!("{head}\n{lock}").as_bytes()).map_err(refused)
    }

    /// forgets the record [`Project::write_pending`] made, once both files are written.
    pub(crate) fn forget_pending(&self) {
        let path = self.own_folder().join(PENDING);
        if let Err(err) = fs::remove_file(&path)
            && err.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(%err, "could not remove the record of a data.lock to write");
        }
    }

    /// writes the data.lock a run cut short between data.toml and data.lock recorded, when
    /// data.toml is the one that run wrote in this folder, and forgets the record either way: a
    /// data.toml it never wrote, one edited since, or a record another folder's run left keeps
    /// data.lock as it is. says whether it wrote data.lock. a holder of the project calls it before
    /// it reads either file.
    pub(crate) fn finish_pending(&self) -> Result<bool, Error> {
        let path = self.own_folder().join(PENDING);
        let record = match read_capped(&path, LOCK_MOST) {
            Ok(record) => record,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                tracing::debug!(%err, "the record of a data.lock to write cannot be read");
                self.forget_pending();
                return Ok(false);
            }
        };
        let here = Folder::of(&self.root);
        let written = record.split_once('\n').filter(|(head, _)| {
            let Ok(head) = serde_json::from_str::<Pending>(head) else {
                return false;
            };
            let ours = Folder::written_here(head.folder.as_ref(), here.as_ref(), Untold::Elsewhere);
            ours && self
                .read_own(manifest::FILE)
                .is_ok_and(|bytes| crate::digest::sha256(&bytes).hex() == head.manifest)
        });
        if let Some((_, lock)) = written {
            self.write(lock::FILE, lock.as_bytes())?;
        }
        self.forget_pending();
        Ok(written.is_some())
    }

    /// refuses data.lock's new text `bytes` when it drops the pin of a dataset that waits for its
    /// file list, one data.lock names and data.lock.d lacks, while data.toml still asks for that
    /// pin, or pins it by another list: only that list, once it is back, says what it pins. every
    /// writer keeps such a pin as it was read, so this stops only a mistake.
    fn keeps_waiting(&self, bytes: &[u8]) -> Result<(), Error> {
        let Some(old) = self.own_text(lock::FILE) else {
            return Ok(());
        };
        let waiting = lock::missing_lists(&old, &self.root);
        if waiting.is_empty() {
            return Ok(());
        }
        let pinned = lock::listed(&String::from_utf8_lossy(bytes));
        let asked = self.manifest().ok().map(|(_, manifest)| manifest.datasets);
        let lost = waiting.iter().find(|waiting| {
            let kept = pinned
                .iter()
                .any(|(name, parts)| *name == waiting.name && parts.contains(&waiting.part));
            !kept
                && asked
                    .as_ref()
                    .is_none_or(|asked| waiting.answers(asked.get(&waiting.name)))
        });
        match lost {
            Some(waiting) => Err(waiting.error()),
            None => Ok(()),
        }
    }

    /// writes the parts of `lock`'s file lists not yet beside data.lock, returning their paths
    /// below the project. a part of a list data.lock on disk names stays as it is; any other file
    /// under a part's name is written over unless it holds that part, as a clone can plant one for
    /// the list a sync will write. refused, even with nothing to write, when data.lock.d is a
    /// link or a file, and before any part is written when a list would hold a path or link
    /// longer than a list keeps.
    pub(crate) fn write_lists(&self, lock: &Lock) -> Result<Vec<String>, Error> {
        let folder = lock::lists::folder(&self.root)?;
        lock.listable()?;
        let parts = lock.hashed_lists();
        if parts.is_empty() {
            return Ok(Vec::new());
        }
        fs::create_dir_all(&folder).map_err(|err| {
            self.refused(format_args!("could not make {}", lock::lists::FOLDER), &err)
        })?;
        let named = self.named_lists();
        let mut written = Vec::new();
        for (part, text, sha256) in parts {
            let path = self
                .root
                .join(part.replace('/', std::path::MAIN_SEPARATOR_STR));
            let there = fs::symlink_metadata(&path).is_ok_and(|found| found.is_file());
            if there && (named.contains(&sha256) || lock::lists::holds(&path, &text)) {
                continue;
            }
            let bytes = lock::lists::compress(&text)
                .map_err(|err| Error::io(format_args!("could not compress {part}"), &err))?;
            self.write(&part, &bytes)?;
            written.push(part);
        }
        Ok(written)
    }

    /// the lists data.lock on disk names, by the SHA-256 of each; none when it is no file.
    fn named_lists(&self) -> std::collections::HashSet<crate::digest::Sha256> {
        self.own_text(lock::FILE)
            .map(|text| lock::named_lists(&text))
            .unwrap_or_default()
    }

    /// removes the parts of file lists `lock` no longer names, and any a run left half written,
    /// and their folder once it is empty. other files there stay. a read-only part goes too, on
    /// every disk. a part that cannot be removed is left for the next write, and a data.lock.d
    /// that is a link or a file is left alone.
    pub(crate) fn sweep_lists(&self, lock: &Lock) {
        let Ok(folder) = lock::lists::folder(&self.root) else {
            return;
        };
        let Ok(entries) = fs::read_dir(&folder) else {
            return;
        };
        let volume = Volume::new(self.own_folder());
        // the parts there of a list that waits for another stay with it
        let named: std::collections::HashSet<String> = lock
            .lists()
            .into_iter()
            .map(|(part, _)| part)
            .chain(
                lock.still_waiting()
                    .flat_map(|waiting| waiting.parts().iter().cloned()),
            )
            .collect();
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let unnamed = lock::lists::is_part(&file)
                && !named.contains(&format!("{}/{file}", lock::lists::FOLDER));
            if (unnamed || lock::lists::is_unfinished(&file))
                && let Err(err) = volume.remove(&entry.path())
            {
                tracing::debug!(%err, file, "could not remove a file list data.lock no longer names");
            }
        }
        // only an empty folder goes
        let _ = fs::remove_dir(&folder);
    }

    /// fails, naming the folder, when the project folder is read-only for this user, so a command
    /// finds out before work it could not record. elsewhere than unix the first write finds out.
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
    /// the project, which fetchloom could not lock at its owner record `owner`, named with it. an
    /// owner record fetchloom cannot read as its own is told, so a person can delete it once no
    /// other fetchloom works there.
    fn not_locked(&self, owner: &Path, err: &io::Error) -> Error {
        let error = self.refused(
            format_args!("could not lock the project with {}", owner.display()),
            err,
        );
        if err.kind() == io::ErrorKind::InvalidData {
            return error.help(format!(
                "if no other fetchloom is working in this project, delete {}, then run the command again",
                owner.display()
            ));
        }
        error
    }

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

    /// saves exact metadata before a command changes it, never in logs, readable on unix by its
    /// owner alone. fails before the caller changes anything if it cannot be written and flushed.
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
        let folder = Folder::of(&self.root);
        write_atomically(&path, &state.render_for(folder.as_ref())).map_err(|err| {
            let error = Error::io(
                format_args!(
                    "could not save the list of placed files in {}",
                    path.display()
                ),
                &err,
            );
            if path.symlink_metadata().is_ok_and(|metadata| metadata.is_dir()) {
                error.help(format!(
                    "remove the folder {}, where fetchloom keeps that list, then run the same command again",
                    path.display()
                ))
            } else {
                error
            }
        })?;
        *self
            .followed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(state.followed.clone());
        Ok(())
    }

    /// takes the project lock and owner record for `command`, unless another fetchloom holds it.
    pub(crate) fn hold(&self, command: &str) -> Result<Hold, Error> {
        self.hold_at(command, SystemTime::now())
    }

    fn hold_at(&self, command: &str, now: SystemTime) -> Result<Hold, Error> {
        self.hold_with(command, now, names)
    }

    /// [`Project::hold_at`], with `named` telling whether `.fetchloom/lock` still names the file
    /// locked. one that lost its name, as [`Project::unstart`] removes it, is opened again.
    fn hold_with(
        &self,
        command: &str,
        now: SystemTime,
        named: impl Fn(&Path, &File) -> io::Result<bool>,
    ) -> Result<Hold, Error> {
        const TRIES: usize = 8;
        // every command that holds the project may write or sweep the lists, so a linked
        // data.lock.d stops it before anything changes
        lock::lists::folder(&self.root)?;
        for _ in 0..TRIES {
            let folder = self.inner()?;
            let path = folder.join("lock");
            let opening = |err: io::Error| {
                self.refused(
                    format_args!("could not lock the project with {}", path.display()),
                    &err,
                )
            };
            let file = match crate::volume::open_unfollowed(&path, true) {
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
            let hold = Held {
                lock,
                owner: None,
                removed: None,
                here: Folder::of(&self.root),
            }
            .claim(&owner, command, now)
            .map_err(|err| self.not_locked(&owner, &err))?;
            if matches!(hold, Hold::Held(_)) {
                self.sweep_staged();
            }
            return Ok(hold);
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

    /// removes what a write killed between staging a file in `.fetchloom` and naming it left
    /// there, read-only as one stopped while naming a read-only file. only a command that holds
    /// the project stages one, so none is being written now.
    fn sweep_staged(&self) {
        let own = self.own_folder();
        let Ok(entries) = fs::read_dir(&own) else {
            return;
        };
        let volume = Volume::new(own);
        for entry in entries.flatten() {
            let staged = entry.file_name().to_str().is_some_and(is_staged)
                && entry.file_type().is_ok_and(|found| found.is_file());
            if staged && let Err(err) = volume.remove(&entry.path()) {
                tracing::debug!(%err, path = %entry.path().display(), "could not remove what a stopped write left");
            }
        }
    }

    /// ends `held`, the hold of a command that set out to start this project and did not, leaving
    /// its folder as it was: `data` goes when it holds only empty folders, and `.fetchloom` when
    /// `own` says the command made it and it holds only fetchloom's records. anything else stays.
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

    /// refuses a data folder `data_dir` that a link leads to, unless [`Project::follow`] follows
    /// it, before anything reads in it, so a placer then opens the folder found there. the link it
    /// followed, if any.
    pub(crate) fn refuse_linked_data_dir(&self, data_dir: &str) -> Result<Option<Followed>, Error> {
        let (folder, followed) = self.link_to(data_dir)?;
        if let Some(link) = &followed {
            crate::place::open_at(&link.at, link.found.clone()).map_err(|err| {
                Error::io(format_args!("could not read {}", link.at.display()), &err)
            })?;
            if folder.is_dir() {
                remember(&folder)?;
            }
        }
        Ok(followed)
    }

    /// the folder `data_dir`, in the project, and the one link on the way to it, as
    /// [`Project::follow`] follows it, opening nothing there yet.
    fn link_to(&self, data_dir: &str) -> Result<(PathBuf, Option<Followed>), Error> {
        let mut folder = self.root.clone();
        let mut path = String::new();
        let mut followed = None;
        for part in data_dir.split('/') {
            folder.push(part);
            path = if path.is_empty() {
                part.to_owned()
            } else {
                format!("{path}/{part}")
            };
            if fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                self.follow(&folder, &path, &mut followed)
                    .map_err(|error| self.cloned_link(&path, error))?;
            }
        }
        Ok((folder, followed))
    }

    /// `error`, refusing the link `path` in the project, with the way to end it for good when git
    /// tracks the link, as a teammate's clone holds one its author committed: it came with the
    /// clone, and deleting it, and committing that, gives a plain data folder. asking git takes a
    /// run of it, which only a refusal makes.
    pub(crate) fn cloned_link(&self, path: &str, error: Error) -> Error {
        let tracked = Project::repository(&self.root).is_some()
            && git::tracked(&self.root, path).is_some_and(|tracked| tracked.contains(path));
        if !tracked {
            return error;
        }
        let delete = crate::suggested::git(
            &self.root,
            &format!("rm -- {}", crate::suggested::shell_word(path)),
        );
        error.detail(format!(
            "the link came with the clone, as git tracks it: deleting it with `{delete}`, and committing that, gives this project a plain data folder"
        ))
    }

    /// where the data folder `data_dir` is when [`Project::follow`] follows the one link on the
    /// way to it: below the folder that link leads to, which the state records at its first
    /// follow. none when no link is on the way. it takes nothing over, so that first follow stays
    /// the survey's to check and record.
    pub(crate) fn followed_folder(&self, data_dir: &str) -> Result<Option<PathBuf>, Error> {
        let (_, followed) = self.link_to(data_dir)?;
        Ok(followed.map(|link| {
            let below = data_dir[link.path.len()..].split('/');
            below
                .filter(|part| !part.is_empty())
                .fold(link.found, |folder, part| folder.join(part))
        }))
    }

    /// refuses a data folder `data_dir`, or an output's folder when `output` says it is one, that
    /// is the cache folder at `cache`, lies in it or holds it, before anything is read or written
    /// there: a dataset's files would pass for the cache's own records, as an unpacked archive's,
    /// or land among them.
    pub(crate) fn refuse_cache_overlap(
        &self,
        data_dir: &str,
        cache: &Path,
        output: bool,
    ) -> Result<(), Error> {
        let folder = native(&self.root, data_dir);
        let shown = cache.display();
        let what = if output {
            "the output folder"
        } else {
            "the data folder"
        };
        let (said, help) = match overlap::overlap(&folder, cache) {
            None => return Ok(()),
            Some(overlap::Overlap::Inside) => (
                format!(
                    "{what} {data_dir} is inside the cache folder {shown}, where a dataset's files would pass for the cache's own"
                ),
                if output {
                    "set the output's at in data.toml to a folder outside the cache, or keep the cache elsewhere with FETCHLOOM_CACHE"
                } else {
                    "set data_dir in [project] in data.toml to a folder outside the cache, such as data, or keep the cache elsewhere with FETCHLOOM_CACHE"
                },
            ),
            Some(overlap::Overlap::Holds) => (
                format!(
                    "the cache folder {shown} is inside {what} {data_dir}, where datasets' files are placed"
                ),
                if output {
                    "keep the cache outside the output's folder: set FETCHLOOM_CACHE, or the cache setting with `fetchloom settings set cache <folder>`, to a folder elsewhere"
                } else {
                    "keep the cache outside the data folder: set FETCHLOOM_CACHE, or the cache setting with `fetchloom settings set cache <folder>`, to a folder elsewhere"
                },
            ),
        };
        Err(Error::new(Kind::ProjectInvalid, said).help(help))
    }

    fn data_folder(&self, manifest: &Manifest, create: bool) -> Result<PathBuf, Error> {
        // the one link on the way is checked whole before any folder is made through it
        let linked = self.refuse_linked_data_dir(&manifest.data_dir)?.is_some();
        let mut folder = self.root.clone();
        for part in manifest.data_dir.split('/') {
            folder.push(part);
            match fs::symlink_metadata(&folder) {
                Ok(metadata) if metadata.file_type().is_symlink() => {}
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
        if linked {
            remember(&folder)?;
        }
        Ok(folder)
    }

    /// follows the link at `folder`, `path` in the project, on the way to the data folder, as a
    /// person on a cluster links `data` to scratch: the first link there, when the folder it leads
    /// to exists and lies in a folder `data_links` lists and in no other project's folder. a link a
    /// cloned project brings could aim placement anywhere, another project's data folder too, so
    /// any other is refused, a second one included, and placing refuses a first follow into a
    /// folder holding what a run could harm. the folder a link leads to is never made: one that is
    /// gone may be on a disk not mounted now.
    fn follow(
        &self,
        folder: &Path,
        path: &str,
        followed: &mut Option<Followed>,
    ) -> Result<(), Error> {
        if let Some(first) = followed {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "{} is a second link on the way to the data folder, and fetchloom follows only the one at {}",
                    folder.display(),
                    first.at.display()
                ),
            )
            .help("replace the second link with a real folder"));
        }
        let found = match fs::canonicalize(folder) {
            Ok(found) => found,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let leads = fs::read_link(folder).map_or_else(
                    |_| folder.to_path_buf(),
                    |to| {
                        folder
                            .parent()
                            .map_or_else(|| to.clone(), |parent| parent.join(&to))
                    },
                );
                return Err(Error::new(
                    Kind::ProjectInvalid,
                    format!(
                        "{} is a link to {}, which does not exist: its disk may be unmounted",
                        folder.display(),
                        crate::machine::plain(&leads).display()
                    ),
                )
                .help("mount the disk that holds it, or point the link at a folder that exists, since fetchloom never makes the folder a link leads to"));
            }
            Err(err) => {
                return Err(Error::io(
                    format_args!("could not follow the link {}", folder.display()),
                    &err,
                ));
            }
        };
        let shown = crate::machine::plain(&found);
        if !found.is_dir() {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "{} is a link to {}, a file where the data folder should be",
                    folder.display(),
                    shown.display()
                ),
            )
            .help("point the link at a folder"));
        }
        let Some(listed) = self
            .data_links
            .folders
            .iter()
            .filter_map(|entry| fs::canonicalize(entry).ok())
            .find(|entry| found.starts_with(entry))
        else {
            return Err(unlisted(folder, &shown, &self.data_links));
        };
        if let Some(other) = self.other_project(&found, &listed) {
            return Err(taken(folder, &shown, &crate::machine::plain(&other)));
        }
        let first = self.first_follow(path, &found);
        *followed = Some(Followed {
            at: folder.to_path_buf(),
            path: path.to_owned(),
            found,
            first,
        });
        Ok(())
    }

    /// whether following the link `path` in the project to `found` is its first follow: the state
    /// records the link leading elsewhere or nowhere.
    fn first_follow(&self, path: &str, found: &Path) -> bool {
        let mut followed = self
            .followed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let recorded =
            followed.get_or_insert_with(|| self.state_within(&[], &|_| false).0.followed);
        recorded.get(path).map(String::as_str) != Some(&*found.to_string_lossy())
    }

    /// the folder of another project that `found`, the folder a data folder link leads to, lies
    /// in, up to `listed`, the listed folder that holds it: one holding data.toml or a .fetchloom
    /// folder, other than this project's own.
    fn other_project(&self, found: &Path, listed: &Path) -> Option<PathBuf> {
        let own = fs::canonicalize(&self.root).ok();
        found
            .ancestors()
            .take_while(|folder| folder.starts_with(listed))
            .find(|folder| {
                own.as_deref() != Some(*folder)
                    && [manifest::FILE, FOLDER]
                        .iter()
                        .any(|name| fs::symlink_metadata(folder.join(name)).is_ok())
            })
            .map(Path::to_path_buf)
    }

    /// the .fetchloom folder, which tells git to ignore it with a .gitignore made where none
    /// stands: when the folder is made, and again after a cleanup took the file but left the
    /// folder. a folder this call made and could not finish goes again.
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
        let ignore = folder.join(".gitignore");
        if created {
            let finished = write_atomically(&ignore, b"*\n")
                .map_err(|err| {
                    Error::io(format_args!("could not write {}", folder.display()), &err)
                })
                .and_then(|()| {
                    sync_directory(&self.root).map_err(|err| {
                        Error::io("could not flush the new project metadata folder", &err)
                    })
                });
            if let Err(error) = finished {
                let _ = fs::remove_file(&ignore);
                let _ = fs::remove_dir(&folder);
                return Err(error);
            }
        } else if fs::symlink_metadata(&ignore)
            .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
        {
            // over nothing: a file someone put there meanwhile stays
            let made = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&ignore)
                .and_then(|file| fill(file, b"*\n"));
            if let Err(err) = made {
                tracing::debug!(%err, "could not make .fetchloom/.gitignore again");
            }
        }
        Ok(folder)
    }

    fn unreadable(&self, name: &str, err: &io::Error) -> Error {
        Error::io(
            format_args!("could not read {}", self.root.join(name).display()),
            err,
        )
    }

    /// the refusal of the project file `name`, which [`open_own`] could not open with `err`: a
    /// link, as the refusal of a linked `.fetchloom` words it, a folder or a special file, told
    /// with `help`, or the system's own error.
    fn refusal(&self, name: &str, err: &io::Error, help: &str) -> Error {
        let path = self.root.join(name);
        match in_place_of_file(&path) {
            Some(Standing::Link) => {
                Error::new(Kind::ProjectInvalid, format!("{} {LINKED}", path.display()))
                    .help(replace_link(name))
            }
            Some(standing) => Error::new(
                Kind::LocalIo,
                format!("{} is {}, not a file", path.display(), standing.said()),
            )
            .help(help),
            None => self.unreadable(name, err),
        }
    }

    /// the bytes of the project file `name`, data.toml or data.lock, opened as [`open_own`] opens
    /// it and read up to the most fetchloom reads of it, [`MANIFEST_MOST`] or [`LOCK_MOST`]: a
    /// link, a folder, a special file or a larger file is refused with an error saying which.
    pub(crate) fn read_own(&self, name: &str) -> io::Result<Vec<u8>> {
        let path = self.root.join(name);
        let most = if name == lock::FILE {
            LOCK_MOST
        } else {
            MANIFEST_MOST
        };
        let refused = |cause: io::Error, words: String, help: Option<String>| {
            io::Error::new(cause.kind(), crate::error::Explained { words, help, cause })
        };
        let (file, found) = open_own(&path).map_err(|err| match in_place_of_file(&path) {
            Some(Standing::Link) => refused(err, format!("it {LINKED}"), Some(replace_link(name))),
            Some(standing) => refused(err, format!("it is {}, not a file", standing.said()), None),
            None => err,
        })?;
        let too_large = || {
            refused(
                io::ErrorKind::FileTooLarge.into(),
                format!(
                    "it is larger than {}, more than fetchloom reads of any {name}",
                    crate::units::size(most)
                ),
                None,
            )
        };
        if found.len() > most {
            return Err(too_large());
        }
        let mut bytes = Vec::with_capacity(usize::try_from(found.len()).unwrap_or_default());
        file.take(most + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > most {
            return Err(too_large());
        }
        Ok(bytes)
    }

    /// the text of the project file `name`, as [`Project::read_own`] reads it, or none when it
    /// cannot be read or is no UTF-8 text.
    pub(crate) fn own_text(&self, name: &str) -> Option<String> {
        String::from_utf8(self.read_own(name).ok()?).ok()
    }
}

/// what stands where a project file should be, when it is no plain file.
#[derive(Clone, Copy)]
enum Standing {
    Link,
    Folder,
    Special,
}

impl Standing {
    fn said(self) -> &'static str {
        match self {
            Standing::Link => "a link",
            Standing::Folder => "a folder",
            Standing::Special => "a special file",
        }
    }
}

/// what fetchloom says of a project file that is a link, after its name.
const LINKED: &str = "is a link, and fetchloom does not follow it";

/// what to do about the project file `name`, a link.
fn replace_link(name: &str) -> String {
    format!("replace {name} with the file it names before running this command")
}

/// what stands at `path` in place of a plain file: a link, which fetchloom never follows to a
/// project file, a folder or a special file. none for a plain file, or nothing there at all.
fn in_place_of_file(path: &Path) -> Option<Standing> {
    let found = fs::symlink_metadata(path).ok()?.file_type();
    if found.is_symlink() {
        Some(Standing::Link)
    } else if found.is_dir() {
        Some(Standing::Folder)
    } else if found.is_file() {
        None
    } else {
        Some(Standing::Special)
    }
}

/// the project file at `path`, data.toml or data.lock, opened to read as every project file is,
/// with what the opened file is: never through a link, since a link a clone holds would have the
/// file it names read, and printed where it does not parse, and never waiting on a fifo or a
/// device. a link, a folder or a special file is refused before a byte is read.
fn open_own(path: &Path) -> io::Result<(File, fs::Metadata)> {
    let file = crate::volume::read_unfollowed(path)?;
    let found = file.metadata()?;
    Ok((file, found))
}

/// the file at `path`, a path in the project with `/` between names, below `root` and written with
/// this system's separators, so a message names it as the person's own tools do.
pub(crate) fn native(root: &Path, path: &str) -> PathBuf {
    if cfg!(windows) {
        root.join(path.replace('/', "\\"))
    } else {
        root.join(path)
    }
}

/// whether `folder` is where a git repository begins: it holds `.git`, as a folder or as the file
/// a submodule or worktree has.
fn is_repository(folder: &Path) -> bool {
    folder.join(".git").exists()
}

/// `path`, a name or a path in a project, as a .gitignore pattern that names it and nothing else:
/// git reads `[`, `]`, `*`, `?` and `\` as a pattern, and drops the spaces that end a line unless
/// each is escaped. no line can hold a line break, so each control character becomes `?`, which
/// matches it, and a few names beside it.
pub(crate) fn gitignore_escaped(path: &str) -> String {
    let kept = path.trim_end_matches(' ');
    let mut pattern = String::with_capacity(path.len());
    for c in kept.chars() {
        match c {
            '[' | ']' | '*' | '?' | '\\' => {
                pattern.push('\\');
                pattern.push(c);
            }
            c if c.is_control() => pattern.push('?'),
            c => pattern.push(c),
        }
    }
    for _ in kept.len()..path.len() {
        pattern.push_str("\\ ");
    }
    pattern
}

/// the .gitattributes pattern of every file below the folder `at`, a path from data.toml's folder,
/// naming no other: `at` escaped as .gitignore reads a pattern, as .gitattributes reads one alike,
/// with a leading `#` or `!` escaped too, and quoted where it holds a space, which would end the
/// pattern there, or begins with a quote.
pub(crate) fn below_in_attributes(at: &str) -> String {
    let mut pattern = format!("{}/**", gitignore_escaped(at));
    if pattern.starts_with(['#', '!']) {
        pattern.insert(0, '\\');
    }
    if pattern.contains(' ') || pattern.starts_with('"') {
        pattern = format!("\"{}\"", pattern.replace('\\', "\\\\").replace('"', "\\\""));
    }
    pattern
}

/// .gitignore's text, read without following a link and only up to [`GITIGNORE_CAP`]: a link a
/// clone holds would have the file it names copied in, and a pipe or a device never ends.
/// `InvalidInput` for a link or a special file, `FileTooLarge` past the cap.
pub(crate) fn read_gitignore(path: &Path) -> io::Result<String> {
    let file = crate::volume::read_unfollowed(path)?;
    let mut bytes = Vec::new();
    file.take(GITIGNORE_CAP + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > GITIGNORE_CAP {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// the text of the plain file at `path`, read without following a link, up to `most` bytes.
fn read_capped(path: &Path, most: u64) -> io::Result<String> {
    let mut text = String::new();
    crate::volume::read_unfollowed(path)?
        .take(most)
        .read_to_string(&mut text)?;
    Ok(text)
}

/// the error for project file `name` when its `text` holds git's conflict lines at a line start,
/// `<<<<<<<`, `|||||||`, `=======` or `>>>>>>>`. none when no such line is there.
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
            crate::suggested::command("lock")
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

/// whether `file`, a name in `.fetchloom`, is how [`write_atomically_with`] stages data.toml,
/// data.lock, the state or a list's part: a dot, the name, a dot and 8 hex digits, then `.tmp`.
fn is_staged(file: &str) -> bool {
    file.strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(name, random)| {
            random.len() == 8
                && random.bytes().all(|byte| byte.is_ascii_hexdigit())
                && (matches!(name, manifest::FILE | lock::FILE | "state")
                    || lock::lists::is_part(name))
        })
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
    // a list's part stages in the project's folder too: half written in data.lock.d, as a kill
    // leaves it, `git add data.lock.d` would take it
    let beside = match parent.file_name() {
        Some(name) if name == lock::lists::FOLDER => parent.parent().unwrap_or(parent),
        _ => parent,
    };
    let own = beside.join(FOLDER);
    // a link there, as a clone can hold, would stage the file wherever it points
    let folder = if fs::symlink_metadata(&own).is_ok_and(|found| found.is_dir()) {
        own.as_path()
    } else {
        parent
    };
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let kept = kept_permissions(path);
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
    let written = keep_mode(&file, kept.as_ref())
        .and_then(|()| write(file))
        .and_then(|()| replace(&temporary, path, kept.as_ref()))
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

/// what a rewrite of `path` keeps of the file there: its mode on unix, without the bits that run
/// it as another user, and elsewhere its read-only attribute. none for no file, or a link.
fn kept_permissions(path: &Path) -> Option<fs::Permissions> {
    let found = fs::symlink_metadata(path)
        .ok()
        .filter(fs::Metadata::is_file)?;
    let permissions = found.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Some(fs::Permissions::from_mode(permissions.mode() & 0o777))
    }
    #[cfg(not(unix))]
    permissions.readonly().then_some(permissions)
}

/// gives the new file, open as `file`, the mode `kept`, flushed with its bytes.
#[cfg(unix)]
fn keep_mode(file: &File, kept: Option<&fs::Permissions>) -> io::Result<()> {
    kept.map_or(Ok(()), |kept| file.set_permissions(kept.clone()))
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "windows keeps the read-only attribute once the new file is in place"
)]
fn keep_mode(_: &File, _: Option<&fs::Permissions>) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace(temporary: &Path, path: &Path, _: Option<&fs::Permissions>) -> io::Result<()> {
    fs::rename(temporary, path)
}

/// renames `temporary` onto `path`, which windows refuses while `path` is read-only, so one that
/// is turns writable just before. the new file is read-only already, so no stop after the rename
/// leaves it writable; only the old bytes can be left so, by a stop right before it.
#[cfg(not(unix))]
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "windows has no mode bits, so this clears the read-only attribute alone"
)]
fn replace(temporary: &Path, path: &Path, kept: Option<&fs::Permissions>) -> io::Result<()> {
    let Some(kept) = kept else {
        return naming(path, || fs::rename(temporary, path));
    };
    let mut writable = kept.clone();
    writable.set_readonly(false);
    fs::set_permissions(temporary, kept.clone())?;
    let renamed = fs::set_permissions(path, writable.clone()).and_then(|()| {
        naming(path, || fs::rename(temporary, path)).inspect_err(|_| {
            let _ = fs::set_permissions(path, kept.clone());
        })
    });
    if renamed.is_err() {
        // windows removes no read-only file, and the caller removes this one
        let _ = fs::set_permissions(temporary, writable);
    }
    #[cfg(test)]
    if renamed.is_ok() {
        tests::renamed(path);
    }
    renamed
}

/// flushes the entries of the folder `path`, as [`crate::volume::sync_directory`] does, where
/// tests watch each flush.
fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(all(test, unix))]
    tests::syncing_directory(path)?;
    crate::volume::sync_directory(path)
}

/// writes `bytes` into a new file and flushes them to disk.
fn fill(mut file: File, bytes: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    tests::flushing_file();
    file.write_all(bytes)?;
    file.sync_all()
}

/// the folders a data folder may be a link into, as the `data_links` setting lists them.
#[derive(Clone, Debug, Default)]
pub(crate) struct DataLinks {
    pub(crate) folders: Vec<PathBuf>,
    /// whether `FETCHLOOM_DATA_LINKS` gave them, not the settings file, so a refusal names it.
    pub(crate) from_variable: bool,
}

/// a data folder link a run followed: where it is, its path in the project, the folder it led
/// to, resolved, and whether this was its first follow, which the state is to record.
#[derive(Clone, Debug)]
pub(crate) struct Followed {
    pub(crate) at: PathBuf,
    pub(crate) path: String,
    pub(crate) found: PathBuf,
    pub(crate) first: bool,
}

/// notes the data folder at `folder`, reached through a link [`Project::follow`] followed, so a placer
/// opens the folder found there now, whatever the link says by then.
fn remember(folder: &Path) -> Result<(), Error> {
    let found = fs::canonicalize(folder)
        .map_err(|err| Error::io(format_args!("could not read {}", folder.display()), &err))?;
    crate::place::open_at(folder, found)
        .map_err(|err| Error::io(format_args!("could not read {}", folder.display()), &err))
}

/// the refusal of the link at `at` to `found`, which `why` says what keeps fetchloom from following;
/// each refusal adds its own help.
pub(crate) fn link_refused(at: &Path, found: &Path, why: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{} is a link to {}, {why}, so fetchloom places nothing through it",
            at.display(),
            crate::machine::plain(found).display()
        ),
    )
}

/// the refusal of the link at `folder` to `found`, a folder `listed` holds, which lies in the
/// project at `other`.
fn taken(folder: &Path, found: &Path, other: &Path) -> Error {
    link_refused(
        folder,
        found,
        &format!("which lies in the project at {}", other.display()),
    )
    .help("point the link at a folder of this project's own, outside every other project's folder")
}

/// the refusal of the link at `folder` to `found`, which no folder `listed` holds, with how to
/// list the folder where the list came from.
fn unlisted(folder: &Path, found: &Path, listed: &DataLinks) -> Error {
    let error = link_refused(folder, found, "which no folder in data_links holds");
    let entries = listed.folders.as_slice();
    let check = "check where it leads, since a cloned project's link can aim anywhere; if you made it to keep data on another disk";
    if listed.from_variable {
        let value: Vec<String> = entries
            .iter()
            .chain([&found.to_path_buf()])
            .map(|entry| entry.display().to_string())
            .collect();
        return error.help(format!(
            "{check}, list that folder in FETCHLOOM_DATA_LINKS too, as `{}`",
            value.join(",")
        ));
    }
    let words: Vec<String> = entries
        .iter()
        .map(PathBuf::as_path)
        .chain([found])
        .map(crate::suggested::shell_folder_path)
        .collect();
    error.help(format!(
        "{check}, list that folder with `fetchloom settings set data_links {}`",
        words.join(" ")
    ))
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

    #[test]
    fn the_told_attributes_pattern_names_just_the_folder_below_it() {
        for (at, pattern) in [
            ("results", "results/**"),
            ("results/[run]", "results/\\[run\\]/**"),
            ("out/*?", "out/\\*\\?/**"),
            ("my results", "\"my results/**\""),
            ("results/[run] 1", "\"results/\\\\[run\\\\] 1/**\""),
            ("#hash", "\\#hash/**"),
            ("!bang", "\\!bang/**"),
            ("q\"uote", "q\"uote/**"),
            ("\"lead", "\"\\\"lead/**\""),
        ] {
            assert_eq!(below_in_attributes(at), pattern, "{at}");
        }
    }

    /// a power cut leaves the flushed record whole for the next command; its folder is not flushed.
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
        let project = Project::at(dir.path().to_path_buf());
        (dir, project)
    }

    /// the data.lock a command recorded is written only while data.toml is the one that command
    /// wrote: before that write, or after an edit, data.lock stays as it is. either way the record
    /// goes.
    #[test]
    fn a_pending_data_lock_follows_only_its_own_data_toml() {
        let old_lock = "version = 1\n";
        let new_lock = "version = 1\n# the new one\n";
        for (manifest, finished) in [("[datasets]\n# written\n", true), ("[datasets]\n", false)] {
            let (_dir, project) = project(&[("data.toml", manifest), ("data.lock", old_lock)]);
            project
                .write_pending(b"[datasets]\n# written\n", new_lock)
                .unwrap();
            assert_eq!(project.finish_pending().unwrap(), finished, "{manifest}");
            let lock = fs::read_to_string(project.root.join(lock::FILE)).unwrap();
            assert_eq!(
                lock,
                if finished { new_lock } else { old_lock },
                "{manifest}"
            );
            assert!(!project.own_folder().join(PENDING).exists());
            assert!(!project.finish_pending().unwrap(), "the record is gone");
        }
    }

    /// a record another folder's run left, as a clone or a copy of the project holds it, finishes
    /// nothing here though data.toml is the same: data.lock stays as committed, and the record
    /// goes. so does one that names no folder.
    #[test]
    fn a_pending_data_lock_of_another_folder_is_never_written() {
        let manifest = "[datasets]\n# written\n";
        let old_lock = "version = 1\n";
        let (_dir, project) = project(&[("data.toml", manifest), ("data.lock", old_lock)]);
        let path = project.own_folder().join(PENDING);
        // the same record as another folder would hold it: another number and creation time,
        // since two folders made at once on fat can share their creation time; or no folder
        for moved in [true, false] {
            project
                .write_pending(manifest.as_bytes(), "version = 1\n# another one\n")
                .unwrap();
            let record = fs::read_to_string(&path).unwrap();
            let (head, lock) = record.split_once('\n').unwrap();
            let mut head: serde_json::Value = serde_json::from_str(head).unwrap();
            for number in ["inode", "created"] {
                if let Some(value) = head["folder"][number].as_u64() {
                    head["folder"][number] = (value + 1).into();
                }
            }
            if !moved {
                head["folder"] = serde_json::Value::Null;
            }
            fs::write(&path, format!("{head}\n{lock}")).unwrap();
            assert!(!project.finish_pending().unwrap(), "{head}");
            assert_eq!(
                fs::read_to_string(project.root.join(lock::FILE)).unwrap(),
                old_lock
            );
            assert!(!path.exists());
        }
    }

    /// links `path` to the file `target`, or false where no symbolic link can be made, as on
    /// windows without developer mode or admin.
    fn linked(target: &Path, path: &Path) -> bool {
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(target, path);
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(target, path);
        made.is_ok()
    }

    /// a project folder so deep that `.fetchloom` fits linux's 4,095 bytes a path but nothing
    /// inside it does: the folder this command made goes with the error.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_metadata_folder_that_cannot_be_finished_is_not_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let deepest = 4_095 - "/.fetchloom".len();
        let mut root = dir.path().to_path_buf();
        loop {
            let left = deepest - root.as_os_str().len() - 1;
            if left <= 200 {
                root.push("e".repeat(left));
                break;
            }
            root.push("d".repeat(200));
        }
        fs::create_dir_all(&root).unwrap();
        let project = Project::at(root);
        let error = project.inner().unwrap_err();
        assert!(error.message.contains("file name too long"), "{error:?}");
        assert!(!project.own_folder().exists());
    }

    #[test]
    fn the_project_is_the_nearest_folder_with_a_data_toml() {
        let (dir, _) = project(&[("data.toml", ""), ("a/b/c.txt", "")]);
        let found = Project::find(&dir.path().join("a").join("b")).unwrap();
        assert_eq!(found.root, dir.path());
        assert!(Project::find(&tempfile::tempdir().unwrap().path().join("x")).is_none());
    }

    /// the search ends at the first folder holding `.git`, a folder or a submodule's or worktree's
    /// file, once its own data.toml is checked.
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

    /// naming the holder lets a person find a forgotten terminal.
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

    /// an owner record a fetchloom on `os` wrote for `project`'s folder.
    fn owner_record(os: &str, project: &Project) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "os": os,
            "pid": 42,
            "started": "2026-10-30T12:00:00Z",
            "command": "sync",
            "folder": Folder::of_on(&project.root, os),
        }))
        .unwrap()
    }

    #[test]
    fn claiming_publishes_a_complete_owner_without_temporary_files() {
        let (_dir, project) = project(&[]);
        if !crate::cache::tests::hardlinks(&project.root) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
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
            removed: None,
            here: None,
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
        for (replaced, elapsed) in [(true, Duration::ZERO), (false, Duration::from_secs(20))] {
            let (_dir, project) = project(&[(".fetchloom/owner", "")]);
            let replacement = if replaced {
                owner_record(os, &project)
            } else {
                Vec::new()
            };
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

    /// wsl and windows share a project on a windows drive: a record wsl wrote there is this
    /// folder's on windows by its path, mapped between /mnt/c/... and C:\..., in any case, and a
    /// copy's at another path; a record of another system's naming no path holds, since nothing
    /// tells it apart, and so does one whose folder names no system, as records from before
    /// folders named theirs: the record's own system is the folder's. the folder's number and
    /// creation time differ across the two.
    #[test]
    fn another_systems_owner_record_is_this_folders_by_its_path() {
        let windows = |path: &str| -> Folder {
            serde_json::from_value(serde_json::json!({
                "os": "windows",
                "path": path,
                "inode": 9,
                "created": 10,
            }))
            .unwrap()
        };
        let wsl = |path: Option<&str>| {
            let mut folder = serde_json::json!({ "os": "linux", "inode": 5 });
            if let Some(path) = path {
                folder["path"] = path.into();
            }
            folder
        };
        for (here, written, holds) in [
            (
                windows(r"C:\Users\me\proj"),
                wsl(Some("/mnt/c/Users/me/proj")),
                true,
            ),
            (
                windows(r"\\?\c:\users\ME\proj"),
                wsl(Some("/mnt/c/Users/me/proj")),
                true,
            ),
            (
                windows(r"D:\copy\proj"),
                wsl(Some("/mnt/c/Users/me/proj")),
                false,
            ),
            (windows(r"C:\Users\me\proj"), wsl(None), true),
            (
                windows(r"C:\Users\me\proj"),
                serde_json::json!({ "inode": 5 }),
                true,
            ),
        ] {
            let (_dir, project) = project(&[(".fetchloom/owner", "")]);
            let path = project.own_folder().join("owner");
            let record = serde_json::to_vec(&serde_json::json!({
                "os": "linux",
                "pid": 42,
                "started": "2026-10-30T12:00:00Z",
                "command": "sync",
                "folder": written,
            }))
            .unwrap();
            fs::write(&path, &record).unwrap();
            let claimed = Held {
                lock: None,
                owner: None,
                removed: None,
                here: Some(here),
            }
            .claim(&path, "remove", SystemTime::now())
            .unwrap();
            if holds {
                assert!(
                    matches!(claimed, Hold::Owner(_) | Hold::Unchecked { .. }),
                    "a record wsl wrote for this folder was removed: {written}"
                );
                assert_eq!(fs::read(&path).unwrap(), record, "{written}");
            } else {
                let Hold::Held(mut held) = claimed else {
                    panic!("a record wsl wrote for another folder holds this one: {written}")
                };
                assert_eq!(held.removed_unread(), Some(path.clone()));
            }
        }
    }

    /// another system's record of this folder, by the path a windows drive gives windows and wsl
    /// alike, may be a process on this machine, so a command waits for it, naming the record;
    /// one whose folder this system cannot match with its own, as a project copied from windows
    /// to a linux disk carries, no process on this machine holds, and this machine cannot check
    /// one elsewhere, so the command stops at once, and the record stays.
    #[test]
    fn another_systems_owner_waits_only_for_a_folder_both_systems_name() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let (os, other) = if cfg!(windows) {
            ("windows", "linux")
        } else {
            ("linux", "windows")
        };
        let folder = |os: &str, path: &str| -> Folder {
            serde_json::from_value(serde_json::json!({ "os": os, "path": path, "inode": 7 }))
                .unwrap()
        };
        let named = |os: &str, path: &str| {
            if os == "windows" {
                format!(r"C:\Users\ada\{path}")
            } else {
                format!("/mnt/c/Users/ada/{path}")
            }
        };
        for (here, waits) in [
            (folder(os, &named(os, "cc")), true),
            (folder(os, "/datadisk/ada/cc"), false),
        ] {
            let record = serde_json::to_vec(&serde_json::json!({
                "os": other,
                "pid": 4242,
                "started": "2026-11-19T02:00:00Z",
                "command": "sync",
                "folder": folder(other, &named(other, "cc")),
            }))
            .unwrap();
            fs::write(&path, &record).unwrap();
            let held = Held {
                lock: None,
                owner: None,
                removed: None,
                here: Some(here.clone()),
            };
            let who = format!("`fetchloom sync` ({other}, pid 4242, started 2026-11-19T02:00:00Z)");
            match held.claim(&path, "remove", SystemTime::now()).unwrap() {
                Hold::Owner(message) if waits => assert_eq!(
                    message,
                    format!(
                        "for {who} working in this project; if that process is gone, delete {}",
                        path.display()
                    )
                ),
                Hold::Unchecked {
                    who: named,
                    path: at,
                } if !waits => {
                    assert_eq!((named, at), (who, path.clone()));
                }
                _ => panic!("{here:?} waits: {waits}"),
            }
            assert_eq!(fs::read(&path).unwrap(), record);
        }
    }

    /// an owner record this system wrote for another folder, as a clone or a copy of a project
    /// carries, holds nobody out, and neither does one that names no folder, whatever system it
    /// names, as every record this build writes names one; one another system wrote for this
    /// folder holds it. `another_systems_owner_record_is_this_folders_by_its_creation_time` tells
    /// another system's records apart.
    #[test]
    fn an_owner_record_written_for_another_folder_is_stale() {
        let (_dir, project) = project(&[(".fetchloom/notes", "")]);
        let path = project.own_folder().join("owner");
        let other = if cfg!(windows) { "linux" } else { "windows" };
        let record = |os: &str, folder: Option<serde_json::Value>| {
            let mut record = serde_json::json!({
                "os": os,
                "pid": 4242,
                "started": "2026-11-18T10:00:00Z",
                "command": "sync",
            });
            if let Some(folder) = folder {
                record["folder"] = folder;
            }
            record.to_string()
        };
        // another folder's number and creation time, as the one a clone was made from has, or no
        // folder, as a record someone wrote or committed by hand holds
        for (os, elsewhere) in [
            (
                std::env::consts::OS,
                Some(serde_json::json!({ "os": std::env::consts::OS, "inode": 1, "created": 1 })),
            ),
            (other, None),
        ] {
            fs::write(&path, record(os, elsewhere.clone())).unwrap();
            let Hold::Held(mut held) = project.hold("remove").unwrap() else {
                panic!("an owner record written for another folder keeps nobody out: {elsewhere:?}")
            };
            assert_eq!(held.removed_unread(), Some(path.clone()));
            let ours = Owner::read(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                ours.folder,
                Folder::of(&project.root),
                "the record names its folder"
            );
            drop(held);
        }
        let here = serde_json::to_value(Folder::of_on(&project.root, other)).unwrap();
        fs::write(&path, record(other, Some(here))).unwrap();
        assert!(
            matches!(
                project.hold("remove").unwrap(),
                Hold::Owner(_) | Hold::Unchecked { .. }
            ),
            "one written for this folder by another system holds it"
        );
    }

    /// a record no fetchloom writes, as a clone or a crash can leave, holds nobody out: other
    /// words, broken JSON, more than fetchloom writes or a link, which is never followed. taking
    /// the project removes it, once, for a warning, and the record there is this command's.
    #[test]
    fn an_owner_record_fetchloom_cannot_read_is_stale() {
        let (_dir, project) = project(&[(".fetchloom/elsewhere", "{}")]);
        let path = project.own_folder().join("owner");
        let target = project.own_folder().join("elsewhere");
        let record = |os: &str, command: &str, started: &str| {
            serde_json::json!({
                "os": os,
                "pid": 42,
                "started": started,
                "command": command,
            })
            .to_string()
        };
        for (what, planted) in [
            (
                "a system",
                Some(record("plan9", "sync", "2026-10-30T12:00:00Z")),
            ),
            (
                "a command",
                Some(record("windows", "rm -rf ~", "2026-10-30T12:00:00Z")),
            ),
            (
                "a time",
                Some(record("windows", "sync", "\u{1b}]52;c;aGk=\u{7}")),
            ),
            ("broken json", Some("{".to_owned())),
            ("too large", Some("x".repeat(2 << 20))),
            ("a link", None),
        ] {
            match planted {
                Some(text) => fs::write(&path, text).unwrap(),
                None if linked(&target, &path) => {}
                None => continue,
            }
            let Hold::Held(mut held) = project.hold("remove").unwrap() else {
                panic!("{what}: a record no fetchloom writes keeps nobody out")
            };
            assert_eq!(held.removed_unread(), Some(path.clone()), "{what}");
            assert_eq!(held.removed_unread(), None, "{what}: told once");
            let ours = Owner::read(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(ours.command, "remove", "{what}");
            drop(held);
            assert!(!path.exists(), "{what}");
        }
        assert_eq!(fs::read(&target).unwrap(), b"{}");
    }

    #[test]
    fn same_os_owner_is_taken_over_under_the_os_lock() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        fs::write(&path, owner_record(std::env::consts::OS, &project)).unwrap();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234);
        let Hold::Held(held) = project.hold_at("remove", now).unwrap() else {
            panic!("the os lock proves this owner is stale")
        };
        assert!(held.lock.is_some());
        let owner: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(owner["os"], std::env::consts::OS);
        assert_eq!(owner["pid"], std::process::id());
        assert_eq!(owner["started"], crate::units::timestamp(now));
        assert_eq!(owner["command"], "remove");
    }

    #[test]
    fn same_os_owner_without_an_os_lock_waits() {
        let (_dir, project) = project(&[(".fetchloom/owner", "")]);
        let path = project.own_folder().join("owner");
        let record = owner_record(std::env::consts::OS, &project);
        fs::write(&path, &record).unwrap();
        let held = Held {
            lock: None,
            owner: None,
            removed: None,
            here: None,
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
            removed: None,
            here: None,
        };
        for lock in [None, Some(File::open(&path).unwrap())] {
            let second = Held {
                lock,
                owner: None,
                removed: None,
                here: None,
            }
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
    fn an_owner_too_large_to_be_one_is_not_read_whole() {
        let large = "x".repeat(2 << 20);
        let (_dir, project) = project(&[(".fetchloom/owner", &large)]);
        let path = project.own_folder().join("owner");
        let err = read_owner(&path).err().map(|err| err.kind());
        assert_eq!(err, Some(io::ErrorKind::InvalidData));
    }

    #[test]
    fn a_linked_owner_is_never_followed() {
        let (_dir, project) = project(&[(".fetchloom/elsewhere", "{}")]);
        let target = project.own_folder().join("elsewhere");
        let path = project.own_folder().join("owner");
        if !linked(&target, &path) {
            return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
        }
        let err = read_owner(&path).err().map(|err| err.kind());
        assert_eq!(err, Some(io::ErrorKind::InvalidInput));
        assert_eq!(fs::read(&target).unwrap(), b"{}");
    }

    /// a `.fetchloom/lock` link is refused whatever it names: locking its target keeps no one out.
    #[test]
    fn a_linked_project_lock_is_refused_and_never_followed() {
        let (_dir, project) = project(&[(".fetchloom/elsewhere", "")]);
        let folder = project.own_folder();
        let path = folder.join("lock");
        for target in [folder.join("elsewhere"), folder.join("nothing")] {
            let _ = fs::remove_file(&path);
            if !linked(&target, &path) {
                return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
            }
            let (answer, answered) = std::sync::mpsc::channel();
            let holder = Project::at(project.root.clone());
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

    /// where names and open files disagree, the hold ends with an error rather than trying forever.
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
        if !linked(&target, &path) {
            return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
        }
        assert_eq!(project.state().1, 0);
    }

    /// a cloned project's data.lock.d may link anywhere, so it is refused before fetchloom writes
    /// a list there or removes one, and a sweep leaves it alone.
    #[test]
    fn a_linked_lists_folder_is_refused_and_never_swept() {
        let (_dir, project) = project(&[("data.toml", "")]);
        let outside = tempfile::tempdir().unwrap();
        let old = outside.path().join("old.0123456789ab.tsv.zst");
        fs::write(&old, "x").unwrap();
        let link = project.root.join(lock::lists::FOLDER);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        // making a symlink on windows needs developer mode or admin
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();
        if !made {
            return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
        }
        project.sweep_lists(&Lock::default());
        assert!(old.is_file(), "a sweep went through the link");
        let held = project.hold("sync").err();
        let written = project.write_lists(&Lock::default()).err();
        for error in [held, written] {
            let error = error.expect("a linked data.lock.d was accepted");
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(
                error.message.contains(&link.display().to_string()),
                "{}",
                error.message
            );
            assert!(error.help.is_some());
        }
        assert!(
            !project.own_folder().exists(),
            "the refused hold made .fetchloom"
        );
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

    /// what a run killed between staging a file and naming it left in `.fetchloom`, a part of
    /// tens of megabytes as like as data.lock or the state, goes once a command holds the
    /// project; anything else there stays.
    #[test]
    fn holding_the_project_sweeps_what_a_killed_write_staged() {
        let left = [
            ".data.lock.0123abcd.tmp",
            ".data.toml.89abcdef.tmp",
            ".state.deadbeef.tmp",
            ".clean.0123456789ab.tsv.zst.0badf00d.tmp",
        ];
        let kept = [
            ".notes.0123abcd.tmp",
            ".data.lock.tmp",
            "data.lock.recovery.0123456789abcdef",
        ];
        let files: Vec<(String, &str)> = left
            .iter()
            .chain(&kept)
            .map(|name| (format!(".fetchloom/{name}"), "x"))
            .collect();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), *text))
            .collect();
        let (_dir, project) = project(&files);
        let own = project.own_folder();
        // a write stopped between staging and naming, as a kill stops it
        let stopped = std::panic::catch_unwind(|| {
            write_atomically_with(&project.root.join(lock::FILE), |_| panic!("killed"))
        });
        assert!(stopped.is_err());
        let staged = || {
            fs::read_dir(&own)
                .unwrap()
                .filter(|entry| {
                    let name = entry.as_ref().unwrap().file_name();
                    let name = name.to_string_lossy();
                    let planted = left.iter().chain(&kept).any(|file| *file == name);
                    name.starts_with(".data.lock.") && !planted
                })
                .count()
        };
        assert_eq!(staged(), 1);
        let Hold::Held(_held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        assert_eq!(staged(), 0);
        for name in left {
            assert!(!own.join(name).exists(), "{name}");
        }
        for name in kept {
            assert!(own.join(name).exists(), "{name}");
        }
    }

    /// a write stopped as it named a read-only file leaves its staged copy read-only, and a person
    /// may make a part so: fat and exfat remove neither until the flag is cleared.
    #[cfg(windows)]
    #[test]
    fn a_sweep_removes_read_only_files_on_every_disk() {
        let staged = ".fetchloom/.data.toml.0123abcd.tmp";
        let part = "data.lock.d/old.0123456789ab.tsv.zst";
        let (_dir, project) = project(&[(staged, "x"), (part, "x")]);
        for file in [staged, part] {
            crate::volume::set_read_only(&project.root.join(file), true).unwrap();
        }
        let Hold::Held(_held) = project.hold("sync").unwrap() else {
            panic!("free")
        };
        project.sweep_lists(&Lock::default());
        for file in [staged, part] {
            assert!(!project.root.join(file).exists(), "{file} was left");
        }
    }

    #[test]
    fn an_error_releases_the_owner() {
        let (_dir, project) = project(&[]);
        let path = project.inner().unwrap().join("owner");
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1234);
        // a forked test child can retain an OS lock until exec; owner cleanup needs no OS lock
        let claim = || {
            Held {
                lock: None,
                owner: None,
                removed: None,
                here: None,
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
        // FAT32 and exFAT can make neither a hardlink nor a junction
        if fs::hard_link(project.manifest_path(), project.root.join("link-probe")).is_err() {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let outside = tempfile::tempdir().unwrap();
        let link = project.own_folder();
        crate::test_links::link_folder(outside.path(), &link).unwrap();

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

    /// a part is staged in fetchloom's own folder, not in data.lock.d, where a run killed while
    /// writing it would leave a half-written file that `git add data.lock.d` takes; one an older
    /// run left there goes with the next sweep.
    #[test]
    fn a_part_is_never_staged_in_the_lists_folder() {
        let (_dir, project) = project(&[]);
        let own = project.inner().unwrap();
        let lists = project.root.join(lock::lists::FOLDER);
        fs::create_dir(&lists).unwrap();
        let staged_in = |folder: &Path| {
            fs::read_dir(folder)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".first."))
        };
        let part = lists.join("first.0123456789ab.tsv.zst");
        let failure = write_atomically_with(&part, |mut file| {
            file.write_all(b"partial")?;
            assert!(staged_in(&own));
            assert!(!staged_in(&lists));
            Err(io::ErrorKind::Interrupted.into())
        });
        assert_eq!(failure.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(!staged_in(&own));
        let left = lists.join(".first.0123456789ab.tsv.zst.0badf00d.tmp");
        fs::write(&left, b"partial").unwrap();
        let theirs = lists.join(".notes.tmp");
        fs::write(&theirs, b"a person's file").unwrap();
        project.sweep_lists(&Lock::default());
        assert!(!left.exists());
        assert!(theirs.exists());
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
        let project = Project::at(root);
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
                return crate::test_skips::skip("unprivileged", "root ignores file permissions");
            }
            let shown = format!("{} is read-only", project.root.display());
            for error in [written.unwrap_err(), checked.unwrap_err()] {
                assert!(error.message.contains(&shown), "{}", error.message);
                assert!(error.help.unwrap().contains("writable"));
            }
            assert_eq!(fs::read(project.root.join("data.lock")).unwrap(), b"before");
        }
    }

    /// a command that did not start its project never removes what it did not make.
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

    /// a lock on a file that lost its name keeps nobody out, as when a command that started no
    /// project removed `.fetchloom` while another waited, so the name is opened again.
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
            return crate::test_skips::skip(
                "freed-names",
                "this disk keeps a removed file's name while it is open",
            );
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
        assert_eq!(lock.help.as_deref(), Some(lock::recovery().as_str()));
    }

    /// the message names the merge, not the TOML mistake its conflict lines make, in either of
    /// git's conflict styles and either line ending.
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

    /// data.toml and data.lock are read only as plain files: a link, which a clone can hold to any
    /// file of the person's, is refused unfollowed, and a folder or a special file, as a fifo a
    /// clone holds, before a read that would fail or never end.
    #[cfg(unix)]
    #[test]
    fn a_project_file_that_is_no_plain_file_is_refused_unread() {
        type Refused = fn(&Project) -> Option<Error>;
        let read: [(&str, Refused); 2] = [
            (manifest::FILE, |project| project.manifest().err()),
            (lock::FILE, |project| project.lock().err()),
        ];
        for (name, read) in read {
            let (dir, project) = project(&[("data.toml", "")]);
            let path = project.root.join(name);
            let elsewhere = dir.path().join("elsewhere.toml");
            fs::write(&elsewhere, "version = 1\n").unwrap();
            let _ = fs::remove_file(&path);
            std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
            let linked = read(&project).unwrap();
            assert_eq!(linked.kind, Kind::ProjectInvalid, "{name}");
            assert_eq!(
                linked.message,
                format!(
                    "{} is a link, and fetchloom does not follow it",
                    path.display()
                )
            );
            assert_eq!(
                linked.help.as_deref(),
                Some(
                    format!("replace {name} with the file it names before running this command")
                        .as_str()
                )
            );
            assert!(linked.details.is_empty(), "{name}: {:?}", linked.details);
            fs::remove_file(&path).unwrap();
            let made = std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap();
            assert!(made.success());
            let special = read(&project).unwrap();
            assert_eq!(special.kind, Kind::LocalIo, "{name}");
            assert_eq!(
                special.message,
                format!("{} is a special file, not a file", path.display())
            );
            fs::remove_file(&path).unwrap();
            fs::create_dir(&path).unwrap();
            assert_eq!(
                read(&project).unwrap().message,
                format!("{} is a folder, not a file", path.display())
            );
        }
    }

    /// a DESCRIPTION that is a link, as a clone can hold to a fifo or a terminal a read would wait
    /// on, makes no R package, whatever the file it names says: it is never read.
    #[cfg(unix)]
    #[test]
    fn a_linked_description_makes_no_r_package() {
        let (dir, project) = project(&[("DESCRIPTION", "Package: irisdata\nVersion: 0.1.0\n")]);
        assert!(project.is_r_package());
        let named = dir.path().join("named");
        fs::rename(project.root.join("DESCRIPTION"), &named).unwrap();
        std::os::unix::fs::symlink(&named, project.root.join("DESCRIPTION")).unwrap();
        assert!(!project.is_r_package());
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

    /// a state another folder wrote, as a copy or a moved root brings it, is read once: its markers
    /// stay, as a copy brings their .gitignore files too, and the rest comes back as what it still
    /// tells this folder, its id, its records as hints and its told keys, never as its own. this
    /// folder's own state is read as it is.
    #[test]
    fn a_state_another_folder_wrote_tells_this_one_what_it_kept() {
        use crate::place::Method;
        use crate::project::state::{Once, Placed};

        let (_dir, project) = project(&[]);
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
                writable: false,
                checked: 43,
                adopted: false,
                output: false,
                unlinkable: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
        state
            .markers
            .insert("data/set".to_owned(), "wrote".to_owned());
        state.id = Some("ab12".to_owned());
        state.told.note(Once::Edit, "data/set/a:1:42");
        project.write_state(&state).unwrap();
        let path = project.own_folder().join("state");
        let ours: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        // as written, and as a state from before states named their system and path holds it
        let mut old = ours.clone();
        for field in ["os", "path"] {
            old["folder"].as_object_mut().unwrap().remove(field);
        }
        for written in [&ours, &old] {
            fs::write(&path, written.to_string()).unwrap();
            let (own, _) = project.state();
            assert_eq!(own.id.as_deref(), Some("ab12"), "{written}");
            assert_eq!(own.files.len(), 1, "{written}");
            assert!(own.copied.files.is_empty() && own.copied.id.is_none());
        }
        // the same state as another folder holds it: another number and creation time, or no
        // folder at all, which says of no folder that it is this one
        let mut moved = ours.clone();
        for number in ["inode", "created"] {
            if let Some(value) = moved["folder"][number].as_u64() {
                moved["folder"][number] = (value + 1).into();
            }
        }
        let mut untold = ours;
        untold.as_object_mut().unwrap().remove("folder");
        for elsewhere in [moved, untold] {
            fs::write(&path, elsewhere.to_string()).unwrap();
            let (copied, written) = project.state();
            assert_eq!(written, 0, "{elsewhere}");
            assert!(copied.files.is_empty() && copied.told.is_empty());
            assert_eq!(copied.id, None);
            assert_eq!(copied.markers, state.markers);
            assert_eq!(copied.copied.id.as_deref(), Some("ab12"), "{elsewhere}");
            assert_eq!(
                copied.copied.files.into_iter().collect::<Vec<_>>(),
                [("data/set/a".to_owned(), "set".to_owned())]
            );
            assert!(copied.copied.told.has(Once::Edit, "data/set/a:1:42"));
        }
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
                output: false,
                unlinkable: false,
                racy_intact: std::sync::OnceLock::new(),
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

    /// a hand-changed index changes no answer, since the whole read ignores the index.
    #[test]
    fn a_state_read_within_a_folder_answers_as_a_whole_read() {
        use crate::place::Method;
        use crate::project::state::Placed;

        let (_dir, project) = project(&[]);
        let mut state = State::default();
        for (dataset, path, size) in (0..2_000)
            .map(|number| ("large", format!("data/large/{number:04}"), number))
            .chain([("tiny", "data/tiny/payload.bin".to_owned(), 14)])
        {
            let placed = Placed {
                dataset: dataset.to_owned(),
                sha256: crate::digest::sha256(path.as_bytes()),
                method: Method::Hardlink,
                look: Look { size, modified: 42 },
                writable: false,
                checked: 43,
                adopted: false,
                output: false,
                unlinkable: false,
                racy_intact: std::sync::OnceLock::new(),
            };
            state.files.insert(path, placed);
        }
        project.write_state(&state).unwrap();
        let folders = ["data/tiny".to_owned()];
        let keep = |path: &str| path.starts_with("data/tiny/");
        let whole = |project: &Project| {
            let (mut state, written) = project.state();
            state.files.retain(|path, _| keep(path));
            (state.files, written)
        };
        let within = |project: &Project| {
            let (state, written) = project.state_within(&folders, &keep);
            (state.files, written)
        };
        assert_eq!(within(&project), whole(&project));
        assert_eq!(within(&project).0.len(), 1);
        // the last block is said to start past the folder, so only the one before it would be read
        let path = project.own_folder().join("state");
        let bytes = fs::read(&path).unwrap();
        let index = bytes
            .windows(10)
            .rposition(|window| window == br#","blocks":"#)
            .unwrap()
            + 10;
        let mut blocks: Vec<(String, u64, u64)> =
            serde_json::from_slice(&bytes[index..bytes.len() - 1]).unwrap();
        assert!(blocks.len() > 2, "{} blocks", blocks.len());
        blocks.last_mut().unwrap().0 = "data/tinz".to_owned();
        let mut changed = bytes[..index].to_vec();
        changed.extend(serde_json::to_vec(&blocks).unwrap());
        changed.push(b'}');
        fs::write(&path, changed).unwrap();
        assert_eq!(within(&project), whole(&project));
        assert_eq!(within(&project).0.len(), 1);
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

    /// a rewrite keeps the mode a person gave the file, read-only too, but never a link's mode
    /// or a mode that runs as someone else.
    #[cfg(unix)]
    #[test]
    fn a_rewrite_keeps_the_mode_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, project) = project(&[]);
        let path = project.root.join(manifest::FILE);
        for (mode, kept) in [(0o444, 0o444), (0o640, 0o640), (0o4755, 0o755)] {
            let _ = fs::remove_file(&path);
            fs::write(&path, "# before\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            project.write(manifest::FILE, b"# after\n").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "# after\n");
            let now = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(format!("{now:o}"), format!("{kept:o}"), "{mode:o}");
        }
        fs::remove_file(&path).unwrap();
        let target = project.root.join("elsewhere");
        fs::write(&target, "# linked\n").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        project.write(manifest::FILE, b"# after\n").unwrap();
        let now = fs::symlink_metadata(&path).unwrap();
        assert!(now.is_file());
        assert_eq!(
            now.permissions().mode() & 0o111,
            0,
            "{:o}",
            now.permissions().mode()
        );
    }

    /// a read-only file is replaced and stays read-only, as on unix.
    #[cfg(windows)]
    #[test]
    fn a_rewrite_keeps_a_read_only_file_read_only() {
        let (_dir, project) = project(&[("data.toml", "# before\n")]);
        let path = project.root.join(manifest::FILE);
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        project.write(manifest::FILE, b"# after\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# after\n");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        project.write(manifest::FILE, b"# again\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "# again\n");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
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

    #[cfg(not(unix))]
    thread_local! {
        /// a look at a rewritten file right as its new bytes take its name.
        static RENAMED: std::cell::Cell<Option<fn(&Path)>> = const { std::cell::Cell::new(None) };
    }

    #[cfg(not(unix))]
    pub(super) fn renamed(path: &Path) {
        if let Some(look) = RENAMED.take() {
            look(path);
        }
    }

    /// a read-only file is read-only the moment its new bytes take its name, so no stop right
    /// after the rename leaves it writable.
    #[cfg(windows)]
    #[test]
    fn a_read_only_file_is_read_only_as_its_new_bytes_take_its_name() {
        let (_dir, project) = project(&[("data.toml", "# before\n")]);
        let path = project.root.join(manifest::FILE);
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        RENAMED.set(Some(|path: &Path| {
            assert!(
                fs::metadata(path).unwrap().permissions().readonly(),
                "{} was writable once renamed",
                path.display()
            );
        }));
        project.write(manifest::FILE, b"# after\n").unwrap();
        assert!(RENAMED.take().is_none(), "the rename was not looked at");
        assert_eq!(fs::read_to_string(&path).unwrap(), "# after\n");
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

    /// a `.fetchloom` link beside a file, as in a cloned data.lock.d, is never a folder to stage the
    /// new file in: it is staged beside its name, and nothing is made or flushed through the link.
    #[cfg(unix)]
    #[test]
    fn a_linked_metadata_folder_is_never_staged_in() {
        let (dir, _) = project(&[("data.lock.d/old.tsv.zst", "")]);
        let outside = tempfile::tempdir().unwrap();
        let folder = dir.path().join("data.lock.d");
        std::os::unix::fs::symlink(outside.path(), folder.join(FOLDER)).unwrap();
        DIRECTORY_SYNCS.with_borrow_mut(Vec::clear);
        write_atomically(&folder.join("new.tsv.zst"), b"rows").unwrap();
        assert_eq!(
            DIRECTORY_SYNCS.with_borrow_mut(std::mem::take),
            std::slice::from_ref(&folder)
        );
        assert_eq!(fs::read(folder.join("new.tsv.zst")).unwrap(), b"rows");
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
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
            [project.own_folder(), project.root]
        );
    }

    /// while data.lock names a list data.lock.d lacks, it is written only with that pin, by the same
    /// list, unless data.toml no longer has the dataset.
    #[test]
    fn a_write_never_drops_or_moves_the_pin_of_a_dataset_that_waits() {
        let root = tempfile::tempdir().unwrap();
        let project = Project::at(root.path().to_path_buf());
        let pinned = |name: &str, list: char| {
            format!(
                "version = 1\n\n[[dataset]]\nname = \"{name}\"\nref = \"zenodo:3608990\"\nspec = \"sha256:11\"\nresolved = \"zenodo:3608990\"\nlocked = \"2026-10-27T17:30:00Z\"\nfiles_in = [\"data.lock.d/{name}.{}.tsv.zst\"]\nfile_count = 2\nfile_bytes = 9\nfiles_sha256 = \"sha256:{}\"\n",
                list.to_string().repeat(12),
                list.to_string().repeat(64)
            )
        };
        let lock = root.path().join(lock::FILE);
        fs::write(&lock, pinned("iris", 'a')).unwrap();
        for (text, kept) in [
            ("version = 1\n".to_owned(), false),
            (pinned("other", 'a'), false),
            (pinned("iris", 'b'), false),
            (pinned("iris", 'a'), true),
        ] {
            let written = project.write(lock::FILE, text.as_bytes());
            assert_eq!(written.is_ok(), kept, "{text}");
            if !kept {
                let error = written.unwrap_err();
                assert!(error.message.contains("the file list of iris"), "{error:?}");
            }
        }
        fs::write(root.path().join(manifest::FILE), "").unwrap();
        project.write(lock::FILE, b"version = 1\n").unwrap();
    }
}
