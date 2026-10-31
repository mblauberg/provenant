//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{artifacts::read_sealed, db::decode, *};
use provenant_core::{ActorId, AttemptId, OperationId, RunId, TaskId};
use provenant_supervisor::custody::{
    self, CapturePlan, ExitObservation, OwnedLaunch, ProviderCommand, PublicationReservation, Scope,
};
use rusqlite::{OptionalExtension, params};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

/// Host-validated identity and the network policy inherited at launch.
#[derive(Clone, Debug)]
pub struct ValidatedChild {
    request: CanonicalRequest,
    parent_network: NetworkAccess,
}
impl std::ops::Deref for ValidatedChild {
    type Target = CanonicalRequest;
    fn deref(&self) -> &Self::Target {
        &self.request
    }
}
impl ValidatedChild {
    pub fn effective_task(&self, task: &TaskRequest) -> TaskRequest {
        let mut task = task.clone();
        if task.access.network.is_none() && self.parent_network != NetworkAccess::Open {
            task.access.network = Some(self.parent_network);
        }
        task
    }
    #[cfg(test)]
    pub(crate) fn fixture(request: CanonicalRequest) -> Self {
        Self {
            request,
            parent_network: NetworkAccess::Open,
        }
    }
}

/// An explicit dispatch argument wins over the effective profile default. A
/// batch with no writer defaults from `lanes.max_concurrency_read_only`
/// (16); any batch with a writer defaults from `lanes.max_concurrency` (8).
/// `bound` is a `lanes.max_concurrency` the user set while leaving the
/// read-only knob alone: it caps the read-only default, never an explicit value.
pub fn concurrency(
    explicit: Option<u64>,
    profile: &HarnessProfile,
    read_only: bool,
    bound: Option<u64>,
) -> u64 {
    explicit.unwrap_or_else(|| {
        if read_only {
            let default = profile
                .knob_u64("lanes.max_concurrency_read_only")
                .unwrap_or(16);
            bound.map_or(default, |bound| default.min(bound))
        } else {
            profile.knob_u64("lanes.max_concurrency").unwrap_or(8)
        }
    })
}

/// The wall-clock allowance of one lane: the task's timeout, tightened by the
/// provider's host cap.
pub fn runtime_seconds(task: &TaskRequest, cap: Option<u64>) -> u64 {
    cap.map_or(task.provider_timeout_seconds, |cap| {
        cap.min(task.provider_timeout_seconds)
    })
}

pub fn capture_plan(task: &TaskRequest, runtime_seconds: u64) -> CapturePlan {
    CapturePlan {
        stdout: if matches!(&task.route, crate::RequestedRoute::Adapter { provider, .. } if provider == "command")
        {
            provenant_supervisor::custody::StreamMode::Tail {
                limit: provenant_records::COMMAND_OUTPUT_TAIL_BYTES as u64,
            }
        } else {
            CapturePlan::default().stdout
        },
        wall_ms: runtime_seconds.saturating_mul(1000),
        silence_ms: task
            .profile
            .knob_u64("lanes.silence_seconds")
            .unwrap_or(1800)
            .saturating_mul(1000),
        ..CapturePlan::default()
    }
}

/// One prepared provider invocation. Secret inputs stay in memory and travel
/// on the handoff pipe, then a dedicated read pipe into the provider.
pub struct PreparedLaunch {
    pub argv: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    pub stdin: Vec<u8>,
    pub secret_input: Option<provenant_os::exec::SecretInput>,
    pub cwd: PathBuf,
    /// Exact provider/model/effort the invocation applies.
    pub route: RouteRecord,
    /// Fallback candidates that failed before this one launched.
    pub skipped_routes: Vec<SkippedRoute>,
    /// HEAD of the checkout a read-only lane runs in, read by the host at
    /// preparation. Writers record theirs from delivery instead.
    pub observed_head: Option<provenant_core::GitOid>,
    /// Canonical Git common directory verified by the host preparation edge.
    pub repository: Option<provenant_records::AbsolutePath>,
    pub confinement: Option<crate::LaunchConfinement>,
}

#[derive(Debug, thiserror::Error)]
enum WriteGrantError {
    #[error("invalid write grant path")]
    InvalidPath,
    #[error("write grants exceed 1,024 entries or 256 KiB")]
    Limit,
}

fn applied_write_grants(
    plan: &PreparedLaunch,
) -> std::result::Result<admission::AppliedWriteGrants, WriteGrantError> {
    let Some(confinement) = &plan.confinement else {
        return Ok(admission::AppliedWriteGrants::default());
    };
    let permissions = confinement.plan.permissions();
    let grants = provenant_confinement::write_grants(permissions, &confinement.profile)
        .into_iter()
        .map(|grant| {
            Ok(WriteGrantRecord {
                extra: Default::default(),
                source: grant.source,
                path: provenant_records::AbsolutePath::new(&grant.path)
                    .map_err(|_| WriteGrantError::InvalidPath)?,
                scope: grant.scope,
            })
        })
        .collect::<std::result::Result<Vec<_>, WriteGrantError>>()?;
    if !crate::reader::write_grants_fit(&grants) {
        return Err(WriteGrantError::Limit);
    }
    let git_grants_digest = match &permissions.mode {
        provenant_confinement::permissions::AccessMode::ReadOnly => None,
        provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) => Some(
            provenant_core::Digest::from_bytes(authority.git_grants_digest),
        ),
    };
    Ok(admission::AppliedWriteGrants {
        grants,
        git_grants_digest,
    })
}
/// Provider-decoded facts from a completed capture.
pub struct Decoded {
    pub outcome: Outcome,
    pub result: String,
    pub complete: bool,
    pub observed_route: Option<RouteRecord>,
    pub usage: UsageRecord,
    /// Provider session handle decoded from the stream, if any.
    pub session: Option<String>,
    pub reason: Option<String>,
    /// Partial-finish signals the provider decode found; run adds its own.
    pub partial_signals: Vec<PartialSignal>,
}
/// Run-owned per-attempt paths. Allocated once, shared by explicit fallback candidates.
pub struct LaunchPaths {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub task_index: u64,
    pub lane_depth: u64,
    pub run_id: RunId,
    pub deadline_unix_ms: u64,
    pub scratch: PathBuf,
    pub provider_state: PathBuf,
    /// Private temporary directory for a lane, never a shared system
    /// one. Terminal commit and withdrawal remove it best-effort.
    pub temporary: PathBuf,
}

/// A fresh continuation prepared before its operation and attempt are recorded.
/// Only this invocation's exclusively created, unrecorded staging is disposable.
pub struct PreparedStart {
    pub task: TaskRequest,
    pub attempt: AttemptId,
    pub plan: std::cell::RefCell<Option<PreparedLaunch>>,
    runs: provenant_os::fs::Directory,
    run: RunId,
    retained: bool,
}
impl Drop for PreparedStart {
    fn drop(&mut self) {
        if !self.retained {
            let _ = crate::artifacts::remove_entry_tree(
                &self.runs,
                std::ffi::OsStr::new(&self.run.to_string()),
            );
        }
    }
}

