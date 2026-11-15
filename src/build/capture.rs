//! taking in what a step wrote, never following a link. a file with another name is copied rather
//! than moved, so a step that linked a person's own file never makes it the cache's.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use icu_normalizer::ComposingNormalizerBorrowed;

use crate::cache::{Cache, Copied, Member, Seen};
use crate::digest::{Block, Hasher, Sha256};
use crate::name;
use crate::place::run_all;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::work::Work;

/// what a step wrote that a result cannot hold, by its path in the result folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// the folder holds no file at all.
    Nothing,
    Link(String),
    /// a device, a fifo or a socket.
    Special(String),
    /// a name that is not UTF-8 text, shown as near as text can.
    NotText(String),
    /// a name some disk cannot store, and why.
    Name(String, String),
    /// two names one disk would take for one: by case, by how an accent is written, or a file
    /// where the other needs a folder.
    Clash(String, String),
    /// more files than the `most_files` setting allows.
    TooMany(usize),
    /// a file that changed while it was taken in.
    Changed(String),
}

/// why taking in a result failed.
#[derive(Debug)]
pub(crate) enum Fault {
    Refused(Refused),
    /// the disk or the cache failed, or a stop was asked, which is `Interrupted`.
    Io(io::Error),
}

impl From<io::Error> for Fault {
    fn from(err: io::Error) -> Fault {
        Fault::Io(err)
    }
}

impl From<Refused> for Fault {
    fn from(refused: Refused) -> Fault {
        Fault::Refused(refused)
    }
}

/// a result read from a step's folder and hashed, not in the cache yet.
#[derive(Debug)]
pub(crate) struct Found {
    files: Vec<Taken>,
    /// whether a file may move into the cache rather than be copied.
    moves: bool,
    _holds: Holds,
    /// sorted by path.
    pub(crate) members: Vec<Member>,
    pub(crate) tree: Sha256,
}

/// on windows, the folders a result's walk held, each with those from the cache's root down to it,
/// so none is renamed or swapped for a junction until the result is in the cache.
#[derive(Default)]
struct Holds(#[cfg(windows)] Vec<Box<dyn std::any::Any + Send + Sync>>);

impl std::fmt::Debug for Holds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Holds")
    }
}

#[cfg(test)]
thread_local! {
    /// what a test does once a result is walked and before its files are read, as someone who
    /// shares the cache swapping one of its folders for a link meanwhile would, once.
    pub(crate) static WALKED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// a step's `out` folder to take a result from. on unix it is held open, and every file in it is
/// reached from it without following a link, so a folder put in place of the step's own on the
/// way there is never read. on windows each folder is held from the cache's root down while the
/// result is taken in, which keeps it from being replaced.
pub(crate) struct Out {
    path: PathBuf,
    #[cfg(unix)]
    handle: Arc<fs::File>,
    /// on windows, the cache's folder, from which the walk holds the folders down to each it lists.
    #[cfg(not(unix))]
    root: Option<PathBuf>,
    /// whether a file may move into the cache, the folder being on its disk, rather than be copied.
    moves: bool,
}

impl Out {
    /// the folder at `path`, opened without following a link where it ends.
    #[cfg(test)]
    #[cfg_attr(
        not(unix),
        expect(
            clippy::unnecessary_wraps,
            reason = "unix opens the folder, which windows only names"
        )
    )]
    pub(crate) fn open(path: &Path) -> io::Result<Out> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let handle = fs::File::from(rustix::fs::open(path, flags, Mode::empty())?);
            Ok(Out::held(path.to_path_buf(), handle, true))
        }
        #[cfg(not(unix))]
        Ok(Out {
            path: path.to_path_buf(),
            root: None,
            moves: false,
        })
    }

    /// the folder at `path`, below the cache's folder `root`, from which the walk holds each
    /// folder it lists.
    #[cfg(not(unix))]
    pub(crate) fn below(root: PathBuf, path: PathBuf) -> Out {
        Out {
            path,
            root: Some(root),
            moves: false,
        }
    }

    /// the folder `handle` holds open, shown as `path`, whose files may move into the cache when
    /// `moves` says so.
    #[cfg(unix)]
    pub(crate) fn held(path: PathBuf, handle: fs::File, moves: bool) -> Out {
        Out {
            path,
            handle: Arc::new(handle),
            moves,
        }
    }
}

/// where a file the step wrote is: its path, for messages and, on windows, for every look, and on
/// unix its name in the folder held open that holds it.
#[derive(Clone, Debug)]
struct Spot {
    disk: PathBuf,
    #[cfg(unix)]
    at: (Arc<fs::File>, std::ffi::CString),
}

