//! Provider-process custody in the short-lived guardian.
//!
//! This module owns only the provider child, its pipes, and census-proven
//! descendants. Utility execution remains in `exec`. Provider signals use
//! `ProviderGroup::stop`; adopted-child signals require exact tracked identity
//! and a live direct-child wait relationship.
use super::{
    child,
    reaper::{Permit, Reaper},
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::machine::{DeliveryFailure, DeliveryOutcome};
use crate::{
    census::{
        Ancestry, Census, MAX_CENSUS_ENTRIES, ProcessIdentity, ProcessLookup, ProcessObservation,
    },
    exec::Termination,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const EINTR_RETRIES: usize = 8;
const MAX_REACTOR_WAIT: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const MAX_GUARDIAN_THREADS: usize = 256;
#[cfg(target_os = "linux")]
const MAX_CHILD_LIST_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PollInterest {
    Read,
    Write,
}

/// A borrowed descriptor is safe to poll and does not transfer custody.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Watch<'a> {
    fd: BorrowedFd<'a>,
    interest: PollInterest,
}
impl<'a> Watch<'a> {
    pub(crate) fn read(fd: BorrowedFd<'a>) -> Self {
        Self {
            fd,
            interest: PollInterest::Read,
        }
    }
    pub(crate) fn write(fd: BorrowedFd<'a>) -> Self {
        Self {
            fd,
            interest: PollInterest::Write,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReactorEvent {
    pub(crate) supervisor_signal: bool,
}

/// Guardian-only signal/event source. Install before spawning a provider.
/// Signals stay blocked through guardian exit and are consumed through signalfd
/// on Linux or reported through kqueue on macOS; no handler or process-global
/// mutable fd is used.
pub(crate) struct GuardianReactor {
    #[cfg(target_os = "linux")]
    signal_fd: std::fs::File,
    #[cfg(target_os = "macos")]
    queue: std::fs::File,
    _mask: SignalMask,
}
impl GuardianReactor {
    pub(crate) fn install() -> io::Result<Self> {
        super::raw_exec::verify_child_wait_policy()?;
        let mask = SignalMask::block()?;
        #[cfg(target_os = "linux")]
        {
            // SAFETY: `mask.set` is a live, initialised sigset_t. signalfd
            // borrows it for this call only and returns a fresh descriptor.
            let fd =
                unsafe { libc::signalfd(-1, &mask.set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful signalfd returns a fresh owning descriptor;
            // this File becomes its sole owner.
            let signal_fd = unsafe { std::fs::File::from_raw_fd(fd) };
            Ok(Self {
                signal_fd,
                _mask: mask,
            })
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: kqueue has no pointer arguments and returns a fresh fd.
            let fd = unsafe { libc::kqueue() };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful kqueue returns a fresh owning descriptor;
            // this File becomes its sole owner.
            let queue = unsafe { std::fs::File::from_raw_fd(fd) };
            let _ = &mask.set;
            for signal in guardian_signals() {
                // SAFETY: zero is a valid empty kevent record; fields below
                // describe one signal filter and retain no pointers.
                let mut change: libc::kevent = unsafe { std::mem::zeroed() };
                change.ident = signal as usize;
                change.filter = libc::EVFILT_SIGNAL;
                change.flags = libc::EV_ADD | libc::EV_CLEAR;
                // SAFETY: queue is live, change points to one initialised
                // record, and the null event list/timeout request no output.
                let result = unsafe {
                    libc::kevent(
                        queue.as_raw_fd(),
                        &change,
                        1,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    )
                };
                if result < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(Self { queue, _mask: mask })
        }
    }

    /// Waits for at most the requested duration, capped to one second per
    /// turn. EINTR retries consume the same monotonic budget.
    pub(crate) fn wait(
        &mut self,
        watches: &[Watch<'_>],
        timeout: Duration,
    ) -> io::Result<ReactorEvent> {
        if watches.len() > 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many guardian descriptors",
            ));
        }
        let started = Instant::now();
        let mut event = ReactorEvent::default();
        let mut attempts = 0;
        loop {
            let remaining = timeout
                .saturating_sub(started.elapsed())
                .min(MAX_REACTOR_WAIT);
            let millis = remaining.as_millis().min(1_000) as i32;
            let mut pollfds = Vec::with_capacity(watches.len() + 1);
            #[cfg(target_os = "linux")]
            let signal_raw = self.signal_fd.as_raw_fd();
            #[cfg(target_os = "macos")]
            let signal_raw = self.queue.as_raw_fd();
            pollfds.push(libc::pollfd {
                fd: signal_raw,
                events: libc::POLLIN,
                revents: 0,
            });
            for watch in watches {
                let mut events = 0;
                match watch.interest {
                    PollInterest::Read => events |= libc::POLLIN,
                    PollInterest::Write => events |= libc::POLLOUT,
                }
                pollfds.push(libc::pollfd {
                    fd: watch.fd.as_raw_fd(),
                    events,
                    revents: 0,
                });
            }
            // SAFETY: pollfds is a live, exclusively borrowed array whose
            // elements refer to live borrowed descriptors; the count matches
            // its length and the timeout is always finite and bounded.
            let result =
                unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, millis) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted && attempts < EINTR_RETRIES {
                    attempts += 1;
                    continue;
                }
                return Err(error);
            }
            if let Some(signal_ready) = pollfds.first().map(|fd| fd.revents != 0)
                && signal_ready
            {
                #[cfg(target_os = "linux")]
                self.read_signalfd(&mut event)?;
                #[cfg(target_os = "macos")]
                self.read_kqueue(&mut event)?;
            }
            return Ok(event);
        }
    }

    #[cfg(target_os = "linux")]
    fn read_signalfd(&self, event: &mut ReactorEvent) -> io::Result<()> {
        for _ in 0..64 {
            let mut info = std::mem::MaybeUninit::<libc::signalfd_siginfo>::uninit();
            // SAFETY: info points to writable storage of the exact kernel
            // record size; signalfd writes at most that size and retains no
            // pointer. The descriptor is owned and live in self.
            let result = unsafe {
                libc::read(
                    self.signal_fd.as_raw_fd(),
                    info.as_mut_ptr().cast(),
                    std::mem::size_of::<libc::signalfd_siginfo>(),
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "guardian signalfd closed",
                ));
            }
            if usize::try_from(result).ok() != Some(std::mem::size_of::<libc::signalfd_siginfo>()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short guardian signalfd record",
                ));
            }
            // SAFETY: an exact-size successful signalfd read initialised info.
            let info = unsafe { info.assume_init() };
            match info.ssi_signo as i32 {
                libc::SIGCHLD => {}
                libc::SIGTERM | libc::SIGINT | libc::SIGHUP => event.supervisor_signal = true,
                _ => {}
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn read_kqueue(&self, event: &mut ReactorEvent) -> io::Result<()> {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        for _ in 0..4 {
            // SAFETY: zeroed kevent slots are writable output records; the
            // queue is live, and the zero timeout makes each drain nonblocking.
            let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
            // SAFETY: events is a live array of 16 output records; no change
            // list is supplied and the finite zero timeout cannot block.
            let result = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    16,
                    &timeout,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if result == 0 {
                return Ok(());
            }
            for item in events.iter().take(result as usize) {
                if item.filter != libc::EVFILT_SIGNAL {
                    continue;
                }
                match item.ident as i32 {
                    libc::SIGCHLD => {}
                    libc::SIGTERM | libc::SIGINT | libc::SIGHUP => event.supervisor_signal = true,
                    _ => {}
                }
            }
            if result < 16 {
                return Ok(());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn guardian_signals() -> [i32; 4] {
    [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD]
}

struct SignalMask {
    set: libc::sigset_t,
}
impl SignalMask {
    fn block() -> io::Result<Self> {
        let mut set = std::mem::MaybeUninit::<libc::sigset_t>::zeroed();
        // SAFETY: sigemptyset initialises writable, aligned storage and keeps
        // no pointer after returning.
        if unsafe { libc::sigemptyset(set.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful sigemptyset fully initialised the signal set.
        let mut set = unsafe { set.assume_init() };
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGCHLD] {
            // SAFETY: set is a live signal set exclusively owned here;
            // sigaddset mutates it and retains no pointer.
            if unsafe { libc::sigaddset(&mut set, signal) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: set is a live signal set borrowed only during this call.
        // sigprocmask changes the calling thread's mask and retains no pointer;
        // other threads neither share the storage nor have their masks changed.
        if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // This dedicated guardian exits after settlement, including on errors.
        // Keep its mask blocked through exit: kqueue reports blocked signals
        // without consuming them, so restoring would re-deliver handled stops.
        Ok(Self { set })
    }
}

/// Linux subreaper state is process-local and is enabled only by the guardian.
/// The short-lived internal-supervise process exits after settlement, so the
/// setting cannot escape into the shared Run/MCP host.
#[cfg(target_os = "linux")]
pub(crate) struct SubreaperGuard {
    previous: i32,
}
#[cfg(target_os = "linux")]
impl SubreaperGuard {
    #[cfg(target_os = "linux")]
    pub(crate) fn enable() -> io::Result<Self> {
        let mut previous = 0_i32;
        // SAFETY: prctl receives scalar values and a live writable i32 pointer;
        // it retains no pointer. Only the isolated guardian calls this method.
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: scalar-only process setting scoped to this guardian; no
        // pointer is retained. The prior value is kept for explicit restoration.
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { previous })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn restore(self) -> io::Result<()> {
        // SAFETY: scalar-only process setting in the same isolated guardian.
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.previous, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdoptedCleanup {
    pub(crate) reaped: Vec<ProcessIdentity>,
    pub(crate) unresolved: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) struct AdoptedDelivery {
    pub(crate) identity: ProcessIdentity,
    pub(crate) outcome: DeliveryOutcome,
}

/// The child cannot exec until the observer has captured its native identity.
/// Command::spawn waits for exec, so a short-lived helper handles the parent end
/// of the handshake. It is joined on every path before custody is returned.
fn spawn_identified(
    mut command: Command,
    lease: &crate::fs::LockGuard,
    env: &[(OsString, OsString)],
    boot: &str,
    permit: Permit,
    secret: Option<&child::SecretPipe>,
) -> io::Result<(Child, ProcessIdentity, Permit)> {
    let observer =
        super::census::NativeProcessObserver::for_boot(boot).map_err(io::Error::other)?;
    let (mut parent, channel) = UnixStream::pair()?;
    parent.set_read_timeout(Some(Duration::from_secs(5)))?;
    parent.set_write_timeout(Some(Duration::from_secs(5)))?;
    // SAFETY: this hook runs only after fork. It uses async-signal-safe
    // syscalls on the inherited owned socket, stack bytes and a bounded wait;
    // no allocation or lock is used. channel remains live through spawn.
    unsafe {
        command.pre_exec(move || identity_gate(channel.as_raw_fd()));
    }
    let mut pass = vec![lease.handoff_file().as_fd()];
    if let Some(pipe) = secret {
        pass.push(pipe.fd());
    }
    child::configure_command(&mut command, &pass, Some(env), false)?;
    let worker = std::thread::Builder::new()
        .name("provider-identity".into())
        .spawn(move || {
            let mut pid = [0_u8; 4];
            parent.read_exact(&mut pid)?;
            match observer.observe(i64::from(i32::from_ne_bytes(pid))) {
                ProcessObservation::Present(fact) => {
                    parent.write_all(&[1])?;
                    Ok(fact.identity)
                }
                _ => {
                    parent.write_all(&[0])?;
                    Err(io::Error::other(
                        "provider identity unavailable before exec",
                    ))
                }
            }
        })?;
    let spawned = command.spawn();
    // On pre-exec failure, closing the child's socket wakes the helper too.
    drop(command);
    let identity = worker
        .join()
        .map_err(|_| io::Error::other("provider identity observer failed"))
        .and_then(|identity| identity);
    match (spawned, identity) {
        (Ok(child), Ok(identity)) => Ok((child, identity, permit)),
        (Ok(child), Err(error)) => {
            permit.transfer(child);
            Err(error)
        }
        (Err(error), _) => Err(error),
    }
}

fn identity_gate(fd: i32) -> io::Result<()> {
    // SAFETY: getpid has no pointers and is async-signal-safe after fork.
    let pid = unsafe { libc::getpid() }.to_ne_bytes();
    // SAFETY: fd is the hook's live inherited socket; pid is a four-byte stack
    // buffer. send retains no pointer. MSG_NOSIGNAL avoids an inherited handler.
    if unsafe { libc::send(fd, pid.as_ptr().cast(), pid.len(), libc::MSG_NOSIGNAL) } != 4 {
        return Err(io::Error::last_os_error());
    }
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one writable stack record with a live socket fd;
    // the five-second deadline bounds a lost helper before exec.
    if unsafe { libc::poll(&mut poll, 1, 5_000) } <= 0 {
        return Err(io::Error::from_raw_os_error(libc::ETIMEDOUT));
    }
    let mut accepted = 0_u8;
    // SAFETY: read writes at most one byte to live exclusive stack storage;
    // fd is readable or closed after poll and no pointer is retained.
    if unsafe { libc::read(fd, (&mut accepted as *mut u8).cast(), 1) } != 1 || accepted != 1 {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}

/// Provider owner. Dropping this value transfers the direct child to its
/// pre-reserved bounded reaper without signalling it; normal cleanup must call
/// `stop` and `cleanup` explicitly while the unreaped child reserves its PID.
pub(crate) struct ProviderGroup {
    child: Option<Child>,
    permit: Option<Permit>,
    _reaper: Reaper,
    pid: i32,
    last_direct_signal: Option<i32>,
    last_group_signal: Option<i32>,
    observed: Option<Termination>,
    provider_identity: Option<ProcessIdentity>,
    owned_descendants: BTreeSet<ProcessIdentity>,
    spared_descendants: BTreeSet<ProcessIdentity>,
    last_adopted_signal: BTreeMap<ProcessIdentity, i32>,
    ownership_uncertain: bool,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}
impl ProviderGroup {
    pub(crate) fn spawn(
        spec: super::raw_exec::SpawnSpec<'_>,
        lease: &crate::fs::LockGuard,
        boot: &str,
    ) -> io::Result<Self> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty provider argv"))?;
        let reaper = Reaper::new(1)?;
        let permit = reaper.reserve().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "provider reaper admission is full",
            )
        })?;
        let secret = spec.secret_input.map(child::SecretPipe::new).transpose()?;
        let mut env = spec.env.map(<[_]>::to_vec).unwrap_or_default();
        if let (Some(pipe), Some(input)) = (&secret, spec.secret_input) {
            env = pipe.environment(input, &env);
        }
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
            .current_dir(spec.cwd)
            .process_group(0)
            .stdin(if spec.input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if spec.capture_stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(if spec.capture_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let (child, identity, permit) =
            spawn_identified(command, lease, &env, boot, permit, secret.as_ref())?;
        drop(secret);
        let pid = match i32::try_from(child.id()) {
            Ok(pid) if pid > 0 => pid,
            _ => {
                permit.transfer(child);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "provider PID is outside the native range",
                ));
            }
        };
        let mut group = Self {
            child: Some(child),
            permit: Some(permit),
            _reaper: reaper,
            pid,
            last_direct_signal: None,
            last_group_signal: None,
            observed: None,
            provider_identity: Some(identity),
            owned_descendants: BTreeSet::new(),
            spared_descendants: BTreeSet::new(),
            last_adopted_signal: BTreeMap::new(),
            ownership_uncertain: false,
            stdin: None,
            stdout: None,
            stderr: None,
        };
        if let Some(child) = group.child.as_mut() {
            group.stdin = child.stdin.take();
            group.stdout = child.stdout.take();
            group.stderr = child.stderr.take();
        }
        if let Some(pipe) = group.stdin.as_ref() {
            set_nonblocking(pipe.as_raw_fd())?;
        }
        if let Some(pipe) = group.stdout.as_ref() {
            set_nonblocking(pipe.as_raw_fd())?;
        }
        if let Some(pipe) = group.stderr.as_ref() {
            set_nonblocking(pipe.as_raw_fd())?;
        }
        Ok(group)
    }

    pub(crate) fn id(&self) -> i64 {
        i64::from(self.pid)
    }

    pub(crate) fn identity(&self) -> Option<&ProcessIdentity> {
        self.provider_identity.as_ref()
    }

    pub(crate) fn watches(&self) -> Vec<Watch<'_>> {
        let mut watches = Vec::with_capacity(3);
        if let Some(pipe) = self.stdin.as_ref() {
            watches.push(Watch::write(pipe.as_fd()));
        }
        if let Some(pipe) = self.stdout.as_ref() {
            watches.push(Watch::read(pipe.as_fd()));
        }
        if let Some(pipe) = self.stderr.as_ref() {
            watches.push(Watch::read(pipe.as_fd()));
        }
        watches
    }

    /// Nonblocking WNOWAIT observation. Errors remain distinct from an absent
    /// status, so failed observation can never be mistaken for a live child.
    pub(crate) fn observe(&mut self) -> io::Result<Option<Termination>> {
        if self.observed.is_some() || self.child.is_none() {
            return Ok(self.observed);
        }
        self.observed = wait_exit(self.pid, true)?;
        Ok(self.observed)
    }

    /// The sole provider-signalling API. `escalation` selects KILL after the
    /// caller's bounded TERM grace. A failed observation refuses to signal.
    pub(crate) fn stop(&mut self, escalation: bool) -> io::Result<bool> {
        if self.child.is_none() || self.observe()?.is_some() {
            return Ok(false);
        }
        let signal = if escalation {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        if self.last_direct_signal == Some(signal) {
            return Ok(false);
        }
        // SAFETY: the live Child is owned by this ProviderGroup and has not
        // been reaped. Its kernel child identity reserves this positive PID;
        // this signals only that provider, never its mutable process group.
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            self.last_direct_signal = Some(signal);
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// Signal the provider's whole process group, for explicit cancellation
    /// only. While the leader is unreaped (live or a WNOWAIT zombie) it
    /// reserves the PGID. After it is reaped, the group is signalled only if a
    /// `kill(-pgid, 0)` probe shows a live member, which keeps the id
    /// allocated (the probe-to-signal gap is the accepted residual). Members
    /// that left the group are not reached.
    pub(crate) fn stop_group(&mut self, escalation: bool) -> io::Result<bool> {
        let signal = if escalation {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        if self.last_group_signal == Some(signal) {
            return Ok(false);
        }
        if self.child.is_none() {
            // SAFETY: signal 0 delivers nothing; it only probes the group.
            if unsafe { libc::kill(-self.pid, 0) } != 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(false)
                } else {
                    Err(error)
                };
            }
        }
        // SAFETY: the provider was spawned as its own group leader
        // (`process_group(0)`), so `-pid` names exactly the group it leads; a
        // live member (or the unreaped leader) keeps the PGID allocated.
        if unsafe { libc::kill(-self.pid, signal) } == 0 {
            self.last_group_signal = Some(signal);
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// Record exact census identities proven to descend from this provider.
    /// Tracking survives reparenting; only an explicit absence or changed
    /// full native identity in the census can discard old proof.
    pub(crate) fn record_census(&mut self, census: &Census) {
        let provider = match census.process(i64::from(self.pid)) {
            ProcessLookup::Present(fact) => match &self.provider_identity {
                Some(known) if known != &fact.identity => None,
                Some(known) => Some(known.clone()),
                None => {
                    self.provider_identity = Some(fact.identity.clone());
                    Some(fact.identity.clone())
                }
            },
            ProcessLookup::Absent | ProcessLookup::Unknown => self.provider_identity.clone(),
        };
        if let Some(provider) = provider {
            #[cfg(target_os = "linux")]
            let guardian_pid = i64::from(std::process::id());
            for fact in census.facts() {
                if fact.identity == provider {
                    continue;
                }
                let ancestry = provider_ancestry(census, &fact.identity, &provider);
                #[cfg(target_os = "linux")]
                let adopted_direct_child = fact.parent_pid == guardian_pid
                    && fact.identity.pid != provider.pid
                    && fact.identity.boot == provider.boot
                    && match native_pid(fact.identity.pid)
                        .and_then(|pid| wait_exit(pid, true).map(|_| ()))
                    {
                        // A successful WNOWAIT call (including si_pid == 0)
                        // proves the exact PID is still this guardian's child.
                        // The census row supplies its full boot/PID/start tuple;
                        // the dedicated guardian has no unrelated child source.
                        Ok(()) => true,
                        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => false,
                        Err(_) => {
                            self.ownership_uncertain = true;
                            false
                        }
                    };
                #[cfg(not(target_os = "linux"))]
                let adopted_direct_child = false;
                if ancestry == Ancestry::Yes || adopted_direct_child {
                    if self.owned_descendants.contains(&fact.identity)
                        || self.owned_descendants.len() < MAX_CENSUS_ENTRIES
                    {
                        self.owned_descendants.insert(fact.identity.clone());
                    } else {
                        self.ownership_uncertain = true;
                    }
                }
            }
        }
        self.owned_descendants
            .retain(|identity| match census.process(identity.pid) {
                ProcessLookup::Present(fact) => fact.identity == *identity,
                ProcessLookup::Absent => false,
                ProcessLookup::Unknown => true,
            });
        self.spared_descendants
            .retain(|identity| match census.process(identity.pid) {
                ProcessLookup::Present(fact) => fact.identity == *identity,
                ProcessLookup::Absent => false,
                ProcessLookup::Unknown => true,
            });
        self.last_adopted_signal
            .retain(|identity, _| self.owned_descendants.contains(identity));
    }

    fn record_sparing(&mut self, census: &Census, spared: &BTreeSet<ProcessIdentity>) {
        self.spared_descendants.extend(spared.iter().cloned());
        let Some(provider) = self.provider_identity.as_ref() else {
            return;
        };
        let valid_spares: Vec<_> = spared
            .iter()
            .filter(|identity| {
                matches!(census.process(identity.pid), ProcessLookup::Present(fact) if fact.identity == **identity)
                    && provider_ancestry(census, identity, provider) == Ancestry::Yes
            })
            .cloned()
            .collect();
        for spare in valid_spares {
            for identity in &self.owned_descendants {
                if provider_ancestry(census, identity, &spare) == Ancestry::Yes {
                    self.spared_descendants.insert(identity.clone());
                }
            }
        }
    }

    /// Reap only exact native identities previously proven below the provider
    /// and now adopted by this guardian. Sparing includes descendants proven
    /// through `Census::ancestry`; unknown and escaped rows remain fenced.
    pub(crate) fn reap_adopted(
        &mut self,
        census: &Census,
        spared: &BTreeSet<ProcessIdentity>,
    ) -> io::Result<AdoptedCleanup> {
        self.record_census(census);
        self.record_sparing(census, spared);
        let guardian_pid = i64::from(std::process::id());
        let identities: Vec<ProcessIdentity> = self.owned_descendants.iter().cloned().collect();
        let mut result = AdoptedCleanup {
            reaped: Vec::new(),
            unresolved: self.ownership_uncertain || !census.is_complete(),
        };
        for identity in identities {
            let row = match census.process(identity.pid) {
                ProcessLookup::Present(fact) if fact.identity == identity => fact,
                ProcessLookup::Present(_) | ProcessLookup::Absent => {
                    self.owned_descendants.remove(&identity);
                    self.last_adopted_signal.remove(&identity);
                    continue;
                }
                ProcessLookup::Unknown => {
                    result.unresolved = true;
                    continue;
                }
            };
            // A spared owner is never signalled, but once it has exited and
            // this guardian adopted it, reaping the zombie is safe and keeps a
            // long-lived fenced guardian from pinning it.
            if self.spared_descendants.contains(&identity)
                && !(row.zombie && row.parent_pid == guardian_pid)
            {
                result.unresolved = true;
                continue;
            }
            if row.parent_pid != guardian_pid {
                // Still running below the provider, escaped, or not yet
                // adopted. It cannot be reaped by this guardian as a child.
                result.unresolved = true;
                continue;
            }
            match wait_exit(native_pid(identity.pid)?, true) {
                Ok(Some(observed)) => match wait_exit(native_pid(identity.pid)?, false) {
                    Ok(Some(reaped)) if reaped == observed => {
                        self.owned_descendants.remove(&identity);
                        self.last_adopted_signal.remove(&identity);
                        result.reaped.push(identity);
                    }
                    Ok(_) => result.unresolved = true,
                    Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                        result.unresolved = true;
                    }
                    Err(error) => return Err(error),
                },
                Ok(None) => result.unresolved = true,
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                    result.unresolved = true;
                }
                Err(error) => return Err(error),
            }
        }
        if !self.owned_descendants.is_empty() {
            result.unresolved = true;
        }
        Ok(result)
    }

    /// Signal only an exact, previously tracked descendant which the current
    /// census shows is now this guardian's direct child. Successful `waitid`
    /// with WNOWAIT reserves that child identity until this guardian reaps it.
    #[cfg(target_os = "linux")]
    pub(crate) fn stop_adopted(
        &mut self,
        escalation: bool,
        census: &Census,
        spared: &BTreeSet<ProcessIdentity>,
    ) -> io::Result<Vec<AdoptedDelivery>> {
        self.record_census(census);
        self.record_sparing(census, spared);
        let guardian_pid = i64::from(std::process::id());
        let identities: Vec<ProcessIdentity> = self.owned_descendants.iter().cloned().collect();
        let signal = if escalation {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        let mut deliveries = Vec::new();
        for identity in identities {
            if self.spared_descendants.contains(&identity) {
                continue;
            }
            let row = match census.process(identity.pid) {
                ProcessLookup::Present(fact)
                    if fact.identity == identity && fact.parent_pid == guardian_pid =>
                {
                    fact
                }
                ProcessLookup::Present(_) | ProcessLookup::Absent | ProcessLookup::Unknown => {
                    continue;
                }
            };
            if row.zombie || self.last_adopted_signal.get(&identity) == Some(&signal) {
                continue;
            }
            match wait_exit(native_pid(identity.pid)?, true) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => continue,
                Err(error) => return Err(error),
            }
            // SAFETY: census matched the full boot/PID/start/group/session
            // identity and current parent PID; the successful WNOWAIT child
            // observation above proves this is our unreaped direct child. The
            // guardian has one waiter for adopted children, so its positive
            // PID cannot be reused between waitid and kill. This targets one
            // child only, never a caller-selected group or arbitrary PID.
            if unsafe { libc::kill(native_pid(identity.pid)?, signal) } == 0 {
                self.last_adopted_signal.insert(identity.clone(), signal);
                deliveries.push(AdoptedDelivery {
                    identity,
                    outcome: DeliveryOutcome::Delivered,
                });
                continue;
            }
            deliveries.push(AdoptedDelivery {
                identity,
                outcome: DeliveryOutcome::Failed(kill_failure(&io::Error::last_os_error())),
            });
        }
        Ok(deliveries)
    }

    /// Signal exact identities the guardian found still inside the attempt's
    /// sandbox after they left the provider tree. Each identity is re-observed
    /// immediately before the signal, so a recycled PID is never targeted.
    #[cfg(target_os = "macos")]
    pub(crate) fn stop_marked(
        &mut self,
        escalation: bool,
        marked: &[ProcessIdentity],
    ) -> Vec<AdoptedDelivery> {
        let signal = if escalation {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        let mut deliveries = Vec::new();
        for identity in marked {
            if self.last_adopted_signal.get(identity) == Some(&signal) {
                continue;
            }
            let Ok(pid) = native_pid(identity.pid) else {
                continue;
            };
            match super::census::observe_with_boot(identity.pid, &identity.boot) {
                ProcessObservation::Present(fact) if fact.identity == *identity && !fact.zombie => {
                }
                _ => continue,
            }
            // SAFETY: the identity (boot, PID, start stamp) was re-read just
            // above and is inside the attempt's own sandbox. This
            // targets that one process only, never a group or caller PID.
            let outcome = if unsafe { libc::kill(pid, signal) } == 0 {
                self.last_adopted_signal.insert(identity.clone(), signal);
                DeliveryOutcome::Delivered
            } else {
                match kill_failure(&io::Error::last_os_error()) {
                    // Gone between the observation and the signal: the goal.
                    DeliveryFailure::NoSuchProcess => continue,
                    failure => DeliveryOutcome::Failed(failure),
                }
            };
            deliveries.push(AdoptedDelivery {
                identity: identity.clone(),
                outcome,
            });
        }
        deliveries
    }

    /// Prove the guardian has no provider descendants left after the provider
    /// itself is terminal. Linux's per-thread children lists cover direct
    /// children created by every guardian thread; with the provider reaped only
    /// by this guardian, any surviving descendant branch must have a direct
    /// guardian child. On Darwin, the caller combines reaping with two complete
    /// censuses and an independently acquired writer lease.
    pub(crate) fn quiescent_children(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            if self.child.is_none() || wait_exit(self.pid, true)?.is_none() {
                return Ok(false);
            }
            linux_quiescent_children(self.pid)
        }
        #[cfg(target_os = "macos")]
        {
            Ok(self.child.is_none() && self.observed.is_some())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Ok(false)
        }
    }

    /// Consume the provider's wait status only after it was observed with
    /// WNOWAIT and the caller's complete census proves all descendants settled.
    /// This is a WNOHANG wait, never an unbounded Child::wait.
    pub(crate) fn cleanup(&mut self, complete: bool) -> io::Result<Option<Termination>> {
        if !complete || self.child.is_none() {
            return Ok(None);
        }
        let observed = match self.observe()? {
            Some(termination) => termination,
            None => return Ok(None),
        };
        match wait_exit(self.pid, false)? {
            Some(reaped) if reaped == observed => {
                self.child.take();
                self.permit.take();
                self.observed = Some(reaped);
                Ok(Some(reaped))
            }
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "provider wait status changed",
            )),
            None => Ok(None),
        }
    }
    /// One-way retirement after bounded explicit cleanup and durable unresolved
    /// reservation/exit publication. Consuming self releases the provider owner
    /// only after the known-dead leader is reaped. Descendants are neither
    /// signalled nor claimed settled: the persistent fence belongs to recovery.
    pub(crate) fn finish_fenced(mut self) -> Result<Termination, Box<(Self, io::Error)>> {
        let observed = match self.observe() {
            Ok(Some(status)) => status,
            Ok(None) => return Err(Box::new((self, io::ErrorKind::WouldBlock.into()))),
            Err(error) => return Err(Box::new((self, error))),
        };
        if self.child.is_none() {
            return Ok(observed);
        }
        match wait_exit(self.pid, false) {
            Ok(Some(status)) if status == observed => {
                self.child.take();
                self.permit.take();
                Ok(status)
            }
            Ok(_) => Err(Box::new((self, io::ErrorKind::WouldBlock.into()))),
            Err(error) => Err(Box::new((self, error))),
        }
    }
}
impl Drop for ProviderGroup {
    fn drop(&mut self) {
        if let (Some(child), Some(permit)) = (self.child.take(), self.permit.take()) {
            // Permit was reserved before spawn. Transfer preserves bounded
            // direct-child reaping and deliberately performs no signal.
            permit.transfer(child);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn kill_failure(error: &io::Error) -> DeliveryFailure {
    match error.raw_os_error() {
        Some(libc::ESRCH) => DeliveryFailure::NoSuchProcess,
        Some(libc::EPERM) => DeliveryFailure::PermissionDenied,
        Some(code) => DeliveryFailure::OsError(code),
        None => DeliveryFailure::OsError(libc::EIO),
    }
}

/// Liveness probe only (signal 0): true unless every PID is provably absent.
pub(crate) fn any_pid_alive(pids: &[i64]) -> bool {
    pids.iter().any(|&pid| match i32::try_from(pid) {
        Ok(pid) if pid > 0 => {
            // SAFETY: signal 0 delivers nothing and cannot affect the target.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            alive || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
        _ => true,
    })
}

fn provider_ancestry(
    census: &Census,
    child: &ProcessIdentity,
    ancestor: &ProcessIdentity,
) -> Ancestry {
    #[cfg(target_os = "linux")]
    {
        linux_provider_ancestry(census, child, ancestor)
    }
    #[cfg(not(target_os = "linux"))]
    {
        census.ancestry(child, ancestor)
    }
}

/// Linux start times are clock ticks, so a live parent and child can share a
/// stamp. Accept equal stamps only along exact parent-PID edges in this one
/// census; boot, full identities, PID cycles and newer-parent anomalies remain
/// checked.
#[cfg(target_os = "linux")]
fn linux_provider_ancestry(
    census: &Census,
    child: &ProcessIdentity,
    ancestor: &ProcessIdentity,
) -> Ancestry {
    if child == ancestor || child.boot != ancestor.boot {
        return Ancestry::No;
    }
    for identity in [child, ancestor] {
        match census.process(identity.pid) {
            ProcessLookup::Present(fact) if fact.identity == *identity => {}
            ProcessLookup::Present(_) | ProcessLookup::Absent => return Ancestry::No,
            ProcessLookup::Unknown => return Ancestry::Unknown,
        }
    }
    let mut current = child.clone();
    let mut seen = BTreeSet::from([current.pid]);
    for _ in 0..census.bound() {
        let fact = match census.process(current.pid) {
            ProcessLookup::Present(fact) if fact.identity == current => fact,
            ProcessLookup::Present(_) | ProcessLookup::Absent => return Ancestry::No,
            ProcessLookup::Unknown => return Ancestry::Unknown,
        };
        if fact.parent_pid <= 0 {
            return Ancestry::No;
        }
        let parent = match census.process(fact.parent_pid) {
            ProcessLookup::Present(parent) => parent,
            ProcessLookup::Absent => return Ancestry::No,
            ProcessLookup::Unknown => return Ancestry::Unknown,
        };
        if parent.identity.boot != current.boot
            || parent.identity.native_start > current.native_start
            || !seen.insert(parent.identity.pid)
        {
            return Ancestry::Unknown;
        }
        if parent.identity == *ancestor {
            return Ancestry::Yes;
        }
        current = parent.identity.clone();
    }
    Ancestry::Unknown
}

#[cfg(target_os = "linux")]
fn linux_quiescent_children(provider_pid: i32) -> io::Result<bool> {
    let first = linux_children_snapshot()?;
    std::thread::sleep(Duration::from_millis(20));
    let second = linux_children_snapshot()?;
    if first != second {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "guardian child census changed between snapshots",
        ));
    }
    Ok(first.values().all(|children| {
        children
            .iter()
            .all(|child_pid| *child_pid == i64::from(provider_pid))
    }))
}

#[cfg(target_os = "linux")]
fn linux_children_snapshot() -> io::Result<BTreeMap<i64, BTreeSet<i64>>> {
    let tids = linux_task_ids()?;
    let mut snapshot = BTreeMap::new();
    for tid in &tids {
        let path = std::path::Path::new("/proc/self/task")
            .join(tid.to_string())
            .join("children");
        let contents = read_proc_file_bounded(&path, MAX_CHILD_LIST_BYTES)?;
        if snapshot.insert(*tid, parse_child_ids(&contents)?).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate guardian thread ID in child census",
            ));
        }
    }
    let after = linux_task_ids()?;
    if tids != after {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "guardian thread list changed during child census",
        ));
    }
    Ok(snapshot)
}

#[cfg(target_os = "linux")]
fn linux_task_ids() -> io::Result<BTreeSet<i64>> {
    let mut tids = BTreeSet::new();
    for entry in std::fs::read_dir("/proc/self/task")? {
        let entry = entry?;
        let tid = parse_positive_id(entry.file_name().as_bytes())?;
        if !tids.insert(tid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate guardian thread ID",
            ));
        }
        if tids.len() > MAX_GUARDIAN_THREADS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "guardian thread census exceeded its bound",
            ));
        }
    }
    if tids.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guardian thread census is empty",
        ));
    }
    Ok(tids)
}

