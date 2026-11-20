use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

const CONTENTS: &str = "station,temperature\nlerwick,9.1\n";

/// a temporary folder holding one cached object, read-only like every object in the cache
/// unless `read_only` is false.
fn cache(read_only: bool) -> (TempDir, PathBuf) {
    let folder = TempDir::new().unwrap();
    let object = folder.path().join("object");
    fs::write(&object, CONTENTS).unwrap();
    if read_only {
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&object, permissions).unwrap();
    }
    (folder, object)
}

/// where the project lives is the person's choice, and no setting gives links inside a folder a
/// sync client keeps, so the reason comes with no advice.
#[test]
fn synced_copy_advice_does_not_promise_zero_disk() {
    for (place, subject, expected) in [
        (
            "data",
            "files were",
            "data is inside onedrive, which breaks links, so files were copied",
        ),
        (
            "iris",
            "the file was",
            "iris is inside onedrive, which breaks links, so the file was copied",
        ),
    ] {
        assert_eq!(
            Unlinked::Synced("onedrive")
                .copied_because(place, subject)
                .to_string(),
            expected
        );
    }
}

#[test]
fn copy_advice_on_drives_without_links_names_the_place() {
    assert_eq!(
        Unlinked::NoLinks
            .copied_because("data", "files were")
            .to_string(),
        "the drive holding data cannot link files, so files were copied"
    );
    assert_eq!(
        Unlinked::NoLinks
            .copied_because("iris", "the file was")
            .to_string(),
        "the drive holding iris cannot link files, so the file was copied"
    );
}

/// the advice moves the cache to the files' drive and never the files: a project's code stays
/// where it is backed up, and its data folder may be a link to scratch.
#[test]
fn a_cache_on_another_drive_explains_the_copy_and_the_way_to_links() {
    assert_eq!(
        Unlinked::OtherDrive
            .copied_because("iris", "the file was")
            .to_string(),
        "the cache is on another drive than iris, so the file was copied. to link files instead, set FETCHLOOM_CACHE to a folder on the drive of iris. see https://nikhil-yn-aix.github.io/fetchloom/user/concepts/cache-and-placement/#links-need-one-drive"
    );
}

#[test]
fn a_hardlink_is_chosen_on_one_volume() {
    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
    let expected = if makes_hardlinks(folder.path()) {
        Method::Hardlink
    } else {
        Method::Copy
    };

    let method = placer.place(&object, &destination, false).unwrap();

    assert_eq!(method, expected);
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    if method == Method::Hardlink {
        assert!(is_read_only(&destination));
        assert!(is_read_only(&object));
    }
}

#[test]
fn starting_below_hardlinks_falls_back_to_a_copy() {
    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

    let method = placer.place(&object, &destination, false).unwrap();

    assert_eq!(method, Method::Copy);
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
}

#[test]
fn checking_missing_parents_does_not_create_them() {
    let folder = TempDir::new().unwrap();
    let parent = folder.path().join("missing");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

    let error = placer
        .check_folders(&parent.join("file"), false)
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!parent.exists());
}

#[test]
fn a_file_in_place_of_a_parent_folder_is_rejected() {
    let folder = TempDir::new().unwrap();
    let parent = folder.path().join("file");
    fs::write(&parent, b"keep these bytes").unwrap();
    for make in [false, true] {
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

        let error = placer
            .check_folders(&parent.join("child"), make)
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert_eq!(fs::read(&parent).unwrap(), b"keep these bytes");
    }
}

#[test]
fn missing_parent_folders_are_created() {
    let (folder, object) = cache(true);
    let destination = folder
        .path()
        .join("data")
        .join("weather")
        .join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    placer.place(&object, &destination, false).unwrap();

    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
}

#[test]
fn an_existing_destination_is_an_error_whatever_the_method() {
    for first in [Method::Reflink, Method::Hardlink, Method::Copy] {
        let (folder, object) = cache(true);
        let destination = folder.path().join("placed.csv");
        fs::write(&destination, "edited by hand").unwrap();
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), first, None);

        let error = placer.place(&object, &destination, false).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{first:?}");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "edited by hand",
            "{first:?}"
        );
    }
}

#[test]
fn a_copy_is_made_read_only() {
    let (folder, object) = cache(false);
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None);

    placer.place(&object, &destination, false).unwrap();

    assert!(is_read_only(&destination));
    // the copy is staged in a hidden file, and a placed file must not stay hidden
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 2;
        let attributes = fs::metadata(&destination).unwrap().file_attributes();
        assert_eq!(attributes & FILE_ATTRIBUTE_HIDDEN, 0);
    }
}

#[test]
fn a_writable_placement_is_a_writable_copy() {
    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    let method = placer.place(&object, &destination, true).unwrap();

    assert_eq!(method, Method::Copy);
    assert!(!is_read_only(&destination));
    assert!(is_read_only(&object));
}

/// a file copied within the root is a file of its own, so a write to the copy leaves the original
/// as it was, and a name already taken is refused without a change.
#[test]
fn a_copy_within_the_root_is_its_own_file_and_replaces_nothing() {
    let folder = TempDir::new().unwrap();
    let from = folder.path().join("iris.csv");
    fs::write(&from, CONTENTS).unwrap();
    let to = folder.path().join("iris.csv.changed");
    let placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

    let method = placer.copy_file(&from, &to, true).unwrap();

    assert_ne!(method, Method::Hardlink);
    assert!(!same_file::is_same_file(&from, &to).unwrap());
    fs::write(&to, "edited apart").unwrap();
    assert_eq!(fs::read_to_string(&from).unwrap(), CONTENTS);
    let error = placer.copy_file(&from, &to, true).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&to).unwrap(), "edited apart");
}

/// a clone keeps the read bits of the file it copies and the owner's write as asked, never a write
/// bit for others, on a disk that clones.
#[cfg(unix)]
#[test]
fn a_clone_takes_no_write_bit_for_others() {
    use std::os::unix::fs::PermissionsExt;
    let folder = TempDir::new().unwrap();
    let from = folder.path().join("iris.csv");
    fs::write(&from, CONTENTS).unwrap();
    // write for others, which a clone must not keep
    fs::set_permissions(&from, fs::Permissions::from_mode(0o666)).unwrap();
    let to = folder.path().join("iris.csv.changed");
    let placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

    if placer.copy_file(&from, &to, true).unwrap() != Method::Reflink {
        return crate::test_skips::skip("reflinks", "this disk makes no reflinks");
    }

    let mode = fs::metadata(&to).unwrap().permissions().mode();
    assert_eq!(format!("{:o}", mode & 0o777), "644");
}

/// links the file at `destination` to `object` in place, as `relink_all` does with one file,
/// saying what `holds` and `unchanged` say of it.
fn relink_one(
    placer: &Placer,
    object: &Path,
    destination: &Path,
    holds: Option<bool>,
    unchanged: impl Fn(&Metadata) -> bool + Sync,
) -> Relinked {
    let mut relinked = placer.relink_all(
        1,
        |_| Relink {
            object: object.to_path_buf(),
            destination: destination.to_path_buf(),
        },
        |_| holds,
        |_, now| unchanged(now),
    );
    relinked.pop().unwrap()
}

/// a copy of an object's bytes is replaced by a link to the object in one rename, as placing
/// links a new file, only once it is found to hold the bytes and while it still looks as it did.
/// a file that holds other bytes, could not be read or changed meanwhile stays, and no hidden link
/// is left beside it.
#[test]
fn a_copy_is_linked_only_while_it_holds_its_bytes_and_looks_as_it_did() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT link nothing, so a copy stays as it is there
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let destination = folder.path().join("found.csv");
    fs::write(&destination, CONTENTS).unwrap();
    let seen = fs::symlink_metadata(&destination).unwrap();
    let looks_as_seen =
        |now: &Metadata| now.len() == seen.len() && now.modified().ok() == seen.modified().ok();
    let placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

    for (holds, unchanged, stays) in [
        (Some(false), true, "other bytes"),
        (None, true, "unread"),
        (Some(true), false, "changed"),
    ] {
        let relinked = relink_one(&placer, &object, &destination, holds, |now| {
            unchanged && looks_as_seen(now)
        });
        let expected = if holds == Some(false) {
            matches!(relinked, Relinked::Other)
        } else {
            matches!(relinked, Relinked::Kept)
        };
        assert!(expected, "{stays}: {relinked:?}");
        assert!(!same_file::is_same_file(&object, &destination).unwrap());
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
        assert_eq!(placed_names(folder.path()), ["found.csv"], "{stays}");
    }

    let relinked = relink_one(&placer, &object, &destination, Some(true), looks_as_seen);

    let Relinked::Linked(method, metadata) = relinked else {
        panic!("not linked: {relinked:?}");
    };
    // a disk that clones gets a clone, as a new placement would
    assert_eq!(
        same_file::is_same_file(&object, &destination).unwrap(),
        method == Method::Hardlink
    );
    assert_ne!(method, Method::Copy);
    assert_eq!(metadata.len(), CONTENTS.len() as u64);
    assert!(is_read_only(&destination));
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert_eq!(placed_names(folder.path()), ["found.csv"]);
}

/// a sync client breaks hardlinks or uploads each as a whole file, so a copy in a synced folder is
/// never relinked as a hardlink, though it may be cloned where the disk clones, and the same
/// folder outside a client's gets a link.
#[test]
fn a_synced_folder_gets_no_relinked_hardlink() {
    let (folder, object) = cache(true);
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let destination = folder.path().join("found.csv");
    fs::write(&destination, CONTENTS).unwrap();
    let root = folder.path().to_path_buf();
    let synced = Placer::starting_at(root.clone(), Method::Reflink, Some("onedrive"));
    match relink_one(&synced, &object, &destination, Some(true), |_| true) {
        Relinked::Linked(method, _) => assert_eq!(method, Method::Reflink),
        Relinked::Refused => {
            assert!(!same_file::is_same_file(&object, &destination).unwrap());
        }
        other => panic!("{other:?}"),
    }
    let destination = folder.path().join("unsynced.csv");
    fs::write(&destination, CONTENTS).unwrap();
    let unsynced = Placer::starting_at(root, Method::Reflink, None);
    let relinked = relink_one(&unsynced, &object, &destination, Some(true), |_| true);
    assert!(matches!(relinked, Relinked::Linked(..)), "{relinked:?}");
}

/// the error a disk that makes no such link answers with: ENOTSUP, a code of its own on macos,
/// where hfs+ answers a clone with it, or windows's `ERROR_NOT_SUPPORTED`.
fn not_supported() -> io::Error {
    #[cfg(unix)]
    let code = rustix::io::Errno::NOTSUP.raw_os_error();
    #[cfg(windows)]
    let code = 50;
    io::Error::from_raw_os_error(code)
}