fn output_prompt(directory: &std::path::Path, prompt: &str) -> String {
    format!(
        "Durable output for this attempt: {result} (your final answer); {exit}, {stdout}, {stderr} (harness evidence). These are the only durable outputs. Put your report and evidence in your final answer; the harness writes these files. Keep scratch files under the provided $TMPDIR, not in the worktree. Files in $TMPDIR are discarded when the attempt ends. Provider history and handoff progress are kept. Files left in $TMPDIR are not deliverables.\n\n{handoff}\n\n{prompt}",
        handoff = crate::completion::guidance(),
        result = directory.join("result.md").display(),
        exit = directory.join("process-exit.json").display(),
        stdout = directory.join("stdout.log").display(),
        stderr = directory.join("stderr.log").display(),
    )
}

#[cfg(test)]
#[test]
fn output_prompt_directs_scratch_outside_worktree() {
    let prompt = output_prompt(std::path::Path::new("/attempt"), "task");
    assert!(prompt.contains("Keep scratch files under the provided $TMPDIR, not in the worktree."));
}

impl RunStore {
    pub fn effective_launch_task(
        &self,
        request: &CanonicalRequest,
        task: &TaskRequest,
        child: Option<&ValidatedChild>,
    ) -> TaskRequest {
        let mut task = child.map_or_else(|| task.clone(), |value| value.effective_task(task));
        if (request.parent.is_some() && request.parent_attempt.is_some())
            || (child.is_none()
                && (self.sealed_requester.is_some()
                    || self.context.state_root.as_path() != self.writer_registry()))
        {
            // A sealed contributor cannot add a capability through a rewritten
            // request. Database access needs a host-validated declaration.
            if child.is_none() {
                task.access.database_ipc = false;
            }
            let bound = self
                .sealed_parent_network
                .unwrap_or(NetworkAccess::Provider);
            let reach = |network| match network {
                NetworkAccess::Provider => 0,
                NetworkAccess::Local => 1,
                NetworkAccess::Open => 2,
            };
            if reach(task.access.network(task.writer.is_some())) > reach(bound) {
                task.access.network = Some(bound);
            }
        }
        task
    }
    pub(crate) fn prepare_fresh_start(
        &self,
        input: &PreparedRun,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<PreparedStart> {
        let [task] = input.request.tasks.as_slice() else {
            return Err(RunError::Validation("continuation task count"));
        };
        let effective = self.effective_launch_task(&input.request, task, None);
        let task = &effective;
        let [attempt] = input.attempts.as_slice() else {
            return Err(RunError::Validation("continuation attempt count"));
        };
        let runs = crate::artifacts::subdir(&self.root, std::ffi::OsStr::new("runs"), true)?;
        // Exclusive creation makes cleanup safe even if admission races a replay.
        runs.mkdir(std::ffi::OsStr::new(&input.run_id.to_string()), 0o700)?;
        let mut ready = PreparedStart {
            task: task.clone(),
            attempt: attempt.attempt_id,
            plan: std::cell::RefCell::new(None),
            runs,
            run: input.run_id,
            retained: false,
        };
        ready.runs.sync()?;
        let directory = self.attempt_dir(input.run_id, attempt.attempt_id, true)?;
        for name in ["scratch", "provider-state", "tmp"] {
            crate::artifacts::subdir(&directory, std::ffi::OsStr::new(name), true)?;
        }
        let base = self.context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}",
            input.run_id, attempt.attempt_id
        ));
        let paths = LaunchPaths {
            attempt_id: attempt.attempt_id,
            operation_id: input.operation_id.clone(),
            task_index: 0,
            lane_depth: input.request.lane_depth.unwrap_or(1),
            run_id: input.run_id,
            deadline_unix_ms: u64::try_from(
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))?
                    .as_micros()
                    / 1000,
            )
            .unwrap_or(0)
            .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
            scratch: base.join("scratch"),
            provider_state: base.join("provider-state"),
            temporary: base.join("tmp"),
        };
        let prompt = output_prompt(&base, &attempt.prompt);
        let plan = providers
            .check_policy(task)
            .and_then(|()| providers.prepare(task, &prompt, &paths, None))
            .and_then(|plan| {
                applied_write_grants(&plan)
                    .map(|_| plan)
                    .map_err(|error| error.to_string())
            })
            .map_err(|message| RunError::Input {
                kind: ErrorKind::ContinuationUnsupported,
                message: format!("fresh fallback refused before admission: {message}"),
            })?;
        *ready.plan.get_mut() = Some(plan);
        Ok(ready)
    }

    pub(crate) fn retain_prepared_start(&self, ready: &mut PreparedStart, run: RunId) {
        ready.retained = ready.run == run;
    }
}
pub trait Providers {
    /// A host cap, in seconds, on the runtime deadline of lanes on `provider`.
    /// The task's own timeout applies when it is shorter or there is no cap.
    fn runtime_cap(&self, _provider: &str) -> Option<u64> {
        None
    }
    /// Host policy can reject a result and request a bounded successor. The
    /// store preserves authority and refuses writer route fallback.
    fn assess_result(
        &self,
        _task: &TaskRequest,
        _record: &AttemptRecord,
        _decoded: &Decoded,
        _handoff: Option<&provenant_records::handoff::Handoff>,
        _context: Option<&crate::HolderContext>,
        _policy_attempt: u64,
    ) -> std::result::Result<crate::ResultAssessment, String> {
        Ok(crate::ResultAssessment::default())
    }
    /// Host-selected minimum nice value for this lane.
    fn priority(&self, task: &TaskRequest) -> std::result::Result<u8, String> {
        Ok(if task.writer.is_some() { 10 } else { 5 })
    }
    /// Host policy checks before provider preparation. A refusal is task-local;
    /// no provider is prepared and siblings remain eligible to launch.
    fn check_policy(&self, _task: &TaskRequest) -> std::result::Result<(), String> {
        Ok(())
    }
    /// Prepare one task, trying its fallback routes in order. An error is a
    /// task-local refusal reason; siblings still launch. `resume` is the prior
    /// terminal attempt of a continuation, whose exact session must be used:
    /// without one, refuse rather than start a fresh session.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, String>;
    /// A recognised diagnostic from the sealed stderr of a failed provider.
    fn failure_diagnostic(&self, _route: &RouteRecord, _stderr: &[u8]) -> Option<String> {
        None
    }
    /// Decode the sealed stdout capture of a provider that exited.
    fn decode(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
    ) -> std::result::Result<Decoded, String>;
    /// Decode with the host-sealed roots used by the guardian.
    fn decode_with_context(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
        _context: &crate::HolderContext,
    ) -> std::result::Result<Decoded, String> {
        self.decode(route, stdout, exit, provider_state)
    }
}
enum HandOff {
    /// Failed before anything was sent to a guardian.
    Local(String),
    Busy,
    /// The guardian may have received the launch.
    Uncertain(String),
}
#[derive(Debug)]
pub enum Started {
    Launched(AttemptId),
    /// Held child custody is full, or a policy retry's launch time has not
    /// come; the attempt stays Queued for a later scan.
    Deferred(AttemptId),
    Refused {
        attempt: AttemptId,
        reason: String,
        kind: RefusalKind,
    },
    /// Handoff did not confirm; the attempt stays fenced for `reconcile`.
    Unknown {
        attempt: AttemptId,
        error: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    Policy,
    Preparation,
    WorktreeBusy,
    PriorityUnavailable,
    StoredPromptUnreadable,
    InvalidWriteGrantPath,
    WriteGrantsLimit,
    Handoff,
}

/// Check a canonical root against state, allowing installed releases for reads.
pub fn access_root_allowed(root: &std::path::Path, state: &std::path::Path, read: bool) -> bool {
    let releases = state.join("releases");
    let release = root.strip_prefix(&releases).ok().is_some_and(|relative| {
        matches!(relative.components().next(), Some(std::path::Component::Normal(digest))
            if digest.to_str().is_some_and(|text| provenant_records::SourceDigest::parse(text).is_ok()))
    });
    (read && release) || (!root.starts_with(state) && !state.starts_with(root))
}

/// A child's network reach may not exceed its parent's, each write root must
/// sit inside a parent write root and each read root inside any parent root.
/// The host repeats submission checks: roots must be existing canonical
/// directories, clear of state except installed release roots granted for reads.
fn child_access_within(
    parent: &TaskRequest,
    child: &TaskRequest,
    state_root: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Result<()> {
    if child.access.database_ipc && !parent.access.database_ipc {
        return Err(RunError::Validation(
            "child database IPC exceeds parent access",
        ));
    }
    let reach = |network| match network {
        NetworkAccess::Provider => 0,
        NetworkAccess::Local => 1,
        NetworkAccess::Open => 2,
    };
    if reach(child.access.network(child.writer.is_some()))
        > reach(parent.access.network(parent.writer.is_some()))
    {
        return Err(RunError::Validation("child network exceeds parent access"));
    }
    let state =
        provenant_os::fs::canonicalize(state_root).unwrap_or_else(|_| state_root.to_path_buf());
    let resolve = |root: &PathBuf, read: bool| {
        provenant_os::fs::canonicalize(root)
            .ok()
            .filter(|canonical| {
                canonical == root
                    && canonical.is_dir()
                    && access_root_allowed(canonical, &state, read)
            })
    };
    for (root, read) in child
        .access
        .read_roots
        .iter()
        .map(|root| (root, true))
        .chain(child.access.write_roots.iter().map(|root| (root, false)))
    {
        // Apply the same subtree rule as the confinement compiler.
        if provenant_os::fs::canonicalize(root)
            .ok()
            .is_some_and(|canonical| {
                provenant_confinement::refuses_lane_root(&canonical, read, home)
            })
        {
            return Err(RunError::Confinement(
                provenant_confinement::permissions::PermissionError::CredentialRoot,
            ));
        }
    }
    let writes: Vec<PathBuf> = parent
        .access
        .write_roots
        .iter()
        .filter_map(|root| resolve(root, false))
        .collect();
    let reads: Vec<PathBuf> = parent
        .access
        .read_roots
        .iter()
        .filter_map(|root| resolve(root, true))
        .chain(writes.iter().cloned())
        .collect();
    let within = |roots: &[PathBuf], granted: &[PathBuf], read| {
        roots.iter().all(|root| {
            resolve(root, read)
                .is_some_and(|root| granted.iter().any(|parent| root.starts_with(parent)))
        })
    };
    if !within(&child.access.write_roots, &writes, false)
        || !within(&child.access.read_roots, &reads, true)
    {
        return Err(RunError::Validation("child root exceeds parent access"));
    }
    Ok(())
}

#[cfg(test)]
mod release_root_tests {
    use super::*;
    use std::path::Path;

    /// A read root in a Codex standalone release passes the shared entry
    /// exception; a write root there stays refused.
    #[test]
    fn child_allows_codex_standalone_read_roots()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        // The rule is lexical, so check it without the filesystem first.
        let shape = std::path::Path::new(
            "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin/bin",
        );
        assert!(
            !provenant_confinement::refuses_lane_root(shape, true, None),
            "reads pass entry"
        );
        assert!(
            provenant_confinement::refuses_lane_root(shape, false, None),
            "writes stay strict"
        );
        // Against a machine's real home: build a temp release and walk it
        // through `child_access_within`. A lane can seal `~/.codex`; then the
        // fixture cannot exist and the filesystem part is skipped silently.
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return Ok(());
        };
        let releases = home.join(".codex/packages/standalone/releases");
        if std::fs::create_dir_all(&releases).is_err() {
            return Ok(());
        }
        let Ok(temp) = tempfile::tempdir_in(&releases) else {
            return Ok(());
        };
        let release = std::fs::canonicalize(temp.path())?;
        let state_dir = tempfile::tempdir()?;
        let state = std::fs::canonicalize(state_dir.path())?.join("state");
        let mut parent = crate::tests::input().request.tasks.remove(0);
        parent.access.read_roots = vec![release.clone()];
        let mut child = parent.clone();
        child.access.read_roots = vec![release.clone()];
        child.access.write_roots = vec![];
        assert!(
            child_access_within(&parent, &child, &state, Some(&home)).is_ok(),
            "reads in a standalone release pass entry"
        );
        child.access.write_roots = vec![release.clone()];
        child.access.read_roots = vec![];
        let error = child_access_within(&parent, &child, &state, Some(&home)).unwrap_err();
        assert!(error.to_string().contains(
            &provenant_confinement::permissions::PermissionError::CredentialRoot.to_string()
        ));
        Ok(())
    }

