#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

//! The Windows calls the seam needs, each wrapped once.
//!
//! Every unsafe block here states the invariant it relies on. Opening a handle
//! is done through the standard library rather than through `CreateFileW`, so
//! the only unsafe surface is the queries and the operations themselves.

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use windows_sys::core::PWSTR;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateSymbolicLinkW, FILE_ALLOCATION_INFO, FILE_BASIC_INFO, FILE_END_OF_FILE_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FileAllocationInfo, FileBasicInfo, FileEndOfFileInfo,
    FileIdInfo, FlushFileBuffers, GetDriveTypeW, GetFileInformationByHandleEx,
    GetVolumeInformationByHandleW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    SYMBOLIC_LINK_FLAG_DIRECTORY, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE};
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JobObjectCpuRateControlInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, GetProcessAffinityMask,
    GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The drive type the platform reports for a volume reached over a network.
const DRIVE_REMOTE: u32 = 4;

/// The flag that permits an unprivileged process to create a symbolic link.
///
/// Named here because `windows-sys` types it as a symbolic link flag while the
/// call takes the two flags combined.
const ALLOW_UNPRIVILEGED_CREATE: u32 = 0x2;

/// Encodes a path the way every wide-character Windows call expects it.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Encodes text the way every wide-character Windows call expects it.
fn wide_text(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens a handle to a file or a directory for querying.
///
/// Takes any existing path. Returns an open handle, which backup semantics
/// makes legal for a directory as well as a file. Fails when the path cannot be
/// opened.
pub(crate) fn open_for_query(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Reads the identifier of a file and of the volume holding it.
///
/// Takes an open handle. Returns the volume serial number and the whole
/// sixteen-byte file identifier, which is the one unique on `ReFS`. Fails when
/// the platform refuses the query.
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

/// Reads the times the filesystem records for a file.
///
/// Takes an open handle. Returns the last write time and the change time, both
/// in the platform's own hundred-nanosecond units. Fails when the platform
/// refuses the query.
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

/// What the volume behind a handle reports about itself.
pub(crate) struct VolumeInformation {
    /// The longest single name component the volume accepts.
    pub(crate) max_component_length: u32,
    /// The capability flags the volume advertises.
    pub(crate) flags: u32,
}

/// Reads what a volume advertises about itself.
///
/// Takes a handle to any file or directory on the volume. Returns the longest
/// component it accepts and its capability flags. Fails when the platform
/// refuses the query.
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

/// Renames a file or a directory onto its final name.
///
/// Takes the two paths and whether the rename must reach the disk before it
/// returns. Replaces an existing target. Never copies across volumes, because
/// the flag that would permit that is never passed. Fails when the rename does
/// not complete.
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

/// Pushes a file's buffered bytes to the device.
///
/// Takes an open handle. Fails when the platform reports the flush did not
/// complete.
pub(crate) fn flush_file_buffers(file: &File) -> io::Result<()> {
    // SAFETY: the handle is owned and open for the call.
    let ok = unsafe { FlushFileBuffers(file.as_raw_handle() as HANDLE) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reserves clusters for a file and then sets its length.
///
/// Takes an open handle and the length to reserve. The allocation is set before
/// the end of file, because the end of file must never exceed the allocation.
/// Fails when the volume has no room.
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

/// Shares the blocks of one file with another rather than writing them again.
///
/// Takes the source and the already-created target, both open, and how many
/// bytes to share. Fails on every volume that does not reference-count blocks,
/// which is every volume except `ReFS` and a Dev Drive.
pub(crate) fn duplicate_extents(source: &File, target: &File, length: u64) -> io::Result<()> {
    let request = DUPLICATE_EXTENTS_DATA {
        FileHandle: source.as_raw_handle() as HANDLE,
        SourceFileOffset: 0,
        TargetFileOffset: 0,
        ByteCount: i64::try_from(length).unwrap_or(i64::MAX),
    };
    let size = u32::try_from(size_of::<DUPLICATE_EXTENTS_DATA>()).unwrap_or(u32::MAX);
    let mut returned = 0u32;
    // SAFETY: both handles are owned and open for the call, the input buffer is exactly the size passed, and no output buffer is requested.
    let ok = unsafe {
        DeviceIoControl(
            target.as_raw_handle() as HANDLE,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            std::ptr::from_ref(&request).cast::<c_void>(),
            size,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Creates a symbolic link with the given target.
///
/// Takes the target text and where the link goes. Fails when this process may
/// not create one, which covers a missing privilege, Developer Mode being off,
/// and a volume without reparse points.
pub(crate) fn create_symlink(target: &str, link: &Path, directory: bool) -> io::Result<()> {
    let target_wide = wide_text(target);
    let link_wide = wide(link);
    let mut flags = ALLOW_UNPRIVILEGED_CREATE;
    if directory {
        flags |= SYMBOLIC_LINK_FLAG_DIRECTORY;
    }
    // SAFETY: both buffers are NUL-terminated wide strings that outlive the call.
    let ok = unsafe { CreateSymbolicLinkW(link_wide.as_ptr(), target_wide.as_ptr(), flags) };
    if !ok {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reports whether a path sits on a volume reached over a network.
///
/// Takes any path. Returns whether the drive it names is a remote one, and
/// false when the drive cannot be determined.
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

/// Reads a string value from the local machine registry.
///
/// Takes the subkey and the value name. Returns the text, and nothing when the
/// value cannot be read.
pub(crate) fn registry_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = wide_text(subkey);
    let value = wide_text(value);
    let mut buffer = [0u16; 256];
    let mut size = u32::try_from(size_of_val(&buffer)).unwrap_or(u32::MAX);
    // SAFETY: both name buffers are NUL-terminated wide strings that outlive the call, and the size passed is the buffer's own size in bytes, so the kernel cannot write past it.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast::<c_void>(),
            &raw mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let characters = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..characters]))
}

/// Reads a numeric value from the local machine registry.
///
/// Takes the subkey and the value name. Returns the number, and nothing when
/// the value cannot be read.
pub(crate) fn registry_number(subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide_text(subkey);
    let value = wide_text(value);
    let mut found = 0u32;
    let mut size = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: both name buffers are NUL-terminated wide strings that outlive the call, and the size passed is the size of the single number behind the pointer.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut found).cast::<c_void>(),
            &raw mut size,
        )
    };
    if status != 0 {
        return None;
    }
    Some(found)
}

/// What a query about a process found.
pub(crate) enum ProcessQuery {
    /// The process exists and started at this instant.
    Started(u64),
    /// No process with that identifier exists.
    Gone,
    /// A process with that identifier exists and could not be inspected.
    Unreadable,
}

/// Reads when a process started.
///
/// Takes a process identifier. Returns the creation time in the platform's own
/// units, that no such process exists, or that one exists and cannot be
/// inspected, which is never treated as the process being gone.
pub(crate) fn process_start(pid: u32) -> ProcessQuery {
    // SAFETY: the call takes no pointer and returns a handle this function owns and closes.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(87) => ProcessQuery::Gone,
            _ => ProcessQuery::Unreadable,
        };
    }
    let mut creation = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut ignored = creation;
    // SAFETY: the handle was just opened and is closed below, and all four out pointers address live locals.
    let ok = unsafe {
        GetProcessTimes(
            handle,
            &raw mut creation,
            &raw mut ignored,
            &raw mut ignored,
            &raw mut ignored,
        )
    };
    // SAFETY: the handle was opened by this function and is not used again.
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return ProcessQuery::Unreadable;
    }
    let started = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    ProcessQuery::Started(started)
}

