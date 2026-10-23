//! Dev-only fixtures: dispatch scenario arguments before the test suite.
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

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
            let mut child = command_with_args("hold-stdout", &[socket], &root, true)?;
            let _ = child.spawn()?;
            let mut exiting = std::os::unix::net::UnixStream::connect(exit_socket)?;
            exiting.write_all(b"E")?;
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
                    if scenario != "wait-policy-blocked" => {}
                Ok(outcome)
                    if scenario == "wait-policy-blocked"
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
