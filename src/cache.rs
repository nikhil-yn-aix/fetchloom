//! the content-addressed cache: one read-only file per object, named by its SHA-256.

mod blobs;
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
pub(crate) use object::start_writeback;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use object::{Batch, batch_in};
pub(crate) use object::{Copied, NewObject, Sealed, holds_pinned, sha256_of};
#[cfg(test)]
pub(crate) use partial::partial_key;
pub(crate) use partial::{Claim, Hashing, Partial, PartialWriter, SAVE_EVERY, names};
pub(crate) use sharing::Sharing;
pub(crate) use staging::Staging;
pub(crate) use unpacked::{
    Member, decompressed_key, members_of, parts_key, tree, tree_hash, tree_key,
};
#[cfg(test)]
pub(crate) use upkeep::Object;
pub(crate) use upkeep::{Checked, Freed, GRACE, Needed, Scan, Unfinished};

use crate::digest::{self, Block, Md5, Sha256};
use crate::place::{FolderUse, found_folder, real_folder};
use crate::volume::{
    Volume, is_plain, name_count, open_unfollowed, read_unfollowed, refuses_links,
};

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

/// what [`adoptable`] found of fetchloom's tag in the folder it takes as the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Whole,
    /// none yet, in a new folder or one that holds nothing but what [`left_before_tag`] passes.
    None,
    /// none, in a cache whose tag is gone, as [`lost_its_tag`] says.
    Lost,
}

/// refuses `root` as the cache unless it holds fetchloom's tag, nothing but what a system keeps
/// there, fetchloom's own state and what a first run stopped before its tag was whole leaves: the
/// start of the tag and macos's mark against indexing, or only what a cache whose tag is gone
/// holds, as [`lost_its_tag`] says. gc clears old files from tmp/, and the tag tells backup tools
/// to skip the whole folder, which a folder of a person's own files must not get, as a home named
/// by mistake, or a scratch folder whose folders have a cache's names.
fn adoptable(root: &Path) -> io::Result<Tag> {
    if is_cache(root) {
        return Ok(Tag::Whole);
    }
    meanwhile();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Tag::None),
        Err(err) => return Err(err),
    };
    for entry in entries {
        if left_before_tag(root, &entry?.file_name()) {
            continue;
        }
        // a run making the cache at the same time writes its whole tag before any folder of it,
        // so a folder that has the tag by now is that cache
        if is_cache(root) {
            return Ok(Tag::Whole);
        }
        if lost_its_tag(root) {
            return Ok(Tag::Lost);
        }
        return Err(io::Error::new(io::ErrorKind::InvalidInput, Foreign));
    }
    Ok(Tag::None)
}

/// the refusal of a folder of other files as the cache, which an empty or a new folder avoids.
#[derive(Debug)]
struct Foreign;

impl std::fmt::Display for Foreign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "it holds other files and is not a fetchloom cache, and fetchloom makes its cache only \
             in an empty folder or a new one",
        )
    }
}

impl std::error::Error for Foreign {}

/// whether `err` refuses a folder of other files as the cache.
pub(crate) fn is_foreign(err: &io::Error) -> bool {
    matches!(err.get_ref(), Some(inner) if inner.is::<Foreign>())
}

/// whether `root`, a folder without the tag, holds only what a cache holds, so it is a cache whose
/// tag is gone: a cleaner that deletes old files takes the tag first and leaves the folders, and a
/// build that made a cache's folders before its tag, stopped between them, leaves one too. every
/// name at its top is one fetchloom keeps there, objects/ is there, and objects/, tmp/ and
/// projects/ hold only names fetchloom gives, so a folder with any other name in them, as a
/// person's notes, is no such cache: the tag makes backup tools skip the folder.
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