impl Spot {
    /// the file, opened to read without following a link.
    fn open(&self) -> io::Result<fs::File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            let (folder, name) = &self.at;
            let file = fs::File::from(rustix::fs::openat(
                &**folder,
                name.as_c_str(),
                flags,
                Mode::empty(),
            )?);
            if !crate::volume::is_plain(&file.metadata()?) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} is no longer a plain file", self.disk.display()),
                ));
            }
            Ok(file)
        }
        #[cfg(not(unix))]
        crate::volume::read_unfollowed(&self.disk)
    }
}

/// one file of a result: where the step wrote it, what it holds, and how it looked then.
#[derive(Debug)]
struct Taken {
    spot: Spot,
    member: Member,
    /// unix moves a file into the cache and checks it is still the file hashed; windows copies
    /// every file, checking its bytes as it copies.
    #[cfg(unix)]
    seen: Seen,
}

/// reads everything `out` holds, checks every name and hashes every file on `threads`. it fails on
/// what a result cannot hold, as more than `most` files, on a failed read, or with a stop.
pub(crate) fn read(out: &Out, most: usize, threads: Threads, stop: &Stop) -> Result<Found, Fault> {
    read_with(out, most, threads, stop, &|_| {})
}

/// [`read`], calling `read_once` with each file's path once its bytes are read and before it is
/// looked at again, which a test uses to change it there.
fn read_with(
    out: &Out,
    most: usize,
    threads: Threads,
    stop: &Stop,
    read_once: &(dyn Fn(&Path) + Sync),
) -> Result<Found, Fault> {
    let span = tracing::debug_span!("capture", files = tracing::field::Empty).entered();
    let (paths, holds) = walk(out, most, stop)?;
    span.record("files", paths.len());
    if paths.is_empty() {
        return Err(Refused::Nothing.into());
    }
    let named = names(&paths)?;
    #[cfg(test)]
    if let Some(swap) = WALKED.take() {
        swap();
    }
    let taken = run_all(&named, threads, |(spot, path)| {
        take(spot, path, stop, read_once)
    });
    let mut files = taken.into_iter().collect::<Result<Vec<_>, _>>()?;
    files.sort_by(|a, b| a.member.path.cmp(&b.member.path));
    let members: Vec<Member> = files.iter().map(|file| file.member.clone()).collect();
    Ok(Found {
        tree: crate::cache::tree_hash(&members),
        members,
        files,
        moves: out.moves,
        _holds: holds,
    })
}

/// every file under `out` with its path as the disk spells it, found without following a link. a
/// folder the step took its owner's access from gets it back, so its files can be read and moved.
#[cfg(unix)]
fn walk(out: &Out, most: usize, stop: &Stop) -> Result<(Vec<(Spot, String)>, Holds), Fault> {
    use rustix::fs::{AtFlags, FileType};
    let mut files = Vec::new();
    let mut folders = vec![(Arc::clone(&out.handle), out.path.clone(), String::new())];
    while let Some((folder, disk, inside)) = folders.pop() {
        stop.check_io()?;
        for name in crate::cache::names_at(&folder)? {
            let Ok(text) = name.to_str() else {
                return Err(
                    Refused::NotText(name::joined(&inside, &name.to_string_lossy())).into(),
                );
            };
            let path = name::joined(&inside, text);
            let found = rustix::fs::statat(&*folder, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(io::Error::from)?;
            match FileType::from_raw_mode(found.st_mode) {
                FileType::Symlink => return Err(Refused::Link(path).into()),
                FileType::Directory => {
                    let inner = crate::cache::enter_at(&folder, name.as_c_str())?;
                    folders.push((Arc::new(inner), disk.join(text), path));
                }
                FileType::RegularFile => {
                    if files.len() == most {
                        return Err(Refused::TooMany(most).into());
                    }
                    let spot = Spot {
                        disk: disk.join(text),
                        at: (Arc::clone(&folder), name),
                    };
                    files.push((spot, path));
                }
                _ => return Err(Refused::Special(path).into()),
            }
        }
    }
    Ok((files, Holds::default()))
}

#[cfg(not(unix))]
fn walk(out: &Out, most: usize, stop: &Stop) -> Result<(Vec<(Spot, String)>, Holds), Fault> {
    let mut files = Vec::new();
    let mut holds = Holds::default();
    let mut folders = vec![(out.path.clone(), String::new())];
    while let Some((folder, inside)) = folders.pop() {
        stop.check_io()?;
        // held before it is listed, a folder is never one swapped in once listed
        #[cfg(windows)]
        if let Some(root) = &out.root {
            let held = crate::place::hold_below(root, &folder.join("member"))?;
            holds.0.push(Box::new(held));
        }
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(
                    Refused::NotText(name::joined(&inside, &name.to_string_lossy())).into(),
                );
            };
            let path = name::joined(&inside, name);
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(Refused::Link(path).into());
            } else if kind.is_dir() {
                folders.push((entry.path(), path));
            } else if kind.is_file() {
                if files.len() == most {
                    return Err(Refused::TooMany(most).into());
                }
                files.push((Spot { disk: entry.path() }, path));
            } else {
                return Err(Refused::Special(path).into());
            }
        }
    }
    Ok((files, holds))
}

