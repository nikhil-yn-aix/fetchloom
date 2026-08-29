//! Shared helpers for the cache contract tests.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers, where a failure to build the input is the assertion"
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
