//! Read-only operating-system observations used by worker liveness.
//!
//! These functions are deliberately outside the liveness policy. They collect
//! filesystem facts and run a small, closed set of diagnostic commands. The
//! command and filesystem traits keep those effects fixture-driven in tests.

use super::{require_inspection_quiescent, LivenessError, Process};
use crate::exec::{run_text, ExecOptions, SystemExecutor, Termination, TextError};
use crate::sys::{self, Account};
use jiff::tz::TimeZone;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// Diagnostic commands are killed after this budget, including a grace period.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_OUTPUT_LIMIT: i64 = 16 * 1024 * 1024;
const SESSION_FIRST_LINE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
struct FirstLineTooLarge;

impl std::fmt::Display for FirstLineTooLarge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("session log first line exceeds limit")
    }
}

impl std::error::Error for FirstLineTooLarge {}

#[derive(Debug, Error)]
pub enum ObserveError {
    #[error("filesystem {operation} failed for {path:?}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Could not determine home directory.")]
    HomeDirectory { path: PathBuf },
    #[error("observed cwd is not valid UTF-8: {path:?}")]
    NonUtf8Path { path: PathBuf },
    #[error("process command is not valid UTF-8")]
    NonUtf8Command { command: OsString },
    #[error("could not decode UTF-8 output from {source_name}")]
    Utf8 {
        source_name: String,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("session log record has an invalid object shape: {path:?}")]
    RecordShape { path: PathBuf },
    #[error("session log first line exceeds {limit} bytes: {path:?}")]
    RecordTooLarge { path: PathBuf, limit: u64 },
    #[error("JSON integer is outside i64/u64 range in session log: {value} ({path:?})")]
    NumberOutOfRange { path: PathBuf, value: String },
    #[error("session log file size is outside i64 range: {path:?}")]
    FileSizeOutOfRange { path: PathBuf },
    #[error("diagnostic command timed out: {program}")]
    TimedOut { program: String },
    #[error("diagnostic command output exceeds {limit} bytes: {program}")]
    CommandOutputLimit { program: String, limit: i64 },
    #[error(transparent)]
    Command(#[from] TextError),
    #[error("cannot inspect processes: {0}")]
    ProcessCensus(#[source] crate::process_info::ProcessError),
    #[error(transparent)]
    Liveness(#[from] LivenessError),
    #[error(transparent)]
    Timestamp(#[from] provenant_core::py::DateTimeError),
}

fn io_error<'a>(
    operation: &'static str,
    path: &'a Path,
) -> impl FnOnce(io::Error) -> ObserveError + 'a {
    move |source| ObserveError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedMetadata {
    /// `st_mtime` with the same floating-seconds shape exposed by Python.
    pub modified_seconds: f64,
    pub size: u64,
}

/// Filesystem behaviour used by the observation adapter.
///
/// This API follows symlinks where `pathlib.Path` does. `SafeFs::open_source`
/// deliberately rejects final symlinks, so the diagnostic reader uses the
/// ordinary read-only path operations here to preserve the source contract.
pub trait ObservationFs: Send + Sync {
    fn current_dir(&self) -> io::Result<PathBuf>;
    fn is_linux(&self) -> bool;
    /// Unsupported by default so filesystem fixtures can retain lsof inputs.
    fn pid_cwd(&self, _pid: i64) -> Result<crate::pid::CwdObservation, crate::pid::PidError> {
        Ok(crate::pid::CwdObservation::Unsupported)
    }
    fn exists(&self, path: &Path) -> bool;
    fn resolve_non_strict(&self, path: &Path) -> io::Result<PathBuf>;
    fn lookup_home(&self, user: Option<&OsStr>) -> io::Result<Option<PathBuf>>;
    fn is_dir(&self, path: &Path) -> bool;
    fn walk_session_logs(&self, root: &Path) -> io::Result<Vec<PathBuf>>;
    fn read_first_line(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>>;
    fn metadata(&self, path: &Path) -> io::Result<ObservedMetadata>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemObservationFs;

impl ObservationFs for SystemObservationFs {
    fn current_dir(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn is_linux(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn pid_cwd(&self, pid: i64) -> Result<crate::pid::CwdObservation, crate::pid::PidError> {
        crate::pid::CwdObserver::cwd(&crate::pid::SystemCwdObserver, pid)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn resolve_non_strict(&self, path: &Path) -> io::Result<PathBuf> {
        resolve_non_strict(path)
    }

    fn lookup_home(&self, user: Option<&OsStr>) -> io::Result<Option<PathBuf>> {
        if let Some(user) = user {
            let account = sys::account(Account::Named(user)).map_err(|error| error.source)?;
            return Ok(account.map(|info| PathBuf::from(info.home)));
        }
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(Some(PathBuf::from(home)));
        }
        let account = sys::account(Account::Current).map_err(|error| error.source)?;
        Ok(account.map(|info| PathBuf::from(info.home)))
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn walk_session_logs(&self, root: &Path) -> io::Result<Vec<PathBuf>> {
        if !root.is_dir() {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        let mut ancestors = BTreeSet::new();
        walk_rollouts(root, &mut ancestors, &mut result);
        Ok(result)
    }

    fn read_first_line(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let file = fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            let end = available
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(available.len(), |index| index + 1);
            line.extend_from_slice(&available[..end]);
            let ended_line = matches!(available.get(end.wrapping_sub(1)), Some(b'\r' | b'\n'));
            reader.consume(end);
            if line.len() as u64 > limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    FirstLineTooLarge,
                ));
            }
            if ended_line {
                break;
            }
        }
        Ok(line)
    }

    fn metadata(&self, path: &Path) -> io::Result<ObservedMetadata> {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(path)?;
        let modified_seconds = metadata.mtime() as f64 + metadata.mtime_nsec() as f64 / 1e9;
        Ok(ObservedMetadata {
            modified_seconds,
            size: metadata.size(),
        })
    }
}

fn walk_rollouts(path: &Path, ancestors: &mut BTreeSet<PathBuf>, output: &mut Vec<PathBuf>) {
    let Ok(canonical) = fs::canonicalize(path) else {
        return;
    };
    if !ancestors.insert(canonical.clone()) {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        ancestors.remove(&canonical);
        return;
    };
    let entries: Vec<_> = entries.flatten().collect();
    // Recursive glob yields this directory's matches before entering children;
    // tie-breaking among equally recent logs therefore depends on this order.
    for entry in &entries {
        if is_rollout_name(&entry.file_name()) {
            output.push(entry.path());
        }
    }
    for entry in entries {
        // pathlib's ** expansion does not recurse through directory symlinks.
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            walk_rollouts(&entry.path(), ancestors, output);
        }
    }
    ancestors.remove(&canonical);
}

#[cfg(unix)]
fn is_rollout_name(name: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    name.as_bytes().starts_with(b"rollout-") && name.as_bytes().ends_with(b".jsonl")
}

#[cfg(not(unix))]
fn is_rollout_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
}

/// Process execution results retain only the fields this adapter observes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub returncode: i32,
    pub stdout: String,
}

pub trait ProcessCensus: Send + Sync {
    fn processes(&self) -> Result<Vec<Process>, ObserveError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeCensus;

impl ProcessCensus for NativeCensus {
    fn processes(&self) -> Result<Vec<Process>, ObserveError> {
        crate::process_info::processes()
            .map_err(ObserveError::ProcessCensus)?
            .into_iter()
            .map(|row| {
                let command = row
                    .command
                    .into_string()
                    .map_err(|command| ObserveError::NonUtf8Command { command })?;
                Ok(Process {
                    pid: i64::from(row.pid),
                    ppid: i64::from(row.ppid),
                    elapsed: row.elapsed,
                    cpu: row.cpu,
                    command,
                })
            })
            .collect()
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, args: &[OsString]) -> Result<CommandOutput, ObserveError>;
}

#[derive(Clone, Debug)]
pub struct SystemCommandRunner {
    pub timeout: Duration,
    pub output_limit_bytes: i64,
}

impl Default for SystemCommandRunner {
    fn default() -> Self {
        Self {
            timeout: COMMAND_TIMEOUT,
            output_limit_bytes: COMMAND_OUTPUT_LIMIT,
        }
    }
}

impl CommandRunner for SystemCommandRunner {
    fn run(&self, args: &[OsString]) -> Result<CommandOutput, ObserveError> {
        let current_dir = Path::new(".");
        let mut options = ExecOptions::new(args, current_dir, self.timeout);
        options.stdout_limit =
            usize::try_from(self.output_limit_bytes.max(0)).unwrap_or(usize::MAX);
        options.stderr_limit = options.stdout_limit;
        let result = run_text(&SystemExecutor, options)?;
        let program = args
            .first()
            .map(|arg| arg.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<empty>".into());
        if result.timed_out {
            return Err(ObserveError::TimedOut { program });
        }
        if result.stdout_truncated || result.stderr_truncated {
            return Err(ObserveError::CommandOutputLimit {
                program,
                limit: self.output_limit_bytes,
            });
        }
        Ok(CommandOutput {
            returncode: match result.termination {
                Termination::Exit(code) => code,
                Termination::Signal(signal) => -signal,
            },
            stdout: result.stdout.replace("\r\n", "\n").replace('\r', "\n"),
        })
    }
}

fn expand_user(fs: &dyn ObservationFs, path: &Path) -> Result<PathBuf, ObserveError> {
    let Some(text) = path.to_str() else {
        return Ok(path.to_owned());
    };
    if !text.starts_with('~') {
        return Ok(path.to_owned());
    }
    let end = text.find('/').unwrap_or(text.len());
    let user = &text[1..end];
    let account = if user.is_empty() {
        fs.lookup_home(None)
    } else {
        fs.lookup_home(Some(OsStr::new(user)))
    }
    .map_err(io_error("lookup home", path))?
    .ok_or_else(|| ObserveError::HomeDirectory {
        path: path.to_owned(),
    })?;
    let suffix = &text[end..];
    if account.as_os_str().is_empty() {
        return Ok(if suffix.is_empty() {
            PathBuf::from("/")
        } else {
            PathBuf::from(suffix)
        });
    }
    Ok(account.join(suffix.trim_start_matches('/')))
}

/// Expand `~` and resolve existing symlinks, while retaining missing path tails.
pub fn canonical_cwd(fs: &dyn ObservationFs, value: &str) -> Result<String, ObserveError> {
    if value == "?" {
        return Ok(value.into());
    }
    let path = Path::new(value);
    let expanded = expand_user(fs, path)?;
    match fs.resolve_non_strict(&expanded) {
        Ok(path) => Ok(path.to_string_lossy().into_owned()),
        Err(_) => Ok(value.into()),
    }
}

pub fn process_cwd(
    fs: &dyn ObservationFs,
    runner: &dyn CommandRunner,
    pid: i64,
) -> Result<String, ObserveError> {
    if fs.is_linux() {
        if let Ok(crate::pid::CwdObservation::Path(path)) = fs.pid_cwd(pid) {
            return path
                .clone()
                .into_os_string()
                .into_string()
                .map_err(|_| ObserveError::NonUtf8Path { path });
        }
        let proc_link = PathBuf::from(format!("/proc/{pid}/cwd"));
        if fs.exists(&proc_link) {
            if let Ok(path) = fs.resolve_non_strict(&proc_link) {
                return Ok(path.to_string_lossy().into_owned());
            }
        }
    }
    let args = [
        OsString::from("lsof"),
        OsString::from("-a"),
        OsString::from("-p"),
        OsString::from(pid.to_string()),
        OsString::from("-d"),
        OsString::from("cwd"),
        OsString::from("-Fn"),
    ];
    let result = runner.run(&args)?;
    for line in provenant_core::py::python_splitlines(&result.stdout) {
        if let Some(path) = line.strip_prefix('n') {
            return Ok(path.into());
        }
    }
    Ok("?".into())
}

pub fn worker_cwd(
    fs: &dyn ObservationFs,
    runner: &dyn CommandRunner,
    process: &Process,
) -> Result<String, ObserveError> {
    let launch_cwd = canonical_cwd(fs, &process_cwd(fs, runner, process.pid)?)?;
    let Some(target) = super::command_cwd(&process.command) else {
        return Ok(launch_cwd);
    };
    let expanded = expand_user(fs, Path::new(&target))?;
    if expanded.is_absolute() {
        return canonical_cwd(fs, &expanded.to_string_lossy());
    }
    if launch_cwd == "?" {
        return Ok(target);
    }
    canonical_cwd(fs, &Path::new(&launch_cwd).join(expanded).to_string_lossy())
}

pub fn session_logs(
    fs: &dyn ObservationFs,
    root: &Path,
) -> Result<BTreeMap<String, PathBuf>, ObserveError> {
    let mut result = BTreeMap::new();
    if !fs.is_dir(root) {
        return Ok(result);
    }
    for path in fs
        .walk_session_logs(root)
        .map_err(io_error("scan session logs", root))?
    {
        let bytes = match fs.read_first_line(&path, SESSION_FIRST_LINE_LIMIT) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .get_ref()
                    .is_some_and(|source| source.is::<FirstLineTooLarge>()) =>
            {
                return Err(ObserveError::RecordTooLarge {
                    path,
                    limit: SESSION_FIRST_LINE_LIMIT,
                });
            }
            Err(_) => continue,
        };
        let text = String::from_utf8(bytes).map_err(|source| ObserveError::Utf8 {
            source_name: path.to_string_lossy().into_owned(),
            source,
        })?;
        let record = match provenant_records::checked_json::from_str(&text) {
            Ok(value) => value,
            Err(provenant_records::DecodeError::NumberOutOfRange(error)) => {
                return Err(ObserveError::NumberOutOfRange {
                    path,
                    value: error.value,
                });
            }
            Err(provenant_records::DecodeError::Json(_)) => continue,
        };
        let Some(record) = record.as_object() else {
            return Err(ObserveError::RecordShape { path });
        };
        let Some(payload) = record.get("payload") else {
            continue;
        };
        let Some(payload) = payload.as_object() else {
            return Err(ObserveError::RecordShape { path });
        };
        let Some(cwd) = payload.get("cwd").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let cwd = canonical_cwd(fs, cwd)?;
        let replace = match result.get(&cwd) {
            None => true,
            Some(current) => fs
                .metadata(&path)
                .and_then(|candidate| {
                    fs.metadata(current)
                        .map(|current| candidate.modified_seconds > current.modified_seconds)
                })
                .unwrap_or(false),
        };
        if replace {
            result.insert(cwd, path);
        }
    }
    Ok(result)
}

pub fn worktree_state(
    runner: &dyn CommandRunner,
    cwd: &str,
    owned_pid: Option<i64>,
    live_pids: Option<&BTreeSet<i64>>,
) -> Result<String, ObserveError> {
    require_inspection_quiescent(owned_pid, live_pids)?;
    if cwd == "?" {
        return Ok("unknown".into());
    }
    let args = [
        OsString::from("git"),
        OsString::from("--no-optional-locks"),
        OsString::from("-C"),
        OsString::from(cwd),
        OsString::from("status"),
        OsString::from("--porcelain=v1"),
        OsString::from("--untracked-files=all"),
    ];
    let result = runner.run(&args)?;
    if result.returncode != 0 {
        return Ok("not-a-worktree".into());
    }
    Ok(if result.stdout.is_empty() {
        "clean"
    } else {
        "dirty"
    }
    .into())
}

pub fn repository_id(
    fs: &dyn ObservationFs,
    runner: &dyn CommandRunner,
    cwd: &Path,
) -> Result<Option<String>, ObserveError> {
    if cwd.to_string_lossy() == "?" {
        return Ok(None);
    }
    let args = [
        OsString::from("git"),
        OsString::from("--no-optional-locks"),
        OsString::from("-C"),
        cwd.as_os_str().into(),
        OsString::from("rev-parse"),
        OsString::from("--path-format=absolute"),
        OsString::from("--git-common-dir"),
    ];
    let result = runner.run(&args)?;
    if result.returncode != 0 {
        return Ok(None);
    }
    let output = provenant_core::py::python_strip(&result.stdout);
    if output.is_empty() {
        return Ok(None);
    }
    canonical_cwd(fs, output).map(Some)
}

pub fn log_metadata(
    fs: &dyn ObservationFs,
    path: Option<&Path>,
    now_seconds: f64,
) -> Result<(String, Option<f64>), ObserveError> {
    let Some(path) = path else {
        return Ok(("-".into(), None));
    };
    let metadata = match fs.metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(("-".into(), None)),
    };
    let age_seconds = (now_seconds - metadata.modified_seconds).max(0.0);
    let timestamp = provenant_core::py::from_timestamp(metadata.modified_seconds)?;
    let timestamp = timestamp.to_jiff()?.to_zoned(TimeZone::UTC);
    let base = timestamp.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = timestamp.subsec_nanosecond() / 1_000;
    let modified = if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    };
    Ok((modified, Some(age_seconds)))
}

pub fn output_size(
    fs: &dyn ObservationFs,
    path: Option<&Path>,
) -> Result<Option<i64>, ObserveError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let metadata = match fs.metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    i64::try_from(metadata.size)
        .map(Some)
        .map_err(|_| ObserveError::FileSizeOutOfRange {
            path: path.to_owned(),
        })
}

fn resolve_non_strict(path: &Path) -> io::Result<PathBuf> {
    let mut pending = path
        .components()
        .map(|component| PathBuf::from(component.as_os_str()))
        .collect::<VecDeque<_>>();
    let mut output = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        fs::canonicalize(".")?
    };
    let mut links = 0_u8;
    while let Some(component_path) = pending.pop_front() {
        let Some(component) = component_path.components().next() else {
            continue;
        };
        match component {
            Component::RootDir | Component::Prefix(_) => output.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            Component::Normal(name) => {
                output.push(name);
                match fs::symlink_metadata(&output) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        links = links.saturating_add(1);
                        if links > 40 {
                            return Err(io::Error::from_raw_os_error(sys::errno::ELOOP));
                        }
                        let target = fs::read_link(&output)?;
                        output.pop();
                        if target.is_absolute() {
                            output = PathBuf::from("/");
                        }
                        let mut target_components = target
                            .components()
                            .map(|component| PathBuf::from(component.as_os_str()))
                            .collect::<VecDeque<_>>();
                        target_components.append(&mut pending);
                        pending = target_components;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(output)
}