/// each file's path in NFC, as data.lock records it, once every name passed: none leaves the
/// folder, names repository metadata, fails to store on some disk or clashes with another.
fn names<D: Clone>(paths: &[(D, String)]) -> Result<Vec<(D, String)>, Refused> {
    let nfc = ComposingNormalizerBorrowed::new_nfc();
    let mut files: HashMap<String, &str> = HashMap::with_capacity(paths.len());
    let mut folders: HashMap<String, &str> = HashMap::new();
    let mut named = Vec::with_capacity(paths.len());
    for (disk, path) in paths {
        let composed = nfc.normalize(path).into_owned();
        let reason = name::escapes(&composed)
            .or_else(|| name::metadata(&composed))
            .map(str::to_owned)
            .or_else(|| name::unportable(&composed));
        if let Some(reason) = reason {
            return Err(Refused::Name(path.clone(), reason));
        }
        let key = name::fold(&composed);
        if let Some(other) = files.insert(key.clone(), path) {
            return Err(Refused::Clash(other.to_owned(), path.clone()));
        }
        let mut folder = key.as_str();
        while let Some((parent, _)) = folder.rsplit_once('/') {
            folders.entry(parent.to_owned()).or_insert(path);
            folder = parent;
        }
        named.push((disk.clone(), composed));
    }
    let keys: HashSet<&String> = files.keys().collect();
    if let Some((folder, inside)) = folders.iter().find(|(folder, _)| keys.contains(folder)) {
        return Err(Refused::Clash(
            files[folder].to_owned(),
            (*inside).to_owned(),
        ));
    }
    Ok(named)
}

/// hashes the file at `spot` unfollowed, checking it looked the same before and after.
fn take(
    spot: &Spot,
    path: &str,
    stop: &Stop,
    read_once: &(dyn Fn(&Path) + Sync),
) -> Result<Taken, Fault> {
    let mut file = spot.open()?;
    let before = Seen::of(&file.metadata()?);
    let mut hasher = Hasher::default();
    let mut block = Block::sized(before.size.saturating_add(1));
    let block = block.get();
    let mut size = 0;
    loop {
        stop.check_io()?;
        let read = match file.read(block) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        hasher.update(&block[..read]);
        size += read as u64;
    }
    read_once(&spot.disk);
    if Seen::of(&file.metadata()?) != before || size != before.size {
        return Err(Refused::Changed(path.to_owned()).into());
    }
    Ok(Taken {
        spot: spot.clone(),
        member: Member {
            path: path.to_owned(),
            size,
            sha256: hasher.finish().0,
        },
        #[cfg(unix)]
        seen: before,
    })
}

/// puts every file of `found` in `cache`, each on the disk before any takes its name in
/// `objects/`, counting new objects in `work`. it fails on a file changed since it was read, a
/// cache that cannot take it, or a stop.
pub(crate) fn publish(
    cache: &Cache,
    found: &Found,
    threads: Threads,
    stop: &Arc<Stop>,
    work: &Work,
) -> Result<(), Fault> {
    let _span = tracing::debug_span!("publish", files = found.files.len()).entered();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = if found.files.len() > crate::volume::MOST_FLUSHED_ALONE {
        cache.batch()?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush = true;
    let sealed = run_all(&found.files, threads, |file| {
        seal(cache, file, flush, found.moves, stop)
    });
    let sealed = sealed.into_iter().collect::<Result<Vec<_>, _>>()?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        let _span = tracing::debug_span!("flush").entered();
        batch.flush()?;
    }
    let mut copies = Vec::new();
    let mut moved = Vec::new();
    for (file, sealed) in found.files.iter().zip(sealed) {
        match sealed {
            Some(copy) => copies.push(copy),
            None => moved.push(file),
        }
    }
    let published = run_all(&moved, threads, |file| move_in(cache, file));
    for (file, new) in moved.iter().zip(published) {
        if new? {
            work.wrote(&file.member.sha256);
        }
    }
    for copy in copies {
        stop.check_io()?;
        let stored = copy.publish(cache)?;
        if stored.new {
            work.wrote(&stored.digest);
        }
    }
    Ok(())
}

