use std::ffi::OsString;
use std::io;
use std::mem::{MaybeUninit, size_of};

pub struct DarwinBsd {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub uid: i64,
    pub status: u32,
    pub device: u32,
    pub start_sec: u64,
    pub start_usec: u64,
    pub command: String,
}

pub fn darwin_bsd(pid: i32) -> Option<DarwinBsd> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: flavour 3 writes proc_bsdinfo, with its exact ABI size and a live
    // aligned exclusive buffer. Only an exact successful read is initialised.
    // libproc retains no pointer/handle; concurrent observations are independent.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            3,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if read != size_of::<libc::proc_bsdinfo>() as i32 {
        return None;
    }
    // SAFETY: the exact successful read above initialised all fields, with no
    // pointers or invalid Rust enum values in this C record.
    let info = unsafe { info.assume_init() };
    let decode = |chars: &[libc::c_char]| {
        let bytes: Vec<_> = chars
            .iter()
            .take_while(|&&ch| ch != 0)
            .map(|&ch| ch as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let mut command = decode(&info.pbi_name);
    if command.is_empty() {
        command = decode(&info.pbi_comm);
    }
    Some(DarwinBsd {
        pid,
        ppid: info.pbi_ppid as i32,
        pgid: info.pbi_pgid as i32,
        uid: info.pbi_uid.into(),
        status: info.pbi_status,
        device: info.e_tdev,
        start_sec: info.pbi_start_tvsec,
        start_usec: info.pbi_start_tvusec,
        command,
    })
}

pub struct DarwinTask {
    pub cpu_seconds: f64,
    pub rss_kib: i64,
    pub running: bool,
}

pub fn darwin_task(pid: i32) -> Result<Option<DarwinTask>, crate::process_info::ProcessParseError> {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::uninit();
    // SAFETY: flavour 4 and size match the live exclusive ABI buffer. An exact
    // return initialises this pointer-free C record, and no storage is retained.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            4,
            0,
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_taskinfo>() as i32,
        )
    };
    if read != size_of::<libc::proc_taskinfo>() as i32 {
        return Ok(None);
    }
    // SAFETY: the complete read above initialised every field of the C record.
    let info = unsafe { info.assume_init() };
    Ok(Some(DarwinTask {
        cpu_seconds: task_cpu_seconds(
            info.pti_total_user,
            info.pti_total_system,
            mach_tick_seconds(),
        )?,
        rss_kib: (info.pti_resident_size / 1024) as i64,
        running: info.pti_numrunning > 0,
    }))
}

fn task_cpu_seconds(
    user: u64,
    system: u64,
    tick_seconds: f64,
) -> Result<f64, crate::process_info::ProcessParseError> {
    let total = user.checked_add(system).ok_or(
        crate::process_info::ProcessParseError::NumberOutOfRange("task CPU ticks"),
    )?;
    Ok(total as f64 * tick_seconds)
}

#[cfg(test)]
mod task_tests {
    use super::*;

    #[test]
    fn cpu_ticks_are_added_exactly_before_float_conversion() {
        let actual = task_cpu_seconds((1 << 53) + 1, 1, 1.0).unwrap();
        assert_eq!(actual.to_bits(), 9007199254740994_f64.to_bits());
        assert_eq!(
            task_cpu_seconds(u64::MAX, 1, 1.0).unwrap_err(),
            crate::process_info::ProcessParseError::NumberOutOfRange("task CPU ticks")
        );
    }
}

// libc deprecates this in favour of a dependency outside the allowlist.
#[allow(deprecated)]
pub fn mach_tick_seconds() -> f64 {
    let mut timebase = MaybeUninit::<libc::mach_timebase_info>::uninit();
    // SAFETY: the kernel writes the exact pointer-free C struct to a live
    // aligned exclusive buffer, retains no pointer, and returns a status.
    if unsafe { libc::mach_timebase_info(timebase.as_mut_ptr()) } != 0 {
        return 1e-9;
    }
    // SAFETY: successful mach_timebase_info initialises both fields.
    let timebase = unsafe { timebase.assume_init() };
    if timebase.denom == 0 {
        1e-9
    } else {
        f64::from(timebase.numer) / f64::from(timebase.denom) / 1e9
    }
}

pub fn darwin_pids() -> io::Result<Vec<i32>> {
    // SAFETY: null/zero queries capacity only, with no memory or handles retained.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    let size = (count as usize)
        .checked_add(128)
        .and_then(|count| count.checked_mul(size_of::<i32>()))
        .filter(|&size| size <= i32::MAX as usize)
        .ok_or_else(|| io::Error::other("process census too large"))?;
    let mut pids = vec![0_i32; size / size_of::<i32>()];
    // SAFETY: the byte size is exactly the writable live vector's allocation;
    // libproc retains no pointer and only writes integral PIDs. Growth between
    // calls is truncated to capacity before exposing the result.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size as i32) };
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    pids.truncate(count as usize);
    Ok(pids)
}

fn sysctl_bytes(mib: &mut [i32], limit: usize) -> io::Result<Vec<u8>> {
    let mut size = 0;
    // SAFETY: the live integral mib and size pointer have the documented sizes.
    // Null oldp queries only; no new value is supplied and nothing is retained.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size > limit {
        return Err(io::Error::other("process observation too large"));
    }
    let mut data = vec![0_u8; size];
    // SAFETY: size bounds the exclusive writable vector and mib stays live.
    // sysctl updates size and retains no pointers. No writes to kernel state.
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            data.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if size > data.len() {
        return Err(io::Error::other("process observation grew"));
    }
    data.truncate(size);
    Ok(data)
}

pub fn darwin_command(pid: i32) -> OsString {
    sysctl_bytes(&mut [1, 49, pid], 1_048_576)
        .ok()
        .map(|raw| crate::process_info::darwin_argv(&raw))
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq)]
pub struct DarwinFacts {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    pub uid: u32,
    pub start_epoch: f64,
}

/// The accepted 64-bit SDK layout, shared by arm64 and x86_64. Decode bytes
/// rather than dereferencing potentially unaligned C fields.
pub fn darwin_facts_bytes(pid: i32, raw: &[u8]) -> Option<DarwinFacts> {
    if raw.len() < 648 {
        return None;
    }
    let integer = |offset| {
        raw.get(offset..offset + 4)
            .and_then(|b| b.try_into().ok())
            .map(i32::from_ne_bytes)
    };
    let wide = |offset| {
        raw.get(offset..offset + 8)
            .and_then(|b| b.try_into().ok())
            .map(i64::from_ne_bytes)
    };
    if integer(40)? != pid {
        return None;
    }
    let sec = wide(0)?;
    if sec <= 0 {
        return None;
    }
    Some(DarwinFacts {
        pid,
        ppid: integer(560)?,
        pgid: integer(564)?,
        uid: integer(420)? as u32,
        start_epoch: sec as f64 + wide(8)? as f64 / 1_000_000.0,
    })
}

pub fn darwin_facts(pid: i32) -> Option<DarwinFacts> {
    if !cfg!(any(target_arch = "aarch64", target_arch = "x86_64")) {
        return None;
    }
    let data = sysctl_bytes(&mut [1, 14, 1, pid], 648).ok()?;
    darwin_facts_bytes(pid, &data)
}
