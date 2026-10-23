//! Dev-only fixtures: dispatch scenario arguments before the test suite.
use crate::census::ProcessObserver;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// A process recorded by the guardian is terminated if its owning test fails.
/// Successful tests disarm the guard after proving normal settlement.
pub struct ProcessCleanupGuard {
    root: PathBuf,
    armed: bool,
}
impl ProcessCleanupGuard {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            armed: true,
        }
    }

    pub fn disarm(mut self) {
        self.armed = false;
    }
}
impl Drop for ProcessCleanupGuard {
    fn drop(&mut self) {
        if self.armed {
            kill_recorded_processes(&self.root);
        }
    }
}

fn kill_recorded_processes(root: &Path) {
    let mut custodians = std::collections::BTreeSet::new();
    let mut dependents = std::collections::BTreeSet::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        collect_recorded_processes(root, &mut custodians, &mut dependents);
        for identity in &dependents {
            if process_identity_is_live(identity) {
                let _ = signal_recorded_process(identity, libc::SIGKILL);
            }
        }
        if dependents
            .iter()
            .all(|identity| !process_identity_is_live(identity))
            || std::time::Instant::now() >= deadline
        {
            break;
        }
        std::thread::park_timeout(std::time::Duration::from_millis(25));
    }
    collect_recorded_processes(root, &mut custodians, &mut dependents);
    for identity in custodians.iter().chain(dependents.iter()) {
        if process_identity_is_live(identity) {
            let _ = signal_recorded_process(identity, libc::SIGKILL);
        }
    }
    let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < reap_deadline
        && custodians
            .iter()
            .chain(dependents.iter())
            .any(process_identity_is_live)
    {
        std::thread::park_timeout(std::time::Duration::from_millis(10));
    }
}

fn collect_recorded_processes(
    directory: &Path,
    custodians: &mut std::collections::BTreeSet<crate::census::ProcessIdentity>,
    dependents: &mut std::collections::BTreeSet<crate::census::ProcessIdentity>,
) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_recorded_processes(&path, custodians, dependents);
            continue;
        }
        if !matches!(
            entry.file_name().to_str(),
            Some("reservation.json" | "process.json")
        ) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if let Some(identity) = value.get("guardian").and_then(parse_identity) {
            custodians.insert(identity);
        }
        if let Some(identity) = value.get("provider").and_then(parse_identity) {
            dependents.insert(identity);
        }
        if let Some(descendants) = value
            .get("descendants")
            .and_then(serde_json::Value::as_array)
        {
            dependents.extend(descendants.iter().filter_map(parse_identity));
        }
    }
}

fn parse_identity(value: &serde_json::Value) -> Option<crate::census::ProcessIdentity> {
    serde_json::from_value(value.clone()).ok()
}

#[allow(clippy::disallowed_methods)]
pub fn signal_recorded_process(
    identity: &crate::census::ProcessIdentity,
    signal: i32,
) -> io::Result<()> {
    if !process_identity_is_live(identity) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "recorded process identity is absent",
        ));
    }
    let pid = i32::try_from(identity.pid).map_err(|_| io::ErrorKind::InvalidInput)?;
    let child = Command::new("/bin/kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .spawn()?;
    let mut child = ChildGuard::new(child);
    let status = child.wait()?;
    child.detach();
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "kill command rejected the recorded process",
        ))
    }
}

fn process_identity_is_live(identity: &crate::census::ProcessIdentity) -> bool {
    matches!(
        RecordedBootObserver.observe(identity.pid),
        crate::census::ProcessObservation::Present(fact) if fact.identity == *identity
    )
}

/// Test fixture wrapper that kills and waits for an owned child during unwind.
pub struct ChildGuard {
    child: Child,
    armed: bool,
}
impl ChildGuard {
    pub fn new(child: Child) -> Self {
        Self { child, armed: true }
    }

