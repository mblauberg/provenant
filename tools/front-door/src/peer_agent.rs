//! Keychain-capable peer execution. On macOS a process started from an SSH
//! session cannot read the login keychain, so Claude reports no login there.
//! A per-user LaunchAgent in the GUI domain can. The agent listens on a private
//! Unix socket under the state root and relays each framed `fabric peer`
//! request to a fresh `fabric peer --in-process` child, so the request, its
//! credential reads and the lanes it launches all run in the login session.
//! The SSH entrypoint forwards when the agent accepts a connection and handles
//! the request itself otherwise.
use provenant_fabric_runtime::hosts::MAX_FRAME;
use provenant_supervisor::exec::{
    ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
};
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;

pub const LABEL: &str = "dev.provenant.peer";
const SOCKET: &str = "peer.sock";
const LOG: &str = "agent.log";
/// Bound on one relayed request; the caller's SSH budget is shorter.
const REQUEST: Duration = Duration::from_secs(90);
const CONCURRENT: usize = 16;
const REFUSING: usize = 32;
/// Written to a connection refused at the cap. It is not a `PeerResponse`, so
/// the caller recognises it and builds the busy reply itself; the agent never
/// parses a request.
const BUSY: &[u8] = b"{\"agent\":\"busy\"}\n";
/// Already-buffered request bytes discarded before an at-cap refusal closes.
/// Closing with unread data resets the connection, and on Linux the reset
/// discards the busy frame before the caller can read it.
const BUFFERED_DRAIN: usize = 64 * 1024;
/// Pause after a failed `accept`, and the run of consecutive failures after
/// which the agent exits so launchd restarts it.
const ACCEPT_PAUSE: Duration = Duration::from_millis(100);
const ACCEPT_FAILURES: u32 = 50;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("peer agent directory {0} is not private to this user")]
    NotPrivate(PathBuf),
    #[error("a peer agent is already serving {0}")]
    Running(PathBuf),
    #[error("peer agent closed without a reply; see {0}")]
    NoReply(PathBuf),
    #[error("peer agent frame exceeds its bound")]
    Frame,
    #[error("peer agent path is not UTF-8: {0}")]
    Path(PathBuf),
    #[error("launchctl {0} failed: {1}")]
    Launchctl(&'static str, String),
    #[error(transparent)]
    Exec(#[from] provenant_supervisor::exec::ExecError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn directory(state: &Path) -> PathBuf {
    state.join("peer-agent")
}

/// Owned by this user and closed to everyone else, so no other account can
/// plant the socket or connect to it.
fn private(directory: &Path) -> bool {
    fs::symlink_metadata(directory).is_ok_and(|meta| {
        meta.is_dir()
            && meta.uid() == provenant_supervisor::fs::effective_uid()
            && meta.mode() & 0o077 == 0
    })
}

/// Read to EOF within `timeout`, bounding each blocking read by the time left.
fn read_bounded(stream: &mut UnixStream, timeout: Duration) -> Result<Vec<u8>, AgentError> {
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::from(io::ErrorKind::TimedOut).into());
        }
        let read = match stream.set_read_timeout(Some(remaining)) {
            Ok(()) => stream.read(&mut chunk),
            Err(error)
                if cfg!(target_os = "macos") && error.kind() == io::ErrorKind::InvalidInput =>
            {
                // macOS rejects setsockopt after the peer closes. Drain any
                // buffered reply without risking a read past the deadline.
                stream.set_nonblocking(true)?;
                let result = stream.read(&mut chunk);
                stream.set_nonblocking(false)?;
                result
            }
            Err(error) => return Err(error.into()),
        };
        match read {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                bytes.extend_from_slice(&chunk[..count]);
                if bytes.len() > MAX_FRAME {
                    return Err(AgentError::Frame);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// Where one framed request went. Nothing is sent before the connection is
/// accepted, so only `Unreachable` may fall back to in-process handling; a
/// failure after that is an error, never a second attempt. `Busy` means the
/// agent refused the request at its cap without reading it, so nothing ran and
/// the caller reports a retryable busy, not an unknown outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum Forward {
    Replied(Vec<u8>),
    Busy,
    Unreachable,
}

pub fn forward(state: &Path, request: &[u8]) -> Result<Forward, AgentError> {
    let directory = directory(state);
    if !private(&directory) {
        return Ok(Forward::Unreachable);
    }
    let Ok(mut stream) = UnixStream::connect(directory.join(SOCKET)) else {
        return Ok(Forward::Unreachable);
    };
    stream.set_write_timeout(Some(REQUEST))?;
    stream.set_read_timeout(Some(REQUEST))?;
    if let Err(error) = stream.write_all(request) {
        // A refusal at the cap closes unread, so a request larger than the
        // socket buffers fails to write; the busy frame is still waiting.
        // Only `refuse` writes it, so seeing it proves nothing ran.
        if matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        ) && read_bounded(&mut stream, Duration::from_secs(5)).is_ok_and(|reply| reply == BUSY)
        {
            return Ok(Forward::Busy);
        }
        return Err(error.into());
    }
    forward_reply(&mut stream, &directory.join(LOG))
}

