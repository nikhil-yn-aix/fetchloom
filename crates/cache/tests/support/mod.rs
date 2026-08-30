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
use fetchloom_engine::partial_key::PartialKey;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_platform::NativePlatform;
use serde_json as _;

/// Opens a cache under a directory, which is where every test puts one.
///
/// # Errors
///
/// Fails when the cache cannot be opened.
pub fn open_cache(under: &Path) -> Result<Cache<NativePlatform>, Error> {
    open_cache_with(under, VerificationPolicy::Fingerprint)
}

/// Opens a cache under a directory with a given check on a hit.
///
/// # Errors
///
/// Fails when the cache cannot be opened.
pub fn open_cache_with(
    under: &Path,
    policy: VerificationPolicy,
) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        under.join("cache"),
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        policy,
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        processor(),
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
    let lease = into.lease(PartialKey::of_content(digest)).unwrap();
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
///
/// # Errors
///
/// Fails when the cache cannot be opened.
pub fn open_cache_at(root: &Path) -> Result<Cache<NativePlatform>, Error> {
    Cache::open(
        root,
        NativePlatform::new(std::sync::Arc::new(
            fetchloom_engine::work::WorkCounter::new(),
        )),
        DurabilityTier::Fast,
        VerificationPolicy::Fingerprint,
        std::sync::Arc::new(fetchloom_engine::work::WorkCounter::new()),
        processor(),
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

/// A property a test needs from a volume, rather than a filesystem name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    /// The volume is reached over a network protocol.
    Network,
    /// The volume cannot be written to.
    ReadOnly,
    /// The volume is small enough to fill.
    Small,
    /// The volume is not the one the temporary directory is on.
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

/// Every volume the environment offers with the given property.
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

/// A directory inside each volume offering the given property.
pub fn scratch_on(property: Property) -> Vec<tempfile::TempDir> {
    volumes(property)
        .iter()
        .map(|volume| {
            tempfile::TempDir::new_in(volume)
                .unwrap_or_else(|reason| panic!("{} is not writable: {reason}", volume.display()))
        })
        .collect()
}

/// Points a name at a directory on another volume.
///
/// Returns whether the platform allowed it, because a symbolic link needs a
/// privilege on one of the three.
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

/// The user a test hands an object to, when the environment names one.
///
/// The volume script names a user this process can give a file to, which is
/// what makes the ownership rule in contracts.md Cache testable at all. A
/// verification run on Linux promised that user, so an absent name there is a
/// provisioning failure rather than a reason to skip.
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

/// Gives a file to another user through the platform s own tool.
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

/// Reports whether a directory holds no entries at all.
pub fn is_empty(directory: &std::path::Path) -> bool {
    std::fs::read_dir(directory).is_ok_and(|mut entries| entries.next().is_none())
}

/// Lists the names a directory holds, sorted.
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

/// A source record a test writes beside a partial.
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

/// The processor pool every cache in a test is opened with.
pub fn processor() -> std::sync::Arc<fetchloom_engine::pool::Processor> {
    let budget = fetchloom_engine::threads::ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    std::sync::Arc::new(fetchloom_engine::pool::Processor::new(budget).unwrap())
}