/// a link that fails leaves the copy as it is, unread. one the disk refuses for good, as linux's
/// `protected_hardlinks` refuses a link to another user's object, or a disk that makes no such
/// link, is said to be refused, so sync remembers it rather than reading the file at every run,
/// while one that fails for now, as on a full disk, is tried again by the next sync.
#[test]
fn a_failed_link_leaves_the_copy_unread() {
    for (failure, refusal, for_good) in [
        (
            "denied",
            (|| io::ErrorKind::PermissionDenied.into()) as fn() -> io::Error,
            true,
        ),
        ("not supported", not_supported, true),
        ("full disk", || io::ErrorKind::StorageFull.into(), false),
    ] {
        let (folder, object) = cache(true);
        let destination = folder.path().join("found.csv");
        fs::write(&destination, CONTENTS).unwrap();
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
        placer.link_refusal = Some(refusal);
        let read = std::sync::atomic::AtomicBool::new(false);

        let relinked = placer.relink_all(
            1,
            |_| Relink {
                object: object.clone(),
                destination: destination.clone(),
            },
            |_| {
                read.store(true, std::sync::atomic::Ordering::Relaxed);
                Some(true)
            },
            |_, _| true,
        );

        let expected = if for_good {
            matches!(relinked[..], [Relinked::Refused])
        } else {
            matches!(relinked[..], [Relinked::Kept])
        };
        assert!(expected, "{failure}: {relinked:?}");
        assert!(
            !read.load(std::sync::atomic::Ordering::Relaxed),
            "{failure}"
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
        assert_eq!(placed_names(folder.path()), ["found.csv"], "{failure}");
    }
}

/// a crash can empty a clone whose blocks are not on disk yet, so a clone takes the place of a
/// copy only once they are: in a batch of many it waits for the one flush, and when that fails
/// every copy stays as it was, while a few are flushed on their own.
#[cfg(target_os = "linux")]
#[test]
fn a_clone_takes_the_place_of_a_copy_only_once_flushed() {
    use crate::volume::MOST_FLUSHED_ALONE;
    use rustix::fs::{Mode, OFlags};

    let (folder, object) = cache(true);
    for files in [MOST_FLUSHED_ALONE, MOST_FLUSHED_ALONE + 1] {
        let root = folder.path().join(format!("data-{files}"));
        fs::create_dir(&root).unwrap();
        let mut placer = Placer::starting_at(root.clone(), Method::Reflink, None);
        // the test's disk may clone nothing, so its clone writes the bytes instead
        placer.clone_with = Some(|file, source| io::copy(&mut &*source, &mut &*file).map(drop));
        // syncfs rejects an O_PATH handle, so a batch that waits for one flush fails
        let handle = rustix::fs::open(folder.path(), OFlags::PATH | OFlags::CLOEXEC, Mode::empty());
        placer.batch.set(Some(File::from(handle.unwrap()))).unwrap();
        let destinations: Vec<PathBuf> = (0..files)
            .map(|index| root.join(format!("{index}.csv")))
            .collect();
        for destination in &destinations {
            fs::write(destination, CONTENTS).unwrap();
        }
        let copies: Vec<u64> = destinations
            .iter()
            .map(|path| {
                use std::os::unix::fs::MetadataExt;
                fs::metadata(path).unwrap().ino()
            })
            .collect();

        let relinked = placer.relink_all(
            files,
            |index| Relink {
                object: object.clone(),
                destination: destinations[index].clone(),
            },
            |_| Some(true),
            |_, _| true,
        );

        let inodes: Vec<u64> = destinations
            .iter()
            .map(|path| {
                use std::os::unix::fs::MetadataExt;
                fs::metadata(path).unwrap().ino()
            })
            .collect();
        if files > MOST_FLUSHED_ALONE {
            assert!(
                relinked
                    .iter()
                    .all(|result| matches!(result, Relinked::Kept)),
                "{relinked:?}"
            );
            assert_eq!(inodes, copies);
            assert_eq!(placed_names(&root).len(), files, "no hidden clone is left");
        } else {
            assert!(
                relinked
                    .iter()
                    .all(|result| matches!(result, Relinked::Linked(Method::Reflink, _))),
                "{relinked:?}"
            );
            assert!(inodes.iter().zip(&copies).all(|(now, was)| now != was));
        }
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        }
    }
}

#[test]
fn a_copy_goes_only_into_the_file_that_claimed_the_name() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT cannot hardlink, so nobody can swap in a second name of a file
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let destination = folder.path().join("placed.csv");
    let outside = folder.path().join("outside.csv");
    fs::write(&outside, "someone else's file").unwrap();
    let claimed = File::create_new(&destination).unwrap();
    // someone swaps a link to another file in for the name once it is claimed
    fs::rename(&destination, folder.path().join("claimed.csv")).unwrap();
    fs::hard_link(&outside, &destination).unwrap();

    fill(
        &object,
        &claimed,
        false,
        Threads::default(),
        &Stop::default(),
    )
    .unwrap();

    assert_eq!(fs::read_to_string(&outside).unwrap(), "someone else's file");
    assert!(!is_read_only(&outside));
    let copied = folder.path().join("claimed.csv");
    assert_eq!(fs::read_to_string(&copied).unwrap(), CONTENTS);
    assert!(is_read_only(&copied));
}

/// windows stamps a written file's time when its handle closes, coarser on fat and exfat than the
/// open handle shows, so a copy must look as its folder lists it even if the clock ticked between.
#[cfg(windows)]
#[test]
fn a_copy_looks_as_its_folder_lists_it() {
    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let (staged, file) = stage_copy(
        &object,
        &destination,
        false,
        Threads::default(),
        &Stop::default(),
    )
    .unwrap();
    staged.publish().unwrap();
    passed(folder.path(), file.metadata().unwrap().modified().unwrap());

    let look = closed_look(file, &Location::standalone(&destination).unwrap()).unwrap();

    let listed = fs::read_dir(folder.path())
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| entry.file_name() == "placed.csv")
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(
        (look.len(), look.modified().unwrap()),
        (listed.len(), listed.modified().unwrap())
    );
}

/// a folder a placement holds can be neither renamed nor removed by anyone until the placement
/// lets go, on every windows filesystem: the hold asks for an access the system counts in sharing,
/// where one that only reads attributes withholds delete sharing from no one.
#[cfg(windows)]
#[test]
fn a_held_folder_cannot_be_renamed_or_removed_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let outer = dir.path().join("a");
    let inner = outer.join("b");
    fs::create_dir_all(&inner).unwrap();

    let held = super::hold_below(dir.path(), &inner.join("file.csv")).unwrap();

    let moved = fs::rename(&inner, outer.join("c"));
    assert!(moved.is_err(), "the held folder was renamed");
    assert!(
        fs::rename(&outer, dir.path().join("z")).is_err(),
        "its held parent was renamed"
    );
    assert!(
        fs::remove_dir(&inner).is_err(),
        "the held folder was removed"
    );
    drop(held);
    fs::rename(&inner, outer.join("c")).unwrap();
}

/// a windows batch holds a folder's chain from its first file to its last, so no one renames or
/// removes the folder between them, and lets it go after the last; a batch bound to keep no
/// handle holds nothing past its file.
#[cfg(windows)]
#[test]
fn a_batch_holds_each_folder_from_its_first_file_to_its_last() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let folder = root.join("a").join("b");
    fs::create_dir_all(&folder).unwrap();
    let parents = Parents::default();
    let below = |name: &str| Path::new("a").join("b").join(name);
    let relatives = [Path::new("a/b"), Path::new("a/b"), Path::new("a/b")];
    let handles = FolderHandles::of(relatives.iter().copied());

    for name in ["1.csv", "2.csv"] {
        let located = parents
            .locate_kept(&handles, &root, &below(name), true)
            .unwrap();
        assert_eq!(
            located.path(),
            parents
                .locate(&root, &below(name), true, false)
                .unwrap()
                .path()
        );
        drop(located);
        assert!(
            fs::rename(&folder, root.join("a").join("c")).is_err(),
            "the folder was renamed after {name}, between the batch's files"
        );
        assert!(
            fs::rename(root.join("a"), root.join("z")).is_err(),
            "its parent was renamed after {name}"
        );
    }
    let last = parents
        .locate_kept(&handles, &root, &below("3.csv"), true)
        .unwrap();
    drop(last);
    fs::rename(&folder, root.join("a").join("c")).unwrap();

    fs::create_dir_all(&folder).unwrap();
    let none = FolderHandles::keeping_at_most(0, relatives.iter().copied());
    drop(
        parents
            .locate_kept(&none, &root, &below("1.csv"), true)
            .unwrap(),
    );
    fs::rename(&folder, root.join("a").join("d")).unwrap();
}

/// a windows batch holds the folder it places in from its first file there to its last: renaming
/// the folder fails as each file but the last is placed, and works once the last is.
#[cfg(windows)]
#[test]
fn a_folder_cannot_be_renamed_between_the_files_a_batch_places_in_it() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let inner = root.join("a").join("b");
    let destinations: Vec<PathBuf> = (0..3)
        .map(|index| inner.join(format!("{index}.csv")))
        .collect();
    let renamed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tried = Arc::clone(&renamed);
    let (from, to) = (inner.clone(), root.join("a").join("moved"));
    let one =
        Threads::read(&|name| (name == "FETCHLOOM_THREADS").then(|| std::ffi::OsString::from("1")))
            .unwrap();
    let mut placer = Placer::starting_at(root.clone(), Method::Hardlink, None)
        .with_threads(one)
        .observing(move |_| {
            let moved = fs::rename(&from, &to).is_ok();
            if moved {
                fs::rename(&to, &from).unwrap();
            }
            tried.lock().unwrap().push(moved);
        });
    let results = placer.place_all(
        destinations.len(),
        |index| Order {
            object: object.clone(),
            destination: destinations[index].clone(),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        |_| (),
    );
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(*renamed.lock().unwrap(), [false, false, true]);
    for destination in &destinations {
        assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
    }
}

