//! the cache writes only below its own real folders, and never through a link someone planted.

use std::fs;
use std::io::Write;

use super::tests::hardlinks;
use super::{Cache, Claim, Validators};

/// the name every file of the download of `url` shares in `partial/`.
fn key(url: &str) -> String {
    crate::digest::sha256(url.as_bytes()).hex()
}

fn owned(cache: &Cache, url: &str) -> super::Partial {
    match cache.claim(url).unwrap() {
        Claim::Owned(partial) => partial,
        Claim::Busy => panic!("nothing else holds {url}"),
    }
}

#[test]
fn cache_folders_cannot_be_links_to_another_folder() {
    // FAT32 and exFAT can make neither a junction nor a hardlink, so nobody can plant a link
    if !hardlinks(tempfile::tempdir().unwrap().path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    for name in [
        "objects", "partial", "urls", "stated", "unpacked", "projects", "tmp", "changed",
        "imported", "keyed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let outside = dir.path().join("outside");
        Cache::open(root.clone()).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::remove_dir(root.join(name)).unwrap();
        crate::test_links::link_folder(&outside, &root.join(name)).unwrap();
        assert!(
            Cache::open(root).is_err(),
            "{name} must not redirect cache writes"
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
}

#[test]
fn replacing_a_partial_folder_never_redirects_writes() {
    let (dir, cache) = super::tests::cache();
    // FAT32 and exFAT can make neither a junction nor a hardlink, so nobody can plant a link
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let partial = cache.root.join("partial");
    fs::rename(&partial, cache.root.join("previous-partial")).unwrap();
    crate::test_links::link_folder(&outside, &partial).unwrap();
    if let Ok(Claim::Owned(download)) = cache.claim("https://example.org/data")
        && let Ok(mut writer) = download.writer(0, None, Validators::default(), false, false)
    {
        writer.write_all(b"never outside").unwrap();
    }
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn sealing_never_reaches_a_file_swapped_in_for_the_download() {
    let (dir, cache) = super::tests::cache();
    // FAT32 and exFAT cannot hardlink, so nobody can swap in a second name of a file
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let url = "https://example.org/data";
    let outside = dir.path().join("outside");
    fs::write(&outside, b"abc").unwrap();
    let download = owned(&cache, url);
    let mut writer = download
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.flush().unwrap();
    let part = cache
        .partial_folder(&key(url))
        .join(format!("{}.part", key(url)));
    fs::rename(&part, dir.path().join("moved")).unwrap();
    fs::hard_link(&outside, &part).unwrap();
    // publishing takes whatever has the name, which anyone who can write to the cache could
    // change anyway, but the seal must not make a file outside the cache read-only
    let _ = writer.finish();
    assert!(!fs::metadata(&outside).unwrap().permissions().readonly());
}

#[test]
fn a_writable_partial_with_a_second_name_is_never_written_into() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let url = "https://example.org/data";
    let outside = dir.path().join("outside");
    fs::write(&outside, b"keep these bytes").unwrap();
    let download = owned(&cache, url);
    let part = cache
        .partial_folder(&key(url))
        .join(format!("{}.part", key(url)));
    fs::hard_link(&outside, &part).unwrap();
    let mut writer = download
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"new").unwrap();
    drop(writer);
    assert_eq!(fs::read(&outside).unwrap(), b"keep these bytes");
}

#[cfg(unix)]
#[test]
fn a_partial_and_its_lock_never_open_a_link() {
    let (dir, cache) = super::tests::cache();
    let url = "https://example.org/data";
    let outside = dir.path().join("outside");
    fs::write(&outside, b"keep these bytes").unwrap();
    let folder = cache.partial_folder(&key(url));
    let part = folder.join(format!("{}.part", key(url)));
    let download = owned(&cache, url);
    let mut writer = download
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"keep").unwrap();
    writer.save().unwrap();
    drop(writer);
    fs::remove_file(&part).unwrap();
    fs::hard_link(&outside, &part).unwrap();
    assert!(
        download.resumable().is_none(),
        "a second name is never continued"
    );
    fs::remove_file(&part).unwrap();
    std::os::unix::fs::symlink(&outside, &part).unwrap();
    assert!(download.resumable().is_none(), "a link is never continued");
    let mut writer = download
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"new").unwrap();
    drop(writer);
    assert!(fs::symlink_metadata(&part).unwrap().is_file());
    drop(download);

    let lock = folder.join(format!("{}.lock", key(url)));
    fs::remove_file(&lock).unwrap();
    std::os::unix::fs::symlink(&outside, lock).unwrap();
    assert!(cache.claim(url).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"keep these bytes");
}