/// readies `file` to enter the cache: sealed where it is when it may move, which gives none, or
/// copied into the cache's `tmp/` otherwise, on unix through the folder held open, so a folder
/// swapped on the way since it was read never sends another file's bytes in.
fn seal(
    cache: &Cache,
    file: &Taken,
    flush: bool,
    moves: bool,
    stop: &Stop,
) -> Result<Option<crate::cache::Sealed>, Fault> {
    stop.check_io()?;
    #[cfg(unix)]
    if moves && file.seen.movable {
        let sealed = file
            .spot
            .open()
            .and_then(|opened| crate::cache::seal_captured(&opened, &file.seen, flush));
        match sealed {
            Ok(()) => return Ok(None),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                return Err(Refused::Changed(file.member.path.clone()).into());
            }
            Err(err) => {
                tracing::debug!(%err, path = file.member.path, "could not seal a step's file where it is, so it is copied");
            }
        }
    }
    #[cfg(unix)]
    let copied = match file.spot.open() {
        Ok(input) => {
            cache.copy_opened(input, &file.member.sha256, file.member.size, flush, stop)?
        }
        Err(err)
            if err.kind() == io::ErrorKind::InvalidInput
                || err.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) =>
        {
            Copied::NotPlain
        }
        Err(err) => Copied::Unread(err),
    };
    #[cfg(not(unix))]
    let copied = {
        let _ = moves;
        cache.copy_in(
            &file.spot.disk,
            &file.member.sha256,
            file.member.size,
            flush,
            stop,
        )?
    };
    match copied {
        Copied::Sealed(sealed) => Ok(Some(sealed)),
        Copied::OtherSize | Copied::OtherBytes | Copied::NotPlain => {
            Err(Refused::Changed(file.member.path.clone()).into())
        }
        Copied::Unread(err) => Err(err.into()),
    }
}

