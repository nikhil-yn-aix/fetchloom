#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;

use windows_sys::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};

use super::encode::wide_text;

/// Reads a string value from the local machine registry.
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
