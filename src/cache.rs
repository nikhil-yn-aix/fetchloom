//! the content-addressed cache: one read-only file per object, named by its SHA-256.

mod changed;
mod deferred;
mod failure;
mod folder;
mod held;
mod imported;
mod keyed;
mod object;
mod partial;
mod sharing;
mod staging;
mod unpacked;
mod upkeep;
mod urls;

#[cfg(test)]
mod link_tests;

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

pub(crate) use changed::{Doubt, Identity, may_follow};
pub(crate) use failure::Failure;
pub(crate) use held::Access;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use object::{Batch, batch_in};
pub(crate) use object::{Copied, NewObject, Sealed, holds_pinned, sha256_of};
#[cfg(test)]
pub(crate) use partial::partial_key;
pub(crate) use partial::{Claim, Hashing, Partial, PartialWriter, names};
use sharing::Sharing;
pub(crate) use staging::Staging;
pub(crate) use unpacked::{Member, members_of, parts_key, tree, tree_hash};
#[cfg(test)]
pub(crate) use upkeep::Object;
pub(crate) use upkeep::{Checked, Freed, GRACE, Needed, Scan, Unfinished};

use crate::digest::{self, Block, Md5, Sha256};
use crate::place::{FolderUse, found_folder, real_folder};
use crate::volume::{Volume, is_plain, open_unfollowed, read_unfollowed, refuses_links};

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# a cache made by fetchloom. backup tools can skip this folder.\n";
const FOLDERS: [&str; 10] = [
    "objects", "partial", "urls", "stated", "unpacked", "projects", "tmp", "changed", "imported",
    "keyed",
];

/// the file that keeps spotlight out of the cache on macos, made just before the tag.
const NEVER_INDEX: &str = ".metadata_never_index";

/// what a system keeps in a folder that holds nothing of a person's: a new disk's lost+found, and
/// what macos and windows put at a disk's top or in a folder someone looked at.
const SYSTEM_NAMES: [&str; 7] = [
    "lost+found",
    ".DS_Store",
    ".Spotlight-V100",
    ".fseventsd",
    ".Trashes",
    "System Volume Information",
    "$RECYCLE.BIN",
];

/// refuses `root` as the cache unless it holds fetchloom's tag, or nothing but what a system keeps
/// there, fetchloom's own state and what a first run stopped before its tag was whole leaves: the
/// start of the tag and macos's mark against indexing. gc clears old files from tmp/, and the tag
/// tells backup tools to skip the whole folder, which a folder of a person's own files must not
/// get, as a home named by mistake, or a scratch folder whose folders have a cache's names.
fn adoptable(root: &Path) -> io::Result<()> {
    if is_cache(root) {
        return Ok(());
    }
    meanwhile();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    for entry in entries {
        // a run making the cache at the same time writes its whole tag before any folder of it,
        // so a folder that has the tag by now is that cache
        if !left_before_tag(root, &entry?.file_name()) && !is_cache(root) {
            let untagged = lost_its_tag(root);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                Foreign { untagged },
            ));
        }
    }
    Ok(())
}

/// the refusal of a folder of other files as the cache, which an empty or a new folder avoids.
/// `untagged` is a folder that looks like a cache whose tag is gone, as [`lost_its_tag`] says.
#[derive(Debug)]
struct Foreign {
    untagged: bool,
}

impl std::fmt::Display for Foreign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.untagged {
            "it looks like a fetchloom cache whose tag is gone: it holds only what a cache holds, \
             but no CACHEDIR.TAG, the file fetchloom knows its cache by"
        } else {
            "it holds other files and is not a fetchloom cache, and fetchloom makes its cache only \
             in an empty folder or a new one"
        })
    }
}

impl std::error::Error for Foreign {}

/// whether `err` refuses a folder of other files as the cache.
pub(crate) fn is_foreign(err: &io::Error) -> bool {
    matches!(err.get_ref(), Some(inner) if inner.is::<Foreign>())
}

/// whether `root`, a folder without the tag, holds only what a cache holds, so it looks like a
/// cache whose tag is gone: a cleaner leaves one, and so does a build that made a cache's folders
/// before its tag, stopped between them. gc removes old files from tmp/, and the tag makes backup
/// tools skip the folder, so a folder whose objects/, tmp/ or projects/ holds a name fetchloom
/// never gives a file there, as a person's notes, is no such cache.
fn lost_its_tag(root: &Path) -> bool {
    let entries = |folder: &Path| -> Option<Vec<(String, fs::Metadata)>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(folder).ok()? {
            let entry = entry.ok()?;
            let metadata = entry.metadata().ok()?;
            found.push((entry.file_name().into_string().ok()?, metadata));
        }
        Some(found)
    };
    let system = |name: &str| SYSTEM_NAMES.contains(&name);
    let within = |folder: &str, holds: &dyn Fn(&str, &fs::Metadata) -> bool| {
        let folder = root.join(folder);
        match entries(&folder) {
            Some(found) => found
                .iter()
                .all(|(name, metadata)| system(name) || holds(name, metadata)),
            None => fs::symlink_metadata(&folder)
                .is_err_and(|err| err.kind() == io::ErrorKind::NotFound),
        }
    };
    let hex = |text: &str, digits: usize| {
        text.len() == digits && text.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    let Some(top) = entries(root) else {
        return false;
    };
    top.iter().all(|(name, found)| {
        system(name)
            || (FOLDERS.contains(&name.as_str()) && found.is_dir())
            || (["gc.lock", "reminded", NEVER_INDEX].contains(&name.as_str()) && found.is_file())
            || own_state(root, std::ffi::OsStr::new(name))
    }) && fs::symlink_metadata(root.join("objects")).is_ok_and(|found| found.is_dir())
        && within("objects", &|name, found| found.is_dir() && hex(name, 2))
        && within("projects", &|name, found| {
            found.is_file() && name.strip_suffix(".json").is_some_and(|stem| hex(stem, 64))
        })
        && within("tmp", &|name, _| {
            let name = name.strip_prefix(".fetchloom-").unwrap_or(name);
            name.split_once('.').is_some_and(|(stem, ending)| {
                hex(stem, 16)
                    && !ending.is_empty()
                    && ending.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        })
}

/// how to put the tag back in `root` when it looks like a cache whose tag is gone, as
/// [`lost_its_tag`] says: a command for the system's usual shell that makes the tag, and refuses a
/// file already at its name, a link included, so it never writes where a link leads.
pub(crate) fn tag_back(root: &Path) -> Option<String> {
    if !lost_its_tag(root) {
        return None;
    }
    let tag = root.join("CACHEDIR.TAG");
    let tag = crate::suggested::shell_word(&tag.to_string_lossy()).into_owned();
    let signature = TAG.lines().next().unwrap_or_default();
    let command = if cfg!(windows) {
        format!(
            "New-Item -ItemType File -Path {tag} -Value ('{signature}', '# a cache made by fetchloom' -join [char]10)"
        )
    } else {
        format!("(set -C; printf '{signature}\\n# a cache made by fetchloom\\n' > {tag})")
    };
    Some(format!(
        "if it is your fetchloom cache, put its tag back with `{command}` and run the command again"
    ))
}

/// whether `root` is a fetchloom cache: it holds fetchloom's tag, never only a cache's folder names.
pub(crate) fn is_cache(root: &Path) -> bool {
    tagged(root)
}

/// the file on the way to `root`, or `root` itself, where a folder of the cache's path must be,
/// which no run can make the cache through, said as a reason.
pub(crate) fn in_the_way(root: &Path) -> Option<String> {
    crate::error::file_in_the_way(root)
}

/// why a run would refuse `root` as the cache, read as it is with nothing created: none for a
/// cache, an empty folder or one not there yet.
pub(crate) fn refused(root: &Path) -> Option<String> {
    in_the_way(root).or_else(|| {
        Cache::unopened(root.to_path_buf())
            .err()
            .map(|err| crate::error::said(&err))
    })
}

/// whether `name` in `root` is what a system keeps in a folder, fetchloom's own state, or what a
/// first run stopped before its tag was whole leaves: the start of the tag and macos's mark against
/// indexing.
fn left_before_tag(root: &Path, name: &std::ffi::OsStr) -> bool {
    SYSTEM_NAMES.iter().any(|system| name == *system)
        || name == NEVER_INDEX
        || (name == "CACHEDIR.TAG" && tag_begun(root))
        || own_state(root, name)
}

/// whether `name` in `root` is the folder fetchloom keeps what it remembers on this machine in,
/// which windows keeps in the default cache folder, `%LOCALAPPDATA%\fetchloom\state`, so a run with
/// the cache elsewhere makes it before any cache there: a real folder holding only its files.
fn own_state(root: &Path, name: &std::ffi::OsStr) -> bool {
    let folder = root.join(name);
    name == crate::machine::STATE
        && fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir())
        && fs::read_dir(&folder).is_ok_and(|mut entries| {
            entries.all(|entry| {
                entry.is_ok_and(|entry| {
                    crate::machine::kept_in_state(&entry.file_name())
                        && entry.file_type().is_ok_and(|kind| kind.is_file())
                })
            })
        })
}

/// whether `root` holds no cache yet: it is missing, or holds no tag and nothing but what
/// [`left_before_tag`] passes. a command that only reads or frees a cache makes none there, where
/// another makes one.
pub(crate) fn absent(root: &Path) -> bool {
    if tagged(root) {
        return false;
    }
    match fs::read_dir(root) {
        Ok(mut entries) => {
            entries.all(|entry| entry.is_ok_and(|entry| left_before_tag(root, &entry.file_name())))
        }
        Err(err) => err.kind() == io::ErrorKind::NotFound,
    }
}

/// what lets others change the files projects place from the cache folder at `root`: it, or a
/// folder fetchloom writes in inside it that every user may reach, belongs to another user, or
/// every user may write to it. on windows, where a folder takes its access from the folders above
/// it, a folder outside the user's profile, unless on a FAT or exFAT drive, which keeps no
/// permissions to change and is made to be shared.
pub(crate) fn exposed(root: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let user = rustix::process::geteuid().as_raw();
        let groups = rustix::process::getgroups().unwrap_or_default();
        let group = rustix::process::getegid();
        exposed_as(root, user, &|gid| {
            group.as_raw() == gid || groups.iter().any(|member| member.as_raw() == gid)
        })
    }
    #[cfg(not(unix))]
    exposed_on(root)
}

/// [`exposed`] as the user `user` sees it, who `member` says is in a group or not.
#[cfg(unix)]
fn exposed_as(root: &Path, user: u32, member: &dyn Fn(u32) -> bool) -> Option<String> {
    {
        use std::os::unix::fs::MetadataExt;
        // a folder a group shares on purpose, as the clusters guide makes a lab's cache: setgid,
        // writable by its group, which this user is in, and by no one else
        let shared = |found: &fs::Metadata| found.mode() & 0o2022 == 0o2020 && member(found.gid());
        let found = fs::metadata(root).ok()?;
        if let Some(what) = exposure(found.uid(), found.mode(), user, shared(&found)) {
            return Some(format!(
                "the cache folder {} {what}, so they can change the files projects place from it",
                root.display()
            ));
        }
        // no one else reaches what a folder only its owner and group may enter holds
        if found.mode() & 0o001 == 0 {
            return None;
        }
        let told = |folder: &Path, what: &str| {
            format!(
                "the folder {} in the cache {what}, so they can change the files projects place from it",
                folder.display()
            )
        };
        let inner = FOLDERS.iter().find_map(|name| {
            let folder = root.join(name);
            let inside = fs::symlink_metadata(&folder)
                .ok()
                .filter(fs::Metadata::is_dir)?;
            exposure(inside.uid(), inside.mode(), user, shared(&inside))
                .map(|what| told(&folder, what))
        });
        // a fan of objects/ every user may write, as `umask 000` makes them, lets them put any
        // bytes under an object's name; each member of a team may own some of them
        inner.or_else(|| {
            let objects = root.join("objects");
            if fs::metadata(&objects).ok()?.mode() & 0o001 == 0 {
                return None;
            }
            fs::read_dir(&objects).ok()?.flatten().find_map(|fan| {
                let found = fan.metadata().ok().filter(fs::Metadata::is_dir)?;
                (found.mode() & 0o002 != 0)
                    .then(|| told(&fan.path(), "lets every user of this machine write to it"))
            })
        })
    }
}

