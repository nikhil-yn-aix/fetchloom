#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::core::PWSTR;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenOwner, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

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
