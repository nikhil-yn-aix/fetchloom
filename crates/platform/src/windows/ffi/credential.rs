#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Security::Credentials::{
    CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW,
};

const ERROR_NOT_FOUND: i32 = 1168;

pub(crate) fn read_credential(target: &str) -> io::Result<Option<Vec<u8>>> {
    let wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut held: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: the target is a NUL terminated wide string that outlives the call, and the out pointer is owned here and freed below.
    let ok = unsafe { CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &raw mut held) };
    if ok == 0 {
        let failure = io::Error::last_os_error();
        if failure.raw_os_error() == Some(ERROR_NOT_FOUND) {
            return Ok(None);
        }
        return Err(failure);
    }
    if held.is_null() {
        return Ok(None);
    }
    // SAFETY: the call succeeded and returned a non-null credential, so the blob pointer and its length are the ones the store wrote.
    let blob = unsafe {
        let credential = &*held;
        let length = credential.CredentialBlobSize as usize;
        if credential.CredentialBlob.is_null() || length == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(credential.CredentialBlob, length).to_vec()
        }
    };
    // SAFETY: the pointer came from a successful CredReadW and is freed exactly once here.
    unsafe { CredFree(held.cast::<c_void>()) };
    Ok(Some(blob))
}
