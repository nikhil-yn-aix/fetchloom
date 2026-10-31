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

#[test]
fn synced_copy_advice_does_not_promise_zero_disk() {
    for (place, subject, expected) in [
        (
            "data",
            "files were",
            "data is inside onedrive, which breaks links, so files were copied. a folder outside onedrive lets fetchloom try links",
        ),
        (
            "iris",
            "the file was",
            "iris is inside onedrive, which breaks links, so the file was copied. a folder outside onedrive lets fetchloom try links",
        ),
    ] {
        assert_eq!(
            Unlinked::Synced("onedrive").copied_because(place, subject),
            expected
        );
    }
}

#[test]
fn copy_advice_on_drives_without_links_names_the_place() {
    assert_eq!(
        Unlinked::NoLinks.copied_because("data", "files were"),
        "the drive holding data cannot link files, so files were copied"
    );
    assert_eq!(
        Unlinked::NoLinks.copied_because("iris", "the file was"),
        "the drive holding iris cannot link files, so the file was copied"
    );
}

#[test]
fn a_cache_on_another_drive_explains_the_copy() {
    assert_eq!(
        Unlinked::OtherDrive.copied_because("data", "files were"),
        "the cache is on another drive, so files were copied. set FETCHLOOM_CACHE to a folder on this drive to let fetchloom try links"
    );
    assert_eq!(
        Unlinked::OtherDrive.copied_because("iris", "the file was"),
        "the cache is on another drive, so the file was copied. set FETCHLOOM_CACHE to a folder on this drive to let fetchloom try links"
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
fn a_hardlink_keeps_the_objects_read_only_flag() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT make no hardlinks
    if !makes_hardlinks(folder.path()) {
        return;
    }
    let destination = folder.path().join("placed.csv");
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    let method = placer.place(&object, &destination, false).unwrap();

    assert_eq!(method, Method::Hardlink);
    assert!(is_read_only(&destination));
    assert!(is_read_only(&object));
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

#[test]
fn a_copy_goes_only_into_the_file_that_claimed_the_name() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT cannot hardlink, so nobody can swap in a second name of a file
    if !makes_hardlinks(folder.path()) {
        return;
    }
    let destination = folder.path().join("placed.csv");
    let outside = folder.path().join("outside.csv");
    fs::write(&outside, "someone else's file").unwrap();
    let claimed = File::create_new(&destination).unwrap();
    // someone swaps a link to another file in for the name once it is claimed
    fs::rename(&destination, folder.path().join("claimed.csv")).unwrap();
    fs::hard_link(&outside, &destination).unwrap();

    fill(&object, &claimed, false, Threads::default()).unwrap();

    assert_eq!(fs::read_to_string(&outside).unwrap(), "someone else's file");
    assert!(!is_read_only(&outside));
    let copied = folder.path().join("claimed.csv");
    assert_eq!(fs::read_to_string(&copied).unwrap(), CONTENTS);
    assert!(is_read_only(&copied));
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

    let result = fill(&object, &output, true, Threads::default());

    assert!(
        result.is_ok(),
        "unchanged permissions need no write access to attributes: {result:?}"
    );
    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert!(!is_read_only(&destination));
    let readonly = undo_on_error(
        fill(&object, &output, false, Threads::default()),
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

    copy(&object, &destination, true, Threads::default(), true).unwrap();

    assert_eq!(fs::read_to_string(&destination).unwrap(), CONTENTS);
    assert!(!is_read_only(&destination));
    assert!(is_read_only(&object));
    let readonly = destination_folder.path().join("readonly.csv");
    assert!(copy(&object, &readonly, false, Threads::default(), true).is_err());
    assert!(!readonly.exists());
}

#[test]
fn an_interrupted_copy_never_leaves_partial_bytes_at_its_name() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const CHILD: &str = "FETCHLOOM_TEST_COPY_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        copy(
            &root.join("object"),
            &root.join("placed.bin"),
            false,
            Threads::default(),
            true,
        )
        .unwrap();
        return;
    }
    let folder = TempDir::new().unwrap();
    let object = folder.path().join("object");
    let destination = folder.path().join("placed.bin");
    fs::write(&object, vec![0x5a; 64 << 20]).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "place::tests::an_interrupted_copy_never_leaves_partial_bytes_at_its_name",
        ])
        .env(CHILD, folder.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let started_copy = loop {
        if fs::read_dir(folder.path())
            .unwrap()
            .flatten()
            .any(|entry| entry.path() != object)
        {
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
        started_copy,
        "the copy never started: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if destination.exists() {
        assert_eq!(
            fs::metadata(&destination).unwrap().len(),
            64 << 20,
            "a killed copy left partial bytes at the destination"
        );
        assert_eq!(
            crate::digest::Sha256::of_file(&destination).unwrap(),
            crate::digest::Sha256::of_file(&object).unwrap()
        );
    }
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

    copy(&object, &destination, false, Threads::default(), true).unwrap();

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
            let error = copy(&object, &destination, writable, threads, true).unwrap_err();
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

    assert!(copy(&missing, &destination, false, Threads::default(), true).is_err());

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

    copy(&object, &destination, false, Threads::default(), true).unwrap();

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

#[test]
fn the_whole_ladder_never_copies_within_one_volume() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT link nothing, so there every rung ends in a copy
    if !makes_hardlinks(folder.path()) {
        return;
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
        // how NTFS refuses a reflink: "incorrect function", wrapped in an HRESULT.
        #[cfg(windows)]
        io::Error::from_raw_os_error(0x8007_0001_u32.cast_signed()),
    ];
    for refusal in refusals {
        let (folder, object) = cache(true);
        let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);
        // a volume whose own probe agrees that it links nothing
        placer.volume = Volume::refusing(folder.path().to_path_buf(), false);
        let described = refusal.to_string();

        placer.learn(Method::Hardlink, refusal).unwrap();
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
        return;
    }
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    // code 1 is EPERM on linux, which protected_hardlinks answers for another user's object
    // on a volume that links
    placer
        .learn(Method::Hardlink, io::Error::from_raw_os_error(1))
        .unwrap();
    let method = placer
        .place(&object, &folder.path().join("placed.csv"), false)
        .unwrap();

    assert_eq!(method, Method::Hardlink);
    assert_eq!(placer.unlinked(), None);
}

