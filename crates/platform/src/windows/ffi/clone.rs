#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE};

/// Shares the blocks of one file with another rather than writing them again.
pub(crate) fn duplicate_extents(
    source: &File,
    target: &File,
    at: u64,
    span: u64,
) -> io::Result<()> {
    let request = DUPLICATE_EXTENTS_DATA {
        FileHandle: source.as_raw_handle() as HANDLE,
        SourceFileOffset: i64::try_from(at).unwrap_or(i64::MAX),
        TargetFileOffset: i64::try_from(at).unwrap_or(i64::MAX),
        ByteCount: i64::try_from(span).unwrap_or(i64::MAX),
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

/// The largest region one clone may cover, which the specification states is
/// under four gigabytes.
pub(crate) const CLONE_CEILING: u64 = 4 * 1024 * 1024 * 1024 - 1;

/// Returns the spans a clone covers and how many bytes are left over.
///
/// The specification requires every cloned region to begin and end on a cluster
/// boundary and to be under four gigabytes, so an object whose length is not a
/// multiple of the cluster size is cloned up to the last whole cluster and the
/// remainder, which is under one cluster, is written.
pub(crate) fn clone_spans(length: u64, cluster: u64, ceiling: u64) -> (Vec<(u64, u64)>, u64) {
    if cluster == 0 || ceiling < cluster {
        return (Vec::new(), length);
    }
    let aligned = length - length % cluster;
    let step = ceiling - ceiling % cluster;
    let mut spans = Vec::new();
    let mut at = 0;
    while at < aligned {
        let span = step.min(aligned - at);
        spans.push((at, span));
        at += span;
    }
    (spans, length - aligned)
}

#[cfg(test)]
mod tests {
    use super::{CLONE_CEILING, clone_spans};

    #[test]
    fn a_length_that_is_a_whole_number_of_clusters_is_cloned_entirely() {
        let (spans, remainder) = clone_spans(4096 * 3, 4096, CLONE_CEILING);
        assert_eq!(spans, vec![(0, 4096 * 3)]);
        assert_eq!(remainder, 0);
    }

    #[test]
    fn a_length_that_is_not_leaves_the_last_partial_cluster_to_be_written() {
        let (spans, remainder) = clone_spans(4096 * 3 + 17, 4096, CLONE_CEILING);
        assert_eq!(spans, vec![(0, 4096 * 3)]);
        assert_eq!(remainder, 17);
        assert_eq!(
            spans.iter().map(|(_, span)| span).sum::<u64>() + remainder,
            4096 * 3 + 17,
            "the cloned spans and the remainder do not cover the object"
        );
    }

    #[test]
    fn a_length_under_one_cluster_is_written_rather_than_cloned() {
        let (spans, remainder) = clone_spans(100, 4096, CLONE_CEILING);
        assert!(spans.is_empty());
        assert_eq!(remainder, 100);
    }

    #[test]
    fn a_region_is_never_larger_than_the_ceiling_and_never_unaligned() {
        let cluster = 4096;
        let length = 10 * 1024 * 1024 * 1024 + 5;
        let (spans, remainder) = clone_spans(length, cluster, CLONE_CEILING);
        assert_eq!(remainder, 5);
        let mut at = 0;
        for (offset, span) in &spans {
            assert_eq!(*offset, at, "a span did not begin where the last one ended");
            assert_eq!(offset % cluster, 0, "a span began off a cluster boundary");
            assert_eq!(span % cluster, 0, "a span ended off a cluster boundary");
            assert!(*span <= CLONE_CEILING, "a span was larger than the ceiling");
            at += span;
        }
        assert_eq!(at + remainder, length, "the spans do not cover the object");
    }

    #[test]
    fn a_volume_that_states_no_cluster_size_is_written_rather_than_cloned() {
        let (spans, remainder) = clone_spans(8192, 0, CLONE_CEILING);
        assert!(spans.is_empty());
        assert_eq!(remainder, 8192);
    }
}
