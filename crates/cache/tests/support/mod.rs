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
use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::Error;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_platform::NativePlatform;
use serde_json as _;

pub fn open_cache(under: &Path) -> Result<Cache<NativePlatform>, Error> {
    open_cache_with(under, VerificationPolicy::Fingerprint)
}

pub fn open_cache_with(
    under: &Path,
    policy: VerificationPolicy,
) -> Result<Cache<NativePlatform>, Error> {
    open_cache_with_io(under, policy, IoMode::Buffered)
}

pub fn open_cache_with_io(
    under: &Path,
    policy: VerificationPolicy,
    io: IoMode,
) -> Result<Cache<NativePlatform>, Error> {
    open_cache_compressed(under, policy, io, CompressionChoice::Auto)
}

pub fn open_cache_compressed(
    under: &Path,
    policy: VerificationPolicy,
    io: IoMode,
    compression: CompressionChoice,
) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        under.join("cache"),
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        fetchloom_cache::CacheSettings {
            tier: DurabilityTier::Fast,
            policy,
            io,
            compression,
        },
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        processor(),
    )
}

pub fn cache_in(under: &Path) -> Cache<NativePlatform> {
    open_cache(under).unwrap()
}

/// A cache that stores every object raw, for tests about something other than
/// compression whose object has to be readable after its bytes are changed.
pub fn raw_cache() -> (tempfile::TempDir, Cache<NativePlatform>) {
    let scratch = tempfile::TempDir::new().unwrap();
    let held = open_cache_compressed(
        scratch.path(),
        VerificationPolicy::Fingerprint,
        IoMode::Buffered,
        CompressionChoice::None,
    )
    .unwrap();
    (scratch, held)
}

pub fn cache() -> (tempfile::TempDir, Cache<NativePlatform>) {
    let scratch = tempfile::TempDir::new().unwrap();
    let held = cache_in(scratch.path());
    (scratch, held)
}

pub fn bytes_of(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| {
            let index = u8::try_from(index % 251).unwrap_or(0);
            index.wrapping_mul(seed).wrapping_add(seed)
        })
        .collect()
}

pub fn publish(into: &Cache<NativePlatform>, bytes: &[u8]) -> ContentDigest {
    let digest = hash_bytes(bytes);
    let lease = into.lease(PartialKey::of_content(digest)).unwrap();
    let mut writer = into.begin(&lease, bytes.len() as u64).unwrap();
    writer.write_all(bytes).unwrap();
    into.commit(lease, writer).unwrap();
    digest
}

pub fn make_writable(path: &Path) {
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "a test damaging an object needs exactly the permissions the platform gives a new file"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).unwrap();
}

pub fn open_cache_at(root: &Path) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        root,
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        fetchloom_cache::CacheSettings {
            tier: DurabilityTier::Fast,
            policy: VerificationPolicy::Fingerprint,
            io: IoMode::Buffered,
            compression: CompressionChoice::Auto,
        },
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        processor(),
    )
}

pub fn digests_are_their_bytes(root: &Path) -> bool {
    let Ok(held) = open_cache_at(root) else {
        return false;
    };
    fetchloom_cache::verify::run(&held).is_ok_and(|report| report.quarantined.is_empty())
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Network,
    ReadOnly,
    Small,
    Second,
}

impl Property {
    fn variable(self) -> &'static str {
        match self {
            Self::Network => "FETCHLOOM_TEST_NETWORK_VOLUMES",
            Self::ReadOnly => "FETCHLOOM_TEST_READ_ONLY_VOLUMES",
            Self::Small => "FETCHLOOM_TEST_SMALL_VOLUMES",
            Self::Second => "FETCHLOOM_TEST_SECOND_VOLUMES",
        }
    }

    fn promised_here(self) -> bool {
        let linux = cfg!(target_os = "linux");
        match self {
            Self::Small | Self::Second => true,
            Self::ReadOnly => linux,
            Self::Network => false,
        }
    }
}