/// a windows batch, holding each folder once, places what placing each file alone places, in a
/// few folders of many files and in many deep folders of two files each, past the bound on the
/// handles it keeps; and a folder a junction replaced before the batch is refused.
#[cfg(windows)]
#[test]
fn a_batch_holding_each_folder_once_places_alike() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let few: Vec<PathBuf> = (0..200)
        .map(|index| {
            root.join(format!("{}", index % 7))
                .join(format!("{index}.csv"))
        })
        .collect();
    // ntfs gives a file at most 1,024 names, so the object takes fewer links than that
    let many: Vec<PathBuf> = (0..1000)
        .map(|index| {
            root.join("a")
                .join("b")
                .join("c")
                .join(format!("d{}", index % 500))
                .join(format!("{index}.csv"))
        })
        .collect();
    for destinations in [few, many] {
        let mut alone = Placer::starting_at(root.clone(), Method::Hardlink, None);
        let one_by_one: Vec<Method> = destinations
            .iter()
            .map(|destination| alone.place(&object, destination, false).unwrap())
            .collect();
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
            remove_placed(destination);
        }
        let mut placer = Placer::starting_at(root.clone(), Method::Hardlink, None);
        let results = placer.place_all(
            destinations.len(),
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            |_| (),
        );
        let methods: Vec<Method> = results
            .into_iter()
            .map(|placed| placed.unwrap().0)
            .collect();
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
            remove_placed(destination);
        }
        assert_eq!(methods, one_by_one);
    }

    let elsewhere = folder.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    if crate::test_links::link_folder(&elsewhere, &root.join("linked")).is_err() {
        return crate::test_skips::skip("junctions", "this disk makes no junctions");
    }
    let mut placer = Placer::starting_at(root.clone(), Method::Hardlink, None);
    let results = placer.place_all(
        2,
        |index| Order {
            object: object.clone(),
            destination: root.join("linked").join(format!("{index}.csv")),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        |_| (),
    );
    assert!(results.iter().all(Result::is_err));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// removes a file a test placed. windows removes a read-only file where std can ask it to ignore
/// the flag, as on ntfs; fat32 and exfat refuse, and there a placed file is a copy of its own,
/// whose flag goes first. a hardlink shares its flag with the cache's object, so it keeps it.
#[cfg(windows)]
fn remove_placed(path: &Path) {
    match fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "fat32 and exfat remove no read-only file"
            )]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).unwrap();
            fs::remove_file(path).unwrap();
        }
        removed => removed.unwrap(),
    }
}

/// a windows batch keeps chains holding no more handles than its bound. in the order a batch
/// places files, the first of every folder before any second, a batch of many deep folders of two
/// files each keeps chains for as many folders as the bound allows, holds the rest for each file
/// alone, locates every file where the plain way does, and keeps none once every folder's last
/// file is located.
#[cfg(windows)]
#[test]
fn a_batch_holding_each_folder_once_keeps_no_more_handles_than_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let folders: Vec<PathBuf> = (0..300)
        .map(|index| Path::new("a").join("b").join("c").join(format!("d{index}")))
        .collect();
    for folder in &folders {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    let parents = Parents::default();
    let relatives: Vec<&Path> = folders
        .iter()
        .flat_map(|folder| [folder.as_path(), folder.as_path()])
        .collect();
    // ten chains of four folders each
    let handles = FolderHandles::keeping_at_most(40, relatives.iter().copied());
    for name in ["1.csv", "2.csv"] {
        for folder in &folders {
            let below = folder.join(name);
            let located = parents.locate_kept(&handles, &root, &below, true).unwrap();
            assert_eq!(
                located.path(),
                parents.locate(&root, &below, true, false).unwrap().path()
            );
            assert!(handles.holding().0 <= 40, "{:?}", handles.holding());
        }
    }
    assert_eq!(handles.holding(), (0, 40));
}

/// a windows batch stopped after its first file lets every folder it holds go as it ends, though
/// the rest of their files were never placed.
#[cfg(windows)]
#[test]
fn a_stopped_batch_holding_each_folder_once_lets_its_folders_go() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let destinations: Vec<PathBuf> = (0..40)
        .map(|index| {
            root.join(format!("{}", index % 4))
                .join(format!("{index}.csv"))
        })
        .collect();
    let stop = Arc::new(Stop::default());
    let mut placer =
        Placer::starting_at(root.clone(), Method::Hardlink, None).with_stop(Arc::clone(&stop));
    let results = placer.place_all(
        destinations.len(),
        |index| Order {
            object: object.clone(),
            destination: destinations[index].clone(),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        |_| stop.ask(),
    );
    assert!(results[0].is_ok());
    assert!(results[1..].iter().all(Result::is_err));
    for index in 0..4 {
        fs::rename(
            root.join(index.to_string()),
            root.join(format!("moved {index}")),
        )
        .unwrap();
    }
}

/// waits until a file written in `folder` gets a later time than `time`: a coarse clock, such
/// as fat's two seconds, moves on only once a tick passes.
#[cfg(windows)]
fn passed(folder: &std::path::Path, time: std::time::SystemTime) {
    let probe = folder.join("clock");
    for _ in 0..1000 {
        // a write that changes the length, which exfat needs before it stamps a time
        fs::write(&probe, b"tick").unwrap();
        if fs::metadata(&probe).unwrap().modified().unwrap() > time {
            return fs::remove_file(&probe).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the disk's clock never passed {time:?}");
}

#[cfg(windows)]
#[test]
fn a_writable_copy_needs_no_permission_change() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_DATA: u32 = 2;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;

    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(FILE_WRITE_DATA | FILE_READ_ATTRIBUTES)
        .open(&destination)
        .unwrap();

    let result = fill(&object, &output, true, Threads::default(), &Stop::default());

    assert!(
        result.is_ok(),
        "unchanged permissions need no write access to attributes: {result:?}"
    );
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert!(!is_read_only(&destination));
    let readonly = undo_on_error(
        fill(
            &object,
            &output,
            false,
            Threads::default(),
            &Stop::default(),
        ),
        &destination,
    );
    assert!(
        readonly.is_err(),
        "a real permission change must still fail"
    );
    drop(output);
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn a_writable_copy_works_where_the_mount_refuses_chmod() {
    let Some(root) = std::env::var_os("FETCHLOOM_TEST_NOOP_CHMOD") else {
        return;
    };
    let destination_folder = TempDir::new_in(root).unwrap();
    let (_source_folder, object) = cache(true);
    let probe = destination_folder.path().join("probe");
    fs::write(&probe, b"writable").unwrap();
    let permissions = fs::metadata(&probe).unwrap().permissions();
    assert_eq!(
        fs::set_permissions(&probe, permissions).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    fs::remove_file(&probe).unwrap();
    let destination = destination_folder.path().join("placed.csv");

    copy(
        &object,
        &destination,
        true,
        Threads::default(),
        true,
        &Stop::default(),
    )
    .unwrap();

    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert!(!is_read_only(&destination));
    assert!(is_read_only(&object));
    let readonly = destination_folder.path().join("readonly.csv");
    assert!(
        copy(
            &object,
            &readonly,
            false,
            Threads::default(),
            true,
            &Stop::default()
        )
        .is_err()
    );
    assert!(!readonly.exists());
}

#[test]
fn an_interrupted_copy_never_leaves_partial_bytes_at_its_name() {
    use std::io::BufRead as _;
    use std::process::{Command, Stdio};

    const CHILD: &str = "FETCHLOOM_TEST_COPY_CHILD";
    const READY: &str = "fetchloom-copy-staged";
    if let Some(root) = std::env::var_os(CHILD) {
        let destination = PathBuf::from(root).join("placed.bin");
        let location = Arc::new(Location::standalone(&destination).unwrap());
        let (_staged, mut file) = location.create(0o600, false).unwrap();
        file.write_all(b"partial bytes").unwrap();
        println!("{READY}");
        std::io::stdout().flush().unwrap();
        let _ = std::io::stdin().read(&mut [0]);
        return;
    }
    let folder = TempDir::new().unwrap();
    let destination = folder.path().join("placed.bin");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "place::tests::an_interrupted_copy_never_leaves_partial_bytes_at_its_name",
            "--nocapture",
        ])
        .env(CHILD, folder.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let staged = std::io::BufReader::new(output)
        .lines()
        .any(|line| line.is_ok_and(|line| line.contains(READY)));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    assert!(
        staged,
        "the child did not stage a copy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!destination.exists());
    let hidden: Vec<_> = fs::read_dir(folder.path())
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(hidden.len(), 1);
    assert!(is_copy_temporary(&hidden[0].file_name().to_string_lossy()));
    assert_eq!(fs::read(hidden[0].path()).unwrap(), b"partial bytes");
}

#[cfg(windows)]
#[test]
fn a_copy_can_be_published_beyond_the_windows_path_limit() {
    let (folder, object) = cache(true);
    let mut parent = folder.path().to_path_buf();
    while parent.as_os_str().len() < 300 {
        parent.push("a_long_folder_for_a_copy");
    }
    fs::create_dir_all(&parent).unwrap();
    let destination = parent.join("placed.csv");

    copy(
        &object,
        &destination,
        false,
        Threads::default(),
        true,
        &Stop::default(),
    )
    .unwrap();

    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert!(is_read_only(&destination));
    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_copy_never_reads_an_object_swapped_for_a_symlink() {
    let (folder, object) = cache(true);
    let outside = folder.path().join("outside");
    fs::write(&outside, CONTENTS).unwrap();
    fs::rename(&object, folder.path().join("original")).unwrap();
    std::os::unix::fs::symlink(&outside, &object).unwrap();
    let destination = folder.path().join("placed.csv");
    let capped = Threads::read(&|_| Some("1".into())).unwrap();

    for threads in [Threads::default(), capped] {
        for writable in [false, true] {
            let error = copy(
                &object,
                &destination,
                writable,
                threads,
                true,
                &Stop::default(),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains(&object.display().to_string()));
            assert!(!destination.exists());
            assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 3);
        }
    }
    assert_eq!(fs::read_to_string(&outside).unwrap(), CONTENTS);
    assert!(!is_read_only(&outside));
}

#[cfg(unix)]
#[test]
fn placement_refuses_an_object_swapped_for_a_symlink() {
    for first in [Method::Reflink, Method::Hardlink, Method::Copy] {
        let (folder, object) = cache(true);
        let outside = folder.path().join("outside");
        fs::write(&outside, CONTENTS).unwrap();
        fs::rename(&object, folder.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &object).unwrap();
        let root = folder.path().join("data");
        let destination = root.join("placed.csv");
        let mut placer = Placer::starting_at(root, first, None);

        let error = placer.place(&object, &destination, false).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains(&object.display().to_string()));
        assert!(fs::symlink_metadata(&destination).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), CONTENTS);
        assert!(!is_read_only(&outside));
    }
}

#[test]
fn a_failed_copy_leaves_no_staging_file() {
    let folder = TempDir::new().unwrap();
    let missing = folder.path().join("missing");
    let destination = folder.path().join("placed.csv");

    assert!(
        copy(
            &missing,
            &destination,
            false,
            Threads::default(),
            true,
            &Stop::default()
        )
        .is_err()
    );

    assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
}

#[test]
fn a_file_of_many_blocks_is_copied_byte_for_byte() {
    let folder = TempDir::new().unwrap();
    let object = folder.path().join("object");
    let bytes: Vec<u8> = (0..(3 << 20) + 123)
        .map(|at| u8::try_from(at % 251).unwrap())
        .collect();
    fs::write(&object, &bytes).unwrap();
    let destination = folder.path().join("placed.bin");

    copy(
        &object,
        &destination,
        false,
        Threads::default(),
        true,
        &Stop::default(),
    )
    .unwrap();

    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert!(is_read_only(&destination));
}

#[test]
fn a_synced_folder_gets_a_copy_instead_of_a_hardlink() {
    let (folder, object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(
        folder.path().to_path_buf(),
        Method::Hardlink,
        Some("onedrive"),
    );

    let method = placer.place(&object, &destination, false).unwrap();

    assert_eq!(method, Method::Copy);
}

/// the names in `folder` other than the cached object's.
fn placed_names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        // an nfs client gives a file removed while its close is still on the wire a `.nfs`
        // name of its own, and removes it once the close lands
        .filter(|name| name != "object" && !name.starts_with(".nfs"))
        .collect();
    names.sort();
    names
}

#[test]
fn a_clone_is_made_under_a_hidden_name_and_named_only_once_complete() {
    use std::io::Write as _;
    let (folder, _object) = cache(true);
    let destination = folder.path().join("placed.csv");
    let mut named_while_filled = None;

    let published = publish_new(
        &Arc::new(Location::standalone(&destination).unwrap()),
        0o666,
        |mut file| {
            named_while_filled = Some(fs::symlink_metadata(&destination).is_ok());
            file.write_all(CONTENTS.as_bytes())
        },
    )
    .unwrap();

    assert_eq!(published.len(), CONTENTS.len() as u64);
    assert_eq!(
        named_while_filled,
        Some(false),
        "a cut-off clone would stay"
    );
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert_eq!(placed_names(folder.path()), ["placed.csv"]);
}

#[test]
fn a_clone_never_replaces_a_file_that_took_its_name() {
    let (folder, _object) = cache(true);
    let destination = folder.path().join("placed.csv");

    let error = publish_new(
        &Arc::new(Location::standalone(&destination).unwrap()),
        0o666,
        |_| fs::write(&destination, "edited by hand"),
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&destination).unwrap(), "edited by hand");
    assert_eq!(placed_names(folder.path()), ["placed.csv"]);
}

