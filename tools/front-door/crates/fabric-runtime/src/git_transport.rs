//! Bounded Git transport for committed writer results and fenced landing.
//!
//! Every Git child runs through supervisor custody with a fixed executable,
//! explicit environment, disabled hooks and bounded capture. Bundle refs are
//! operation-scoped under `refs/provenant/transport/`; no caller supplied ref
//! or command string reaches Git.
use provenant_core::{CancelToken, GitOid, OperationId, ProjectId};
use provenant_fabric_store::{
    ClaimFence, Error as StoreError, LandingGuard, PushAction, PushOperation, PushPhase,
    Request as StoreRequest, Store,
};
use provenant_supervisor::{
    exec::{ExecError, ExecOptions, ExecOutcome, Executor, StreamPolicy, Termination},
    fs::{self, Directory, FileKind, FsError, LockGuard, Publication, PublishMode},
};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

pub const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 64 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_HOME: &str = "/nonexistent";
const PATH_VALUE: &str = "/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin:/opt/local/bin";
const BUNDLE_DIR_MODE: u32 = 0o700;
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
    #[error("Git bundle exceeds the configured transport bound")]
    BundleTooLarge,
    #[error("Git bundle publication is visible but its directory sync failed")]
    BundleDurabilityUnconfirmed,
    #[error("Git bundle did not advertise exactly the requested head")]
    BundleHeadMismatch,
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

/// A private, durable directory for bundles. Files remain for explicit
/// maintenance; retries with the same operation ID verify the existing bundle.
pub struct BundleArea {
    directory: Directory,
    path: PathBuf,
}
impl BundleArea {
    /// Open or create the final directory component beneath an existing private
    /// parent. The parent and child are held through no-follow descriptors.
    pub fn open_or_create(path: &Path) -> Result<Self> {
        if !path.is_absolute() {
            return Err(GitTransportError::Invalid("bundle area path"));
        }
        let name = path
            .file_name()
            .ok_or(GitTransportError::Invalid("bundle area path"))?;
        if name.as_bytes().is_empty() || name.as_bytes().contains(&0) {
            return Err(GitTransportError::Invalid("bundle area path"));
        }
        let parent_path = path
            .parent()
            .ok_or(GitTransportError::Invalid("bundle area parent"))?;
        let parent = Directory::open_anchor(parent_path)?;
        require_private(&parent)?;
        match parent.mkdir(name, BUNDLE_DIR_MODE) {
            Ok(()) => parent.sync()?,
            Err(error) if error.is_exists() => {}
            Err(error) => return Err(error.into()),
        }
        let directory = parent.open_verified_directory(name)?;
        require_private(&directory)?;
        let canonical_parent = fs::canonicalize(parent_path)?;
        let rebound_parent = Directory::open_anchor(&canonical_parent)?;
        if !rebound_parent
            .metadata()?
            .same_identity(&parent.metadata()?)
        {
            return Err(GitTransportError::Invalid("bundle area parent changed"));
        }
        Ok(Self {
            directory,
            path: canonical_parent.join(name),
        })
    }

    fn bundle_path(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }

    fn read_bundle(&self, name: &OsStr, max_bytes: usize) -> Result<Vec<u8>> {
        let mut file = self.directory.open_read(name)?;
        let metadata = file.metadata()?;
        let area = self.directory.metadata()?;
        if metadata.kind != FileKind::Regular
            || metadata.links != 1
            || metadata.device != area.device
        {
            return Err(GitTransportError::Invalid("bundle file identity"));
        }
        let bytes = file
            .read_bounded(max_bytes as u64)
            .map_err(|error| match error {
                FsError::TooLarge { .. } => GitTransportError::BundleTooLarge,
                other => GitTransportError::Filesystem(other),
            })?;
        if bytes.is_empty() {
            return Err(GitTransportError::Invalid("empty Git bundle"));
        }
        Ok(bytes)
    }
}

