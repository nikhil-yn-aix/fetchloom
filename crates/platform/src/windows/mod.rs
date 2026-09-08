//! The calls that differ on Windows.

mod ffi;
mod probe;

use std::fs::File;
use std::num::NonZeroUsize;
use std::path::Path;

use fetchloom_engine::capability::{Backing, InteropAcceleration, VectorLevel, VolumeCapabilities};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::identity::{BootId, FileId, Fingerprint, MachineId, VolumeId};

use fetchloom_engine::degrade::DegradeQueue;

use crate::ProcessState;

const WINDOWS_TO_UNIX_INTERVALS: i64 = 116_444_736_000_000_000;

pub(crate) const PATH_LENGTHS: [u32; 2] = [32_767, 260];

const MACHINE_KEY: &str = "SOFTWARE\\Microsoft\\Cryptography";

const BOOT_KEY: &str =
    "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Memory Management\\PrefetchParameters";

fn nanos_from_windows(value: i64) -> i128 {
    i128::from(value - WINDOWS_TO_UNIX_INTERVALS) * 100
}

pub(crate) fn volume_id(path: &Path) -> Result<VolumeId, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (serial, _) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    Ok(VolumeId::new(serial))
}

pub(crate) fn file_id(path: &Path) -> Result<FileId, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (_, id) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    Ok(FileId::new(id))
}

pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (serial, id) =
        ffi::id_info(&file).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let (written, changed) = ffi::basic_info(&file)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let size = file
        .metadata()
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?
        .len();
    Ok(Fingerprint {
        volume: VolumeId::new(serial),
        file: FileId::new(id),
        size,
        modified_nanos: nanos_from_windows(written),
        changed_nanos: nanos_from_windows(changed),
    })
}

pub(crate) fn volume_capabilities(
    probe_directory: &Path,
    degradations: &DegradeQueue,
    remembered_path_length: Option<u32>,
) -> Result<VolumeCapabilities, Error> {
    probe::capabilities(probe_directory, degradations, remembered_path_length)
}

pub(crate) fn detected_parallelism() -> NonZeroUsize {
    let standard = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let corrected = ffi::usable_processors().map_or(standard, |usable| usable.min(standard));
    NonZeroUsize::new(corrected.max(1)).unwrap_or(NonZeroUsize::MIN)
}

pub(crate) fn vector_level() -> VectorLevel {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::arch::is_x86_feature_detected!("avx512f") {
            VectorLevel::Avx512
        } else if std::arch::is_x86_feature_detected!("avx2") {
            VectorLevel::Avx2
        } else if std::arch::is_x86_feature_detected!("sse4.1") {
            VectorLevel::Sse41
        } else {
            VectorLevel::Portable
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        VectorLevel::Neon
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        VectorLevel::Portable
    }
}

pub(crate) fn interop_acceleration(degradations: &DegradeQueue) -> InteropAcceleration {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let _ = degradations;
        if std::arch::is_x86_feature_detected!("sha") {
            InteropAcceleration::Usable
        } else {
            InteropAcceleration::Absent
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        degradations.record(
            "the processor's own instruction for the interop digest",
            "the portable implementation",
            "this target has no runtime detection for that instruction, so it is never claimed",
        );
        InteropAcceleration::Undetectable
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = degradations;
        InteropAcceleration::Absent
    }
}

pub(crate) fn preallocate(
    file: &File,
    length: u64,
    _degradations: &DegradeQueue,
) -> Result<(), Error> {
    ffi::preallocate(file, length).map_err(|reason| {
        Error::new(
            fetchloom_engine::error::filesystem_kind(Surface::Cache, &reason),
            format!("free {length} bytes on the volume: {reason}"),
        )
    })
}

pub(crate) fn flush(
    file: &File,
    tier: DurabilityTier,
    _degradations: &DegradeQueue,
) -> Result<(), Error> {
    match tier {
        DurabilityTier::Strict | DurabilityTier::Normal => file.sync_all().map_err(|reason| {
            Error::new(
                fetchloom_engine::error::filesystem_kind(Surface::Cache, &reason),
                format!("{reason}"),
            )
        }),
        DurabilityTier::Fast => Ok(()),
    }
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the shape is the seam's, and the Linux side of it can fail"
)]
pub(crate) fn release_written(_file: &File, _from: u64, _length: u64) -> Result<bool, Error> {
    Ok(false)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the shape is the seam's, and the Unix side of it can fail"
)]
pub(crate) fn flush_directory(_directory: &Path, _tier: DurabilityTier) -> Result<(), Error> {
    Ok(())
}

pub(crate) fn rename(from: &Path, to: &Path, tier: DurabilityTier) -> Result<(), Error> {
    let write_through = matches!(tier, DurabilityTier::Strict | DurabilityTier::Normal);
    ffi::rename(from, to, write_through)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))
}