#[cfg(target_os = "linux")]
fn read_proc_file_bounded(path: &std::path::Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(limit.saturating_add(1));
    file.take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc child list exceeded its bound",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn parse_child_ids(contents: &[u8]) -> io::Result<BTreeSet<i64>> {
    let mut pids = BTreeSet::new();
    for token in contents
        .split(u8::is_ascii_whitespace)
        .filter(|token| !token.is_empty())
    {
        let pid = parse_positive_id(token)?;
        if !pids.insert(pid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate child PID in proc list",
            ));
        }
    }
    Ok(pids)
}

#[cfg(target_os = "linux")]
fn parse_positive_id(bytes: &[u8]) -> io::Result<i64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc process ID is not decimal",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "proc ID is not ASCII"))?;
    text.parse::<i64>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "proc ID is outside i64 range"))
}

fn native_pid(pid: i64) -> io::Result<i32> {
    i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "PID outside native range"))
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    // SAFETY: fd is borrowed from a live Child pipe for both calls; fcntl
    // changes only that pipe's status flags and retains no pointer or owner.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same borrowed descriptor is live; existing flags are
    // preserved while O_NONBLOCK is added. No pointer or ownership escapes.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// waitid helper shared by leader observation and adopted-child reaping.
/// `nowait` keeps the kernel identity reserved until explicit cleanup.
fn wait_exit(pid: i32, nowait: bool) -> io::Result<Option<Termination>> {
    let mut attempts = 0;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let options = libc::WEXITED | libc::WNOHANG | if nowait { libc::WNOWAIT } else { 0 };
        // SAFETY: siginfo is correctly aligned writable storage. waitid writes
        // it only on success; the positive PID comes from the owned provider
        // Child or an exact census identity whose direct-child relationship is
        // checked by its caller. WNOHANG bounds this call; WNOWAIT preserves PID.
        let result = unsafe { libc::waitid(libc::P_PID, pid as _, info.as_mut_ptr(), options) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted && attempts < EINTR_RETRIES {
                attempts += 1;
                continue;
            }
            return Err(error);
        }
        // SAFETY: a successful waitid call initialises siginfo even when no
        // process is ready; in that case si_pid is zero.
        let info = unsafe { info.assume_init() };
        #[cfg(target_os = "linux")]
        let (seen_pid, status, code) = {
            // SAFETY: waitid succeeded, so siginfo is either zeroed (no event)
            // or a SIGCHLD record whose pid and status fields are initialised.
            let seen_pid = unsafe { info.si_pid() };
            // SAFETY: as above, the SIGCHLD status field is initialised.
            let status = unsafe { info.si_status() };
            (seen_pid, status, info.si_code)
        };
        #[cfg(target_os = "macos")]
        let (seen_pid, status, code) = (info.si_pid, info.si_status, info.si_code);
        if seen_pid == 0 {
            return Ok(None);
        }
        if seen_pid != pid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "waitid returned a different child",
            ));
        }
        let status = i64::from(status);
        return match code {
            libc::CLD_EXITED => Ok(Some(Termination::Exit(status))),
            libc::CLD_KILLED | libc::CLD_DUMPED => Ok(Some(Termination::Signal(status))),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected waitid status",
            )),
        };
    }
}

#[cfg(all(test, target_os = "linux"))]
mod quiescence_tests {
    use super::parse_child_ids;
    use std::collections::BTreeSet;

    #[test]
    fn parses_bounded_proc_child_id_lists() {
        assert_eq!(
            parse_child_ids(b" 17\t23\n").ok(),
            Some(BTreeSet::from([17, 23]))
        );
        assert_eq!(parse_child_ids(b"\n").ok(), Some(BTreeSet::new()));
    }

    #[test]
    fn rejects_malformed_proc_child_id_lists() {
        for contents in [
            &b"0"[..],
            &b"-1"[..],
            &b"9223372036854775808"[..],
            &b"7 duplicate"[..],
            &b"7 7"[..],
        ] {
            assert!(parse_child_ids(contents).is_err());
        }
    }
}