/// Counts the processors this process may actually run on.
///
/// Returns the smallest of the process affinity mask, any job object rate
/// limit, and the machine's own count across every processor group, because the
/// standard library's own answer reads none of those.
pub(crate) fn usable_processors() -> Option<usize> {
    let mut process_mask = 0usize;
    let mut system_mask = 0usize;
    // SAFETY: the pseudo handle for this process needs no closing, and both out pointers address live locals.
    let ok = unsafe {
        GetProcessAffinityMask(
            GetCurrentProcess(),
            &raw mut process_mask,
            &raw mut system_mask,
        )
    };
    let from_affinity = if ok == 0 {
        None
    } else {
        Some(process_mask.count_ones() as usize)
    };

    // SAFETY: the call takes no pointer.
    let across_groups = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } as usize;
    let across_groups = (across_groups > 0).then_some(across_groups);

    let mut control = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    let size =
        u32::try_from(size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>()).unwrap_or(u32::MAX);
    let mut returned = 0u32;
    // SAFETY: a null job handle asks about the job this process belongs to, and the buffer is exactly the size passed.
    let queried = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectCpuRateControlInformation,
            std::ptr::from_mut(&mut control).cast::<c_void>(),
            size,
            &raw mut returned,
        )
    };
    let from_job = if queried != 0 && control.ControlFlags & JOB_OBJECT_CPU_RATE_CONTROL_ENABLE != 0
    {
        // SAFETY: the enable flag is set, so the rate field of the union is the initialized one.
        let rate = unsafe { control.Anonymous.CpuRate };
        let machine = across_groups.unwrap_or(1);
        let allowed = (machine * rate as usize).div_ceil(10_000);
        Some(allowed.max(1))
    } else {
        None
    };

    [from_affinity, across_groups, from_job]
        .into_iter()
        .flatten()
        .min()
}