#[test]
fn a_changed_folder_swapped_for_a_link_never_redirects_work() {
    let (dir, cache) = super::tests::cache();
    // FAT32 and exFAT can make neither a junction nor a hardlink, so nobody can plant a link
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let digest = crate::digest::sha256(b"object");
    let kept = outside.join(digest.hex());
    fs::write(&kept, b"keep these bytes").unwrap();
    let folder = cache.root.join("changed");
    fs::create_dir_all(&folder).unwrap();
    fs::rename(&folder, cache.root.join("old-changed")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    assert!(cache.mark_changed(&digest).is_err());
    assert!(cache.marks().is_err());
    cache.forget_changed(&std::collections::HashSet::new());

    assert_eq!(fs::read(kept).unwrap(), b"keep these bytes");
}

/// a folder outside the cache holding a file named as a marked object is, as git lfs and hugging
/// face name theirs, and a test step that swaps `changed/` for a link to it, as someone sharing the
/// cache can, once the cache has looked at `changed/`.
fn changed_swapped_meanwhile(cache: &Cache, digest: &crate::digest::Sha256) -> std::path::PathBuf {
    let outside = cache.root.with_file_name("outside");
    fs::create_dir(&outside).unwrap();
    let theirs = outside.join(digest.hex());
    fs::write(&theirs, b"keep these bytes").unwrap();
    let folder = cache.root.join("changed");
    let aside = cache.root.join("old-changed");
    super::MEANWHILE.set(Some(Box::new(move || {
        fs::rename(&folder, aside).unwrap();
        crate::test_links::link_folder(&outside, &folder).unwrap();
    })));
    theirs
}

/// gc lists `changed/`, then removes the mark of each object that is gone, so `changed/` swapped
/// for a link in between must not lead it to delete files of those names elsewhere.
#[test]
fn forgetting_marks_never_follows_a_changed_folder_swapped_meanwhile() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let digest = crate::digest::sha256(b"object");
    cache.mark_changed(&digest).unwrap();
    let theirs = changed_swapped_meanwhile(&cache, &digest);

    let mut not_removed = Vec::new();
    cache
        .forget_changed_reporting(
            &std::collections::HashSet::new(),
            &crate::stop::Stop::default(),
            &mut not_removed,
        )
        .unwrap();

    assert!(super::MEANWHILE.take().is_none(), "the folder was swapped");
    assert_eq!(fs::read(theirs).unwrap(), b"keep these bytes");
}

/// a mark is written aside in `tmp/`, then renamed into `changed/`, so `changed/` swapped for a
/// link in between must not lead the rename to replace a file of that name elsewhere.
#[test]
fn a_mark_never_replaces_a_file_through_a_changed_folder_swapped_meanwhile() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let digest = crate::digest::sha256(b"object");
    let theirs = changed_swapped_meanwhile(&cache, &digest);

    let _ = cache.mark_changed(&digest);

    assert!(super::MEANWHILE.take().is_none(), "the folder was swapped");
    assert_eq!(fs::read(theirs).unwrap(), b"keep these bytes");
}

/// a mark swapped for a link once looked at is never read through it: a fifo there would never
/// end the read, and a file elsewhere would pass for the mark.
#[cfg(unix)]
#[test]
fn a_mark_swapped_for_a_link_meanwhile_is_never_read_through_it() {
    let (dir, cache) = super::tests::cache();
    let digest = crate::digest::sha256(b"object");
    cache
        .mark_replaced(&digest, std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let mark = cache.root.join("changed").join(digest.hex());
    let outside = dir.path().join("outside");
    fs::write(&outside, b"9000000000000000000\n").unwrap();
    let link = mark.clone();
    super::MEANWHILE.set(Some(Box::new(move || {
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
    })));

    let read = cache.changed_at(&digest);

    assert!(super::MEANWHILE.take().is_none(), "the mark was swapped");
    assert_eq!(
        read.map_err(|error| error.message),
        Err(format!(
            "could not read the cache's list of changed files at {}: it is not a plain file",
            mark.display()
        ))
    );
}

/// a fan folder someone made a link, as a junction on windows, is reported as the link it is, with
/// help to remove it, and never listed: gc would count, and verify hash, the files it leads to.
#[test]
fn scanning_never_follows_a_fan_folder_swapped_for_a_link() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let hex = "cd".repeat(32);
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join(&hex[2..]), b"theirs").unwrap();
    let fan = cache.root.join("objects").join(&hex[..2]);
    let _ = fs::remove_dir(&fan);
    crate::test_links::link_folder(&outside, &fan).unwrap();

    let scan = cache
        .scan_objects(
            crate::threads::Threads::default(),
            &crate::stop::Stop::default(),
        )
        .unwrap();

    assert_eq!(scan.objects.len(), 0, "a file the link leads to");
    let [failure] = scan.unreadable.as_slice() else {
        panic!("one failure, not {:?}", scan.unreadable);
    };
    assert_eq!(failure.path, fan);
    assert!(
        failure.advice().contains("remove the link"),
        "{}",
        failure.advice()
    );
}