/// [`exposed`] where folders have no owner and group of their own.
#[cfg(not(unix))]
fn exposed_on(root: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use crate::space::Filesystem;
        let shared = !crate::machine::in_profile(&crate::machine::env, root)
            && !matches!(Filesystem::of(root), Filesystem::Fat | Filesystem::Exfat);
        shared.then(|| {
            format!(
                "the cache folder {} is outside your user profile and takes its permissions from \
                 the folders above it, so other users of this machine may be able to change the \
                 files projects place from it",
                root.display()
            )
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = root;
        None
    }
}

/// what keeps the cache at `root`, which its group shares, from staying one the group may add to:
/// a folder in it that lets only its owner write to it, or `umask`, this run's on linux, which
/// takes the group's write from what the run makes. none for a cache that is no group's, whose
/// folder is not setgid and writable by its group, as the clusters guide makes a lab's.
#[cfg(unix)]
pub(crate) fn unshared(root: &Path, umask: Option<u32>) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if fs::metadata(root).ok()?.mode() & 0o2020 != 0o2020 {
        return None;
    }
    let fans = ["objects", "keyed"]
        .iter()
        .flat_map(|top| fs::read_dir(root.join(top)).into_iter().flatten().flatten())
        .map(|fan| fan.path());
    let closed = FOLDERS
        .iter()
        .map(|name| root.join(name))
        .chain(fans)
        .find(|folder| {
            fs::symlink_metadata(folder)
                .is_ok_and(|found| found.is_dir() && found.mode() & 0o020 == 0)
        });
    if let Some(folder) = closed {
        return Some(format!(
            "the folder {} in the cache lets only its owner write to it, though a group shares the cache, so the group's other members cannot add to it",
            folder.display()
        ));
    }
    let umask = umask.filter(|umask| umask & 0o020 != 0)?;
    Some(format!(
        "this run's umask, {umask:03o}, keeps the group from writing what it adds to the cache {}, which a group shares",
        root.display()
    ))
}

/// [`exposed`] for a folder of `owner` with `mode`, as `user` sees it; `shared` when a group
/// shares it on purpose, so whose it is matters no more than it does to a git repository's group.
#[cfg(unix)]
fn exposure(owner: u32, mode: u32, user: u32, shared: bool) -> Option<&'static str> {
    if owner != user && !shared {
        Some("belongs to another user")
    } else if mode & 0o002 != 0 {
        Some("lets every user of this machine write to it")
    } else {
        None
    }
}

/// the start of `CACHEDIR.TAG` in `root`, read without following a link.
fn tag_text(root: &Path) -> Option<String> {
    let mut text = String::new();
    read_unfollowed(&root.join("CACHEDIR.TAG"))
        .and_then(|file| file.take(4096).read_to_string(&mut text))
        .ok()?;
    Some(text)
}

/// whether `root` holds the tag fetchloom writes in its cache, perhaps after the byte order mark
/// some windows tools start a file with.
fn tagged(root: &Path) -> bool {
    tag_text(root).is_some_and(|text| {
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        text.lines().next() == TAG.lines().next() && text.contains("fetchloom")
    })
}

/// whether `root` holds the start of fetchloom's tag, as a run stopped while writing it leaves.
fn tag_begun(root: &Path) -> bool {
    tag_text(root).is_some_and(|text| TAG.starts_with(&text))
}

/// writes the tag backup tools look for with `write`, unless one is there, before any folder of
/// the cache, since a folder without it is no cache; true when this call wrote it. a failed write
/// passes once a whole tag is in place, as another run opening the cache may have written it
/// meanwhile.
fn tag(root: &Path, write: impl FnOnce(&Path, &[u8]) -> io::Result<()>) -> io::Result<bool> {
    if tagged(root) {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    exclude_native_backup_and_indexing(root)?;
    match write(&root.join("CACHEDIR.TAG"), TAG.as_bytes()) {
        Err(_) if tagged(root) => Ok(false),
        written => written.map(|()| true),
    }
}

/// makes the folder `path` of the cache at `root` where missing, as [`real_folder`] does: one it
/// makes gets what `sharing` gives the group the cache is shared with.
fn make_folder(root: &Path, path: &Path, sharing: Sharing) -> io::Result<()> {
    let missing = sharing.is_shared()
        && fs::symlink_metadata(path).is_err_and(|err| err.kind() == io::ErrorKind::NotFound);
    real_folder(path, FolderUse::Cache)?;
    if missing {
        sharing.share_folder(root, path);
    }
    Ok(())
}

/// writes `bytes` over the start of the file at `path`, made if missing, never through a link nor
/// into a file with another name. a tag a stopped run cut short holds the start of the same bytes,
/// so the file never holds others, whichever runs write it at once.
fn write_over(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_unfollowed(path, true)?;
    // a co-user of an empty shared cache, where anyone may link any file, could link one of yours
    if name_count(&file)? > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} has another name, as a hardlink to a file elsewhere does, so fetchloom never writes it",
                path.display()
            ),
        ));
    }
    file.write_all(bytes)?;
    file.sync_all()
}

/// how many names the open `file` has.
fn name_count(file: &File) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        Ok(winapi_util::file::information(file)?.number_of_links())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(1)
    }
}

#[cfg(test)]
thread_local! {
    /// what a test does once a record's folder in the cache is looked at and before the record is
    /// renamed, read or removed there, as someone sharing the cache swapping that folder or that
    /// record for a link meanwhile would, once.
    static MEANWHILE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// runs what a test asked to happen at this point, once.
fn meanwhile() {
    #[cfg(test)]
    if let Some(swap) = MEANWHILE.take() {
        swap();
    }
}

/// has `swap` happen once a test's next record or object is looked at, as [`meanwhile`] runs it,
/// for a test outside the cache's own.
#[cfg(all(test, unix))]
pub(crate) fn swap_meanwhile(swap: impl FnOnce() + 'static) {
    MEANWHILE.set(Some(Box::new(swap)));
}

/// whether the swap a test asked [`swap_meanwhile`] for happened.
#[cfg(all(test, unix))]
pub(crate) fn swapped() -> bool {
    MEANWHILE.with_borrow(Option::is_none)
}

/// the room the cache's disk had free as the archives this run unpacks at once began, which they
/// share, so archives unpacking together never each count on all of it.
#[derive(Debug, Default)]
pub(crate) struct Room {
    state: std::sync::Mutex<Rooms>,
    /// what a test says the disk has free.
    #[cfg(test)]
    said: Option<u64>,
}

/// the unpacks running at once, the room the disk had free as the first began, and the whole
/// blocks they took of it.
#[derive(Debug, Default)]
struct Rooms {
    running: usize,
    free: Option<u64>,
    taken: u64,
}

impl Room {
    /// a part of the room for one unpack, measured by `free` when no other unpack is running.
    pub(crate) fn share(&self, free: impl FnOnce() -> Option<u64>) -> Share<'_> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(test)]
        let said = self.said;
        #[cfg(not(test))]
        let said = None;
        if state.running == 0 {
            state.free = said.or_else(free);
            state.taken = 0;
        }
        state.running += 1;
        Share {
            room: self,
            free: state.free,
            own: std::cell::Cell::new(0),
            kept: std::cell::Cell::new(false),
        }
    }
}

/// one unpack's part of a [`Room`]: what it took is given back unless [`Share::keep`] says its
/// files were kept.
#[derive(Debug)]
pub(crate) struct Share<'r> {
    room: &'r Room,
    free: Option<u64>,
    own: std::cell::Cell<u64>,
    kept: std::cell::Cell<bool>,
}

impl Share<'_> {
    /// what the disk had free as the first of the unpacks running at once began, none unknown.
    pub(crate) fn free(&self) -> Option<u64> {
        self.free
    }

    /// takes `more` bytes of the room; false, with them taken still, once the unpacks running at
    /// once took more than the disk had free, and whether others took any of it.
    pub(crate) fn take(&self, more: u64) -> (bool, bool) {
        let mut state = self
            .room
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.taken = state.taken.saturating_add(more);
        self.own.set(self.own.get().saturating_add(more));
        let fits = self.free.is_none_or(|free| state.taken <= free);
        (fits, state.taken > self.own.get())
    }

    /// keeps what this unpack took counted, as the files it wrote stay in the cache.
    pub(crate) fn keep(&self) {
        self.kept.set(true);
    }
}

impl Drop for Share<'_> {
    fn drop(&mut self) {
        let mut state = self
            .room
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.running = state.running.saturating_sub(1);
        if !self.kept.get() {
            state.taken = state.taken.saturating_sub(self.own.get());
        }
    }
}

impl Cache {
    /// a part of the room the cache's disk has for one unpack, which archives this run unpacks at
    /// once share.
    pub(crate) fn share_room(&self) -> Share<'_> {
        self.room.share(|| crate::space::free(&self.root))
    }

    /// this cache, as if its disk had `free` bytes free for unpacking.
    #[cfg(test)]
    pub(crate) fn with_free_room(self, free: u64) -> Cache {
        let room = Arc::new(Room {
            said: Some(free),
            ..Room::default()
        });
        Cache { room, ..self }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Cache {
    root: PathBuf,
    /// the folders under `objects/`, by their hashes' first byte, this run saw to be real folders.
    checked: Arc<[AtomicBool; 256]>,
    volume: Arc<Volume>,
    /// downloads waiting for a flush that covers them, shared by every clone.
    deferred: Arc<deferred::Deferred>,
    /// the cache-wide lock this run holds, let go when the last clone is dropped.
    held: Option<Arc<File>>,
    /// the url records as this run read them, shared by every clone.
    urls: Arc<urls::Shards>,
    /// the room the cache's disk had free for the archives this run unpacks at once.
    room: Arc<Room>,
    /// what the group the cache is shared with gets of what this run makes in it.
    sharing: Sharing,
    /// on windows, what removes the cache's files, shared by every clone.
    #[cfg(not(unix))]
    remover: Arc<Remover>,
    /// whether downloads may lock their claims, which a test turns off to stand for a disk that
    /// cannot lock files.
    #[cfg(test)]
    locks: bool,
}

/// the placer windows removes the cache's files through, made at the first removal and kept, so
/// each removal does not open the cache's root and the folders above it again.
#[cfg(not(unix))]
#[derive(Default)]
// it keeps only folder handles, which a panic cannot leave half changed
struct Remover(std::panic::AssertUnwindSafe<std::sync::OnceLock<crate::place::Placer>>);

#[cfg(not(unix))]
impl Remover {
    fn placer(&self, root: &Path) -> &crate::place::Placer {
        self.0
            .get_or_init(|| crate::place::Placer::in_named_folder(root))
    }
}

#[cfg(not(unix))]
impl std::fmt::Debug for Remover {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Remover")
    }
}

/// a file's bytes on their way into the cache from a folder on this machine, and how they came.
pub(crate) struct Imported {
    pub(crate) sealed: Sealed,
    /// whether the object is a clone sharing the file's blocks, which takes no room of its own.
    pub(crate) cloned: bool,
}

#[derive(Debug)]
pub(crate) struct Stored {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    /// the MD5 of the bytes, when it was computed to check one a source stated.
    pub(crate) md5: Option<Md5>,
    /// false when the cache already held these bytes.
    pub(crate) new: bool,
}

/// how a server identified the bytes it sent, so a later request can ask whether they changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Validators {
    #[serde(deserialize_with = "Option::deserialize")]
    pub(crate) etag: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub(crate) last_modified: Option<String>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    pub(crate) date: Option<String>,
    /// the version an S3 bucket that keeps versions named for the bytes, `x-amz-version-id`.
    #[serde(deserialize_with = "Option::deserialize")]
    pub(crate) version: Option<String>,
}

/// the bytes a link served when they matched the MD5 its source states, and the S3 version they
/// were, when its bucket keeps versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stated {
    pub(crate) digest: Sha256,
    pub(crate) size: u64,
    pub(crate) version: Option<String>,
}