fn forward_reply(stream: &mut UnixStream, log: &Path) -> Result<Forward, AgentError> {
    if let Err(error) = stream.shutdown(Shutdown::Write) {
        // Refusing at both caps closes with the request unread. The peer may
        // already be disconnected while its busy reply is still buffered.
        if !matches!(
            error.kind(),
            io::ErrorKind::NotConnected
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::ConnectionReset
        ) {
            return Err(error.into());
        }
    }
    let reply = read_bounded(stream, REQUEST + Duration::from_secs(5))?;
    if reply.is_empty() {
        return Err(AgentError::NoReply(log.into()));
    }
    if reply == BUSY {
        return Ok(Forward::Busy);
    }
    Ok(Forward::Replied(reply))
}

fn log(message: &str) {
    let _ = io::stderr()
        .lock()
        .write_all(format!("provenant: fabric peer-agent: {message}\n").as_bytes());
}

/// Run one request in a fresh child of this login-session process. An empty
/// request is a liveness probe. The reply is written only after the child
/// exits cleanly; otherwise the connection closes empty and the caller treats
/// the outcome as unknown.
fn relay(
    mut stream: UnixStream,
    program: &Path,
    state: &Path,
    cwd: &Path,
) -> Result<(), AgentError> {
    let timeout = Some(Duration::from_secs(30));
    // macOS refuses the timeouts once a liveness probe has already closed.
    if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
        return Ok(());
    }
    let request = read_bounded(&mut stream, Duration::from_secs(30))?;
    if request.is_empty() {
        return Ok(());
    }
    let argv: [OsString; 6] = [
        program.into(),
        "--state-dir".into(),
        state.into(),
        "fabric".into(),
        "peer".into(),
        "--in-process".into(),
    ];
    let mut options = ExecOptions::new(&argv, cwd, REQUEST);
    options.stdin = Some(&request);
    options.stdout = StreamPolicy::Capture { limit: MAX_FRAME };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    options.stop_on_output_limit = true;
    let outcome = SystemExecutor::new(1)?.execute(options)?;
    if outcome.termination != Termination::Exit(0)
        || outcome.timed_out
        || outcome.drain_truncated
        || outcome.stdout.truncated
        || outcome.stdout.read_failed
    {
        log(&format!(
            "request failed ({:?}): {}",
            outcome.termination,
            String::from_utf8_lossy(&outcome.stderr.bytes).trim_end()
        ));
        return Ok(());
    }
    stream.write_all(&outcome.stdout.bytes)?;
    Ok(())
}

