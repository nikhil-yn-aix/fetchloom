//! a mirror on a disk this machine reads, such as a lab nas, a usb drive or a network share. its
//! files are copied into the cache, cloned where the disk can, and never linked, so a later
//! change on the mirror cannot reach a project.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{LARGEST_TREE, MARKER, MARKER_LINE, Wanted, below};
use crate::cache::{Cache, Copied, Stored, read_unfollowed};
use crate::digest::Sha256;
use crate::stop::Stop;
use crate::threads::Threads;

/// copies from one folder mirror running at once, so a nas is not flooded.
const COPIES: usize = 8;

/// why a folder mirror cannot be used this run.
pub(crate) enum Missing {
    /// nothing is there, as when its disk is not mounted.
    Folder,
    /// the folder holds no marker, so it is not a mirror's top, or it is the empty folder a disk
    /// mounts on.
    Marker,
    Unreadable(io::Error),
}

/// what a folder mirror gave for one wanted file.
pub(crate) enum Gave {
    /// the pinned bytes, now in the cache.
    Stored(Stored),
    /// nothing at the file's place.
    Absent,
    /// a copy of another size or with other bytes, or a link or a special file in its place.
    Damaged,
    Unreadable(io::Error),
}

/// a tree a folder mirror gave.
pub(crate) enum Tree {
    Bytes(Vec<u8>),
    /// none there, or one too large to read, so the archive itself is needed.
    Absent,
    /// a link or a special file in its place.
    Damaged,
}

/// checks that a mirror is at `root`: a folder whose marker's first line is [`MARKER_LINE`].
pub(crate) fn probe(root: &Path) -> Result<(), Missing> {
    match fs::metadata(root) {
        Ok(found) if found.is_dir() => {}
        Ok(_) => return Err(Missing::Folder),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(Missing::Folder),
        Err(err) => return Err(Missing::Unreadable(err)),
    }
    let file = match read_unfollowed(&root.join(MARKER)) {
        Ok(file) => file,
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Err(Missing::Marker);
        }
        Err(err) => return Err(Missing::Unreadable(err)),
    };
    let mut first = String::new();
    // a marker that is not text is no marker
    let _ = file.take(256).read_to_string(&mut first);
    if first.lines().next().map(str::trim) == Some(MARKER_LINE) {
        Ok(())
    } else {
        Err(Missing::Marker)
    }
}

/// copies each of `wanted` the folder mirror at `root` holds into the cache, a few at once within
/// `threads`, and says what each gave, in order. past `MOST_FLUSHED_ALONE` files they reach the
/// disk with one flush, where the system can, before any takes its name.
///
/// # Errors
///
/// a failure to write the cache, and `Interrupted` when `stop` asks.
pub(crate) fn copy_all(
    cache: &Cache,
    root: &Path,
    wanted: &[Wanted],
    threads: Threads,
    stop: &Stop,
) -> io::Result<Vec<Gave>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let batch = if wanted.len() > crate::cache::MOST_FLUSHED_ALONE {
        cache.batch()?
    } else {
        None
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let flush = batch.is_none();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let flush = true;
    let copied = crate::place::run_all(wanted, threads.at_most(COPIES), |want| {
        cache.copy_in(
            &place(root, "objects", &want.digest),
            &want.digest,
            want.size,
            flush,
            stop,
        )
    });
    let mut gave = Vec::with_capacity(wanted.len());
    let mut sealed = Vec::new();
    for copied in copied {
        gave.push(match copied? {
            Copied::Sealed(object) => {
                sealed.push((gave.len(), object));
                Gave::Absent
            }
            Copied::Unread(err) if err.kind() == io::ErrorKind::NotFound => Gave::Absent,
            Copied::Unread(err) => Gave::Unreadable(err),
            Copied::OtherSize | Copied::OtherBytes | Copied::NotPlain => Gave::Damaged,
        });
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(batch) = batch {
        batch.flush()?;
    }
    for (index, object) in sealed {
        gave[index] = Gave::Stored(object.publish(cache)?);
    }
    Ok(gave)
}

/// the tree whose bytes hash to `tree`, as the folder mirror at `root` holds it, read a block at a
/// time and never past [`LARGEST_TREE`].
///
/// # Errors
///
/// a failure to read it, and `Interrupted` when `stop` asks.
pub(crate) fn read_tree(root: &Path, tree: &Sha256, stop: &Stop) -> io::Result<Tree> {
    let path = place(root, "trees", tree);
    let mut file = match read_unfollowed(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Tree::Absent),
        Err(err) if err.kind() == io::ErrorKind::InvalidInput => return Ok(Tree::Damaged),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    let mut block = vec![0; 1 << 20];
    loop {
        if stop.is_asked() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "stopped at your request",
            ));
        }
        let read = match file.read(&mut block) {
            Ok(0) => return Ok(Tree::Bytes(bytes)),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if (bytes.len() + read) as u64 > LARGEST_TREE {
            tracing::debug!(path = %path.display(), "a tree past 64 MiB, so the archive itself is needed");
            return Ok(Tree::Absent);
        }
        bytes.extend_from_slice(&block[..read]);
    }
}

