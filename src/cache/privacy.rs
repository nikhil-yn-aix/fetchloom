use super::tests::cache;
use super::*;

const SECRET: &str = "wave1-record-secret";
const URL: &str =
    "https://example.org/data?token=wave1-record-secret#access_token=wave1-record-secret";

fn record() -> UrlRecord {
    UrlRecord {
        url: URL.to_owned(),
        sha256: digest::sha256(b"abcdef"),
        size: 6,
        name: Some("data.csv".to_owned()),
        validators: Validators {
            etag: Some("\"v1\"".to_owned()),
            ..Validators::default()
        },
    }
}

fn partial(cache: &Cache, url: &str) -> partial::Partial {
    let partial::Claim::Owned(partial) = cache.claim(url).unwrap() else {
        panic!("the fixture has no competing owner");
    };
    partial
}

fn checkpoint(cache: &Cache) -> partial::Partial {
    let partial = partial(cache, URL);
    let mut writer = partial
        .writer(0, Some(6), record().validators, false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    partial
}

fn checkpoint_path(cache: &Cache, url: &str, extension: &str) -> PathBuf {
    let key = digest::sha256(url.as_bytes()).hex();
    cache
        .partial_folder(&key)
        .join(format!("{key}.{extension}"))
}

#[test]
fn url_records_hide_the_url_without_losing_exact_lookup_identity() {
    let (_folder, cache) = cache();
    let record = record();
    cache.save_url_record(&record).unwrap();
    let path = cache.url_record_path(URL);
    let text = fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains(SECRET),
        "a URL-record secret was persisted: {text}"
    );
    assert!(
        !text.contains("https://"),
        "the metadata stores an identity, not a URL"
    );
    assert_eq!(cache.url_record(URL), Some(record));
    let other = "https://example.org/data?token=another-secret";
    fs::copy(path, cache.url_record_path(other)).unwrap();
    assert!(
        cache.url_record(other).is_none(),
        "a copied record cannot describe another URL"
    );
}

#[test]
fn partial_records_hide_the_url_and_keep_resume_validators_and_bytes() {
    let (_folder, cache) = cache();
    let partial = checkpoint(&cache);
    let text = fs::read_to_string(checkpoint_path(&cache, URL, "json")).unwrap();
    assert!(
        !text.contains(SECRET),
        "a partial-record secret was persisted: {text}"
    );
    let (received, validators) = partial.resumable().unwrap();
    assert_eq!(received, 3);
    assert_eq!(validators, record().validators);
    let mut writer = partial
        .writer(received, Some(6), validators, false, false)
        .unwrap();
    writer.write_all(b"def").unwrap();
    let stored = writer.finish().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdef"
    );
}

#[test]
fn a_partial_record_copied_to_another_url_cannot_resume() {
    let (_folder, cache) = cache();
    let _original = checkpoint(&cache);
    let other_url = "https://example.org/data?token=another-secret";
    let other = partial(&cache, other_url);
    for extension in ["part", "json"] {
        fs::copy(
            checkpoint_path(&cache, URL, extension),
            checkpoint_path(&cache, other_url, extension),
        )
        .unwrap();
    }
    assert!(other.resumable().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn a_secret_url_keeps_its_in_memory_deferred_publication_identity() {
    let (_folder, cache) = cache();
    assert!(cache.defer_downloads().unwrap());
    let partial = checkpoint(&cache);
    let (received, validators) = partial.resumable().unwrap();
    let mut writer = partial
        .writer(received, Some(6), validators, false, false)
        .unwrap();
    writer.write_all(b"def").unwrap();
    let stored = writer.finish().unwrap();
    assert!(cache.publish_download_of(URL).unwrap());
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdef"
    );
    cache.publish_downloads().unwrap();
}