/// what a URL served the last time it was downloaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UrlRecord {
    /// the live URL; its on-disk value is a fingerprint so credentials do not enter the index.
    #[serde(serialize_with = "serialize_url")]
    pub(crate) url: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
    /// the file name the server gave, when it gave one.
    #[serde(deserialize_with = "Option::deserialize")]
    pub(crate) name: Option<String>,
    pub(crate) validators: Validators,
    /// whether the bytes came as a web page, which counts as the file only when asked for again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) page: bool,
}

fn url_identity(url: &str) -> String {
    format!("sha256:{}", digest::sha256(url.as_bytes()).hex())
}

fn serialize_url<S: serde::Serializer>(url: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&url_identity(url))
}

fn recorded_url_matches(recorded: &str, url: &str) -> bool {
    recorded == url_identity(url)
}

impl Cache {
    /// the cache folder: `FETCHLOOM_CACHE` when set, else `configured`, the folder the settings
    /// file names, else the platform's cache folder.
    pub(crate) fn locate(
        var: &dyn Fn(&str) -> Option<OsString>,
        configured: Option<&Path>,
    ) -> Option<PathBuf> {
        let set = |name: &str| {
            var(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        if let Some(path) = set("FETCHLOOM_CACHE") {
            return Some(path);
        }
        if let Some(path) = configured {
            return Some(path.to_path_buf());
        }
        if cfg!(windows) {
            set("LOCALAPPDATA").map(|path| path.join("fetchloom"))
        } else if cfg!(target_os = "macos") {
            // where macos keeps caches, which time machine skips
            set("HOME").map(|home| home.join("Library").join("Caches").join("fetchloom"))
        } else {
            // the XDG spec has a relative value ignored, which would put a cache in every folder
            set("XDG_CACHE_HOME")
                .filter(|path| path.is_absolute())
                .or_else(|| set("HOME").map(|home| home.join(".cache")))
                .map(|path| path.join("fetchloom"))
        }
    }

    /// opens the cache at `root`, creating its folders. a cache folder it makes is this user's
    /// alone on unix, as gated datasets land in it; one that exists, as a team's, keeps its own.
    pub(crate) fn open(root: PathBuf) -> io::Result<Cache> {
        Cache::open_tagging(root, write_over)
    }

    /// [`Cache::open`], writing a new cache's tag with `write`.
    fn open_tagging(
        root: PathBuf,
        write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
    ) -> io::Result<Cache> {
        let _span = tracing::debug_span!("open_cache").entered();
        #[cfg(unix)]
        if !root.exists() {
            use std::os::unix::fs::DirBuilderExt;
            if let Some(parent) = root.parent() {
                fs::create_dir_all(parent)?;
            }
            // only the cache's own folder: parents, as a team's shared folder, keep the umask
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Err(err) if err.kind() != io::ErrorKind::AlreadyExists => return Err(err),
                _ => {}
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&root)?;
        adoptable(&root)?;
        let sharing = Sharing::of(&root);
        if tag(&root, write)? {
            sharing.share_file(&root.join("CACHEDIR.TAG"));
            #[cfg(target_os = "macos")]
            sharing.share_file(&root.join(NEVER_INDEX));
        }
        for folder in FOLDERS {
            make_folder(&root, &root.join(folder), sharing)?;
        }
        Ok(Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            root,
            checked: Arc::new(std::array::from_fn(|_| AtomicBool::new(false))),
            deferred: Arc::default(),
            held: None,
            urls: Arc::new(urls::Shards::shared(sharing)),
            room: Arc::default(),
            sharing,
            #[cfg(not(unix))]
            remover: Arc::default(),
            #[cfg(test)]
            locks: true,
        })
    }

    /// the cache at `root` as it is, creating nothing, for a dry run: a missing cache or folder
    /// holds nothing, and what [`Cache::open`] refuses is refused here too.
    pub(crate) fn unopened(root: PathBuf) -> io::Result<Cache> {
        match fs::metadata(&root) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("{} is a file where the cache should be", root.display()),
                ));
            }
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        adoptable(&root)?;
        for folder in FOLDERS {
            let path = root.join(folder);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => found_folder(&path, &metadata, FolderUse::Cache)?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        let sharing = Sharing::of(&root);
        Ok(Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            root,
            checked: Arc::new(std::array::from_fn(|_| AtomicBool::new(false))),
            deferred: Arc::default(),
            held: None,
            urls: Arc::new(urls::Shards::shared(sharing)),
            room: Arc::default(),
            sharing,
            #[cfg(not(unix))]
            remover: Arc::default(),
            #[cfg(test)]
            locks: true,
        })
    }

    /// the folder an unfinished download keyed `key` sits in: one of 16 under `partial/`, by the
    /// key's first character, so downloads running at once do not all wait on one folder.
    pub(super) fn partial_folder(&self, key: &str) -> PathBuf {
        self.root
            .join("partial")
            .join(key.get(..1).unwrap_or_default())
    }

    /// the folders unfinished downloads sit in: the shards under `partial/`. a link in place of
    /// `partial/` or of a shard is never entered, so nothing outside the cache is counted or swept.
    pub(super) fn partial_folders(&self) -> Vec<PathBuf> {
        let folder = self.root.join("partial");
        if !fs::symlink_metadata(&folder).is_ok_and(|found| found.is_dir()) {
            return Vec::new();
        }
        let mut folders = Vec::new();
        if let Ok(entries) = fs::read_dir(&folder) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let shard = name.len() == 1
                    && name
                        .to_str()
                        .is_some_and(|name| name.chars().all(|c| c.is_ascii_hexdigit()));
                if shard && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    folders.push(entry.path());
                }
            }
        }
        folders
    }

    /// this cache, with every removal recorded instead of made, so gc can say what it would free.
    pub(crate) fn pretending(&self) -> Cache {
        Cache {
            volume: Arc::new(Volume::pretend(self.root.join("tmp"))),
            ..self.clone()
        }
    }

    /// this cache, as on a disk that cannot lock files, for tests.
    #[cfg(test)]
    pub(crate) fn without_locks(self) -> Cache {
        Cache {
            locks: false,
            ..self
        }
    }

    /// what a [`Cache::pretending`] cache would have removed, in order.
    pub(crate) fn pretended(&self) -> Vec<PathBuf> {
        self.volume.pretended()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn object_path(&self, digest: &Sha256) -> PathBuf {
        let hex = digest.hex();
        self.root.join("objects").join(&hex[..2]).join(&hex[2..])
    }

    /// the newest change, in nanoseconds since 1970, of the folders the objects for `digests`
    /// would sit in, by one look at each, none of which counts as 0. placing or removing an
    /// object changes its folder's time, so while this stays the same no object came or went.
    pub(crate) fn newest_folder<'d>(&self, digests: impl IntoIterator<Item = &'d Sha256>) -> u64 {
        let mut fans = [false; 256];
        for digest in digests {
            fans[usize::from(digest.bytes()[0])] = true;
        }
        let objects = self.root.join("objects");
        fans.iter()
            .enumerate()
            .filter(|(_, wanted)| **wanted)
            .filter_map(|(fan, _)| fs::symlink_metadata(objects.join(format!("{fan:02x}"))).ok())
            .filter_map(|folder| folder.modified().ok())
            .map(crate::units::nanos)
            .max()
            .unwrap_or(0)
    }

    /// the object's path, when the cache holds it. a known size must match, and writable,
    /// marked or retimed objects must still hash to their names.
    pub(crate) fn find(&self, digest: &Sha256, size: Option<u64>) -> Option<PathBuf> {
        let path = self.object_path(digest);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !is_plain(&metadata) || size.is_some_and(|size| metadata.len() != size) {
            return None;
        }
        if changed::doubted(&path, &metadata, self.changed_at(digest).ok()?)
            && !changed::vouch(&path, &metadata, digest).ok()?
        {
            return None;
        }
        Some(path)
    }

    /// the object holding `digest`'s bytes, opened to read through its fan folder held from the
    /// cache's root, never through a link, and judged as [`Cache::find`] judges an object, by the
    /// file opened: one that also has another name, as a hardlink someone sharing the cache put
    /// there has, must hash to its name too. `NotFound` when the cache does not hold the bytes,
    /// `InvalidData` when the file there no longer hashes to its name, which is then marked.
    pub(crate) fn open_object(&self, digest: &Sha256, size: Option<u64>) -> io::Result<File> {
        let hex = digest.hex();
        let gone = || io::Error::from(io::ErrorKind::NotFound);
        let below = Path::new("objects").join(&hex[..2]);
        let folder = folder::Folder::existing(&self.root, &below, self.sharing)?;
        let file = folder.read_file(&hex[2..])?;
        let metadata = file.metadata()?;
        if size.is_some_and(|size| metadata.len() != size) {
            return Err(gone());
        }
        let marked = self.changed_at(digest).map_err(|_| gone())?;
        if (changed::doubted(&folder.join(&hex[2..]), &metadata, marked) || name_count(&file)? > 1)
            && Sha256::of_reader(&file, Block::sized(metadata.len().saturating_add(1)))? != *digest
        {
            // so the next look at it doubts it and it downloads again, not trusted by its name
            let _ = self.mark_changed(digest);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "its bytes no longer hash to its name",
            ));
        }
        meanwhile();
        Ok(file)
    }

    /// a new object in `staging` with the bytes `source`, a person's file opened without following
    /// a link, had at the start: cloned where the disk can, else copied, never linked, which would
    /// seal their file read-only. sealed, and flushed if `flush`, it waits for [`Sealed::publish`].
    ///
    /// # Errors
    ///
    /// `source` is no plain file or cannot be read, the cache cannot be written, or `stop` is
    /// asked, which gives `Interrupted`.
    pub(crate) fn import_file(
        &self,
        mut source: File,
        staging: &Arc<Staging>,
        flush: bool,
        stop: &crate::stop::Stop,
    ) -> io::Result<Imported> {
        let metadata = source.metadata()?;
        if !is_plain(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a link or a special file, which fetchloom never imports",
            ));
        }
        let size = metadata.len();
        let written = |err| self.explained(&self.root.join("tmp"), err);
        let mut object = staging.new_object().map_err(written)?;
        // a clone is hashed whole: its file holds every byte it shares, however many there are
        #[cfg(target_os = "linux")]
        let cloned = object.clone_from(&source, u64::MAX, size.saturating_add(1), stop)?;
        #[cfg(not(target_os = "linux"))]
        let cloned = false;
        if !cloned {
            match object.copy_from(&mut source, size, stop) {
                Ok(()) => {}
                Err(object::Fault::Source(err)) => return Err(err),
                Err(object::Fault::Cache(err)) => return Err(written(err)),
            }
        }
        Ok(Imported {
            sealed: object.seal(flush).map_err(written)?,
            cloned,
        })
    }

    /// removes an object the cache no longer needs to hold, such as an archive whose files are
    /// all unpacked, through its fan folder held from the cache's root, so a fan folder someone
    /// swapped for a link sends the removal nowhere else. files placed from it keep their bytes.
    pub(crate) fn drop_object(&self, digest: &Sha256) -> io::Result<()> {
        match self.remove_inside(&self.object_path(digest)) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
    }

    /// `err`, from writing in `folder`, in words that say what to fix: which folder refused, or
    /// what unfinished downloads hold of a full disk, neither of which the system says.
    pub(crate) fn explained(&self, folder: &Path, err: io::Error) -> io::Error {
        // an error fetchloom built already says what it concerns
        if err.raw_os_error().is_none() {
            return err;
        }
        let words = match err.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                format!("{} in {}", crate::error::said(&err), folder.display())
            }
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
                match self.unfinished_held() {
                    Some(held) => format!("{}; {held}", crate::error::said(&err)),
                    None => return err,
                }
            }
            _ => return err,
        };
        io::Error::new(err.kind(), Explained { words, cause: err })
    }

    /// what unfinished downloads no run holds take of the cache's disk and how to free it, when
    /// they take anything: the ones a run holds are its own to finish or delete.
    pub(crate) fn unfinished_held(&self) -> Option<String> {
        let unfinished = self.unfinished_unheld();
        (unfinished.bytes > 0).then(|| {
            format!(
                "unfinished downloads in {} hold {} of it, which `fetchloom cache gc --now` frees",
                self.root.join("partial").display(),
                crate::units::size(unfinished.bytes)
            )
        })
    }

    /// hashes an object again, marking changed bytes, which stay until a good copy arrives and
    /// make projects check their placed hardlinks. a read error changes nothing.
    pub(crate) fn verify(&self, digest: &Sha256) -> io::Result<bool> {
        let path = self.object_path(digest);
        match hash_object(&path, None) {
            Ok(actual) if actual == *digest => Ok(true),
            Ok(_) => {
                self.mark_changed(digest)?;
                Ok(false)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn url_record(&self, url: &str) -> Option<UrlRecord> {
        self.urls.find(&self.root, url)
    }

    pub(crate) fn save_url_record(&self, record: &UrlRecord) -> io::Result<()> {
        self.urls.save(&self.root, record)
    }

    /// the object holding the bytes `url` served when an earlier download hashed them to this
    /// MD5. another link stating the same MD5 may serve other bytes, since MD5 collides.
    pub(crate) fn stated_md5(&self, md5: &Md5, url: &str) -> Option<(Sha256, u64)> {
        self.stated(md5, url)
            .map(|stated| (stated.digest, stated.size))
    }

    /// [`Cache::stated_md5`], with the S3 version the bytes were.
    pub(crate) fn stated(&self, md5: &Md5, url: &str) -> Option<Stated> {
        // a record is a line; one someone put there as a link or a pipe is none
        let mut text = String::new();
        read_unfollowed(&self.stated_path(md5, url))
            .and_then(|file| file.take(4096).read_to_string(&mut text))
            .ok()?;
        let mut words = text.split_whitespace();
        Some(Stated {
            digest: Sha256::from_hex(words.next()?)?,
            size: words.next()?.parse().ok()?,
            version: words.next().map(str::to_owned),
        })
    }

    pub(crate) fn save_stated_md5(&self, md5: &Md5, url: &str, stated: &Stated) -> io::Result<()> {
        let path = self.stated_path(md5, url);
        make_folder(
            &self.root,
            &self.root.join("stated").join("md5"),
            self.sharing,
        )?;
        if let Some(parent) = path.parent() {
            make_folder(&self.root, parent, self.sharing)?;
        }
        let line = match stated {
            Stated {
                digest,
                size,
                version: Some(version),
            } => format!("{digest} {size} {version}\n"),
            Stated { digest, size, .. } => format!("{digest} {size}\n"),
        };
        self.write_hint(&path, line.as_bytes())
    }

    fn stated_path(&self, md5: &Md5, url: &str) -> PathBuf {
        let hex = md5.hex();
        let link = digest::sha256(url.as_bytes());
        self.root
            .join("stated")
            .join("md5")
            .join(&hex[..2])
            .join(format!("{}-{link}", &hex[2..]))
    }

    /// replaces `path` in one step, so a reader sees the old file or the new, never half of one.
    fn write_atomically(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_file(path, bytes, true)
    }

    /// replaces `path` in one step without flushing it first. for records whose loss after a power
    /// cut costs only a download or a resume, and which read as absent when torn. dropping their
    /// flushes halved fetching 1,000 files of 100 KiB from a server on the same machine.
    fn write_hint(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        self.replace_file(path, bytes, false)
    }

    /// writes `bytes` to a new file in tmp/ and renames it to `path`, both through folders held
    /// from the cache's root, so neither goes elsewhere when someone sharing the cache swaps a
    /// folder on the way for a link meanwhile.
    fn replace_file(&self, path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
        let parent = path.parent().unwrap_or(&self.root);
        let scratch = self.root.join("tmp");
        #[cfg(unix)]
        {
            let below = parent.strip_prefix(&self.root).ok();
            let name = path.file_name().and_then(std::ffi::OsStr::to_str);
            let (Some(below), Some(name)) = (below, name) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is not a record in the cache", path.display()),
                ));
            };
            let folder = folder::Folder::open(&self.root, below, self.sharing)
                .map_err(|err| self.explained(parent, err))?;
            let tmp = folder::Folder::open(&self.root, Path::new("tmp"), self.sharing)
                .map_err(|err| self.explained(&scratch, err))?;
            let (temporary, file) = loop {
                let temporary = format!("{:016x}.tmp", fastrand::u64(..));
                match tmp.create_new_with(&temporary, 0o666) {
                    Ok(file) => break (temporary, file),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(self.explained(&scratch, err)),
                }
            };
            let written = fill_and_rename_with(
                file,
                bytes,
                [&tmp.join(&temporary), path],
                durable,
                File::sync_all,
                |_, _| {
                    meanwhile();
                    tmp.rename_into(&temporary, &folder, name)
                },
                |at| {
                    if at == scratch {
                        tmp.flush()
                    } else {
                        folder.flush()
                    }
                },
            );
            if written.is_err() {
                let _ = tmp.remove(&temporary);
            }
            written.map_err(|err| self.explained(parent, err))
        }
        #[cfg(not(unix))]
        {
            real_folder(parent, FolderUse::Cache).map_err(|err| self.explained(parent, err))?;
            let placer = self.remover.placer(&self.root);
            let (temporary, file) = loop {
                let temporary = scratch.join(format!("{:016x}.tmp", fastrand::u64(..)));
                match placer.create_file(&temporary, false) {
                    Ok(file) => break (temporary, file),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(self.explained(&scratch, err)),
                }
            };
            let written = fill_and_rename_with(
                file,
                bytes,
                [&temporary, path],
                durable,
                File::sync_all,
                |from, to| {
                    meanwhile();
                    placer.rename_file(from, to)
                },
                crate::volume::sync_directory,
            );
            if written.is_err() {
                let _ = placer.delete_file(&temporary);
            }
            written.map_err(|err| self.explained(parent, err))
        }
    }

    /// moves a sealed file into the cache under its hash. the link never replaces an intact
    /// object, so of two processes racing for the same bytes exactly one places them.
    fn link_object(
        &self,
        source: &Path,
        digest: Sha256,
        size: u64,
        md5: Option<Md5>,
    ) -> io::Result<Stored> {
        let path = self.object_path(&digest);
        let first = usize::from(digest.bytes()[0]);
        if !self.checked[first].load(Ordering::Relaxed) {
            if let Some(folder) = path.parent() {
                make_folder(&self.root, folder, self.sharing)?;
            }
            self.checked[first].store(true, Ordering::Relaxed);
        }
        let new = self.link_new(source, &path, &digest, size)?;
        Ok(Stored {
            digest,
            size,
            md5,
            new,
        })
    }

    /// publishes `source` at `path` unless the object is there, then removes `source`; true when
    /// new. a file there with other bytes, as an edit through a hardlink leaves, is marked and
    /// replaced, lest every project place the edit; one that cannot be read stays.
    fn link_new(&self, source: &Path, path: &Path, digest: &Sha256, size: u64) -> io::Result<bool> {
        let volume = &self.volume;
        let new = match volume.hard_link(source, path) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
            // something removed the object's folder since this run checked it
            Err(err) if err.kind() == io::ErrorKind::NotFound && source.exists() => {
                if let Some(parent) = path.parent() {
                    make_folder(&self.root, parent, self.sharing)?;
                }
                return self.link_new(source, path, digest, size);
            }
            Err(_) => match volume.rename_new(source, path) {
                Ok(()) => return Ok(true),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
                // an object's name is its verified sha-256 and every writer seals its copy first,
                // so on a disk with no other way a rename that replaces swaps identical bytes
                Err(err) if refuses_links(&err) => {
                    volume.rename(source, path)?;
                    return Ok(true);
                }
                Err(err) => return Err(err),
            },
        };
        if !new {
            let found = fs::symlink_metadata(path);
            match existing(&found, size, || hash_object(path, Some(size)), digest) {
                Existing::Keep => {}
                Existing::Gone => return self.link_new(source, path, digest, size),
                Existing::Changed => {
                    // the mark doubts the object until the swap succeeds, so a failed repair never
                    // makes the changed bytes look like the replacement
                    if found.is_ok() {
                        self.mark_changed(digest)?;
                    }
                    let written = fs::metadata(source).and_then(|metadata| metadata.modified());
                    match volume.replace(source, path) {
                        Ok(()) => {
                            sync_publication(source, path, crate::volume::sync_directory)?;
                            // without the replacement's time the object stays in doubt and is
                            // hashed when found, which costs time but trusts nothing changed
                            if found.is_ok()
                                && let Ok(written) = written
                            {
                                let _ = self.mark_replaced(digest, written);
                            }
                            return Ok(true);
                        }
                        // another repair may have replaced the read-only destination while ours
                        // was being opened to clear its flag
                        Err(_)
                            if source.exists()
                                && hash_object(path, Some(size))
                                    .is_ok_and(|actual| actual == *digest) => {}
                        Err(err) => {
                            return Err(io::Error::new(
                                err.kind(),
                                format!("could not replace {}: {err}", path.display()),
                            ));
                        }
                    }
                }
            }
        }
        volume.remove(source)?;
        Ok(new)
    }
}