#[test]
fn a_reflink_the_volume_refuses_leaves_no_hidden_file() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::starting_at(root.clone(), Method::Reflink, None);

    placer
        .place(&object, &root.join("placed.csv"), true)
        .unwrap();

    assert_eq!(placed_names(&root), ["placed.csv"]);
    assert_eq!(
        fs::read_to_string(root.join("placed.csv")).unwrap(),
        CONTENTS
    );
    assert!(!is_read_only(&root.join("placed.csv")));
}

/// three copies of the cached object staged in `folder`, writable and not yet named.
fn staged_copies(folder: &Path, object: &Path) -> (Vec<PathBuf>, Vec<(usize, Method, Staged)>) {
    let destinations: Vec<PathBuf> = (0..3)
        .map(|index| folder.join(format!("{index}.csv")))
        .collect();
    let staged = destinations
        .iter()
        .enumerate()
        .map(|(index, destination)| {
            let (staged, _) = stage_copy(
                object,
                destination,
                true,
                Threads::default(),
                &Stop::default(),
            )
            .unwrap();
            (index, Method::Copy, staged)
        })
        .collect();
    (destinations, staged)
}

fn publication_work(destinations: &[PathBuf]) -> schedule::Work {
    let mut folders = schedule::Folders::new(destinations.len());
    for (index, destination) in destinations.iter().enumerate() {
        folders.record(index, destination);
    }
    folders.finish(false)
}

#[test]
fn batched_copies_are_named_only_after_the_flush() {
    let (folder, object) = cache(true);
    let (destinations, staged) = staged_copies(folder.path(), &object);
    let mut named_before_flush = None;

    let results = publish_flushed(
        staged,
        || {
            named_before_flush = Some(destinations.iter().filter(|path| path.exists()).count());
            Ok(())
        },
        Threads::default(),
        &|metadata: &Metadata| metadata.len(),
        &Stop::default(),
        &publication_work(&destinations),
    );

    assert_eq!(
        named_before_flush,
        Some(0),
        "a crash could empty named copies"
    );
    assert_eq!(results.len(), 3);
    for (index, result) in results {
        let (method, size) = result.unwrap();
        assert_eq!((method, size), (Method::Copy, CONTENTS.len() as u64));
        assert_eq!(fs::read_to_string(&destinations[index]).unwrap(), CONTENTS);
    }
    assert_eq!(placed_names(folder.path()), ["0.csv", "1.csv", "2.csv"]);
}

/// a placement of a few files flushes each copy on its own, so it never waits on a flush of the
/// whole filesystem; only a larger one flushes the filesystem once before naming any copy.
#[cfg(target_os = "linux")]
#[test]
fn only_a_placement_of_many_copies_waits_for_one_flush() {
    use crate::volume::MOST_FLUSHED_ALONE;
    use rustix::fs::{Mode, OFlags};

    let (folder, object) = cache(true);
    for files in [MOST_FLUSHED_ALONE, MOST_FLUSHED_ALONE + 1] {
        let root = folder.path().join(format!("data-{files}"));
        let mut placer = Placer::starting_at(root.clone(), Method::Copy, None);
        // syncfs rejects an O_PATH handle, so a placement that waits for one flush fails
        let handle = rustix::fs::open(folder.path(), OFlags::PATH | OFlags::CLOEXEC, Mode::empty());
        placer.batch.set(Some(File::from(handle.unwrap()))).unwrap();
        let destinations: Vec<PathBuf> = (0..files)
            .map(|index| root.join(format!("{index}.csv")))
            .collect();

        let results = placer.place_all(
            files,
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            Metadata::len,
        );

        let flushed_alone = results.iter().filter(|result| result.is_ok()).count();
        if files > MOST_FLUSHED_ALONE {
            assert_eq!(flushed_alone, 0);
            let failed = results[0].as_ref().unwrap_err();
            assert!(failed.to_string().contains("could not flush"), "{failed}");
        } else {
            assert_eq!(flushed_alone, files, "{results:?}");
        }
    }
}

/// a crash can empty a clone whose blocks are not on disk yet, so a clone takes its name only once
/// they are, as a copy does: in a placement of many files it waits for the one flush, which names
/// none when it fails, and in a placement of a few it is flushed on its own.
#[cfg(target_os = "linux")]
#[test]
fn a_clone_waits_for_the_flush_as_a_copy_does() {
    use crate::volume::MOST_FLUSHED_ALONE;
    use rustix::fs::{Mode, OFlags};

    let (folder, object) = cache(true);
    for files in [MOST_FLUSHED_ALONE, MOST_FLUSHED_ALONE + 1] {
        let root = folder.path().join(format!("data-{files}"));
        let mut placer = Placer::starting_at(root.clone(), Method::Reflink, None);
        // the test's disk may clone nothing, so its clone writes the bytes instead
        placer.clone_with = Some(|file, source| io::copy(&mut &*source, &mut &*file).map(drop));
        // syncfs rejects an O_PATH handle, so a placement that waits for one flush fails
        let handle = rustix::fs::open(folder.path(), OFlags::PATH | OFlags::CLOEXEC, Mode::empty());
        placer.batch.set(Some(File::from(handle.unwrap()))).unwrap();
        let destinations: Vec<PathBuf> = (0..files)
            .map(|index| root.join(format!("{index}.csv")))
            .collect();

        let results = placer.place_all(
            files,
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            Metadata::len,
        );

        if files > MOST_FLUSHED_ALONE {
            for result in &results {
                let failed = result.as_ref().unwrap_err();
                assert!(failed.to_string().contains("could not flush"), "{failed}");
            }
            assert!(destinations.iter().all(|path| !path.exists()));
        } else {
            for result in &results {
                assert_eq!(result.as_ref().unwrap().0, Method::Reflink, "{results:?}");
            }
            assert_eq!(fs::read_to_string(&destinations[0]).unwrap(), CONTENTS);
        }
    }
}

#[test]
fn a_failed_flush_names_no_batched_copy() {
    let (folder, object) = cache(true);
    let (destinations, staged) = staged_copies(folder.path(), &object);

    let results = publish_flushed(
        staged,
        || Err(io::Error::other("write-back failed")),
        Threads::default(),
        &|_: &Metadata| (),
        &Stop::default(),
        &publication_work(&destinations),
    );

    assert_eq!(results.len(), 3);
    for (_, result) in results {
        let error = result.unwrap_err();
        assert!(error.to_string().contains("write-back failed"), "{error}");
    }
    assert!(destinations.iter().all(|path| !path.exists()));
    assert_eq!(placed_names(folder.path()), Vec::<String>::new());
}

#[test]
fn the_whole_ladder_never_copies_within_one_volume() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT link nothing, so there every rung ends in a copy
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

    for name in ["first.csv", "second.csv"] {
        let destination = folder.path().join(name);
        let method = placer.place(&object, &destination, false).unwrap();
        assert_ne!(method, Method::Copy, "{name}");
        assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    }
}

#[test]
fn a_method_the_filesystem_refuses_is_not_tried_again() {
    let refusals = [
        io::Error::from(io::ErrorKind::Unsupported),
        io::Error::from(io::ErrorKind::CrossesDevices),
        // how FAT32 and exFAT refuse a hardlink: ERROR_INVALID_FUNCTION on windows, EPERM on
        // linux
        io::Error::from_raw_os_error(1),
        // how NTFS refuses a reflink: "incorrect function", wrapped in an HRESULT
        #[cfg(windows)]
        io::Error::from_raw_os_error(0x8007_0001_u32.cast_signed()),
    ];
    for refusal in refusals {
        let (folder, object) = cache(true);
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
        // a volume whose own probe agrees that it links nothing
        placer.links = OnceLock::from(false);
        let described = refusal.to_string();

        placer
            .learn(Method::Hardlink, refusal, folder.path())
            .unwrap();
        let method = placer
            .place(&object, &folder.path().join("placed.csv"), false)
            .unwrap();

        assert_eq!(method, Method::Copy, "{described}");
    }
}