    /// Leave a deliberately detached fixture process alive after its setup succeeds.
    pub fn detach(&mut self) {
        self.armed = false;
    }
}
impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.armed && !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Minimal libtest command protocol for `harness = false` executables.
pub fn run_libtest(
    args: &[OsString],
    test_names: &[&str],
    mut run: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    let mut list = false;
    let mut ignored = false;
    let mut exact = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--list") => list = true,
            Some("--ignored") => ignored = true,
            Some("--exact") => {
                index += 1;
                exact = Some(
                    args.get(index)
                        .and_then(|arg| arg.to_str())
                        .ok_or(io::ErrorKind::InvalidInput)?,
                );
            }
            Some("--format") => {
                index += 1;
                if args.get(index).and_then(|arg| arg.to_str()) != Some("terse") {
                    return Err(io::ErrorKind::InvalidInput.into());
                }
            }
            Some("--nocapture" | "--show-output" | "--quiet") => {}
            Some("--test-threads") => index += 1,
            Some(argument) if argument.starts_with("--test-threads=") => {}
            Some(argument) if argument.starts_with("--color=") => {}
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        }
        index += 1;
    }
    if ignored {
        return Ok(());
    }
    let selected: Vec<&str> = if let Some(name) = exact {
        if !test_names.contains(&name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "unknown exact test name",
            ));
        }
        vec![name]
    } else {
        test_names.to_vec()
    };
    if list {
        let mut stdout = io::stdout().lock();
        for name in selected {
            stdout.write_all(format!("{name}: test\n").as_bytes())?;
        }
        return Ok(());
    }
    for name in selected {
        io::stdout().lock().write_all(format!("test {name} ... ").as_bytes())?;
        run(name)?;
        io::stdout().lock().write_all(b"ok\n")?;
    }
    Ok(())
}