/// One filter driver the platform has loaded.
pub(crate) struct LoadedFilter {
    /// The product's own name.
    pub(crate) name: String,
    /// Where the platform ordered it relative to other filters.
    pub(crate) altitude: u32,
}

/// Lists the filter drivers inspecting file operations on this machine.
///
/// Returns each loaded filter's name and altitude, and an empty list when the
/// filter manager reports that none are registered. Returns nothing at all when
/// the filter manager cannot be asked, which is not the same answer as none
/// being loaded and is never treated as proof that none are.
pub(crate) fn loaded_minifilters() -> Option<Vec<LoadedFilter>> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS};
    use windows_sys::Win32::Storage::InstallableFileSystems::{
        FILTER_AGGREGATE_STANDARD_INFORMATION, FilterAggregateStandardInformation, FilterFindClose,
        FilterFindFirst, FilterFindNext,
    };

    const NO_MORE_ITEMS: i32 = win32_result(ERROR_NO_MORE_ITEMS);
    const INSUFFICIENT_BUFFER: i32 = win32_result(ERROR_INSUFFICIENT_BUFFER);

    let mut buffer = vec![0u8; 8192];
    let mut returned = 0u32;
    let mut find = std::ptr::null_mut();
    let mut size = u32::try_from(buffer.len()).unwrap_or(u32::MAX);

    // SAFETY: the buffer is at least the size passed, and the find handle is written to a live local that is closed below.
    let mut status = unsafe {
        FilterFindFirst(
            FilterAggregateStandardInformation,
            buffer.as_mut_ptr().cast::<c_void>(),
            size,
            &raw mut returned,
            &raw mut find,
        )
    };
    if status == INSUFFICIENT_BUFFER {
        buffer = vec![0u8; returned as usize];
        size = returned;
        // SAFETY: the buffer is at least the size passed, and the find handle is written to a live local that is closed below.
        status = unsafe {
            FilterFindFirst(
                FilterAggregateStandardInformation,
                buffer.as_mut_ptr().cast::<c_void>(),
                size,
                &raw mut returned,
                &raw mut find,
            )
        };
    }
    if status == NO_MORE_ITEMS {
        return Some(Vec::new());
    }
    if status != 0 {
        return None;
    }

    let mut found = Vec::new();
    loop {
        collect_filters(&buffer, &mut found);
        // SAFETY: the find handle is open until it is closed below, and the buffer is at least the size passed.
        let status = unsafe {
            FilterFindNext(
                find,
                FilterAggregateStandardInformation,
                buffer.as_mut_ptr().cast::<c_void>(),
                size,
                &raw mut returned,
            )
        };
        if status != 0 {
            break;
        }
    }
    // SAFETY: the find handle was opened by this function and is not used again.
    unsafe {
        FilterFindClose(find);
    }
    let _ = size_of::<FILTER_AGGREGATE_STANDARD_INFORMATION>();
    Some(found)
}

/// Builds the result value the filter manager returns for a Win32 error.
const fn win32_result(code: u32) -> i32 {
    i32::from_ne_bytes((0x8007_0000 | (code & 0xffff)).to_ne_bytes())
}

