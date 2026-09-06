//! Filling a staging directory beside a destination, and publishing it whole.

use super::context::Materialization;
use super::paths::{containing_directory, staging_beside};
use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::tree::TreeEntry;
use std::path::Path;

pub(crate) type Fill<'a> = &'a dyn Fn(&Path) -> Result<Vec<TreeEntry>, Error>;

pub(crate) fn open_staging(
    destination: &Path,
    with: &Materialization<'_>,
) -> Result<std::path::PathBuf, Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;
    Ok(staging)
}

pub(crate) fn publish(
    with: &Materialization<'_>,
    staging: &Path,
    destination: &Path,
) -> Result<(), Error> {
    {
        let parent = containing_directory(destination);
        with.platform.create_directories(&parent)?;
    }
    if let Err(error) = with
        .platform
        .publish_directory(staging, destination, with.durability)
    {
        let _ = std::fs::remove_dir_all(staging);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn stage_and_publish(
    with: &Materialization<'_>,
    destination: &Path,
    fill: Fill<'_>,
) -> Result<Vec<TreeEntry>, Error> {
    let staging = open_staging(destination, with)?;
    let entries = match fill(&staging) {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    publish(with, &staging, destination)?;
    Ok(entries)
}
