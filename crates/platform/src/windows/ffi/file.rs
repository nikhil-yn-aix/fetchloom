#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ALLOCATION_INFO, FILE_END_OF_FILE_INFO, FileAllocationInfo, FileEndOfFileInfo,
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, SetFileInformationByHandle,
};

use super::encode::wide;

pub(crate) fn rename(from: &Path, to: &Path, write_through: bool) -> io::Result<()> {
    let source = wide(from);
    let target = wide(to);
    let mut flags = MOVEFILE_REPLACE_EXISTING;
    if write_through {
        flags |= MOVEFILE_WRITE_THROUGH;
    }
    // SAFETY: both buffers are NUL-terminated wide strings that outlive the call.
    let ok = unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), flags) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn preallocate(file: &File, length: u64) -> io::Result<()> {
    let signed = i64::try_from(length).unwrap_or(i64::MAX);
    let allocation = FILE_ALLOCATION_INFO {
        AllocationSize: signed,
    };
    let size = u32::try_from(size_of::<FILE_ALLOCATION_INFO>()).unwrap_or(u32::MAX);
    // SAFETY: the handle is owned and open for the call, and the buffer is exactly the size passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileAllocationInfo,
            std::ptr::from_ref(&allocation).cast::<c_void>(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }

    let end = FILE_END_OF_FILE_INFO { EndOfFile: signed };
    let size = u32::try_from(size_of::<FILE_END_OF_FILE_INFO>()).unwrap_or(u32::MAX);
    // SAFETY: the handle is owned and open for the call, and the buffer is exactly the size passed.
    let ok = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileEndOfFileInfo,
            std::ptr::from_ref(&end).cast::<c_void>(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn create_symlink(target: &str, link: &Path, directory: bool) -> io::Result<()> {
    if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
