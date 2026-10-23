//! Writer retry and branch-name policy. Lease effects remain with the safe OS owner.
use crate::{policy::hashable, PolicyError};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WORKTREE_WRITER_LOCK: &str = "provenant-dispatch-writer.lock";
pub const GRAPH_RESERVATION_FILE: &str = "provenant-graph-reservation.json";
pub const WORKTREE_WRITE_ADAPTERS: &[&str] = &[
    "claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot",
];
pub const STARTUP_WALK_SKIP: &[&str] = &[
    ".git",
    ".agent-run",
    "node_modules",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    "dist",
    "build",
];
pub const STARTUP_WALK_MAX_ENTRIES: u64 = 200_000;
pub const STARTUP_WALK_MAX_SECONDS: f64 = 5.0;
const RETRY_FIX: &str = "inspect the worktree: it changed or was dirty when the writer stopped; then dispatch again on another model or adapter";
pub const GIT_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(3);
pub const WRITER_STATE_TIMEOUT: Duration = Duration::from_secs(10);
pub const GIT_PATH_TIMEOUT: Duration = Duration::from_secs(30);
pub const GRAPH_STORE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub return_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStderr {
    Capture,
    Discard,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitObservationError {
    #[error("{0}")]
    Host(String),
    #[error("Git output was not valid UTF-8")]
    InvalidUtf8,
}

/// Filesystem, environment and bounded Git observations used by writer policy.
/// The confinement owner can implement this without exposing OS handles here.
pub trait GitObservationHost {
    fn expand_user(&self, path: &Path) -> io::Result<PathBuf>;
    fn canonicalize(&self, path: &Path, strict: bool) -> io::Result<PathBuf>;
    fn is_directory(&self, path: &Path) -> io::Result<bool>;
    fn read_utf8(&self, path: &Path) -> io::Result<String>;
    fn run_git(
        &self,
        cwd: &Path,
        args: &[OsString],
        clear_git_environment: bool,
        stderr: GitStderr,
        timeout: Duration,
    ) -> Result<GitOutput, GitObservationError>;
    fn environment(&self, name: &str) -> Option<String>;
    fn home_directory(&self) -> io::Result<PathBuf>;
}

/// Read-only graph-state lookup supplied by Fabric's store owner.
pub trait WriterReservationStore {
    fn graph_state(
        &self,
        database_path: &Path,
        token: &str,
        timeout: Duration,
    ) -> Result<Option<String>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIdentity {
    pub cwd: String,
    pub root: String,
    pub base_revision: Option<String>,
    pub working_tree: String,
}

fn git_args(args: &[&OsStr]) -> Vec<OsString> {
    args.iter().map(|arg| arg.to_os_string()).collect()
}

fn git_text(output: &[u8]) -> Result<String, GitObservationError> {
    String::from_utf8(output.to_vec()).map_err(|_| GitObservationError::InvalidUtf8)
}

/// Return workspace identity, retaining resolved paths if Git cannot observe a
/// revision or status. This mirrors the source's deliberately best-effort Git
/// fields while leaving path-resolution errors visible to the caller.
pub fn workspace_identity(
    host: &impl GitObservationHost,
    workspace: &Path,
    provider_cwd: Option<&Path>,
) -> Result<WorkspaceIdentity, GitObservationError> {
    let cwd = host
        .canonicalize(provider_cwd.unwrap_or(workspace), false)
        .map_err(|error| GitObservationError::Host(error.to_string()))?;
    let root = host
        .canonicalize(workspace, false)
        .map_err(|error| GitObservationError::Host(error.to_string()))?;
    let mut identity = WorkspaceIdentity {
        cwd: cwd.to_string_lossy().into_owned(),
        root: root.to_string_lossy().into_owned(),
        base_revision: None,
        working_tree: "unavailable".into(),
    };
    let base_args = git_args(&[
        OsStr::new("rev-parse"),
        OsStr::new("--show-toplevel"),
        OsStr::new("HEAD"),
    ]);
    let base = match host.run_git(
        workspace,
        &base_args,
        false,
        GitStderr::Discard,
        GIT_OBSERVATION_TIMEOUT,
    ) {
        Ok(output) if output.return_code == 0 => git_text(&output.stdout)?,
        Ok(_) | Err(GitObservationError::Host(_)) => return Ok(identity),
        Err(error) => return Err(error),
    };
    let lines = provenant_core::py::python_splitlines(&base);
    if lines.len() != 2
        || lines[1].len() < 40
        || lines[1].len() > 64
        || !lines[1].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok(identity);
    }
    let status_args = git_args(&[
        OsStr::new("status"),
        OsStr::new("--porcelain=v1"),
        OsStr::new("--untracked-files=normal"),
    ]);
    let status = match host.run_git(
        workspace,
        &status_args,
        false,
        GitStderr::Discard,
        GIT_OBSERVATION_TIMEOUT,
    ) {
        Ok(output) if output.return_code == 0 => git_text(&output.stdout)?,
        Ok(_) | Err(GitObservationError::Host(_)) => return Ok(identity),
        Err(error) => return Err(error),
    };
    identity.base_revision = Some(lines[1].to_ascii_lowercase());
    identity.working_tree = if status.is_empty() { "clean" } else { "dirty" }.into();
    Ok(identity)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileTimes {
    pub modified: f64,
    pub changed: f64,
}

impl FileTimes {
    fn latest(self) -> f64 {
        if self.changed > self.modified {
            self.changed
        } else {
            self.modified
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    pub path: PathBuf,
    pub name: OsString,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StartupWalkLimits {
    pub max_entries: u64,
    pub max_seconds: f64,
}

impl Default for StartupWalkLimits {
    fn default() -> Self {
        Self {
            max_entries: STARTUP_WALK_MAX_ENTRIES,
            max_seconds: STARTUP_WALK_MAX_SECONDS,
        }
    }
}

/// Filesystem and clock observations are supplied by the permitted host owner.
pub trait StartupWalkHost {
    type DirectoryEntries<'a>: Iterator<Item = io::Result<WalkEntry>>
    where
        Self: 'a;

    fn monotonic_seconds(&self) -> f64;
    fn lstat(&self, path: &Path) -> io::Result<FileTimes>;
    fn read_dir<'a>(&'a self, path: &Path) -> io::Result<Self::DirectoryEntries<'a>>;
    fn is_dir_without_following(&self, entry: &WalkEntry) -> io::Result<bool>;
    fn stat_without_following(&self, entry: &WalkEntry) -> io::Result<FileTimes>;
}

/// Fail-closed bounded walk matching `modified_since`, including ignored files.
pub fn modified_since(
    host: &impl StartupWalkHost,
    cwd: &Path,
    since: f64,
    limits: StartupWalkLimits,
) -> bool {
    let deadline = host.monotonic_seconds() + limits.max_seconds;
    let mut visited = 0_u64;
    let mut pending = vec![cwd.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(metadata) = host.lstat(&directory) else {
            return true;
        };
        if metadata.latest() >= since {
            return true;
        }
        let Ok(entries) = host.read_dir(&directory) else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            visited = visited.saturating_add(1);
            if visited > limits.max_entries || host.monotonic_seconds() > deadline {
                return true;
            }
            match host.is_dir_without_following(&entry) {
                Ok(true) => {
                    if !STARTUP_WALK_SKIP
                        .iter()
                        .any(|name| entry.name == OsStr::new(name))
                    {
                        pending.push(entry.path);
                    }
                }
                Ok(false) => match host.stat_without_following(&entry) {
                    Ok(metadata) if metadata.latest() < since => {}
                    Ok(_) | Err(_) => return true,
                },
                Err(_) => return true,
            }
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub head: String,
    pub porcelain: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WriterStart {
    pub state: Option<WorktreeSnapshot>,
    pub launched_at: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WorktreeLeaseError(pub String);

fn git_path(
    host: &impl GitObservationHost,
    worktree: &Path,
    which: &str,
) -> Result<Option<PathBuf>, GitObservationError> {
    let args = git_args(&[
        OsStr::new("rev-parse"),
        OsStr::new("--path-format=absolute"),
        OsStr::new(which),
    ]);
    let output = match host.run_git(worktree, &args, false, GitStderr::Capture, GIT_PATH_TIMEOUT) {
        Ok(output) if output.return_code == 0 => output,
        Ok(_) | Err(GitObservationError::Host(_)) => return Ok(None),
        Err(error @ GitObservationError::InvalidUtf8) => return Err(error),
    };
    let value = provenant_core::py::python_strip(&git_text(&output.stdout)?).to_owned();
    Ok((!value.is_empty()).then(|| PathBuf::from(value)))
}

fn writer_worktree_state(host: &impl GitObservationHost, cwd: &Path) -> Option<WorktreeSnapshot> {
    let head_args = git_args(&[OsStr::new("rev-parse"), OsStr::new("HEAD")]);
    let head_output = host
        .run_git(
            cwd,
            &head_args,
            true,
            GitStderr::Capture,
            WRITER_STATE_TIMEOUT,
        )
        .ok()?;
    if head_output.return_code != 0 {
        return None;
    }
    let head = git_text(&head_output.stdout).ok()?;
    let status_args = git_args(&[
        OsStr::new("status"),
        OsStr::new("--porcelain=v1"),
        OsStr::new("-z"),
        OsStr::new("--untracked-files=normal"),
    ]);
    let status_output = host
        .run_git(
            cwd,
            &status_args,
            true,
            GitStderr::Capture,
            WRITER_STATE_TIMEOUT,
        )
        .ok()?;
    if status_output.return_code != 0 {
        return None;
    }
    Some(WorktreeSnapshot {
        head: provenant_core::py::python_strip(&head).to_owned(),
        porcelain: status_output.stdout,
    })
}

/// Observe the current worktree only when the starting snapshot was clean.
pub fn worktree_unchanged_at(
    host: &impl GitObservationHost,
    start: Option<&WorktreeSnapshot>,
    cwd: &Path,
) -> Option<bool> {
    let start = start.filter(|snapshot| snapshot.porcelain.is_empty())?;
    writer_worktree_state(host, cwd).map(|current| current == *start)
}

/// Apply the retry guard while observing Git and filesystem state through
/// bounded host traits. The status membership error is checked before effects.
pub fn guard_writer_retry_at(
    host: &impl GitObservationHost,
    walk: &impl StartupWalkHost,
    record: &mut Map<String, Value>,
    start: Option<&WriterStart>,
    cwd: &Path,
) -> Result<(), PolicyError> {
    let status = record.get("status").unwrap_or(&Value::Null);
    hashable(status, "status")?;
    if !status
        .as_str()
        .is_some_and(|value| ["startup_timeout", "rate_limited"].contains(&value))
    {
        return Ok(());
    }
    let may_retry = start.is_some_and(|start| {
        start.state.as_ref().is_some_and(|state| {
            state.porcelain.is_empty()
                && writer_worktree_state(host, cwd).as_ref() == Some(state)
                && !modified_since(
                    walk,
                    cwd,
                    start.launched_at - 1.0,
                    StartupWalkLimits::default(),
                )
        })
    });
    if may_retry {
        return Ok(());
    }
    record.insert("retryable".into(), Value::Bool(false));
    record.insert("fix".into(), Value::String(RETRY_FIX.into()));
    Ok(())
}

fn git_path_for_worktree(
    host: &impl GitObservationHost,
    worktree: &Path,
    which: &str,
) -> Result<Option<PathBuf>, GitObservationError> {
    git_path(host, worktree, which)
}

#[cfg(unix)]
fn os_path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn os_path_from_bytes(bytes: &[u8]) -> Result<PathBuf, GitObservationError> {
    String::from_utf8(bytes.to_vec())
        .map(PathBuf::from)
        .map_err(|_| GitObservationError::InvalidUtf8)
}

/// Validate that a path is the registered root of a linked worktree.
pub fn resolve_writer_worktree(
    host: &impl GitObservationHost,
    worktree: &Path,
) -> Result<PathBuf, WorktreeLeaseError> {
    let expanded = host.expand_user(worktree).map_err(|_| {
        WorktreeLeaseError(format!(
            "worktree is not a readable directory: {}",
            worktree.display()
        ))
    })?;
    let resolved = host.canonicalize(&expanded, true).map_err(|_| {
        WorktreeLeaseError(format!(
            "worktree is not a readable directory: {}",
            worktree.display()
        ))
    })?;
    if !host.is_directory(&resolved).map_err(|_| {
        WorktreeLeaseError(format!(
            "worktree is not a directory: {}",
            resolved.display()
        ))
    })? {
        return Err(WorktreeLeaseError(format!(
            "worktree is not a directory: {}",
            resolved.display()
        )));
    }
    let top = git_path_for_worktree(host, &resolved, "--show-toplevel")
        .map_err(|error| WorktreeLeaseError(error.to_string()))?
        .ok_or_else(|| {
            WorktreeLeaseError(format!(
                "worktree is not inside a Git repository: {}",
                resolved.display()
            ))
        })?;
    let top = host.canonicalize(&top, false).map_err(|_| {
        WorktreeLeaseError(format!(
            "worktree root could not be resolved: {}",
            resolved.display()
        ))
    })?;
    if top != resolved {
        return Err(WorktreeLeaseError(format!(
            "worktree must be the root of a Git worktree: {}",
            resolved.display()
        )));
    }

    let listing_args = git_args(&[
        OsStr::new("worktree"),
        OsStr::new("list"),
        OsStr::new("--porcelain"),
        OsStr::new("-z"),
    ]);
    let listing = host
        .run_git(
            &resolved,
            &listing_args,
            true,
            GitStderr::Capture,
            GIT_OBSERVATION_TIMEOUT,
        )
        .map_err(|_| WorktreeLeaseError("cannot verify registered Git worktree".into()))?;
    if listing.return_code != 0 {
        return Err(WorktreeLeaseError(
            "cannot verify registered Git worktree".into(),
        ));
    }
    let mut registered = false;
    for field in listing.stdout.split(|byte| *byte == 0) {
        let Some(path_bytes) = field.strip_prefix(b"worktree ") else {
            continue;
        };
        #[cfg(unix)]
        let path = os_path_from_bytes(path_bytes);
        #[cfg(not(unix))]
        let path = os_path_from_bytes(path_bytes)
            .map_err(|_| WorktreeLeaseError("cannot verify registered Git worktree".into()))?;
        let path = host
            .canonicalize(&path, false)
            .map_err(|_| WorktreeLeaseError("cannot verify registered Git worktree".into()))?;
        if path == resolved {
            registered = true;
        }
    }
    if !registered {
        return Err(WorktreeLeaseError(
            "worktree must be registered with Git".into(),
        ));
    }

    let git_dir = git_path_for_worktree(host, &resolved, "--git-dir")
        .map_err(|error| WorktreeLeaseError(error.to_string()))?;
    let common_dir = git_path_for_worktree(host, &resolved, "--git-common-dir")
        .map_err(|error| WorktreeLeaseError(error.to_string()))?;
    let (Some(git_dir), Some(common_dir)) = (git_dir, common_dir) else {
        return Err(WorktreeLeaseError(
            "worktree_write requires a linked worktree; fix: create a linked worktree".into(),
        ));
    };
    let git_dir = host
        .canonicalize(&git_dir, false)
        .map_err(|error| WorktreeLeaseError(error.to_string()))?;
    let common_dir = host
        .canonicalize(&common_dir, false)
        .map_err(|error| WorktreeLeaseError(error.to_string()))?;
    if git_dir == common_dir {
        return Err(WorktreeLeaseError(
            "worktree_write requires a linked worktree; fix: create a linked worktree".into(),
        ));
    }
    Ok(resolved)
}

/// Observe the branch and return its repository naming warning, if any.
pub fn branch_name_warning_at(
    host: &impl GitObservationHost,
    worktree: &Path,
) -> Result<Option<String>, GitObservationError> {
    let args = git_args(&[
        OsStr::new("symbolic-ref"),
        OsStr::new("--quiet"),
        OsStr::new("--short"),
        OsStr::new("HEAD"),
    ]);
    let branch = match host.run_git(
        worktree,
        &args,
        true,
        GitStderr::Capture,
        GIT_OBSERVATION_TIMEOUT,
    ) {
        Ok(output) if output.return_code == 0 => {
            provenant_core::py::python_strip(&git_text(&output.stdout)?).to_owned()
        }
        Ok(_) => "(detached HEAD)".into(),
        Err(error) => return Err(error),
    };
    branch_warning(&branch).map_err(|error| GitObservationError::Host(error.to_string()))
}

/// Refuse a worktree while a Fabric graph reservation remains active.
pub fn graph_reservation_refusal(
    host: &impl GitObservationHost,
    store: &impl WriterReservationStore,
    worktree: &Path,
) -> Result<Option<String>, GitObservationError> {
    let Some(git_dir) = git_path_for_worktree(host, worktree, "--absolute-git-dir")? else {
        return Ok(None);
    };
    let reservation_path = git_dir.join(GRAPH_RESERVATION_FILE);
    let reservation_text = match host.read_utf8(&reservation_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Ok(Some(format!(
                "the graph reservation {} is unreadable; fix: check it, or remove it if no graph uses this worktree",
                reservation_path.display()
            )));
        }
    };
    let reservation: Value = match serde_json::from_str(&reservation_text) {
        Ok(value) => value,
        Err(_) => {
            return Ok(Some(format!(
                "the graph reservation {} is unreadable; fix: check it, or remove it if no graph uses this worktree",
                reservation_path.display()
            )));
        }
    };
    let token = reservation
        .as_object()
        .and_then(|object| object.get("token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty());
    let Some(token) = token else {
        return Ok(Some(format!(
            "the graph reservation {} names no token; fix: check it, or remove it if no graph uses this worktree",
            reservation_path.display()
        )));
    };
    if host.environment("PROVENANT_GRAPH_RESERVATION").as_deref() == Some(token) {
        return Ok(None);
    }
    let object = reservation.as_object();
    let graph = object
        .and_then(|object| object.get("graph_id"))
        .cloned()
        .unwrap_or_else(|| Value::String("?".into()));
    let node = object
        .and_then(|object| object.get("node_id"))
        .cloned()
        .unwrap_or_else(|| Value::String("?".into()));
    let graph = provenant_core::py::python_str(&graph);
    let node = provenant_core::py::python_str(&node);
    let state = match host.environment("AGENT_FABRIC_STATE_DIRECTORY") {
        Some(path) => PathBuf::from(path),
        None => host
            .home_directory()
            .map_err(|error| GitObservationError::Host(error.to_string()))?
            .join(".local/state/agent-harness/fabric"),
    };
    let database = host
        .canonicalize(&state.join("fabric.sqlite3"), false)
        .map_err(|error| GitObservationError::Host(error.to_string()))?;
    let graph_state = store.graph_state(&database, token, GRAPH_STORE_TIMEOUT);
    let state = match graph_state {
        Ok(state) => state,
        Err(_) => {
            return Ok(Some(format!(
                "worktree is reserved by graph {graph} node {node} and the Fabric store cannot confirm it ended; fix: run provenant graph status {graph}"
            )));
        }
    };
    if state.as_deref() != Some("running") {
        return Ok(None);
    }
    Ok(Some(format!(
        "worktree is reserved by graph {graph} node {node}; fix: wait for it, or run provenant graph cancel {graph}"
    )))
}

/// Unknown from a missing or dirty start, matching the source's tri-state result.
pub fn worktree_unchanged(
    start: Option<&WorktreeSnapshot>,
    current: Option<&WorktreeSnapshot>,
) -> Option<bool> {
    let start = start.filter(|s| s.porcelain.is_empty())?;
    current.map(|current| current == start)
}

/// `modified_since` is the bounded filesystem observation for launched - 1.0
/// seconds, including ignored entries and fail-closed unreadable/cap results.
/// A caller supplies both observations; this policy does not acquire a lease.
pub fn guard_writer_retry(
    record: &mut Map<String, Value>,
    start: Option<&WorktreeSnapshot>,
    current: Option<&WorktreeSnapshot>,
    modified_since: bool,
) -> Result<(), PolicyError> {
    let status = record.get("status").unwrap_or(&Value::Null);
    hashable(status, "status")?;
    if !status
        .as_str()
        .is_some_and(|s| ["startup_timeout", "rate_limited"].contains(&s))
    {
        return Ok(());
    }
    if start.is_some_and(|state| {
        state.porcelain.is_empty() && current == Some(state) && !modified_since
    }) {
        return Ok(());
    }
    record.insert("retryable".into(), Value::Bool(false));
    record.insert("fix".into(), Value::String(RETRY_FIX.into()));
    Ok(())
}

pub fn branch_warning(branch: &str) -> Result<Option<String>, regex::Error> {
    let pattern = Regex::new(
        r"\A(?:(?:feat|fix|refactor|perf|test|docs|chore|ci|build|proto)/[a-z][a-z0-9]*-[a-z0-9]+(?:-[a-z0-9]+){1,4}|land/[0-9]{8}-[a-z0-9]+(?:-[a-z0-9]+)*)\z",
    )?;
    if branch.chars().count() <= 48 && pattern.is_match(branch) {
        Ok(None)
    } else {
        Ok(Some(format!(
            "branch {branch} is outside <type>/<area>-<slug> (or land/, proto/); rename for repository convention"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, VecDeque};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct GitCall {
        cwd: PathBuf,
        args: Vec<OsString>,
        clear_git_environment: bool,
        stderr: GitStderr,
        timeout: Duration,
    }

    struct FakeGitHost {
        outputs: RefCell<VecDeque<Result<GitOutput, GitObservationError>>>,
        calls: RefCell<Vec<GitCall>>,
        files: BTreeMap<PathBuf, String>,
        environment: BTreeMap<String, String>,
    }

    impl FakeGitHost {
        fn new(outputs: impl IntoIterator<Item = GitOutput>) -> Self {
            Self {
                outputs: RefCell::new(outputs.into_iter().map(Ok).collect()),
                calls: RefCell::new(Vec::new()),
                files: BTreeMap::new(),
                environment: BTreeMap::new(),
            }
        }

        fn with_file(mut self, path: &Path, contents: &str) -> Self {
            self.files.insert(path.to_path_buf(), contents.into());
            self
        }

        fn with_environment(mut self, name: &str, value: &str) -> Self {
            self.environment.insert(name.into(), value.into());
            self
        }
    }

    impl GitObservationHost for FakeGitHost {
        fn expand_user(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }

        fn canonicalize(&self, path: &Path, _strict: bool) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }

        fn is_directory(&self, _path: &Path) -> io::Result<bool> {
            Ok(true)
        }

        fn read_utf8(&self, path: &Path) -> io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn run_git(
            &self,
            cwd: &Path,
            args: &[OsString],
            clear_git_environment: bool,
            stderr: GitStderr,
            timeout: Duration,
        ) -> Result<GitOutput, GitObservationError> {
            self.calls.borrow_mut().push(GitCall {
                cwd: cwd.to_path_buf(),
                args: args.to_vec(),
                clear_git_environment,
                stderr,
                timeout,
            });
            self.outputs
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err(GitObservationError::Host("fake output missing".into())))
        }

        fn environment(&self, name: &str) -> Option<String> {
            self.environment.get(name).cloned()
        }

        fn home_directory(&self) -> io::Result<PathBuf> {
            Ok(PathBuf::from("/home/fixture"))
        }
    }

    fn git_output(return_code: i32, stdout: impl AsRef<[u8]>) -> GitOutput {
        GitOutput {
            return_code,
            stdout: stdout.as_ref().to_vec(),
            stderr: Vec::new(),
        }
    }

    struct FakeReservationStore(Result<Option<String>, String>);

    impl WriterReservationStore for FakeReservationStore {
        fn graph_state(
            &self,
            _database_path: &Path,
            _token: &str,
            _timeout: Duration,
        ) -> Result<Option<String>, String> {
            self.0.clone()
        }
    }

    #[derive(Clone)]
    struct FakeDirectory {
        times: FileTimes,
        entries: Vec<WalkEntry>,
        unreadable: bool,
    }

    #[derive(Clone, Copy)]
    enum FakeEntry {
        Directory,
        File(FileTimes),
        IsDirectoryError,
        StatError,
    }

    struct FakeWalkHost {
        directories: BTreeMap<PathBuf, FakeDirectory>,
        entries: BTreeMap<PathBuf, FakeEntry>,
        clock: Vec<f64>,
        clock_calls: Cell<usize>,
    }

    impl StartupWalkHost for FakeWalkHost {
        type DirectoryEntries<'a> = std::vec::IntoIter<io::Result<WalkEntry>>;

        fn monotonic_seconds(&self) -> f64 {
            let index = self.clock_calls.get();
            self.clock_calls.set(index.saturating_add(1));
            self.clock
                .get(index)
                .copied()
                .or_else(|| self.clock.last().copied())
                .unwrap_or(0.0)
        }

        fn lstat(&self, path: &Path) -> io::Result<FileTimes> {
            self.directories
                .get(path)
                .map(|directory| directory.times)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn read_dir<'a>(&'a self, path: &Path) -> io::Result<Self::DirectoryEntries<'a>> {
            let directory = self
                .directories
                .get(path)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            if directory.unreadable {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(directory
                    .entries
                    .clone()
                    .into_iter()
                    .map(Ok)
                    .collect::<Vec<_>>()
                    .into_iter())
            }
        }

        fn is_dir_without_following(&self, entry: &WalkEntry) -> io::Result<bool> {
            match self.entries.get(&entry.path) {
                Some(FakeEntry::Directory) => Ok(true),
                Some(FakeEntry::File(_)) => Ok(false),
                Some(FakeEntry::IsDirectoryError) | None => {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                }
                Some(FakeEntry::StatError) => Ok(false),
            }
        }

        fn stat_without_following(&self, entry: &WalkEntry) -> io::Result<FileTimes> {
            match self.entries.get(&entry.path) {
                Some(FakeEntry::File(times)) => Ok(*times),
                Some(FakeEntry::StatError) | None => {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                }
                Some(FakeEntry::Directory) | Some(FakeEntry::IsDirectoryError) => {
                    Err(io::Error::from(io::ErrorKind::InvalidInput))
                }
            }
        }
    }

    fn fixture_walk_host(case: &Value) -> FakeWalkHost {
        let root = PathBuf::from("/fixture");
        let quiet = FileTimes {
            modified: 0.0,
            changed: 0.0,
        };
        let future = FileTimes {
            modified: 5_000_000_000.0,
            changed: 0.0,
        };
        let mut host = FakeWalkHost {
            directories: BTreeMap::from([(
                root.clone(),
                FakeDirectory {
                    times: quiet,
                    entries: Vec::new(),
                    unreadable: case["kind"] == "unreadable",
                },
            )]),
            entries: BTreeMap::new(),
            clock: vec![0.0],
            clock_calls: Cell::new(0),
        };
        match case["kind"].as_str() {
            Some("file_changed") => {
                let path = root.join("work.txt");
                if let Some(directory) = host.directories.get_mut(&root) {
                    directory.entries.push(WalkEntry {
                        path: path.clone(),
                        name: "work.txt".into(),
                    });
                }
                host.entries.insert(path, FakeEntry::File(future));
            }
            Some("skipped_directory") => {
                let ignored = root.join(".git");
                let path = ignored.join("work.txt");
                if let Some(directory) = host.directories.get_mut(&root) {
                    directory.entries.push(WalkEntry {
                        path: ignored.clone(),
                        name: ".git".into(),
                    });
                }
                host.entries.insert(ignored.clone(), FakeEntry::Directory);
                host.directories.insert(
                    ignored,
                    FakeDirectory {
                        times: quiet,
                        entries: vec![WalkEntry {
                            path: path.clone(),
                            name: "work.txt".into(),
                        }],
                        unreadable: false,
                    },
                );
                host.entries.insert(path, FakeEntry::File(future));
            }
            Some("entry_cap") => {
                for index in 0..2 {
                    let name = format!("file-{index}");
                    let path = root.join(&name);
                    if let Some(directory) = host.directories.get_mut(&root) {
                        directory.entries.push(WalkEntry {
                            path: path.clone(),
                            name: name.into(),
                        });
                    }
                    host.entries.insert(path, FakeEntry::File(quiet));
                }
            }
            Some("deadline") => {
                host.clock = vec![0.0, 0.01];
                let path = root.join("file");
                if let Some(directory) = host.directories.get_mut(&root) {
                    directory.entries.push(WalkEntry {
                        path: path.clone(),
                        name: "file".into(),
                    });
                }
                host.entries.insert(path, FakeEntry::File(quiet));
            }
            Some("entry_is_dir_error") => {
                let path = root.join("entry");
                if let Some(directory) = host.directories.get_mut(&root) {
                    directory.entries.push(WalkEntry {
                        path: path.clone(),
                        name: "entry".into(),
                    });
                }
                host.entries.insert(path, FakeEntry::IsDirectoryError);
            }
            Some("entry_stat_error") => {
                let path = root.join("entry");
                if let Some(directory) = host.directories.get_mut(&root) {
                    directory.entries.push(WalkEntry {
                        path: path.clone(),
                        name: "entry".into(),
                    });
                }
                host.entries.insert(path, FakeEntry::StatError);
            }
            Some("missing") => {
                host.directories.clear();
            }
            _ => {}
        }
        host
    }
    #[test]
    fn retry_requires_a_clean_equal_snapshot_and_no_ignored_file_changes() {
        let clean = WorktreeSnapshot {
            head: "h".into(),
            porcelain: vec![],
        };
        let dirty = WorktreeSnapshot {
            head: "h".into(),
            porcelain: b"M file".to_vec(),
        };
        for status in ["startup_timeout", "rate_limited", "ok"] {
            for start in [None, Some(&clean), Some(&dirty)] {
                for current in [None, Some(&clean), Some(&dirty)] {
                    for modified in [false, true] {
                        let mut record = json!({"status":status,"retryable":true})
                            .as_object()
                            .unwrap()
                            .clone();
                        guard_writer_retry(&mut record, start, current, modified).unwrap();
                        assert_eq!(
                            record["retryable"],
                            status == "ok"
                                || (start == Some(&clean) && current == Some(&clean) && !modified)
                        );
                    }
                }
            }
        }
        assert_eq!(worktree_unchanged(Some(&dirty), Some(&clean)), None);
        assert_eq!(worktree_unchanged(Some(&clean), None), None);
        assert_eq!(worktree_unchanged(Some(&clean), Some(&dirty)), Some(false));
    }
    #[test]
    fn branch_name_contract_matches_python_without_running_git() {
        let branches = json!([
            "feat/workflow-port-batch",
            "fix/a-b-c",
            "land/20261023-port-workflow",
            "proto/workflow-test-one",
            "port/workflow-2",
            "(detached HEAD)",
            "feat/a-b",
            "feat/a-b-c-d-e-f-g-h",
            "feat/area-abcdefghijklmnopqrstuvwxyz-abcdefghijklmnop",
            "feat/a-b-c\n"
        ]);
        let actual: Vec<_> = branches
            .as_array()
            .unwrap()
            .iter()
            .map(|b| json!(branch_warning(b.as_str().unwrap()).unwrap()))
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_writer.py", "writer_branch", &branches)
        {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn retry_guard_matches_python_for_clean_dirty_unknown_and_unhashable_statuses() {
        let clean = json!({"head":"abc","porcelain":[]});
        let dirty = json!({"head":"abc","porcelain":[77,32,120]});
        let mut cases = Vec::new();
        for status in [
            json!("startup_timeout"),
            json!("rate_limited"),
            json!("ok"),
            json!(null),
            json!([]),
            json!({}),
        ] {
            for start in [None, Some(&clean), Some(&dirty)] {
                for current in [None, Some(&clean), Some(&dirty)] {
                    for modified in [false, true] {
                        cases.push(json!({
                            "status":status,
                            "start":start,
                            "current":current,
                            "modified":modified
                        }));
                    }
                }
            }
        }
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                let mut record = json!({"status":case["status"],"retryable":true})
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                let state = |value: &Value| -> Option<WorktreeSnapshot> {
                    if value.is_null() {
                        return None;
                    }
                    Some(WorktreeSnapshot {
                        head: value["head"].as_str()?.to_owned(),
                        porcelain: value["porcelain"]
                            .as_array()?
                            .iter()
                            .map(|byte| u8::try_from(byte.as_u64()?).ok())
                            .collect::<Option<Vec<_>>>()?,
                    })
                };
                let start = state(&case["start"]);
                let current = state(&case["current"]);
                match guard_writer_retry(
                    &mut record,
                    start.as_ref(),
                    current.as_ref(),
                    case["modified"].as_bool().unwrap_or(false),
                ) {
                    Ok(()) => json!({"record":record}),
                    Err(PolicyError::Unhashable { kind, .. }) => {
                        json!({"error_type":"TypeError","unhashable_type":kind})
                    }
                    Err(error) => json!({"error":error.to_string()}),
                }
            })
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_writer.py", "writer_retry", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn unchanged_snapshot_is_unknown_from_dirty_or_unavailable_start() {
        let clean = json!({"head":"abc","porcelain":[]});
        let dirty = json!({"head":"abc","porcelain":[77,32,120]});
        let cases = vec![
            json!({"start":null,"current":clean}),
            json!({"start":dirty,"current":clean}),
            json!({"start":clean,"current":null}),
            json!({"start":clean,"current":clean}),
            json!({"start":clean,"current":dirty}),
        ];
        let decode = |value: &Value| -> Option<WorktreeSnapshot> {
            if value.is_null() {
                return None;
            }
            Some(WorktreeSnapshot {
                head: value["head"].as_str()?.to_owned(),
                porcelain: value["porcelain"]
                    .as_array()?
                    .iter()
                    .map(|byte| u8::try_from(byte.as_u64()?).ok())
                    .collect::<Option<Vec<_>>>()?,
            })
        };
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                json!(worktree_unchanged(
                    decode(&case["start"]).as_ref(),
                    decode(&case["current"]).as_ref()
                ))
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle_script(
            "oracle_writer.py",
            "writer_unchanged",
            &json!(cases),
        ) {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn bounded_startup_walk_matches_python_on_files_skips_and_unreadable_paths() {
        let cases = vec![
            json!({"kind":"clean","since":4_000_000_000.0}),
            json!({"kind":"file_changed","since":4_000_000_000.0}),
            json!({"kind":"skipped_directory","since":4_000_000_000.0}),
            json!({"kind":"missing","since":4_000_000_000.0}),
        ];
        let actual: Vec<_> = cases
            .iter()
            .map(|case| {
                let host = fixture_walk_host(case);
                json!(modified_since(
                    &host,
                    Path::new("/fixture"),
                    case["since"].as_f64().unwrap_or(0.0),
                    StartupWalkLimits::default()
                ))
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle_script(
            "oracle_writer.py",
            "writer_modified_since",
            &json!(cases),
        ) {
            assert_eq!(json!(actual), expected);
        }

        let cap_case = json!({"kind":"entry_cap"});
        let cap_host = fixture_walk_host(&cap_case);
        assert!(modified_since(
            &cap_host,
            Path::new("/fixture"),
            4_000_000_000.0,
            StartupWalkLimits {
                max_entries: 1,
                max_seconds: 5.0,
            }
        ));
        let deadline_case = json!({"kind":"deadline"});
        let deadline_host = fixture_walk_host(&deadline_case);
        assert!(modified_since(
            &deadline_host,
            Path::new("/fixture"),
            4_000_000_000.0,
            StartupWalkLimits {
                max_entries: 200_000,
                max_seconds: 0.0,
            }
        ));
        for kind in ["unreadable", "entry_is_dir_error", "entry_stat_error"] {
            let case = json!({"kind":kind});
            let host = fixture_walk_host(&case);
            assert!(modified_since(
                &host,
                Path::new("/fixture"),
                4_000_000_000.0,
                StartupWalkLimits::default()
            ));
        }
    }

    #[test]
    fn git_observation_wrappers_preserve_timeouts_environment_and_source_fallbacks() {
        let revision = "ABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let host = FakeGitHost::new([
            git_output(0, format!("/workspace\n{revision}\n").into_bytes()),
            git_output(0, b""),
            git_output(0, format!("{revision}\n").into_bytes()),
            git_output(0, b""),
        ]);
        let identity =
            workspace_identity(&host, Path::new("/workspace"), Some(Path::new("/provider")))
                .expect("fixture paths are valid");
        assert_eq!(identity.cwd, "/provider");
        assert_eq!(identity.root, "/workspace");
        let revision_lower = revision.to_ascii_lowercase();
        assert_eq!(
            identity.base_revision.as_deref(),
            Some(revision_lower.as_str())
        );
        assert_eq!(identity.working_tree, "clean");
        let snapshot = WorktreeSnapshot {
            head: revision.into(),
            porcelain: Vec::new(),
        };
        assert_eq!(
            worktree_unchanged_at(&host, Some(&snapshot), Path::new("/writer")),
            Some(true)
        );
        let calls = host.calls.borrow();
        assert_eq!(calls.len(), 4);
        assert!(!calls[0].clear_git_environment);
        assert!(!calls[1].clear_git_environment);
        assert_eq!(calls[0].stderr, GitStderr::Discard);
        assert_eq!(calls[1].stderr, GitStderr::Discard);
        assert!(calls[2].clear_git_environment);
        assert!(calls[3].clear_git_environment);
        assert_eq!(calls[2].stderr, GitStderr::Capture);
        assert_eq!(calls[0].timeout, GIT_OBSERVATION_TIMEOUT);
        assert_eq!(calls[2].timeout, WRITER_STATE_TIMEOUT);

        let unavailable = FakeGitHost::new([git_output(1, b"")]);
        let identity = workspace_identity(&unavailable, Path::new("/workspace"), None)
            .expect("a Git failure leaves path identity available");
        assert_eq!(identity.base_revision, None);
        assert_eq!(identity.working_tree, "unavailable");
    }

    #[test]
    fn retry_guard_observes_a_clean_worktree_and_fails_closed_on_change() {
        let snapshot = WorktreeSnapshot {
            head: "head".into(),
            porcelain: Vec::new(),
        };
        let start = WriterStart {
            state: Some(snapshot.clone()),
            launched_at: 10.0,
        };
        let walk_case = json!({"kind":"clean"});
        let walk = fixture_walk_host(&walk_case);
        let unchanged = FakeGitHost::new([git_output(0, b"head\n"), git_output(0, b"")]);
        let mut record = json!({"status":"startup_timeout","retryable":true})
            .as_object()
            .cloned()
            .unwrap_or_default();
        guard_writer_retry_at(
            &unchanged,
            &walk,
            &mut record,
            Some(&start),
            Path::new("/fixture"),
        )
        .expect("the status is hashable");
        assert_eq!(record.get("retryable"), Some(&Value::Bool(true)));
        assert_eq!(unchanged.calls.borrow().len(), 2);

        let changed = FakeGitHost::new([git_output(0, b"other-head\n"), git_output(0, b"")]);
        let mut record = json!({"status":"rate_limited","retryable":true})
            .as_object()
            .cloned()
            .unwrap_or_default();
        guard_writer_retry_at(
            &changed,
            &walk,
            &mut record,
            Some(&start),
            Path::new("/fixture"),
        )
        .expect("the status is hashable");
        assert_eq!(record.get("retryable"), Some(&Value::Bool(false)));
        assert_eq!(record.get("fix"), Some(&Value::String(RETRY_FIX.into())));
        assert_eq!(changed.calls.borrow().len(), 2);
    }

    #[test]
    fn linked_worktree_and_reservation_policy_use_bounded_owner_seams() {
        let host = FakeGitHost::new([
            git_output(0, b"/linked\n"),
            git_output(0, b"worktree /linked\0worktree /main\0"),
            git_output(0, b"/linked/.git/worktrees/name\n"),
            git_output(0, b"/main/.git\n"),
        ]);
        assert_eq!(
            resolve_writer_worktree(&host, Path::new("/linked")),
            Ok(PathBuf::from("/linked"))
        );
        assert_eq!(host.calls.borrow().len(), 4);
        assert_eq!(host.calls.borrow()[1].timeout, GIT_OBSERVATION_TIMEOUT);
        assert_eq!(
            host.calls
                .borrow()
                .iter()
                .map(|call| call.clear_git_environment)
                .collect::<Vec<_>>(),
            [false, true, false, false]
        );

        let reservation_path = Path::new("/linked/.git/provenant-graph-reservation.json");
        let reservation_host = FakeGitHost::new([git_output(0, b"/linked/.git\n")]).with_file(
            reservation_path,
            r#"{"token":"token-1","graph_id":"graph-1","node_id":"node-2"}"#,
        );
        let refusal = graph_reservation_refusal(
            &reservation_host,
            &FakeReservationStore(Ok(Some("running".into()))),
            Path::new("/linked"),
        )
        .expect("the reservation data is valid");
        assert_eq!(
            refusal.as_deref(),
            Some(
                "worktree is reserved by graph graph-1 node node-2; fix: wait for it, or run provenant graph cancel graph-1"
            )
        );

        let owned_host = FakeGitHost::new([git_output(0, b"/linked/.git\n")])
            .with_file(
                reservation_path,
                r#"{"token":"token-1","graph_id":"graph-1","node_id":"node-2"}"#,
            )
            .with_environment("PROVENANT_GRAPH_RESERVATION", "token-1");
        assert_eq!(
            graph_reservation_refusal(
                &owned_host,
                &FakeReservationStore(Err("must not query owned graph".into())),
                Path::new("/linked")
            )
            .expect("matching owner token is accepted"),
            None
        );
    }

    #[test]
    fn git_path_inherits_git_environment_like_python() {
        let cases = json!([{
            "git_dir": "/repo/.git/worktrees/fixture",
            "common_dir": "/repo/.git"
        }]);
        let host = FakeGitHost::new([
            git_output(0, b"/linked\n"),
            git_output(0, b"worktree /linked\0"),
            git_output(0, b"/repo/.git/worktrees/fixture\n"),
            git_output(0, b"/repo/.git\n"),
        ])
        .with_environment("GIT_DIR", "/repo/.git/worktrees/fixture")
        .with_environment("GIT_WORK_TREE", "/linked");
        let accepted = resolve_writer_worktree(&host, Path::new("/linked")).is_ok();
        let modes = host
            .calls
            .borrow()
            .iter()
            .map(|call| {
                if call.clear_git_environment {
                    "clear"
                } else {
                    "inherit"
                }
            })
            .collect::<Vec<_>>();
        let actual = json!([{"accepted":accepted,"modes":modes}]);
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_writer.py", "writer_git_env", &cases)
        {
            assert_eq!(actual, expected);
        }
        assert_eq!(actual[0]["accepted"], true);
        assert_eq!(
            actual[0]["modes"],
            json!(["inherit", "clear", "inherit", "inherit"])
        );
    }
}