    #[test]
    fn child_refuses_credential_and_enclosing_roots()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let credential = temp.path().join(".ssh").join("nested");
        std::fs::create_dir_all(&credential)?;
        let mut roots = vec![std::fs::canonicalize(credential)?];
        // The home itself is an enclosing root; use the host's real directory
        // so the check sees a conventional home.
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
            && home.is_dir()
        {
            roots.push(home);
        }
        assert!(roots.len() >= 2);
        for root in roots {
            let mut parent = crate::tests::input().request.tasks.remove(0);
            parent.access.read_roots = vec![root.clone()];
            parent.access.write_roots = vec![root.clone()];
            for read in [true, false] {
                let mut child = parent.clone();
                child.access.read_roots = if read { vec![root.clone()] } else { vec![] };
                child.access.write_roots = if read { vec![] } else { vec![root.clone()] };
                let error = child_access_within(
                    &parent,
                    &child,
                    &temp.path().join("state"),
                    Some(Path::new("/fixture/host-home")),
                )
                .unwrap_err();
                assert!(matches!(
                    error,
                    RunError::Confinement(
                        provenant_confinement::permissions::PermissionError::CredentialRoot
                    )
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn child_root_table_matches_confinement() {
        let standalone =
            "/Users/alice/.codex/packages/standalone/releases/0.163.0-aarch64-apple-darwin";
        let home = Path::new("/fixture/host-home");
        for (path, read_allowed, write_allowed) in [
            ("/Users/alice/.ssh", false, false),
            ("/Users/alice/.config", false, false),
            (standalone, true, false),
            ("/Users/alice/project", true, true),
        ] {
            for (flag, read, allowed) in [
                ("--read-root", true, read_allowed),
                ("--write-root", false, write_allowed),
            ] {
                assert_eq!(
                    !provenant_confinement::refuses_lane_root(Path::new(path), read, Some(home)),
                    allowed,
                    "{flag} {path}"
                );
            }
        }
        // An injected nonconventional home is refused, matching compile.
        let home = Path::new("/srv/team/alice");
        for (path, read_allowed, write_allowed) in [
            ("/srv/team", false, false),
            ("/srv/team/alice", false, false),
            ("/srv/team/alice/.config", false, false),
            ("/srv/team/alice/project", true, true),
        ] {
            for (flag, read, allowed) in [
                ("--read-root", true, read_allowed),
                ("--write-root", false, write_allowed),
            ] {
                assert_eq!(
                    !provenant_confinement::refuses_lane_root(Path::new(path), read, Some(home)),
                    allowed,
                    "{flag} {path}"
                );
            }
        }
    }

    #[test]
    fn child_refuses_a_root_enclosing_an_injected_nonconventional_home()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let team = temp.path().join("srv/team");
        let home = team.join("alice");
        let project = home.join("project");
        let config = home.join(".config");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&config)?;
        let team = std::fs::canonicalize(team)?;
        let home = std::fs::canonicalize(home)?;
        let project = std::fs::canonicalize(project)?;
        let config = std::fs::canonicalize(config)?;
        let state = temp.path().join("state");
        for root in [&team, &home, &config] {
            for read in [true, false] {
                let mut parent = crate::tests::input().request.tasks.remove(0);
                parent.access.read_roots = vec![root.clone()];
                parent.access.write_roots = vec![root.clone()];
                let mut child = parent.clone();
                child.access.read_roots = if read { vec![root.clone()] } else { vec![] };
                child.access.write_roots = if read { vec![] } else { vec![root.clone()] };
                let error = child_access_within(&parent, &child, &state, Some(&home)).unwrap_err();
                assert!(matches!(
                    error,
                    RunError::Confinement(
                        provenant_confinement::permissions::PermissionError::CredentialRoot
                    )
                ));
            }
        }
        let mut parent = crate::tests::input().request.tasks.remove(0);
        parent.access.read_roots = vec![project.clone()];
        parent.access.write_roots = vec![project.clone()];
        let mut child = parent.clone();
        child.access.read_roots = vec![project.clone()];
        child.access.write_roots = vec![project];
        assert!(child_access_within(&parent, &child, &state, Some(&home)).is_ok());
        Ok(())
    }

    #[test]
    fn child_inherits_only_release_read_roots()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = std::fs::canonicalize(temp.path())?.join("state");
        let release = state
            .join("releases")
            .join(provenant_records::SourceDigest::from_bytes([1; 32]).to_string());
        let skills = release.join("skills");
        std::fs::create_dir_all(&skills)?;
        std::fs::create_dir_all(state.join("projects"))?;
        let mut parent = crate::tests::input().request.tasks.remove(0);
        parent.access.read_roots = vec![release.clone()];
        let mut child = parent.clone();
        child.access.read_roots = vec![skills.clone()];
        assert!(
            child_access_within(
                &parent,
                &child,
                &state,
                Some(Path::new("/fixture/host-home"))
            )
            .is_ok()
        );
        let other = state
            .join("releases")
            .join(provenant_records::SourceDigest::from_bytes([2; 32]).to_string());
        std::fs::create_dir_all(&other)?;
        child.access.read_roots = vec![other];
        assert!(
            child_access_within(
                &parent,
                &child,
                &state,
                Some(Path::new("/fixture/host-home"))
            )
            .is_err()
        );
        for root in [
            state.join("releases"),
            state.join("projects"),
            state.clone(),
            state.join("releases/not-a-digest/skills"),
            state.join("releases/.provenant-staging.fixture/skills"),
        ] {
            std::fs::create_dir_all(&root)?;
            parent.access.read_roots = vec![root.clone()];
            child.access.read_roots = vec![root];
            assert!(
                child_access_within(
                    &parent,
                    &child,
                    &state,
                    Some(Path::new("/fixture/host-home"))
                )
                .is_err()
            );
        }
        parent.access.write_roots = vec![release];
        child.access.read_roots.clear();
        child.access.write_roots = vec![skills];
        assert!(
            child_access_within(
                &parent,
                &child,
                &state,
                Some(Path::new("/fixture/host-home"))
            )
            .is_err()
        );
        Ok(())
    }
}

#[derive(Default)]
struct ChildAttempts {
    counts: BTreeMap<TaskId, u64>,
    refusals: BTreeMap<AttemptId, &'static str>,
}

impl RunStore {
    /// Authenticate a lane-local continuation source before inheriting host
    /// associations. A legacy request cannot prove attempt membership.
    pub fn check_child_attempt_id(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
    ) -> Result<()> {
        if request.child_attempt_id_version.is_none() {
            return Err(RunError::Validation("legacy child attempt identity"));
        }
        if let Some(reason) = self.child_attempt_reason(record.run_id, request, record)? {
            return Err(RunError::Validation(reason));
        }
        Ok(())
    }