/// moves a sealed file into `objects/`, and says whether the cache lacked its bytes. it leaves the
/// step's folder by its name in the folder held open, into the cache's tmp/, so a folder swapped
/// on the way since never sends another file into the cache.
#[cfg(unix)]
fn move_in(cache: &Cache, file: &Taken) -> Result<bool, Fault> {
    use std::os::fd::AsFd;
    let (folder, name) = &file.spot.at;
    let staged = cache
        .root()
        .join("tmp")
        .join(format!("{:016x}.capture", fastrand::u64(..)));
    crate::volume::rename_new_at(
        folder.as_fd(),
        name.as_c_str(),
        rustix::fs::CWD,
        staged.as_path(),
        crate::volume::Lacking::Nothing,
    )?;
    let published = cache.publish_captured(&staged, file.member.sha256, &file.seen);
    if published.is_err() {
        let _ = fs::remove_file(&staged);
    }
    match published {
        Ok(stored) => Ok(stored.new),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => {
            Err(Refused::Changed(file.member.path.clone()).into())
        }
        Err(err) => Err(err.into()),
    }
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "windows copies every file, so nothing moves"
)]
fn move_in(_cache: &Cache, _file: &Taken) -> Result<bool, Fault> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::cache;

    fn read_all(out: &Path) -> Result<Found, Fault> {
        read(&Out::open(out)?, 100, Threads::default(), &Stop::default())
    }

    fn refused(out: &Path) -> Refused {
        match read_all(out) {
            Err(Fault::Refused(refused)) => refused,
            other => panic!("{other:?}"),
        }
    }

    /// each link or special file is refused by its own path in the result, never followed.
    #[cfg(unix)]
    #[test]
    fn links_and_special_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        fs::create_dir_all(out.join("sub")).unwrap();
        fs::write(out.join("sub").join("real.csv"), b"a").unwrap();
        std::os::unix::fs::symlink("real.csv", out.join("sub").join("latest")).unwrap();
        assert_eq!(refused(&out), Refused::Link("sub/latest".to_owned()));
        fs::remove_file(out.join("sub").join("latest")).unwrap();
        // sockets limit the path length, and rustix cannot make a fifo on macos
        let made = std::process::Command::new("mkfifo")
            .arg(out.join("queue"))
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(refused(&out), Refused::Special("queue".to_owned()));
    }

    /// a name some disk cannot store, or a clash, stops a result here, not on a teammate's machine.
    #[test]
    fn names_every_disk_can_store_and_tell_apart() {
        let file = |path: &str| (PathBuf::from(path), path.to_owned());
        assert_eq!(
            names(&[file("data/b.csv"), file("data/b:c.csv")]),
            Err(Refused::Name(
                "data/b:c.csv".to_owned(),
                "\"b:c.csv\" has ':', which windows does not allow in names".to_owned()
            ))
        );
        assert_eq!(
            names(&[file("Data.csv"), file("data.csv")]),
            Err(Refused::Clash("Data.csv".to_owned(), "data.csv".to_owned()))
        );
        assert_eq!(
            names(&[file("caf\u{e9}.csv"), file("cafe\u{301}.csv")]),
            Err(Refused::Clash(
                "caf\u{e9}.csv".to_owned(),
                "cafe\u{301}.csv".to_owned()
            ))
        );
        assert!(matches!(
            names(&[file("A"), file("a/b.csv")]),
            Err(Refused::Clash(..))
        ));
        assert!(matches!(
            names(&[file(".git/config")]),
            Err(Refused::Name(..))
        ));
        let decomposed = names(&[file("cafe\u{301}.csv")]).unwrap();
        assert_eq!(decomposed[0].1, "caf\u{e9}.csv", "data.lock records NFC");
    }

    /// the cache gets a copy, and the file's other name keeps its bytes and its write access.
    #[cfg(unix)]
    #[test]
    fn a_file_with_two_names_is_copied_not_moved() {
        use std::os::unix::fs::MetadataExt;
        let (dir, cache) = cache();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, b"my notes").unwrap();
        fs::hard_link(&notes, out.join("notes.txt")).unwrap();
        fs::write(out.join("alone.csv"), b"a,b\n").unwrap();
        let alone = fs::metadata(out.join("alone.csv")).unwrap().ino();
        let found = read_all(&out).unwrap();
        publish(
            &cache,
            &found,
            Threads::default(),
            &Arc::default(),
            &Work::default(),
        )
        .unwrap();
        let object = |bytes: &[u8]| cache.object_path(&crate::digest::sha256(bytes));
        assert!(!fs::metadata(&notes).unwrap().permissions().readonly());
        assert_ne!(
            fs::metadata(object(b"my notes")).unwrap().ino(),
            fs::metadata(&notes).unwrap().ino()
        );
        assert_eq!(
            fs::metadata(object(b"a,b\n")).unwrap().ino(),
            alone,
            "a file with one name moves"
        );
        assert!(
            fs::metadata(object(b"a,b\n"))
                .unwrap()
                .permissions()
                .readonly()
        );
    }

    /// changed while hashed or after, a file never enters the cache under a hash not its bytes'.
    #[test]
    fn a_file_changed_while_read_stops_capture() {
        let (dir, cache) = cache();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("a.csv"), b"a,b\n").unwrap();
        let grow = |path: &Path| {
            let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
            io::Write::write_all(&mut file, b"more\n").unwrap();
        };
        let during = read_with(
            &Out::open(&out).unwrap(),
            100,
            Threads::default(),
            &Stop::default(),
            &grow,
        );
        assert!(
            matches!(during, Err(Fault::Refused(Refused::Changed(ref path))) if path == "a.csv"),
            "{during:?}"
        );
        let found = read_all(&out).unwrap();
        grow(&out.join("a.csv"));
        let after = publish(
            &cache,
            &found,
            Threads::default(),
            &Arc::default(),
            &Work::default(),
        );
        assert!(
            matches!(after, Err(Fault::Refused(Refused::Changed(_)))),
            "{after:?}"
        );
        assert!(cache.find(&found.members[0].sha256, None).is_none());
    }

    /// after `chmod -R a-w` on the result, its files are still taken in and moved out.
    #[cfg(unix)]
    #[test]
    fn folders_the_step_made_read_only_are_taken_in() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, cache) = cache();
        let out = dir.path().join("out");
        fs::create_dir_all(out.join("sub")).unwrap();
        fs::write(out.join("sub").join("a.csv"), b"a,b\n").unwrap();
        for folder in [out.join("sub"), out.clone()] {
            fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let found = read_all(&out).unwrap();
        publish(
            &cache,
            &found,
            Threads::default(),
            &Arc::default(),
            &Work::default(),
        )
        .unwrap();
        assert!(cache.find(&found.members[0].sha256, Some(4)).is_some());
        assert!(!out.join("sub").join("a.csv").exists(), "it moved");
    }

    /// an empty folder, or one holding only folders, is no result.
    #[test]
    fn nothing_written_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("out").join("empty")).unwrap();
        assert_eq!(refused(&dir.path().join("out")), Refused::Nothing);
    }
}