/// Refuse a connection at the cap with the busy frame. Best effort: a caller
/// that has already gone away needs no reply. The request is then read and
/// discarded, briefly and within the frame bound: closing with unread data
/// resets the connection, and on Linux the reset can discard the busy frame
/// before the caller reads it. It runs on its own thread so a slow caller
/// cannot stall the accept loop.
fn refuse(mut stream: UnixStream, refusing: &Arc<AtomicUsize>) {
    if refusing
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            (count < REFUSING).then_some(count + 1)
        })
        .is_err()
    {
        // At the refusal cap, a best-effort busy frame must not stall accept.
        if stream.set_nonblocking(true).is_ok() {
            drain_buffered(&mut stream);
            let _ = stream.write(BUSY);
            let _ = stream.shutdown(Shutdown::Write);
        }
        return;
    }
    let refusing = Arc::clone(refusing);
    std::thread::spawn(move || {
        let timeout = Some(Duration::from_secs(1));
        if stream.set_write_timeout(timeout).is_ok()
            && stream.set_read_timeout(timeout).is_ok()
            && stream.write_all(BUSY).is_ok()
        {
            let _ = stream.shutdown(Shutdown::Write);
            let _ = read_bounded(&mut stream, Duration::from_secs(2));
        }
        refusing.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Read without blocking whatever the caller has already sent, up to
/// `BUFFERED_DRAIN`, so closing the refused stream does not reset it.
fn drain_buffered(stream: &mut UnixStream) {
    let mut buffer = [0; 4096];
    let mut drained = 0;
    while drained < BUFFERED_DRAIN {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => drained += read,
        }
    }
}

/// Count consecutive `accept` failures: pause and carry on, then give up once
/// the run is long enough that the cause (such as descriptor exhaustion) is
/// not passing. A success resets the run.
fn accepted<T>(
    result: io::Result<T>,
    failures: &mut u32,
    pause: Duration,
) -> Result<Option<T>, AgentError> {
    match result {
        Ok(value) => {
            *failures = 0;
            Ok(Some(value))
        }
        Err(error) => {
            log(&format!("accept: {error}"));
            *failures += 1;
            if *failures >= ACCEPT_FAILURES {
                return Err(error.into());
            }
            std::thread::park_timeout(pause);
            Ok(None)
        }
    }
}

fn listen(state: &Path) -> Result<UnixListener, AgentError> {
    let directory = directory(state);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    if !private(&directory) {
        return Err(AgentError::NotPrivate(directory));
    }
    let socket = directory.join(SOCKET);
    if UnixStream::connect(&socket).is_ok() {
        return Err(AgentError::Running(socket));
    }
    match fs::remove_file(&socket) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn serve_state(state: &Path) -> Result<(), AgentError> {
    serve_listener(listen(state)?, state)
}

fn serve_listener(listener: UnixListener, state: &Path) -> Result<(), AgentError> {
    let program = std::env::current_exe()?;
    let cwd = crate::roots::home().map_err(|error| io::Error::other(error.to_string()))?;
    let active = Arc::new(AtomicUsize::new(0));
    let refusing = Arc::new(AtomicUsize::new(0));
    let mut failures = 0;
    loop {
        let Some((stream, _)) = accepted(listener.accept(), &mut failures, ACCEPT_PAUSE)? else {
            continue;
        };
        // Over the cap the request is refused unread, with a busy frame.
        if active.fetch_add(1, Ordering::SeqCst) >= CONCURRENT {
            active.fetch_sub(1, Ordering::SeqCst);
            refuse(stream, &refusing);
            continue;
        }
        let (program, state, cwd, active) = (
            program.clone(),
            state.to_path_buf(),
            cwd.clone(),
            Arc::clone(&active),
        );
        std::thread::spawn(move || {
            if let Err(error) = relay(stream, &program, &state, &cwd) {
                log(&error.to_string());
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// The LaunchAgent's long-running entrypoint, `fabric peer-agent`.
pub fn serve(globals: &provenant_cli::Globals) -> u8 {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if provenant_cli::fabric::in_lane() {
            return Err(
                provenant_fabric_runtime::coordination::CoordinationError::LaneMutation.into(),
            );
        }
        let home = crate::roots::home()?;
        let assets = crate::roots::product_root()?;
        let state = crate::roots::roots(globals, &assets, &home)?.state;
        Ok(serve_state(state.as_path())?)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            log(&error.to_string());
            3
        }
    }
}

fn utf8(path: &Path) -> Result<&str, AgentError> {
    path.to_str().ok_or_else(|| AgentError::Path(path.into()))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent's property list. `AbandonProcessGroup` keeps lanes running when
/// launchd restarts the agent. Composition supplies a fixed absolute PATH.
pub fn render(program: &Path, state: &Path, path: &OsStr) -> Result<String, AgentError> {
    let program = xml(utf8(program)?);
    let log = xml(utf8(&directory(state).join(LOG))?);
    let state = xml(utf8(state)?);
    let path = xml(path.to_str().ok_or_else(|| AgentError::Path(path.into()))?);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>fabric</string>
    <string>peer-agent</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>PROVENANT_STATE_DIR</key>
    <string>{state}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>AbandonProcessGroup</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    ))
}

pub fn plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn launchctl(args: &[&OsStr], home: &Path) -> Result<(bool, String), AgentError> {
    let mut argv: Vec<OsString> = vec!["/bin/launchctl".into()];
    argv.extend(args.iter().map(|arg| arg.to_os_string()));
    let env: [(OsString, OsString); 2] = [
        ("HOME".into(), home.into()),
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
    ];
    let mut options = ExecOptions::new(&argv, home, Duration::from_secs(10));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let outcome = SystemExecutor::new(1)?.execute(options)?;
    Ok((
        outcome.termination == Termination::Exit(0) && !outcome.timed_out,
        String::from_utf8_lossy(&outcome.stderr.bytes)
            .trim()
            .to_owned(),
    ))
}

/// Start the receipt-owned plist after managed publication has succeeded.
fn bootstrap(state: &Path, home: &Path, changed: bool) -> Result<(), AgentError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory(state))?;
    let plist = plist(home);
    let domain = format!("gui/{}", provenant_supervisor::fs::effective_uid());
    let service = format!("{domain}/{LABEL}");
    let loaded = launchctl(&["print".as_ref(), service.as_ref()], home)?.0;
    if loaded && !changed {
        let (ok, error) = launchctl(
            &["kickstart".as_ref(), "-k".as_ref(), service.as_ref()],
            home,
        )?;
        return if ok {
            Ok(())
        } else {
            Err(AgentError::Launchctl("kickstart", error))
        };
    }
    if loaded {
        launchctl(&["bootout".as_ref(), service.as_ref()], home)?;
    }
    // A just-booted-out service can refuse bootstrap briefly while it exits.
    let mut last = String::new();
    for _ in 0..5 {
        let (ok, error) = launchctl(
            &["bootstrap".as_ref(), domain.as_ref(), plist.as_os_str()],
            home,
        )?;
        if ok {
            return Ok(());
        }
        last = error;
        std::thread::park_timeout(Duration::from_millis(300));
    }
    Err(AgentError::Launchctl("bootstrap", last))
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Installed {
    Started,
    /// Not macOS, or not this account's default state root.
    Skipped,
    Failed {
        message: String,
    },
}

impl Installed {
    pub fn describe(&self) -> String {
        match self {
            Self::Started => format!("{LABEL} started in the login session"),
            Self::Skipped => "not installed (macOS default state root only)".into(),
            Self::Failed { message } => format!("failed: {message}"),
        }
    }
}

/// The installer's step. Only an install for this account's home and default
/// state root owns the single per-user agent, so test and worktree installs
/// with their own roots leave it alone.
pub fn eligible(state: &Path, home: &Path) -> bool {
    let Ok(home) = fs::canonicalize(home) else {
        return false;
    };
    let home = home.as_path();
    let account_home =
        provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Current)
            .ok()
            .flatten()
            .and_then(|account| fs::canonicalize(PathBuf::from(account.home)).ok());
    if !cfg!(target_os = "macos")
        || account_home.as_deref() != Some(home)
        || state != home.join(".local/share/provenant/v1")
    {
        return false;
    }
    true
}

pub fn install(state: &Path, home: &Path, changed: bool) -> Installed {
    if !eligible(state, home) {
        return Installed::Skipped;
    }
    match bootstrap(state, home, changed) {
        Ok(()) => Installed::Started,
        Err(error) => Installed::Failed {
            message: error.to_string(),
        },
    }
}

/// Only fixed system directories, the install bin and known executable parents
/// enter the agent environment. Relative, empty and missing directories vanish.
fn sanitised_path(bin: &Path, executables: &[PathBuf]) -> Result<OsString, AgentError> {
    let mut directories: Vec<PathBuf> = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .map(PathBuf::from)
        .collect();
    for directory in std::iter::once(bin).chain(
        executables
            .iter()
            .filter_map(|path| path.is_absolute().then(|| path.parent()).flatten()),
    ) {
        if directory.is_absolute()
            && (directory == bin || directory.is_dir())
            && !directory.as_os_str().as_encoded_bytes().contains(&b':')
            && !directories.iter().any(|existing| existing == directory)
        {
            directories.push(directory.to_path_buf());
        }
    }
    std::env::join_paths(directories).map_err(|error| io::Error::other(error).into())
}

fn provider_executables(search: &OsStr) -> Vec<PathBuf> {
    let mut executables = Vec::new();
    // Resolve only through absolute PATH directories. Never turn a relative
    // shell entry into an absolute agent search directory by canonicalising it.
    for name in ["claude", "codex"] {
        if let Some(program) = std::env::split_paths(search)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|path| path.is_file() && provenant_supervisor::sys::is_executable(path))
        {
            executables.push(program);
        }
    }
    executables
}

pub fn install_plist(program: &Path, state: &Path) -> Result<Vec<u8>, AgentError> {
    let search = std::env::var_os("PATH").unwrap_or_default();
    let mut executables = provider_executables(&search);
    executables.extend(crate::roots::git_binary());
    let bin = program
        .parent()
        .ok_or_else(|| AgentError::Path(program.into()))?;
    Ok(render(program, state, &sanitised_path(bin, &executables)?)?.into_bytes())
}

fn historical_plist(bytes: &[u8], program: &Path, state: &Path) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (_, value) = text.split_once("<key>PATH</key>\n    <string>")?;
    let (path, _) = value.split_once("</string>")?;
    let path = path
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&");
    (render(program, state, OsStr::new(&path)).ok()?.as_bytes() == bytes).then(|| bytes.to_vec())
}