/// why a run would refuse `root` as the cache, read as it is with nothing created: none for a
/// cache, an empty folder or one not there yet.
pub(crate) fn refused(root: &Path) -> Option<String> {
    crate::error::file_in_the_way(root).or_else(|| {
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
    if is_cache(root) {
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
        exposed_as(root, run_as(), &sharing::in_group)
    }
    #[cfg(not(unix))]
    exposed_on(root)
}

/// [`exposed`] as the user `user` sees it, who `member` says is in a group or not.
#[cfg(unix)]
fn exposed_as(root: &Path, user: u32, member: &dyn Fn(u32) -> bool) -> Option<String> {
    {
        use std::os::unix::fs::MetadataExt;
        let shared = |found: &fs::Metadata| {
            sharing::shared_with(found.mode(), found.gid(), member).is_some()
        };
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

/// whether users other than this one may write the records of the cache at `root`, its url,
/// MD5 and unpacked records, so one there may be theirs: its folder is another user's, or its
/// group or every user may write it, as a group shares a lab's, setgid and writable by the group,
/// as the clusters guide makes it; or they may enter it, and a folder of its records is another
/// user's or one they may write.
#[cfg(unix)]
fn others_write(root: &Path) -> bool {
    others_write_as(root, run_as())
}

/// [`others_write`] as the user `user` sees it.
#[cfg(unix)]
fn others_write_as(root: &Path, user: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    // a cache only its owner may enter, as fetchloom makes one, keeps to itself the folders in it
    // a umask of 002 left writable by the group
    let entered = fs::metadata(root).is_ok_and(|found| found.mode() & 0o011 != 0);
    others_may_write(root, user)
        || entered
            && ["urls", "stated", "stated/md5", "unpacked"]
                .iter()
                .any(|records| others_may_write(&root.join(records), user))
}

/// whether someone other than `user` may write `folder`: it is another user's, or its group or
/// every user may write it.
#[cfg(unix)]
fn others_may_write(folder: &Path, user: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(folder).is_ok_and(|found| found.uid() != user || found.mode() & 0o022 != 0)
}

#[cfg(not(unix))]
fn others_write(_root: &Path) -> bool {
    false
}

/// the shards a run keeps its user's own url records in, in a cache others may write; none in one
/// only this user writes, whose every record is theirs.
#[cfg(unix)]
fn own_records(others_write: bool) -> Option<Arc<urls::Shards>> {
    others_write.then(|| Arc::new(urls::Shards::own(run_as())))
}

#[cfg(not(unix))]
fn own_records(_others_write: bool) -> Option<Arc<urls::Shards>> {
    None
}

/// what keeps the cache at `root`, which its group shares, from staying one the group may add to:
/// the folders in it that let only their owners write to them, as one another program or an
/// account outside the group made may, since a run gives the group its write on each folder it
/// makes there; a few of them named, and how many more. none for a cache that is no group's, whose
/// folder is not setgid and writable by its group, as the clusters guide makes a lab's.
#[cfg(unix)]
pub(crate) fn unshared(root: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if !sharing::group_shares(fs::metadata(root).ok()?.mode()) {
        return None;
    }
    let mut fans: Vec<PathBuf> = ["objects", "keyed", "partial"]
        .iter()
        .flat_map(|top| fs::read_dir(root.join(top)).into_iter().flatten().flatten())
        .map(|fan| fan.path())
        .collect();
    fans.sort();
    let closed: Vec<String> = FOLDERS
        .iter()
        .map(|name| root.join(name))
        .chain(fans)
        .filter(|folder| {
            fs::symlink_metadata(folder)
                .is_ok_and(|found| found.is_dir() && found.mode() & 0o020 == 0)
        })
        .map(|folder| folder.display().to_string())
        .collect();
    match closed.as_slice() {
        [] => None,
        [folder] => Some(format!(
            "the folder {folder} in the cache lets only its owner write to it, though a group shares the cache, so the group's other members cannot add to it"
        )),
        many => Some(format!(
            "the folders {} in the cache let only their owners write to them, though a group shares the cache, so the group's other members cannot add to them",
            crate::units::some_of(many, 3)
        )),
    }
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

/// who owns `path`, or what a link there leads to, by user id.
#[cfg(unix)]
pub(crate) fn owner_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|found| found.uid())
}

/// who owns the entry at `path`, itself and never what a link there leads to, by user id.
#[cfg(unix)]
pub(crate) fn entry_owner(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).ok().map(|found| found.uid())
}

/// the user `id` in words: its name, else root by name and any other by the number `id -u` gives,
/// as a container may know no name for a user of the machine it runs on.
#[cfg(unix)]
pub(crate) fn said_user(id: u32) -> String {
    user_name(id).unwrap_or_else(|| match id {
        0 => "root".to_owned(),
        _ => format!("user {id}"),
    })
}

/// the name of the user `id`, as `id -nu` tells it: a cluster's accounts are mostly kept in a
/// directory service, which `/etc/passwd` does not list and only the system's own lookup asks.
#[cfg(unix)]
fn user_name(id: u32) -> Option<String> {
    let program = id_program(std::ffi::OsStr::new("/usr/bin:/bin"), &crate::machine::env)?;
    let told = std::process::Command::new(program)
        .arg("-nu")
        .arg(id.to_string())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8(told.stdout).ok()?;
    let name = name.trim();
    (told.status.success() && !name.is_empty()).then(|| name.to_owned())
}

/// the `id` to ask: the system's own, in a folder of `system`, else the one on the PATH `var`
/// gives as [`crate::machine::program`] finds every program fetchloom runs: a cloned project can
/// hold one in a folder PATH names.
#[cfg(unix)]
fn id_program(system: &std::ffi::OsStr, var: crate::machine::Var<'_>) -> Option<PathBuf> {
    crate::machine::installed(system, "id").or_else(|| crate::machine::program(var, "id").ok())
}

/// why a run as root takes no cache at `root` that another user owns: what it would make there
/// would be root's, which that user's runs could not change, failing them one hash at a time, as
/// with each folder of objects/ root made. a cache a group root is in shares is taken, since what
/// root makes there gets the group's write as any member's work does, and so is a new one.
pub(crate) fn root_refusal(root: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        root_refusal_as(root, run_as(), &sharing::in_group)
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        None
    }
}

/// the user this run is, which a debug build's tests may give with `FETCHLOOM_TEST_EUID`, since
/// no test runs as root.
#[cfg(unix)]
pub(crate) fn run_as() -> u32 {
    crate::machine::pretended("FETCHLOOM_TEST_EUID")
        .and_then(|id| u32::try_from(id).ok())
        .unwrap_or_else(made_as)
}

/// the user the system gives what this process makes, which [`run_as`] leaves as it is: what a
/// run finds of its own making, it compares with this.
#[cfg(unix)]
pub(crate) fn made_as() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// [`root_refusal`] for a run as `user`, in the groups `member` says.
#[cfg(unix)]
fn root_refusal_as(root: &Path, user: u32, member: &dyn Fn(u32) -> bool) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if user != 0 {
        return None;
    }
    let found = fs::metadata(root).ok()?;
    if found.uid() == 0 || sharing::shared_with(found.mode(), found.gid(), member).is_some() {
        return None;
    }
    Some(format!(
        "it belongs to {}, who could not change what this run, as root, would make there",
        said_user(found.uid())
    ))
}

/// the start of `CACHEDIR.TAG` in `root`, read without following a link.
fn tag_text(root: &Path) -> Option<String> {
    let _span = tracing::debug_span!("read_tag").entered();
    let mut text = String::new();
    read_unfollowed(&root.join("CACHEDIR.TAG"))
        .and_then(|file| file.take(4096).read_to_string(&mut text))
        .ok()?;
    Some(text)
}

/// whether `root` is a fetchloom cache: it holds the tag fetchloom writes in its cache, perhaps
/// after the byte order mark some windows tools start a file with, never only a cache's folders.
pub(crate) fn is_cache(root: &Path) -> bool {
    tag_text(root).is_some_and(|text| {
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        text.lines().next() == TAG.lines().next() && text.contains("fetchloom")
    })
}

/// whether `root` holds the start of fetchloom's tag, as a run stopped while writing it leaves.
fn tag_begun(root: &Path) -> bool {
    tag_text(root).is_some_and(|text| TAG.starts_with(&text))
}