pub fn volumes(property: Property) -> Vec<std::path::PathBuf> {
    let named = std::env::var_os(property.variable()).unwrap_or_default();
    let found: Vec<std::path::PathBuf> = std::env::split_paths(&named)
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    assert!(
        !(found.is_empty()
            && std::env::var_os("FETCHLOOM_VERIFY_VOLUMES").is_some()
            && property.promised_here()),
        "{} is unset in a verification run that builds this filesystem",
        property.variable()
    );
    found
}

pub fn scratch_on(property: Property) -> Vec<tempfile::TempDir> {
    volumes(property)
        .iter()
        .map(|volume| {
            tempfile::TempDir::new_in(volume)
                .unwrap_or_else(|reason| panic!("{} is not writable: {reason}", volume.display()))
        })
        .collect()
}

pub fn link_directory(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
    }
}

pub fn another_owner() -> Option<String> {
    let named = std::env::var("FETCHLOOM_TEST_OTHER_OWNER").ok();
    assert!(
        !(named.is_none()
            && std::env::var_os("FETCHLOOM_VERIFY_VOLUMES").is_some()
            && cfg!(target_os = "linux")),
        "FETCHLOOM_TEST_OTHER_OWNER is unset in a verification run that creates the user"
    );
    named
}

pub fn give_away(path: &Path, owner: &str) {
    let status = std::process::Command::new("chown")
        .arg(owner)
        .arg(path)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "{} could not be given away",
        path.display()
    );
}

pub fn is_empty(directory: &std::path::Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none())
}

pub fn names(directory: &std::path::Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

pub fn a_source_record() -> fetchloom_engine::source_record::SourceRecord {
    fetchloom_engine::source_record::SourceRecord {
        location: fetchloom_engine::redact::SafeUrl::new("https://example.invalid/object"),
        host: "example.invalid".to_owned(),
        size: Some(4096),
        identity: fetchloom_engine::seam::source::SourceIdentity::StrongValidator(
            "\"one\"".to_owned(),
        ),
        etag: Some("\"one\"".to_owned()),
        last_modified: None,
        accepts_ranges: true,
        written: 4096,
        rung: fetchloom_engine::resume::ResumeRung::StrongValidator,
    }
}

pub fn processor() -> std::sync::Arc<fetchloom_engine::pool::Processor> {
    let budget = fetchloom_engine::threads::ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    std::sync::Arc::new(fetchloom_engine::pool::Processor::new(budget).unwrap())
}

pub fn damage(cache: &Cache<NativePlatform>, digest: ContentDigest, with: &[u8]) {
    use std::io::{Seek, SeekFrom, Write};

    let Some(placed) = cache.placement(digest) else {
        panic!("the cache holds no such object");
    };
    let container = placed.container().to_path_buf();
    make_writable(&container);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&container)
        .unwrap();
    let span = placed
        .length()
        .unwrap_or_else(|| file.metadata().unwrap().len() - placed.offset());
    let within = usize::try_from(span).unwrap_or(usize::MAX).min(with.len());
    file.seek(SeekFrom::Start(placed.offset())).unwrap();
    file.write_all(&with[..within]).unwrap();
}

#[derive(Debug, Default)]
pub struct Checked {
    seen: std::collections::BTreeSet<String>,
}

impl Checked {
    pub fn newly_published_are_their_bytes(&mut self, root: &Path) -> bool {
        let Ok(held) = open_cache_at(root) else {
            return false;
        };
        let Ok(digests) = held.list() else {
            return false;
        };
        for digest in digests {
            if !self.seen.insert(digest.to_string()) {
                continue;
            }
            if !matches!(held.object_is_its_digest(digest), Ok(true)) {
                return false;
            }
        }
        true
    }
}

pub fn scratch() -> tempfile::TempDir {
    tempfile::TempDir::new().unwrap()
}

/// Bytes no compressor can shrink, for a test that has to occupy a volume
/// rather than merely write to it. A cache that compresses what it stores will
/// never fill a small volume with a repeating pattern.
#[must_use]
pub fn incompressible(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "one byte of a 64 bit state is the point"
            )]
            {
                (state >> 24) as u8
            }
        })
        .collect()
}