fn require_private(directory: &Directory) -> Result<()> {
    let metadata = directory.metadata()?;
    if metadata.kind != FileKind::Directory
        || metadata.uid != fs::effective_uid()
        || metadata.mode & 0o077 != 0
    {
        return Err(GitTransportError::Invalid(
            "Git transport directory permissions",
        ));
    }
    Ok(())
}

pub struct GitBundle {
    pub operation: OperationId,
    pub base: GitOid,
    pub head: GitOid,
    pub bytes: Vec<u8>,
}

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
    max_bundle_bytes: usize,
}
impl<'a> GitTransport<'a> {
    pub fn new(
        executor: &'a dyn Executor,
        git_executable: PathBuf,
        ssh_auth_sock: Option<PathBuf>,
    ) -> Result<Self> {
        let ssh_executable = crate::hosts::discover_ssh_executable(None)
            .map_err(|_| GitTransportError::SshUnavailable)?;
        Self::new_with_policy(
            executor,
            git_executable,
            ssh_executable,
            ssh_auth_sock,
            MAX_BUNDLE_BYTES,
        )
    }

    fn new_with_policy(
        executor: &'a dyn Executor,
        git_executable: PathBuf,
        ssh_executable: PathBuf,
        ssh_auth_sock: Option<PathBuf>,
        max_bundle_bytes: usize,
    ) -> Result<Self> {
        if !git_executable.is_absolute()
            || !provenant_supervisor::sys::is_executable(&git_executable)
        {
            return Err(GitTransportError::GitUnavailable);
        }
        if !crate::config::valid_executable_override(&ssh_executable)
            || !provenant_supervisor::sys::is_executable(&ssh_executable)
        {
            return Err(GitTransportError::SshUnavailable);
        }
        if max_bundle_bytes == 0 || max_bundle_bytes > MAX_BUNDLE_BYTES {
            return Err(GitTransportError::Invalid("Git bundle limit"));
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
            max_bundle_bytes,
        })
    }

    pub fn discover(executor: &'a dyn Executor, ssh_auth_sock: Option<PathBuf>) -> Result<Self> {
        Self::discover_with_policy(executor, ssh_auth_sock, None, MAX_BUNDLE_BYTES as u64)
    }

    pub fn discover_with_policy(
        executor: &'a dyn Executor,
        ssh_auth_sock: Option<PathBuf>,
        ssh_executable: Option<&Path>,
        bundle_bytes: u64,
    ) -> Result<Self> {
        if bundle_bytes == 0 || bundle_bytes > MAX_BUNDLE_BYTES as u64 {
            return Err(GitTransportError::Invalid("Git bundle limit"));
        }
        let git =
            provenant_supervisor::git::git_executable().ok_or(GitTransportError::GitUnavailable)?;
        let ssh = crate::hosts::discover_ssh_executable(ssh_executable).map_err(|_| {
            if ssh_executable.is_some() {
                GitTransportError::Invalid("SSH executable")
            } else {
                GitTransportError::SshUnavailable
            }
        })?;
        Self::new_with_policy(executor, git, ssh, ssh_auth_sock, bundle_bytes as usize)
    }

    /// Create and retain a bounded bundle containing the exact committed range
    /// `base..head`. The checkout must be clean and `head` must be its HEAD.
    pub fn create_bundle(
        &self,
        repository: &Path,
        area: &BundleArea,
        operation: &OperationId,
        base: GitOid,
        head: GitOid,
        cancel: &CancelToken,
    ) -> Result<GitBundle> {
        self.ensure_clean_worktree(repository, cancel)?;
        self.ensure_commit(repository, base, cancel)?;
        self.ensure_commit(repository, head, cancel)?;
        if !self.is_ancestor(repository, base, head, cancel)? {
            return Err(GitTransportError::NonFastForward);
        }
        if self.head(repository, cancel)? != head {
            return Err(GitTransportError::OidMismatch);
        }
        let name = bundle_name(operation);
        match area.directory.metadata_at(&name, false) {
            Ok(_) => {
                let bytes = area.read_bundle(&name, self.max_bundle_bytes)?;
                self.verify_bundle(repository, area, &name, base, head, cancel)?;
                return Ok(GitBundle {
                    operation: operation.clone(),
                    base,
                    head,
                    bytes,
                });
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        if base == head {
            return Err(GitTransportError::Invalid("empty Git transport range"));
        }
        let mut args = vec![
            OsString::from("bundle"),
            OsString::from("create"),
            OsString::from("--quiet"),
            OsString::from("-"),
        ];
        let exported = isolated_ref(operation, "export")?;
        match self.read_ref(repository, &exported, cancel)? {
            Some(existing) if existing == head => {}
            Some(_) => return Err(GitTransportError::TransportConflict),
            None => {
                self.require_success(
                    "bind bundle head",
                    repository,
                    &[
                        "update-ref".into(),
                        exported.clone().into(),
                        head.to_string().into(),
                        "0".repeat(head.to_string().len()).into(),
                    ],
                    None,
                    MAX_TEXT_BYTES,
                    cancel,
                )?;
            }
        }
        args.push(exported.into());
        args.push(format!("^{base}").into());
        let outcome = self.run_git(
            "bundle create",
            repository,
            &args,
            None,
            self.max_bundle_bytes,
            cancel,
        )?;
        if outcome.termination != Termination::Exit(0) {
            return Err(GitTransportError::GitCommand {
                command: "bundle create",
            });
        }
        if outcome.stdout.bytes.is_empty() || outcome.stdout.bytes.len() > self.max_bundle_bytes {
            return Err(GitTransportError::BundleTooLarge);
        }
        match area.directory.atomic_write(
            &name,
            ".provenant-bundle.",
            &outcome.stdout.bytes,
            PublishMode::NoReplace,
        ) {
            Ok(Publication::Durable) => {}
            Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => {
                return Err(GitTransportError::BundleDurabilityUnconfirmed);
            }
            Err(error) if error.is_exists() => {
                if area.read_bundle(&name, self.max_bundle_bytes)? != outcome.stdout.bytes {
                    return Err(GitTransportError::TransportConflict);
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.verify_bundle(repository, area, &name, base, head, cancel)?;
        let bytes = outcome.stdout.bytes;
        Ok(GitBundle {
            operation: operation.clone(),
            base,
            head,
            bytes,
        })
    }

    /// Verify and fetch a received bundle to an operation-scoped quarantine
    /// ref. Promotion to `verified-head` happens only after the exact OIDs and
    /// ancestry are checked. `writer` must be run's held checkout writer lock.
    // Keep the held custody proof explicit at this I/O boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn receive_bundle(
        &self,
        repository: &Path,
        area: &BundleArea,
        writer: &LockGuard,
        operation: &OperationId,
        base: GitOid,
        head: GitOid,
        bytes: &[u8],
        cancel: &CancelToken,
    ) -> Result<TransportedHead> {
        // LockGuard's safe acquisition verifies the persistent private lock
        // inode and retains its descriptor. The owner never unlinks these
        // lock files; Store separately binds the landing guard to its directory.
        let _held_writer = writer;
        if bytes.is_empty() || bytes.len() > self.max_bundle_bytes {
            return Err(GitTransportError::BundleTooLarge);
        }
        self.ensure_clean_worktree(repository, cancel)?;
        self.ensure_commit(repository, base, cancel)?;
        let name = bundle_name(operation);
        match area.directory.atomic_write(
            &name,
            ".provenant-bundle.",
            bytes,
            PublishMode::NoReplace,
        ) {
            Ok(Publication::Durable) => {}
            Ok(Publication::VisibleDurabilityUnconfirmed { .. }) => {
                return Err(GitTransportError::BundleDurabilityUnconfirmed);
            }
            Err(error) if error.is_exists() => {
                if area.read_bundle(&name, self.max_bundle_bytes)? != bytes {
                    return Err(GitTransportError::TransportConflict);
                }
            }
            Err(error) => return Err(error.into()),
        }
        self.verify_bundle(repository, area, &name, base, head, cancel)?;
        let incoming = isolated_ref(operation, "incoming")?;
        let verified = isolated_ref(operation, "verified-head")?;
        match self.read_ref(repository, &incoming, cancel)? {
            Some(existing) if existing == head => {}
            Some(_) => return Err(GitTransportError::TransportConflict),
            None => {
                let path = area.bundle_path(&name);
                let refspec = format!("{head}:{incoming}");
                let args = vec![
                    OsString::from("fetch"),
                    OsString::from("--no-tags"),
                    OsString::from("--no-write-fetch-head"),
                    path.into_os_string(),
                    refspec.into(),
                ];
                self.require_success(
                    "fetch bundle",
                    repository,
                    &args,
                    None,
                    MAX_TEXT_BYTES,
                    cancel,
                )?;
            }
        }
        if self.read_ref(repository, &incoming, cancel)? != Some(head) {
            return Err(GitTransportError::OidMismatch);
        }
        self.ensure_commit(repository, head, cancel)?;
        if !self.is_ancestor(repository, base, head, cancel)? {
            return Err(GitTransportError::NonFastForward);
        }
        match self.read_ref(repository, &verified, cancel)? {
            Some(existing) if existing == head => {}
            Some(_) => return Err(GitTransportError::TransportConflict),
            None => {
                let zero = "0".repeat(head.to_string().len());
                let args = vec![
                    OsString::from("update-ref"),
                    verified.clone().into(),
                    head.to_string().into(),
                    zero.into(),
                ];
                self.require_success(
                    "promote verified transport ref",
                    repository,
                    &args,
                    None,
                    MAX_TEXT_BYTES,
                    cancel,
                )?;
            }
        }
        Ok(TransportedHead {
            operation: operation.clone(),
            base,
            head,
            reference: verified,
        })
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
        if output_limit == 0 || output_limit > MAX_BUNDLE_BYTES {
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
        let mut options = ExecOptions::new(&argv, cwd, GIT_TIMEOUT);
        options.env = Some(&env);
        options.stdin = stdin;
        options.stdout = StreamPolicy::Capture {
            limit: output_limit,
        };
        options.stderr = StreamPolicy::Discard;
        options.cancel = Some(cancel);
        options.stop_on_output_limit = true;
        let outcome = self.executor.execute(options)?;
        if command == "bundle create" && outcome.overflow_stopped {
            return Err(GitTransportError::BundleTooLarge);
        }
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

    fn verify_bundle(
        &self,
        repository: &Path,
        area: &BundleArea,
        name: &OsStr,
        base: GitOid,
        head: GitOid,
        cancel: &CancelToken,
    ) -> Result<()> {
        let path = area.bundle_path(name);
        let path_arg = path.into_os_string();
        let verify = vec![
            OsString::from("bundle"),
            OsString::from("verify"),
            path_arg.clone(),
        ];
        self.require_success(
            "bundle verify",
            repository,
            &verify,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        let list = vec![
            OsString::from("bundle"),
            OsString::from("list-heads"),
            path_arg,
        ];
        let output = self.require_success(
            "bundle head listing",
            repository,
            &list,
            None,
            MAX_TEXT_BYTES,
            cancel,
        )?;
        let advertised = parse_bundle_heads(&output.stdout.bytes)?;
        if advertised.len() != 1 || advertised[0] != head {
            return Err(GitTransportError::BundleHeadMismatch);
        }
        // The base prerequisite must already exist. The head enters the object
        // database only on fetch; receive_bundle checks its ancestry afterwards.
        self.ensure_commit(repository, base, cancel)?;
        Ok(())
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

fn parse_bundle_heads(bytes: &[u8]) -> Result<Vec<GitOid>> {
    let text = std::str::from_utf8(bytes).map_err(|_| GitTransportError::InvalidOutput {
        command: "bundle head listing",
    })?;
    let mut heads = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split_ascii_whitespace();
        let oid = fields.next().ok_or(GitTransportError::InvalidOutput {
            command: "bundle head listing",
        })?;
        let reference = fields.next().ok_or(GitTransportError::InvalidOutput {
            command: "bundle head listing",
        })?;
        if fields.next().is_some() || reference.is_empty() {
            return Err(GitTransportError::InvalidOutput {
                command: "bundle head listing",
            });
        }
        heads.push(
            GitOid::parse(oid).map_err(|_| GitTransportError::InvalidOutput {
                command: "bundle head listing",
            })?,
        );
    }
    if heads.len() != 1 {
        return Err(GitTransportError::BundleHeadMismatch);
    }
    Ok(heads)
}

fn bundle_name(operation: &OperationId) -> OsString {
    format!("bundle-{}.bundle", operation.as_str()).into()
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
    use provenant_supervisor::{
        exec::{CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, SystemExecutor},
        fs::Directory,
    };
    use std::{fs, os::unix::fs::PermissionsExt};

    struct Fixture {
        root: tempfile::TempDir,
        clock: TestClock,
        cancel: CancelToken,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                clock: TestClock::new(
                    WallTime::from_micros(2_000_000_000_000_000).unwrap(),
                    MonoTime::from_micros(0),
                ),
                cancel: CancelToken::new(),
            }
        }
        fn area(&self, name: &str) -> BundleArea {
            BundleArea::open_or_create(&self.root.path().join(name)).unwrap()
        }
    }

    fn transport_with_executor(executor: &SystemExecutor) -> GitTransport<'_> {
        GitTransport::discover(executor, None).unwrap()
    }

    struct OverflowBundleExecutor {
        inner: SystemExecutor,
    }
    impl Executor for OverflowBundleExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
            if options.argv.iter().any(|arg| arg == "bundle")
                && options.argv.iter().any(|arg| arg == "create")
            {
                return Ok(ExecOutcome {
                    termination: Termination::Exit(1),
                    stdout: CapturedStream::default(),
                    stderr: CapturedStream::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: true,
                    cancelled: false,
                    elapsed_micros: 0,
                });
            }
            self.inner.execute(options)
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

    fn git(transport: &GitTransport<'_>, cwd: &Path, args: &[&str]) -> String {
        let output = transport
            .require_success(
                "test setup",
                cwd,
                &strings(args),
                None,
                MAX_TEXT_BYTES,
                &CancelToken::new(),
            )
            .unwrap();
        String::from_utf8(output.stdout.bytes)
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn committed_bundle_round_trip_is_verified_and_isolated() {
        let fixture = Fixture::new();
        let executor = SystemExecutor::new(2).unwrap();
        let transport = transport_with_executor(&executor);
        let repo = fixture.root.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(
            &transport,
            fixture.root.path(),
            &["init", "-b", "main", repo.to_str().unwrap()],
        );
        git(
            &transport,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        let base = transport.head(&repo, &fixture.cancel).unwrap();
        git(
            &transport,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "head",
            ],
        );
        let head = transport.head(&repo, &fixture.cancel).unwrap();
        let operation = OperationId::parse("lane-c-1").unwrap();
        let bundle = transport
            .create_bundle(
                &repo,
                &fixture.area("bundles"),
                &operation,
                base,
                head,
                &fixture.cancel,
            )
            .unwrap();

        let receiver = fixture.root.path().join("receiver");
        fs::create_dir(&receiver).unwrap();
        git(
            &transport,
            fixture.root.path(),
            &["init", "-b", "main", receiver.to_str().unwrap()],
        );
        // The receiver already has the base prerequisite, imported as a local bundle first.
        let base_text = base.to_string();
        let fetch_args = vec![
            OsString::from("fetch"),
            repo.as_os_str().to_owned(),
            format!("{base_text}:refs/provenant/base").into(),
        ];
        transport
            .require_success(
                "seed receiver",
                &receiver,
                &fetch_args,
                None,
                MAX_TEXT_BYTES,
                &fixture.cancel,
            )
            .unwrap();
        let writer_path = fixture.root.path().join("writer-locks");
        fs::create_dir(&writer_path).unwrap();
        fs::set_permissions(&writer_path, fs::Permissions::from_mode(0o700)).unwrap();
        let writer_dir = Directory::open_anchor(&writer_path).unwrap();
        let writer = writer_dir
            .lock(
                OsStr::new("writer.lock"),
                provenant_supervisor::fs::LockMode::Exclusive,
                WaitBudget::cli(fixture.clock.mono_now(), 1).unwrap(),
                &fixture.cancel,
                &fixture.clock,
            )
            .unwrap();
        let transported = transport
            .receive_bundle(
                &receiver,
                &fixture.area("received"),
                &writer,
                &operation,
                base,
                head,
                &bundle.bytes,
                &fixture.cancel,
            )
            .unwrap();
        assert_eq!(transported.head, head);
        assert_eq!(
            transport
                .read_ref(&receiver, &transported.reference, &fixture.cancel)
                .unwrap(),
            Some(head)
        );
    }

    #[test]
    fn bundle_output_overflow_maps_to_bundle_too_large() {
        let fixture = Fixture::new();
        let executor = SystemExecutor::new(2).unwrap();
        let setup = transport_with_executor(&executor);
        let repo = fixture.root.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(
            &setup,
            fixture.root.path(),
            &["init", "-b", "main", repo.to_str().unwrap()],
        );
        git(
            &setup,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        let base = setup.head(&repo, &fixture.cancel).unwrap();
        git(
            &setup,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "head",
            ],
        );
        let head = setup.head(&repo, &fixture.cancel).unwrap();
        let overflow = OverflowBundleExecutor {
            inner: SystemExecutor::new(2).unwrap(),
        };
        let transport = GitTransport::discover(&overflow, None).unwrap();

        assert!(matches!(
            transport.create_bundle(
                &repo,
                &fixture.area("bundles"),
                &OperationId::parse("overflow").unwrap(),
                base,
                head,
                &fixture.cancel,
            ),
            Err(GitTransportError::BundleTooLarge)
        ));
    }

    #[test]
    fn configured_bundle_byte_limit_is_enforced() {
        let fixture = Fixture::new();
        let executor = SystemExecutor::new(2).unwrap();
        let setup = transport_with_executor(&executor);
        let repo = fixture.root.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(
            &setup,
            fixture.root.path(),
            &["init", "-b", "main", repo.to_str().unwrap()],
        );
        git(
            &setup,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        let base = setup.head(&repo, &fixture.cancel).unwrap();
        git(
            &setup,
            &repo,
            &[
                "-c",
                "user.name=Fabric Test",
                "-c",
                "user.email=fabric@example.test",
                "commit",
                "--allow-empty",
                "-m",
                "head",
            ],
        );
        let head = setup.head(&repo, &fixture.cancel).unwrap();
        let transport = GitTransport::discover_with_policy(&executor, None, None, 1).unwrap();

        assert!(matches!(
            transport.create_bundle(
                &repo,
                &fixture.area("bundles"),
                &OperationId::parse("small-limit").unwrap(),
                base,
                head,
                &fixture.cancel,
            ),
            Err(GitTransportError::BundleTooLarge)
        ));
    }

    #[test]
    fn configured_ssh_executable_is_used_for_git_remotes() {
        let executor = CaptureCommandExecutor {
            argv: std::sync::Mutex::new(None),
        };
        let transport = GitTransport::discover_with_policy(
            &executor,
            None,
            Some(Path::new("/bin/sh")),
            MAX_BUNDLE_BYTES as u64,
        )
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
        let executor = SystemExecutor::new(1).unwrap();
        let transport = transport_with_executor(&executor);
        let repo = fixture.root.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(
            &transport,
            fixture.root.path(),
            &["init", "-b", "main", repo.to_str().unwrap()],
        );
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

    #[test]
    fn bundle_head_parser_refuses_multiple_or_wrong_advertised_oids() {
        let head = "a".repeat(40);
        let other = "b".repeat(40);
        assert_eq!(
            parse_bundle_heads(format!("{head} HEAD\n").as_bytes()).unwrap(),
            vec![GitOid::parse(&head).unwrap()]
        );
        assert!(matches!(
            parse_bundle_heads(format!("{head} HEAD\n{other} refs/heads/other\n").as_bytes()),
            Err(GitTransportError::BundleHeadMismatch)
        ));
    }
}

#[cfg(test)]
#[path = "landing_tests.rs"]
mod landing_tests;