    fn child_attempt_reason(
        &self,
        run: RunId,
        request: &CanonicalRequest,
        record: &AttemptRecord,
    ) -> Result<Option<&'static str>> {
        self.check_child_run_id(run, request, &record.operation_id)?;
        Ok(if record.run_id != run {
            Some("child attempt run does not match")
        } else if crate::correlation::child_attempt_id(
            self.context.project_id,
            run,
            request,
            record.task_id,
        )?
        .is_some_and(|id| id != record.attempt_id)
        {
            Some("child attempt ID does not match host derivation")
        } else if !request
            .tasks
            .iter()
            .any(|task| task.task_id == record.task_id)
        {
            Some("child attempt task missing from request")
        } else if matches!(record.state, AttemptState::Queued { .. })
            && self
                .launched_tasks
                .borrow()
                .contains(&(run, record.task_id))
        {
            Some("child task already launched")
        } else {
            None
        })
    }
    /// Count lane-writable records, including terminal attempts. The task
    /// index is untrusted and must not hide duplicate decoded task IDs.
    fn child_attempts(
        &self,
        run: RunId,
        request: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<ChildAttempts> {
        call.check()?;
        let mut query = self
            .db
            .prepare("SELECT id,record FROM attempts WHERE run=?1 AND project=?2")?;
        let mut rows = query.query(params![
            run.to_string(),
            self.context.project_id.to_string()
        ])?;
        let mut attempts = ChildAttempts::default();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let raw: Vec<u8> = row.get(1)?;
            let record: AttemptRecord = decode(&raw)?;
            if let Some(reason) = self.child_attempt_reason(run, request, &record)? {
                attempts.refusals.insert(id, reason);
                continue;
            }
            *attempts.counts.entry(record.task_id).or_insert(0) += 1;
        }
        Ok(attempts)
    }
    /// Validate lane-writable ingress against the host's recorded parent and
    /// its sealed task. A child never gains authority its parent lacks, and a
    /// lane cannot propose a writer or delivering child: the host has no
    /// sealed list of worktrees a lane may delegate, and a writer child in any
    /// registered worktree could plant code that later runs unconfined. Access
    /// must sit within the parent's. A child cannot continue an attempt, whose
    /// retained state the host would copy. The lane can rewrite its store at
    /// any time, so launch only the request returned here.
    pub fn validate_child(
        &self,
        run: RunId,
        parent: AttemptId,
        parent_task: &TaskRequest,
        parent_depth: u64,
        maximum: u64,
        call: &Request<'_>,
    ) -> Result<ValidatedChild> {
        let expected = next_lane_depth(parent_depth, maximum)?;
        let request = self.request(run, call)?;
        if request.parent_attempt != Some(parent) || request.lane_depth != Some(expected) {
            return Err(RunError::Validation(
                "child request does not match host parent record",
            ));
        }
        if request.parent.is_some() {
            return Err(RunError::Validation(
                "a child run cannot continue an attempt",
            ));
        }
        self.check_child_run_id(run, &request, &self.run(run, call)?.operation_id)?;
        let attempts = self.child_attempts(run, &request, call)?;
        if let Some(reason) = attempts.refusals.values().next() {
            return Err(RunError::Validation(reason));
        }
        if attempts.counts.values().any(|count| *count > 1) {
            return Err(RunError::Validation("child task has more than one attempt"));
        }
        let parent_network = parent_task.access.network(parent_task.writer.is_some());
        let validated = ValidatedChild {
            request,
            parent_network,
        };
        for task in &validated.tasks {
            if task.writer.is_some() || task.deliver.is_some() {
                return Err(RunError::Validation("a lane cannot propose a writer child"));
            }
            child_access_within(
                parent_task,
                &validated.effective_task(task),
                self.context.state_root.as_path(),
                self.host_home.as_deref(),
            )?;
        }
        Ok(validated)
    }
    /// Refuse queued proposals without touching any launched provider.
    pub fn refuse_queued(&self, run: RunId, reason: &str, call: &Request<'_>) -> Result<()> {
        let ids: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0")?;
            let rows = query.query_map(
                rusqlite::params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            if matches!(self.record(id, call)?.state, AttemptState::Queued { .. }) {
                self.withdraw(
                    id,
                    AttemptState::Queued {
                        extra: Default::default(),
                    },
                    Outcome::Rejected,
                    reason,
                    call.clock
                        .wall_now()
                        .map_err(|_| RunError::Validation("wall clock"))?,
                    call,
                )?;
            }
        }
        Ok(())
    }
    /// Bounded ingress for the parent host's child launcher. No provider or
    /// custody action occurs in the submitting lane.
    pub fn queued_runs(&self, call: &Request<'_>) -> Result<Vec<RunId>> {
        call.check()?;
        let records: Vec<Vec<u8>> = {
            let mut query = self.db.prepare(
                "SELECT record FROM attempts WHERE project=?1 AND terminal=0 ORDER BY sequence LIMIT 256",
            )?;
            let rows = query.query_map([self.context.project_id.to_string()], |row| row.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut runs = std::collections::BTreeSet::new();
        for raw in records {
            let record: AttemptRecord = crate::db::decode(&raw)?;
            if matches!(record.state, AttemptState::Queued { .. }) {
                runs.insert(record.run_id);
            }
        }
        // `create` commits the queued attempts before it seals `request.json`
        // and the prompts; the operation is only accepted once they exist.
        let mut ready = Vec::new();
        for run in runs {
            let raw: Option<Vec<u8>> = {
                self.db
                    .query_row(
                        "SELECT record FROM operations WHERE project=?1 AND run=?2",
                        params![self.context.project_id.to_string(), run.to_string()],
                        |row| row.get(0),
                    )
                    .optional()?
            };
            let Some(raw) = raw else { continue };
            let operation: OperationRecord = decode(&raw)?;
            if operation.state != OperationState::Prepared {
                ready.push(run);
            }
        }
        Ok(ready)
    }

    pub fn request(&self, run: RunId, call: &Request<'_>) -> Result<CanonicalRequest> {
        Ok(self.request_with_digest(run, call)?.0)
    }
    /// The request and the digest of its stored bytes. Callers compare this
    /// digest, never a reserialised request, because its schema may have
    /// evolved since the run was written.
    pub(crate) fn request_with_digest(
        &self,
        run: RunId,
        call: &Request<'_>,
    ) -> Result<(CanonicalRequest, provenant_core::Digest)> {
        let raw = read_sealed(&self.run_dir(run, false)?, "request.json", RECORD_LIMIT)?
            .ok_or(RunError::NotFound)?;
        let digest = provenant_records::stored_record_digest("run-request", 1, &raw)?;
        let stored = self.run(run, call)?.request_digest;
        if digest != stored {
            return Err(RunError::artifact_mismatch(
                ArtifactCheck::RequestDigest,
                "request.json",
                Some(ArtifactValue::digest(stored)),
                Some(ArtifactValue::digest(digest)),
            ));
        }
        Ok((decode(&raw)?, digest))
    }
    /// Hand each queued attempt of `run` to its own guardian, which admits it
    /// and later commits its terminal record. `context` builds the opaque
    /// holder reference for one attempt and the requester it launches for.
    /// Only the host writes a top-level store, so its run record names the
    /// requester.
    pub fn start(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            run,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            None,
            None,
            call,
        )
    }
    pub fn start_checked(
        &self,
        run: RunId,
        expected: &CanonicalRequest,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            run,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            None,
            Some(expected.digest()?),
            call,
        )
    }
    /// Start a child run from the request `validate_child` returned. The lane
    /// may have rewritten its store since, so a request whose digest no longer
    /// matches is refused rather than launched. For the same reason the child
    /// launches for the store's sealed requester, never the actor the child's
    /// run record names. Versioned child IDs must match the host derivation;
    /// legacy attempts without a version remain unindexed.
    pub fn start_child(
        &self,
        run: RunId,
        validated: &ValidatedChild,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            run,
            providers,
            context,
            &|directory, scope, binding| {
                custody::acquire_writer(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            Some((validated, self.child_requester()?)),
            None,
            call,
        )
    }
    /// Native observation seam for integration fixtures. The ordinary start
    /// path always uses supervisor's system observer and nonblocking lease.
    #[cfg(feature = "fixture")]
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_writer_lease(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        validated: Option<&ValidatedChild>,
        expected: Option<&CanonicalRequest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        let child = match validated {
            Some(request) => Some((request, self.child_requester()?)),
            None => None,
        };
        self.start_inner(
            run,
            providers,
            context,
            acquire_writer,
            child,
            expected.map(CanonicalRequest::digest).transpose()?,
            call,
        )
    }
    pub(crate) fn child_requester(&self) -> Result<ActorId> {
        self.sealed_requester.ok_or(RunError::Validation(
            "child launch lacks a sealed requester",
        ))
    }
    /// `child` is a validated child request's digest and its host-held
    /// requester; a top-level run takes its requester from its own record.
    #[allow(clippy::too_many_arguments)]
    fn start_inner(
        &self,
        run: RunId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        child: Option<(&ValidatedChild, ActorId)>,
        expected: Option<provenant_core::Digest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        // Every later use reads this one value, never the store again.
        let request = self.request(run, call)?;
        if let Some(digest) = expected
            && request.digest()? != digest
        {
            return Err(RunError::Validation(
                "request changed after launch validation",
            ));
        }
        let requester = match child {
            Some((validated, _)) if request.digest()? != validated.digest()? => {
                return Err(RunError::Validation(
                    "child request changed after validation",
                ));
            }
            Some((_, requester)) => {
                self.check_child_run_id(run, &request, &self.run(run, call)?.operation_id)?;
                requester
            }
            None => self.run(run, call)?.actor_id,
        };
        if self.sealed_parent_network.is_none()
            && ((request.parent.is_some() && request.parent_attempt.is_some())
                || (child.is_none()
                    && (self.sealed_requester.is_some()
                        || self.context.state_root.as_path() != self.writer_registry())))
        {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: parent network unavailable; child-store launch uses the same provider-only profile as other provider-only lanes (macOS egress limitation: #2118)"
            );
        }
        let resume = request
            .parent
            .filter(|_| request.fresh_continuation.is_none())
            .map(|prior| self.record(prior, call))
            .transpose()?;
        let ids: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut started = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            // The lane may have added or rewritten rows since validation.
            // Treat bad ingress as a refusal, so the scan can finish.
            let child_attempts = child
                .map(|_| self.child_attempts(run, &request, call))
                .transpose()?;
            let child_reason = child_attempts
                .as_ref()
                .and_then(|attempts| attempts.refusals.get(&id))
                .copied();
            let record = match self.record(id, call) {
                Ok(record) => record,
                Err(RunError::Validation(reason)) if child.is_some() => {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: child_reason.unwrap_or(reason).into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                Err(error) => return Err(error),
            };
            if !matches!(record.state, AttemptState::Queued { .. }) {
                continue;
            }
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };
            let child_reason = if child_reason.is_some() {
                child_reason
            } else if child.is_none() {
                None
            } else if let Some(reason) = self.child_attempt_reason(run, &request, &record)? {
                Some(reason)
            } else if child_attempts
                .as_ref()
                .and_then(|attempts| attempts.counts.get(&record.task_id))
                .is_some_and(|count| *count > 1)
            {
                Some("child task has more than one attempt")
            } else {
                None
            };
            if let Some(reason) = child_reason {
                match self.withdraw(
                    id,
                    AttemptState::Queued {
                        extra: Default::default(),
                    },
                    Outcome::Rejected,
                    reason,
                    now()?,
                    call,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(RunError::Conflict) => continue,
                    Err(error) => return Err(error),
                }
                started.push(Started::Refused {
                    attempt: id,
                    reason: reason.into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            if child.is_some()
                && let Some(parent) = request.parent_attempt
            {
                self.contribution_diagnostic(
                    record.run_id,
                    self.capture_child_contribution(parent, &record, call),
                );
            }
            // At the cap the attempt stays Queued: a terminal refusal would spend
            // the task's only child attempt. Later attempts meet the same cap.
            if self.sealed_requester.is_some() && self.check_held_capacity(id).is_err() {
                started.push(Started::Deferred(id));
                break;
            }
            let (task_index, task) = request
                .tasks
                .iter()
                .enumerate()
                .find(|(_, task)| task.task_id == record.task_id)
                .ok_or(RunError::Validation("attempt task missing from request"))?;
            let mut effective_task =
                self.effective_launch_task(&request, task, child.map(|(validated, _)| validated));
            let policy_retry = matches!(
                record.continuation.as_deref(),
                Some("policy_continuation" | "policy_fallback")
            );
            if policy_retry {
                if self.sealed_requester.is_some()
                    || request.parent_attempt.is_some()
                    || self.context.state_root.as_path() != self.writer_registry()
                {
                    return Err(RunError::Validation(
                        "policy retry requires a host-owned store",
                    ));
                }
                let prior = record
                    .retry_of
                    .map(|prior| self.record(prior, call))
                    .transpose()?;
                // Every launcher, the settling guardian or a later sweep, waits
                // for the retry's sealed launch time.
                let not_before = match &prior {
                    Some(prior) => self
                        .staged_policy_retry(prior)?
                        .and_then(|retry| retry.not_before_unix_seconds),
                    None => None,
                };
                if not_before.is_some_and(|not_before| {
                    not_before > now().map_or(i64::MAX, |now| now.as_micros().div_euclid(1_000_000))
                }) {
                    started.push(Started::Deferred(id));
                    continue;
                }
                if record.continuation.as_deref() == Some("policy_fallback")
                    && !retry_keeps_account_pin(
                        &record.requested_route,
                        &task.route,
                        prior
                            .and_then(|prior| prior.applied_route)
                            .and_then(|route| route.provider)
                            .as_deref(),
                    )
                {
                    effective_task.account = None;
                }
                effective_task.route = record.requested_route.clone();
                effective_task.fallback.clear();
            }
            let policy_resume = if record.continuation.as_deref() == Some("policy_continuation") {
                record
                    .retry_of
                    .map(|prior| self.record(prior, call))
                    .transpose()?
            } else {
                None
            };
            let task = &effective_task;
            let task_index = u64::try_from(task_index)
                .map_err(|_| RunError::Validation("attempt task index"))?;
            let prompt = match self
                .artifact_bytes(run, &record.prompt, RECORD_LIMIT)
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| RunError::Validation("prompt is not UTF-8"))
                }) {
                Ok(prompt) => prompt,
                Err(error) => {
                    // A prompt the host cannot read back ends the attempt here;
                    // leaving it Queued would meet the same unreadable input on
                    // every later start.
                    let reason = format!("stored prompt unreadable: {error}");
                    match self.withdraw(
                        id,
                        AttemptState::Queued {
                            extra: Default::default(),
                        },
                        Outcome::Rejected,
                        &reason,
                        now()?,
                        call,
                    ) {
                        Ok(Some(_)) => {}
                        Ok(None) | Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::StoredPromptUnreadable,
                    });
                    continue;
                }
            };
            let directory = self
                .context
                .state_root
                .as_path()
                .join(format!("runs/{run}/attempts/{id}"));
            let prompt = output_prompt(&directory, &prompt);
            let attempt = self.attempt_dir(record.run_id, id, false)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("scratch"), true)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("provider-state"), true)?;
            crate::artifacts::subdir(&attempt, std::ffi::OsStr::new("tmp"), true)?;
            let base = self
                .context
                .state_root
                .as_path()
                .join("runs")
                .join(record.run_id.to_string())
                .join("attempts")
                .join(id.to_string());
            let paths = LaunchPaths {
                attempt_id: id,
                operation_id: record.operation_id.clone(),
                task_index,
                lane_depth: request.lane_depth.unwrap_or(1),
                run_id: record.run_id,
                deadline_unix_ms: u64::try_from(now()?.as_micros() / 1000)
                    .unwrap_or(0)
                    .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
                scratch: base.join("scratch"),
                provider_state: base.join("provider-state"),
                temporary: base.join("tmp"),
            };
            let prepared = providers
                .priority(task)
                .map_err(|reason| (RefusalKind::PriorityUnavailable, reason))
                .and_then(|priority| {
                    providers
                        .check_policy(task)
                        .map(|()| priority)
                        .map_err(|reason| (RefusalKind::Policy, reason))
                })
                .and_then(|priority| {
                    providers
                        .prepare(
                            task,
                            &prompt,
                            &paths,
                            policy_resume.as_ref().or(if policy_retry {
                                None
                            } else {
                                resume.as_ref()
                            }),
                        )
                        .map(|plan| (plan, priority))
                        .map_err(|reason| (RefusalKind::Preparation, reason))
                });
            let (mut plan, priority) = match prepared {
                Ok(prepared) => prepared,
                Err((kind, reason)) => {
                    match self.withdraw(
                        id,
                        AttemptState::Queued {
                            extra: Default::default(),
                        },
                        Outcome::Rejected,
                        &reason,
                        now()?,
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt)?,
                        Ok(None) => continue,
                        // Another launcher or cancellation changed the attempt
                        // while provider preparation was in progress.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind,
                    });
                    continue;
                }
            };
            let runtime = runtime_seconds(
                task,
                providers.runtime_cap(plan.route.provider.as_deref().unwrap_or_default()),
            );
            set_lane_environment(&mut plan.environment, request.lane_depth.unwrap_or(1), id);
            // Record the exact project-relative exceptions of the plan that launches.
            let writable = plan
                .confinement
                .as_ref()
                .and_then(|c| c.plan.permissions().read_only.as_ref())
                .map(|g| g.writable_paths())
                .unwrap_or_default();
            let write_grants = applied_write_grants(&plan);
            let write_grants = match write_grants {
                Ok(grants) => grants,
                Err(error) => {
                    let kind = match error {
                        WriteGrantError::InvalidPath => RefusalKind::InvalidWriteGrantPath,
                        WriteGrantError::Limit => RefusalKind::WriteGrantsLimit,
                    };
                    let reason = error.to_string();
                    match self.withdraw(
                        id,
                        AttemptState::Queued {
                            extra: Default::default(),
                        },
                        Outcome::Rejected,
                        &reason,
                        now()?,
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt)?,
                        Ok(None) => continue,
                        // Another launcher or cancellation changed the attempt
                        // while write grants were being recorded.
                        Err(RunError::Conflict) => continue,
                        Err(error) => return Err(error),
                    }
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind,
                    });
                    continue;
                }
            };
            if !plan.skipped_routes.is_empty() {
                self.record_skipped_routes(id, &plan.skipped_routes, call)?;
            }
            // Capture errors leave the claim available to this or another launcher.
            self.seal_work(&record, &plan, child.map(|_| &request), call)?;
            let record = match self.mark_starting_with_priority(
                &record,
                &plan.route,
                writable,
                &write_grants,
                plan.observed_head,
                Some(priority),
                runtime,
                call,
            ) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            // Consume the host's launch claim before invoking any handoff
            // callback. Keep it for every outcome, including an uncertain
            // launch: lane-owned rows cannot authorise another attempt.
            if child.is_some() && !self.launched_tasks.borrow_mut().insert((run, task.task_id)) {
                continue;
            }
            started.push(
                match self.hand_off(
                    task,
                    plan,
                    requester,
                    context,
                    acquire_writer,
                    &record,
                    runtime,
                    call,
                ) {
                    Ok(()) => Started::Launched(id),
                    // Nothing reached a guardian: the attempt ends here.
                    Err(error @ (HandOff::Local(_) | HandOff::Busy)) => {
                        let (reason, kind) = match error {
                            HandOff::Busy => ("worktree is busy".into(), RefusalKind::WorktreeBusy),
                            HandOff::Local(error) => (
                                format!("launch preparation failed: {error}"),
                                RefusalKind::Handoff,
                            ),
                            _ => unreachable!(),
                        };
                        self.withdraw(
                            id,
                            AttemptState::Starting {
                                extra: Default::default(),
                            },
                            Outcome::Rejected,
                            &reason,
                            now()?,
                            call,
                        )?;
                        Started::Refused {
                            attempt: id,
                            reason,
                            kind,
                        }
                    }
                    Err(HandOff::Uncertain(error)) => {
                        self.launch_unknown(run, call)?;
                        Started::Unknown { attempt: id, error }
                    }
                },
            );
        }
        Ok(started)
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "the requester is resolved once per run, not per attempt"
    )]
    fn hand_off(
        &self,
        task: &TaskRequest,
        mut plan: PreparedLaunch,
        requester: ActorId,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        record: &AttemptRecord,
        runtime_seconds: u64,
        call: &Request<'_>,
    ) -> std::result::Result<(), HandOff> {
        // Each failure names its phase, so the refusal says what was not ready.
        let local = |phase: &str, error: &dyn std::fmt::Display| {
            HandOff::Local(format!("{phase}: {error}"))
        };
        let id = record.attempt_id;
        let scope = Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        };
        let directory = || {
            self.attempt_dir(record.run_id, id, false)
                .map_err(|e| local("attempt directory", &e))
        };
        let mut holder_context =
            context(record, requester).map_err(|e| local("launch context", &e))?;
        let reservation = PublicationReservation::prepare(
            directory()?,
            call.lock_budget(),
            call.cancel,
            call.clock,
        )
        .map_err(|e| local("publication reservation", &e))?;
        // Clone before acquiring custody: failure leaves no writer reservation.
        let held = if self.sealed_requester.is_some() {
            Some((
                custody::hold(
                    reservation
                        .directory()
                        .try_clone()
                        .map_err(|e| local("publication reservation", &e))?,
                    scope,
                    requester,
                ),
                reservation
                    .custody_metadata()
                    .map_err(|e| local("publication reservation", &e))?,
            ))
        } else {
            None
        };
        let writer = plan.confinement.as_mut().and_then(|c| c.writer.as_mut());
        if task.writer.as_ref().map(|w| w.worktree.as_path())
            != writer.as_ref().map(|w| w.worktree.path())
        {
            return Err(local("writer confinement", &"does not match the task"));
        }
        let mut lease = match writer {
            Some(writer) => {
                let (directory, binding) = writer
                    .custody
                    .take()
                    .ok_or_else(|| local("writer custody", &"missing"))?;
                acquire_writer(directory, scope, binding)
            }
            // Readers take the same injected acquisition as writers, so a
            // fixture guardian and its reservation share one boot observer.
            None => acquire_writer(directory()?, scope, custody::WriterBinding::read_only()),
        }
        .map_err(|e| {
            if matches!(e, custody::CustodyError::Busy) {
                HandOff::Busy
            } else {
                local("custody", &e)
            }
        })?;
        if record.retry_of.is_some()
            && let Some(writer) = plan
                .confinement
                .as_ref()
                .and_then(|plan| plan.writer.as_ref())
            && let Err(error) = self.check_writer_lineage(record, &writer.worktree, call)
        {
            lease
                .withdraw_unlaunched()
                .map_err(|error| HandOff::Uncertain(error.to_string()))?;
            return Err(local("writer lineage", &error));
        }
        // Reconcile may have withdrawn this attempt before the lease existed.
        match self.record(id, call) {
            Ok(now)
                if now.revision == record.revision
                    && now.run_id == record.run_id
                    && now.task_id == record.task_id => {}
            Ok(_) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("attempt", &"changed before handoff"));
            }
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("attempt record", &error));
            }
        }
        if let Some(deliver) = &task.deliver {
            let prepare = || -> Result<Vec<u8>> {
                let mut holder: HolderContext = decode(&holder_context)?;
                let writer = task
                    .writer
                    .as_ref()
                    .ok_or(RunError::Validation("delivery requires a writer"))?;
                let host = holder
                    .host_delivery
                    .take()
                    .ok_or(RunError::Validation("delivery lacks host context"))?;
                holder.host_delivery = Some(host.prepare(writer, deliver)?);
                holder.encode()
            };
            match prepare() {
                Ok(context) => holder_context = context,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("delivery", &error));
                }
            }
        }
        // Keep the shared config lock until custody confirms the child launch.
        let mut _git_setup_lock = None;
        if let Some(confinement) = &mut plan.confinement {
            _git_setup_lock = match confinement.prepare_git() {
                Ok(lock) => lock,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("Git setup", &error));
                }
            };
            let writer_attempt = directory()?;
            if let Some(writer) = &confinement.writer
                && let Err(error) = crate::objects::prepare_bound(
                    &provenant_os::git::git_executable()
                        .ok_or_else(|| local("Git", &RunError::GitUnavailable))?,
                    writer.worktree.repo(),
                    &writer_attempt,
                    &confinement.plan.permissions().scratch.path,
                    writer.branch.as_str(),
                    writer.base,
                )
                .and_then(|()| {
                    writer.snapshot(self.context.state_root.as_path(), &writer_attempt, scope)
                })
            {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("lane Git objects", &error));
            }
            match confinement.command(
                &plan.argv,
                &plan.environment,
                &id,
                call.cancel,
                &directory()?,
            ) {
                Ok(argv) => plan.argv = argv,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local("confinement command", &error));
                }
            }
        }
        // Route records name models as `adapter/model`; prefix only a
        // provider-native one (which may itself contain `/`).
        let provider = plan.route.provider.as_deref().unwrap_or_default();
        let model = plan.route.model.as_deref().unwrap_or_default();
        let route = match model.strip_prefix(provider) {
            Some(rest) if rest.starts_with('/') => model.to_owned(),
            _ => format!("{provider}/{model}"),
        };
        let lane_ports = match plan
            .confinement
            .as_ref()
            .filter(|c| c.writer.is_some())
            .map(|c| lane_port_plan(&c.state_root))
            .transpose()
        {
            Ok(plan) => plan.flatten(),
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local("lane ports", &error));
            }
        };
        let command = ProviderCommand {
            lane_ports,
            resolved_route: route,
            permission_plan_digest: task.authority_digest,
            task: task.task_id,
            requester,
            protected_commands: vec![],
            // OpenCode stores answering-model evidence in its shared XDG data
            // directory. The guardian otherwise starts with an empty environment
            // and no Registry home; retain the absolute path selected at launch.
            config_environment: if provider == "opencode" {
                vec!["XDG_DATA_HOME".into()]
            } else {
                vec![]
            },
            argv: plan.argv,
            environment: plan.environment,
            cwd: plan.cwd,
            stdin: plan.stdin,
            secret_input: plan.secret_input,
            holder_context,
            priority: record
                .requested_priority
                .ok_or_else(|| local("lane priority", &"missing"))?,
        };
        let capture = capture_plan(task, runtime_seconds);
        // Held before launch: an unconfirmed handoff may still have a guardian.
        if let Some((held, custody)) = held {
            self.hold_child(held, custody);
        }
        custody::launch(
            OwnedLaunch {
                command,
                lease: &mut lease,
                handshake: call.handshake_budget(),
                cancel: call.cancel,
                clock: call.clock,
            },
            capture,
            reservation,
        )
        .map_err(|failure| HandOff::Uncertain(failure.error.to_string()))?;
        Ok(())
    }
    fn launch_unknown(&self, run: RunId, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row(
                "SELECT record FROM operations WHERE run=?1 AND project=?2",
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            let mut op: OperationRecord = decode(&raw)?;
            if op.state == OperationState::Terminal {
                return Ok(());
            }
            op.state = OperationState::LaunchUnknown;
            tx.execute(
                "UPDATE operations SET record=?1 WHERE run=?2",
                params![crate::db::bytes(&op)?, run.to_string()],
            )?;
            Ok(())
        })
    }
}

