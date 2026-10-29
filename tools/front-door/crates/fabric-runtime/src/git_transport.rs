//! Bounded Git transport for committed writer results and fenced landing.
//!
//! Every Git child uses bounded OS execution with a fixed executable,
//! explicit environment, disabled hooks and bounded capture. Transport refs
//! are operation-scoped under `refs/provenant/transport/`; no caller supplied
//! ref or command string reaches Git. Peer objects arrive only through `git
//! fetch` with `fsckObjects` and a forced `index-pack`, so Git's own receive
//! path refuses corrupt, missing, duplicate or malformed objects.
use provenant_core::{CancelToken, GitOid, OperationId, ProjectId};
use provenant_fabric_store::{
    ClaimFence, Error as StoreError, LandingGuard, PushAction, PushOperation, PushPhase,
    Request as StoreRequest, Store,
};
use provenant_os::{
    exec::{ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination},
    fs::{self, Directory, FsError, LockGuard},
};
use std::{
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

const MAX_STATUS_BYTES: usize = 64 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
/// A peer fetch has no size cap, so its wall-clock bound is the only limit.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_HOME: &str = "/nonexistent";
const PATH_VALUE: &str = "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin:/opt/local/bin";
const LANDING_REMOTE: &str = "origin";

#[derive(Debug, Error)]
pub enum GitTransportError {
    #[error("invalid Git transport input: {0}")]
    Invalid(&'static str),
    #[error("no supported executable Git was found")]
    GitUnavailable,
    #[error("no supported executable SSH was found")]
    SshUnavailable,
    #[error("Git transport process failed during {command}")]
    GitCommand { command: &'static str },
    #[error("Git process output was incomplete during {command}")]
    IncompleteOutput { command: &'static str },
    #[error("Git returned an invalid result during {command}")]
    InvalidOutput { command: &'static str },
    #[error("writer checkout has uncommitted or untracked changes")]
    DirtyWorktree,
    #[error("shallow or partial Git repositories cannot be transported")]
    IncompleteRepository,
    #[error("Git object identity did not match the requested commit")]
    OidMismatch,
    #[error("candidate is not a fast-forward from the expected commit")]
    NonFastForward,
    #[error("operation already has a different isolated Git object")]
    TransportConflict,
    #[error("remote `origin` has moved from the expected commit")]
    RemoteMoved,
    #[error("configured push URL is unsupported or contains user information")]
    InvalidRemote,
    #[error("push operation {operation} is unresolved and must be reconciled")]
    PushUnresolved { operation: OperationId },
    #[error("push operation {operation} has no durable operation record")]
    PushNotFound { operation: OperationId },
    #[error("push reply was not conclusive; operation {operation} remains fenced")]
    PushUnknown { operation: OperationId },
    #[error("filesystem operation failed")]
    Filesystem(#[from] FsError),
    #[error("Fabric store operation failed")]
    Store(#[from] StoreError),
    #[error("Git supervisor execution failed")]
    Execution(#[from] ExecError),
}

impl GitTransportError {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::PushUnknown { .. } | Self::PushUnresolved { .. })
    }
}

type Result<T> = std::result::Result<T, GitTransportError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportedHead {
    pub operation: OperationId,
    pub base: GitOid,
    pub head: GitOid,
    pub reference: String,
}

/// Only the caller injects the optional SSH agent socket. User Git config,
/// credential helpers, hooks, replacement objects, grafts and lazy fetch are
/// disabled; the URL itself is resolved and frozen before a landing push.
pub struct GitTransport<'a> {
    executor: &'a dyn Executor,
    git_executable: PathBuf,
    ssh_executable: PathBuf,
    ssh_auth_sock: Option<PathBuf>,
}
impl<'a> GitTransport<'a> {
    pub fn new(
        executor: &'a dyn Executor,
        git_executable: PathBuf,
        ssh_auth_sock: Option<PathBuf>,
    ) -> Result<Self> {
        let ssh_executable = crate::hosts::discover_ssh_executable(None)
            .map_err(|_| GitTransportError::SshUnavailable)?;
        Self::new_with_ssh(executor, git_executable, ssh_executable, ssh_auth_sock)
    }

    fn new_with_ssh(
        executor: &'a dyn Executor,
        git_executable: PathBuf,
        ssh_executable: PathBuf,
        ssh_auth_sock: Option<PathBuf>,
    ) -> Result<Self> {
        if !git_executable.is_absolute() || !provenant_os::sys::is_executable(&git_executable) {
            return Err(GitTransportError::GitUnavailable);
        }
        if !crate::config::valid_executable_override(&ssh_executable)
            || !provenant_os::sys::is_executable(&ssh_executable)
        {
            return Err(GitTransportError::SshUnavailable);
        }
        if ssh_auth_sock
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || path.as_os_str().as_bytes().contains(&0))
        {
            return Err(GitTransportError::Invalid("SSH agent socket"));
        }
        Ok(Self {
            executor,
            git_executable,
            ssh_executable,
            ssh_auth_sock,
        })
    }

    pub fn discover(executor: &'a dyn Executor, ssh_auth_sock: Option<PathBuf>) -> Result<Self> {
        Self::discover_with_policy(executor, ssh_auth_sock, None)
    }

    pub fn discover_with_policy(
        executor: &'a dyn Executor,
        ssh_auth_sock: Option<PathBuf>,
        ssh_executable: Option<&Path>,
    ) -> Result<Self> {
        let git = provenant_os::git::git_executable().ok_or(GitTransportError::GitUnavailable)?;
        let ssh = crate::hosts::discover_ssh_executable(ssh_executable).map_err(|_| {
            if ssh_executable.is_some() {
                GitTransportError::Invalid("SSH executable")
            } else {
                GitTransportError::SshUnavailable
            }
        })?;
        Self::new_with_ssh(executor, git, ssh, ssh_auth_sock)
    }

    /// Bind the exact committed range `base..head` to the operation's export
    /// ref, which a peer then fetches by name. The checkout must be clean and
    /// `head` must be its HEAD.
    pub fn export(
        &self,
        repository: &Path,
        operation: &OperationId,
        base: GitOid,
        head: GitOid,
        cancel: &CancelToken,
    ) -> Result<String> {
        self.ensure_clean_worktree(repository, cancel)?;
        self.ensure_commit(repository, base, cancel)?;
        self.ensure_commit(repository, head, cancel)?;
        if base == head {
            return Err(GitTransportError::Invalid("empty Git transport range"));
        }
        if !self.is_ancestor(repository, base, head, cancel)? {
            return Err(GitTransportError::NonFastForward);
        }
        if self.head(repository, cancel)? != head {
            return Err(GitTransportError::OidMismatch);
        }
        let exported = isolated_ref(operation, "export")?;
        self.bind_ref("bind export ref", repository, &exported, head, cancel)?;
        Ok(exported)
    }

    /// Fetch a peer's export ref into an operation-scoped quarantine ref.
    /// `fsckObjects` and a forced `index-pack` check every received object
    /// and connectivity before the ref moves. Promotion to `verified-head`
    /// happens only after the exact OIDs and ancestry are checked. `writer`
    /// must be the run's held checkout writer lock.
    // Keep the held custody proof explicit at this I/O boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn fetch(
        &self,
        repository: &Path,
        writer: &LockGuard,
        remote: &str,
        operation: &OperationId,
        base: GitOid,
        head: GitOid,
        cancel: &CancelToken,
    ) -> Result<TransportedHead> {
        // LockGuard's safe acquisition verifies the persistent private lock
        // inode and retains its descriptor. The owner never unlinks these
        // lock files; Store separately binds the landing guard to its directory.
        let _held_writer = writer;
        validate_remote_url(remote)?;
        let repository = self.ensure_clean_worktree(repository, cancel)?;
        self.ensure_commit(&repository, base, cancel)?;
        let incoming = isolated_ref(operation, "incoming")?;
        let verified = isolated_ref(operation, "verified-head")?;
        match self.read_ref(&repository, &incoming, cancel)? {
            Some(existing) if existing == head => {}
            Some(_) => return Err(GitTransportError::TransportConflict),
            None => {
                self.refuse_url_rewrites(&repository, cancel)?;
                let refspec = format!("{}:{incoming}", isolated_ref(operation, "export")?);
                let mut args = Vec::new();
                for setting in [
                    "fetch.fsckObjects=true",
                    "transfer.fsckObjects=true",
                    "fetch.unpackLimit=1",
                    "transfer.unpackLimit=1",
                    "gc.auto=0",
                    "maintenance.auto=false",
                ] {
                    args.push(OsString::from("-c"));
                    args.push(OsString::from(setting));
                }
                args.extend(strings(&[
                    "fetch",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--no-recurse-submodules",
                    "--",
                    remote,
                    &refspec,
                ]));
                let outcome = self.run_git_for(
                    "fetch transport ref",
                    &repository,
                    &args,
                    MAX_TEXT_BYTES,
                    FETCH_TIMEOUT,
                    cancel,
                )?;
                if outcome.termination != Termination::Exit(0) {
                    return Err(GitTransportError::GitCommand {
                        command: "fetch transport ref",
                    });
                }
            }
        }
        if self.read_ref(&repository, &incoming, cancel)? != Some(head) {
            return Err(GitTransportError::OidMismatch);
        }
        self.ensure_commit(&repository, head, cancel)?;
        if !self.is_ancestor(&repository, base, head, cancel)? {
            return Err(GitTransportError::NonFastForward);
        }
        self.bind_ref(
            "promote verified transport ref",
            &repository,
            &verified,
            head,
            cancel,
        )?;
        Ok(TransportedHead {
            operation: operation.clone(),
            base,
            head,
            reference: verified,
        })
    }

    /// Create `reference` at `oid`, or accept it when it already names `oid`.
    fn bind_ref(
        &self,
        command: &'static str,
        repository: &Path,
        reference: &str,
        oid: GitOid,
        cancel: &CancelToken,
    ) -> Result<()> {
        match self.read_ref(repository, reference, cancel)? {
            Some(existing) if existing == oid => Ok(()),
            Some(_) => Err(GitTransportError::TransportConflict),
            None => {
                let zero = "0".repeat(oid.to_string().len());
                let args = vec![
                    OsString::from("update-ref"),
                    reference.into(),
                    oid.to_string().into(),
                    zero.into(),
                ];
                self.require_success(command, repository, &args, None, MAX_TEXT_BYTES, cancel)?;
                Ok(())
            }
        }
    }

    /// Push the acquired landing candidate through the durable store phases.
    /// The operation's resolved `origin` URL is frozen in the store request.
    // Keep the held custody proof explicit at this I/O boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn push_landing(
        &self,
        store: &Store,
        guard: &LandingGuard,
        fence: &ClaimFence,
        repository: &Path,
        operation: &OperationId,
        request: &StoreRequest<'_>,
        cancel: &CancelToken,
    ) -> Result<PushOperation> {
        let prior = match store.push_operation(guard.project(), operation, request) {
            Ok(value) => {
                if value.request.actor != fence.actor
                    || value.request.claim != fence.id
                    || value.request.generation != fence.generation
                    || value.request.reference != guard.reference()
                {
                    return Err(StoreError::StaleFence.into());
                }
                if value.phase == PushPhase::Committed {
                    return Ok(value);
                }
                Some(value)
            }
            Err(StoreError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        let landing = store.verify_landing(guard, fence, request)?;
        validate_branch_ref(&landing.reference)?;
        let repository = self.ensure_clean_worktree(repository, cancel)?;
        self.ensure_commit(&repository, landing.expected, cancel)?;
        self.ensure_commit(&repository, landing.candidate, cancel)?;
        if !self.is_ancestor(&repository, landing.expected, landing.candidate, cancel)? {
            return Err(GitTransportError::NonFastForward);
        }

        let remote = match &prior {
            Some(value) => value.request.remote.clone(),
            None => self.resolve_origin_url(&repository, cancel)?,
        };
        validate_remote_url(&remote)?;
        let current = store.prepare_push(guard, fence, operation, &remote, request)?;
        if current.phase == PushPhase::Committed {
            return Ok(current);
        }
        if matches!(current.phase, PushPhase::Pushing | PushPhase::Unknown) {
            return Err(GitTransportError::PushUnresolved {
                operation: operation.clone(),
            });
        }
        let remote_oid = self.remote_oid(&repository, &remote, &landing.reference, cancel)?;
        if remote_oid != Some(landing.expected) {
            return Err(GitTransportError::RemoteMoved);
        }
        store.verify_landing(guard, fence, request)?;
        store.change_push(guard, fence, operation, PushAction::Begin, request)?;
        self.refuse_url_rewrites(&repository, cancel)?;
        let lease = format!(
            "--force-with-lease={}:{}",
            landing.reference, landing.expected
        );
        let refspec = format!("{}:{}", landing.candidate, landing.reference);
        let args = vec![
            OsString::from("push"),
            OsString::from("--porcelain"),
            OsString::from("--no-verify"),
            OsString::from("--no-follow-tags"),
            lease.into(),
            OsString::from("--"),
            remote.into(),
            refspec.into(),
        ];
        match self.run_git("push", &repository, &args, None, MAX_TEXT_BYTES, cancel) {
            Ok(outcome) if outcome.termination == Termination::Exit(0) => {
                Ok(store.change_push(guard, fence, operation, PushAction::Confirmed, request)?)
            }
            _ => {
                // Once Begin is durable, any process or reply failure is
                // uncertain. Never release or start a successor from here.
                let _ = store.change_push(guard, fence, operation, PushAction::Uncertain, request);
                Err(GitTransportError::PushUnknown {
                    operation: operation.clone(),
                })
            }
        }
    }

    /// Reconcile only a durable in-flight/unknown push. No retry is attempted
    /// when the remote still equals the expected OID; the caller must request it.
    // Keep the held custody proof explicit at this I/O boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_landing(
        &self,
        store: &Store,
        guard: &LandingGuard,
        fence: &ClaimFence,
        project: ProjectId,
        operation: &OperationId,
        request: &StoreRequest<'_>,
        repository: &Path,
        cancel: &CancelToken,
    ) -> Result<PushOperation> {
        let value = store.push_operation(project, operation, request)?;
        if !matches!(value.phase, PushPhase::Pushing | PushPhase::Unknown) {
            return Ok(value);
        }
        let remote = value.request.remote.clone();
        validate_remote_url(&remote)?;
        let observed = self.remote_oid(repository, &remote, &value.request.reference, cancel)?;
        Ok(store.change_push(
            guard,
            fence,
            operation,
            PushAction::Reconcile { remote: observed },
            request,
        )?)
    }

    fn run_git(
        &self,
        command: &'static str,
        cwd: &Path,
        args: &[OsString],
        stdin: Option<&[u8]>,
        output_limit: usize,
        cancel: &CancelToken,
    ) -> Result<ExecOutcome> {
        self.execute(command, cwd, args, stdin, output_limit, GIT_TIMEOUT, cancel)
    }

    fn run_git_for(
        &self,
        command: &'static str,
        cwd: &Path,
        args: &[OsString],
        output_limit: usize,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<ExecOutcome> {
        self.execute(command, cwd, args, None, output_limit, timeout, cancel)
    }

    // One bounded Git child; the arguments mirror ExecOptions.
    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        command: &'static str,
        cwd: &Path,
        args: &[OsString],
        stdin: Option<&[u8]>,
        output_limit: usize,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<ExecOutcome> {
        if output_limit == 0 || output_limit > MAX_STATUS_BYTES.max(MAX_TEXT_BYTES) {
            return Err(GitTransportError::Invalid("Git output limit"));
        }
        let mut argv = vec![
            self.git_executable.as_os_str().to_owned(),
            OsString::from("--no-pager"),
            OsString::from("--no-optional-locks"),
            OsString::from("--no-replace-objects"),
            OsString::from("--literal-pathspecs"),
        ];
        let ssh_command = format!(
            "core.sshCommand={} -oBatchMode=yes",
            self.ssh_executable.display()
        );
        for setting in [
            "core.hooksPath=/dev/null".to_owned(),
            ssh_command,
            "core.fsmonitor=false".to_owned(),
            "core.attributesFile=/dev/null".to_owned(),
            "core.fsmonitorHookVersion=0".to_owned(),
            "credential.helper=".to_owned(),
            "credential.interactive=false".to_owned(),
            "protocol.allow=never".to_owned(),
            "protocol.file.allow=always".to_owned(),
            "protocol.ssh.allow=always".to_owned(),
            "protocol.https.allow=always".to_owned(),
            "protocol.http.allow=always".to_owned(),
            "core.bare=false".to_owned(),
        ] {
            argv.push(OsString::from("-c"));
            argv.push(OsString::from(setting));
        }
        argv.extend_from_slice(args);
        let mut env = vec![
            (OsString::from("HOME"), OsString::from(DEFAULT_HOME)),
            (OsString::from("TMPDIR"), OsString::from("/tmp")),
            (OsString::from("PATH"), OsString::from(PATH_VALUE)),
            (OsString::from("LANG"), OsString::from("C")),
            (OsString::from("LC_ALL"), OsString::from("C")),
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_SYSTEM"),
                OsString::from("/dev/null"),
            ),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                OsString::from("/dev/null"),
            ),
            (OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")),
            (
                OsString::from("GIT_NO_REPLACE_OBJECTS"),
                OsString::from("1"),
            ),
            (
                OsString::from("GIT_GRAFT_FILE"),
                OsString::from("/dev/null"),
            ),
            (OsString::from("GIT_NO_LAZY_FETCH"), OsString::from("1")),
            (OsString::from("GIT_ATTR_NOSYSTEM"), OsString::from("1")),
            (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
            (OsString::from("GIT_PAGER"), OsString::from("cat")),
            (OsString::from("PAGER"), OsString::from("cat")),
            (OsString::from("GIT_CONFIG_COUNT"), OsString::from("0")),
        ];
        if let Some(socket) = &self.ssh_auth_sock {
            env.push((
                OsString::from("SSH_AUTH_SOCK"),
                socket.as_os_str().to_owned(),
            ));
        }
        let mut options = ExecOptions::new(&argv, cwd, timeout);
        options.env = Some(&env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture {
            limit: output_limit,
        };
        options.stderr = StreamPolicy::Discard;
        options.cancel = Some(cancel);
        options.stop_on_output_limit = true;
        let outcome = self.executor.execute(options)?;
        if outcome.timed_out
            || outcome.cancelled
            || outcome.overflow_stopped
            || outcome.drain_truncated
        {
            return Err(GitTransportError::IncompleteOutput { command });
        }
        for stream in [&outcome.stdout, &outcome.stderr] {
            if stream.truncated
                || stream.sink_failed
                || stream.sink_lost_bytes != 0
                || stream.oversized_frames != 0
                || stream.decoder_lost_frames != 0
                || stream.read_failed
                || stream.filter_failed
            {
                return Err(GitTransportError::IncompleteOutput { command });
            }
        }
        Ok(outcome)
    }

    fn require_success(
        &self,
        command: &'static str,
        cwd: &Path,
        args: &[OsString],
        stdin: Option<&[u8]>,
        output_limit: usize,
        cancel: &CancelToken,
    ) -> Result<ExecOutcome> {
        let outcome = self.run_git(command, cwd, args, stdin, output_limit, cancel)?;
        if outcome.termination != Termination::Exit(0) {
            return Err(GitTransportError::GitCommand { command });
        }
        Ok(outcome)
    }

    fn ensure_repo_root(&self, repository: &Path, cancel: &CancelToken) -> Result<PathBuf> {
        let root = fs::canonicalize(repository)?;
        let _held = Directory::open_anchor(&root)?;
        let args = strings(&["rev-parse", "--show-toplevel"]);
        let output = self.require_success(
            "repository root",
            &root,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        let value = one_line(&output.stdout.bytes, "repository root")?;
        let text = std::str::from_utf8(value).map_err(|_| GitTransportError::InvalidOutput {
            command: "repository root",
        })?;
        let git_root = fs::canonicalize(Path::new(text))?;
        if git_root != root {
            return Err(GitTransportError::Invalid("repository root binding"));
        }
        let shallow = self.require_success(
            "shallow repository check",
            &root,
            &strings(&["rev-parse", "--is-shallow-repository"]),
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        if one_line(&shallow.stdout.bytes, "shallow repository check")? != b"false" {
            return Err(GitTransportError::IncompleteRepository);
        }
        Ok(root)
    }

    fn ensure_clean_worktree(&self, repository: &Path, cancel: &CancelToken) -> Result<PathBuf> {
        let root = self.ensure_repo_root(repository, cancel)?;
        let status = self.require_success(
            "worktree status",
            &root,
            &strings(&[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ]),
            None,
            MAX_STATUS_BYTES,
            cancel,
        )?;
        if !status.stdout.bytes.is_empty() {
            return Err(GitTransportError::DirtyWorktree);
        }
        Ok(root)
    }

    fn object_format(&self, repository: &Path, cancel: &CancelToken) -> Result<&'static str> {
        let output = self.require_success(
            "object format",
            repository,
            &strings(&["rev-parse", "--show-object-format"]),
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        match one_line(&output.stdout.bytes, "object format")? {
            b"sha1" => Ok("sha1"),
            b"sha256" => Ok("sha256"),
            _ => Err(GitTransportError::InvalidOutput {
                command: "object format",
            }),
        }
    }

    fn ensure_commit(&self, repository: &Path, oid: GitOid, cancel: &CancelToken) -> Result<()> {
        let format = self.object_format(repository, cancel)?;
        if !matches!(
            (format, oid),
            ("sha1", GitOid::Sha1(_)) | ("sha256", GitOid::Sha256(_))
        ) {
            return Err(GitTransportError::OidMismatch);
        }
        let revision = format!("{oid}^{{commit}}");
        let args = vec![
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--end-of-options"),
            revision.into(),
        ];
        let output = self.require_success(
            "commit identity",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        if parse_oid_line(&output.stdout.bytes, "commit identity")? != oid {
            return Err(GitTransportError::OidMismatch);
        }
        Ok(())
    }

    fn head(&self, repository: &Path, cancel: &CancelToken) -> Result<GitOid> {
        let output = self.require_success(
            "HEAD identity",
            repository,
            &strings(&["rev-parse", "--verify", "HEAD^{commit}"]),
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        parse_oid_line(&output.stdout.bytes, "HEAD identity")
    }

    fn is_ancestor(
        &self,
        repository: &Path,
        base: GitOid,
        head: GitOid,
        cancel: &CancelToken,
    ) -> Result<bool> {
        let args = vec![
            OsString::from("merge-base"),
            OsString::from("--is-ancestor"),
            base.to_string().into(),
            head.to_string().into(),
        ];
        let outcome = self.run_git(
            "ancestry check",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        match outcome.termination {
            Termination::Exit(0) => Ok(true),
            Termination::Exit(1) => Ok(false),
            _ => Err(GitTransportError::GitCommand {
                command: "ancestry check",
            }),
        }
    }

    fn read_ref(
        &self,
        repository: &Path,
        reference: &str,
        cancel: &CancelToken,
    ) -> Result<Option<GitOid>> {
        let args = vec![
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            reference.into(),
        ];
        let outcome = self.run_git(
            "isolated ref lookup",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        match outcome.termination {
            Termination::Exit(0) => Ok(Some(parse_oid_line(
                &outcome.stdout.bytes,
                "isolated ref lookup",
            )?)),
            Termination::Exit(1) if outcome.stdout.bytes.is_empty() => Ok(None),
            _ => Err(GitTransportError::GitCommand {
                command: "isolated ref lookup",
            }),
        }
    }

    fn refuse_url_rewrites(&self, repository: &Path, cancel: &CancelToken) -> Result<()> {
        let args = strings(&[
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            r"^url\..*\.(insteadof|pushinsteadof)$",
        ]);
        let outcome = self.run_git(
            "check URL rewrites",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        if outcome.termination == Termination::Exit(1) && outcome.stdout.bytes.is_empty() {
            Ok(())
        } else {
            Err(GitTransportError::InvalidRemote)
        }
    }
    fn resolve_origin_url(&self, repository: &Path, cancel: &CancelToken) -> Result<String> {
        self.refuse_url_rewrites(repository, cancel)?;
        let args = strings(&["remote", "get-url", "--push", "--all", LANDING_REMOTE]);
        let output = self.require_success(
            "resolve origin push URL",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        let value = one_line(&output.stdout.bytes, "resolve origin push URL")?;
        let remote = std::str::from_utf8(value)
            .map_err(|_| GitTransportError::InvalidRemote)?
            .to_owned();
        validate_remote_url(&remote)?;
        if Path::new(&remote).is_absolute() {
            let canonical = fs::canonicalize(Path::new(&remote))?;
            return Ok(canonical
                .to_str()
                .ok_or(GitTransportError::InvalidRemote)?
                .to_owned());
        }
        Ok(remote)
    }

    fn remote_oid(
        &self,
        repository: &Path,
        remote: &str,
        reference: &str,
        cancel: &CancelToken,
    ) -> Result<Option<GitOid>> {
        validate_remote_url(remote)?;
        self.refuse_url_rewrites(repository, cancel)?;
        validate_branch_ref(reference)?;
        let args = vec![
            OsString::from("ls-remote"),
            OsString::from("--refs"),
            OsString::from("--"),
            remote.into(),
            reference.into(),
        ];
        let output = self.require_success(
            "remote OID observation",
            repository,
            &args,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        if output.stdout.bytes.is_empty() {
            return Ok(None);
        }
        let line = one_line(&output.stdout.bytes, "remote OID observation")?;
        let mut fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|part| !part.is_empty());
        let oid_text = fields.next().ok_or(GitTransportError::InvalidOutput {
            command: "remote OID observation",
        })?;
        let ref_text = fields.next().ok_or(GitTransportError::InvalidOutput {
            command: "remote OID observation",
        })?;
        if fields.next().is_some() || ref_text != reference.as_bytes() {
            return Err(GitTransportError::InvalidOutput {
                command: "remote OID observation",
            });
        }
        let text = std::str::from_utf8(oid_text).map_err(|_| GitTransportError::InvalidOutput {
            command: "remote OID observation",
        })?;
        Ok(Some(GitOid::parse(text).map_err(|_| {
            GitTransportError::InvalidOutput {
                command: "remote OID observation",
            }
        })?))
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn one_line<'a>(bytes: &'a [u8], command: &'static str) -> Result<&'a [u8]> {
    let line = bytes
        .strip_suffix(b"\n")
        .ok_or(GitTransportError::InvalidOutput { command })?;
    if line.is_empty() || line.contains(&b'\n') || line.contains(&0) {
        return Err(GitTransportError::InvalidOutput { command });
    }
    Ok(line)
}

fn parse_oid_line(bytes: &[u8], command: &'static str) -> Result<GitOid> {
    let line = one_line(bytes, command)?;
    let text =
        std::str::from_utf8(line).map_err(|_| GitTransportError::InvalidOutput { command })?;
    GitOid::parse(text).map_err(|_| GitTransportError::InvalidOutput { command })
}

fn isolated_ref(operation: &OperationId, leaf: &str) -> Result<String> {
    let value = format!("refs/provenant/transport/{}/{leaf}", operation.as_str());
    if value.len() > 1024 || value.contains("..") {
        return Err(GitTransportError::Invalid("transport ref"));
    }
    Ok(value)
}

fn validate_branch_ref(reference: &str) -> Result<()> {
    if !reference.starts_with("refs/heads/")
        || reference.len() > 1024
        || !reference.is_ascii()
        || reference
            .bytes()
            .any(|byte| byte <= 32 || byte == 127 || b"~^:?*[\\".contains(&byte))
        || reference.contains("..")
        || reference.contains("@{")
        || reference.ends_with('.')
        || reference
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
    {
        return Err(GitTransportError::Invalid("landing branch ref"));
    }
    Ok(())
}

fn validate_remote_url(remote: &str) -> Result<()> {
    if remote.is_empty() || remote.len() > 2048 || remote.contains('\0') {
        return Err(GitTransportError::InvalidRemote);
    }
    if Path::new(remote).is_absolute() {
        if remote.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(GitTransportError::InvalidRemote);
        }
        if Path::new(remote)
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(GitTransportError::InvalidRemote);
        }
        return Ok(());
    }
    if remote
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || remote.contains('?')
        || remote.contains('#')
        || remote.contains('\\')
    {
        return Err(GitTransportError::InvalidRemote);
    }
    if let Some(rest) = remote.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty()
            || authority.contains('@')
            || authority.contains(':')
                && !authority.rsplit_once(':').is_some_and(|(_, port)| {
                    !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
                })
        {
            return Err(GitTransportError::InvalidRemote);
        }
        return Ok(());
    }
    if let Some(rest) = remote.strip_prefix("ssh://") {
        let authority = rest.split('/').next().unwrap_or_default();
        let host_port = authority
            .rsplit_once('@')
            .map_or(Some(authority), |(user, host_port)| {
                (!user.is_empty() && !user.contains(':')).then_some(host_port)
            })
            .ok_or(GitTransportError::InvalidRemote)?;
        let valid_port = |port: &str| {
            !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port > 0)
        };
        let host = if let Some(bracketed) = host_port.strip_prefix('[') {
            let (host, suffix) = bracketed
                .split_once(']')
                .ok_or(GitTransportError::InvalidRemote)?;
            if host.is_empty()
                || host.contains('[')
                || suffix.contains(']')
                || (!suffix.is_empty() && !suffix.strip_prefix(':').is_some_and(valid_port))
            {
                return Err(GitTransportError::InvalidRemote);
            }
            host
        } else {
            match host_port.split_once(':') {
                Some((host, port)) if !host.contains(':') && valid_port(port) => host,
                Some(_) => return Err(GitTransportError::InvalidRemote),
                None => host_port,
            }
        };
        if host.is_empty() || host.starts_with('-') || host.contains('@') {
            return Err(GitTransportError::InvalidRemote);
        }
        return Ok(());
    }
    // The conventional `git@host:path` form has a user name but no password.
    if let Some((authority, path)) = remote.split_once(':') {
        if authority.is_empty()
            || path.is_empty()
            || authority.contains('/')
            || authority.matches('@').count() > 1
        {
            return Err(GitTransportError::InvalidRemote);
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(user, host)| {
                if user.is_empty() || user.contains(':') {
                    ""
                } else {
                    host
                }
            });
        if host.is_empty()
            || host.starts_with('-')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(GitTransportError::InvalidRemote);
        }
        return Ok(());
    }
    Err(GitTransportError::InvalidRemote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{CancelToken, Clock, MonoTime, TestClock, WaitBudget, WallTime};
    use provenant_os::{
        exec::{CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, SystemExecutor},
        fs::Directory,
    };
    use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt};

    struct Fixture {
        root: tempfile::TempDir,
        clock: TestClock,
        cancel: CancelToken,
        executor: SystemExecutor,
    }
    impl Fixture {
        fn new() -> Self {
            let root = crate::test_support::tempdir().unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                clock: TestClock::new(
                    WallTime::from_micros(2_000_000_000_000_000).unwrap(),
                    MonoTime::from_micros(0),
                ),
                cancel: CancelToken::new(),
                executor: SystemExecutor::new(2).unwrap(),
            }
        }

        fn transport(&self) -> GitTransport<'_> {
            GitTransport::discover(&self.executor, None).unwrap()
        }

        /// A fresh repository. Git children get the transport's explicit
        /// environment, so the test process's `GIT_*` variables never leak in.
        fn repo(&self, name: &str) -> PathBuf {
            let path = self.root.path().join(name);
            fs::create_dir(&path).unwrap();
            git(
                &self.transport(),
                self.root.path(),
                &["init", "-b", "main", path.to_str().unwrap()],
            );
            path
        }

        fn writer(&self) -> LockGuard {
            let path = self.root.path().join("writer-locks");
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Directory::open_anchor(&path)
                .unwrap()
                .lock(
                    OsStr::new("writer.lock"),
                    provenant_os::fs::LockMode::Exclusive,
                    WaitBudget::cli(self.clock.mono_now(), 1).unwrap(),
                    &self.cancel,
                    &self.clock,
                )
                .unwrap()
        }

        /// A receiver that already holds `base`, as a peer with the shared
        /// remote does.
        fn receiver(&self, sender: &Path, base: GitOid) -> PathBuf {
            let receiver = self.repo("receiver");
            let refspec = format!("{base}:refs/provenant/base");
            git(
                &self.transport(),
                &receiver,
                &["fetch", "-q", sender.to_str().unwrap(), &refspec],
            );
            receiver
        }

        fn fetch(
            &self,
            receiver: &Path,
            sender: &Path,
            operation: &str,
            base: GitOid,
            head: GitOid,
        ) -> Result<TransportedHead> {
            self.transport().fetch(
                receiver,
                &self.writer(),
                sender.to_str().unwrap(),
                &OperationId::parse(operation).unwrap(),
                base,
                head,
                &self.cancel,
            )
        }
    }

    struct CaptureCommandExecutor {
        argv: std::sync::Mutex<Option<Vec<OsString>>>,
    }
    impl Executor for CaptureCommandExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
            *self.argv.lock().unwrap() = Some(options.argv.to_vec());
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream::default(),
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    fn git_with(transport: &GitTransport<'_>, cwd: &Path, args: &[&str], stdin: &[u8]) -> String {
        let output = transport
            .require_success(
                "test setup",
                cwd,
                &strings(args),
                Some(stdin),
                MAX_TEXT_BYTES,
                &CancelToken::new(),
            )
            .unwrap();
        String::from_utf8(output.stdout.bytes)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn git(transport: &GitTransport<'_>, cwd: &Path, args: &[&str]) -> String {
        git_with(transport, cwd, args, b"")
    }

    fn commit(transport: &GitTransport<'_>, repo: &Path, args: &[&str]) -> GitOid {
        let mut argv = vec![
            "-c",
            "user.name=Fabric Test",
            "-c",
            "user.email=fabric@example.test",
            "commit",
            "-q",
        ];
        argv.extend_from_slice(args);
        git(transport, repo, &argv);
        transport.head(repo, &CancelToken::new()).unwrap()
    }

    /// Write `bytes` as an object Git would refuse to create itself.
    fn literal(transport: &GitTransport<'_>, repo: &Path, kind: &str, bytes: &[u8]) -> String {
        git_with(
            transport,
            repo,
            &["hash-object", "-t", kind, "-w", "--literally", "--stdin"],
            bytes,
        )
    }

    fn tree_entry(name: &str, oid: &str) -> Vec<u8> {
        let mut entry = format!("100644 {name}\0").into_bytes();
        entry.extend(
            (0..oid.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&oid[i..i + 2], 16).unwrap()),
        );
        entry
    }

    fn has_object(transport: &GitTransport<'_>, repo: &Path, oid: &str) -> bool {
        transport
            .run_git(
                "test object lookup",
                repo,
                &strings(&["cat-file", "-e", oid]),
                None,
                MAX_TEXT_BYTES,
                &CancelToken::new(),
            )
            .unwrap()
            .termination
            == Termination::Exit(0)
    }

    fn writable(path: &Path) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn committed_head_round_trip_is_fetched_verified_and_isolated() {
        let fixture = Fixture::new();
        let transport = fixture.transport();
        let sender = fixture.repo("sender");
        let base = commit(&transport, &sender, &["--allow-empty", "-m", "base"]);
        let head = commit(&transport, &sender, &["--allow-empty", "-m", "head"]);
        let operation = OperationId::parse("lane-c-1").unwrap();
        assert!(matches!(
            transport.export(&sender, &operation, base, base, &fixture.cancel),
            Err(GitTransportError::Invalid(_))
        ));
        let exported = transport
            .export(&sender, &operation, base, head, &fixture.cancel)
            .unwrap();
        assert_eq!(exported, "refs/provenant/transport/lane-c-1/export");

        let receiver = fixture.receiver(&sender, base);
        let transported = fixture
            .fetch(&receiver, &sender, "lane-c-1", base, head)
            .unwrap();
        assert_eq!(transported.head, head);
        assert_eq!(
            transported.reference,
            "refs/provenant/transport/lane-c-1/verified-head"
        );
        assert_eq!(
            transport
                .read_ref(&receiver, &transported.reference, &fixture.cancel)
                .unwrap(),
            Some(head)
        );
        // A retry with the same operation is idempotent; another head is not.
        assert_eq!(
            fixture
                .fetch(&receiver, &sender, "lane-c-1", base, head)
                .unwrap(),
            transported
        );
        assert!(matches!(
            fixture.fetch(&receiver, &sender, "lane-c-1", base, base),
            Err(GitTransportError::TransportConflict)
        ));
    }

    #[test]
    fn commits_over_64_mib_transport() {
        let fixture = Fixture::new();
        let transport = fixture.transport();
        let sender = fixture.repo("sender");
        let base = commit(&transport, &sender, &["--allow-empty", "-m", "base"]);
        // Incompressible content, so the pack itself exceeds the old bound.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let bytes: Vec<u8> = (0..(65 << 20) / 8)
            .flat_map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()
            })
            .collect();
        fs::write(sender.join("large.bin"), bytes).unwrap();
        git(&transport, &sender, &["add", "large.bin"]);
        let head = commit(&transport, &sender, &["-m", "large"]);
        transport
            .export(
                &sender,
                &OperationId::parse("large").unwrap(),
                base,
                head,
                &fixture.cancel,
            )
            .unwrap();

        let receiver = fixture.receiver(&sender, base);
        let transported = fixture
            .fetch(&receiver, &sender, "large", base, head)
            .unwrap();
        assert_eq!(transported.head, head);
        let size = git(
            &transport,
            &receiver,
            &["cat-file", "-s", &format!("{head}:large.bin")],
        );
        assert_eq!(size, (65 << 20).to_string());
    }

    #[test]
    fn fetch_refuses_corrupt_missing_duplicate_and_malformed_objects() {
        let fixture = Fixture::new();
        let transport = fixture.transport();
        let sender = fixture.repo("sender");
        let base = commit(&transport, &sender, &["--allow-empty", "-m", "base"]);
        let receiver = fixture.receiver(&sender, base);
        let base_tree = git(&transport, &sender, &["rev-parse", "HEAD^{tree}"]);
        let child = |tree: &str, message: &str| {
            git(
                &transport,
                &sender,
                &[
                    "-c",
                    "user.name=Fabric Test",
                    "-c",
                    "user.email=fabric@example.test",
                    "commit-tree",
                    tree,
                    "-p",
                    &base.to_string(),
                    "-m",
                    message,
                ],
            )
        };

        // Malformed: a commit whose author line has no valid date.
        let malformed = literal(
            &transport,
            &sender,
            "commit",
            format!(
                "tree {base_tree}\nparent {base}\nauthor A <a@example.test> never +0000\n\
                 committer A <a@example.test> 1 +0000\n\nmalformed\n"
            )
            .as_bytes(),
        );
        // Duplicate: a tree naming the same path twice.
        let blob = git_with(
            &transport,
            &sender,
            &["hash-object", "-w", "--stdin"],
            b"x\n",
        );
        let duplicate = literal(
            &transport,
            &sender,
            "tree",
            &[tree_entry("a", &blob), tree_entry("a", &blob)].concat(),
        );
        let duplicate = child(&duplicate, "duplicate");
        // Missing: a tree naming a blob the sender does not have.
        let missing = literal(
            &transport,
            &sender,
            "tree",
            &tree_entry("a", &"1".repeat(40)),
        );
        let missing = child(&missing, "missing");
        // Corrupt: a loose object whose content hashes to another name.
        let named = git_with(
            &transport,
            &sender,
            &["hash-object", "-w", "--stdin"],
            b"named\n",
        );
        let other = git_with(
            &transport,
            &sender,
            &["hash-object", "-w", "--stdin"],
            b"other\n",
        );
        let loose = |oid: &str| sender.join(format!(".git/objects/{}/{}", &oid[..2], &oid[2..]));
        writable(&loose(&named));
        fs::copy(loose(&other), loose(&named)).unwrap();
        let tree = git_with(
            &transport,
            &sender,
            &["mktree"],
            format!("100644 blob {named}\tf\n").as_bytes(),
        );
        let mismatched = child(&tree, "mismatched");

        for (index, head) in [malformed, duplicate, missing, mismatched]
            .iter()
            .enumerate()
        {
            let operation = format!("refused-{index}");
            let export = isolated_ref(&OperationId::parse(&operation).unwrap(), "export").unwrap();
            // A hostile peer advertises the object directly.
            git(&transport, &sender, &["update-ref", &export, head]);
            let head = GitOid::parse(head).unwrap();
            let result = fixture.fetch(&receiver, &sender, &operation, base, head);
            assert!(
                matches!(
                    result,
                    Err(GitTransportError::GitCommand {
                        command: "fetch transport ref"
                    })
                ),
                "{operation}: {result:?}"
            );
            assert!(
                !has_object(&transport, &receiver, &head.to_string()),
                "{operation}"
            );
            let incoming =
                isolated_ref(&OperationId::parse(&operation).unwrap(), "incoming").unwrap();
            assert_eq!(
                transport
                    .read_ref(&receiver, &incoming, &fixture.cancel)
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn fetch_refuses_a_corrupt_pack() {
        let fixture = Fixture::new();
        let transport = fixture.transport();
        let sender = fixture.repo("sender");
        let base = commit(&transport, &sender, &["--allow-empty", "-m", "base"]);
        let receiver = fixture.receiver(&sender, base);
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let noise: Vec<u8> = (0..512)
            .flat_map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()
            })
            .collect();
        fs::write(sender.join("noise.bin"), noise).unwrap();
        git(&transport, &sender, &["add", "noise.bin"]);
        let head = commit(&transport, &sender, &["-m", "noise"]);
        transport
            .export(
                &sender,
                &OperationId::parse("corrupt-pack").unwrap(),
                base,
                head,
                &fixture.cancel,
            )
            .unwrap();
        // Upload-pack reuses packed bytes verbatim, so a flipped byte in the
        // sender's pack reaches the receiver's index-pack unchanged.
        git(&transport, &sender, &["repack", "-q", "-a", "-d"]);
        let pack = fs::read_dir(sender.join(".git/objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension() == Some(OsStr::new("pack")))
            .unwrap();
        writable(&pack);
        let mut bytes = fs::read(&pack).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        fs::write(&pack, bytes).unwrap();

        let result = fixture.fetch(&receiver, &sender, "corrupt-pack", base, head);
        assert!(
            matches!(result, Err(GitTransportError::GitCommand { .. })),
            "{result:?}"
        );
        assert!(!has_object(&transport, &receiver, &head.to_string()));
    }

    #[test]
    fn configured_ssh_executable_is_used_for_git_remotes() {
        let executor = CaptureCommandExecutor {
            argv: std::sync::Mutex::new(None),
        };
        let transport =
            GitTransport::discover_with_policy(&executor, None, Some(Path::new("/bin/sh")))
                .unwrap();

        transport
            .run_git(
                "test",
                Path::new("/"),
                &[OsString::from("--version")],
                None,
                MAX_TEXT_BYTES,
                &CancelToken::new(),
            )
            .unwrap();

        assert!(
            executor
                .argv
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .iter()
                .any(|arg| arg == "core.sshCommand=/bin/sh -oBatchMode=yes")
        );
    }

    #[test]
    fn dirty_worktree_and_untrusted_remote_inputs_refuse_before_transfer() {
        let fixture = Fixture::new();
        let transport = fixture.transport();
        let repo = fixture.repo("repo");
        fs::write(repo.join("untracked"), b"dirty").unwrap();
        assert!(matches!(
            transport.ensure_clean_worktree(&repo, &fixture.cancel),
            Err(GitTransportError::DirtyWorktree)
        ));
        for remote in [
            "https://user:password@example.test/repo.git",
            "ext::sh -c evil",
            "ssh://user:password@example.test/repo.git",
            "git@example.test:../escape.git",
        ] {
            assert!(validate_remote_url(remote).is_err(), "{remote}");
        }
        assert!(validate_remote_url("git@example.test:org/repo.git").is_ok());
    }
}

#[cfg(test)]
#[path = "landing_tests.rs"]
mod landing_tests;
