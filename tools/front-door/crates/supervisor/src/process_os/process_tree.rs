//! Narrow native edge for the host sampler and known-PID confined reader.
#[cfg(target_os = "macos")]
use crate::process_tree::{MAX_PROCESSES, ProcessMemory};
use crate::process_tree::{NativeProcess, Unavailable};

#[cfg(target_os = "macos")]
pub(crate) fn member(pid: u32, name: &std::ffi::CStr) -> Result<bool, Unavailable> {
    unsafe extern "C" {
        fn sandbox_check(
            pid: libc::pid_t,
            operation: *const libc::c_char,
            filter_type: libc::c_int,
            ...
        ) -> libc::c_int;
    }
    let pid = i32::try_from(pid).map_err(|_| Unavailable)?;
    // SAFETY: a null operation and filter 0 query only the scalar PID.
    let sandboxed = unsafe { sandbox_check(pid, std::ptr::null(), 0) };
    let lookup = |name: &std::ffi::CStr| {
        // SAFETY: filter 2 takes one NUL-terminated global-name pointer,
        // borrowed for the call; neither pointer is retained.
        unsafe { sandbox_check(pid, c"mach-lookup".as_ptr(), 2, name.as_ptr()) }
    };
    match (
        sandboxed,
        lookup(name),
        lookup(c"dev.provenant.attempt.control"),
    ) {
        (1, 0, 1) => Ok(true),
        (0..=1, 0..=1, 0..=1) => Ok(false),
        _ => Err(Unavailable),
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn process(pid: u32) -> Result<NativeProcess, Unavailable> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or(Unavailable)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_taskallinfo>::uninit();
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: info is aligned writable storage for exactly this ABI record;
    // libproc borrows it for the call and retains no pointer.
    let count = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKALLINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if count != size {
        return Err(Unavailable);
    }
    // SAFETY: a full successful read initialised the whole record.
    let info = unsafe { info.assume_init() };
    let bsd = info.pbsd;
    if bsd.pbi_pid != pid as u32 || bsd.pbi_pgid == 0 || bsd.pbi_start_tvusec >= 1_000_000 {
        return Err(Unavailable);
    }
    let start = bsd
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(bsd.pbi_start_tvusec))
        .ok_or(Unavailable)?;
    Ok(NativeProcess {
        zombie: bsd.pbi_status == libc::SZOMB,
        memory: ProcessMemory {
            pid: bsd.pbi_pid,
            ppid: bsd.pbi_ppid,
            pgid: bsd.pbi_pgid,
            rss_bytes: info.ptinfo.pti_resident_size,
        },
        start,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn children(pid: u32) -> Result<Vec<u32>, Unavailable> {
    unsafe extern "C" {
        fn proc_listchildpids(
            pid: libc::pid_t,
            buffer: *mut libc::c_void,
            size: libc::c_int,
        ) -> libc::c_int;
    }
    let pid = i32::try_from(pid).map_err(|_| Unavailable)?;
    let mut slots = 32;
    loop {
        let mut pids = vec![0_i32; slots];
        // SAFETY: __error supplies this thread's live errno slot; resetting it
        // distinguishes a legitimate empty list from libproc's zero-on-error.
        let errno = unsafe { libc::__error() };
        // SAFETY: errno is this thread's live errno slot, obtained above.
        unsafe {
            *errno = 0;
        }
        // SAFETY: pids supplies exactly the stated byte capacity of writable
        // storage; proc_listchildpids returns a PID count and retains nothing.
        let count = unsafe {
            proc_listchildpids(
                pid,
                pids.as_mut_ptr().cast(),
                (slots * std::mem::size_of::<i32>()) as i32,
            )
        };
        // SAFETY: __error returns this thread's live errno slot; copy only.
        let error = unsafe { *errno };
        if count < 0 || (count == 0 && error != 0) {
            return Err(Unavailable);
        }
        if count as usize >= slots {
            if slots > MAX_PROCESSES {
                return Err(Unavailable);
            }
            slots = (slots * 2).min(MAX_PROCESSES + 1);
            continue;
        }
        pids.truncate(count as usize);
        return pids
            .into_iter()
            .map(|pid| {
                u32::try_from(pid)
                    .ok()
                    .filter(|pid| *pid > 0)
                    .ok_or(Unavailable)
            })
            .collect();
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn member(_: u32, _: &std::ffi::CStr) -> Result<bool, Unavailable> {
    Err(Unavailable)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn process(_: u32) -> Result<NativeProcess, Unavailable> {
    Err(Unavailable)
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn children(_: u32) -> Result<Vec<u32>, Unavailable> {
    Err(Unavailable)
}