/// the help for a cache that is read-only for this account, as a lab's shared one may be: it takes
/// nothing new, so the help names a cache of the person's own.
pub(crate) const READ_ONLY: &str = "the cache is read-only for you, as a shared cache may be: set FETCHLOOM_CACHE to a folder you may write to, or ask whoever keeps the shared cache to add what you need";

/// whether `err`, from writing in the cache, says the cache is read-only for this account. on
/// windows a refused write is more often a file another program holds open.
pub(crate) fn read_only(err: &io::Error) -> bool {
    match err.kind() {
        io::ErrorKind::ReadOnlyFilesystem => true,
        io::ErrorKind::PermissionDenied => !cfg!(windows),
        _ => false,
    }
}

/// the error for a write to the cache that `err` refused, with [`READ_ONLY`] for a cache that is
/// read-only for this account.
pub(crate) fn unwritten(err: &io::Error) -> crate::error::Error {
    let error = crate::error::Error::io("could not write to the cache", err);
    if read_only(err) {
        return error.help(READ_ONLY);
    }
    error
}

#[cfg(target_os = "macos")]
const BACKUP_EXCLUSION: &str = "com.apple.metadata:com_apple_backup_excludeItem";

#[cfg(target_os = "macos")]
fn exclude_native_backup_and_indexing(root: &Path) -> io::Result<()> {
    match rustix::fs::setxattr(
        root,
        BACKUP_EXCLUSION,
        b"com.apple.backupd",
        rustix::fs::XattrFlags::CREATE,
    ) {
        Ok(()) | Err(rustix::io::Errno::EXIST | rustix::io::Errno::NOTSUP) => {}
        Err(err) => return Err(err.into()),
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(NEVER_INDEX))
    {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

/// hashes the object at `path`, never through a link, which holds `size` bytes when known.
fn hash_object(path: &Path, size: Option<u64>) -> io::Result<Sha256> {
    let file = read_unfollowed(path)?;
    let size = match size {
        Some(size) => size,
        None => file.metadata()?.len(),
    };
    Sha256::of_reader(file, Block::sized(size.saturating_add(1)))
}

/// what is at an object's path when a new copy of its bytes arrives.
enum Existing {
    /// these bytes, or a file that cannot be read, which is no proof that it changed.
    Keep,
    /// something else: other bytes, another size or not a file.
    Changed,
    /// nothing, since another process took it away.
    Gone,
}

/// judges what `found` at an object's path holds, hashing it with `hash` only when its size
/// matches.
fn existing(
    found: &io::Result<fs::Metadata>,
    size: u64,
    hash: impl FnOnce() -> io::Result<Sha256>,
    digest: &Sha256,
) -> Existing {
    match found {
        Ok(found) if is_plain(found) && found.len() == size => match hash() {
            Ok(actual) if actual == *digest => Existing::Keep,
            Ok(_) => Existing::Changed,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Existing::Gone,
            Err(_) => Existing::Keep,
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Existing::Gone,
        _ => Existing::Changed,
    }
}

/// writes `bytes` to the new file at `temporary` and gives it the name `path` with `rename`, each
/// flushed with `sync_file` and `sync_directory` when `durable`.
fn fill_and_rename_with(
    mut file: File,
    bytes: &[u8],
    [temporary, path]: [&Path; 2],
    durable: bool,
    sync_file: impl FnOnce(&File) -> io::Result<()>,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
    sync_directory: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    file.write_all(bytes)?;
    if durable {
        sync_file(&file)?;
    }
    drop(file);
    // a rename moves a name: a link at `path` is replaced, never followed
    rename(temporary, path)?;
    if durable {
        sync_publication(temporary, path, sync_directory)?;
    }
    Ok(())
}

/// flushes the published name's folder, and the source's when it differs, before the source goes.
fn sync_publication(
    source: &Path,
    path: &Path,
    mut sync_directory: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let destination = path.parent().unwrap_or_else(|| Path::new("."));
    let source = source.parent().unwrap_or_else(|| Path::new("."));
    for folder in std::iter::once(destination).chain((source != destination).then_some(source)) {
        sync_directory(folder).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "{} was published, but could not flush directory {}: {err}",
                    path.display(),
                    folder.display()
                ),
            )
        })?;
    }
    Ok(())
}

