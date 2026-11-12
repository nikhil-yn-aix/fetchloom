use super::super::Claim;
use super::super::tests::{cache, rewrite, store};
use super::*;
use crate::cache::Validators;
#[cfg(target_os = "linux")]
use crate::digest;

/// opens a batch: linux's own, or one without its flush elsewhere.
fn deferring(cache: &Cache) {
    #[cfg(target_os = "linux")]
    assert!(cache.defer_downloads(None).unwrap());
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

#[cfg(target_os = "linux")]
#[test]
fn a_failed_batch_flush_never_names_its_downloads() {
    let (_dir, cache) = cache();
    deferring(&cache);
    // syncfs rejects an O_PATH handle, so this exercises a real flush error without disk faults.
    let file = rustix::fs::open(
        cache.root.join("tmp"),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    lock(&cache.deferred.pending).as_mut().unwrap().batch = Arc::new(
        super::super::object::Batch::with_file_for_test(std::fs::File::from(file)),
    );
    let stored = finished(&cache, "https://example.org/flush-error", b"unflushed");
    assert!(cache.publish_downloads().is_err());
    assert!(!cache.object_path(&stored.digest).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn a_latched_flush_failure_blocks_the_final_pending_group() {
    let (_dir, cache) = cache();
    deferring(&cache);
    let stored = finished(&cache, "https://example.org/later-group", b"later");
    *lock(&cache.deferred.publishing) = Publishing {
        failed: Some(io::ErrorKind::StorageFull.into()),
        unflushed: true,
    };
    assert_eq!(
        cache.publish_downloads().unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert!(
        !cache.object_path(&stored.digest).exists(),
        "a latched flush failure cannot publish later downloads"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn discarding_a_duplicate_updates_only_its_last_pending_digest() {
    let (_dir, cache) = cache();
    deferring(&cache);
    let pending = |url: &str, bytes: &[u8]| {
        let Claim::Owned(partial) = cache.claim(url).unwrap() else {
            panic!("the test owns every download");
        };
        let mut writer = partial
            .writer(0, None, Validators::default(), false, false)
            .unwrap();
        writer.write_at(0, bytes).unwrap();
        let stored = writer.finish().unwrap();
        (partial, stored)
    };
    let (first, first_stored) = pending("https://example.org/a", b"same");
    let (second, second_stored) = pending("https://example.org/b", b"same");
    assert!(first_stored.new);
    assert!(!second_stored.new);
    let other = finished(&cache, "https://example.org/c", b"other");
    first.discard();
    let (third, third_stored) = pending("https://example.org/d", b"same");
    assert!(!third_stored.new, "another copy still waits in the batch");
    second.discard();
    third.discard();
    let (_last, last) = pending("https://example.org/e", b"same");
    assert!(
        last.new,
        "no copy of these bytes remains in the batch or cache"
    );
    cache.publish_downloads().unwrap();
    assert_eq!(fs::read(cache.object_path(&last.digest)).unwrap(), b"same");
    assert_eq!(
        fs::read(cache.object_path(&other.digest)).unwrap(),
        b"other"
    );
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
    assert_eq!(
        lock(&cache.deferred.pending)
            .as_ref()
            .unwrap()
            .finished
            .len(),
        10
    );
    for stored in &stored[..GROUP] {
        assert!(cache.object_path(&stored.digest).is_file());
    }
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
    assert!(cache.defer_downloads(None).unwrap());
    let first = finished(&cache, "https://example.org/first", b"first");
    let second = finished(&cache, "https://example.org/second", b"second");
    let claim = Arc::downgrade(
        lock(&cache.deferred.pending).as_ref().unwrap().finished[0]
            .lock
            .as_ref()
            .unwrap(),
    );
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
    assert!(claim.upgrade().is_none());
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
    assert!(cache.defer_downloads(None).unwrap());
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
    assert!(cache.defer_downloads(None).unwrap());
    assert!(finished(&cache, "https://example.org/a", b"same").new);
    assert!(!finished(&cache, "https://example.org/b", b"same").new);
    cache.publish_downloads().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_second_download_of_the_same_link_puts_the_first_in_place_instead_of_waiting() {
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads(None).unwrap());
    let url = "https://example.org/twice";
    let stored = finished(&cache, url, b"twice");
    let claim = Arc::downgrade(
        lock(&cache.deferred.pending).as_ref().unwrap().finished[0]
            .lock
            .as_ref()
            .unwrap(),
    );
    assert!(
        !cache
            .publish_download_of("https://example.org/other")
            .unwrap()
    );
    assert!(cache.publish_download_of(url).unwrap());
    assert!(cache.object_path(&stored.digest).is_file());
    assert!(claim.upgrade().is_none());
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

/// a group that waits for more downloads than come takes its names once its first download
/// waited [`PUBLISH_WITHIN`], so another run that wants one of them does not wait for this run's
/// longest download, and not a moment before.
#[test]
fn a_group_takes_its_names_once_its_first_download_waited_long_enough() {
    let (_dir, cache) = cache();
    deferring(&cache);
    assert!(!cache.publish_due(Instant::now()).unwrap(), "nothing waits");
    let first = finished(&cache, "https://example.org/first", b"first");
    let since = lock(&cache.deferred.pending)
        .as_ref()
        .unwrap()
        .since
        .unwrap();
    let second = finished(&cache, "https://example.org/second", b"second");
    let early = since + PUBLISH_WITHIN.saturating_sub(Duration::from_millis(1));
    assert!(!cache.publish_due(early).unwrap());
    for stored in [&first, &second] {
        assert!(!cache.object_path(&stored.digest).exists());
    }
    assert!(cache.publish_due(since + PUBLISH_WITHIN).unwrap());
    for stored in [&first, &second] {
        assert!(cache.object_path(&stored.digest).is_file());
    }
    let third = finished(&cache, "https://example.org/third", b"third");
    let came = lock(&cache.deferred.pending)
        .as_ref()
        .unwrap()
        .since
        .unwrap();
    assert!(came >= since, "a later download waits from when it came");
    assert!(
        !cache
            .publish_due(came + PUBLISH_WITHIN.saturating_sub(Duration::from_millis(1)))
            .unwrap()
    );
    assert!(!cache.object_path(&third.digest).exists());
    cache.publish_downloads().unwrap();
    assert!(cache.object_path(&third.digest).is_file());
}

/// each download waiting for its group's flush keeps its claim's file open, so a process that
/// may open few files waits in smaller groups: a group of 256 ran out of files under a limit of
/// 256, as `add -i` of 1,000 links did.
#[cfg(target_os = "linux")]
#[test]
fn downloads_wait_in_groups_a_low_open_file_limit_can_hold() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    const CHILD: &str = "FETCHLOOM_TEST_FEW_OPEN_FILES";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cache::deferred::tests::downloads_wait_in_groups_a_low_open_file_limit_can_hold",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let limits = getrlimit(Resource::Nofile);
    let open_files = limits.maximum.map_or(256, |hard| hard.min(256));
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(open_files),
            maximum: limits.maximum,
        },
    )
    .unwrap();
    let (_dir, cache) = cache();
    assert!(cache.defer_downloads(Some(open_files)).unwrap());
    let urls: Vec<String> = (0..1_000)
        .map(|n| format!("https://example.org/{n}"))
        .collect();
    let stored: Vec<Stored> = urls
        .iter()
        .map(|url| finished(&cache, url, url.as_bytes()))
        .collect();
    cache.publish_downloads().unwrap();
    for (url, stored) in urls.iter().zip(&stored) {
        assert_eq!(
            fs::read(cache.object_path(&stored.digest)).unwrap(),
            url.as_bytes()
        );
    }
}