/// writes the tag backup tools look for with `write`, unless `found` is a whole one, before any
/// folder of the cache, since a folder without it is no cache; true when this call wrote it. a
/// failed write passes once a whole tag is in place, as another run opening the cache may have
/// written it meanwhile.
fn tag(
    root: &Path,
    found: Tag,
    write: impl FnOnce(&Path, &[u8]) -> io::Result<()>,
) -> io::Result<bool> {
    if found == Tag::Whole {
        return Ok(false);
    }
    #[cfg(target_os = "macos")]
    exclude_native_backup_and_indexing(root)?;
    match write(&root.join("CACHEDIR.TAG"), TAG.as_bytes()) {
        Err(_) if is_cache(root) => Ok(false),
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

#[cfg(test)]
thread_local! {
    /// what a test does once [`Cache::find`] found an object and before its caller reads it by
    /// its name, as someone sharing the cache swapping the object for a link meanwhile would, once.
    static FOUND: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// has `swap` happen once a test's next [`Cache::find`] found an object, before its caller reads
/// the object by its name.
#[cfg(all(test, unix))]
pub(crate) fn swap_once_found(swap: impl FnOnce() + 'static) {
    FOUND.set(Some(Box::new(swap)));
}

/// whether the swap a test asked [`swap_once_found`] for happened.
#[cfg(all(test, unix))]
pub(crate) fn swapped_once_found() -> bool {
    FOUND.with_borrow(Option::is_none)
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
    /// in a cache others may write, the records this run's user wrote, kept apart where no one
    /// else may write, which stand for their bytes as any record does in a cache of one user's.
    own: Option<Arc<urls::Shards>>,
    /// the room the cache's disk had free for the archives this run unpacks at once.
    room: Arc<Room>,
    /// what the group the cache is shared with gets of what this run makes in it.
    sharing: Sharing,
    /// whether users other than this one may write the cache's url records.
    others_write: bool,
    /// whether the cache had lost its tag when this run opened it, as [`lost_its_tag`] says.
    pub(crate) lost_tag: bool,
    /// on windows, what removes the cache's files, shared by every clone.
    #[cfg(not(unix))]
    remover: Arc<Remover>,
    /// whether downloads may lock their claims, which a test turns off to stand for a disk that
    /// cannot lock files.
    #[cfg(test)]
    locks: bool,
}

/// whether the object of each SHA-256 in each cache has each MD5 a record there claims for it, as
/// this run hashed it, so an object is hashed once a run however often a record asks.
type Proved = std::collections::HashMap<(PathBuf, Md5, Sha256), bool>;
static PROVED_MD5S: std::sync::LazyLock<std::sync::Mutex<Proved>> =
    std::sync::LazyLock::new(std::sync::Mutex::default);

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
        let found = adoptable(&root)?;
        let sharing = Sharing::of(&root);
        if tag(&root, found, write)? {
            sharing.share_file(&root.join("CACHEDIR.TAG"));
            #[cfg(target_os = "macos")]
            sharing.share_file(&root.join(NEVER_INDEX));
        }
        for folder in FOLDERS {
            make_folder(&root, &root.join(folder), sharing)?;
        }
        let others_write = others_write(&root);
        Ok(Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            others_write,
            own: own_records(others_write),
            lost_tag: found == Tag::Lost,
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
        let found = adoptable(&root)?;
        for folder in FOLDERS {
            let path = root.join(folder);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => found_folder(&path, &metadata, FolderUse::Cache)?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        let sharing = Sharing::of(&root);
        let others_write = others_write(&root);
        Ok(Cache {
            volume: Arc::new(Volume::new(root.join("tmp"))),
            others_write,
            own: own_records(others_write),
            lost_tag: found == Tag::Lost,
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
        #[cfg(test)]
        if let Some(swap) = FOUND.take() {
            swap();
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

    /// `err`, from writing in `folder`, in words that say what to fix: which folder refused and,
    /// when another user owns it, who, or what unfinished downloads hold of a full disk, none of
    /// which the system says.
    pub(crate) fn explained(&self, folder: &Path, err: io::Error) -> io::Error {
        // an error fetchloom built already says what it concerns
        if err.raw_os_error().is_none() {
            return err;
        }
        if crate::volume::out_of_files(&err, folder) {
            return crate::volume::files_ran_out(err, folder, crate::error::FEWER_FILES);
        }
        let mut taken = None;
        let words = match err.kind() {
            io::ErrorKind::PermissionDenied => {
                let words = format!("{} in {}", crate::error::said(&err), folder.display());
                taken = self.closed_to_group(folder);
                match self.denied_by_owner(folder) {
                    Some((owner, back)) => {
                        taken = back.or(taken);
                        format!("{words}, which belongs to {owner}")
                    }
                    None => words,
                }
            }
            io::ErrorKind::ReadOnlyFilesystem => {
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
        io::Error::new(
            err.kind(),
            crate::error::Explained {
                words,
                help: taken,
                cause: err,
            },
        )
    }

    /// the help for `folder`, which refused a write, in a cache a group shares where the folder
    /// lets the group no write: its owner gives the group its write, as the clusters guide says,
    /// rather than a member leaving the group's cache.
    fn closed_to_group(&self, folder: &Path) -> Option<String> {
        self.sharing.opening(folder, "the cache")
    }

    /// the owner of `folder`, which refused a write, when it is another user than this run's, and
    /// where root owns it in this user's own cache, as a container run as root leaves what it
    /// makes, the help that takes the cache back. a group's cache is no one's to take.
    fn denied_by_owner(&self, folder: &Path) -> Option<(String, Option<String>)> {
        #[cfg(unix)]
        {
            let user = run_as();
            let owner = owner_of(folder).filter(|owner| *owner != user)?;
            let taken = (owner == 0
                && owner_of(&self.root) == Some(user)
                && !self.sharing.is_shared())
            .then(|| {
                // chown -R changes a link itself, never what it leads to
                let root = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
                format!(
                    "root made it, as a container run as root does: take the cache back with `sudo chown -R $(id -u):$(id -g) {}`, then run the command again",
                    crate::suggested::shell_folder_path(&root)
                )
            });
            Some((said_user(owner), taken))
        }
        #[cfg(not(unix))]
        {
            let _ = (self, folder);
            None
        }
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

    /// the latest record of what `url` served: this user's own, in a cache others may write, else
    /// whoever's the cache holds.
    pub(crate) fn url_record(&self, url: &str) -> Option<UrlRecord> {
        self.own_url_record(url)
            .or_else(|| self.urls.find(&self.root, url))
    }

    /// the record of what `url` served that this user wrote in a cache others may write.
    fn own_url_record(&self, url: &str) -> Option<UrlRecord> {
        self.own.as_ref()?.find(&self.root, url)
    }

    /// the record of what `url` served, when it may stand for those bytes without their download:
    /// any in a cache only this user writes; in one others may write, as a group shares a lab's,
    /// one this user wrote, or else one naming `was`, the bytes data.lock already pins for the
    /// link, since anyone there may add a record naming bytes of their choosing.
    pub(crate) fn vouched_url_record(&self, url: &str, was: Option<&Sha256>) -> Option<UrlRecord> {
        let shared = || self.urls.find(&self.root, url);
        if !self.others_write {
            return shared();
        }
        match (self.own_url_record(url), shared()) {
            (_, Some(record)) if was == Some(&record.sha256) => Some(record),
            (own, _) => own,
        }
    }

    /// keeps `record` of what its link served, and in a cache others may write, keeps it apart as
    /// this user's own too, so it stands for its bytes in their later runs, even where the shared
    /// records refuse it.
    pub(crate) fn save_url_record(&self, record: &UrlRecord) -> io::Result<()> {
        let shared = self.urls.save(&self.root, record);
        if let Some(own) = &self.own
            && let Err(err) = own.save(&self.root, record)
        {
            // the record only spares a later run a download, as the shared one does
            tracing::debug!(%err, "could not keep the record as this user's own");
        }
        shared
    }

    /// the object holding the bytes `url` served when an earlier download hashed them to this
    /// MD5. another link stating the same MD5 may serve other bytes, since MD5 collides.
    pub(crate) fn stated_md5(&self, md5: &Md5, url: &str) -> Option<(Sha256, u64)> {
        self.stated(md5, url)
            .map(|stated| (stated.digest, stated.size))
    }

    /// [`Cache::stated_md5`], with the S3 version the bytes were. in a cache others may write,
    /// where anyone may record any bytes for an MD5, only bytes that have it count.
    pub(crate) fn stated(&self, md5: &Md5, url: &str) -> Option<Stated> {
        let stated = self.recorded_stated(md5, url)?;
        (!self.others_write || self.holds_md5(md5, &stated.digest, stated.size)).then_some(stated)
    }

    /// the record of the bytes `url` served when an earlier download hashed them to `md5`, as
    /// written there, whoever wrote it.
    fn recorded_stated(&self, md5: &Md5, url: &str) -> Option<Stated> {
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

    /// whether [`Cache::stated`] hashes an object before its MD5 record stands for it, as in a
    /// cache others may write.
    pub(crate) fn proves_md5_records(&self) -> bool {
        self.others_write
    }

    /// in a cache others may write, proves at once on `threads` the MD5 records of the `wanted`
    /// links that [`Cache::stated`] would prove one at a time, so its later asks find them proved.
    /// a record no object proves is proved false, as there.
    pub(crate) fn prove_md5s(&self, wanted: &[(Md5, &str)], threads: crate::threads::Threads) {
        if !self.others_write {
            return;
        }
        let recorded: Vec<(Md5, Sha256, u64)> = wanted
            .iter()
            .filter_map(|(md5, url)| {
                let stated = self.recorded_stated(md5, url)?;
                Some((*md5, stated.digest, stated.size))
            })
            .collect();
        let unproved: Vec<(Md5, Sha256, u64)> = {
            let proved = PROVED_MD5S
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut seen = std::collections::HashSet::new();
            recorded
                .into_iter()
                .filter(|(md5, digest, _)| {
                    !proved.contains_key(&(self.root.clone(), *md5, *digest))
                        && seen.insert((*md5, *digest))
                })
                .collect()
        };
        if unproved.is_empty() {
            return;
        }
        let bytes = unproved
            .iter()
            .fold(0_u64, |sum, (.., size)| sum.saturating_add(*size));
        let _span = tracing::debug_span!("prove_md5s", records = unproved.len(), bytes).entered();
        // hashed as holds_md5 hashes one, without a stop, as a doubted object's are
        let held =
            crate::place::run_hashes(&unproved, bytes, threads, None, |(md5, digest, size)| {
                self.find(digest, Some(*size)).is_some_and(|path| {
                    object::md5_of(&path, *size, &crate::stop::Stop::default())
                        .is_ok_and(|found| found == Some(*md5))
                })
            });
        let mut proved = PROVED_MD5S
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ((md5, digest, _), held) in unproved.into_iter().zip(held) {
            proved.insert((self.root.clone(), md5, digest), held);
        }
    }

    /// what this run proved of the record that the object for `digest` has the MD5 `md5`.
    #[cfg(all(test, unix))]
    pub(crate) fn proved_md5(&self, md5: &Md5, digest: &Sha256) -> Option<bool> {
        PROVED_MD5S
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(self.root.clone(), *md5, *digest))
            .copied()
    }

    /// whether the object for `digest`, of `size` bytes, has the MD5 `md5`, hashed once a run.
    fn holds_md5(&self, md5: &Md5, digest: &Sha256, size: u64) -> bool {
        let key = (self.root.clone(), *md5, *digest);
        let proved = || {
            PROVED_MD5S
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if let Some(held) = proved().get(&key).copied() {
            return held;
        }
        // a cache's own checks hash without a stop, as a doubted object's do
        let held = self.find(digest, Some(size)).is_some_and(|path| {
            object::md5_of(&path, size, &crate::stop::Stop::default())
                .is_ok_and(|found| found == Some(*md5))
        });
        proved().insert(key, held);
        held
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
        let fan = path.parent().unwrap_or(&self.root);
        let first = usize::from(digest.bytes()[0]);
        if !self.checked[first].load(Ordering::Relaxed) {
            make_folder(&self.root, fan, self.sharing).map_err(|err| self.explained(fan, err))?;
            self.checked[first].store(true, Ordering::Relaxed);
        }
        let new = self
            .link_new(source, &path, &digest, size)
            .map_err(|err| self.explained(fan, err))?;
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
        // the download and the new object are one file until the download's name goes
        if new {
            volume.remove_twin(source, path)?;
        } else {
            volume.remove(source)?;
        }
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
/// read-only for this account, unless `err` says what to fix itself, as for one folder of a cache
/// a group shares.
pub(crate) fn unwritten(err: &io::Error) -> crate::error::Error {
    let error = crate::error::Error::io("could not write to the cache", err);
    if read_only(err) && crate::error::explained_help(err).is_none() {
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

    /// a full disk that ran out of files, not bytes, says so, and what needs fewer of them: an
    /// archive's members kept packed where they were being made. one with bytes run out says the
    /// disk is full, as before.
    #[test]
    fn a_disk_out_of_files_says_so() {
        let (_dir, cache) = cache();
        let partial = cache.root.join("partial");
        let full = || io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 });
        crate::volume::NO_FILES_LEFT.set(Some(true));
        let written = crate::error::Error::io(
            "could not write to the cache",
            &cache.explained(&partial, full()),
        );
        let member = crate::volume::files_ran_out(full(), &partial, crate::error::FEWER_MEMBERS);
        let unpacked =
            crate::error::Error::io("could not write \"d02/f0205.txt\" into the cache", &member);
        let denied = cache.explained(&partial, io::Error::from_raw_os_error(13));
        crate::volume::NO_FILES_LEFT.set(Some(false));
        let bytes = crate::error::Error::io(
            "could not write to the cache",
            &cache.explained(&partial, full()),
        );
        crate::volume::NO_FILES_LEFT.set(None);
        assert_eq!(
            written.message,
            "could not write to the cache: the disk has no room for more files"
        );
        assert_eq!(written.help.as_deref(), Some(crate::error::FEWER_FILES));
        assert_eq!(
            unpacked.message,
            "could not write \"d02/f0205.txt\" into the cache: the disk has no room for more files"
        );
        assert_eq!(unpacked.help.as_deref(), Some(crate::error::FEWER_MEMBERS));
        assert_ne!(denied.to_string(), crate::error::NO_FILES);
        assert_eq!(
            bytes.message,
            "could not write to the cache: the disk is full"
        );
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
        let found = super::adoptable(&cache.root).unwrap();
        super::tag(&cache.root, found, |_, _| {
            panic!("a tag that is there is not written again")
        })
        .unwrap();
        fs::remove_file(&tag).unwrap();
        super::tag(&cache.root, Tag::None, |path, bytes| {
            // the other run's tag lands first, and this run's write is refused
            fs::write(path, bytes)?;
            Err(denied())
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&tag).unwrap(), TAG);
        fs::remove_file(&tag).unwrap();
        let refused = super::tag(&cache.root, Tag::None, |_, _| Err(denied())).unwrap_err();
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
    /// even when its folders have a cache's names, as a scratch folder's may, or a cache that lost
    /// its tag holds a file of a person's.
    #[test]
    fn a_folder_of_other_files_never_becomes_the_cache() {
        let folders: [&[&str]; 8] = [
            &["tmp/important.txt", "thesis.tex"],
            &["tmp/important.txt"],
            &["tmp/notes.txt", "objects/pack1"],
            &["projects/myapp/main.py"],
            &[
                "objects/ab/cdef",
                "partial/0/",
                "gc.lock",
                "objects/notes.txt",
            ],
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
        assert!(is_cache(&root));
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

    /// a folder holding only what a cache holds but no tag is a cache whose tag is gone, as a
    /// cleaner leaves one, or as a build that made a cache's folders before its tag left one when
    /// stopped between them: opening it puts the tag back, and reading it as it is writes nothing.
    /// a scratch folder with a cache's names and files of its own stays refused.
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
        for root in [cache.root.clone(), stopped.path().to_path_buf()] {
            let read = Cache::unopened(root.clone()).unwrap();
            assert!(read.lost_tag, "{}", root.display());
            assert!(!root.join("CACHEDIR.TAG").exists(), "{}", root.display());
            assert!(Cache::open(root.clone()).unwrap().lost_tag);
            assert_eq!(fs::read_to_string(root.join("CACHEDIR.TAG")).unwrap(), TAG);
            assert!(!Cache::open(root.clone()).unwrap().lost_tag);
        }
        let scratch = tempfile::tempdir().unwrap();
        for file in ["tmp/notes.txt", "objects/pack1"] {
            let path = scratch.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"mine").unwrap();
        }
        let refusals = [
            Cache::open(scratch.path().to_path_buf()).unwrap_err(),
            Cache::unopened(scratch.path().to_path_buf()).unwrap_err(),
        ];
        for error in refusals {
            assert!(
                error.to_string().starts_with("it holds other files"),
                "{error}"
            );
        }
        assert!(!scratch.path().join("CACHEDIR.TAG").exists());
    }

    /// a run as root makes what it makes as root, which the owner of a cache that is another
    /// user's could not change, so root refuses such a cache before writing there, unless a group
    /// root is in shares it, which then gets the group's write as any member's work does. root's
    /// own cache, a new one, and any cache for a run that is not root's are taken.
    #[cfg(unix)]
    #[test]
    fn root_refuses_a_cache_another_user_owns() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        let owner = owner_of(&root).unwrap();
        if owner == 0 {
            return;
        }
        let none = |_: u32| false;
        assert_eq!(
            root_refusal_as(&root, 0, &none),
            Some(format!(
                "it belongs to {}, who could not change what this run, as root, would make there",
                said_user(owner)
            ))
        );
        assert_eq!(root_refusal_as(&root, owner, &none), None);
        assert_eq!(root_refusal_as(&dir.path().join("new"), 0, &none), None);
        assert_eq!(root_refusal_as(Path::new("/"), 0, &none), None);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o2770)).unwrap();
        let group = fs::metadata(&root).unwrap().gid();
        assert_eq!(root_refusal_as(&root, 0, &|id| id == group), None);
        assert!(root_refusal_as(&root, 0, &none).is_some());
    }

    /// a write refused in a folder another user owns names that owner, root by name, and where root
    /// owns a folder of this user's own cache, as a container run as root leaves one, the help
    /// takes the cache back; a folder this user owns is said as it was.
    #[cfg(unix)]
    #[test]
    fn a_refused_write_names_the_folder_and_its_owner() {
        let (_dir, cache) = cache();
        if owner_of(&cache.root) == Some(0) {
            return;
        }
        let denied = || io::Error::from_raw_os_error(13);
        let objects = cache.root.join("objects");
        let error =
            crate::error::Error::io("could not write", &cache.explained(&objects, denied()));
        assert_eq!(
            error.to_string(),
            format!(
                "could not write: permission was denied in {}",
                objects.display()
            )
        );
        // "/" stands in for a folder root made in the cache
        let error = crate::error::Error::io(
            "could not write",
            &cache.explained(Path::new("/"), denied()),
        );
        assert_eq!(
            error.to_string(),
            "could not write: permission was denied in /, which belongs to root"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                format!(
                    "root made it, as a container run as root does: take the cache back with `sudo chown -R $(id -u):$(id -g) {}`, then run the command again",
                    crate::suggested::shell_folder_path(&fs::canonicalize(&cache.root).unwrap())
                )
                .as_str()
            )
        );
        // a user no name is known for, as a container's host user, by the number `id -u` gives;
        // macos names 4294967294 nobody, and no system gives out this one
        assert_eq!(said_user(3_999_999_999), "user 3999999999");
    }

    /// where the system's folders hold no `id`, as on nixos, the one asked is never one the
    /// project holds, even in a folder PATH names in full, as an activated environment's is.
    #[cfg(unix)]
    #[test]
    fn the_id_asked_is_never_one_the_project_holds() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (system, project, outside) = (
            root.join("system"),
            root.join("project"),
            root.join("outside"),
        );
        let planted = project.join(".venv").join("bin");
        fs::create_dir(&system).unwrap();
        for folder in [&planted, &outside] {
            fs::create_dir_all(folder).unwrap();
            let id = folder.join("id");
            fs::write(&id, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&id, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([&planted, &outside]).unwrap();
        let var = |name: &str| (name == "PATH").then(|| path.clone());
        let _distrusted = crate::machine::distrusting(project);
        assert_eq!(
            id_program(system.as_os_str(), &var),
            Some(outside.join("id"))
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
    /// and never anything to others; an object stays read-only, and the records a member keeps as
    /// their own stay theirs alone. it runs under `umask 022` in a process of its own, so the umask
    /// of the process running the tests stays as it was.
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
    /// cannot use as the owner can, and what every user may write, sorted. the folder of this
    /// user's own records must be theirs alone instead.
    #[cfg(unix)]
    fn unshared_below(root: &Path) -> Vec<String> {
        use std::os::unix::fs::MetadataExt;
        let group = fs::metadata(root).unwrap().gid();
        let own = root.join("urls").join(format!("user-{}", run_as()));
        let mut wrong = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                let found = fs::symlink_metadata(&path).unwrap();
                let mode = found.mode() & 0o7777;
                let shown = format!("{mode:o} {}", path.display());
                if path == own {
                    if mode & 0o077 != 0 {
                        wrong.push(format!("others may enter {shown}"));
                    }
                    continue;
                }
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

    /// in a cache its group shares, each folder only its owner may write keeps the group's other
    /// members from adding there, so the warning names every one of them, and past three, how many
    /// more there are.
    #[cfg(unix)]
    #[test]
    fn every_folder_the_group_cannot_write_is_named() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, cache) = cache();
        let root = &cache.root;
        let set = |name: &str, mode| {
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(mode)).unwrap();
        };
        fs::set_permissions(root, fs::Permissions::from_mode(0o2770)).unwrap();
        if fs::metadata(root).unwrap().mode() & 0o7777 != 0o2770 {
            return crate::test_skips::skip("permissions", "this disk keeps no setgid folder");
        }
        for name in FOLDERS {
            set(name, 0o2770);
        }
        assert_eq!(unshared(root), None);
        let told = |names: &str| {
            format!(
                "the folders {names} in the cache let only their owners write to them, though a group shares the cache, so the group's other members cannot add to them"
            )
        };
        let shown = |name: &str| root.join(name).display().to_string();
        // as `chmod g-w objects urls` leaves them
        set("objects", 0o2750);
        set("urls", 0o2750);
        assert_eq!(
            unshared(root),
            Some(told(&format!("{} and {}", shown("objects"), shown("urls"))))
        );
        set("tmp", 0o2750);
        set("keyed", 0o2750);
        assert_eq!(
            unshared(root),
            Some(told(&format!(
                "{}, {}, {} and 1 more",
                shown("objects"),
                shown("urls"),
                shown("tmp")
            )))
        );
    }

    /// in a cache its group shares, a write a folder there refuses that lets the group no write
    /// says how to give the group its write, for the folder's owner to run, never to leave the
    /// group's cache; a folder whose owner is this user, who may not write it either, gets both.
    #[cfg(unix)]
    #[test]
    fn a_write_a_folder_closed_to_the_group_refuses_says_how_to_open_it() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, cache) = cache();
        let root = cache.root.clone();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o2770)).unwrap();
        if fs::metadata(&root).unwrap().mode() & 0o7777 != 0o2770 {
            return crate::test_skips::skip("permissions", "this disk keeps no setgid folder");
        }
        let shared = Cache::open(root.clone()).unwrap();
        let folder = root.join("partial").join("f");
        fs::create_dir_all(&folder).unwrap();
        let denied = || io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error());
        for (mode, help) in [
            (
                0o2550,
                Some(format!(
                    "the folder is yours, and a group shares the cache: give yourself and the group its write with `chmod ug+rwx {}`",
                    folder.display()
                )),
            ),
            (0o2770, None),
        ] {
            fs::set_permissions(&folder, fs::Permissions::from_mode(mode)).unwrap();
            let error = unwritten(&shared.explained(&folder, denied()));
            let help = help.unwrap_or_else(|| READ_ONLY.to_owned());
            assert_eq!(error.help.as_deref(), Some(help.as_str()), "{mode:o}");
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
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

    /// a cache on a disk that links files but refuses to remove a read-only one, as `ReFS` on windows
    /// server 2022 does, still takes a sealed download as a new object, leaving no download behind
    /// and the object read-only, and drops a second download of the same bytes.
    #[test]
    fn a_disk_that_links_but_keeps_read_only_files_still_takes_objects() {
        let (dir, mut cache) = cache();
        if !hardlinks(dir.path()) {
            return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
        }
        cache.volume = Arc::new(Volume::refusing(cache.root.join("tmp"), true));
        let digest = crate::digest::sha256(b"good");
        for (name, new) in [("first", true), ("second", false)] {
            let source = dir.path().join(name);
            fs::write(&source, b"good").unwrap();
            seal(&source);
            let stored = cache.link_object(&source, digest, 4, None).unwrap();
            assert_eq!(stored.new, new, "{name}");
            assert!(!source.exists(), "{name}");
        }
        let object = cache.object_path(&digest);
        assert_eq!(fs::read(&object).unwrap(), b"good");
        assert!(fs::metadata(&object).unwrap().permissions().readonly());
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

    /// a record stands for its bytes in a cache only this user writes; in one a group shares, or
    /// its group or every user may write, only when it names the pin it is asked with, since
    /// anyone there may have written it.
    #[cfg(unix)]
    #[test]
    fn a_record_others_may_have_written_stands_only_for_the_pinned_bytes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, cache) = cache();
        let recorded = store(&cache, b"recorded").digest;
        let other = digest::sha256(b"pinned before");
        let url = "https://example.org/iris.csv";
        cache
            .save_url_record(&UrlRecord {
                url: url.to_owned(),
                sha256: recorded,
                size: 8,
                name: None,
                validators: Validators::default(),
                page: false,
            })
            .unwrap();
        for was in [None, Some(&recorded), Some(&other)] {
            assert!(cache.vouched_url_record(url, was).is_some(), "{was:?}");
        }
        // the cache's folder as a group shares it, or its group or every user may write it, or
        // the `urls` folder in it as every user may enter the cache and write it
        let urls = cache.root.join("urls");
        for (root, records) in [
            (0o770, 0o700),
            (0o777, 0o700),
            (0o755, 0o777),
            (0o2770, 0o700),
        ] {
            fs::set_permissions(&urls, fs::Permissions::from_mode(records)).unwrap();
            fs::set_permissions(&cache.root, fs::Permissions::from_mode(root)).unwrap();
            if fs::metadata(&cache.root).unwrap().mode() & 0o7777 != root {
                return crate::test_skips::skip("permissions", "this disk keeps no setgid folder");
            }
            let opened = Cache::open(cache.root.clone()).unwrap();
            for (was, stands) in [
                (None, false),
                (Some(&recorded), true),
                (Some(&other), false),
            ] {
                assert_eq!(
                    opened.vouched_url_record(url, was).is_some(),
                    stands,
                    "{root:o} {records:o} {was:?}"
                );
            }
            for folder in [&cache.root, &urls] {
                fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }

    /// in a cache others may write, a record this user wrote is kept apart, in a folder of `urls/`
    /// no one else may enter, and stands for its bytes in their later runs as in a cache of their
    /// own, until gc forgets it with its object; one only in the shared records, as another
    /// member's is, or once that folder lets others in, stands only for the pin it names.
    #[cfg(unix)]
    #[test]
    fn a_record_this_user_wrote_stands_in_a_cache_others_may_write() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, cache) = cache();
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o770)).unwrap();
        let shared = Cache::open(cache.root.clone()).unwrap();
        let recorded = store(&shared, b"recorded").digest;
        let other = digest::sha256(b"pinned before");
        let record = |url: &str| UrlRecord {
            url: url.to_owned(),
            sha256: recorded,
            size: 8,
            name: None,
            validators: Validators::default(),
            page: false,
        };
        let (mine, theirs) = ("https://example.org/mine", "https://example.org/theirs");
        shared.save_url_record(&record(mine)).unwrap();
        shared.urls.save(&shared.root, &record(theirs)).unwrap();
        let stands = |cache: &Cache, url: &str| {
            [None, Some(&recorded), Some(&other)]
                .map(|was| cache.vouched_url_record(url, was).is_some())
        };
        let later = Cache::open(cache.root.clone()).unwrap();
        for opened in [&shared, &later] {
            assert_eq!(stands(opened, mine), [true, true, true]);
            assert_eq!(stands(opened, theirs), [false, true, false]);
        }
        let own = cache.root.join("urls").join(format!("user-{}", run_as()));
        fs::set_permissions(&own, fs::Permissions::from_mode(0o750)).unwrap();
        let entered = Cache::open(cache.root.clone()).unwrap();
        assert_eq!(stands(&entered, mine), [false, true, false]);
        fs::set_permissions(&own, fs::Permissions::from_mode(0o700)).unwrap();
        later.sweep(
            &Needed::default(),
            std::time::SystemTime::now() + GRACE + std::time::Duration::from_secs(60),
        );
        let swept = Cache::open(cache.root.clone()).unwrap();
        assert_eq!(swept.url_record(mine), None, "gc forgot it with its object");
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// in a cache others may write, a record the shared records refuse, as when another member
    /// left them read-only, is still kept as this user's own, which their later runs read.
    #[cfg(unix)]
    #[test]
    fn a_record_the_shared_records_refuse_is_still_kept_as_this_users_own() {
        use std::os::unix::fs::PermissionsExt;
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores file permissions");
        }
        let (_dir, cache) = cache();
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o770)).unwrap();
        let shared = Cache::open(cache.root.clone()).unwrap();
        let recorded = store(&shared, b"recorded").digest;
        let record = |url: &str| UrlRecord {
            url: url.to_owned(),
            sha256: recorded,
            size: 8,
            name: None,
            validators: Validators::default(),
            page: false,
        };
        shared
            .save_url_record(&record("https://example.org/first"))
            .unwrap();
        let urls = cache.root.join("urls");
        let logs: Vec<PathBuf> = fs::read_dir(&urls)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .collect();
        let close = |file: u32, folder: u32| {
            for log in &logs {
                fs::set_permissions(log, fs::Permissions::from_mode(file)).unwrap();
            }
            fs::set_permissions(&urls, fs::Permissions::from_mode(folder)).unwrap();
        };
        close(0o440, 0o550);
        let mine = "https://example.org/mine";
        let saved = shared.save_url_record(&record(mine));
        close(0o660, 0o770);
        assert_eq!(saved.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let later = Cache::open(cache.root.clone()).unwrap();
        assert!(later.vouched_url_record(mine, None).is_some());
        fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// in a cache others may write, MD5 records proved at once answer as records proved one at a
    /// time as asked do: an honest one, one whose object has another MD5, one whose object is gone,
    /// each proved before anything asks. a cache only this user writes proves none.
    #[cfg(unix)]
    #[test]
    fn md5_records_proved_at_once_answer_as_each_proved_alone() {
        use std::os::unix::fs::PermissionsExt;
        let md5 = |bytes: &[u8]| {
            let mut hasher = digest::Md5Hasher::default();
            hasher.update(bytes);
            hasher.finish()
        };
        let mut answers = Vec::new();
        for ahead in [false, true] {
            let (_dir, cache) = cache();
            let honest = store(&cache, b"honest bytes");
            let other = store(&cache, b"other bytes");
            let records = [
                ("https://example.org/honest", md5(b"honest bytes"), honest),
                ("https://example.org/other", md5(b"not these"), other),
                (
                    "https://example.org/gone",
                    md5(b"gone"),
                    Stored {
                        digest: digest::sha256(b"gone"),
                        size: 4,
                        md5: None,
                        new: true,
                    },
                ),
            ];
            for (url, md5, stored) in &records {
                let stated = Stated {
                    digest: stored.digest,
                    size: stored.size,
                    version: None,
                };
                cache.save_stated_md5(md5, url, &stated).unwrap();
            }
            let wanted: Vec<(Md5, &str)> = records
                .iter()
                .map(|(url, md5, _)| (*md5, *url))
                .chain([(md5(b"no record"), "https://example.org/none")])
                .collect();
            let proved = |cache: &Cache| {
                records
                    .iter()
                    .map(|(_, md5, stored)| cache.proved_md5(md5, &stored.digest))
                    .collect::<Vec<_>>()
            };
            if ahead {
                cache.prove_md5s(&wanted, crate::threads::Threads::default());
                assert_eq!(proved(&cache), [None, None, None]);
            }
            fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o770)).unwrap();
            let shared = Cache::open(cache.root.clone()).unwrap();
            if ahead {
                shared.prove_md5s(&wanted, crate::threads::Threads::default());
                assert_eq!(proved(&shared), [Some(true), Some(false), Some(false)]);
            }
            answers.push(
                records
                    .iter()
                    .map(|(url, md5, _)| shared.stated(md5, url).is_some())
                    .collect::<Vec<_>>(),
            );
            fs::set_permissions(&cache.root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(answers[0], [true, false, false]);
        assert_eq!(answers[1], answers[0]);
    }

    /// others may write a cache whose folder, or a folder of its records they may reach, every
    /// user may write, though its group may not, or that another user owns, as the user a run is
    /// sees it; a folder in a cache only its owner may enter is no one else's.
    #[cfg(unix)]
    #[test]
    fn a_cache_another_user_owns_or_every_user_may_write_is_written_by_others() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, cache) = cache();
        let root = cache.root.clone();
        let records = [
            ("its urls folder", root.join("urls")),
            ("its MD5 folder", root.join("stated")),
            ("its MD5 records folder", root.join("stated").join("md5")),
            ("its unpacked folder", root.join("unpacked")),
        ];
        for (_, folder) in &records {
            fs::create_dir_all(folder).unwrap();
            fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mine = fs::metadata(&root).unwrap().uid();
        let mode = |folder: &Path, mode: u32| {
            fs::set_permissions(folder, fs::Permissions::from_mode(mode)).unwrap();
            others_write_as(&root, mine)
        };
        let mut seen = vec![
            ("private".to_owned(), mode(&root, 0o700)),
            ("private, every user enters".to_owned(), mode(&root, 0o755)),
            (
                "its folder, every user writes".to_owned(),
                mode(&root, 0o757),
            ),
        ];
        mode(&root, 0o700);
        let another = others_write_as(&root, mine + 1);
        seen.push(("its folder, another user's".to_owned(), another));
        for (case, folder) in &records {
            mode(&root, 0o755);
            seen.push((format!("{case}, every user writes"), mode(folder, 0o757)));
            let alone = mode(&root, 0o700);
            seen.push((format!("{case}, in a cache only its owner enters"), alone));
            mode(folder, 0o755);
            let another = others_may_write(folder, mine + 1);
            seen.push((format!("{case}, another user's"), another));
        }
        let urls = &records[0].1;
        mode(&root, 0o750);
        let group = mode(urls, 0o770);
        seen.push((
            "its urls folder, in a cache its group enters".to_owned(),
            group,
        ));
        mode(&root, 0o701);
        let others = mode(urls, 0o707);
        seen.push((
            "its urls folder, in a cache every user enters".to_owned(),
            others,
        ));
        let expected: Vec<(String, bool)> = seen
            .iter()
            .map(|(case, _)| {
                let alone = case.starts_with("private") || case.ends_with("only its owner enters");
                (case.clone(), !alone)
            })
            .collect();
        assert_eq!(seen, expected);
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