/// an io error in words that say what to fix, which keeps the error it explains as its source,
/// so the system's own code stays within reach of [`crate::error::io_cause`].
#[derive(Debug)]
struct Explained {
    words: String,
    cause: io::Error,
}

impl std::fmt::Display for Explained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words)
    }
}

impl std::error::Error for Explained {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn cache() -> (tempfile::TempDir, Cache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        (dir, cache)
    }

    #[test]
    fn an_imported_file_is_cloned_or_copied_and_never_linked() {
        let (dir, cache) = cache();
        let original = dir.path().join("flowers.csv");
        fs::write(&original, b"length\n1.4\n").unwrap();
        // the import clones on linux wherever this probe can
        let clones = reflink_copy::reflink(&original, dir.path().join("probe")).is_ok();
        let staging = cache.staging().unwrap();
        let stop = crate::stop::Stop::default();
        let source = File::open(&original).unwrap();
        let imported = cache.import_file(source, &staging, true, &stop).unwrap();
        assert_eq!(imported.cloned, clones && cfg!(target_os = "linux"));
        let stored = imported.sealed.publish(&cache).unwrap();
        assert_eq!(stored.digest, digest::sha256(b"length\n1.4\n"));
        assert_eq!(stored.size, 11);
        let object = cache.object_path(&stored.digest);
        assert_eq!(fs::read(&object).unwrap(), b"length\n1.4\n");
        assert!(
            !same_file::is_same_file(&object, &original).unwrap(),
            "the person's file is never the cache's object"
        );
        assert!(!fs::metadata(&original).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            let folder = File::open(dir.path()).unwrap();
            let refused = cache.import_file(folder, &staging, true, &stop);
            assert_eq!(
                refused.err().map(|err| err.kind()),
                Some(io::ErrorKind::InvalidInput)
            );
        }
    }