#[test]
fn a_link_refused_for_one_file_only_is_tried_for_the_next() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT link nothing, so there no refusal is for one file only
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    // code 1 is EPERM on linux, which protected_hardlinks answers for another user's object
    // on a volume that links
    placer
        .learn(
            Method::Hardlink,
            io::Error::from_raw_os_error(1),
            folder.path(),
        )
        .unwrap();
    let method = placer
        .place(&object, &folder.path().join("placed.csv"), false)
        .unwrap();

    assert_eq!(method, Method::Hardlink);
    assert_eq!(placer.unlinked(), None);
}

/// no link may leave a read-only mount, even one of the project's own drive, so a read-only
/// cache is no reason to move the project.
#[test]
fn a_read_only_cache_is_told_apart_from_another_drive() {
    let folder = TempDir::new().unwrap();
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    for (read_only, why) in [
        (false, Unlinked::OtherDrive),
        (true, Unlinked::ReadOnlyCache),
    ] {
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
        let pretend: fn(&Path) -> bool = if read_only { |_| true } else { |_| false };
        placer.read_only = pretend;
        placer
            .learn(
                Method::Hardlink,
                io::ErrorKind::CrossesDevices.into(),
                folder.path(),
            )
            .unwrap();
        assert_eq!(placer.unlinked(), Some(why));
    }
    assert_eq!(
        Unlinked::ReadOnlyCache
            .copied_because("the project", "files were")
            .to_string(),
        "the cache is read-only, so files were copied"
    );
}

/// a hardlink past ext4's 65,000 names fails with EMLINK, and past windows' 1,023 with
/// `ERROR_TOO_MANY_LINKS`, so that file is copied and the next one linked again.
#[test]
fn too_many_links_copies_that_file_alone() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT make no hardlinks to fall back from
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
    placer.link_refusal = Some(|| io::ErrorKind::TooManyLinks.into());

    let copied = folder.path().join("copied.csv");
    let method = placer.place(&object, &copied, false).unwrap();

    assert_eq!(method, Method::Copy);
    assert_eq!(fs::read_to_string(&copied).unwrap(), CONTENTS);
    assert!(!same_file::is_same_file(&copied, &object).unwrap());
    assert!(is_read_only(&copied));
    placer.link_refusal = None;
    let linked = folder.path().join("linked.csv");
    assert_eq!(
        placer.place(&object, &linked, false).unwrap(),
        Method::Hardlink
    );
    assert_eq!(placer.unlinked(), None);
}

#[test]
fn a_drive_that_refuses_every_link_is_told_from_a_cache_elsewhere() {
    let folder = TempDir::new().unwrap();
    let placer = |synced| Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, synced);
    let refused = || io::Error::from(io::ErrorKind::Unsupported);

    let linking = placer(None);
    assert_eq!(linking.unlinked(), None);
    let mut no_links = placer(None);
    no_links.links = OnceLock::from(false);
    no_links
        .learn(Method::Reflink, refused(), folder.path())
        .unwrap();
    no_links
        .learn(Method::Hardlink, refused(), folder.path())
        .unwrap();
    assert_eq!(no_links.unlinked(), Some(Unlinked::NoLinks));
    let elsewhere = placer(None);
    elsewhere
        .learn(Method::Reflink, refused(), folder.path())
        .unwrap();
    elsewhere
        .learn(
            Method::Hardlink,
            io::ErrorKind::CrossesDevices.into(),
            folder.path(),
        )
        .unwrap();
    // a cache on this drive helps only where this drive makes links, which FAT does not
    let blamed = if makes_hardlinks(folder.path()) {
        Unlinked::OtherDrive
    } else {
        Unlinked::NoLinks
    };
    assert_eq!(elsewhere.unlinked(), Some(blamed));
    let synced = placer(Some("onedrive"));
    synced
        .learn(Method::Reflink, refused(), folder.path())
        .unwrap();
    assert_eq!(synced.unlinked(), Some(Unlinked::Synced("onedrive")));
}

#[test]
fn a_cache_elsewhere_is_not_blamed_for_a_drive_that_links_nothing() {
    let folder = TempDir::new().unwrap();
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);
    placer.links = OnceLock::from(false);

    placer
        .learn(
            Method::Reflink,
            io::ErrorKind::CrossesDevices.into(),
            folder.path(),
        )
        .unwrap();
    placer
        .learn(
            Method::Hardlink,
            io::ErrorKind::CrossesDevices.into(),
            folder.path(),
        )
        .unwrap();

    assert_eq!(placer.unlinked(), Some(Unlinked::NoLinks));
}

/// a link the disk cannot make is not asked for again after the first file: hfs+ answers a clone,
/// and exfat a clone and a hardlink, with macos's own ENOTSUP, so placing there goes straight to
/// what the disk makes, and says so where it links nothing.
#[test]
fn a_link_the_disk_cannot_make_is_not_asked_for_again() {
    let (folder, object) = cache(true);
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);

    let method = placer
        .place(&object, &folder.path().join("first.csv"), false)
        .unwrap();

    // windows tries a hardlink first, and a clone only when that fails
    let cloning_tried = !(cfg!(windows) && method == Method::Hardlink);
    if cloning_tried && method != Method::Reflink {
        assert!(!placer.can_reflink.load(Ordering::Relaxed), "{method:?}");
    }
    let hardlinks = makes_hardlinks(folder.path());
    assert_eq!(placer.can_hardlink(), Some(hardlinks), "{method:?}");
    if method == Method::Copy {
        assert!(!placer.can_hardlink.load(Ordering::Relaxed));
        assert_eq!(placer.unlinked(), Some(Unlinked::NoLinks));
    }
}

/// whether the filesystem holding `folder` makes hardlinks, as NTFS and ext4 do and FAT
/// and exFAT do not.
pub(super) fn makes_hardlinks(folder: &Path) -> bool {
    let original = folder.join("probe");
    let link = folder.join("probe-link");
    fs::write(&original, CONTENTS).unwrap();
    let made = fs::hard_link(&original, &link).is_ok();
    for probe in [link, original] {
        let _ = fs::remove_file(probe);
    }
    made
}

#[test]
fn many_files_are_placed_on_several_threads_and_reported_in_order() {
    let (folder, object) = cache(true);
    let expected = if makes_hardlinks(folder.path()) {
        Method::Hardlink
    } else {
        Method::Copy
    };
    let root = folder.path().join("data");
    let mut placer = Placer::starting_at(root.clone(), Method::Hardlink, None);
    let mut destinations: Vec<PathBuf> = (0..300)
        .map(|index| {
            root.join(format!("{}", index % 7))
                .join(format!("{index}.csv"))
        })
        .collect();
    // one order that cannot succeed, to see its error land in its own place
    let taken = root.join("taken.csv");
    fs::create_dir_all(&root).unwrap();
    fs::write(&taken, "mine").unwrap();
    destinations.insert(150, taken.clone());

    let results = placer.place_all(
        destinations.len(),
        |index| Order {
            object: object.clone(),
            destination: destinations[index].clone(),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        |_| (),
    );

    assert_eq!(results.len(), 301);
    for (index, result) in results.iter().enumerate() {
        match index {
            150 => assert_eq!(
                result.as_ref().unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            ),
            _ => assert_eq!(result.as_ref().unwrap().0, expected, "{index}"),
        }
    }
    for (index, destination) in destinations.iter().enumerate() {
        let expected = if index == 150 { "mine" } else { CONTENTS };
        assert_eq!(fs::read_to_string(destination).unwrap(), expected);
    }
    assert_eq!(fs::read_to_string(&taken).unwrap(), "mine");
}

#[test]
fn many_copies_are_placed_and_named_whole() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::starting_at(root.clone(), Method::Copy, None);
    let destinations: Vec<PathBuf> = (0..300)
        .map(|index| {
            root.join(format!("{}", index % 7))
                .join(format!("{index}.csv"))
        })
        .collect();

    let results = placer.place_all(
        destinations.len(),
        |index| Order {
            object: object.clone(),
            destination: destinations[index].clone(),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        Metadata::len,
    );

    assert_eq!(results.len(), 300);
    for (result, destination) in results.into_iter().zip(&destinations) {
        assert_eq!(result.unwrap(), (Method::Copy, CONTENTS.len() as u64));
        assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        assert!(is_read_only(destination));
    }
    for folder in 0..7 {
        let names = placed_names(&root.join(format!("{folder}")));
        assert!(
            names.iter().all(|name| !is_copy_temporary(name)),
            "{names:?}"
        );
    }
    placer.flush_copies().unwrap();
}

#[test]
fn neighbouring_orders_go_to_different_folders() {
    let paths = ["a/1", "a/2", "a/3", "b/1", "c/1", "c/2"];
    let mut folders = schedule::Folders::new(paths.len());
    let orders: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| (index, folders.record(index, Path::new(path)).0))
        .collect();
    assert_eq!(spread(orders), [0, 3, 4, 1, 5, 2]);
}

#[test]
fn work_spread_over_threads_comes_back_in_order() {
    let items: Vec<u64> = (0..1000).collect();
    let doubled = run_all(&items, Threads::default(), |item| item * 2);
    assert_eq!(
        doubled,
        items.iter().map(|item| item * 2).collect::<Vec<_>>()
    );
    assert_eq!(
        run_all(&Vec::<u64>::new(), Threads::default(), |item| *item),
        Vec::<u64>::new()
    );
    assert_eq!(
        run_owned(items, Threads::default(), |item| item * 2),
        doubled
    );
    assert_eq!(
        run_owned(Vec::<u64>::new(), Threads::default(), |item| item),
        Vec::<u64>::new()
    );
}

#[test]
fn hashing_workers_use_bytes_without_spending_more_at_small_caps() {
    for (items, bytes, processors, ceiling, expected) in [
        (8, 512 << 20, 16, None, 8),
        (8, 32 << 20, 16, None, 8),
        (8, 8 << 20, 16, None, 2),
        (8, 32 << 10, 16, None, 1),
        (1, 512 << 20, 16, None, 1),
        (0, 0, 16, None, 0),
        (8, 512 << 20, 1, None, 1),
        (8, 512 << 20, 2, None, 2),
        (8, 512 << 20, 16, Some(2), 1),
        (8, 512 << 20, 16, Some(4), 4),
        (512, 2 << 20, 16, Some(2), 2),
        (8, u64::MAX, 16, None, 8),
    ] {
        let cap = ceiling.map_or(Threads::default(), |cap| Threads::default().at_most(cap));
        assert_eq!(
            hash_workers(items, bytes, processors, cap),
            expected,
            "{items} files, {bytes} bytes, {processors} CPUs, {ceiling:?}"
        );
    }
}