/// Recognise only the historical template for the validated installed binary
/// and this state root. Read through held no-follow directory capabilities.
pub fn previous_plist(
    program: &Path,
    state: &Path,
    home: &Path,
) -> Result<Option<Vec<u8>>, AgentError> {
    let path = plist(home);
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Path(path.clone()))?;
    let dir = match provenant_config::private_directory(
        parent,
        false,
        &provenant_supervisor::fs::LocalFs,
    ) {
        Ok(dir) => dir,
        Err(provenant_config::InstallError::Fs(error)) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(io::Error::other(error).into()),
    };
    let file = match dir.open_read(OsStr::new("dev.provenant.peer.plist")) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(io::Error::other(error).into()),
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Ok(None);
    }
    Ok(historical_plist(&bytes, program, state))
}

/// The managed-file owner calls this only for an exact plist match, under its
/// install lock. A missing service needs no bootout; a failure keeps the plist.
pub fn stop(home: &Path) -> Result<(), AgentError> {
    stop_with(home, launchctl)
}

fn stop_with(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let service = format!("gui/{}/{LABEL}", provenant_supervisor::fs::effective_uid());
    if run(&["print".as_ref(), service.as_ref()], home)?.0 {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Stopped,
    NotInstalled,
    Unsupported,
}

/// Doctor's view: a connection the agent accepts is running. The empty probe
/// runs no request.
pub fn status(state: &Path, home: &Path) -> Status {
    if !cfg!(target_os = "macos") {
        return Status::Unsupported;
    }
    let directory = directory(state);
    if private(&directory) && UnixStream::connect(directory.join(SOCKET)).is_ok() {
        return Status::Running;
    }
    if plist(home).is_file() {
        Status::Stopped
    } else {
        Status::NotInstalled
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn read_bounded_drains_a_closed_peer() {
        use super::*;
        let (mut stream, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"reply\n").unwrap();
        drop(peer);
        assert_eq!(
            read_bounded(&mut stream, Duration::from_millis(200)).unwrap(),
            b"reply\n"
        );
    }

    #[test]
    fn read_bounded_times_out_an_idle_connection_within_its_deadline() {
        use super::*;
        let (mut stream, _held) = UnixStream::pair().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let started = Instant::now();
        let result = read_bounded(&mut stream, Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            result,
            Err(AgentError::Io(error))
                if matches!(error.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
        ));
        assert!(matches!(
            read_bounded(&mut stream, Duration::ZERO),
            Err(AgentError::Io(error)) if error.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn historical_plist_recognises_current_release_golden_bytes() {
        // A plist written by the released installer, with only HOME rewritten.
        // Keep these bytes independent of render(), including the final newline.
        let bytes = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.provenant.peer</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/example/.local/bin/provenant</string>
    <string>fabric</string>
    <string>peer-agent</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/Users/example/.local/bin:/Users/example/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/System/Cryptexes/App/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/local/bin:/Applications/Obsidian.app/Contents/MacOS</string>
    <key>PROVENANT_STATE_DIR</key>
    <string>/Users/example/.local/share/provenant/v1</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>AbandonProcessGroup</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>/Users/example/.local/share/provenant/v1/peer-agent/agent.log</string>
</dict>
</plist>
"#;
        assert_eq!(
            super::historical_plist(
                bytes,
                std::path::Path::new("/Users/example/.local/bin/provenant"),
                std::path::Path::new("/Users/example/.local/share/provenant/v1"),
            ),
            Some(bytes.to_vec())
        );
    }

    #[test]
    fn historical_plist_requires_the_exact_template_and_known_binary_and_state() {
        use super::*;
        let program = Path::new("/home/test/.local/bin/provenant");
        let state = Path::new("/home/test/.local/share/provenant/v1");
        let rendered = render(program, state, OsStr::new(":relative:/a&b:/c<d:/e>f")).unwrap();
        assert_eq!(
            historical_plist(rendered.as_bytes(), program, state),
            Some(rendered.as_bytes().to_vec())
        );
        assert!(historical_plist(format!("{rendered}\n").as_bytes(), program, state).is_none());
        assert!(
            historical_plist(rendered.as_bytes(), Path::new("/foreign/provenant"), state).is_none()
        );
        assert!(
            historical_plist(rendered.as_bytes(), program, Path::new("/foreign/state")).is_none()
        );
    }

    #[test]
    fn provider_path_preserves_a_symlinked_command_name_and_drops_other_search_entries() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("tools");
        fs::create_dir(&bin).unwrap();
        let target = root.path().join("version-1.2.3");
        fs::write(&target, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, bin.join("claude")).unwrap();
        let search = OsString::from(format!(
            ":relative:{}:{}",
            root.path().display(),
            bin.display()
        ));
        let tools = provider_executables(&search);
        assert_eq!(tools, vec![bin.join("claude")]);
        let path = sanitised_path(Path::new("/install/bin"), &tools).unwrap();
        let entries = std::env::split_paths(&path).collect::<Vec<_>>();
        assert!(entries.contains(&bin));
        assert!(!entries.contains(&root.path().to_path_buf()));
        assert!(entries.iter().all(|entry| entry.is_absolute()));
    }

    #[test]
    fn agent_path_keeps_only_absolute_known_directories_in_fixed_order() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("new-bin");
        let providers = root.path().join("providers");
        fs::create_dir(&providers).unwrap();
        let executables = [
            providers.join("claude"),
            providers.join("codex"),
            PathBuf::from("relative/bin/claude"),
            PathBuf::from("codex"),
            root.path().join("absent/git"),
            PathBuf::from("/usr/bin/git"),
        ];
        let path = sanitised_path(&bin, &executables).unwrap();
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/usr/sbin"),
                PathBuf::from("/sbin"),
                bin,
                providers,
            ]
        );
        assert_eq!(
            sanitised_path(Path::new(""), &[]).unwrap(),
            OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")
        );
        assert_eq!(
            sanitised_path(Path::new("relative"), &[]).unwrap(),
            OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")
        );
    }

    #[test]
    fn uninstall_boots_out_a_loaded_agent_and_reports_failure() {
        use super::*;
        let home = Path::new("/home/test");
        let mut calls = Vec::new();
        stop_with(home, |args, cwd| {
            assert_eq!(cwd, home);
            calls.push(
                args.iter()
                    .map(|arg| arg.to_os_string())
                    .collect::<Vec<_>>(),
            );
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0][0], "print");
        assert_eq!(calls[1][0], "bootout");
        assert!(
            calls[1][1]
                .to_string_lossy()
                .ends_with("/dev.provenant.peer")
        );
        let mut calls = 0;
        stop_with(home, |_, _| {
            calls += 1;
            Ok((false, String::new()))
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert!(matches!(
            stop_with(home, |args, _| {
                Ok((args[0] == OsStr::new("print"), "denied".into()))
            }),
            Err(AgentError::Launchctl("bootout", _))
        ));
    }

    #[test]
    fn forward_falls_back_only_when_no_agent_accepts() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        assert_eq!(forward(root.path(), b"x").unwrap(), Forward::Unreachable);
        let directory = directory(root.path());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        assert_eq!(forward(root.path(), b"x").unwrap(), Forward::Unreachable);

        // A public directory is not trusted even with a live socket in it.
        let listener = UnixListener::bind(directory.join(SOCKET)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(forward(root.path(), b"x").unwrap(), Forward::Unreachable);
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

        let server = std::thread::spawn(move || {
            for reply in [&b"reply\n"[..], b""] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                stream.read_to_end(&mut request).unwrap();
                assert_eq!(request, b"request");
                stream.write_all(reply).unwrap();
            }
        });
        assert_eq!(
            forward(root.path(), b"request").unwrap(),
            Forward::Replied(b"reply\n".to_vec())
        );
        // Once connected, a missing reply is an error, never a fallback.
        assert!(matches!(
            forward(root.path(), b"request"),
            Err(AgentError::NoReply(_))
        ));
        server.join().unwrap();
    }

    #[test]
    fn forward_reads_busy_after_a_written_request_is_refused_at_both_caps() {
        use super::*;
        let (mut client, server) = UnixStream::pair().unwrap();
        // The request is buffered before accept/refuse, so closing the server
        // leaves it unread and disconnects the caller on macOS.
        client.write_all(b"request").unwrap();
        let refusing = Arc::new(AtomicUsize::new(REFUSING));
        refuse(server, &refusing);
        assert_eq!(refusing.load(Ordering::SeqCst), REFUSING);
        assert_eq!(
            forward_reply(&mut client, Path::new("agent.log")).unwrap(),
            Forward::Busy
        );
    }

    #[test]
    fn refusal_threads_are_bounded_while_64_connections_are_held() {
        use super::*;
        let refusing = Arc::new(AtomicUsize::new(0));
        let mut refused = Vec::new();
        let mut peak = 0;
        for _ in 0..64 {
            let (mut client, stream) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            refuse(stream, &refusing);
            peak = peak.max(refusing.load(Ordering::SeqCst));
            assert!(peak <= REFUSING);
            let mut busy = [0; BUSY.len()];
            client.read_exact(&mut busy).unwrap();
            assert_eq!(busy, BUSY);
            refused.push(client);
        }
        assert_eq!(refused.len(), 64);
        drop(refused);
    }

    #[test]
    fn a_request_refused_at_the_cap_is_busy_not_unknown() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let state = root.path().to_path_buf();
        // Listen before serving, so no readiness probe takes a transient slot.
        let listener = listen(&state).unwrap();
        let server_state = state.clone();
        std::thread::spawn(move || {
            let _ = serve_listener(listener, &server_state);
        });
        let socket = directory(&state).join(SOCKET);
        // Hold the cap with connections that never finish their request. The
        // server accepts its queue in order, so each later request is refused.
        let held: Vec<UnixStream> = (0..CONCURRENT)
            .map(|_| UnixStream::connect(&socket).unwrap())
            .collect();
        assert_eq!(forward(&state, b"x").unwrap(), Forward::Busy);
        // A request beyond the socket buffers must not lose the busy reply.
        assert_eq!(
            forward(&state, &vec![b'x'; 256 * 1024]).unwrap(),
            Forward::Busy
        );
        drop(held);
    }

    #[test]
    fn accept_errors_pause_then_give_up_and_success_resets() {
        use super::*;
        let fail = || Err::<(), _>(io::Error::from_raw_os_error(24));
        let mut failures = 0;
        for _ in 0..ACCEPT_FAILURES - 1 {
            assert!(matches!(
                accepted(fail(), &mut failures, Duration::ZERO),
                Ok(None)
            ));
        }
        assert!(matches!(
            accepted(Ok(()), &mut failures, Duration::ZERO),
            Ok(Some(()))
        ));
        assert_eq!(failures, 0);
        for _ in 0..ACCEPT_FAILURES - 1 {
            assert!(accepted(fail(), &mut failures, Duration::ZERO).is_ok());
        }
        assert!(accepted(fail(), &mut failures, Duration::ZERO).is_err());
    }

    #[test]
    fn listen_replaces_a_stale_socket_but_not_a_live_agent() {
        use super::*;
        let root = tempfile::tempdir().unwrap();
        let directory = directory(root.path());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        drop(UnixListener::bind(directory.join(SOCKET)).unwrap());
        let live = listen(root.path()).unwrap();
        assert!(matches!(listen(root.path()), Err(AgentError::Running(_))));
        let mode = fs::metadata(directory.join(SOCKET)).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(live);
    }

    #[test]
    fn plist_runs_the_agent_in_the_gui_session_with_escaped_paths() {
        use super::*;
        let plist = render(
            Path::new("/Users/a&b/.local/bin/provenant"),
            Path::new("/Users/a&b/.local/share/provenant/v1"),
            OsStr::new("/opt/homebrew/bin:/usr/bin"),
        )
        .unwrap();
        assert!(plist.contains("<string>dev.provenant.peer</string>"));
        assert!(plist.contains(
            "<string>/Users/a&amp;b/.local/bin/provenant</string>\n    <string>fabric</string>\n    <string>peer-agent</string>"
        ));
        assert!(plist.contains("<key>PATH</key>\n    <string>/opt/homebrew/bin:/usr/bin</string>"));
        assert!(plist.contains("<string>/Users/a&amp;b/.local/share/provenant/v1</string>"));
        for key in ["RunAtLoad", "KeepAlive", "AbandonProcessGroup"] {
            assert!(
                plist.contains(&format!("<key>{key}</key>\n  <true/>")),
                "{key}"
            );
        }
        assert!(plist.contains("/v1/peer-agent/agent.log</string>"));
        assert!(!plist.contains("a&b"));
    }
}
