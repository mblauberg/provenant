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

pub fn capture_plan(task: &TaskRequest) -> CapturePlan {
    CapturePlan {
        wall_ms: task.provider_timeout_seconds.saturating_mul(1000),
        silence_ms: task
            .profile
            .knob_u64("lanes.silence_seconds")
            .unwrap_or(300)
            .saturating_mul(1000),
        ..CapturePlan::default()
    }
}

/// One prepared provider invocation. Secret inputs stay in memory and travel
/// on the handoff pipe, then a dedicated read pipe into the provider.
pub struct PreparedLaunch {
    /// Provider lease acquired before executable and availability probes.
    pub provider_lease: Option<provenant_os::fs::LockGuard>,
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

fn applied_write_grants(
    plan: &PreparedLaunch,
) -> std::result::Result<admission::AppliedWriteGrants, String> {
    let Some(confinement) = &plan.confinement else {
        return Ok(admission::AppliedWriteGrants::default());
    };
    let permissions = confinement.plan.permissions();
    let grants = provenant_confinement::write_grants(permissions, &confinement.profile)
        .into_iter()
        .map(|grant| {
            Ok(WriteGrantRecord {
                source: grant.source,
                path: provenant_records::AbsolutePath::new(&grant.path)
                    .map_err(|_| "invalid write grant path".to_owned())?,
                scope: grant.scope,
            })
        })
        .collect::<std::result::Result<Vec<_>, String>>()?;
    if !crate::reader::write_grants_fit(&grants) {
        return Err("write grants exceed 1,024 entries or 256 KiB".to_owned());
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
        "Durable output for this attempt: {result} (your final answer); {exit}, {stdout}, {stderr} (harness evidence). These are the only durable outputs. Put your report and evidence in your final answer; the harness writes these files. Files in $TMPDIR are discarded when the attempt ends. Provider history is kept. Files left in $TMPDIR are not deliverables.\n\n{prompt}",
        result = directory.join("result.md").display(),
        exit = directory.join("process-exit.json").display(),
        stdout = directory.join("stdout.log").display(),
        stderr = directory.join("stderr.log").display(),
    )
}

impl RunStore {
    pub(crate) fn prepare_fresh_start(
        &self,
        input: &PreparedRun,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<PreparedStart> {
        let [task] = input.request.tasks.as_slice() else {
            return Err(RunError::Validation("continuation task count"));
        };
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
            .and_then(|plan| applied_write_grants(&plan).map(|_| plan))
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
    /// Held child custody is full; the attempt stays Queued for a later scan.
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
}

/// A child's network reach may not exceed its parent's, each write root must
/// sit inside a parent write root and each read root inside any parent root.
/// The lane wrote the child's roots, so the host repeats the submission
/// edge's checks against its own filesystem view: a root counts only while it
/// is an existing directory, already canonical and clear of the state root.
fn child_access_within(
    parent: &TaskRequest,
    child: &TaskRequest,
    state_root: &std::path::Path,
) -> Result<()> {
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
    let resolve = |root: &PathBuf| {
        provenant_os::fs::canonicalize(root)
            .ok()
            .filter(|canonical| {
                canonical == root
                    && canonical.is_dir()
                    && !canonical.starts_with(&state)
                    && !state.starts_with(canonical)
            })
    };
    let writes: Vec<PathBuf> = parent
        .access
        .write_roots
        .iter()
        .filter_map(resolve)
        .collect();
    let reads: Vec<PathBuf> = parent
        .access
        .read_roots
        .iter()
        .filter_map(resolve)
        .chain(writes.iter().cloned())
        .collect();
    let within = |roots: &[PathBuf], granted: &[PathBuf]| {
        roots.iter().all(|root| {
            resolve(root).is_some_and(|root| granted.iter().any(|parent| root.starts_with(parent)))
        })
    };
    if !within(&child.access.write_roots, &writes) || !within(&child.access.read_roots, &reads) {
        return Err(RunError::Validation("child root exceeds parent access"));
    }
    Ok(())
}

#[derive(Default)]
struct ChildAttempts {
    counts: BTreeMap<TaskId, u64>,
    refusals: BTreeMap<AttemptId, &'static str>,
}

impl RunStore {
    fn child_attempt_reason(
        &self,
        run: RunId,
        request: &CanonicalRequest,
        record: &AttemptRecord,
    ) -> Option<&'static str> {
        if record.run_id != run {
            Some("child attempt run does not match")
        } else if !request
            .tasks
            .iter()
            .any(|task| task.task_id == record.task_id)
        {
            Some("child attempt task missing from request")
        } else if record.state == AttemptState::Queued
            && self
                .launched_tasks
                .borrow()
                .contains(&(run, record.task_id))
        {
            Some("child task already launched")
        } else {
            None
        }
    }
    /// Count lane-writable records, including terminal attempts. The task
    /// index is untrusted and must not hide duplicate decoded task IDs.
    fn child_attempts(
        &self,
        run: RunId,
        request: &CanonicalRequest,
        call: &Request<'_>,
    ) -> Result<ChildAttempts> {
        call.retry(|| {
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
                let id =
                    AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
                let raw: Vec<u8> = row.get(1)?;
                let record: AttemptRecord = decode(&raw)?;
                if let Some(reason) = self.child_attempt_reason(run, request, &record) {
                    attempts.refusals.insert(id, reason);
                    continue;
                }
                *attempts.counts.entry(record.task_id).or_insert(0) += 1;
            }
            Ok(attempts)
        })
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
    ) -> Result<CanonicalRequest> {
        let expected = next_lane_depth(parent_depth, maximum)?;
        let mut request = self.request(run, call)?;
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
        let attempts = self.child_attempts(run, &request, call)?;
        if let Some(reason) = attempts.refusals.values().next() {
            return Err(RunError::Validation(reason));
        }
        if attempts.counts.values().any(|count| *count > 1) {
            return Err(RunError::Validation("child task has more than one attempt"));
        }
        let parent_network = parent_task.access.network(parent_task.writer.is_some());
        for task in &mut request.tasks {
            if task.writer.is_some() || task.deliver.is_some() {
                return Err(RunError::Validation("a lane cannot propose a writer child"));
            }
            if task.access.network.is_none() && parent_network != NetworkAccess::Open {
                task.access.network = Some(parent_network);
            }
            child_access_within(parent_task, task, self.context.state_root.as_path())?;
        }
        Ok(request)
    }
    /// Refuse queued proposals without touching any launched provider.
    pub fn refuse_queued(&self, run: RunId, reason: &str, call: &Request<'_>) -> Result<()> {
        let ids: Vec<String> = call.retry(|| {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0")?;
            let rows = query.query_map(
                rusqlite::params![run.to_string(), self.context.project_id.to_string()],
                |row| row.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            if self.record(id, call)?.state == AttemptState::Queued {
                self.withdraw(
                    id,
                    AttemptState::Queued,
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
        let records: Vec<Vec<u8>> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT record FROM attempts WHERE project=?1 AND terminal=0 ORDER BY sequence LIMIT 256",
            )?;
            let rows = query.query_map([self.context.project_id.to_string()], |row| row.get(0))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let mut runs = std::collections::BTreeSet::new();
        for raw in records {
            let record: AttemptRecord = crate::db::decode(&raw)?;
            if record.state == AttemptState::Queued {
                runs.insert(record.run_id);
            }
        }
        // `create` commits the queued attempts before it seals `request.json`
        // and the prompts; the operation is only accepted once they exist.
        let mut ready = Vec::new();
        for run in runs {
            let raw: Option<Vec<u8>> = call.retry(|| {
                Ok(self
                    .db
                    .query_row(
                        "SELECT record FROM operations WHERE project=?1 AND run=?2",
                        params![self.context.project_id.to_string(), run.to_string()],
                        |row| row.get(0),
                    )
                    .optional()?)
            })?;
            let Some(raw) = raw else { continue };
            let operation: OperationRecord = decode(&raw)?;
            if operation.state != OperationState::Prepared {
                ready.push(run);
            }
        }
        Ok(ready)
    }

    pub fn request(&self, run: RunId, call: &Request<'_>) -> Result<CanonicalRequest> {
        let raw = read_sealed(&self.run_dir(run, false)?, "request.json", RECORD_LIMIT)?
            .ok_or(RunError::NotFound)?;
        let request: CanonicalRequest = decode(&raw)?;
        if request.digest()? != self.run(run, call)?.request_digest {
            return Err(RunError::ArtifactMismatch);
        }
        Ok(request)
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
            call,
        )
    }
    /// Start a child run from the request `validate_child` returned. The lane
    /// may have rewritten its store since, so a request whose digest no longer
    /// matches is refused rather than launched. For the same reason the child
    /// launches for the store's sealed requester, never the actor the child's
    /// run record names.
    pub fn start_child(
        &self,
        run: RunId,
        validated: &CanonicalRequest,
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
            Some((validated.digest()?, self.child_requester()?)),
            call,
        )
    }
    /// Native observation seam for integration fixtures. The ordinary start
    /// path always uses supervisor's system observer and nonblocking lease.
    #[cfg(feature = "fixture")]
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
        validated: Option<&CanonicalRequest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        let child = match validated {
            Some(request) => Some((request.digest()?, self.child_requester()?)),
            None => None,
        };
        self.start_inner(run, providers, context, acquire_writer, child, call)
    }
    pub(crate) fn child_requester(&self) -> Result<ActorId> {
        self.sealed_requester.ok_or(RunError::Validation(
            "child launch lacks a sealed requester",
        ))
    }
    /// `child` is a validated child request's digest and its host-held
    /// requester; a top-level run takes its requester from its own record.
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
        child: Option<(provenant_core::Digest, ActorId)>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        // Every later use reads this one value, never the store again.
        let request = self.request(run, call)?;
        let requester = match child {
            Some((digest, _)) if request.digest()? != digest => {
                return Err(RunError::Validation(
                    "child request changed after validation",
                ));
            }
            Some((_, requester)) => requester,
            None => self.run(run, call)?.actor_id,
        };
        let resume = request
            .parent
            .filter(|_| request.fresh_continuation.is_none())
            .map(|prior| self.record(prior, call))
            .transpose()?;
        let ids: Vec<String> = call.retry(|| {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE run=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
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
            if record.state != AttemptState::Queued {
                continue;
            }
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };
            let child_reason = child_reason.or_else(|| {
                if child.is_none() {
                    None
                } else if let Some(reason) = self.child_attempt_reason(run, &request, &record) {
                    Some(reason)
                } else if child_attempts
                    .as_ref()
                    .and_then(|attempts| attempts.counts.get(&record.task_id))
                    .is_some_and(|count| *count > 1)
                {
                    Some("child task has more than one attempt")
                } else {
                    None
                }
            });
            if let Some(reason) = child_reason {
                match self.withdraw(
                    id,
                    AttemptState::Queued,
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
                        AttemptState::Queued,
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
                        kind: RefusalKind::Preparation,
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
                .map_err(|reason| (RefusalKind::Preparation, reason))
                .and_then(|priority| {
                    providers
                        .check_policy(task)
                        .map(|()| priority)
                        .map_err(|reason| (RefusalKind::Policy, reason))
                })
                .and_then(|priority| {
                    providers
                        .prepare(task, &prompt, &paths, resume.as_ref())
                        .map(|plan| (plan, priority))
                        .map_err(|reason| (RefusalKind::Preparation, reason))
                });
            let (mut plan, priority) = match prepared {
                Ok(prepared) => prepared,
                Err((kind, reason)) => {
                    match self.withdraw(
                        id,
                        AttemptState::Queued,
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
                Err(reason) => {
                    match self.withdraw(
                        id,
                        AttemptState::Queued,
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
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
            };
            if !plan.skipped_routes.is_empty() {
                self.record_skipped_routes(id, &plan.skipped_routes, call)?;
            }
            // Capture errors leave the claim available to this or another launcher.
            self.seal_work(&record, &plan, call)?;
            let record = match self.mark_starting_with_priority(
                &record,
                &plan.route,
                writable,
                &write_grants,
                plan.observed_head,
                Some(priority),
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
                    call,
                ) {
                    Ok(()) => Started::Launched(id),
                    // Nothing reached a guardian: the attempt ends here.
                    Err(error @ (HandOff::Local(_) | HandOff::Busy)) => {
                        let (reason, kind) = match error {
                            HandOff::Busy => ("worktree is busy".into(), RefusalKind::WorktreeBusy),
                            HandOff::Local(error) => (
                                format!("launch preparation failed: {error}"),
                                RefusalKind::Preparation,
                            ),
                            _ => unreachable!(),
                        };
                        self.withdraw(
                            id,
                            AttemptState::Starting,
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
        call: &Request<'_>,
    ) -> std::result::Result<(), HandOff> {
        let local = |error: &dyn std::fmt::Display| HandOff::Local(error.to_string());
        let id = record.attempt_id;
        let scope = Scope {
            run: record.run_id,
            attempt: id,
            generation: record.generation,
        };
        let directory = || {
            self.attempt_dir(record.run_id, id, false)
                .map_err(|e| local(&e))
        };
        let mut holder_context = context(record, requester).map_err(|e| local(&e))?;
        let reservation =
            PublicationReservation::prepare(directory()?, call.budget, call.cancel, call.clock)
                .map_err(|e| local(&e))?;
        // Clone before acquiring custody: failure leaves no writer reservation.
        let held = if self.sealed_requester.is_some() {
            Some((
                custody::hold(
                    reservation.directory().try_clone().map_err(|e| local(&e))?,
                    scope,
                    requester,
                ),
                reservation.custody_metadata().map_err(|e| local(&e))?,
            ))
        } else {
            None
        };
        let writer = plan.confinement.as_mut().and_then(|c| c.writer.as_mut());
        if task.writer.as_ref().map(|w| w.worktree.as_path())
            != writer.as_ref().map(|w| w.worktree.path())
        {
            return Err(local(&"writer confinement does not match the task"));
        }
        let mut lease = match writer {
            Some(writer) => {
                let (directory, binding) = writer
                    .custody
                    .take()
                    .ok_or_else(|| local(&"writer custody missing"))?;
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
                local(&e)
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
            return Err(local(&error));
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
                return Err(local(&"attempt changed before handoff"));
            }
            Err(error) => {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local(&error));
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
                    return Err(local(&error));
                }
            }
        }
        if let Some(confinement) = &plan.confinement {
            if let Some(writer) = &confinement.writer
                && let Err(error) =
                    writer.snapshot(self.context.state_root.as_path(), &directory()?, scope)
            {
                lease
                    .withdraw_unlaunched()
                    .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                return Err(local(&error));
            }
            match confinement.command(&plan.argv, &id, call.cancel) {
                Ok(argv) => plan.argv = argv,
                Err(error) => {
                    lease
                        .withdraw_unlaunched()
                        .map_err(|e| HandOff::Uncertain(e.to_string()))?;
                    return Err(local(&error));
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
        let command = ProviderCommand {
            provider_lease: plan.provider_lease,
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
                .ok_or_else(|| local(&"lane priority missing"))?,
        };
        let capture = capture_plan(task);
        // Held before launch: an unconfirmed handoff may still have a guardian.
        if let Some((held, custody)) = held {
            self.hold_child(held, custody);
        }
        custody::launch(
            OwnedLaunch {
                command,
                lease: &mut lease,
                handshake: call.budget,
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