/// where the folder mirror at `root` keeps `kind`, `objects` or `trees`, for `digest`.
fn place(root: &Path, kind: &str, digest: &Sha256) -> PathBuf {
    below(kind, digest)
        .iter()
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// makes a folder mirror at `root` holding `files`, with its marker.
    pub(crate) fn mirror(root: &Path, files: &[&[u8]]) {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join(MARKER), format!("{MARKER_LINE}\n")).unwrap();
        for bytes in files {
            let path = place(root, "objects", &crate::digest::sha256(bytes));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
    }

    fn wanted(bytes: &[u8]) -> Wanted {
        Wanted {
            digest: crate::digest::sha256(bytes),
            size: bytes.len() as u64,
            label: String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    #[test]
    fn a_folder_without_its_marker_is_no_mirror() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        assert!(matches!(probe(&root), Err(Missing::Folder)));
        fs::create_dir(&root).unwrap();
        assert!(matches!(probe(&root), Err(Missing::Marker)));
        fs::write(root.join(MARKER), "fetchloom mirror 2\n").unwrap();
        assert!(matches!(probe(&root), Err(Missing::Marker)));
        mirror(&root, &[]);
        assert!(probe(&root).is_ok());
    }

    #[test]
    fn each_wanted_file_says_what_the_mirror_gave() {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[b"kept"]);
        // the mirror's copy of "damaged" holds other bytes than the pin
        let damaged = place(&root, "objects", &crate::digest::sha256(b"damaged"));
        fs::create_dir_all(damaged.parent().unwrap()).unwrap();
        fs::write(damaged, b"DAMAGED").unwrap();
        let gave = copy_all(
            &cache,
            &root,
            &[wanted(b"kept"), wanted(b"absent"), wanted(b"damaged")],
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        assert!(
            matches!(&gave[0], Gave::Stored(stored) if stored.digest == crate::digest::sha256(b"kept"))
        );
        assert!(matches!(gave[1], Gave::Absent));
        assert!(matches!(gave[2], Gave::Damaged));
        assert!(
            cache
                .find(&crate::digest::sha256(b"kept"), Some(4))
                .is_some()
        );
        assert!(
            cache
                .find(&crate::digest::sha256(b"damaged"), None)
                .is_none()
        );
    }

    #[test]
    fn a_tree_is_read_whole_up_to_its_bound() {
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().join("mirror");
        mirror(&root, &[]);
        let tree = crate::digest::sha256(b"tree");
        let read = || read_tree(&root, &tree, &Stop::default()).unwrap();
        assert!(matches!(read(), Tree::Absent));
        let path = place(&root, "trees", &tree);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"a.csv\t1\tff\n").unwrap();
        assert!(matches!(read(), Tree::Bytes(bytes) if bytes == b"a.csv\t1\tff\n"));
        fs::File::create(&path)
            .unwrap()
            .set_len(LARGEST_TREE + 1)
            .unwrap();
        assert!(matches!(read(), Tree::Absent));
    }
}