    /// a cache that acts as if it sat on FAT32 or exFAT: it cannot hardlink, and it refuses to
    /// remove a read-only file, as windows does there.
    pub(crate) fn fat() -> (tempfile::TempDir, Cache) {
        let (dir, mut cache) = cache();
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), false));
        (dir, cache)
    }

    /// whether the volume under `dir` can hardlink. FAT32 and exFAT cannot.
    pub(crate) fn hardlinks(dir: &Path) -> bool {
        let probe = dir.join("probe");
        let link = dir.join("probe link");
        fs::write(&probe, b"").unwrap();
        let linked = fs::hard_link(&probe, &link).is_ok();
        for path in [probe, link] {
            let _ = fs::remove_file(path);
        }
        linked
    }

    /// makes the file at `path` read-only, as every object is.
    pub(crate) fn seal(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }

    /// keeps `path` from being removed while the result lives.
    #[cfg(windows)]
    pub(crate) fn hold(path: &Path) -> impl Sized {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .unwrap()
    }

    #[cfg(unix)]
    pub(crate) fn hold(path: &Path) -> impl Sized {
        use std::os::unix::fs::PermissionsExt;
        struct Locked(PathBuf);
        impl Drop for Locked {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
            }
        }
        let folder = path.parent().unwrap().to_path_buf();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        Locked(folder)
    }

    pub(crate) fn store(cache: &Cache, bytes: &[u8]) -> Stored {
        let mut object = cache.new_object().unwrap();
        object.write_all(bytes).unwrap();
        object.seal(true).unwrap().publish(cache).unwrap()
    }

    /// a full disk under the cache says how much of it unfinished downloads hold and how to free
    /// them, once they hold anything, and a refused write names the folder that refused it.
    #[test]
    fn a_write_the_cache_refuses_says_what_to_fix() {
        let (_dir, cache) = cache();
        let partial = cache.root.join("partial");
        let full = || io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 });
        let error = crate::error::Error::io(
            "could not write to the cache",
            &cache.explained(&partial, full()),
        );
        assert_eq!(
            error.message,
            "could not write to the cache: the disk is full"
        );
        let key = "ab".repeat(32);
        let shard = cache.partial_folder(&key);
        fs::create_dir_all(&shard).unwrap();
        let part = shard.join(format!("{key}.part"));
        fs::write(&part, [0; 2048]).unwrap();
        fs::write(shard.join(format!("{key}.lock")), b"").unwrap();
        let error = crate::error::Error::io(
            "could not write to the cache",
            &cache.explained(&partial, full()),
        );
        assert_eq!(
            error.message,
            format!(
                "could not write to the cache: the disk is full; unfinished downloads in {} hold {} of it, which `fetchloom cache gc --now` frees",
                partial.display(),
                crate::units::size(upkeep::on_disk(&fs::metadata(&part).unwrap()))
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some("free some space on that disk, then run the command again")
        );
        // a download a run holds is that run's to finish or delete, so gc frees none of it
        let claim = fs::File::create(shard.join(format!("{key}.lock"))).unwrap();
        claim.lock().unwrap();
        let error = crate::error::Error::io(
            "could not write to the cache",
            &cache.explained(&partial, full()),
        );
        assert_eq!(
            error.message,
            "could not write to the cache: the disk is full"
        );
        drop(claim);
        let code = if cfg!(windows) { 5 } else { 13 };
        let denied = cache.explained(&partial, io::Error::from_raw_os_error(code));
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        // the system's own cause stays within reach
        assert_eq!(
            crate::error::io_cause(&denied).and_then(io::Error::raw_os_error),
            Some(code)
        );
        let error = crate::error::Error::io("could not write to the cache", &denied);
        assert_eq!(
            error.message,
            format!(
                "could not write to the cache: permission was denied in {}",
                partial.display()
            )
        );
        let built = io::Error::new(io::ErrorKind::StorageFull, "already explained");
        assert_eq!(
            cache.explained(&partial, built).to_string(),
            "already explained"
        );
    }

    #[test]
    fn durable_record_renames_sync_both_parents() {
        use std::cell::RefCell;

        let (_dir, cache) = cache();
        for durable in [false, true] {
            let temporary = cache.root.join("tmp/record");
            let destination = cache.root.join("projects/record");
            let events = RefCell::new(Vec::new());
            fill_and_rename_with(
                File::create(&temporary).unwrap(),
                b"registered",
                [&temporary, &destination],
                durable,
                |_| {
                    events.borrow_mut().push("file");
                    Ok(())
                },
                |from, to| {
                    events.borrow_mut().push("rename");
                    fs::rename(from, to)
                },
                |folder| {
                    events
                        .borrow_mut()
                        .push(if folder == destination.parent().unwrap() {
                            "destination"
                        } else {
                            assert_eq!(folder, temporary.parent().unwrap());
                            "source"
                        });
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(fs::read(&destination).unwrap(), b"registered");
            assert_eq!(
                events.into_inner(),
                if durable {
                    vec!["file", "rename", "destination", "source"]
                } else {
                    vec!["rename"]
                }
            );
        }
    }

    #[test]
    fn durable_record_flush_errors_stop_acknowledgement() {
        use std::cell::RefCell;

        let (_dir, cache) = cache();
        for (index, failing) in ["file", "rename", "destination", "source"]
            .into_iter()
            .enumerate()
        {
            let temporary = cache.root.join(format!("tmp/record-{index}"));
            let destination = cache.root.join(format!("projects/record-{index}"));
            let events = RefCell::new(Vec::new());
            let step = |name| {
                events.borrow_mut().push(name);
                if name == failing {
                    Err(io::Error::other("injected flush failure"))
                } else {
                    Ok(())
                }
            };
            let result = fill_and_rename_with(
                File::create(&temporary).unwrap(),
                b"registered",
                [&temporary, &destination],
                true,
                |_| step("file"),
                |from, to| {
                    step("rename")?;
                    fs::rename(from, to)
                },
                |folder| {
                    assert_eq!(fs::read(&destination).unwrap(), b"registered");
                    assert!(!temporary.exists());
                    step(if folder == destination.parent().unwrap() {
                        "destination"
                    } else {
                        "source"
                    })
                },
            );
            let error = result.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(
                events.into_inner(),
                ["file", "rename", "destination", "source"][..=index]
            );
            if index >= 2 {
                assert!(
                    error
                        .to_string()
                        .contains(&destination.display().to_string()),
                    "{error}"
                );
            }
        }
        let temporary = cache.root.join("tmp/record");
        let destination = cache.root.join("tmp/published");
        let mut folders = Vec::new();
        fill_and_rename_with(
            File::create(&temporary).unwrap(),
            b"registered",
            [&temporary, &destination],
            true,
            File::sync_all,
            |from, to| fs::rename(from, to),
            |folder| {
                folders.push(folder.to_path_buf());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(folders, [cache.root.join("tmp")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_record_directory_flush_is_retried_on_registration() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let (dir, cache) = cache();
        let folder = cache.root.join("projects");
        let permissions = fs::metadata(&folder).unwrap().permissions();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o333)).unwrap();
        let first = cache.register(dir.path());
        let retried = cache.register(dir.path());
        fs::set_permissions(&folder, permissions).unwrap();
        assert_eq!(first.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(retried.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        cache.register(dir.path()).unwrap();
        assert_eq!(
            cache
                .registered_projects(&crate::stop::Stop::default())
                .unwrap(),
            [dir.path()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_repair_is_not_marked_complete_before_directory_flush() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let (_dir, cache) = cache();
        let digest = store(&cache, b"good").digest;
        let path = cache.object_path(&digest);
        rewrite(&path, b"evil");
        seal(&path);
        let mut object = cache.new_object().unwrap();
        object.write_all(b"good").unwrap();
        let sealed = object.seal(true).unwrap();
        let folder = path.parent().unwrap();
        let permissions = fs::metadata(folder).unwrap().permissions();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o333)).unwrap();
        let result = sealed.publish(&cache);
        fs::set_permissions(folder, permissions).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(path).unwrap(), b"good");
        assert_eq!(cache.marks().unwrap()[&digest].replacement, None);
    }

    #[test]
    fn publication_preserves_new_existing_and_damaged_objects() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"good");
        assert!(stored.new);
        let path = cache.object_path(&stored.digest);
        let first = cache.root.join("first");
        let linked = fs::hard_link(&path, &first).is_ok();
        assert!(!store(&cache, b"good").new);
        if linked {
            assert!(same_file::is_same_file(&path, &first).unwrap());
        }
        rewrite(&path, b"evil");
        seal(&path);
        assert!(store(&cache, b"good").new);
        assert_eq!(fs::read(&path).unwrap(), b"good");
        assert!(cache.marks().unwrap()[&stored.digest].replacement.is_some());
        if linked {
            assert_eq!(fs::read(&first).unwrap(), b"evil");
            assert!(fs::metadata(&first).unwrap().permissions().readonly());
        }
        assert_eq!(fs::read_dir(cache.root.join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn racing_publications_keep_one_new_name() {
        let (dir, cache) = cache();
        if !hardlinks(dir.path()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let ready = || {
            let mut object = cache.new_object().unwrap();
            object.write_all(b"good").unwrap();
            object.seal(true).unwrap()
        };
        let (first, second) = (ready(), ready());
        let barrier = std::sync::Barrier::new(3);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                first.publish(&cache).unwrap()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                second.publish(&cache).unwrap()
            });
            barrier.wait();
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|stored| stored.new).count(), 1);
        assert_eq!(
            fs::read(cache.object_path(&results[0].digest)).unwrap(),
            b"good"
        );
        assert_eq!(fs::read_dir(cache.root.join("tmp")).unwrap().count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_marks_the_root_once_for_backup_and_indexing() {
        let (_dir, cache) = cache();
        let marker = cache.root.join(".metadata_never_index");
        assert_eq!(fs::read(&marker).unwrap(), b"");
        let mut value = [0; 64];
        match rustix::fs::getxattr(&cache.root, BACKUP_EXCLUSION, &mut value[..]) {
            Ok(size) => assert_eq!(&value[..size], b"com.apple.backupd"),
            Err(rustix::io::Errno::NOTSUP) => {}
            Err(err) => panic!("backup exclusion could not be read: {err}"),
        }
        fs::write(&marker, b"kept").unwrap();
        Cache::open(cache.root.clone()).unwrap();
        assert_eq!(fs::read(marker).unwrap(), b"kept");
    }

    #[test]
    fn cache_records_require_explicit_nullable_fields() {
        let validators = serde_json::to_value(Validators::default()).unwrap();
        for field in ["etag", "last_modified", "date"] {
            let mut missing = validators.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Validators>(missing).is_err(),
                "missing {field}"
            );
        }
        let record = UrlRecord {
            url: "https://example.org/object".to_owned(),
            sha256: digest::sha256(b"good"),
            size: 4,
            name: None,
            validators: Validators::default(),
            page: false,
        };
        let mut stored = serde_json::to_value(&record).unwrap();
        let mut read = serde_json::from_value::<UrlRecord>(stored.clone()).unwrap();
        assert_eq!(read.url, url_identity(&record.url));
        read.url.clone_from(&record.url);
        assert_eq!(read, record);
        stored.as_object_mut().unwrap().remove("name");
        assert!(serde_json::from_value::<UrlRecord>(stored).is_err());
    }

    #[test]
    fn a_new_cache_marks_itself_for_backup_tools() {
        let (_dir, cache) = cache();
        let tag = fs::read_to_string(cache.root.join("CACHEDIR.TAG")).unwrap();
        assert!(tag.starts_with("Signature: 8a477f597d28d172789f06886806bc55\n"));
    }

    /// runs opening a new cache at once each write its tag, and one run's write may be refused
    /// while another run has the tag open.
    #[test]
    fn a_cache_another_run_tagged_since_the_check_still_opens() {
        let (_dir, cache) = cache();
        let tag = cache.root.join("CACHEDIR.TAG");
        let denied = || io::Error::from_raw_os_error(if cfg!(windows) { 5 } else { 13 });
        super::tag(&cache.root, |_, _| {
            panic!("a tag that is there is not written again")
        })
        .unwrap();
        fs::remove_file(&tag).unwrap();
        super::tag(&cache.root, |path, bytes| {
            // the other run's tag lands first, and this run's write is refused
            fs::write(path, bytes)?;
            Err(denied())
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&tag).unwrap(), TAG);
        fs::remove_file(&tag).unwrap();
        let refused = super::tag(&cache.root, |_, _| Err(denied())).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!tag.exists());
    }

    /// a tag that has another name, as a hardlink a co-user of an empty shared cache put there
    /// to a file of this user's would, is never written, so the other file stays as it was.
    #[test]
    fn a_tag_with_another_name_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("mine.txt");
        fs::write(&mine, b"").unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        if fs::hard_link(&mine, root.join("CACHEDIR.TAG")).is_err() {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }

        let refused = Cache::open(root).err().unwrap();

        assert!(
            refused.to_string().contains("has another name"),
            "{refused}"
        );
        assert_eq!(fs::read(&mine).unwrap(), b"");
    }

    /// two runs that make one new cache at once: one finds no tag yet, and by the time it lists
    /// the folder the other has made the whole cache there, its tag first; it opens that cache.
    #[test]
    fn a_cache_another_run_made_since_the_tag_was_read_opens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        let other = root.clone();
        MEANWHILE.set(Some(Box::new(move || {
            Cache::open(other).unwrap();
        })));

        let opened = Cache::open(root);

        assert!(MEANWHILE.take().is_none(), "the other run made the cache");
        opened.unwrap();
    }

    #[test]
    fn unknown_files_in_the_cache_do_not_affect_upkeep() {
        let (_dir, cache) = cache();
        for name in ["notes.txt", "unknown"] {
            fs::write(cache.root.join(name), b"leave this alone").unwrap();
        }
        let cache = Cache::open(cache.root).unwrap();
        store(&cache, b"abc");
        // finder leaves .DS_Store in any folder a person opens, objects/ too
        fs::write(cache.root.join("objects/.DS_Store"), b"finder").unwrap();

        let checked = cache.verify_all(1, &|_| {});
        assert_eq!(checked.objects, 1);
        assert_eq!(checked.bytes, 3);
        assert_eq!(checked.changed, []);
        assert_eq!(checked.stuck, []);
        assert_eq!(checked.unreadable, []);
        assert_eq!(cache.projects(), Vec::<PathBuf>::new());

        let later = std::time::SystemTime::now() + std::time::Duration::from_hours(48);
        let freed = cache.sweep(&Needed::default(), later);
        assert_eq!(freed.removed_objects, 1);
        assert_eq!(freed.removed_bytes, 3);
        assert_eq!(freed.removed_leftovers, 0);
        for name in ["notes.txt", "unknown"] {
            assert_eq!(
                fs::read(cache.root.join(name)).unwrap(),
                b"leave this alone"
            );
        }
    }

    /// gc clears old files from tmp/ and the tag tells backup tools to skip the whole folder, so a
    /// folder of someone's own files, as a home folder named by mistake, never becomes the cache,
    /// even when its folders have a cache's names, as a scratch folder's may, or a cache lost its
    /// tag.
    #[test]
    fn a_folder_of_other_files_never_becomes_the_cache() {
        let folders: [&[&str]; 8] = [
            &["tmp/important.txt", "thesis.tex"],
            &["tmp/important.txt"],
            &["tmp/notes.txt", "objects/pack1"],
            &["projects/myapp/main.py"],
            &["objects/ab/cdef", "partial/0/", "gc.lock"],
            &["state/speeds.json", "state/notes.txt"],
            &["state/old/speeds.json"],
            &["state"],
        ];
        for files in folders {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            for file in files {
                let path = home.join(file);
                if file.ends_with('/') {
                    fs::create_dir_all(&path).unwrap();
                } else {
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(&path, b"keep these bytes").unwrap();
                }
            }
            let before = entries(&home);

            let error = Cache::open(home.clone()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{files:?}");
            assert!(Cache::unopened(home.clone()).is_err(), "{files:?}");
            assert_eq!(entries(&home), before, "{files:?}");
        }
    }

    /// every file and folder below `root`, by its path there.
    fn entries(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path.clone());
                }
                found.push(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
        found.sort();
        found
    }

    /// an empty folder, as a new disk with its lost+found, becomes the cache, and a cache opens
    /// again. so does a folder a first run left as it was stopped before its tag was whole, with
    /// the start of the tag and macos's mark against indexing, which then gets the whole tag.
    #[test]
    fn an_empty_folder_or_a_cache_opens_as_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let disk = dir.path().join("disk");
        fs::create_dir_all(disk.join("lost+found")).unwrap();
        Cache::open(disk.clone()).unwrap();
        fs::write(disk.join("notes.txt"), b"a person's note").unwrap();
        Cache::open(disk.clone()).unwrap();
        assert!(Cache::unopened(disk).is_ok());
        // empty, the signature line alone, and up to the word that makes it fetchloom's
        for cut in [0, 43, 62] {
            let stopped = dir.path().join(format!("stopped-{cut}"));
            fs::create_dir_all(&stopped).unwrap();
            fs::write(stopped.join("CACHEDIR.TAG"), &TAG.as_bytes()[..cut]).unwrap();
            fs::write(stopped.join(".metadata_never_index"), b"").unwrap();
            assert!(Cache::unopened(stopped.clone()).is_ok(), "{cut}");
            Cache::open(stopped.clone()).unwrap();
            assert_eq!(
                fs::read_to_string(stopped.join("CACHEDIR.TAG")).unwrap(),
                TAG
            );
        }
    }

    /// windows keeps fetchloom's state folder in the default cache folder, so a run with the cache
    /// elsewhere can write it there before any cache: that folder is still no cache yet, and
    /// becomes the cache with the state kept.
    #[test]
    fn fetchloom_state_alone_leaves_a_folder_free_for_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("fetchloom");
        let state = root.join("state");
        let speeds = crate::net::speeds::Speeds::at(Some(state.clone()));
        speeds
            .run
            .answered("zenodo.org", std::time::Duration::from_millis(5));
        speeds.save();
        // what a run stopped while it wrote the file leaves
        fs::write(state.join(".speeds.json.x7Yz2q.tmp"), b"{").unwrap();
        let before = entries(&root);
        Cache::unopened(root.clone()).unwrap();
        assert!(absent(&root));
        assert_eq!(entries(&root), before);
        Cache::open(root.clone()).unwrap();
        assert!(tagged(&root));
        assert!(state.join("speeds.json").is_file());
        #[cfg(unix)]
        {
            let linked = dir.path().join("linked");
            fs::create_dir(&linked).unwrap();
            std::os::unix::fs::symlink(&state, linked.join("state")).unwrap();
            assert!(Cache::open(linked).is_err(), "a link is no state folder");
        }
    }

    /// a new cache gets its tag before any of its folders, so a first run stopped while writing
    /// the tag leaves only the tag's start, which the next run still takes for its cache.
    #[test]
    fn a_new_cache_is_tagged_before_any_folder_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let stopped = Cache::open_tagging(root.clone(), |path, bytes| {
            fs::write(path, &bytes[..20])?;
            Err(io::ErrorKind::Interrupted.into())
        });
        assert!(stopped.is_err());
        // macos also marks a new cache for spotlight to skip, with a file, not a folder
        let made: Vec<PathBuf> = entries(&root)
            .into_iter()
            .filter(|name| name.as_os_str() != NEVER_INDEX)
            .collect();
        assert_eq!(made, [PathBuf::from("CACHEDIR.TAG")]);
        Cache::open(root.clone()).unwrap();
        assert_eq!(fs::read_to_string(root.join("CACHEDIR.TAG")).unwrap(), TAG);
    }

    /// a folder holding only what a cache holds but no tag looks like a cache whose tag is gone,
    /// as a cleaner leaves one, or as a build that made a cache's folders before its tag left one
    /// when stopped between them; a scratch folder with a cache's names and files of its own does
    /// not.
    #[test]
    fn a_cache_whose_tag_was_deleted_is_told_apart() {
        let (_dir, cache) = cache();
        store(&cache, b"abc");
        fs::remove_file(cache.root.join("CACHEDIR.TAG")).unwrap();
        // windows keeps fetchloom's state folder in the default cache folder
        fs::create_dir(cache.root.join("state")).unwrap();
        fs::write(cache.root.join("state").join("speeds.json"), b"{}").unwrap();
        let stopped = tempfile::tempdir().unwrap();
        for folder in FOLDERS {
            fs::create_dir(stopped.path().join(folder)).unwrap();
        }
        let gone = "it looks like a fetchloom cache whose tag is gone: it holds only what a cache \
                    holds, but no CACHEDIR.TAG, the file fetchloom knows its cache by";
        for root in [cache.root.clone(), stopped.path().to_path_buf()] {
            let error = Cache::open(root.clone()).unwrap_err();
            assert_eq!(error.to_string(), gone, "{}", root.display());
            let error = Cache::unopened(root.clone()).unwrap_err();
            assert_eq!(error.to_string(), gone, "{}", root.display());
            assert!(!root.join("CACHEDIR.TAG").exists());
        }
        let scratch = tempfile::tempdir().unwrap();
        for file in ["tmp/notes.txt", "objects/pack1"] {
            let path = scratch.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"mine").unwrap();
        }
        let error = Cache::open(scratch.path().to_path_buf()).unwrap_err();
        assert!(
            error.to_string().starts_with("it holds other files"),
            "{error}"
        );
    }

    /// a team's cache may be another member's folder or one their group writes; only the owner
    /// being someone else or every user writing is told.
    #[cfg(unix)]
    #[test]
    fn a_cache_folder_another_user_owns_or_anyone_writes_is_exposed() {
        assert_eq!(super::exposure(1000, 0o40700, 1000, false), None);
        assert_eq!(super::exposure(1000, 0o42770, 1000, false), None);
        assert_eq!(
            super::exposure(1001, 0o40700, 1000, false),
            Some("belongs to another user")
        );
        assert_eq!(super::exposure(1001, 0o42770, 1000, true), None);
        assert_eq!(
            super::exposure(1000, 0o40777, 1000, false),
            Some("lets every user of this machine write to it")
        );
    }

    /// a folder fetchloom writes in that every user may write, in a cache folder every user may
    /// enter, lets them change what projects place as much as the cache folder itself would; in a
    /// cache folder only its owner may enter, it does not.
    #[cfg(unix)]
    #[test]
    fn a_folder_in_the_cache_that_anyone_writes_is_exposed() {
        use std::os::unix::fs::PermissionsExt;
        for inner in ["objects", "tmp"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("cache");
            Cache::open(root.clone()).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(root.join(inner), fs::Permissions::from_mode(0o777)).unwrap();

            let told = exposed(&root).unwrap_or_default();

            let folder = root.join(inner).display().to_string();
            assert!(
                told.contains(&folder) && told.contains("lets every user of this machine write"),
                "{inner}: {told}"
            );
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                exposed(&root),
                None,
                "{inner} in a folder only its owner enters"
            );
        }
    }

    /// a lab's cache, a setgid folder its group may write and no one else, as the clusters guide
    /// makes it, is shared on purpose: its members hear nothing of whose it is, while one outside
    /// that group, or a cache every user may write, still warns.
    #[cfg(unix)]
    #[test]
    fn a_cache_a_group_shares_on_purpose_is_not_another_users() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, cache) = cache();
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o2770)).unwrap();
        let found = fs::metadata(&cache.root).unwrap();
        // as another member sees the folder its owner made
        let member = found.uid() + 1;
        let group = found.gid();

        assert_eq!(exposed_as(&cache.root, member, &|gid| gid == group), None);
        let outsider = exposed_as(&cache.root, member, &|_| false).unwrap_or_default();
        assert!(outsider.contains("belongs to another user"), "{outsider}");
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o2777)).unwrap();
        assert!(
            exposed_as(&cache.root, member, &|gid| gid == group).is_some(),
            "every user may write it"
        );
    }

    /// in a cache a group shares, a run whose umask takes the group's write from what it makes is
    /// told so before it adds anything; a cache that is no group's is none of its business.
    #[cfg(unix)]
    #[test]
    fn a_umask_that_takes_the_groups_write_is_told_in_a_shared_cache() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        for folder in FOLDERS {
            fs::set_permissions(cache.root.join(folder), fs::Permissions::from_mode(0o2770))
                .unwrap();
        }
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o2770)).unwrap();
        assert_eq!(unshared(&cache.root, Some(0o002)), None);
        assert_eq!(unshared(&cache.root, None), None, "a umask not known");
        let told = unshared(&cache.root, Some(0o022)).unwrap_or_default();
        assert!(told.contains("umask, 022,"), "{told}");
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(unshared(&cache.root, Some(0o022)), None, "no group's cache");
    }

    /// a fan folder of `objects/` every user may write, as a run with `umask 000` makes them,
    /// lets them put any bytes under an object's name, as `objects/` itself would.
    #[cfg(unix)]
    #[test]
    fn a_fan_folder_that_anyone_writes_is_exposed() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        let stored = store(&cache, b"abc");
        let fan = cache
            .object_path(&stored.digest)
            .parent()
            .unwrap()
            .to_path_buf();
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(
            cache.root.join("objects"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(exposed(&cache.root), None, "a fan only its owner writes");
        fs::set_permissions(&fan, fs::Permissions::from_mode(0o777)).unwrap();

        let told = exposed(&cache.root).unwrap_or_default();

        assert!(
            told.contains(&fan.display().to_string())
                && told.contains("lets every user of this machine write"),
            "{told}"
        );
    }

    /// in a cache a group shares, a setgid folder its group may write, every part of the cache
    /// gives that group what each folder and file it makes gives its owner, whatever the umask,
    /// and never anything to others; an object stays read-only. it runs under `umask 022` in a
    /// process of its own, so the umask of the process running the tests stays as it was.
    #[cfg(unix)]
    #[test]
    fn each_part_of_a_groups_cache_shares_what_it_makes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        const CHILD: &str = "FETCHLOOM_TEST_UMASK_022";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cache::tests::each_part_of_a_groups_cache_shares_what_it_makes",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&result.stdout);
            // a name that matched no test would pass without running it
            assert!(
                result.status.success() && stdout.contains(" 1 passed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o2770)).unwrap();
        if fs::metadata(&root).unwrap().mode() & 0o7777 != 0o2770 {
            return crate::test_skips::skip("permissions", "this disk keeps no setgid folder");
        }
        let cache = Cache::open(root.clone())
            .unwrap()
            .hold(Access::Shared, &crate::stop::Stop::default(), &|| {})
            .unwrap();
        let stored = store(&cache, b"abc");
        let staging = cache.staging().unwrap();
        let member = staging.new_object().unwrap();
        let Claim::Owned(partial) = cache.claim("https://example.org/a").unwrap() else {
            panic!("nothing else holds the download");
        };
        let mut writer = partial
            .writer(0, Some(6), Validators::default(), true, false)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        cache.mark_keyed(&stored.digest).unwrap();
        let md5 = Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap();
        let stated = Stated {
            digest: stored.digest,
            size: 3,
            version: None,
        };
        cache
            .save_stated_md5(&md5, "https://example.org/abc", &stated)
            .unwrap();
        cache.mark_changed(&stored.digest).unwrap();
        cache
            .save_url_record(&UrlRecord {
                url: "https://example.org/abc".to_owned(),
                sha256: stored.digest,
                size: 3,
                name: None,
                validators: Validators::default(),
                page: false,
            })
            .unwrap();
        cache.register(dir.path()).unwrap();
        let members = [Member {
            path: "a.csv".to_owned(),
            size: 3,
            sha256: stored.digest,
        }];
        cache.record_unpacked(&stored.digest, &members).unwrap();
        cache.mark_imported(&[stored.digest]).unwrap();

        let wrong = unshared_below(&root);
        drop((writer, member, staging));
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// what below the cache folder `root` its group, which a root of mode 2770 gives every bit,
    /// cannot use as the owner can, and what every user may write, sorted.
    #[cfg(unix)]
    fn unshared_below(root: &Path) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        let group = fs::metadata(root).unwrap().gid();
        let mut wrong = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                let found = fs::symlink_metadata(&path).unwrap();
                let mode = found.mode() & 0o7777;
                let shown = format!("{mode:o} {}", path.display());
                if mode & 0o002 != 0 {
                    wrong.push(format!("every user may write {shown}"));
                }
                if found.is_dir() {
                    if mode & 0o070 != 0o070 || found.gid() != group {
                        wrong.push(format!("the group cannot add to {shown}"));
                    }
                    folders.push(path);
                    continue;
                }
                if mode & 0o040 == 0 {
                    wrong.push(format!("the group cannot read {shown}"));
                }
                if mode & 0o200 != 0 && mode & 0o020 == 0 {
                    wrong.push(format!("only its owner may write {shown}"));
                }
                if path.starts_with(root.join("objects")) && mode & 0o222 != 0 {
                    wrong.push(format!("an object may be written {shown}"));
                }
            }
        }
        wrong.sort();
        wrong
    }

    #[test]
    fn cache_folders_cannot_be_files() {
        for name in FOLDERS {
            let dir = tempfile::tempdir().unwrap();
            Cache::open(dir.path().to_path_buf()).unwrap();
            fs::remove_dir(dir.path().join(name)).unwrap();
            fs::write(dir.path().join(name), b"keep these bytes").unwrap();

            let error = Cache::open(dir.path().to_path_buf()).unwrap_err();

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                fs::read(dir.path().join(name)).unwrap(),
                b"keep these bytes"
            );
        }
    }

    #[test]
    fn an_object_lands_read_only_under_its_hash() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"abc");
        assert!(stored.new);
        assert_eq!(
            stored.digest.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(fs::read(cache.object_path(&stored.digest)).unwrap(), b"abc");
        assert!(
            fs::metadata(cache.object_path(&stored.digest))
                .unwrap()
                .permissions()
                .readonly()
        );
        assert_eq!(
            cache.find(&stored.digest, Some(3)),
            Some(cache.object_path(&stored.digest))
        );
        assert_eq!(
            cache.find(&stored.digest, None),
            Some(cache.object_path(&stored.digest))
        );
        assert_eq!(
            cache.find(&stored.digest, Some(4)),
            None,
            "a size mismatch is not a hit"
        );
    }

    #[test]
    fn the_same_bytes_twice_are_stored_once() {
        for (_dir, cache) in [cache(), fat()] {
            let first = store(&cache, b"same");
            let second = store(&cache, b"same");
            assert!(first.new);
            assert!(!second.new);
            assert_eq!(first.digest, second.digest);
            assert!(
                fs::metadata(cache.object_path(&first.digest))
                    .unwrap()
                    .permissions()
                    .readonly()
            );
            let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
            assert_eq!(leftovers, 0, "nothing is left behind in tmp");
        }
    }

    #[test]
    fn bytes_another_writer_published_since_the_check_are_not_new() {
        let (_dir, mut cache) = cache();
        // FAT refuses the link, and another writer publishes the same bytes before the rename
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), false).losing_renames());
        assert_eq!(cache.find(&digest::sha256(b"same"), None), None);
        let stored = store(&cache, b"same");
        assert!(!stored.new);
        let path = cache.object_path(&stored.digest);
        assert_eq!(fs::read(&path).unwrap(), b"same");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            crate::volume::other_writers_time(),
            "the other writer's file keeps its name"
        );
        let leftovers = fs::read_dir(cache.root.join("tmp")).unwrap().count();
        assert_eq!(leftovers, 0, "nothing is left behind in tmp");
    }

    /// an object's name is its verified sha-256, so a rename that replaces swaps identical bytes.
    #[test]
    fn a_disk_that_renames_only_by_replacing_still_takes_objects() {
        let (_dir, mut cache) = cache();
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), false).replacing_only());
        let mut object = cache.new_object().unwrap();
        object.write_all(b"bytes").unwrap();

        let stored = object.seal(true).unwrap().publish(&cache).unwrap();

        assert!(stored.new);
        let found = cache.find(&digest::sha256(b"bytes"), None).unwrap();
        assert_eq!(fs::read(found).unwrap(), b"bytes");
        assert_eq!(fs::read_dir(cache.root.join("tmp")).unwrap().count(), 0);
    }

    #[test]
    fn a_dropped_object_leaves_the_cache() {
        for (_dir, cache) in [cache(), fat()] {
            let archive = store(&cache, b"an archive whose files are unpacked");
            cache.drop_object(&archive.digest).unwrap();
            assert_eq!(cache.find(&archive.digest, None), None);
        }
    }

    /// writes `bytes` over the object at `path`, as an edit through a hardlink someone made
    /// writable does.
    pub(crate) fn rewrite(path: &Path, bytes: &[u8]) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "the test damages an object on purpose"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// a changed size shows without hashing, while changed bytes of the same size need the hash.
    #[test]
    fn bytes_stored_again_replace_an_object_whose_bytes_changed() {
        for (rewritten, how) in [
            (&b"abcX"[..], "its size changed"),
            (b"abX", "its bytes changed in place"),
        ] {
            for (_dir, cache) in [cache(), fat()] {
                let first = store(&cache, b"abc");
                let object = cache.object_path(&first.digest);
                rewrite(&object, rewritten);
                let again = store(&cache, b"abc");
                assert_eq!(
                    String::from_utf8(fs::read(&object).unwrap()).unwrap(),
                    "abc",
                    "{how}"
                );
                assert!(again.new, "{how}");
                assert!(
                    fs::metadata(&object).unwrap().permissions().readonly(),
                    "{how}"
                );
                assert_eq!(
                    cache.changed().into_keys().collect::<Vec<_>>(),
                    [first.digest],
                    "{how}: projects linking to the changed bytes can learn it"
                );
            }
        }
    }

    #[test]
    fn a_doubted_object_keeps_its_name_until_repaired() {
        for (_dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            rewrite(&object, b"evil");
            seal(&object);
            cache.mark_changed(&stored.digest).unwrap();
            let other = Cache::open(cache.root().to_path_buf()).unwrap();
            for run in [&cache, &other] {
                assert!(
                    run.trusted(&stored.digest, Some(4), &run.doubts().unwrap())
                        .unwrap()
                        .is_none()
                );
                assert_eq!(fs::read(&object).unwrap(), b"evil");
                assert!(run.find(&stored.digest, Some(4)).is_none());
            }
            store(&cache, b"good");
            // the other repair has not published yet, but placement can use the first copy
            let placed = cache.root().join("placed");
            fs::copy(&object, &placed).unwrap();
            assert_eq!(fs::read(&placed).unwrap(), b"good");
            assert!(!store(&other, b"good").new);
            assert_eq!(fs::read(&object).unwrap(), b"good");
        }
    }

    #[test]
    fn repairing_an_object_again_refreshes_its_change_mark() {
        use std::time::{Duration, UNIX_EPOCH};

        for (dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            let repair = |written| {
                rewrite(&object, b"evil");
                seal(&object);
                let source = dir.path().join("repair");
                fs::write(&source, b"good").unwrap();
                File::options()
                    .write(true)
                    .open(&source)
                    .unwrap()
                    .set_modified(written)
                    .unwrap();
                seal(&source);
                assert!(
                    cache
                        .link_object(&source, stored.digest, 4, None)
                        .unwrap()
                        .new
                );
                assert_eq!(fs::read(&object).unwrap(), b"good");
                let marked = UNIX_EPOCH
                    + Duration::from_nanos(cache.changed_at(&stored.digest).unwrap().unwrap());
                assert!(marked < fs::metadata(&object).unwrap().modified().unwrap());
                marked
            };
            // fixed write times keep the two repairs distinct even on coarse clocks
            let first = repair(UNIX_EPOCH + Duration::from_secs(1_700_000_000));
            let second = repair(UNIX_EPOCH + Duration::from_secs(1_700_000_010));
            assert!(second > first, "another corruption needs a fresh mark");
        }
    }

    /// a project that last checked its hardlink just before the corruption was found must still
    /// learn of it after a repair, while the repaired object itself stays out of doubt.
    #[test]
    fn a_repair_still_tells_projects_checked_before_it() {
        use std::time::{SystemTime, UNIX_EPOCH};

        for (dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            let checked = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let checked = u64::try_from(checked).unwrap();
            rewrite(&object, b"evil");
            seal(&object);
            let source = dir.path().join("repair");
            fs::write(&source, b"good").unwrap();
            seal(&source);
            assert!(
                cache
                    .link_object(&source, stored.digest, 4, None)
                    .unwrap()
                    .new
            );
            let found = cache.changed()[&stored.digest];
            assert!(
                may_follow(found, checked),
                "the mark must date from the repair, not before it"
            );
            let metadata = fs::metadata(&object).unwrap();
            assert!(!changed::doubted(
                &object,
                &metadata,
                cache.changed_at(&stored.digest).unwrap()
            ));
        }
    }

    #[test]
    fn verification_keeps_a_copy_published_by_another_repair() {
        for (_dir, cache) in [cache(), fat()] {
            let stored = store(&cache, b"good");
            let object = cache.object_path(&stored.digest);
            rewrite(&object, b"evil");
            seal(&object);
            let checked = cache.verify_all(1, &|_| {
                // verification has read the damaged bytes, then another repair publishes
                assert!(store(&cache, b"good").new);
            });
            assert_eq!(checked.changed, [stored.digest]);
            assert_eq!(fs::read(&object).unwrap(), b"good");
            assert!(cache.find(&stored.digest, Some(4)).is_some());
            assert!(fs::metadata(&object).unwrap().permissions().readonly());
        }
    }

    #[test]
    fn repairing_a_read_only_object_preserves_placed_links() {
        let (dir, cache) = cache();
        if !hardlinks(dir.path()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        let stored = store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let placed = dir.path().join("placed");
        fs::hard_link(&object, &placed).unwrap();
        rewrite(&object, b"evil");
        seal(&object);
        assert!(store(&cache, b"good").new);
        assert_eq!(fs::read(&object).unwrap(), b"good");
        assert_eq!(fs::read(&placed).unwrap(), b"evil");
        for path in [&object, &placed] {
            assert!(fs::metadata(path).unwrap().permissions().readonly());
        }
    }

    #[test]
    fn an_object_found_in_place_is_kept_only_while_it_holds_these_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        fs::write(&path, b"abc").unwrap();
        let found = fs::metadata(&path);
        let digest = crate::digest::sha256(b"abc");
        let hashed = |result: io::Result<Sha256>| move || result;
        let failed = |kind: io::ErrorKind| hashed(Err(io::Error::from(kind)));
        assert!(matches!(
            existing(&found, 3, hashed(Ok(digest)), &digest),
            Existing::Keep
        ));
        assert!(matches!(
            existing(
                &found,
                3,
                hashed(Ok(crate::digest::sha256(b"abX"))),
                &digest
            ),
            Existing::Changed
        ));
        assert!(matches!(
            existing(&found, 4, hashed(Ok(digest)), &digest),
            Existing::Changed
        ));
        assert!(
            matches!(
                existing(&found, 3, failed(io::ErrorKind::PermissionDenied), &digest),
                Existing::Keep
            ),
            "bytes that cannot be read are no proof of change"
        );
        assert!(
            matches!(
                existing(&found, 3, failed(io::ErrorKind::NotFound), &digest),
                Existing::Gone
            ),
            "another process took it away while it was hashed"
        );
        let missing = fs::metadata(dir.path().join("missing"));
        assert!(matches!(
            existing(&missing, 3, hashed(Ok(digest)), &digest),
            Existing::Gone
        ));
        // something is there that cannot be read, so it is judged rather than replaced
        for refused in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::NotADirectory,
        ] {
            assert!(
                matches!(
                    existing(&Err(refused.into()), 3, hashed(Ok(digest)), &digest),
                    Existing::Changed
                ),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn bytes_are_stored_only_once_they_hold_their_name() {
        let (_dir, cache) = cache();
        let object = cache.object_path(&store(&cache, b"abc").digest);
        rewrite(&object, b"abcX");
        seal(&object);
        let held = hold(&object);
        let mut again = cache.new_object().unwrap();
        again.write_all(b"abc").unwrap();
        let published = again.seal(true).unwrap().publish(&cache);
        drop(held);
        // root on unix can replace the changed object despite its folder's permissions
        let holds = fs::read(&object).unwrap() == b"abc";
        assert_eq!(published.is_ok(), holds, "{published:?}");
        assert!(fs::metadata(&object).unwrap().permissions().readonly());
    }

    /// an object another process took away verifies as gone, not as an error that stops the run.
    #[test]
    fn verifying_a_missing_object_reports_it_gone() {
        let (_dir, cache) = cache();
        let digest = crate::digest::sha256(b"never stored");
        assert!(matches!(cache.verify(&digest), Ok(false)));
    }

    #[test]
    fn a_failed_repair_leaves_the_changed_object_in_doubt() {
        let (_dir, cache) = cache();
        let digest = store(&cache, b"good").digest;
        let object = cache.object_path(&digest);
        rewrite(&object, b"evil");
        seal(&object);
        assert!(!cache.verify(&digest).unwrap());
        let held = hold(&object);
        let mut again = cache.new_object().unwrap();
        again.write_all(b"good").unwrap();
        let published = again.seal(true).unwrap().publish(&cache);
        drop(held);
        // root on unix can replace the changed object despite its folder's permissions
        if fs::read(&object).unwrap() == b"good" {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        assert!(published.is_err());
        assert_eq!(cache.find(&digest, Some(4)), None);
        assert!(matches!(
            cache.trusted(&digest, Some(4), &cache.doubts().unwrap()),
            Ok(None)
        ));
    }

    #[test]
    fn url_records_round_trip() {
        let (_dir, cache) = cache();
        let record = UrlRecord {
            url: "https://example.org/iris.csv".to_owned(),
            sha256: store(&cache, b"x").digest,
            size: 1,
            name: Some("iris.csv".to_owned()),
            validators: Validators {
                etag: Some("\"x\"".to_owned()),
                last_modified: Some("Tue, 01 Oct 2024 10:00:00 GMT".to_owned()),
                date: Some("Tue, 01 Oct 2024 10:01:00 GMT".to_owned()),
                version: Some("3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY".to_owned()),
            },
            page: true,
        };
        cache.save_url_record(&record).unwrap();
        assert_eq!(cache.url_record(&record.url), Some(record));
        assert_eq!(cache.url_record("https://example.org/other"), None);
    }

    #[test]
    fn a_stated_md5_finds_its_object_for_its_link_alone() {
        let (_dir, cache) = cache();
        let stored = store(&cache, b"abc");
        let md5 = Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap();
        let url = "https://example.org/abc";
        assert_eq!(cache.stated_md5(&md5, url), None);
        for version in [None, Some("3HL4kqtJlcpXroDTDmJ+rmSpXd3dIbrHY")] {
            let stated = Stated {
                digest: stored.digest,
                size: 3,
                version: version.map(str::to_owned),
            };
            cache.save_stated_md5(&md5, url, &stated).unwrap();
            assert_eq!(cache.stated(&md5, url), Some(stated));
        }
        assert_eq!(cache.stated_md5(&md5, "https://example.org/other"), None);
    }

    /// a stated record someone put in a shared cache as a link, as to a pipe that would hold every
    /// run reading it, is no record: it is never followed.
    #[cfg(unix)]
    #[test]
    fn a_stated_md5_record_that_is_a_link_is_no_record() {
        let (dir, cache) = cache();
        let stored = store(&cache, b"abc");
        let md5 = Md5::from_hex("900150983cd24fb0d6963f7d28e17f72").unwrap();
        let url = "https://example.org/abc";
        let theirs = dir.path().join("theirs");
        fs::write(&theirs, format!("{} 3\n", stored.digest)).unwrap();
        let path = cache.stated_path(&md5, url);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&theirs, &path).unwrap();

        assert_eq!(cache.stated(&md5, url), None);
    }

    #[test]
    fn locate_prefers_the_variable_then_the_settings_file_then_the_platform_folder() {
        let var = |name: &str| (name == "FETCHLOOM_CACHE").then(|| OsString::from("/x/cache"));
        let filed = Path::new("/filed/cache");
        assert_eq!(Cache::locate(&var, None), Some(PathBuf::from("/x/cache")));
        assert_eq!(
            Cache::locate(&var, Some(filed)),
            Some(PathBuf::from("/x/cache"))
        );
        let none = |_: &str| None;
        assert_eq!(Cache::locate(&none, None), None);
        assert_eq!(Cache::locate(&none, Some(filed)), Some(filed.to_path_buf()));
        let home = |name: &str| {
            [
                ("HOME", "/home/ada"),
                ("XDG_CACHE_HOME", "/xdg"),
                ("LOCALAPPDATA", "/local"),
            ]
            .into_iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
        };
        let expected = if cfg!(windows) {
            Path::new("/local").join("fetchloom")
        } else if cfg!(target_os = "macos") {
            Path::new("/home/ada/Library/Caches/fetchloom").to_path_buf()
        } else {
            Path::new("/xdg/fetchloom").to_path_buf()
        };
        assert_eq!(Cache::locate(&home, None), Some(expected));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_relative_xdg_cache_home_is_ignored() {
        let var = |name: &str| match name {
            "HOME" => Some(OsString::from("/home/ada")),
            "XDG_CACHE_HOME" => Some(OsString::from("relative/cache")),
            _ => None,
        };
        assert_eq!(
            Cache::locate(&var, None),
            Some(Path::new("/home/ada/.cache/fetchloom").to_path_buf())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_new_cache_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("made").join("cache");
        Cache::open(root.clone()).unwrap();
        let mode = fs::metadata(&root).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

#[cfg(test)]
mod privacy;