pub(crate) fn clone_file(from: &Path, to: &Path) -> Result<(), Error> {
    let source = File::open(from)
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?;
    let length = source
        .metadata()
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?
        .len();

    let information = ffi::open_for_query(from)
        .and_then(|handle| ffi::volume_information(&handle))
        .map_err(|reason| filesystem_failure(Surface::Destination, from, &reason))?;
    if information.flags
        & windows_sys::Win32::System::SystemServices::FILE_SUPPORTS_BLOCK_REFCOUNTING
        == 0
    {
        return Err(Error::new(
            ErrorKind::DestinationUnrepresentable,
            "this volume does not reference-count blocks, which only ReFS and a Dev Drive do"
                .to_owned(),
        ));
    }

    let target = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    ffi::preallocate(&target, length)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;

    let cluster = ffi::cluster_bytes(to)
        .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    let (spans, remainder) = ffi::clone_spans(length, cluster, ffi::CLONE_CEILING);
    if spans.is_empty() {
        return Err(Error::new(
            ErrorKind::DestinationUnrepresentable,
            format!(
                "copy these bytes rather than cloning them, because a clone begins and ends on a cluster boundary and this object is shorter than the {cluster} byte cluster this volume uses"
            ),
        ));
    }
    for (at, span) in spans {
        ffi::duplicate_extents(&source, &target, at, span)
            .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    }
    if remainder > 0 {
        write_tail(&source, &target, length - remainder, remainder)
            .map_err(|reason| filesystem_failure(Surface::Destination, to, &reason))?;
    }
    Ok(())
}

fn write_tail(source: &File, target: &File, at: u64, span: u64) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut reading = source;
    let mut writing = target;
    reading.seek(SeekFrom::Start(at))?;
    writing.seek(SeekFrom::Start(at))?;
    let mut bytes = vec![0_u8; usize::try_from(span).unwrap_or(0)];
    reading.read_exact(&mut bytes)?;
    writing.write_all(&bytes)
}

pub(crate) fn create_symlink(target: &[u8], link: &Path) -> Result<(), Error> {
    let text = std::str::from_utf8(target).map_err(|_| {
        Error::new(
            ErrorKind::DestinationUnrepresentable,
            "a symbolic link target must be valid Unicode on this platform".to_owned(),
        )
    })?;
    ffi::create_symlink(text, link, false)
        .map_err(|reason| filesystem_failure(Surface::Destination, link, &reason))
}

pub(crate) fn machine_id() -> Option<MachineId> {
    ffi::registry_string(MACHINE_KEY, "MachineGuid").map(MachineId::new)
}

pub(crate) fn boot_id() -> Option<BootId> {
    ffi::registry_number(BOOT_KEY, "BootId").map(|value| BootId::new(value.to_string()))
}

pub(crate) fn process_start(pid: u32) -> ProcessState {
    match ffi::process_start(pid) {
        ffi::ProcessQuery::Started(start) => ProcessState::Started(start),
        ffi::ProcessQuery::Gone => ProcessState::Gone,
        ffi::ProcessQuery::Unreadable => ProcessState::Unreadable,
    }
}

pub(crate) fn file_id_of(file: &File) -> Result<FileId, Error> {
    let (_, id) = ffi::id_info(file).map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("an open file does not report an identity: {reason}"),
        )
    })?;
    Ok(FileId::new(id))
}

pub(crate) fn owns(path: &Path) -> Result<bool, Error> {
    let file = ffi::open_for_query(path)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let found = ffi::file_owner(&file)
        .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
    let ours = ffi::process_owners().map_err(|reason| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!("this process does not report a user: {reason}"),
        )
    })?;
    Ok(ours.contains(&found))
}
pub(crate) fn volume_backing(path: &Path) -> Backing {
    if ffi::is_remote_drive(path) {
        Backing::Network
    } else {
        Backing::Local
    }
}

pub fn stored_token(host: &str) -> Result<Option<String>, Error> {
    let target = format!("fetchloom:{host}");
    let held = ffi::read_credential(&target).map_err(|reason| {
        Error::new(
            ErrorKind::PolicyCredentialInvalid,
            format!(
                "unlock the Windows Credential Manager and run the command again, because the credential stored under {target} could not be read: {reason}"
            ),
        )
    })?;
    held.map(|blob| decode(&blob, &target)).transpose()
}

fn decode(blob: &[u8], target: &str) -> Result<String, Error> {
    if !blob.len().is_multiple_of(2) {
        return Err(Error::new(
            ErrorKind::PolicyCredentialInvalid,
            format!(
                "store the credential under {target} again with cmdkey, because what is there is not the text its own tools write"
            ),
        ));
    }
    let wide: Vec<u16> = blob
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Ok(String::from_utf16_lossy(&wide)
        .trim_end_matches(char::from(0))
        .to_owned())
}

pub(crate) fn free_space(path: &Path) -> Result<u64, Error> {
    ffi::free_space(path).map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))
}