#[test]
fn too_many_links_falls_back_for_that_file_alone() {
    let (folder, object) = cache(true);
    // FAT32 and exFAT make no hardlinks to fall back from
    if !makes_hardlinks(folder.path()) {
        return;
    }
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Hardlink, None);

    placer
        .learn(Method::Hardlink, io::ErrorKind::TooManyLinks.into())
        .unwrap();
    let method = placer
        .place(&object, &folder.path().join("placed.csv"), false)
        .unwrap();

    assert_eq!(method, Method::Hardlink);
}

#[test]
fn a_drive_that_refuses_every_link_is_told_from_a_cache_elsewhere() {
    let folder = TempDir::new().unwrap();
    let placer = |synced| Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, synced);
    let refused = || io::Error::from(io::ErrorKind::Unsupported);

    let linking = placer(None);
    assert_eq!(linking.unlinked(), None);
    let mut no_links = placer(None);
    no_links.volume = Volume::refusing(folder.path().to_path_buf(), false);
    no_links.learn(Method::Reflink, refused()).unwrap();
    no_links.learn(Method::Hardlink, refused()).unwrap();
    assert_eq!(no_links.unlinked(), Some(Unlinked::NoLinks));
    let elsewhere = placer(None);
    elsewhere.learn(Method::Reflink, refused()).unwrap();
    elsewhere
        .learn(Method::Hardlink, io::ErrorKind::CrossesDevices.into())
        .unwrap();
    // a cache on this drive helps only where this drive makes links, which FAT does not
    let blamed = if makes_hardlinks(folder.path()) {
        Unlinked::OtherDrive
    } else {
        Unlinked::NoLinks
    };
    assert_eq!(elsewhere.unlinked(), Some(blamed));
    let synced = placer(Some("onedrive"));
    synced.learn(Method::Reflink, refused()).unwrap();
    assert_eq!(synced.unlinked(), Some(Unlinked::Synced("onedrive")));
}

#[test]
fn a_cache_elsewhere_is_not_blamed_for_a_drive_that_links_nothing() {
    let folder = TempDir::new().unwrap();
    let mut placer = Placer::starting_at(folder.path().to_path_buf(), Method::Reflink, None);
    placer.volume = Volume::refusing(folder.path().to_path_buf(), false);

    placer
        .learn(Method::Reflink, io::ErrorKind::CrossesDevices.into())
        .unwrap();
    placer
        .learn(Method::Hardlink, io::ErrorKind::CrossesDevices.into())
        .unwrap();

    assert_eq!(placer.unlinked(), Some(Unlinked::NoLinks));
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
fn neighbouring_orders_go_to_different_folders() {
    let object = Path::new("object");
    let paths: Vec<PathBuf> = ["a/1", "a/2", "a/3", "b/1", "c/1", "c/2"]
        .iter()
        .map(PathBuf::from)
        .collect();
    let orders = paths.iter().enumerate().map(|(index, destination)| {
        (
            index,
            Order {
                object: object.to_path_buf(),
                destination: destination.clone(),
                writable: false,
            },
        )
    });
    let indexes = spread(orders);
    assert_eq!(indexes, [0, 3, 4, 1, 5, 2]);
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
    if !link_folder(&outside, &root) {
        return;
    }
    let mut placer = Placer::new(&root, folder.path());

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
    if !link_folder(&outside, &link) {
        return;
    }
    let mut placer = Placer::new(&root, folder.path());

    let error = placer
        .place(&object, &link.join("placed.csv"), false)
        .unwrap_err();

    assert!(error.to_string().contains("is a link"), "{error}");
    assert!(!outside.join("placed.csv").exists());
    assert!(placer.check_folders(&link.join("old.csv"), false).is_err());
}

/// makes `link` point at the folder `target`: a junction on windows, which needs no admin.
pub(super) fn link_folder(target: &Path, link: &Path) -> bool {
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success());
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link).is_ok();
    made
}

#[test]
fn a_folder_emptied_by_a_removal_is_made_again_for_the_next_file() {
    let (folder, object) = cache(true);
    let root = folder.path().join("data");
    let mut placer = Placer::new(&root, folder.path());
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
fn on_fat(root: &Path, scratch: &Path) -> Placer {
    let mut placer = Placer::starting_at(root.to_path_buf(), Method::Copy, None);
    placer.volume = Volume::refusing(scratch.to_path_buf(), false);
    placer
}

#[test]
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
