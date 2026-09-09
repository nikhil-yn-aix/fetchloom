//! What a volume states about itself: its name and flags, whether it is
//! remote, what it has free, and how large a cluster is.

#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDiskFreeSpaceW, GetDriveTypeW, GetVolumeInformationByHandleW,
};

use super::encode::wide;

const DRIVE_REMOTE: u32 = 4;

pub(crate) struct VolumeInformation {
    pub(crate) max_component_length: u32,
    pub(crate) flags: u32,
}

pub(crate) fn volume_information(file: &File) -> io::Result<VolumeInformation> {
    let mut serial = 0u32;
    let mut max_component_length = 0u32;
    let mut flags = 0u32;
    // SAFETY: the handle is owned and open for the call, every out pointer addresses a live local, and both name buffers are null with a zero length, which the call accepts.
    let ok = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            &raw mut serial,
            &raw mut max_component_length,
            &raw mut flags,
            std::ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(VolumeInformation {
        max_component_length,
        flags,
    })
}

pub(crate) fn is_remote_drive(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\") {
        return true;
    }
    let Some(root) = path.components().next() else {
        return false;
    };
    let mut root = Path::new(root.as_os_str()).to_path_buf();
    root.push("");
    let wide_root = wide(&root);
    // SAFETY: the buffer is a NUL-terminated wide string that outlives the call.
    let kind = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
    kind == DRIVE_REMOTE
}

pub(crate) fn free_space(path: &Path) -> std::io::Result<u64> {
    let wide = wide(path);
    let mut available: u64 = 0;
    // SAFETY: the name is a NUL-terminated wide string that outlives the call, the out parameter is a u64 this frame owns, and the other two are optional and null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(available)
}

pub(crate) fn cluster_bytes(path: &Path) -> io::Result<u64> {
    let mut root = match path.components().next() {
        Some(first) => Path::new(first.as_os_str()).to_path_buf(),
        None => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
    };
    root.push("");
    let wide_root = wide(&root);
    let mut sectors_per_cluster = 0u32;
    let mut bytes_per_sector = 0u32;
    let mut free_clusters = 0u32;
    let mut total_clusters = 0u32;
    // SAFETY: the buffer is a NUL-terminated wide string that outlives the call, and every out pointer addresses a live local.
    let ok = unsafe {
        GetDiskFreeSpaceW(
            wide_root.as_ptr(),
            &raw mut sectors_per_cluster,
            &raw mut bytes_per_sector,
            &raw mut free_clusters,
            &raw mut total_clusters,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(sectors_per_cluster) * u64::from(bytes_per_sector))
}
