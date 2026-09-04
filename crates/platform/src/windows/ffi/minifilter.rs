#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;

/// One filter driver the platform has loaded.
pub(crate) struct LoadedFilter {
    /// The product's own name.
    pub(crate) name: String,
    /// Where the platform ordered it relative to other filters.
    pub(crate) altitude: u32,
}

/// Lists the filter drivers inspecting file operations on this machine.
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
