#![expect(
    unsafe_code,
    reason = "the platform seam is where Windows calls are made, and each block states its invariant"
)]

use std::ffi::c_void;
use std::io;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
    JobObjectCpuRateControlInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, GetProcessAffinityMask,
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

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
