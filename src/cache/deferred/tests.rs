use super::super::Claim;
use super::super::tests::cache;
use super::*;
use crate::cache::Validators;
#[cfg(target_os = "linux")]
use crate::digest;

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
