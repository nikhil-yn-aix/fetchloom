use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub(super) fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub(super) fn wide_text(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
