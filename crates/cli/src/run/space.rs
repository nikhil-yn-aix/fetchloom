//! What the volumes this run writes to have to hold, asked before it fetches.

use std::path::{Path, PathBuf};

use fetchloom_cache::Cache;
use fetchloom_engine::disk::room_for;
use fetchloom_engine::error::Error;
use fetchloom_engine::manifest::Manifest;
use fetchloom_engine::plan::{PlanDisk, VolumeRequirement};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_platform::NativePlatform;

fn volume_of(path: &Path) -> String {
    let mut components = path.components();
    match components.next() {
        Some(std::path::Component::Prefix(prefix)) => {
            prefix.as_os_str().to_string_lossy().into_owned()
        }
        Some(std::path::Component::RootDir) => "/".to_owned(),
        _ => ".".to_owned(),
    }
}

/// What the manifest states this run will bring down, counting only the
/// artifacts it states a size for and only those the cache does not already
/// hold. An artifact nobody sized is not counted, and a cache hit moves nothing.
fn bytes_to_fetch(manifest: &Manifest, cache: Option<&Cache<NativePlatform>>) -> u64 {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| {
            let held = artifact
                .digest
                .as_ref()
                .and_then(|stated| stated.blake3)
                .and_then(|digest| cache.map(|cache| cache.contains(digest).unwrap_or(false)));
            held != Some(true)
        })
        .filter_map(|artifact| artifact.size)
        .sum()
}

/// Refuses a run whose volumes cannot hold what it is about to fetch, before it
/// fetches any of it.
///
/// # Errors
/// `resource.disk` naming the volume, what the run needs there, and what it
/// holds.
pub(crate) fn confirm_room(
    platform: &NativePlatform,
    cache: Option<&Cache<NativePlatform>>,
    destination: &Path,
    manifest: &Manifest,
) -> Result<(), Error> {
    let bytes = bytes_to_fetch(manifest, cache);
    if bytes == 0 {
        return Ok(());
    }
    let cache_root: PathBuf = cache.map_or_else(
        || destination.to_path_buf(),
        |cache| cache.layout().root().to_path_buf(),
    );
    let disk = PlanDisk {
        partial: VolumeRequirement {
            volume: volume_of(&cache_root),
            bytes: Some(bytes),
        },
        cache: VolumeRequirement {
            volume: volume_of(&cache_root),
            bytes: Some(bytes),
        },
        staging: VolumeRequirement {
            volume: volume_of(destination),
            bytes: None,
        },
        destination: VolumeRequirement {
            volume: volume_of(destination),
            bytes: None,
        },
    };
    let free = |volume: &str| -> Option<u64> {
        let path = if volume == volume_of(&cache_root) {
            cache_root.as_path()
        } else {
            destination
        };
        platform.free_space(path).ok()
    };
    room_for(&disk, &free)
}
