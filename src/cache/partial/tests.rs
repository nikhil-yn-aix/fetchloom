//! resuming a download, what an earlier run left behind, and one process per download.

use super::super::tests::{cache, fat, hardlinks, seal, store};
use super::*;

fn owned(cache: &Cache, url: &str) -> Partial {
    match cache.claim(url).unwrap() {
        Claim::Owned(partial) => partial,
        Claim::Busy => panic!("nothing else holds {url}"),
    }
}

#[test]
fn a_saved_partial_resumes_and_hashes_the_whole_file() {
    let (_dir, cache) = cache();
    let url = "https://example.org/big";
    let validators = Validators {
        etag: Some("\"v1\"".to_owned()),
        last_modified: None,
    };
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(6), validators.clone(), true)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    // bytes written after the last save are lost on a crash, so the record must win
    writer.write_all(b"XYZ").unwrap();
    drop(writer);
    let (received, seen) = partial.resumable().unwrap();
    assert_eq!(received, 3);
    assert_eq!(seen, validators);

    let mut resumed = partial.writer(received, Some(6), seen, true).unwrap();
    assert_eq!(resumed.written(), 3);
    resumed.write_all(b"def").unwrap();
    let stored = resumed.finish().unwrap();
    assert_eq!(fs::read(&stored.path).unwrap(), b"abcdef");
    assert_eq!(stored.size, 6);
    assert_eq!(
        stored.md5.map(|md5| md5.hex()).as_deref(),
        Some("e80b5017098950fc58aad83c8c14978e"),
        "md5 covers the resumed prefix too"
    );
    assert!(
        partial.resumable().is_none(),
        "a finished download leaves nothing to resume"
    );
}

#[test]
fn bytes_that_differ_from_their_record_are_never_continued() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/big");
    let mut writer = partial
        .writer(0, Some(6), Validators::default(), false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a power cut can keep the record and lose the bytes it counts
    fs::write(&partial.bytes, b"\0\0\0").unwrap();
    let (received, validators) = partial.resumable().unwrap();
    let Err(error) = partial.writer(received, Some(6), validators, false) else {
        panic!("zeros were continued as if they were abc");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_sealed_download_is_dropped_and_its_object_stays_read_only() {
    let (dir, cache) = cache();
    // FAT32 and exFAT cannot hardlink, so there a sealed download is never a second name of its
    // object, and a_sealed_download_is_dropped_on_fat covers them
    if !hardlinks(dir.path()) {
        return;
    }
    let url = "https://example.org/sealed";
    let object = store(&cache, b"abc");
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(3), Validators::default(), false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a run that died while publishing leaves its record and, sealed, a second name of the
    // object it was publishing
    fs::remove_file(&partial.bytes).unwrap();
    fs::hard_link(&object.path, &partial.bytes).unwrap();
    drop(partial);

    let partial = owned(&cache, url);
    assert!(partial.resumable().is_none());
    let mut writer = partial
        .writer(0, None, Validators::default(), false)
        .unwrap();
    writer.write_all(b"xyz").unwrap();
    assert_eq!(fs::read(&writer.finish().unwrap().path).unwrap(), b"xyz");
    assert_eq!(fs::read(&object.path).unwrap(), b"abc");
    assert!(fs::metadata(&object.path).unwrap().permissions().readonly());
}

#[test]
fn a_sealed_download_is_dropped_on_fat() {
    let (_dir, cache) = fat();
    let url = "https://example.org/sealed";
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(3), Validators::default(), false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a run that died while publishing leaves its record and the bytes it sealed
    seal(&partial.bytes);
    drop(partial);

    let partial = owned(&cache, url);
    assert!(partial.resumable().is_none());
    let mut writer = partial
        .writer(0, None, Validators::default(), false)
        .unwrap();
    writer.write_all(b"xyz").unwrap();
    assert_eq!(fs::read(&writer.finish().unwrap().path).unwrap(), b"xyz");
}

#[test]
fn a_download_belongs_to_one_process_at_a_time() {
    let (_dir, cache) = cache();
    let url = "https://example.org/shared";
    let first = owned(&cache, url);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Busy));
    assert!(matches!(
        cache.claim("https://example.org/other").unwrap(),
        Claim::Owned(_)
    ));
    drop(first);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
}

#[test]
fn a_writer_keeps_its_download_claimed_until_it_stops() {
    let (_dir, cache) = cache();
    let url = "https://example.org/held";
    let partial = owned(&cache, url);
    let writer = partial
        .writer(0, None, Validators::default(), false)
        .unwrap();
    // a stopped run drops its claim while a writer thread may still be writing
    drop(partial);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Busy));
    drop(writer);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
}

#[test]
fn a_published_object_can_be_read_while_its_download_is_still_claimed() {
    let (_dir, cache) = cache();
    let url = "https://example.org/a";
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, None, Validators::default(), false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    let stored = writer.finish().unwrap();
    assert_eq!(fs::read(&stored.path).unwrap(), b"abc");
    drop(partial);
    assert_eq!(store(&cache, b"abc").path, stored.path);
}
