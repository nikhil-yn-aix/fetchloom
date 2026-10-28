//! the cache writes only below its own real folders, and never through a link someone planted.

use std::fs;
use std::io::Write;
use std::path::Path;

use super::{Cache, Claim, Validators};

fn link_folder(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

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
    for name in [
        "objects", "partial", "urls", "stated", "unpacked", "projects", "tmp", "changed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cache");
        let outside = dir.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        link_folder(&outside, &root.join(name));
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
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let partial = cache.root.join("partial");
    fs::rename(&partial, cache.root.join("previous-partial")).unwrap();
    link_folder(&outside, &partial);
    if let Ok(Claim::Owned(download)) = cache.claim("https://example.org/data")
        && let Ok(mut writer) = download.writer(0, None, Validators::default(), false)
    {
        writer.write_all(b"never outside").unwrap();
    }
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn sealing_never_reaches_a_file_swapped_in_for_the_download() {
    let (dir, cache) = super::tests::cache();
    let url = "https://example.org/data";
    let outside = dir.path().join("outside");
    fs::write(&outside, b"abc").unwrap();
    let download = owned(&cache, url);
    let mut writer = download
        .writer(0, None, Validators::default(), false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.flush().unwrap();
    let part = cache
        .root
        .join("partial")
        .join(format!("{}.part", key(url)));
    fs::rename(&part, dir.path().join("moved")).unwrap();
    fs::hard_link(&outside, &part).unwrap();
    // publishing takes whatever has the name, which anyone who can write to the cache could
    // change anyway, but the seal must not make a file outside the cache read-only
    let _ = writer.finish();
    assert!(!fs::metadata(&outside).unwrap().permissions().readonly());
}

#[cfg(unix)]
#[test]
fn a_partial_and_its_lock_never_open_a_link() {
    let (dir, cache) = super::tests::cache();
    let url = "https://example.org/data";
    let outside = dir.path().join("outside");
    fs::write(&outside, b"keep these bytes").unwrap();
    let folder = cache.root.join("partial");
    let part = folder.join(format!("{}.part", key(url)));
    let download = owned(&cache, url);
    let mut writer = download
        .writer(0, None, Validators::default(), false)
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
        .writer(0, None, Validators::default(), false)
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
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let digest = crate::digest::sha256(b"object");
    let kept = outside.join(digest.hex());
    fs::write(&kept, b"keep these bytes").unwrap();
    let folder = cache.root.join("changed");
    fs::create_dir_all(&folder).unwrap();
    fs::rename(&folder, cache.root.join("old-changed")).unwrap();
    link_folder(&outside, &folder);

    assert!(cache.mark_changed(&digest).is_err());
    assert!(cache.changed().is_empty());
    cache.forget_changed(&std::collections::HashSet::new());

    assert_eq!(fs::read(kept).unwrap(), b"keep these bytes");
}