#[test]
fn hashing_jobs_keep_digests_and_interruptions_in_order_when_spawns_fail() {
    struct Interrupted;
    impl Read for Interrupted {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Interrupted.into())
        }
    }
    let expected = [
        "299285fc41a44cdb038b9fdaf494c76ca9d0c866672b2b266c1a0c17dda60a05",
        "61d678b48de600e6922df82ac9fb5d208d19e98064d0d1d5c14a2ee50481c593",
        "a7e2d83a174914a1232152ca3df32818f5513532e4acd4b4efd87df038907f6a",
        "0020e43347537f3440ee8627f986e6ad02edec405d62d679ee53ecc75c5e2911",
        "0d991aba52ac92e65839edfe37cb9ba77003644998a0b571b1f1a3a21dabc684",
        "efbe555ee4f0de9ad7ba0dfad1f7eef00200a93fabd66fa09c46e4dbef135919",
        "7d14a820ca69354c4f496a91d87a593674beaaf5641ffbe718225bbd33d9f75d",
        "806582984e335c4f5fb7d310dbf7a790d51df348d77dc797a3b27c6da316995e",
    ];
    let items: Vec<_> = (b'a'..=b'h').enumerate().collect();
    let path: FilePath<'_, (usize, u8)> =
        &|(index, _)| Cow::Owned(PathBuf::from(index.to_string()));
    for refuse in [false, true] {
        let _refusing = refuse.then(|| crate::threads::refuse_after(0));
        let caller = std::thread::current().id();
        let seen: Vec<_> = items.iter().map(|_| AtomicUsize::new(0)).collect();
        let ahead = ReadAhead {
            items: &items,
            path,
            asked: AtomicUsize::new(0),
            advise: |_| {},
        };
        let digests = run_hashes(
            &items,
            32 << 20,
            Threads::default(),
            Some(&ahead),
            |(index, byte)| {
                if refuse {
                    assert_eq!(std::thread::current().id(), caller);
                }
                seen[*index].fetch_add(1, Ordering::Relaxed);
                if *index == 3 {
                    crate::digest::Sha256::of_reader(Interrupted, crate::digest::Block::sized(1))
                        .map(|digest| digest.hex())
                } else {
                    let bytes = vec![*byte; 4 << 20];
                    crate::digest::Sha256::of_reader(
                        bytes.as_slice(),
                        crate::digest::Block::sized(bytes.len() as u64),
                    )
                    .map(|digest| digest.hex())
                }
            },
        );
        assert_eq!(digests.len(), expected.len());
        for (index, digest) in digests.into_iter().enumerate() {
            if index == 3 {
                assert_eq!(digest.unwrap_err().kind(), io::ErrorKind::Interrupted);
            } else {
                assert_eq!(digest.unwrap(), expected[index]);
            }
            assert_eq!(seen[index].load(Ordering::Relaxed), 1);
        }
    }
}

#[test]
fn one_worker_runs_local_jobs_on_the_calling_thread() {
    let threads = Threads::read(&|_| Some("1".into())).unwrap();
    let calling = std::thread::current().id();
    let items: Vec<u64> = (0..1000).collect();
    let doubled = run_all(&items, threads, |item| {
        assert_eq!(std::thread::current().id(), calling);
        item * 2
    });
    assert_eq!(
        doubled,
        items.iter().map(|item| item * 2).collect::<Vec<_>>()
    );
}

#[test]
fn a_capped_large_copy_keeps_every_byte() {
    let folder = TempDir::new().unwrap();
    let object = folder.path().join("object");
    let destination = folder.path().join("placed.bin");
    let bytes = vec![0x5a; 3 * 1024 * 1024];
    fs::write(&object, &bytes).unwrap();
    let threads = Threads::read(&|_| Some("1".into())).unwrap();
    let mut placer =
        Placer::starting_at(folder.path().to_path_buf(), Method::Copy, None).with_threads(threads);
    assert_eq!(
        placer.place(&object, &destination, true).unwrap(),
        Method::Copy
    );
    assert_eq!(fs::read(destination).unwrap(), bytes);
}

fn is_read_only(path: &Path) -> bool {
    fs::metadata(path).unwrap().permissions().readonly()
}

#[test]
fn a_link_at_the_root_is_never_followed() {
    let (folder, object) = cache(true);
    let outside = folder.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let root = folder.path().join("data");
    if crate::test_links::link_folder(&outside, &root).is_err() {
        return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
    }
    let mut placer = Placer::new(&root);

    let error = placer
        .place(&object, &root.join("placed.csv"), false)
        .unwrap_err();

    assert!(error.to_string().contains("is a link"), "{error}");
    assert!(!outside.join("placed.csv").exists());
}

#[test]
fn a_link_inside_the_root_is_never_followed() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let outside = folder.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let link = root.join("dataset");
    if crate::test_links::link_folder(&outside, &link).is_err() {
        return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
    }
    let mut placer = Placer::new(&root);

    let error = placer
        .place(&object, &link.join("placed.csv"), false)
        .unwrap_err();

    assert!(error.to_string().contains("is a link"), "{error}");
    assert!(!outside.join("placed.csv").exists());
    assert!(placer.check_folders(&link.join("old.csv"), false).is_err());
}

#[test]
fn a_folder_emptied_by_a_removal_is_made_again_for_the_next_file() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::new(&root);
    let first = root.join("set").join("inner").join("a.csv");
    let method = placer.place(&object, &first, false).unwrap();

    placer.remove(&first, true, Some(method)).unwrap();
    assert!(!root.join("set").exists(), "emptied folders go");
    assert!(root.exists(), "the root stays");
    let second = root.join("set").join("b.csv");
    placer.place(&object, &second, false).unwrap();

    assert_eq!(fs::read_to_string(&second).unwrap(), CONTENTS);
}

/// a placer that copies into `root` as if `root` sat on FAT32 or exFAT, with `scratch` as
/// fetchloom's own folder there.
#[cfg(windows)]
fn on_fat(root: &Path, scratch: &Path) -> Placer {
    let mut placer = Placer::starting_at(root.to_path_buf(), Method::Copy, None);
    placer.volume = Volume::refusing(scratch.to_path_buf(), false);
    placer
}

#[test]
#[cfg(windows)]
fn a_placed_read_only_file_is_removed_on_fat() {
    for recorded in [Some(Method::Copy), Some(Method::Reflink), None] {
        let (folder, object) = cache(true);
        let root = folder.path().join("data");
        let own = folder.path().join(".fetchloom");
        fs::create_dir(&own).unwrap();
        let mut placer = on_fat(&root, &own);
        let destination = root.join("placed.csv");
        placer.place(&object, &destination, false).unwrap();
        assert!(is_read_only(&destination));

        placer.remove(&destination, false, recorded).unwrap();

        assert!(!destination.exists(), "{recorded:?}");
        assert_eq!(
            fs::read_dir(&own).unwrap().count(),
            0,
            "the probe leaves nothing"
        );
    }
}

#[test]
#[cfg(windows)]
fn a_removal_on_fat_leaves_nothing_else_in_a_named_folder() {
    let (folder, object) = cache(true);
    let root = folder.path().join("fetched");
    let mut placer = on_fat(&root, &root);
    for name in ["kept.csv", "replaced.csv"] {
        placer.place(&object, &root.join(name), false).unwrap();
    }

    placer
        .remove(&root.join("replaced.csv"), false, None)
        .unwrap();

    let left: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["kept.csv"]);
}

#[test]
#[cfg(windows)]
fn a_recorded_hardlink_keeps_its_flag_even_on_fat() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = on_fat(&root, folder.path());
    let destination = root.join("placed.csv");
    placer.place(&object, &destination, false).unwrap();

    let refused = placer
        .remove(&destination, false, Some(Method::Hardlink))
        .unwrap_err();

    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(is_read_only(&destination));
}

#[test]
fn refused_placement_workers_keep_borrowed_and_owned_results() {
    let _refusing = crate::threads::refuse_after(0);
    let caller = std::thread::current().id();
    let items: Vec<_> = (0..1024).collect();
    let expected: Vec<_> = items.iter().map(|value| value * 2).collect();
    let borrowed = run_all(&items, Threads::default(), |value| {
        assert_eq!(std::thread::current().id(), caller);
        value * 2
    });
    assert_eq!(borrowed, expected);
    let owned = run_owned(items, Threads::default(), |value| {
        assert_eq!(std::thread::current().id(), caller);
        value * 2
    });
    assert_eq!(owned, expected);
}

#[test]
fn a_stop_after_the_first_placement_starts_no_later_file() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let stop = Arc::new(Stop::default());
    let mut placer =
        Placer::starting_at(root.clone(), Method::Hardlink, None).with_stop(Arc::clone(&stop));
    let seen = AtomicUsize::new(0);
    let results = placer.place_all(
        512,
        |index| Order {
            object: object.clone(),
            destination: root.join(format!("{index}.csv")),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        |_| {
            seen.fetch_add(1, Ordering::Relaxed);
            stop.ask();
        },
    );
    assert_eq!(seen.load(Ordering::Relaxed), 1);
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(fs::read_dir(root).unwrap().count(), 1);
}

#[test]
fn a_copy_checks_for_a_stop_between_blocks() {
    struct Input<'a> {
        left: usize,
        stop: &'a Stop,
    }
    impl Read for Input<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = bytes.len().min(self.left);
            bytes[..count].fill(42);
            self.left -= count;
            self.stop.ask();
            Ok(count)
        }
    }
    let stop = Stop::default();
    let mut input = Input {
        left: 3 * BLOCK,
        stop: &stop,
    };
    let error = copy_stoppable(&mut input, io::sink(), &stop).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(input.left >= 2 * BLOCK);
}

#[test]
fn a_stopped_copy_batch_publishes_nothing_and_drops_staging() {
    let (folder, object) = cache(true);
    let (destinations, staged) = staged_copies(folder.path(), &object);
    let stop = Stop::default();
    stop.ask();
    let results = publish_flushed(
        staged,
        || panic!("a cancelled batch needs no flush"),
        Threads::default(),
        &|_: &Metadata| (),
        &stop,
        &publication_work(&destinations),
    );
    assert!(results.iter().all(|(_, result)| result.is_err()));
    assert!(destinations.iter().all(|path| !path.exists()));
    assert_eq!(placed_names(folder.path()), Vec::<String>::new());
}

#[test]
fn a_stop_during_copy_publication_keeps_only_completed_files() {
    let (folder, object) = cache(true);
    let (destinations, staged) = staged_copies(folder.path(), &object);
    let stop = Stop::default();
    let results = publish_flushed(
        staged,
        || Ok(()),
        Threads::default(),
        &|_: &Metadata| stop.ask(),
        &stop,
        &publication_work(&destinations),
    );
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    assert_eq!(destinations.iter().filter(|path| path.exists()).count(), 1);
    assert_eq!(placed_names(folder.path()), ["0.csv"]);
}

