//! Shared helpers for the cache contract tests.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    dead_code,
    reason = "test helpers, where each test binary uses a different part and a failure to build the input is the assertion"
)]

use std::io::Write;
use std::path::Path;

use fetchloom_cache::Cache;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::Error;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_platform::NativePlatform;
use serde_json as _;

/// Opens a cache under a directory, which is where every test puts one.
pub fn open_cache(under: &Path) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        under.join("cache"),
        NativePlatform::new(),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
    )
}

/// Opens a cache under a directory, failing the test when it cannot be opened.
pub fn cache_in(under: &Path) -> Cache<NativePlatform> {
    open_cache(under).unwrap()
}

/// A cache in a directory that is removed when the test ends.
pub fn cache() -> (tempfile::TempDir, Cache<NativePlatform>) {
    let scratch = tempfile::TempDir::new().unwrap();
    let held = cache_in(scratch.path());
    (scratch, held)
}

/// Bytes that differ from every other seed, so two objects are two digests.
pub fn bytes_of(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| {
            let index = u8::try_from(index % 251).unwrap_or(0);
            index.wrapping_mul(seed).wrapping_add(seed)
        })
        .collect()
}

/// Publishes bytes into a cache and returns their digest.
pub fn publish(into: &Cache<NativePlatform>, bytes: &[u8]) -> ContentDigest {
    let digest = hash_bytes(bytes);
    let lease = into.lease(digest).unwrap();
    let mut writer = into.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(bytes).unwrap();
    into.commit(lease, writer).unwrap();
    digest
}

/// Makes a published object writable, so a test can damage it.
pub fn make_writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "a test damaging an object needs exactly the permissions the platform gives a new file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
}

/// Opens a cache at an exact path.
pub fn open_cache_at(root: &Path) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        root,
        NativePlatform::new(),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
    )
}

/// Reports whether every object hashes to the name it is stored under.
pub fn digests_are_their_bytes(layout: &fetchloom_cache::layout::Layout) -> bool {
    let Ok(entries) = std::fs::read_dir(layout.objects()) else {
        return false;
    };
    for entry in entries.flatten() {
        let Ok(bytes) = std::fs::read(entry.path()) else {
            return false;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            return false;
        };
        if fetchloom_cache::layout::name_of(hash_bytes(&bytes)) != name {
            return false;
        }
    }
    true
}

/// Lists the entries in a directory, ignoring the owner records beside them.
pub fn entries_in(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_none_or(|kind| kind != "owner"))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Rewrites every owner record in the cache as though a previous boot wrote it.
///
/// A test cannot restart the machine, so the generation the records name is what
/// is changed, which is exactly what recovery reads.
pub fn pretend_a_previous_boot(layout: &fetchloom_cache::layout::Layout) {
    let _ = std::fs::remove_file(layout.recovered());
    for directory in [layout.partial(), layout.staging()] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|kind| kind != "owner") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut token: serde_json::Value = serde_json::from_str(&text).unwrap();
            token["boot"] = serde_json::Value::String("a-previous-boot".to_owned());
            std::fs::write(&path, serde_json::to_vec(&token).unwrap()).unwrap();
        }
    }
}

/// Writes an owner record naming a machine that is not this one.
pub fn write_foreign_owner(entry: &Path) {
    let mut name = entry.as_os_str().to_owned();
    name.push(".owner");
    let record = serde_json::json!({
        "machine": "another-machine",
        "boot": "another-boot",
        "pid": 1,
        "start": 1,
    });
    std::fs::write(
        std::path::PathBuf::from(name),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}
