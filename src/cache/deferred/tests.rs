use super::super::Claim;
use super::super::tests::{cache, rewrite, store};
use super::*;
use crate::cache::Validators;
#[cfg(target_os = "linux")]
use crate::digest;

/// opens a batch: linux's own, or one without its flush elsewhere.
fn deferring(cache: &Cache) {
    #[cfg(target_os = "linux")]
    assert!(cache.defer_downloads().unwrap());
    #[cfg(not(target_os = "linux"))]
    cache.defer_unflushed();
}

fn finished(cache: &Cache, url: &str, bytes: &[u8]) -> Stored {
    let Claim::Owned(partial) = cache.claim(url).unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_at(0, bytes).unwrap();
    writer.finish().unwrap()
}

#[test]
fn a_long_batch_keeps_only_its_latest_downloads_claimed() {
    let (_dir, cache) = cache();
    deferring(&cache);
    let urls: Vec<String> = (0..GROUP + 10)
        .map(|n| format!("https://example.org/{n}"))
        .collect();
    let stored: Vec<Stored> = urls
        .iter()
        .map(|url| finished(&cache, url, url.as_bytes()))
        .collect();
    // each claimed download holds a file open, of which a process may have only so many
    let claimed = urls
        .iter()
        .filter(|url| matches!(cache.claim(url).unwrap(), Claim::Busy))
        .count();
    assert!(
        claimed < GROUP,
        "{claimed} finished downloads still hold their claims"
    );
    cache.publish_downloads().unwrap();
    for (url, stored) in urls.iter().zip(&stored) {
        assert_eq!(
            fs::read(cache.object_path(&stored.digest)).unwrap(),
            url.as_bytes()
        );
    }
}

#[test]
fn a_failure_in_an_early_group_is_reported_when_the_batch_ends() {
    let (_dir, cache) = cache();
    deferring(&cache);
    let first = crate::digest::sha256(b"blocked");
    // a file where the object's folder belongs keeps it from taking its name
    let folder = cache.object_path(&first).parent().unwrap().to_path_buf();
    fs::write(&folder, b"").unwrap();
    finished(&cache, "https://example.org/blocked", b"blocked");
    for n in 1..GROUP {
        let url = format!("https://example.org/{n}");
        finished(&cache, &url, url.as_bytes());
    }
    assert!(cache.publish_downloads().is_err());
}

#[test]
fn a_download_in_a_batch_that_repairs_a_changed_object_is_new() {
    let (_dir, cache) = cache();
    let right = store(&cache, b"right");
    let intact = store(&cache, b"intact");
    rewrite(&cache.object_path(&right.digest), b"wrong");
    deferring(&cache);
    assert!(finished(&cache, "https://example.org/repair", b"right").new);
    assert!(!finished(&cache, "https://example.org/intact", b"intact").new);
    cache.publish_downloads().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&right.digest)).unwrap(),
        b"right"
    );
    assert_eq!(
        fs::read(cache.object_path(&intact.digest)).unwrap(),
        b"intact"
    );
}