#[test]
fn a_parent_component_cannot_escape_the_placement_root() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::new(&root);
    let escaped = root.join("../escaped.csv");
    let result = placer.place(&object, &escaped, false);
    assert!(result.is_err());
    assert!(!folder.path().join("escaped.csv").exists());
}

#[cfg(unix)]
fn swapped_parent(first: Method) {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let parent = root.join("set");
    let outside = folder.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let destination = parent.join("file.csv");
    let mut placer = Placer::starting_at(root.clone(), first, None);
    placer.check_folders(&destination, true).unwrap();
    fs::rename(&parent, root.join("saved")).unwrap();
    std::os::unix::fs::symlink(&outside, &parent).unwrap();
    let _ = placer.place(&object, &destination, false);
    assert!(
        !outside.join("file.csv").exists(),
        "{first:?} followed a swapped parent"
    );
}

#[cfg(unix)]
#[test]
fn a_swapped_parent_cannot_redirect_a_reflink() {
    swapped_parent(Method::Reflink);
}

#[cfg(unix)]
#[test]
fn a_swapped_parent_cannot_redirect_a_hardlink() {
    swapped_parent(Method::Hardlink);
}

#[cfg(unix)]
#[test]
fn a_swapped_parent_cannot_redirect_a_copy() {
    swapped_parent(Method::Copy);
}

#[cfg(unix)]
#[test]
fn a_swapped_parent_cannot_redirect_removal() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let destination = root.join("set/file.csv");
    let outside = folder.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("file.csv"), b"keep outside").unwrap();
    let mut placer = Placer::new(&root);
    let method = placer.place(&object, &destination, false).unwrap();
    fs::rename(root.join("set"), root.join("saved")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("set")).unwrap();
    assert!(placer.remove(&destination, true, Some(method)).is_err());
    assert_eq!(fs::read(outside.join("file.csv")).unwrap(), b"keep outside");
}

#[cfg(windows)]
fn undo_on_error<T>(result: io::Result<T>, destination: &Path) -> io::Result<T> {
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

#[test]
fn folder_aware_placement_preserves_results_and_probes_its_filesystem() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::starting_at(root.clone(), Method::Copy, None);
    assert!(placer.filesystem.get().is_none());
    for batch in 0..2 {
        let results = placer.place_all(
            256,
            |index| Order {
                object: object.clone(),
                destination: root.join(format!("{}/{batch}-{index}", index % 3)),
                writable: true,
                size: CONTENTS.len() as u64,
            },
            Metadata::len,
        );
        assert_eq!(results.len(), 256);
        for (index, result) in results.into_iter().enumerate() {
            assert_eq!(result.unwrap(), (Method::Copy, CONTENTS.len() as u64));
            assert_eq!(
                fs::read_to_string(root.join(format!("{}/{batch}-{index}", index % 3))).unwrap(),
                CONTENTS
            );
        }
        assert!(placer.filesystem.get().is_some());
    }
}

#[test]
fn an_empty_placement_does_not_latch_a_failed_filesystem_probe() {
    let folder = tempfile::tempdir().unwrap();
    let mut placer = Placer::new(&folder.path().join("missing"));
    let results = placer.place_all(0, |_| panic!("no order"), |_| ());
    assert_eq!(results.len(), 0);
    assert!(placer.filesystem.get().is_none());
}

#[test]
fn owned_partitions_do_not_retain_old_tail_capacity() {
    let count = 8192;
    let runs = owned_runs((0..count).collect(), 8);
    let capacities: Vec<usize> = runs.iter().map(Vec::capacity).collect();
    let held: usize = capacities.iter().sum();
    assert!(
        held <= count * 2,
        "{held} slots for {count} values: {capacities:?}"
    );
    assert_eq!(
        runs.into_iter().flatten().collect::<Vec<_>>(),
        (0..count).collect::<Vec<_>>()
    );
}

#[test]
fn a_chosen_root_may_contain_parent_components() {
    let (folder, object) = cache(true);
    fs::create_dir(folder.path().join("working")).unwrap();
    let root = folder.path().join("working/../got");
    let mut placer = Placer::in_named_folder(&root);
    placer.place(&object, &root.join("file"), false).unwrap();
    assert_eq!(
        fs::read_to_string(folder.path().join("got/file")).unwrap(),
        CONTENTS
    );
    assert!(
        placer
            .place(&object, &root.join("../escaped"), false)
            .is_err()
    );
    assert!(!folder.path().join("escaped").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_copy_batches_finish_flushing_before_they_return() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::starting_at(root.clone(), Method::Copy, None);
    let results = placer.place_all(
        3,
        |index| Order {
            object: object.clone(),
            destination: root.join(format!("{index}")),
            writable: false,
            size: CONTENTS.len() as u64,
        },
        Metadata::len,
    );
    assert!(
        results
            .into_iter()
            .all(|result| result.unwrap() == (Method::Copy, CONTENTS.len() as u64))
    );
    assert!(placer.copy_flush.lock().unwrap().is_none());
    assert_eq!(placed_names(&root), ["0", "1", "2"]);
}

#[test]
fn pipelined_copy_keeps_all_bytes_when_its_reader_is_refused() {
    let bytes: Vec<_> = (0..3 * BLOCK + 17)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect();
    for refuse in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source");
        let destination = folder.path().join("copy");
        fs::write(&source, &bytes).unwrap();
        let input = File::open(source).unwrap();
        let output = File::create_new(&destination).unwrap();
        let _refusing = refuse.then(|| crate::threads::refuse_after(0));
        overlapped(input, &output, &Stop::default()).unwrap();
        drop(output);
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }
}

#[cfg(windows)]
#[test]
fn readonly_windows_placement_tries_hardlinks_before_clones() {
    let (folder, object) = cache(true);
    if !makes_hardlinks(folder.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let root = folder.path().join("data");
    // a sync client's folder around the test's own would rule hardlinks out
    let mut placer = Placer::starting_at(root.clone(), Method::Reflink, None);
    assert_eq!(
        placer.place(&object, &root.join("file"), false).unwrap(),
        Method::Hardlink
    );
    assert!(is_read_only(&root.join("file")));
}

#[test]
fn aliases_above_the_selected_root_keep_their_filesystem_meaning() {
    let (folder, object) = cache(true);
    let real = folder.path().join("real");
    let alias = folder.path().join("alias");
    fs::create_dir(&real).unwrap();
    if crate::test_links::link_folder(&real, &alias).is_err() {
        return crate::test_skips::skip("symlinks", "no symbolic link can be made here");
    }
    let root = alias.join("data");
    let mut placer = Placer::new(&root);
    placer.place(&object, &root.join("file"), false).unwrap();
    assert_eq!(
        fs::read_to_string(real.join("data/file")).unwrap(),
        CONTENTS
    );
}

/// what placing returns for each new name is what that name holds, though a hardlink takes it
/// from the object rather than opening the name.
#[test]
fn placing_records_what_each_new_name_holds() {
    for first in [Method::Hardlink, Method::Copy] {
        let (folder, object) = cache(true);
        let root = folder.path().join("data");
        let destinations: Vec<PathBuf> = ["a/one.csv", "b/two.csv", "b/three.csv"]
            .iter()
            .map(|path| root.join(path))
            .collect();
        let mut placer = Placer::starting_at(root.clone(), first, None);
        placer.check_folders(&destinations[0], true).unwrap();
        placer.check_folders(&destinations[1], true).unwrap();
        let results = placer.place_all(
            destinations.len(),
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            |metadata| (metadata.len(), metadata.modified().unwrap()),
        );
        for (destination, result) in destinations.iter().zip(results) {
            let (_, (size, modified)) = result.unwrap();
            let actual = fs::symlink_metadata(destination).unwrap();
            assert_eq!(size, actual.len(), "{first:?}");
            assert_eq!(modified, actual.modified().unwrap(), "{first:?}");
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        }
    }
}

#[cfg(unix)]
#[test]
fn a_hardlink_still_refuses_an_object_swapped_for_a_symlink() {
    let (folder, object) = cache(true);
    let outside = folder.path().join("outside");
    fs::write(&outside, CONTENTS).unwrap();
    fs::rename(&object, folder.path().join("original")).unwrap();
    std::os::unix::fs::symlink(&outside, &object).unwrap();
    let root = folder.path().join("data");
    let destination = root.join("placed.csv");
    let mut placer = Placer::starting_at(root, Method::Hardlink, None);

    let error = placer.place(&object, &destination, false).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(fs::symlink_metadata(&destination).is_err());
}

/// FAT32 holds no file of 4 GiB or more, so one is refused before any byte is copied, while one a
/// byte smaller, or one on exFAT, is placed. the order states the size, so the object stays small.
#[test]
fn a_file_fat32_cannot_hold_is_refused_before_it_is_copied() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let largest = crate::space::FAT_LARGEST;
    for (filesystem, size, refused) in [
        (Filesystem::Fat, largest + 1, true),
        (Filesystem::Fat, largest, false),
        (Filesystem::Exfat, largest + 1, false),
    ] {
        let mut placer = Placer::starting_at(root.clone(), Method::Copy, None);
        placer.filesystem = OnceLock::from(filesystem);
        let destination = root.join(format!("{filesystem:?}-{size}.csv"));
        let results = placer.place_all(
            1,
            |_| Order {
                object: object.clone(),
                destination: destination.clone(),
                writable: false,
                size,
            },
            |_| (),
        );
        match &results[0] {
            Err(error) if refused => {
                assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
                assert!(error.to_string().contains("FAT32"), "{error}");
                assert!(!destination.exists());
            }
            Ok((method, ())) if !refused => {
                assert_eq!(*method, Method::Copy);
                assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
            }
            other => panic!("{filesystem:?} {size}: {other:?}"),
        }
    }
}

static ASKED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn ask(path: &Path) {
    ASKED.lock().unwrap().push(path.to_owned());
}

/// a worker taking a file asks the kernel for the next ones, each once, in order, and never one
/// taken already, while workers tell their takes out of the order they were handed out.
#[test]
fn reading_ahead_asks_for_each_later_file_once() {
    let items: Vec<PathBuf> = (0..READ_AHEAD * 3)
        .map(|n| PathBuf::from(format!("f{n}")))
        .collect();
    let path: FilePath<'_, PathBuf> = &|item| Cow::Borrowed(item.as_path());
    let ahead = ReadAhead {
        items: &items,
        path,
        asked: AtomicUsize::new(0),
        advise: ask,
    };
    ahead.took(0);
    assert_eq!(ASKED.lock().unwrap().as_slice(), &items[1..=READ_AHEAD]);
    for taken in (1..items.len() + 8).map(|taken| taken ^ 1) {
        ahead.took(taken);
    }
    assert_eq!(ASKED.lock().unwrap().as_slice(), &items[1..]);
}

