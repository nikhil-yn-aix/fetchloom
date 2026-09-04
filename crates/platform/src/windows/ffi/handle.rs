#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FileBasicInfo, FileIdInfo,
    GetFileInformationByHandleEx,
};

pub(crate) fn open_for_query(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

pub(crate) fn id_info(file: &File) -> io::Result<(u64, u128)> {
    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: windows_sys::Win32::Storage::FileSystem::FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    let size = u32::try_from(size_of::<FILE_ID_INFO>()).unwrap_or(u32::MAX);
    // SAFETY: the handle is owned and open for the call, and the buffer is exactly the size passed, so the kernel cannot write past it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast::<c_void>(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.VolumeSerialNumber,
        u128::from_le_bytes(info.FileId.Identifier),
    ))
}

pub(crate) fn basic_info(file: &File) -> io::Result<(i64, i64)> {
    let mut info = FILE_BASIC_INFO {
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: 0,
    };
    let size = u32::try_from(size_of::<FILE_BASIC_INFO>()).unwrap_or(u32::MAX);
    // SAFETY: the handle is owned and open for the call, and the buffer is exactly the size passed, so the kernel cannot write past it.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileBasicInfo,
            std::ptr::from_mut(&mut info).cast::<c_void>(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((info.LastWriteTime, info.ChangeTime))
}