#[test]
fn without_a_batch_a_download_is_in_place_at_once() {
    let (_dir, cache) = cache();
    let stored = finished(&cache, "https://example.org/alone", b"alone");
    assert!(cache.object_path(&stored.digest).is_file());
    cache.publish_downloads().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_batch_names_its_downloads_only_after_one_flush() {
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads().unwrap());
    let first = finished(&cache, "https://example.org/first", b"first");
    let second = finished(&cache, "https://example.org/second", b"second");
    assert_eq!(first.digest, digest::sha256(b"first"));
    assert!(first.new, "the cache did not hold these bytes");
    for stored in [&first, &second] {
        assert!(
            !cache.object_path(&stored.digest).exists(),
            "no download takes its name before the flush"
        );
    }
    assert!(
        matches!(
            cache.claim("https://example.org/first").unwrap(),
            Claim::Busy
        ),
        "a waiting download stays claimed"
    );
    cache.publish_downloads().unwrap();
    for (stored, bytes) in [(&first, &b"first"[..]), (&second, &b"second"[..])] {
        assert_eq!(fs::read(cache.object_path(&stored.digest)).unwrap(), bytes);
    }
    assert!(matches!(
        cache.claim("https://example.org/first").unwrap(),
        Claim::Owned(_)
    ));
    let later = finished(&cache, "https://example.org/later", b"later");
    assert!(
        cache.object_path(&later.digest).is_file(),
        "after the batch, downloads are in place at once again"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn bytes_thrown_away_after_finishing_never_take_their_hash_in_the_batch() {
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads().unwrap());
    let url = "https://example.org/again";
    let Claim::Owned(partial) = cache.claim(url).unwrap() else {
        panic!("nothing else holds the download");
    };
    let write = |bytes: &[u8]| {
        let mut writer = partial
            .writer(0, None, Validators::default(), false, false)
            .unwrap();
        writer.write_at(0, bytes).unwrap();
        writer.finish().unwrap()
    };
    let bad = write(b"failed its check");
    // a hash check failed, so the download starts over at the same partial name
    partial.discard();
    let good = write(b"the right bytes");
    cache.publish_downloads().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&good.digest)).unwrap(),
        b"the right bytes"
    );
    assert!(!cache.object_path(&bad.digest).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn the_same_bytes_twice_in_a_batch_are_new_once() {
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads().unwrap());
    assert!(finished(&cache, "https://example.org/a", b"same").new);
    assert!(!finished(&cache, "https://example.org/b", b"same").new);
    cache.publish_downloads().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_second_download_of_the_same_link_puts_the_first_in_place_instead_of_waiting() {
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads().unwrap());
    let url = "https://example.org/twice";
    let stored = finished(&cache, url, b"twice");
    assert!(
        !cache
            .publish_download_of("https://example.org/other")
            .unwrap()
    );
    assert!(cache.publish_download_of(url).unwrap());
    assert!(cache.object_path(&stored.digest).is_file());
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
    cache.publish_downloads().unwrap();
}

#[test]
fn an_early_publication_keeps_a_failed_flush_sticky() {
    let (_dir, cache) = cache();
    deferring(&cache);
    let url = "https://example.org/after-failed-flush";
    let stored = finished(&cache, url, b"unflushed");
    *lock(&cache.deferred.publishing) = Publishing {
        failed: Some(io::Error::new(
            io::ErrorKind::StorageFull,
            "batch flush failed",
        )),
        unflushed: true,
    };

    let error = cache.publish_download_of(url).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert!(!cache.object_path(&stored.digest).exists());
    assert_eq!(
        cache.publish_downloads().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
}

#[test]
fn flushing_waiting_claims_keeps_the_batch_open() {
    let (_dir, cache) = cache();
    deferring(&cache);
    assert!(!cache.publish_waiting_downloads().unwrap());
    let first = finished(&cache, "https://example.org/first", b"first");
    assert!(cache.publish_waiting_downloads().unwrap());
    assert_eq!(
        fs::read(cache.object_path(&first.digest)).unwrap(),
        b"first"
    );
    let next = finished(&cache, "https://example.org/next", b"next");
    assert!(!cache.object_path(&next.digest).exists());
    cache.publish_downloads().unwrap();
    assert_eq!(fs::read(cache.object_path(&next.digest)).unwrap(), b"next");
}

#[cfg(target_os = "linux")]
#[test]
fn a_download_outside_the_batch_filesystem_flushes_itself() {
    use std::os::unix::fs::MetadataExt as _;

    let (_dir, cache) = cache();
    deferring(&cache);
    assert_ne!(
        fs::metadata("/proc").unwrap().dev(),
        fs::metadata(cache.root()).unwrap().dev()
    );
    lock(&cache.deferred.pending).as_mut().unwrap().batch =
        Arc::new(super::super::object::Batch::open(Path::new("/proc")).unwrap());

    let stored = finished(
        &cache,
        "https://example.org/another-filesystem",
        b"own flush",
    );

    assert!(
        cache.object_path(&stored.digest).is_file(),
        "a different filesystem cannot wait for this batch's flush"
    );
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"own flush"
    );
    cache.publish_downloads().unwrap();
}