/// files just written are in memory, so nothing reads ahead of them.
#[test]
fn files_in_memory_are_not_read_ahead() {
    let folder = TempDir::new().unwrap();
    let items: Vec<PathBuf> = (0..20)
        .map(|n| {
            let path = folder.path().join(format!("f{n}"));
            fs::write(&path, b"bytes").unwrap();
            path
        })
        .collect();
    let path: FilePath<'_, PathBuf> = &|item| Cow::Borrowed(item.as_path());
    assert!(ReadAhead::when_cold(&items, path).is_none());
}

/// a co-user of a shared cache may set any bit on an object they wrote; a clone keeps none that
/// would let others run the placed file as the person who placed it, or write it.
#[cfg(unix)]
#[test]
fn a_clone_never_takes_the_setuid_setgid_or_sticky_bit() {
    use std::os::unix::fs::PermissionsExt;
    let special = std::fs::Permissions::from_mode(0o7755);
    assert_eq!(super::clone_mode(&special, false), 0o555);
    assert_eq!(super::clone_mode(&special, true), 0o755);
    let open = std::fs::Permissions::from_mode(0o666);
    assert_eq!(super::clone_mode(&open, true), 0o644);
    assert_eq!(super::clone_mode(&open, false), 0o444);
    let group = std::fs::Permissions::from_mode(0o474);
    assert_eq!(super::clone_mode(&group, true), 0o654);
}

/// a macos clone keeps its object's mode, as fclonefileat copies it, so the mode set through the
/// placed file's handle drops what the object had beyond read, write and run.
#[cfg(unix)]
#[test]
fn a_placed_file_made_read_only_keeps_no_setuid_bit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("placed");
    std::fs::write(&path, b"bytes").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o4755)).unwrap();
    if std::fs::metadata(&path).unwrap().permissions().mode() & 0o4000 == 0 {
        return crate::test_skips::skip("setuid", "this disk keeps no setuid bit");
    }
    let file = std::fs::File::open(&path).unwrap();

    super::set_file_writable(&file, false).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o555, "{mode:o}");
}

/// a clone takes its object's attributes, and a cache object whose archive flag a backup cleared
/// has read-only alone. clearing that through a handle hands windows no attribute at all, which it
/// takes as no change, so a dataset that asks for copies would get a read-only one.
#[cfg(windows)]
#[test]
fn a_file_with_read_only_as_its_only_attribute_is_made_writable() {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("placed.csv");
    std::fs::write(&path, b"a,b\n").unwrap();
    let mut sealed = std::fs::metadata(&path).unwrap().permissions();
    sealed.set_readonly(true);
    std::fs::set_permissions(&path, sealed).unwrap();
    let cleared = std::process::Command::new("attrib")
        .arg("-A")
        .arg(&path)
        .status()
        .unwrap();
    assert!(cleared.success());
    assert_eq!(std::fs::metadata(&path).unwrap().file_attributes(), 0x1);
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .open(&path)
        .unwrap();

    super::set_named_writable(&file, &path, true).unwrap();

    drop(file);
    assert!(!std::fs::metadata(&path).unwrap().permissions().readonly());
}

/// a name with a drive, which a path joins as a new start, or with a colon, which names a stream
/// of another file, never leads a placement out of its folder.
#[cfg(windows)]
#[test]
fn a_name_with_a_drive_or_a_stream_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let parents = super::Parents::default();
    for below in ["C:x", "data\\C:x", "x:s", "data\\x:s", "data\\C:x\\file"] {
        assert!(
            parents
                .locate(dir.path(), Path::new(below), true, true)
                .is_err(),
            "{below}"
        );
    }
    assert!(
        parents
            .locate(dir.path(), Path::new("data\\x"), true, true)
            .is_ok()
    );
}

/// a sync placing into btrfs takes at most [`BTRFS_THREADS`] threads, fewer when the run asks for
/// fewer; any other filesystem, and a placer that did not ask for sync's counts, as `get`'s does
/// not, take as many as before, with the same files placed either way.
#[test]
fn placing_into_btrfs_takes_a_few_threads() {
    let any = Threads::default();
    assert_eq!(
        placing_threads(any, Filesystem::Btrfs).limit(8),
        BTRFS_THREADS
    );
    assert_eq!(placing_threads(any, Filesystem::Other).limit(8), 8);
    for asked in [1, 2] {
        let capped = Threads::default().at_most(asked);
        assert_eq!(placing_threads(capped, Filesystem::Btrfs).limit(8), asked);
    }
    assert_eq!(placing_threads(any, Filesystem::Btrfs).limit(2), 2);

    let (folder, object) = cache(true);
    let processors = crate::threads::available_parallelism().map_or(1, NonZero::get);
    // the first file goes alone on the calling thread, before any worker starts
    let caller = std::thread::current().id();
    for (filesystem, sync) in [
        (Filesystem::Other, true),
        (Filesystem::Btrfs, true),
        (Filesystem::Btrfs, false),
    ] {
        let root = folder.path().join(format!("data-{filesystem:?}-{sync}"));
        let workers = Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let seen = Arc::clone(&workers);
        let placer =
            Placer::starting_at(root.clone(), Method::Hardlink, None).observing(move |_| {
                let worker = std::thread::current().id();
                if worker != caller {
                    seen.lock().unwrap().insert(worker);
                }
            });
        let mut placer = if sync {
            placer.with_sync_threads()
        } else {
            placer
        };
        assert!(placer.filesystem.set(filesystem).is_ok());
        let destinations: Vec<PathBuf> = (0..1024)
            .map(|index| root.join(format!("d{}/{index}.csv", index % 4)))
            .collect();
        let results = placer.place_all(
            destinations.len(),
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            Metadata::len,
        );
        for result in results {
            assert_eq!(
                result.unwrap().1,
                CONTENTS.len() as u64,
                "{filesystem:?} {sync}"
            );
        }
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        }
        let used = workers.lock().unwrap().len();
        if filesystem == Filesystem::Btrfs && sync {
            assert!(used <= BTRFS_THREADS, "{used}");
        } else {
            assert!(
                processors <= BTRFS_THREADS || used > BTRFS_THREADS,
                "{filesystem:?} {sync}: {used}"
            );
        }
    }
}

/// jobs run until one fails, flushing or hashing, stop as jobs run in turn do: every job before the
/// first failure in their order runs, and it is the failure the results hold first. one worker
/// starts no job past it; with more, those begun past it are the only extra work.
#[test]
fn jobs_stop_at_the_first_failure_in_order() {
    use std::sync::Mutex;

    let items: Vec<usize> = (0..64).collect();
    for (workers, hashing) in [1, 2, 8].into_iter().flat_map(|n| [(n, false), (n, true)]) {
        for failing in [vec![0], vec![5], vec![5, 9], vec![9, 5], vec![63], vec![]] {
            let ran = Mutex::new(Vec::new());
            let job = |&item: &usize| {
                ran.lock().unwrap().push(item);
                if failing.contains(&item) {
                    Err(item)
                } else {
                    Ok(item)
                }
            };
            let cap = Threads::default().at_most(workers);
            // so many bytes that the hashing runner takes every worker the cap allows
            let results = if hashing {
                run_hashes_until_failure(&items, 1 << 30, cap, job)
            } else {
                run_flushing_until_failure(&items, cap, job)
            };
            let first = failing.iter().min().copied();
            let told = results.iter().find_map(|result| match result {
                Some(Err(item)) => Some(*item),
                Some(Ok(_)) | None => None,
            });
            assert_eq!(told, first, "{workers} {failing:?}");
            let mut ran = ran.into_inner().unwrap();
            ran.sort_unstable();
            let last = first.unwrap_or(items.len() - 1);
            assert!(
                (0..=last).all(|item| ran.contains(&item)),
                "{workers} {failing:?}"
            );
            if workers == 1 {
                assert_eq!(ran, (0..=last).collect::<Vec<_>>(), "{failing:?}");
            }
            for (item, result) in results.iter().enumerate() {
                assert_eq!(
                    result.is_some(),
                    ran.contains(&item),
                    "{workers} {failing:?}"
                );
            }
        }
    }
}

/// on linux a sync's placement of a few copies, each flushed on its own, places them on several
/// threads, each whole under its own name with no hidden copy left; a larger one, which waits for
/// one flush of the filesystem, keeps its one thread, as does a placer that did not ask for sync's
/// counts, as `get`'s does not.
#[cfg(target_os = "linux")]
#[test]
fn a_few_copies_flushed_alone_go_on_threads() {
    use crate::volume::MOST_FLUSHED_ALONE;
    use std::collections::HashSet;
    use std::sync::Mutex;

    let (folder, object) = cache(true);
    let processors = crate::threads::available_parallelism().map_or(1, NonZero::get);
    let runs = [true, false].into_iter().flat_map(|sync| {
        [1, 60, MOST_FLUSHED_ALONE, MOST_FLUSHED_ALONE + 1].map(|files| (sync, files))
    });
    for (sync, files) in runs {
        let root = folder.path().join(format!("data-{files}-{sync}"));
        let workers = Arc::new(Mutex::new(HashSet::new()));
        let seen = Arc::clone(&workers);
        let placer = Placer::starting_at(root.clone(), Method::Copy, None).observing(move |_| {
            seen.lock().unwrap().insert(std::thread::current().id());
        });
        let mut placer = if sync {
            placer.with_sync_threads()
        } else {
            placer
        };
        let destinations: Vec<PathBuf> = (0..files)
            .map(|index| root.join(format!("{index}.csv")))
            .collect();
        let results = placer.place_all(
            files,
            |index| Order {
                object: object.clone(),
                destination: destinations[index].clone(),
                writable: false,
                size: CONTENTS.len() as u64,
            },
            Metadata::len,
        );
        for result in results {
            assert_eq!(result.unwrap(), (Method::Copy, CONTENTS.len() as u64));
        }
        for destination in &destinations {
            assert_eq!(fs::read_to_string(destination).unwrap(), CONTENTS);
        }
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            files,
            "a hidden copy was left"
        );
        let used = workers.lock().unwrap().len();
        let spread = sync && processors > 1 && files > 1 && files <= MOST_FLUSHED_ALONE;
        assert_eq!(used > 1, spread, "{files} {sync}: {used}");
    }
}
