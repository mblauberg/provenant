//! Kernel event and process facts. No signalling authority lives here.
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};

pub fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is borrowed from a live owner; F_GETFL retains no pointer.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` remains owned by its caller; F_SETFL changes only its flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub fn prepare_child(
    command: &mut std::process::Command,
    env: &BTreeMap<String, String>,
) -> io::Result<()> {
    let env: Vec<_> = env
        .iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    super::super::child::configure_command(command, &[], Some(&env), true)
}
const SIGNALS: [i32; 4] = [libc::SIGCHLD, libc::SIGTERM, libc::SIGINT, libc::SIGHUP];
static SIGNAL_FD: AtomicI32 = AtomicI32::new(-1);
extern "C" fn signal_event(sig: i32) {
    let fd = SIGNAL_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        #[cfg(target_os = "macos")]
        // SAFETY: __error returns the current thread's live errno slot.
        let errno = unsafe { libc::__error() };
        #[cfg(target_os = "linux")]
        // SAFETY: __errno_location returns the current thread's live errno slot.
        let errno = unsafe { libc::__errno_location() };
        // SAFETY: `errno` points to the current thread's live errno slot.
        let saved = unsafe { *errno };
        // SAFETY: the byte is live for this one-byte async-signal-safe write;
        // the registered self-pipe fd is process-owned and nonblocking.
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
        // SAFETY: `errno` still points to this thread's live errno slot.
        unsafe { *errno = saved };
    }
}
pub struct Reactor {
    read: File,
    _write: File,
    old: Vec<(i32, libc::sigaction)>,
    old_mask: libc::sigset_t,
    #[cfg(target_os = "macos")]
    queue: File,
}
impl Reactor {
    pub fn new() -> io::Result<Self> {
        // Inherited SIG_IGN would auto-reap and release the group identity.
        // SAFETY: signal takes scalar values and changes this supervisor's
        // process disposition before its lane spawns children.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) };
        // SAFETY: signal takes scalar values and installs the intended SIGPIPE
        // disposition for this supervisor process.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        let mut fds = [0; 2];
        // SAFETY: `fds` points to two writable integers; pipe initialises both
        // entries on success and retains no pointer.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe returned this unique read descriptor; File assumes its
        // ownership exactly once.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        // SAFETY: pipe returned this unique write descriptor; File assumes its
        // ownership exactly once.
        let write = unsafe { File::from_raw_fd(fds[1]) };
        for fd in fds {
            // SAFETY: `fd` is one of the live descriptors just transferred to
            // these Files; F_SETFD retains ownership.
            let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            nonblocking(fd)?;
        }
        SIGNAL_FD.store(write.as_raw_fd(), Ordering::Relaxed);
        let mut old = Vec::new();
        for sig in SIGNALS {
            // SAFETY: zero is a valid initial representation for sigaction;
            // sigemptyset below initialises its mask before use.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = signal_event as *const () as usize;
            // SAFETY: `action.sa_mask` is live writable storage for sigemptyset.
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            action.sa_flags = libc::SA_RESTART;
            // SAFETY: zero is a valid output buffer for sigaction to fill.
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: both action records are live; the kernel copies the new
            // action and writes the previous one without retaining pointers.
            if unsafe { libc::sigaction(sig, &action, &mut previous) } != 0 {
                return Err(io::Error::last_os_error());
            }
            old.push((sig, previous));
        }
        // SAFETY: zero is a valid initial representation; sigemptyset below
        // initialises this live mask before it is passed to libc.
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: `mask` is live writable storage for sigemptyset.
        unsafe { libc::sigemptyset(&mut mask) };
        for sig in SIGNALS {
            // SAFETY: `mask` is initialized live storage and `sig` is a signal
            // number from the fixed supported set.
            unsafe { libc::sigaddset(&mut mask, sig) };
        }
        // SAFETY: mask is initialized and old_mask is a live output record;
        // sigprocmask retains neither pointer.
        let mut old_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: the initialized mask updates this process's thread mask and
        // writes its prior value into live `old_mask` storage.
        unsafe { libc::sigprocmask(libc::SIG_UNBLOCK, &mask, &mut old_mask) };
        #[cfg(target_os = "macos")]
        let queue = {
            // SAFETY: kqueue takes no pointers and returns a new owned fd.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `fd` is the unique descriptor just returned by kqueue.
            let queue = unsafe { File::from_raw_fd(fd) };
            // SAFETY: `fd` remains live in `queue`; F_SETFD retains ownership.
            let _ = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            // Register SIGCHLD before any spawn. The pipe also preserves signals
            // delivered while a different kernel operation is in progress.
            for sig in SIGNALS {
                let ev = libc::kevent {
                    ident: sig as usize,
                    filter: libc::EVFILT_SIGNAL,
                    flags: libc::EV_ADD,
                    fflags: 0,
                    data: 0,
                    udata: std::ptr::null_mut(),
                };
                // SAFETY: `ev` is a live initialized change record; the queue
                // copies it and no output storage is requested.
                if unsafe { libc::kevent(fd, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) }
                    < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            queue
        };
        Ok(Self {
            read,
            _write: write,
            old,
            old_mask,
            #[cfg(target_os = "macos")]
            queue,
        })
    }
    pub fn watch_child(&self, pid: i32) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let ev = libc::kevent {
                ident: pid as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            // SAFETY: `ev` is a live initialized change record; the queue
            // copies it and no output buffer is supplied.
            if unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    &ev,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            } < 0
            {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::ESRCH) {
                    return Err(e);
                }
                // ESRCH and NOTE_EXIT converge on the same WNOWAIT check.
            }
        }
        let _ = pid;
        Ok(())
    }
    pub fn wait(&self, fds: &[(RawFd, bool)], millis: i32) -> io::Result<Vec<i32>> {
        #[cfg(target_os = "macos")]
        {
            let mut changes = Vec::new();
            for (fd, write) in std::iter::once(&(self.read.as_raw_fd(), false)).chain(fds.iter()) {
                changes.push(libc::kevent {
                    ident: *fd as usize,
                    filter: if *write {
                        libc::EVFILT_WRITE
                    } else {
                        libc::EVFILT_READ
                    },
                    flags: libc::EV_ADD | libc::EV_ONESHOT,
                    fflags: 0,
                    data: 0,
                    udata: std::ptr::null_mut(),
                });
            }
            // SAFETY: zero is a valid empty event record; kevent writes only
            // the initialized entries reported by its return value.
            let mut events: [libc::kevent; 32] = unsafe { std::mem::zeroed() };
            let timeout = libc::timespec {
                tv_sec: (millis / 1000) as _,
                tv_nsec: ((millis % 1000) * 1_000_000) as _,
            };
            // SAFETY: changes and events remain live for the call, lengths
            // match their arrays, and `timeout` is a valid relative duration.
            let rc = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    changes.as_ptr(),
                    changes.len() as i32,
                    events.as_mut_ptr(),
                    32,
                    &timeout,
                )
            };
            if rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(target_os = "linux")]
        {
            let mut pollfds = vec![libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            pollfds.extend(fds.iter().map(|(fd, write)| libc::pollfd {
                fd: *fd,
                events: if *write { libc::POLLOUT } else { libc::POLLIN },
                revents: 0,
            }));
            // SAFETY: pollfds is live writable storage for the supplied count;
            // it contains borrowed descriptors and poll retains no pointers.
            let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, millis) };
            if rc < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
        }
        let mut signals = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            // SAFETY: `buf` is live writable storage and `self.read` owns a
            // valid pipe descriptor for the duration of the call.
            let n =
                unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            signals.extend(buf[..n as usize].iter().map(|b| *b as i32));
        }
        Ok(signals)
    }
}
impl Drop for Reactor {
    fn drop(&mut self) {
        SIGNAL_FD.store(-1, Ordering::Relaxed);
        for (sig, old) in &self.old {
            // SAFETY: `old` is the live disposition saved for this signal;
            // sigaction copies it and writes no previous action.
            unsafe { libc::sigaction(*sig, old, std::ptr::null_mut()) };
        }
        // SAFETY: old_mask is the live mask saved by Reactor::new; the kernel
        // copies it and no output pointer is requested.
        unsafe { libc::sigprocmask(libc::SIG_SETMASK, &self.old_mask, std::ptr::null_mut()) };
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub pid: i32,
    pub start: u64,
}
#[derive(Clone, Debug)]
pub struct Row {
    pub id: Identity,
    pub ppid: i32,
    pub pgid: i32,
    pub command: String,
    pub zombie: bool,
    pub epoch: u64,
    pub cpu: u64,
}
#[cfg(target_os = "linux")]
pub fn process(pid: i32) -> Option<Row> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end = text.rfind(") ")?;
    let f: Vec<_> = text[end + 2..].split_whitespace().collect();
    let start = f.get(19)?.parse().ok()?;
    static CLOCK: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    let (boot, ticks) = (*CLOCK.get_or_init(|| {
        let boot = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("btime ").and_then(|s| s.parse::<u64>().ok()))?;
        // SAFETY: sysconf accepts this scalar selector and retains no pointer.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        (ticks > 0).then_some((boot, ticks))
    }))?;
    Some(Row {
        id: Identity { pid, start },
        ppid: f.get(1)?.parse().ok()?,
        pgid: f.get(2)?.parse().ok()?,
        command: text[text.find('(')? + 1..end].into(),
        zombie: f[0] == "Z",
        epoch: boot + start / ticks,
        cpu: f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?,
    })
}
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct BsdInfo {
    flags: u32,
    status: u32,
    xstatus: u32,
    pid: u32,
    ppid: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    reserved: u32,
    comm: [u8; 16],
    name: [u8; 32],
    nfiles: u32,
    pgid: u32,
    pjobc: u32,
    tdev: u32,
    tpgid: u32,
    nice: i32,
    start_sec: u64,
    start_usec: u64,
}
#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_listallpids(buffer: *mut libc::c_void, size: i32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut libc::c_void, size: i32) -> i32;
}
#[cfg(target_os = "macos")]
pub fn process(pid: i32) -> Option<Row> {
    let mut info = BsdInfo::default();
    let size = std::mem::size_of::<BsdInfo>() as i32;
    // SAFETY: `info` is live writable storage of the documented BsdInfo ABI
    // size; proc_pidinfo retains no pointer after returning.
    if unsafe { proc_pidinfo(pid, 3, 0, (&mut info as *mut BsdInfo).cast(), size) } != size {
        return None;
    }
    let bytes = if info.name[0] != 0 {
        &info.name[..]
    } else {
        &info.comm[..]
    };
    let command = String::from_utf8_lossy(bytes.split(|b| *b == 0).next()?).into_owned();
    let mut task = [0u64; 12];
    // SAFETY: `task` is aligned writable storage and its byte size is passed;
    // proc_pidinfo retains no pointer and the count bounds later reads.
    let count = unsafe {
        proc_pidinfo(
            pid,
            4,
            0,
            task.as_mut_ptr().cast(),
            std::mem::size_of_val(&task) as i32,
        )
    };
    Some(Row {
        id: Identity {
            pid,
            start: info.start_sec * 1_000_000 + info.start_usec,
        },
        ppid: info.ppid as i32,
        pgid: info.pgid as i32,
        command,
        zombie: info.status == 5,
        epoch: info.start_sec,
        cpu: if count >= 32 { task[2] + task[3] } else { 0 },
    })
}
pub fn snapshot(root: i32, known: impl Iterator<Item = i32>) -> io::Result<BTreeMap<i32, Row>> {
    #[cfg(target_os = "linux")]
    let pids: Vec<i32> = {
        use std::collections::{BTreeSet, VecDeque};
        let mut queue: VecDeque<_> = std::iter::once(root)
            .chain(known)
            .chain(std::iter::once(std::process::id() as i32))
            .collect();
        let mut visited = BTreeSet::new();
        let mut unavailable = false;
        while let Some(pid) = queue.pop_front() {
            if !visited.insert(pid) {
                continue;
            }
            let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
                Ok(t) => t,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    unavailable = true;
                    break;
                }
            };
            for task in tasks.filter_map(Result::ok) {
                match std::fs::read_to_string(task.path().join("children")) {
                    Ok(s) => {
                        queue.extend(s.split_whitespace().filter_map(|s| s.parse::<i32>().ok()))
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => {
                        unavailable = true;
                        break;
                    }
                }
            }
            if unavailable {
                break;
            }
        }
        if unavailable {
            std::fs::read_dir("/proc")?
                .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
                .collect()
        } else {
            visited.into_iter().collect()
        }
    };
    #[cfg(target_os = "macos")]
    let _ = (root, known);
    #[cfg(target_os = "macos")]
    let pids: Vec<i32> = {
        // SAFETY: the API permits a null buffer with size zero to query the
        // current process count and retains no pointer.
        let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut pids = vec![0i32; count as usize + 128];
        // SAFETY: pids is live writable i32 storage with the byte capacity
        // supplied; proc_listallpids retains no pointer.
        let count = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        pids.truncate(count as usize);
        pids
    };
    let rows: BTreeMap<_, _> = pids
        .into_iter()
        .filter_map(|p| process(p).map(|r| (p, r)))
        .collect();
    if !rows.contains_key(&(std::process::id() as i32)) {
        return Err(io::Error::other("census cannot inspect supervisor"));
    }
    Ok(rows)
}

/// Prove possession of the same open vnode/inode as a held custody lock.
pub fn has_file(pid: i32, file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = file.metadata() else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        fds.filter_map(Result::ok).any(|e| {
            if !std::fs::metadata(e.path())
                .is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
            {
                return false;
            }
            // fdinfo describes locks on this open file description, including
            // descriptions shared across fork; matching only the inode is not proof.
            std::fs::read_to_string(format!(
                "/proc/{pid}/fdinfo/{}",
                e.file_name().to_string_lossy()
            ))
            .is_ok_and(|info| {
                info.lines().any(|line| {
                    let mut words = line.split_whitespace();
                    words.next() == Some("lock:")
                        && words.next().is_some()
                        && words.next() == Some("FLOCK")
                        && words.next() == Some("ADVISORY")
                        && words.next() == Some("WRITE")
                })
            }) && std::fs::metadata(e.path())
                .is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
        })
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn proc_pidfdinfo(
                pid: i32,
                fd: i32,
                flavor: i32,
                buffer: *mut libc::c_void,
                size: i32,
            ) -> i32;
        }
        // proc_fdinfo is two 32-bit fields; vnode_fdinfo starts with the
        // 24-byte proc_fileinfo, then vinfo_stat.dev and .ino at 0 and 8.
        // SAFETY: proc_pidinfo permits this null-buffer size query and keeps
        // no pointer after returning.
        let size = unsafe { proc_pidinfo(pid, 1, 0, std::ptr::null_mut(), 0) };
        if size <= 0 || size > 1024 * 1024 {
            return false;
        }
        let mut fds = vec![0i32; (size as usize + 64) / 4];
        // SAFETY: fds is live writable i32 storage with its byte capacity
        // supplied; the result count bounds the following slice.
        let n = unsafe { proc_pidinfo(pid, 1, 0, fds.as_mut_ptr().cast(), (fds.len() * 4) as i32) };
        for pair in fds[..(n.max(0) as usize / 4)].as_chunks::<2>().0 {
            if pair[1] != 1 {
                continue;
            }
            let mut buf = [0u64; 32];
            // SAFETY: buf is aligned live storage large enough for the
            // proc_fileinfo/vnode fields read below; the call retains no pointer.
            let n = unsafe {
                proc_pidfdinfo(
                    pid,
                    pair[0],
                    1,
                    buf.as_mut_ptr().cast(),
                    std::mem::size_of_val(&buf) as i32,
                )
            };
            // XNU exports fileglob.fg_flag as proc_fileinfo.fi_openflags.
            // FWASLOCKED (0x4000) is set on a successful flock. Custody is
            // monotonic: neither owner nor supervisor may call LOCK_UN; a
            // still-open flagged description therefore retains that custody.
            // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_descrip.c (sys_flock)
            const FWASLOCKED: u32 = 0x4000;
            if n >= 40
                && (buf[0] as u32 & FWASLOCKED) != 0
                && (buf[3] as u32 as u64) == meta.dev()
                && buf[4] == meta.ino()
            {
                return true;
            }
        }
        false
    }
}

/// A single writer owns stdout. Check the kernel's free pipe space before a
/// nonblocking write larger than PIPE_BUF, so backpressure drops whole events.
pub fn write_event(fd: RawFd, line: &[u8]) -> usize {
    // SAFETY: zero is a valid output buffer representation for fstat to fill.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: stat is live writable storage; fd is borrowed for this call and
    // fstat retains no pointer.
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return 0;
    }
    if stat.st_mode & libc::S_IFMT == libc::S_IFIFO {
        // SAFETY: fpathconf accepts this scalar query for the borrowed fd.
        let atomic = unsafe { libc::fpathconf(fd, libc::_PC_PIPE_BUF) };
        if atomic <= 0 {
            return 0;
        }
        if line.len() > atomic as usize {
            #[cfg(target_os = "linux")]
            {
                // SAFETY: fcntl reads flags from the borrowed descriptor and
                // does not retain pointers.
                let capacity = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
                let mut used = 0i32;
                // SAFETY: used is live writable integer storage for FIONREAD;
                // ioctl retains no pointer and fd remains borrowed.
                if capacity < 0
                    || unsafe { libc::ioctl(fd, libc::FIONREAD, &mut used) } != 0
                    || (capacity.saturating_sub(used) as usize) < line.len()
                {
                    return 0;
                }
            }
            #[cfg(target_os = "macos")]
            {
                // SAFETY: kqueue takes no pointers and returns a new owned fd.
                let queue = unsafe { libc::kqueue() };
                if queue < 0 {
                    return 0;
                }
                let change = libc::kevent {
                    ident: fd as usize,
                    filter: libc::EVFILT_WRITE,
                    flags: libc::EV_ADD | libc::EV_ONESHOT,
                    fflags: 0,
                    data: 0,
                    udata: std::ptr::null_mut(),
                };
                // SAFETY: zero is a valid empty event output record.
                let mut event: libc::kevent = unsafe { std::mem::zeroed() };
                let zero = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // SAFETY: change, event and timeout are live records for the
                // call; queue is owned here and the watched fd is borrowed.
                let n = unsafe { libc::kevent(queue, &change, 1, &mut event, 1, &zero) };
                // SAFETY: this block owns the fresh kqueue descriptor and no
                // other owner has been created for it.
                let _ = unsafe { libc::close(queue) };
                if n != 1 || event.flags & libc::EV_ERROR != 0 || event.data < line.len() as isize {
                    return 0;
                }
            }
        }
    }
    // FIFO writes have enough room, or are <= PIPE_BUF (all-or-EAGAIN).
    // Other descriptors are best effort; regular files do not backpressure.
    loop {
        // SAFETY: line is live readable storage for its length and fd is
        // borrowed for this write; write retains no pointer.
        let n = unsafe { libc::write(fd, line.as_ptr().cast(), line.len()) };
        if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return n.max(0) as usize;
    }
}
/// At most one incomplete event is retained. A later event can never be
/// appended to a predecessor's partial line. Permanent sink loss is best effort.
pub struct EventOutput {
    fd: RawFd,
    pending: Vec<u8>,
    at: usize,
    broken: bool,
}
impl EventOutput {
    pub fn new(fd: RawFd) -> Self {
        Self {
            fd,
            pending: Vec::new(),
            at: 0,
            broken: false,
        }
    }
    pub fn emit(&mut self, line: &[u8]) {
        self.flush();
        if self.broken || !self.pending.is_empty() {
            return;
        }
        let n = write_event(self.fd, line);
        if n > 0 && n < line.len() {
            self.pending.extend_from_slice(line);
            self.at = n;
            self.flush();
        }
    }
    pub fn finish(&mut self, budget: std::time::Duration) {
        let started = std::time::Instant::now();
        while !self.pending.is_empty() && !self.broken && started.elapsed() < budget {
            self.flush();
            if self.pending.is_empty() || self.broken {
                break;
            }
            let mut fd = libc::pollfd {
                fd: self.fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let millis = budget.saturating_sub(started.elapsed()).as_millis().min(20) as i32;
            // SAFETY: fd is a live pollfd record and poll retains no pointer.
            unsafe { libc::poll(&mut fd, 1, millis) };
        }
    }
    pub fn flush(&mut self) {
        for _ in 0..4 {
            if self.pending.is_empty() || self.broken {
                return;
            }
            // SAFETY: the selected pending slice is live readable storage for
            // this write, and the output descriptor remains borrowed.
            let n = unsafe {
                libc::write(
                    self.fd,
                    self.pending[self.at..].as_ptr().cast(),
                    self.pending.len() - self.at,
                )
            };
            if n > 0 {
                self.at += n as usize;
                if self.at == self.pending.len() {
                    self.pending.clear();
                    self.at = 0;
                }
                continue;
            }
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if e.kind() == io::ErrorKind::WouldBlock {
                    return;
                }
            }
            self.broken = true;
            self.pending.clear();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn socket_output_finishes_a_short_line_before_close() {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let (writer, mut reader) = UnixStream::pair().unwrap();
        let size = 1024i32;
        // SAFETY: `size` is live integer storage of the SO_SNDBUF option size;
        // writer owns the descriptor throughout the call.
        unsafe {
            assert_eq!(
                libc::setsockopt(
                    writer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&size as *const i32).cast(),
                    4
                ),
                0
            );
        }
        nonblocking(writer.as_raw_fd()).unwrap();
        let line = format!(
            "{{\"event\":\"exited\",\"data\":\"{}\"}}\n",
            "x".repeat(15000)
        );
        let mut output = EventOutput::new(writer.as_raw_fd());
        output.emit(line.as_bytes());
        let receive = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            bytes
        });
        output.finish(std::time::Duration::from_millis(500));
        drop(writer);
        assert_eq!(receive.join().unwrap(), line.as_bytes());
    }
    #[test]
    fn backpressure_drops_a_large_event_without_partial_json() {
        let mut fds = [0; 2];
        // SAFETY: fds is live writable storage for pipe's two returned ints.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: pipe returned this unique read descriptor; File assumes it
        // exactly once and closes it on drop.
        let read = unsafe { File::from_raw_fd(fds[0]) };
        // SAFETY: pipe returned this unique write descriptor; File assumes it
        // exactly once and closes it on drop.
        let write = unsafe { File::from_raw_fd(fds[1]) };
        nonblocking(write.as_raw_fd()).unwrap();
        nonblocking(read.as_raw_fd()).unwrap();
        let filler = [b'x'; 512];
        loop {
            // SAFETY: filler is live readable storage and write owns the pipe
            // descriptor for this call.
            let count =
                unsafe { libc::write(write.as_raw_fd(), filler.as_ptr().cast(), filler.len()) };
            if count <= 0 {
                break;
            }
        }
        let mut discard = [0u8; 512];
        // SAFETY: discard is live writable storage and read owns the pipe fd.
        let read_count = unsafe { libc::read(read.as_raw_fd(), discard.as_mut_ptr().cast(), 512) };
        assert_eq!(read_count, 512);
        let record = format!(
            "{{\"event\":\"exited\",\"data\":\"{}\"}}\n",
            "z".repeat(10000)
        );
        write_event(write.as_raw_fd(), record.as_bytes());
        let mut received = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            // SAFETY: buf is live writable storage and read owns the pipe fd.
            let n = unsafe { libc::read(read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            received.extend_from_slice(&buf[..n as usize]);
        }
        assert!(received.iter().all(|b| *b == b'x'));
    }
}