fn set_lane_environment(
    environment: &mut Vec<(OsString, OsString)>,
    depth: u64,
    parent: AttemptId,
) {
    set_environment_value(environment, LANE_DEPTH_ENV, depth.to_string());
    set_environment_value(environment, PARENT_ATTEMPT_ENV, parent.to_string());
}

fn set_environment_value(environment: &mut Vec<(OsString, OsString)>, name: &str, value: String) {
    let name = OsString::from(name);
    environment.retain(|(existing, _)| existing != &name);
    environment.push((name, OsString::from(value)));
}

/// An account pin belongs to the provider it was granted with. A redraw of the
/// granted route keeps it, whatever that route resolves to; a fallback to
/// another provider resolves a fresh account there.
fn retry_keeps_account_pin(
    retry: &RequestedRoute,
    granted: &RequestedRoute,
    original_provider: Option<&str>,
) -> bool {
    retry == granted
        || match retry {
            RequestedRoute::Model { provider, .. } => original_provider == Some(provider.as_str()),
            _ => original_provider.is_none(),
        }
}

/// The configured lane port space, or none when allocation is switched off.
/// Writer lanes only: read-only lanes run no servers.
fn lane_port_plan(
    state: &std::path::Path,
) -> std::io::Result<Option<provenant_os::lane_ports::Plan>> {
    let settings = provenant_config::LaneBuildSettings::read(state)?;
    let blocks = settings.lane_port_blocks();
    if blocks == 0 {
        return Ok(None);
    }
    let narrow = |value: u64| u32::try_from(value).map_err(std::io::Error::other);
    Ok(Some(provenant_os::lane_ports::Plan {
        state: state.to_owned(),
        base: narrow(settings.lane_port_base_value())?,
        size: narrow(settings.lane_port_block_size())?,
        blocks: narrow(blocks)?,
    }))
}