pub fn config_environment() -> Vec<OsString> {
    [
        "HOME",
        "PROVENANT_STATE_DIR",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "CURSOR_CONFIG_DIR",
        "AGY_HOME",
        "OPENCODE_CONFIG_DIR",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

pub fn protected_commands() -> Vec<String> {
    ["codex-code-mode-host", "codex-code-mode"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub fn environment(root: &Path) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ];
    for name in [
        "HOME",
        "PROVENANT_STATE_DIR",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "CURSOR_CONFIG_DIR",
        "AGY_HOME",
        "OPENCODE_CONFIG_DIR",
    ] {
        env.push((name.into(), root.join(name).into_os_string()));
    }
    env
}

pub fn argv(scenario: &str) -> io::Result<Vec<OsString>> {
    argv_with_args(scenario, &[])
}
pub fn argv_with_args(scenario: &str, extra: &[OsString]) -> io::Result<Vec<OsString>> {
    let mut argv = vec![
        std::env::current_exe()?.into_os_string(),
        "--fixture".into(),
        scenario.into(),
    ];
    argv.extend_from_slice(extra);
    Ok(argv)
}

pub fn command_with_args(
    scenario: &str,
    extra: &[OsString],
    root: &Path,
    new_session: bool,
) -> io::Result<Command> {
    let argv = argv_with_args(scenario, extra)?;
    let env = environment(root);
    crate::sys::fixture_command(&argv, &env, root, new_session)
}
pub fn command(scenario: &str, root: &Path) -> io::Result<Command> {
    command_with_args(scenario, &[], root, false)
}

#[cfg(feature = "fixture")]
pub struct ReaperFixture {
    reaper: crate::sys::reaper::Reaper,
}
#[cfg(feature = "fixture")]
impl ReaperFixture {
    pub fn new(capacity: usize) -> io::Result<Self> {
        Ok(Self {
            reaper: crate::sys::reaper::Reaper::new_paused(capacity)?,
        })
    }
    /// Transfer a spawned fixture child into the paused reaper.
    pub fn transfer(&self, scenario: &str, extra: &[OsString], root: &Path) -> io::Result<()> {
        crate::sys::raw_exec::verify_child_wait_policy()?;
        let permit = self
            .reaper
            .reserve()
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        let argv = argv_with_args(scenario, extra)?;
        let env = environment(root);
        let child = crate::sys::raw_exec::OwnedPid::spawn(
            permit,
            &argv,
            Some(&env),
            root,
            false,
            false,
            false,
        )?;
        drop(child);
        Ok(())
    }
    pub fn occupied(&self) -> usize {
        self.reaper.occupied_fixture()
    }
    pub fn resume(&self) {
        self.reaper.resume();
    }
    pub fn wait_for_occupied(
        &self,
        maximum: usize,
        timeout: std::time::Duration,
    ) -> io::Result<usize> {
        self.reaper.wait_for_occupied(maximum, timeout)
    }
}
#[cfg(feature = "fixture")]
impl Drop for ReaperFixture {
    fn drop(&mut self) {
        self.reaper.resume();
    }
}

#[cfg(feature = "fixture")]
pub fn fixture_accept(
    listener: &std::os::unix::net::UnixListener,
    timeout: std::time::Duration,
) -> io::Result<std::os::unix::net::UnixStream> {
    crate::sys::fixture_accept(listener, timeout)
}

#[cfg(feature = "fixture")]
pub fn fixture_read_byte(
    stream: &mut std::os::unix::net::UnixStream,
    timeout: std::time::Duration,
) -> io::Result<u8> {
    crate::sys::fixture_read_byte(stream, timeout)
}

/// Temporary-root marker driven publisher faults for the guardian integration test.
/// The production path never constructs this type.
pub struct ExitPublicationFaults {
    state_dir: PathBuf,
}
pub const S2_EXIT_PUBLICATION_CONFIG: &str = "s2-exit-publication.config";
pub const S2_EXIT_PUBLICATION_ENOSPC_ONCE: &str = "s2-exit-publication.enospc-once";
pub const S2_EXIT_PUBLICATION_SYNC_BLOCKED: &str = "s2-exit-publication.sync-blocked";
pub const S2_EXIT_PUBLICATION_DURABLE: &str = "s2-exit-publication.durable";
pub const S2_PROCESS_CHECKPOINT_ENOSPC: &str = "s2-process-checkpoint.enospc";
pub const S2_PROCESS_CHECKPOINT_FAILED: &str = "s2-process-checkpoint.failed";
pub const S2_RESERVATION_SYNC_BLOCKED: &str = "s2-reservation-sync-blocked";
pub const S2_RESERVATION_SYNC_PENDING: &str = "s2-reservation-sync-pending";

impl ExitPublicationFaults {
    pub fn from_state_dir(state_dir: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(state_dir.join(S2_EXIT_PUBLICATION_CONFIG)) {
            Ok(config) if config == b"enospc-post-link-sync-v1\n" => Ok(Some(Self {
                state_dir: state_dir.to_path_buf(),
            })),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown exit publication fixture config",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.state_dir.join(name)
    }
}
impl crate::ExitPublisher for ExitPublicationFaults {
    fn checkpoint(
        &self,
        directory: &crate::fs::Directory,
        bytes: &[u8],
    ) -> crate::custody::Result<crate::fs::Publication> {
        let fail = self.marker(S2_PROCESS_CHECKPOINT_ENOSPC);
        match marker_present(&fail) {
            Ok(true) => {
                std::fs::write(
                    self.marker(S2_PROCESS_CHECKPOINT_FAILED),
                    b"ENOSPC injected",
                )?;
                Err(injected_fs_error(
                    "fixture process checkpoint",
                    PathBuf::from("process.json"),
                    libc::ENOSPC,
                )
                .into())
            }
            Ok(false) => Ok(directory.atomic_write(
                std::ffi::OsStr::new("process.json"),
                ".process-",
                bytes,
                crate::fs::PublishMode::Replace,
            )?),
            Err(source) => Err(crate::fs::FsError::Io {
                operation: "read fixture checkpoint marker",
                path: fail,
                source,
            }
            .into()),
        }
    }

    fn publish(
        &self,
        directory: &crate::fs::Directory,
        bytes: &[u8],
    ) -> crate::custody::Result<crate::fs::Publication> {
        let once = self.marker(S2_EXIT_PUBLICATION_ENOSPC_ONCE);
        match std::fs::remove_file(&once) {
            Ok(()) => {
                return Err(injected_fs_error(
                    "fixture exit publication",
                    PathBuf::from("process-exit.json"),
                    libc::ENOSPC,
                )
                .into());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let blocked = self.marker(S2_EXIT_PUBLICATION_SYNC_BLOCKED);
        let publication = directory.atomic_write_with_sync(
            std::ffi::OsStr::new("process-exit.json"),
            ".exit-",
            bytes,
            crate::fs::PublishMode::NoReplace,
            || match marker_present(&blocked) {
                Ok(true) => Err(injected_fs_error(
                    "fixture post-link directory sync",
                    PathBuf::from("process-exit.json"),
                    libc::ENOSPC,
                )),
                Ok(false) => directory.sync(),
                Err(source) => Err(crate::fs::FsError::Io {
                    operation: "read fixture sync marker",
                    path: blocked.clone(),
                    source,
                }),
            },
        )?;
        Ok(publication)
    }

    fn commit_reservation(
        &self,
        directory: &crate::fs::Directory,
        bytes: &[u8],
    ) -> crate::custody::Result<crate::fs::Publication> {
        let blocked = self.marker(S2_RESERVATION_SYNC_BLOCKED);
        let pending = self.marker(S2_RESERVATION_SYNC_PENDING);
        let publication = directory.atomic_write_with_sync(
            std::ffi::OsStr::new("reservation.json"),
            ".reservation-",
            bytes,
            crate::fs::PublishMode::Replace,
            || match marker_present(&blocked) {
                Ok(true) => {
                    std::fs::write(&pending, b"reservation visible; directory sync blocked")
                        .map_err(|source| crate::fs::FsError::Io {
                            operation: "write fixture reservation sync marker",
                            path: pending.clone(),
                            source,
                        })?;
                    Err(injected_fs_error(
                        "fixture post-link reservation directory sync",
                        PathBuf::from("reservation.json"),
                        libc::ENOSPC,
                    ))
                }
                Ok(false) => directory.sync(),
                Err(source) => Err(crate::fs::FsError::Io {
                    operation: "read fixture reservation sync marker",
                    path: blocked.clone(),
                    source,
                }),
            },
        )?;
        Ok(publication)
    }

    fn sync_visible(&self, directory: &crate::fs::Directory) -> crate::custody::Result<()> {
        let blocked = self.marker(S2_EXIT_PUBLICATION_SYNC_BLOCKED);
        if marker_present(&blocked)? {
            return Err(injected_fs_error(
                "fixture visible exit sync",
                PathBuf::from("process-exit.json"),
                libc::ENOSPC,
            )
            .into());
        }
        directory.sync()?;
        std::fs::write(
            self.marker(S2_EXIT_PUBLICATION_DURABLE),
            b"directory sync completed",
        )?;
        Ok(())
    }
}
fn marker_present(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn injected_fs_error(operation: &'static str, path: PathBuf, errno: i32) -> crate::fs::FsError {
    crate::fs::FsError::Io {
        operation,
        path,
        source: io::Error::from_raw_os_error(errno),
    }
}

pub fn dispatch() -> io::Result<bool> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_none_or(|arg| arg != "--fixture") {
        return Ok(false);
    }
    let scenario = args
        .get(2)
        .and_then(|arg| arg.to_str())
        .ok_or(io::ErrorKind::InvalidInput)?;
    match scenario {
        "inheritance-parent" | "failed-replace" => {
            use std::os::fd::AsRawFd;
            let fd = crate::sys::fixture_parent_state()?;
            let raw = fd.as_raw_fd();
            let root = std::env::current_dir()?;
            if scenario == "failed-replace" {
                let error = crate::sys::replace_process(
                    &root.join("does-not-exist"),
                    &[],
                    &environment(&root),
                );
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(error);
                }
                crate::sys::fixture_check_inherited_state(raw, true)?;
            } else {
                let mut child = ChildGuard::new(
                    command_with_args(
                        "inheritance-probe",
                        &[raw.to_string().into()],
                        &root,
                        false,
                    )?
                    .spawn()?,
                );
                if !child.wait()?.success() {
                    return Err(io::Error::other(
                        "child inherited parent descriptors or signals",
                    ));
                }
            }
        }
        "inheritance-probe" => {
            let fd = args
                .get(3)
                .and_then(|arg| arg.to_str())
                .and_then(|arg| arg.parse::<i32>().ok())
                .ok_or(io::ErrorKind::InvalidInput)?;
            crate::sys::fixture_check_inherited_state(fd, false)?;
        }
        "echo" => {
            io::copy(&mut io::stdin().lock(), &mut io::stdout().lock())?;
            io::stderr().write_all(b"done\r\n")?;
        }
        "bytes" => {
            io::stdout().write_all(b"a\0b\xff\nterminal\n")?;
            io::stderr().write_all(b"err\r\n")?;
        }
        "flood" => {
            for _ in 0..32 {
                io::stdout().write_all(&[b'x'; 8192])?;
                io::stderr().write_all(&[b'e'; 8192])?;
            }
        }
        "blocked" => {
            crate::sys::fixture_ignore_term()?;
            io::stdout().write_all(b"ready\n")?;
            let mut byte = [0];
            loop {
                if io::stdin().read(&mut byte)? == 0 {
                    std::thread::park_timeout(std::time::Duration::from_secs(1));
                }
            }
        }
        "socket-blocked" => {
            crate::sys::fixture_ignore_term()?;
            let path = fixture_path(&args, 3)?;
            let mut stream = std::os::unix::net::UnixStream::connect(&path)?;
            stream.write_all(b"R")?;
            wait_for_eof(&mut stream)?;
        }
        "hold-stdout" => {
            let path = fixture_path(&args, 3)?;
            let mut stream = std::os::unix::net::UnixStream::connect(&path)?;
            stream.write_all(b"R")?;
            io::stdout().write_all(b"held\n")?;
            wait_for_eof(&mut stream)?;
            let mut done = std::os::unix::net::UnixStream::connect(path)?;
            done.write_all(b"D")?;
        }
        "exit-with-descendant" => {
            let socket = fixture_path(&args, 3)?.into_os_string();
            let exit_socket = fixture_path(&args, 4)?.into_os_string();
            let root = std::env::current_dir()?;
            let mut child =
                ChildGuard::new(command_with_args("hold-stdout", &[socket], &root, true)?.spawn()?);
            let mut exiting = std::os::unix::net::UnixStream::connect(exit_socket)?;
            exiting.write_all(b"E")?;
            child.detach();
        }
        "s2-setsid-descendant" => {
            crate::sys::fixture_ignore_term()?;
            let mut ready = std::os::unix::net::UnixStream::connect(fixture_path(&args, 3)?)?;
            ready.write_all(b"R")?;
            ready.write_all(std::process::id().to_string().as_bytes())?;
            ready.write_all(b"\n")?;
            wait_for_eof(&mut ready)?;
        }
        "s2-setsid-provider" => {
            let ready_socket = fixture_path(&args, 3)?.into_os_string();
            let gate_socket = fixture_path(&args, 4)?;
            let root = std::env::current_dir()?;
            let mut descendant = ChildGuard::new(
                command_with_args("s2-setsid-descendant", &[ready_socket], &root, true)?.spawn()?,
            );
            let mut gate = std::os::unix::net::UnixStream::connect(gate_socket)?;
            gate.write_all(b"R")?;
            wait_for_eof(&mut gate)?;
            descendant.detach();
        }
        "s2-registered-owner" => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(io::ErrorKind::NotFound)?;
            let directory = crate::fs::Directory::open_anchor(&home).map_err(io::Error::other)?;
            let clock = crate::clock::SystemClock::new();
            let cancel = provenant_core::CancelToken::new();
            let lock = directory
                .lock(
                    std::ffi::OsStr::new("s2-owner.lock"),
                    crate::fs::LockMode::Exclusive,
                    provenant_core::WaitBudget::cli(provenant_core::Clock::mono_now(&clock), 5)
                        .map_err(io::Error::other)?,
                    &cancel,
                    &clock,
                )
                .map_err(io::Error::other)?;
            let identity = match crate::census::ProcessObserver::observe(
                &RecordedBootObserver,
                i64::from(std::process::id()),
            ) {
                crate::census::ProcessObservation::Present(fact) => fact.identity,
                other => {
                    return Err(io::Error::other(format!(
                        "owner identity unavailable: {other:?}"
                    )));
                }
            };
            let bytes = serde_json::to_vec(&identity).map_err(io::Error::other)?;
            std::fs::write(home.join("s2-owner.json"), bytes)?;
            let mut ready = std::os::unix::net::UnixStream::connect(fixture_path(&args, 3)?)?;
            ready.write_all(b"R")?;
            wait_for_eof(&mut ready)?;
            drop(lock);
        }
        "s2-registered-provider" => {
            let ready_socket = fixture_path(&args, 3)?.into_os_string();
            let gate_socket = fixture_path(&args, 4)?;
            let root = std::env::current_dir()?;
            let mut owner = ChildGuard::new(
                command_with_args("s2-registered-owner", &[ready_socket], &root, false)?.spawn()?,
            );
            let mut gate = std::os::unix::net::UnixStream::connect(gate_socket)?;
            gate.write_all(b"R")?;
            wait_for_eof(&mut gate)?;
            owner.detach();
        }
        "s2-late-registered-provider" => {
            let ready_socket = fixture_path(&args, 3)?.into_os_string();
            let launch_gate = fixture_path(&args, 4)?;
            let hold_gate = fixture_path(&args, 5)?;
            let root = std::env::current_dir()?;
            let mut gate = std::os::unix::net::UnixStream::connect(launch_gate)?;
            gate.write_all(b"R")?;
            wait_for_eof(&mut gate)?;
            let mut owner = ChildGuard::new(
                command_with_args("s2-registered-owner", &[ready_socket], &root, false)?.spawn()?,
            );
            let mut gate = std::os::unix::net::UnixStream::connect(hold_gate)?;
            gate.write_all(b"R")?;
            wait_for_eof(&mut gate)?;
            owner.detach();
        }
        "wait-on-socket" => {
            let path = fixture_path(&args, 3)?;
            let mut stream = std::os::unix::net::UnixStream::connect(path)?;
            stream.write_all(b"R")?;
            wait_for_eof(&mut stream)?;
        }
        "terminal-stdin" => {
            io::stdout().write_all(b"terminal\n")?;
            let mut input = Vec::new();
            io::stdin().take(2 * 1024 * 1024).read_to_end(&mut input)?;
            io::stdout().write_all(b"EOF\n")?;
        }
        "terminal-blocked" => {
            io::stdout().write_all(b"terminal\n")?;
            let mut stream = std::os::unix::net::UnixStream::connect(fixture_path(&args, 3)?)?;
            stream.write_all(b"R")?;
            wait_for_eof(&mut stream)?;
        }
        "crash-guardian-provider" => {
            let mut stream = std::os::unix::net::UnixStream::connect(fixture_path(&args, 3)?)?;
            stream.write_all(b"R")?;
            io::stdout().write_all(b"crash-guardian\n")?;
            wait_for_eof(&mut stream)?;
        }
        "exit" => {}
        "marker" => std::fs::write(fixture_path(&args, 3)?, b"spawned")?,
        "wait-policy-ignore"
        | "wait-policy-no-wait"
        | "wait-policy-handler"
        | "wait-policy-blocked" => {
            crate::sys::fixture_wait_policy(scenario)?;
            let marker = fixture_path(&args, 3)?;
            let argv = argv_with_args("marker", &[marker.into_os_string()])?;
            let root = std::env::current_dir()?;
            let env = environment(&root);
            let executor = crate::exec::SystemExecutor::new(1).map_err(io::Error::other)?;
            let mut options =
                crate::exec::ExecOptions::new(&argv, &root, std::time::Duration::from_secs(2));
            options.env = Some(&env);
            use crate::exec::Executor;
            match executor.execute(options) {
                Err(crate::exec::ExecError::ChildWaitPolicy(_))
                    if matches!(scenario, "wait-policy-ignore" | "wait-policy-no-wait") => {}
                Ok(outcome)
                    if matches!(scenario, "wait-policy-blocked" | "wait-policy-handler")
                        && outcome.termination == crate::exec::Termination::Exit(0) => {}
                _ => return Err(io::Error::other("unexpected child-wait policy result")),
            }
        }
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    }
    Ok(true)
}
fn fixture_path(args: &[OsString], index: usize) -> io::Result<PathBuf> {
    args.get(index)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}
fn wait_for_eof(stream: &mut std::os::unix::net::UnixStream) -> io::Result<()> {
    let mut byte = [0; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(io::Error::from(io::ErrorKind::InvalidData)),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Recorded boot key with live native process facts, injected explicitly.
pub struct RecordedBootObserver;
impl crate::census::ProcessObserver for RecordedBootObserver {
    fn boot_identity(&self) -> Result<String, crate::census::CensusError> {
        Ok("fixture-recorded-boot-v1".into())
    }
    fn observe(&self, pid: i64) -> crate::census::ProcessObservation {
        match crate::sys::census::NativeProcessObserver::for_boot("fixture-recorded-boot-v1") {
            Ok(observer) => observer.observe(pid),
            Err(_) => crate::census::ProcessObservation::Unknown(
                crate::census::ObservationIssue::Malformed,
            ),
        }
    }
    fn census(&self, limit: usize) -> Result<crate::census::Census, crate::census::CensusError> {
        crate::sys::census::NativeProcessObserver::for_boot("fixture-recorded-boot-v1")?
            .census(limit)
    }
}

/// Fixture-only injection at the boot observation seam.
pub fn acquire_writer(
    directory: crate::fs::Directory,
    scope: crate::custody::Scope,
    budget: provenant_core::WaitBudget,
    cancel: &provenant_core::CancelToken,
    clock: &dyn provenant_core::Clock,
) -> crate::custody::Result<crate::custody::WriterLease> {
    let binding = crate::custody::WriterBinding::from_verified(
        &directory,
        &directory,
        "refs/heads/fixture",
        provenant_core::GitOid::Sha1([0; 20]),
    )?;
    crate::custody::acquire_writer_with_observer(
        directory,
        scope,
        binding,
        budget,
        cancel,
        clock,
        &RecordedBootObserver,
    )
}

pub fn recovery_observation(recovery: &crate::custody::Recovery) -> crate::custody::RecoveryState {
    recovery.observe_with(&RecordedBootObserver)
}
pub fn advance_attempt(
    writer: &mut crate::custody::WriterLease,
    next: crate::custody::Scope,
) -> crate::custody::Result<()> {
    writer.advance_with(next, &RecordedBootObserver)
}