/// an archive's object dropped once its files are unpacked goes through its fan folder reached from
/// the cache's root, so a fan folder someone swapped for a link sends the removal nowhere else.
#[test]
fn dropping_an_object_never_follows_a_fan_folder_swapped_for_a_link() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let digest = crate::digest::sha256(b"archive");
    let hex = digest.hex();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let theirs = outside.join(&hex[2..]);
    fs::write(&theirs, b"keep these bytes").unwrap();
    let fan = cache.root.join("objects").join(&hex[..2]);
    let _ = fs::remove_dir(&fan);
    crate::test_links::link_folder(&outside, &fan).unwrap();

    let _ = cache.drop_object(&digest);

    assert_eq!(fs::read(theirs).unwrap(), b"keep these bytes");
}

/// gc reads every list of imports and removes each that names no object it holds, so a link
/// swapped in for `imported/` must not lead it to read or delete someone's files.
#[test]
fn an_imported_folder_swapped_for_a_link_keeps_what_it_points_at() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let kept = outside.join("notes.txt");
    fs::write(&kept, b"keep these bytes").unwrap();
    let folder = cache.root.join("imported");
    fs::create_dir_all(&folder).unwrap();
    fs::rename(&folder, cache.root.join("old-imported")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    assert_eq!(cache.imported(), std::collections::HashMap::new());
    cache.forget_imported();

    assert_eq!(fs::read(&kept).unwrap(), b"keep these bytes");
}

/// a removal inside the cache goes through a handle on its folder, so a folder swapped for a link
/// after gc checked it, as by someone sharing the cache, sends it nowhere else.
#[cfg(unix)]
#[test]
fn a_removal_inside_the_cache_never_follows_a_linked_folder() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let kept = outside.join("notes.txt");
    fs::write(&kept, b"keep these bytes").unwrap();
    let folder = cache.root.join("imported");
    fs::rename(&folder, cache.root.join("old-imported")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    assert!(crate::place::remove_below(&cache.root, &folder.join("notes.txt")).is_err());

    assert_eq!(fs::read(&kept).unwrap(), b"keep these bytes");
}

/// on windows a removal inside the cache goes through the cache's placer, which holds each folder
/// on its way, so a folder swapped for a junction after gc checked it sends the removal nowhere
/// else, and the file the junction leads to stays.
#[cfg(windows)]
#[test]
fn a_removal_inside_the_cache_never_follows_a_junction() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let kept = outside.join("notes.txt");
    fs::write(&kept, b"keep these bytes").unwrap();
    let folder = cache.root.join("imported");
    fs::rename(&folder, cache.root.join("old-imported")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    assert!(cache.unlink_inside(&folder.join("notes.txt")).is_err());
    assert!(cache.remove_inside(&folder.join("notes.txt")).is_err());

    assert_eq!(fs::read(&kept).unwrap(), b"keep these bytes");
}

#[test]
fn a_swept_folder_swapped_for_a_link_keeps_what_it_points_at() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let kept = outside.join("old.tmp");
    fs::write(&kept, b"keep these bytes").unwrap();
    let a_week_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(24 * 7);
    fs::File::options()
        .write(true)
        .open(&kept)
        .unwrap()
        .set_modified(a_week_ago)
        .unwrap();
    // swapped after the cache opened, as by someone sharing it while gc reads the projects
    let folder = cache.root.join("tmp");
    fs::create_dir_all(&folder).unwrap();
    fs::rename(&folder, cache.root.join("old-tmp")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    let freed = cache.sweep(&super::Needed::default(), std::time::SystemTime::now());

    assert_eq!(fs::read(&kept).unwrap(), b"keep these bytes");
    assert!(
        freed
            .not_removed
            .iter()
            .any(|failure| failure.path.ends_with("old.tmp"))
    );
}