#[cfg(test)]
mod lane_environment_tests {
    use super::*;

    #[test]
    fn lane_environment_overrides_adapter_values() {
        let parent = AttemptId::from_bytes([6; 16]);
        let mut environment = vec![
            (OsString::from(LANE_DEPTH_ENV), OsString::from("99")),
            (OsString::from("OTHER"), OsString::from("kept")),
        ];
        set_lane_environment(&mut environment, 2, parent);
        assert_eq!(
            environment,
            vec![
                (OsString::from("OTHER"), OsString::from("kept")),
                (OsString::from(LANE_DEPTH_ENV), OsString::from("2")),
                (
                    OsString::from(PARENT_ATTEMPT_ENV),
                    OsString::from(parent.to_string())
                ),
            ]
        );
    }
}

#[cfg(test)]
mod retry_pin_tests {
    use super::*;

    #[test]
    fn redraws_of_a_pinned_granted_route_keep_the_pin() {
        let named = RequestedRoute::Named {
            extra: Default::default(),
            name: "implement".into(),
            effort: None,
        };
        let pool = RequestedRoute::Review {
            extra: Default::default(),
            name: "review".into(),
            effort: None,
            excluded_families: Vec::new(),
            distinct_families: false,
            require_distinct: false,
        };
        let model = |provider: &str| RequestedRoute::Model {
            extra: Default::default(),
            provider: provider.into(),
            model: format!("{provider}/model"),
            effort: None,
        };
        // A sessionless capacity redraw of a pinned named or pool route stays
        // on the pinned account, even after the first draw named a provider.
        for granted in [&named, &pool] {
            assert!(retry_keeps_account_pin(granted, granted, Some("codex")));
            assert!(retry_keeps_account_pin(granted, granted, None));
        }
        // The applied model on the same provider keeps it too.
        assert!(retry_keeps_account_pin(
            &model("codex"),
            &named,
            Some("codex")
        ));
        // A fallback to another provider resolves a fresh account there.
        assert!(!retry_keeps_account_pin(
            &model("claude"),
            &named,
            Some("codex")
        ));
        assert!(!retry_keeps_account_pin(&pool, &named, Some("codex")));
    }
}