fn collect_filters(buffer: &[u8], found: &mut Vec<LoadedFilter>) {
    use windows_sys::Win32::Storage::InstallableFileSystems::FILTER_AGGREGATE_STANDARD_INFORMATION;

    let mut offset = 0usize;
    loop {
        if offset + size_of::<FILTER_AGGREGATE_STANDARD_INFORMATION>() > buffer.len() {
            return;
        }
        let base = buffer.as_ptr().wrapping_add(offset);
        // SAFETY: the offset plus the size of the record is within the buffer, and the read is unaligned because the kernel chose the offset.
        let record = unsafe {
            base.cast::<FILTER_AGGREGATE_STANDARD_INFORMATION>()
                .read_unaligned()
        };
        // SAFETY: the filter manager fills the minifilter arm of the union for every record this information class returns.
        let entry = unsafe { record.Type.MiniFilter };

        if let (Some(name), Some(altitude)) = (
            wide_at(
                buffer,
                offset,
                entry.FilterNameBufferOffset,
                entry.FilterNameLength,
            ),
            wide_at(
                buffer,
                offset,
                entry.FilterAltitudeBufferOffset,
                entry.FilterAltitudeLength,
            ),
        ) {
            let altitude = altitude
                .split('.')
                .next()
                .and_then(|whole| whole.parse::<u32>().ok());
            if let Some(altitude) = altitude {
                found.push(LoadedFilter { name, altitude });
            }
        }

        if record.NextEntryOffset == 0 {
            return;
        }
        offset += record.NextEntryOffset as usize;
    }
}

fn wide_at(buffer: &[u8], base: usize, offset: u16, length: u16) -> Option<String> {
    let start = base.checked_add(offset as usize)?;
    let end = start.checked_add(length as usize)?;
    let bytes = buffer.get(start..end)?;
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some(String::from_utf16_lossy(&wide))
}

/// Reads the user a file belongs to.
///
/// Takes an open handle. Returns the owner security identifier in its string
/// form. Fails when the platform refuses the query.
pub(crate) fn file_owner(file: &File) -> io::Result<String> {
    let mut sid: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is owned and open for the call, and both out pointers address locals the call fills in and nothing reads before it returns.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            std::ptr::from_mut(&mut sid),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_mut(&mut descriptor),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(-1),
        ));
    }
    let owner = sid_text(sid);
    // SAFETY: the descriptor was allocated by the call above and is freed once, and nothing borrows it afterwards.
    unsafe { LocalFree(descriptor.cast()) };
    owner
}

/// Reads the users a file this process creates can be owned by.
///
/// Returns the token's user and the token's owner as security identifiers in
/// their string form. The two differ when the process runs with an elevated
/// token, where a file it creates is owned by the administrators group rather
/// than by the user, and both of those are this process.
/// Fails when the platform refuses the query.
pub(crate) fn process_owners() -> io::Result<Vec<String>> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle for this process is always valid, and the out pointer addresses a local the call fills in.
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            std::ptr::from_mut(&mut token),
        )
    };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }

    let user = token_sid(token, TokenUser);
    let owner = token_sid(token, TokenOwner);
    // SAFETY: the token handle was opened above, is closed once, and nothing uses it afterwards.
    unsafe { CloseHandle(token) };

    let mut found = vec![user?];
    let owner = owner?;
    if !found.contains(&owner) {
        found.push(owner);
    }
    Ok(found)
}

/// Reads one security identifier out of an access token.
///
/// Both classes this is asked for answer with a record whose first field is the
/// identifier, so one reader serves both.
fn token_sid(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<String> {
    let mut needed = 0u32;
    // SAFETY: the token handle is open for the call, and a null buffer with a zero length is how the call is asked for the size it needs.
    unsafe {
        GetTokenInformation(
            token,
            class,
            std::ptr::null_mut(),
            0,
            std::ptr::from_mut(&mut needed),
        )
    };
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: the token handle is open for the call, and the buffer is at least the length passed, which the call itself reported.
    let read = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            std::ptr::from_mut(&mut needed),
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: the call reported it wrote a record beginning with a pointer to an identifier, and the read is unaligned because the buffer is bytes.
    let sid = unsafe { buffer.as_ptr().cast::<PSID>().read_unaligned() };
    sid_text(sid)
}
/// Renders a security identifier as the text form the platform defines.
fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text: PWSTR = std::ptr::null_mut();
    // SAFETY: the identifier came from a call that reported success, and the out pointer addresses a local the call fills in.
    let ok = unsafe { ConvertSidToStringSidW(sid, std::ptr::from_mut(&mut text)) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0usize;
    // SAFETY: the call above wrote a NUL terminated string, so the walk stops inside the allocation.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: the string holds the length just measured and lives until it is freed below.
    let rendered = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: the string was allocated by the call above and is freed once, and nothing borrows it afterwards.
    unsafe { LocalFree(text.cast()) };
    Ok(rendered)
}