#[test]
fn a_staging_folder_reached_through_a_swapped_tmp_keeps_its_files() {
    let (dir, cache) = super::tests::cache();
    if !hardlinks(dir.path()) {
        return crate::test_skips::skip("hardlinks", "this disk makes no hardlinks");
    }
    let outside = dir.path().join("outside");
    let stage = outside.join("0123456789abcdef.stage");
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join("lock"), b"").unwrap();
    let payload = stage.join("payload");
    fs::write(&payload, b"keep these bytes").unwrap();
    let folder = cache.root.join("tmp");
    fs::create_dir_all(&folder).unwrap();
    fs::rename(&folder, cache.root.join("old-tmp")).unwrap();
    crate::test_links::link_folder(&outside, &folder).unwrap();

    let freed = cache.sweep(&super::Needed::default(), std::time::SystemTime::now());

    assert_eq!(fs::read(&payload).unwrap(), b"keep these bytes");
    assert!(stage.join("lock").exists());
    assert_ne!(freed.not_removed, []);
}

#[cfg(unix)]
#[test]
fn verifying_never_hashes_an_object_swapped_for_a_symlink() {
    for bytes in [b"good", b"evil"] {
        let (dir, cache) = super::tests::cache();
        let stored = super::tests::store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let outside = dir.path().join("outside");
        fs::write(&outside, bytes).unwrap();
        fs::rename(&object, dir.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &object).unwrap();

        let error = cache.verify(&stored.digest).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains(&object.display().to_string()));
        assert!(cache.changed().is_empty());
        assert_eq!(fs::read(&outside).unwrap(), bytes);
        assert!(!fs::metadata(&outside).unwrap().permissions().readonly());
    }
}

#[cfg(unix)]
#[test]
fn finding_never_hashes_an_object_swapped_for_a_symlink() {
    for readonly in [false, true] {
        let (dir, cache) = super::tests::cache();
        let stored = super::tests::store(&cache, b"good");
        let object = cache.object_path(&stored.digest);
        let outside = dir.path().join("outside");
        fs::write(&outside, b"good").unwrap();
        if readonly {
            super::tests::seal(&outside);
        }
        fs::rename(&object, dir.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &object).unwrap();
        cache.mark_changed(&stored.digest).unwrap();

        assert!(cache.find(&stored.digest, Some(4)).is_none());
        assert_eq!(fs::read(&outside).unwrap(), b"good");
        assert_eq!(
            fs::metadata(&outside).unwrap().permissions().readonly(),
            readonly
        );
    }
}

#[cfg(unix)]
#[test]
fn trusting_never_hashes_an_object_swapped_for_a_symlink() {
    let (dir, cache) = super::tests::cache();
    let stored = super::tests::store(&cache, b"good");
    let object = cache.object_path(&stored.digest);
    let outside = dir.path().join("outside");
    fs::write(&outside, b"good").unwrap();
    fs::rename(&object, dir.path().join("original")).unwrap();
    std::os::unix::fs::symlink(&outside, &object).unwrap();

    let found = cache.trusted(&stored.digest, Some(4), &cache.doubts().unwrap());

    assert!(!matches!(found, Ok(Some(_))));
    assert_eq!(fs::read(&outside).unwrap(), b"good");
    assert!(!fs::metadata(&outside).unwrap().permissions().readonly());
}

#[cfg(unix)]
#[test]
fn publishing_replaces_an_object_swapped_for_a_symlink() {
    let (dir, cache) = super::tests::cache();
    let stored = super::tests::store(&cache, b"good");
    let object = cache.object_path(&stored.digest);
    let outside = dir.path().join("outside");
    fs::write(&outside, b"good").unwrap();
    fs::rename(&object, dir.path().join("original")).unwrap();
    std::os::unix::fs::symlink(&outside, &object).unwrap();

    let replacement = super::tests::store(&cache, b"good");

    assert!(replacement.new);
    assert!(fs::symlink_metadata(&object).unwrap().is_file());
    assert_eq!(fs::read(&object).unwrap(), b"good");
    assert_eq!(fs::read(&outside).unwrap(), b"good");
    assert!(!fs::metadata(&outside).unwrap().permissions().readonly());
}
