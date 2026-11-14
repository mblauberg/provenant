//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{artifacts::read_sealed, db::decode, *};
use provenant_core::{ActorId, AttemptId, OperationId};
use provenant_supervisor::custody::{
    self, CapturePlan, ExitObservation, OwnedLaunch, ProviderCommand, PublicationReservation, Scope,
};
use rusqlite::params;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

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

pub fn capture_plan(task: &TaskRequest, limits: &provenant_records::LaneLimits) -> CapturePlan {
    let enforced = |limit: &Option<provenant_records::Limit>| {
        limit
            .as_ref()
            .filter(|limit| limit.enforced())
            .map_or(0, |limit| limit.value)
    };
    CapturePlan {
        stdout: if matches!(&task.route, crate::RequestedRoute::Adapter { provider, .. } if provider == "command")
        {
            provenant_supervisor::custody::StreamMode::Tail {
                limit: provenant_records::COMMAND_OUTPUT_TAIL_BYTES as u64,
            }
        } else {
            CapturePlan::default().stdout
        },
        wall_ms: enforced(&limits.deadline_seconds).saturating_mul(1000),
        // Minimal supervision (the `light` preset) marks silence at the
        // longest interval the registry allows; it still never kills for it.
        silence_ms: if task
            .profile
            .knobs
            .get("lanes.supervision")
            .and_then(serde_json::Value::as_str)
            == Some("minimal")
        {
            3_600_000
        } else {
            task.profile
                .knob_u64("lanes.silence_seconds")
                .unwrap_or(1800)
                .saturating_mul(1000)
        },
        token_budget: enforced(&limits.token_budget),
        budget_action: limits.budget_policy.unwrap_or_default().action,
        // Writers only: a fresh attempt continues a writer's retained
        // worktree from its handoff. 0 or unset is off; a provider that
        // reports no per-call context figure is never stopped for it.
        context_handoff: enforced(&limits.context_handoff),
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
    /// The lane's limits, resolved once at preparation. Admission records
    /// them; launch, enforcement and the preamble read nothing else.
    pub limits: provenant_records::LaneLimits,
}

impl PreparedLaunch {
    /// Resolve backend availability once, before recording the applied route.
    /// The provider's already-prepared environment stays at least as narrow.
    fn bind_sandbox_backend(
        &mut self,
        cancel: &provenant_core::CancelToken,
        policy: &provenant_records::policy::Policy,
    ) -> Result<()> {
        use provenant_confinement::run_config::{SANDBOX_ENV, SandboxRestriction};
        use std::io::Write as _;
        let Some(confinement) = self.confinement.as_mut().filter(|c| c.sandbox.effective) else {
            return Ok(());
        };
        let permissions = confinement.plan.permissions();
        let optional = permissions
            .policy
            .as_ref()
            .map(|p| p.optional())
            .cloned()
            .unwrap_or_default();
        let restrictions = SandboxRestriction::for_policy(permissions.network, &optional);
        let availability = confinement.bind_backend(cancel, policy)?;
        confinement.sandbox.resolve_backend(
            availability,
            &restrictions,
            provenant_confinement::run_config::current_process_is_sandboxed(),
        );
        let mode = &confinement.sandbox;
        if let Some(warning) = &mode.warning {
            let _ = writeln!(std::io::stderr().lock(), "warning: {warning}");
        }
        self.environment.retain(|(name, _)| name != SANDBOX_ENV);
        self.environment
            .push((SANDBOX_ENV.into(), mode.env_value().into()));
        self.route.extra.insert(
            provenant_records::run::SANDBOX_ROUTE_FIELD.into(),
            serde_json::to_value(mode)
                .map_err(|_| RunError::Validation("invalid resolved sandbox mode"))?,
        );
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
enum WriteGrantError {
    #[error("invalid write grant path")]
    InvalidPath,
    #[error("write-grant threshold selected strict policy")]
    Policy(provenant_records::diagnostic::ResourceFact),
}

#[allow(clippy::print_stderr)]
fn applied_write_grants(
    plan: &PreparedLaunch,
    limit: WriteGrantLimit,
    policy: &provenant_records::policy::Policy,
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
                path: provenant_records::AbsolutePath::new(&grant.path).map_err(|_| {
                    policy.boundary_error(
                        "LAUNCH_APPLIED_WRITE_GRANTS_AUTHORITY",
                        provenant_records::policy::Facts::violation(
                            "applied write grant path is not canonical and absolute",
                        ),
                        WriteGrantError::InvalidPath,
                    )
                })?,
                scope: grant.scope,
            })
        })
        .collect::<std::result::Result<Vec<_>, WriteGrantError>>()?;
    let encoded = provenant_records::CanonicalV1::bytes(&grants).map_err(|_| {
        policy.boundary_error(
            "LAUNCH_APPLIED_WRITE_GRANTS_AUTHORITY",
            provenant_records::policy::Facts::violation(
                "applied write grants cannot be encoded canonically",
            ),
            WriteGrantError::InvalidPath,
        )
    })?;
    let effect = policy.check(
        "WRITE_GRANT_BYTES",
        provenant_records::policy::Facts {
            violated: encoded.as_bytes().len() as u64 > limit.bytes,
            detail: format!(
                "encoded write grants {} bytes, threshold={}, threshold source={}",
                encoded.as_bytes().len(),
                limit.bytes,
                limit.origin.description()
            ),
        },
    );
    if effect.blocks() {
        return Err(WriteGrantError::Policy(
            provenant_records::diagnostic::ResourceFact::WriteGrantLimit {
                limit: limit.bytes,
                origin: limit.origin,
            },
        ));
    }
    if let Some(message) = effect.message() {
        eprintln!("warning: {message}");
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
    pub terminal_cause: Option<TerminalCause>,
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
    /// Where the provider ended each turn in `result`; empty when it does not
    /// say. Settlement reads the handoff through these boundaries.
    pub turns: Vec<provenant_records::handoff::TurnEnd>,
}
impl Decoded {
    /// The lane's final handoff: the one rule for every provider.
    pub fn handoff(
        &self,
    ) -> std::result::Result<Option<provenant_records::handoff::Handoff>, &'static str> {
        provenant_records::handoff::Handoff::from_turns(&self.result, &self.turns)
    }
}
/// A task-local preparation refusal, retaining candidate classifications.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PrepareRefusal {
    /// Boundary established by the original typed preparation failure, if any.
    pub boundary_class: provenant_records::diagnostic::BoundaryClass,
    /// Stable table row for the predicate that refused preparation.
    pub policy_code: Option<String>,
    /// A configurable resource threshold that refused this launch, if known.
    pub resource: Option<provenant_records::diagnostic::ResourceFact>,
    /// The specific cause. There is no generic refusal.
    pub terminal_cause: TerminalCause,
    /// Fixed-form text naming a configured value, never captured output.
    pub terminal_note: Option<String>,
    pub message: String,
    pub skipped_routes: Vec<SkippedRoute>,
    pub fallback_outcome: Option<provenant_records::run::PreparationFailure>,
}
/// A refusal for test fixtures that do not care about its cause.
#[cfg(test)]
pub(crate) fn fixture_refusal(message: impl Into<String>) -> Box<PrepareRefusal> {
    PrepareRefusal::boxed(TerminalCause::HandoffRefused, message)
}

impl PrepareRefusal {
    pub fn boxed(cause: TerminalCause, message: impl Into<String>) -> Box<Self> {
        Box::new(Self {
            boundary_class: provenant_records::diagnostic::BoundaryClass::None,
            policy_code: None,
            resource: None,
            terminal_cause: cause,
            terminal_note: None,
            message: message.into(),
            skipped_routes: Vec::new(),
            fallback_outcome: None,
        })
    }

    fn kind(&self) -> RefusalKind {
        if let Some(route) = self.skipped_routes.iter().find(|route| {
            route.cause == Some(provenant_records::RouteExclusionReason::PacingReserve)
        }) {
            match route.extra.get("guard_resource").and_then(|value| {
                serde_json::from_value::<provenant_records::diagnostic::ResourceFact>(value.clone())
                    .ok()
            }) {
                Some(fact @ provenant_records::diagnostic::ResourceFact::PacingReserve { .. }) => {
                    RefusalKind::PacingReserveResource(fact)
                }
                _ => RefusalKind::PacingReserve,
            }
        } else if let Some(fact) = self.resource {
            RefusalKind::PreparationResource(fact)
        } else if self.boundary_class != provenant_records::diagnostic::BoundaryClass::None {
            RefusalKind::PreparationBoundary(self.boundary_class)
        } else {
            RefusalKind::Preparation
        }
    }

    pub fn auth_required(&self) -> bool {
        !self.skipped_routes.is_empty()
            && self
                .skipped_routes
                .iter()
                .all(|route| route.kind == SkipKind::AuthRequired)
    }
}
#[cfg(test)]
#[test]
fn pacing_refusal_retains_the_selected_reserve_fact() {
    use provenant_records::diagnostic::{PacingReserveSetting, PacingSettingSource, ResourceFact};
    let fact = ResourceFact::PacingReserve {
        percent: 15,
        setting: PacingReserveSetting::AdapterFiveHour,
        origin: PacingSettingSource::ShippedDefault,
    };
    let mut skipped = SkippedRoute {
        extra: Default::default(),
        route: "fixture/model".into(),
        kind: SkipKind::Unavailable,
        cause: Some(provenant_records::RouteExclusionReason::PacingReserve),
        detail: "opaque source".into(),
    };
    skipped.set_guard_facts(None, Some(fact)).unwrap();
    let mut refusal = PrepareRefusal::boxed(TerminalCause::RouteResolutionRefused, "opaque source");
    refusal.skipped_routes.push(skipped);
    let kind = refusal.kind();
    assert!(matches!(kind, RefusalKind::PacingReserveResource(value) if value == fact));
    let diagnostic = kind.diagnostic().unwrap();
    assert_eq!(diagnostic.code, "REFUSED");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("domains.routing.pacing_reserve"));
    assert!(detail.contains("shipped default"));
    assert!(!detail.contains("opaque source"));
}

/// Run-owned per-attempt paths. Allocated once, shared by explicit fallback candidates.
pub struct LaunchPaths {
    pub attempt_id: AttemptId,
    pub operation_id: OperationId,
    pub task_index: u64,
    pub lane_depth: u64,
    pub deadline_unix_ms: u64,
    pub scratch: PathBuf,
    pub provider_state: PathBuf,
    /// Private temporary directory for a lane, never a shared system
    /// one. Terminal commit and withdrawal remove it best-effort.
    pub temporary: PathBuf,
    /// Short symlink to `temporary` on macOS, for a `TMPDIR` that fits
    /// `sun_path`. Absent when unavailable; grants always use `temporary`.
    pub temporary_alias: Option<PathBuf>,
}

/// A fresh continuation prepared before its operation and attempt are recorded.
/// Only this invocation's exclusively created, unrecorded staging is disposable.
pub struct PreparedStart {
    pub task: TaskRequest,
    pub attempt: AttemptId,
    pub plan: std::cell::RefCell<Option<PreparedLaunch>>,
    attempts: provenant_os::fs::Directory,
    retained: bool,
}
impl Drop for PreparedStart {
    fn drop(&mut self) {
        if !self.retained {
            // No attempt record survives for retention to find the alias by,
            // so unlink it before its attempt directory is deleted.
            let attempt = crate::artifacts::subdir(
                &self.attempts,
                std::ffi::OsStr::new(&self.attempt.to_string()),
                false,
            );
            if let Ok(attempt) = attempt {
                crate::short_temporary::remove(&attempt);
            }
            let _ = crate::artifacts::remove_entry_tree(
                &self.attempts,
                std::ffi::OsStr::new(&self.attempt.to_string()),
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
    assert!(prompt.contains("$TMPDIR"));
    assert!(prompt.contains("not in the worktree"));
}

/// Resolve policy once for this launch from host, project and dispatch settings.
pub fn launch_policy(
    state: &Path,
    project: &str,
    task: &TaskRequest,
) -> provenant_records::policy::Policy {
    provenant_config::policy::for_task(state, project, task)
}

fn check_launch_policy(
    providers: &dyn Providers,
    task: &TaskRequest,
    policy: &provenant_records::policy::Policy,
) -> std::result::Result<(), Box<PrepareRefusal>> {
    use provenant_records::policy::{Effect, Facts};
    let Err(refusal) = providers.check_policy(task) else {
        return Ok(());
    };
    // DiskFloor is emitted only by the shared critical-space decision.
    // Advisory pressure has already warned and continued at that core.
    if refusal.terminal_cause == TerminalCause::DiskFloor {
        return Err(refusal);
    }
    let owned = refusal.policy_code.clone();
    let code = if let Some(code) = owned.as_deref() {
        code
    } else {
        match refusal.boundary_class {
            provenant_records::diagnostic::BoundaryClass::Credentials => {
                "PREPARATION_PREPARE_WITH_CREDENTIALS"
            }
            provenant_records::diagnostic::BoundaryClass::LaneGit => {
                "VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"
            }
            provenant_records::diagnostic::BoundaryClass::Integrity => {
                "PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"
            }
            provenant_records::diagnostic::BoundaryClass::Authority => {
                "LAUNCH_RUNSTORE_START_INNER_AUTHORITY"
            }
            provenant_records::diagnostic::BoundaryClass::None
                if matches!(
                    refusal.terminal_cause,
                    TerminalCause::DiskObservationUnavailable
                ) =>
            {
                "SUBMIT_CHECK_WRITER_DISK_WITH_COLLECTION_NONE"
            }
            // A generic refusal with no boundary is not evidence of disk pressure.
            // Keep its typed outcome instead of letting a disk knob authorize it.
            provenant_records::diagnostic::BoundaryClass::None => return Err(refusal),
        }
    };
    if refusal.boundary_class != provenant_records::diagnostic::BoundaryClass::None {
        return Err(policy.boundary_error(code, Facts::violation(&refusal.message), refusal));
    }
    match policy.check(code, Facts::violation(&refusal.message)) {
        Effect::Proceed => Ok(()),
        Effect::Warn(message) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            Ok(())
        }
        Effect::Refuse(_) => Err(refusal),
    }
}

fn check_preparation_refusal(
    policy: &provenant_records::policy::Policy,
    refusal: Box<PrepareRefusal>,
) -> Box<PrepareRefusal> {
    use provenant_records::diagnostic::BoundaryClass;
    let class = refusal.boundary_class;
    if class == BoundaryClass::None {
        return refusal;
    }
    let fallback = match class {
        BoundaryClass::Credentials => Some("PREPARATION_PREPARE_WITH_CREDENTIALS"),
        BoundaryClass::LaneGit => Some("VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION"),
        BoundaryClass::Integrity => Some("PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY"),
        BoundaryClass::Authority => Some("PREPARATION_PREPARE_CANDIDATES_AUTHORITY"),
        BoundaryClass::None => None,
    };
    let owned = refusal.policy_code.clone();
    let Some(code) = owned.as_deref().or(fallback) else {
        return refusal;
    };
    policy.boundary_error(
        code,
        provenant_records::policy::Facts::violation(&refusal.message),
        refusal,
    )
}

impl RunStore {
    /// Persist final plan facts for the launcher that already claimed
    /// Queued -> Starting before any external admission or provider work.
    #[allow(clippy::too_many_arguments)]
    fn claim_launch(
        &self,
        expected: &AttemptRecord,
        plan: &PreparedLaunch,
        grants: &admission::AppliedWriteGrants,
        priority: u8,
        limits: &provenant_records::LaneLimits,
        call: &Request<'_>,
        output: &mut impl std::io::Write,
    ) -> Result<AttemptRecord> {
        let writable = plan
            .confinement
            .as_ref()
            .and_then(|confinement| confinement.plan.permissions().read_only.as_ref())
            .map(|grants| grants.writable_paths())
            .unwrap_or_default();
        let record = self.record_starting_launch(
            expected,
            &plan.route,
            writable,
            grants,
            plan.observed_head,
            priority,
            limits,
            call,
        )?;
        if let Some(policy) = plan
            .confinement
            .as_ref()
            .and_then(|confinement| confinement.plan.permissions().policy.as_ref())
        {
            let _ = policy.report_launch(output);
        }
        Ok(record)
    }

    pub fn effective_launch_task(
        &self,
        request: &CanonicalRequest,
        task: &TaskRequest,
    ) -> TaskRequest {
        let mut task = task.clone();
        if (request.parent.is_some() && request.parent_attempt.is_some())
            || self.context.state_root.as_path() != self.writer_registry()
        {
            // A sealed contributor cannot add a capability through a rewritten
            // request. Database access needs a host-validated declaration.
            task.access.database_ipc = false;
            let bound = self.sealed_parent_network.unwrap_or(NetworkAccess::Open);
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
    /// Seal the parent's user-enabled sandbox evidence for the shared
    /// admission-floor policy. It is a fact, not an authority boundary; warn
    /// or strict is decided from the request's sealed knob at preparation.
    pub(crate) fn seal_parent_sandbox(
        &self,
        request: &CanonicalRequest,
        task: &mut TaskRequest,
        call: &Request<'_>,
    ) {
        if let Some(parent) = request.parent_attempt
            && self.attempt_floors_children(parent, call)
        {
            task.extra.insert(
                provenant_records::run::PARENT_SANDBOXED_FIELD.into(),
                true.into(),
            );
        }
    }
    pub(crate) fn prepare_fresh_start(
        &self,
        input: &PreparedRun,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<PreparedStart> {
        let mut effective = self.effective_launch_task(&input.request, &input.request.task);
        self.seal_parent_sandbox(&input.request, &mut effective, call);
        let task = &effective;
        let attempts =
            crate::artifacts::subdir(&self.root, std::ffi::OsStr::new("attempts"), true)?;
        // Exclusive creation makes cleanup safe even if admission races a replay.
        attempts.mkdir(std::ffi::OsStr::new(&input.attempt_id.to_string()), 0o700)?;
        let mut ready = PreparedStart {
            task: task.clone(),
            attempt: input.attempt_id,
            plan: std::cell::RefCell::new(None),
            attempts,
            retained: false,
        };
        ready.attempts.sync()?;
        let directory = self.attempt_dir(input.attempt_id, true)?;
        for name in ["scratch", "provider-state", "tmp"] {
            crate::artifacts::subdir(&directory, std::ffi::OsStr::new(name), true)?;
        }
        let base = self
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}", input.attempt_id));
        let paths = LaunchPaths {
            attempt_id: input.attempt_id,
            operation_id: input.operation_id.clone(),
            task_index: 0,
            lane_depth: input.request.lane_depth.unwrap_or(1),
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
            temporary_alias: crate::short_temporary::ensure(&directory),
        };
        let prompt = output_prompt(&base, &input.prompt);
        let policy = launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            task,
        );
        let plan = check_launch_policy(providers, task, &policy)
            .map_err(|error| RunError::ContinuationRefused {
                cause: error.terminal_cause,
            })
            .and_then(|()| {
                providers
                    .prepare(task, &prompt, &paths, None)
                    .map_err(|error| RunError::ContinuationRefused {
                        cause: error.terminal_cause,
                    })
            })
            .and_then(|mut plan| {
                let limit = providers.write_grant_limit(&policy).checked(&policy);
                applied_write_grants(&plan, limit, &policy).map_err(|error| RunError::Input {
                    kind: ErrorKind::ContinuationUnsupported,
                    message: error.to_string(),
                })?;
                plan.route
                    .extra
                    .insert("write_grant_limit".into(), limit.receipt());
                Ok(plan)
            })?;
        *ready.plan.get_mut() = Some(plan);
        Ok(ready)
    }

    pub(crate) fn retain_prepared_start(&self, ready: &mut PreparedStart, attempt: AttemptId) {
        ready.retained = ready.attempt == attempt;
    }
}
/// The host-selected cap and its source, carried into each applied receipt.
#[derive(Clone, Copy, Debug)]
pub struct WriteGrantLimit {
    pub bytes: u64,
    pub origin: provenant_records::diagnostic::SettingSource,
}
impl Default for WriteGrantLimit {
    fn default() -> Self {
        Self {
            bytes: provenant_records::run::MAX_WRITE_GRANT_BYTES,
            origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        }
    }
}
impl WriteGrantLimit {
    fn checked(self, policy: &provenant_records::policy::Policy) -> Self {
        if (1..=provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING).contains(&self.bytes) {
            self
        } else {
            if let provenant_records::policy::Effect::Warn(message) = policy.check(
                "WRITE_GRANT_LIMIT_INVALID_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "domains.run.write_grant_limit_bytes={} has source {}; using compiled default",
                    self.bytes,
                    self.origin.description()
                )),
            ) {
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            }
            Self::default()
        }
    }
    fn receipt(self) -> serde_json::Value {
        serde_json::json!({"key": "domains.run.write_grant_limit_bytes", "value": self.bytes, "origin": match self.origin { provenant_records::diagnostic::SettingSource::Configuration => "host configuration", provenant_records::diagnostic::SettingSource::ShippedDefault => "compiled default" }})
    }
}

pub trait Providers {
    /// Host cap for the encoded applied write-grant ledger.
    fn write_grant_limit(&self, _policy: &provenant_records::policy::Policy) -> WriteGrantLimit {
        WriteGrantLimit::default()
    }
    /// Host-selected minimum nice value for this lane.
    fn priority(&self, task: &TaskRequest) -> std::result::Result<u8, String> {
        Ok(provenant_records::run::default_lane_priority(
            task.writer.is_some(),
            task.build_heavy,
        ))
    }
    /// Host policy checks before provider preparation. A refusal is task-local;
    /// no provider is prepared and siblings remain eligible to launch.
    fn check_policy(&self, _task: &TaskRequest) -> std::result::Result<(), Box<PrepareRefusal>> {
        Ok(())
    }
    /// Prepare one task. An error is a
    /// task-local refusal reason; siblings still launch. `resume` is the prior
    /// terminal attempt of a continuation, whose exact session must be used:
    /// without one, refuse rather than start a fresh session.
    fn prepare(
        &self,
        task: &TaskRequest,
        prompt: &str,
        paths: &LaunchPaths,
        resume: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>>;
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
    /// Decode the sealed captures: the single production entry point. `stderr`
    /// is extra evidence for providers that report limits there, and `context`
    /// carries the host-sealed roots used by the guardian.
    fn decode_captures(
        &self,
        route: &RouteRecord,
        stdout: &[u8],
        _stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &std::path::Path,
        _context: Option<&crate::HolderContext>,
    ) -> std::result::Result<Decoded, String> {
        self.decode(route, stdout, exit, provider_state)
    }
}
enum HandOff {
    /// Failed before anything was sent to a guardian.
    Local(String),
    /// A typed boundary refused the prepared handoff.
    Boundary {
        message: String,
        class: provenant_records::diagnostic::BoundaryClass,
        cause: Option<TerminalCause>,
    },
    Resource {
        message: String,
        fact: provenant_records::diagnostic::ResourceFact,
    },
    TypedLocal {
        message: String,
        cause: TerminalCause,
    },
    Busy,
    /// No guardian was contacted, but custody remains protected.
    Protected(String),
    RetainedImport {
        fence: Box<provenant_supervisor::custody::PendingImport>,
        worktree: std::path::PathBuf,
    },
    /// The guardian may have received the launch.
    Uncertain(String),
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum Started {
    Launched(AttemptId),
    Refused {
        attempt: AttemptId,
        reason: String,
        kind: RefusalKind,
    },
    /// Repository admission was unavailable; release proof controls retry eligibility.
    Skipped {
        attempt: AttemptId,
        reason: String,
    },
    /// Handoff did not confirm; the attempt stays fenced for `reconcile`.
    Unknown {
        attempt: AttemptId,
        error: String,
    },
    /// Preparation failed and custody must be recovered before retrying.
    Blocked {
        attempt: AttemptId,
        error: String,
    },
}

pub(crate) const ADMISSION_PORT_BINDING_FIELD: &str =
    provenant_records::run::REPOSITORY_ADMISSION_BINDING_FIELD;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AdmissionPhase {
    AcquirePending,
    Acquired,
    ReleasePending,
    Released,
}

impl AdmissionPhase {
    fn command(self) -> Option<&'static str> {
        match self {
            Self::AcquirePending => Some("acquire"),
            Self::ReleasePending => Some("release"),
            Self::Acquired | Self::Released => None,
        }
    }
}

/// The command, source and lifecycle phase stay frozen with the attempt across
/// config rotation and recovery. Repository commands key their idempotent
/// acquire/release operations by attempt ID and lease epoch; release fences late
/// acquire work for that epoch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AdmissionPortBinding {
    pub(crate) source: provenant_config::PortSource,
    pub(crate) command: provenant_config::PortCommand,
    pub(crate) command_digest: provenant_records::FileDigest,
    pub(crate) lease_epoch: u64,
    pub(crate) phase: AdmissionPhase,
}

impl AdmissionPortBinding {
    fn new(source: provenant_config::PortSource, command: &provenant_config::PortCommand) -> Self {
        Self {
            source,
            command: command.clone(),
            command_digest: command.digest(),
            lease_epoch: 1,
            phase: AdmissionPhase::AcquirePending,
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.lease_epoch != 0 && self.command_digest == self.command.digest()
    }

    pub(crate) fn can_transition_to(&self, next: AdmissionPhase, active_owner: bool) -> bool {
        matches!(
            (self.phase, next),
            (AdmissionPhase::AcquirePending, AdmissionPhase::Acquired)
                | (
                    AdmissionPhase::AcquirePending,
                    AdmissionPhase::ReleasePending
                )
                | (AdmissionPhase::Acquired, AdmissionPhase::ReleasePending)
                | (AdmissionPhase::ReleasePending, AdmissionPhase::Released)
        ) || (active_owner
            && self.phase == AdmissionPhase::Released
            && next == AdmissionPhase::AcquirePending)
    }
}

pub(crate) fn mark_admission_release_pending(record: &mut AttemptRecord) -> Result<()> {
    let Some(value) = record.extra.get(ADMISSION_PORT_BINDING_FIELD).cloned() else {
        return Ok(());
    };
    let mut binding: AdmissionPortBinding = serde_json::from_value(value)
        .map_err(|_| RunError::Validation("stored repository admission binding"))?;
    if !binding.valid() {
        return Err(RunError::Validation(
            "stored repository admission binding digest",
        ));
    }
    if binding.phase != AdmissionPhase::Released {
        binding.phase = AdmissionPhase::ReleasePending;
        record.extra.insert(
            ADMISSION_PORT_BINDING_FIELD.into(),
            serde_json::to_value(binding)
                .map_err(|_| RunError::Validation("repository admission binding"))?,
        );
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RefusalKind {
    Policy,
    PolicyBoundary(provenant_records::diagnostic::BoundaryClass),
    PolicyResource(provenant_records::diagnostic::ResourceFact),
    Preparation,
    PreparationBoundary(provenant_records::diagnostic::BoundaryClass),
    PreparationResource(provenant_records::diagnostic::ResourceFact),
    PacingReserve,
    PacingReserveResource(provenant_records::diagnostic::ResourceFact),
    WorktreeBusy,
    PriorityUnavailable,
    StoredPromptUnreadable,
    CloudPlacementRemoved,
    InvalidWriteGrantPath,
    WriteGrantsLimit(provenant_records::diagnostic::ResourceFact),
    Handoff,
    HandoffBoundary(provenant_records::diagnostic::BoundaryClass),
    HandoffResource(provenant_records::diagnostic::ResourceFact),
    RetainedImport {
        fence: provenant_supervisor::custody::PendingImport,
        worktree: std::path::PathBuf,
    },
}

impl RefusalKind {
    /// Project an inventoried resource refusal without trusting source text.
    pub fn diagnostic(&self) -> Option<provenant_records::diagnostic::Diagnostic> {
        use provenant_records::diagnostic::{Diagnostic, ResourceFact};
        let fact = match self {
            Self::HandoffResource(fact)
            | Self::PolicyResource(fact)
            | Self::PreparationResource(fact)
            | Self::WriteGrantsLimit(fact)
            | Self::PacingReserveResource(fact) => fact,
            _ => return None,
        };
        let kind = match fact {
            ResourceFact::Disk { .. } => ErrorKind::DiskFloor,
            ResourceFact::ArgvPromptLimit { .. } => ErrorKind::Invalid,
            ResourceFact::WriteGrantLimit { .. } => ErrorKind::Limit,
            ResourceFact::ContinuationCopy { .. }
            | ResourceFact::DependencyPreparation { .. }
            | ResourceFact::LanePorts { .. }
            | ResourceFact::PacingReserve { .. } => ErrorKind::Refused,
            ResourceFact::GrantLimit { .. } | ResourceFact::ProfileLimit { .. } => {
                ErrorKind::Confinement
            }
            ResourceFact::MemoryPressure { .. } => ErrorKind::MemoryPressure,
            ResourceFact::Processes { .. } => ErrorKind::Processes,
            ResourceFact::LaneCap { .. } => ErrorKind::LaneCap,
        };
        Some(
            fact.project(Diagnostic::new(kind.code()).category(kind.category()))
                .fact(provenant_records::diagnostic::DiagnosticFact::Resource(
                    *fact,
                )),
        )
    }

    /// Retain only the typed owner's safe detail and corrective action.
    fn terminal_note(&self) -> Option<String> {
        let diagnostic = self.diagnostic()?;
        let fields = [diagnostic.detail, diagnostic.next];
        Some(provenant_records::diagnostic::safe_text(
            &fields.into_iter().flatten().collect::<Vec<_>>().join("; "),
        ))
    }

    /// The boundary established by this kind alone. Generic preparation and
    /// handoff errors need their original cause to establish a boundary.
    pub fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::HandoffBoundary(class)
            | Self::PreparationBoundary(class)
            | Self::PolicyBoundary(class) => *class,
            Self::WorktreeBusy | Self::InvalidWriteGrantPath => B::Authority,
            Self::RetainedImport { .. } => B::LaneGit,
            Self::StoredPromptUnreadable | Self::CloudPlacementRemoved => B::Authority,
            Self::Policy
            | Self::Preparation
            | Self::PacingReserve
            | Self::PacingReserveResource(_)
            | Self::PriorityUnavailable
            | Self::WriteGrantsLimit(_)
            | Self::Handoff
            | Self::HandoffResource(_)
            | Self::PreparationResource(_)
            | Self::PolicyResource(_) => B::None,
        }
    }
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

impl RunStore {
    /// Refuse queued proposals without touching any launched provider.
    pub fn refuse_queued(
        &self,
        attempt: AttemptId,
        reason: &str,
        call: &Request<'_>,
    ) -> Result<()> {
        let ids: Vec<String> = {
            let mut query = self
                .db
                .prepare("SELECT id FROM attempts WHERE id=?1 AND project=?2 AND terminal=0")?;
            let rows = query.query_map(
                rusqlite::params![attempt.to_string(), self.context.project_id.to_string()],
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
    pub fn request(&self, attempt: AttemptId, call: &Request<'_>) -> Result<CanonicalRequest> {
        Ok(self.request_with_digest(attempt, call)?.0)
    }
    /// The request and the digest of its stored bytes. Callers compare this
    /// digest, never a reserialised request, because its schema may have
    /// evolved since the run was written.
    pub(crate) fn request_with_digest(
        &self,
        attempt: AttemptId,
        call: &Request<'_>,
    ) -> Result<(CanonicalRequest, provenant_core::Digest)> {
        let raw = read_sealed(
            &self.attempt_dir(attempt, false)?,
            "request.json",
            RECORD_LIMIT,
        )?
        .ok_or(RunError::NotFound)?;
        let digest = provenant_records::stored_record_digest("run-request", 1, &raw)?;
        let stored = self.attempt_operation(attempt, call)?.request_digest;
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
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
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
    pub fn start_checked(
        &self,
        attempt: AttemptId,
        expected: &CanonicalRequest,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
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
            Some(expected.digest()?),
            call,
        )
    }
    /// Native observation seam for integration fixtures. The ordinary start
    /// path always uses supervisor's system observer and nonblocking lease.
    #[cfg(feature = "fixture")]
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_writer_lease(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        expected: Option<&CanonicalRequest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        self.start_inner(
            attempt,
            providers,
            context,
            acquire_writer,
            expected.map(CanonicalRequest::digest).transpose()?,
            call,
        )
    }
    fn admission_binding(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        call: &Request<'_>,
    ) -> std::result::Result<(AttemptRecord, Option<AdmissionPortBinding>), &'static str> {
        if let Some(value) = record.extra.get(ADMISSION_PORT_BINDING_FIELD) {
            let binding: AdmissionPortBinding = serde_json::from_value(value.clone())
                .map_err(|_| "stored repository admission binding is invalid")?;
            if !binding.valid() {
                return Err("stored repository admission binding digest is invalid");
            }
            return Ok((record.clone(), Some(binding)));
        }
        let Some(writer) = request.task.writer.as_ref() else {
            return Ok((record.clone(), None));
        };
        let ports =
            provenant_config::PortTable::read_for(Some(&writer.project), &self.writer_registry)
                .map_err(|_| "repository admission declaration is unavailable")?;
        let (source, command) = match &ports.admission.backend {
            provenant_config::PortBackend::Builtin | provenant_config::PortBackend::None => {
                return Ok((record.clone(), None));
            }
            provenant_config::PortBackend::Command { command } => (ports.admission.source, command),
        };
        let binding = AdmissionPortBinding::new(source, command);
        let encoded = serde_json::to_value(&binding)
            .map_err(|_| "repository admission binding could not be encoded")?;
        let updated = self
            .record_admission_binding(record, encoded, call)
            .map_err(|_| "repository admission binding could not be stored")?;
        let value = updated
            .extra
            .get(ADMISSION_PORT_BINDING_FIELD)
            .ok_or("repository admission binding was not stored")?;
        let binding: AdmissionPortBinding = serde_json::from_value(value.clone())
            .map_err(|_| "stored repository admission binding is invalid")?;
        if !binding.valid() {
            return Err("stored repository admission binding digest is invalid");
        }
        Ok((updated, Some(binding)))
    }

    fn run_admission_phase(
        &self,
        request: &CanonicalRequest,
        attempt: AttemptId,
        binding: &AdmissionPortBinding,
        phase: AdmissionPhase,
        cancel: &provenant_core::CancelToken,
    ) -> bool {
        let Ok(executor) = provenant_os::exec::SystemExecutor::new(1) else {
            return false;
        };
        self.run_admission_phase_with_executor(request, attempt, binding, phase, cancel, &executor)
    }

    fn run_admission_phase_with_executor(
        &self,
        request: &CanonicalRequest,
        attempt: AttemptId,
        binding: &AdmissionPortBinding,
        phase: AdmissionPhase,
        cancel: &provenant_core::CancelToken,
        executor: &dyn provenant_os::exec::Executor,
    ) -> bool {
        let Some(writer) = request.task.writer.as_ref() else {
            return false;
        };
        let Some(command_phase) = phase.command() else {
            return false;
        };
        let execution = if binding.valid() {
            let environment = [
                ("PROVENANT_PORT_PHASE".into(), command_phase.into()),
                (
                    "PROVENANT_ADMISSION_ATTEMPT".into(),
                    attempt.to_string().into(),
                ),
                (
                    "PROVENANT_ADMISSION_WORKTREE".into(),
                    writer.worktree.as_os_str().to_owned(),
                ),
                (
                    "PROVENANT_ADMISSION_PROJECT".into(),
                    writer.project.as_os_str().to_owned(),
                ),
                (
                    "PROVENANT_ADMISSION_OWNER_PID".into(),
                    std::process::id().to_string().into(),
                ),
                (
                    "PROVENANT_ADMISSION_EPOCH".into(),
                    binding.lease_epoch.to_string().into(),
                ),
            ];
            let timeout =
                std::time::Duration::from_secs(request.task.provider_timeout_seconds.max(1));
            provenant_config::execute_port_command(
                executor,
                &binding.command,
                &writer.project,
                &[],
                &environment,
                timeout,
                cancel,
            )
        } else {
            provenant_config::PortCommandExecution::failed(&binding.command)
        };
        let request_key = format!("{attempt}:{}", binding.lease_epoch);
        let evidence = provenant_config::record_port_command(
            &self.writer_registry,
            "admission",
            binding.source,
            command_phase,
            &request_key,
            &execution,
        );
        execution.disposition == provenant_config::PortCommandDisposition::Succeeded
            && evidence.is_ok()
    }

    #[cfg(test)]
    fn admission_skip_with_phase(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &AdmissionPortBinding,
            AdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> (AttemptRecord, Option<String>) {
        let (record, binding) = match self.admission_binding(request, record, call) {
            Ok(value) => value,
            Err(_) => {
                return (
                    record.clone(),
                    Some("repository admission configuration or binding is unavailable; attempt remains queued".into()),
                );
            }
        };
        self.admission_skip_with_phase_bound(request, &record, binding, call, run_phase)
    }

    fn admission_skip_with_phase_bound(
        &self,
        request: &CanonicalRequest,
        record: &AttemptRecord,
        binding: Option<AdmissionPortBinding>,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &AdmissionPortBinding,
            AdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> (AttemptRecord, Option<String>) {
        let mut record = record.clone();
        let Some(mut binding) = binding else {
            return (record, None);
        };
        if !matches!(
            record.state,
            AttemptState::Queued { .. } | AttemptState::Starting { .. }
        ) {
            return (
                record,
                Some("attempt state changed during repository admission".into()),
            );
        }
        if binding.phase == AdmissionPhase::Released {
            match self.record_admission_phase(&record, AdmissionPhase::AcquirePending, call) {
                Ok(updated) => {
                    record = updated;
                    binding = match self.admission_binding(request, &record, call) {
                        Ok((_, Some(binding))) => binding,
                        _ => {
                            return (
                                record,
                                Some("repository admission binding could not be reloaded".into()),
                            );
                        }
                    };
                }
                Err(_) => {
                    record = self.record(record.attempt_id, call).unwrap_or(record);
                    return (
                        record,
                        Some("repository admission state changed; attempt remains queued".into()),
                    );
                }
            }
        }
        if binding.phase == AdmissionPhase::Acquired {
            return (record, None);
        }
        if binding.phase == AdmissionPhase::ReleasePending {
            let released = self.release_admission_with_phase(&record, call, run_phase);
            if !released {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: repository admission release remains pending; attempt remains queued"
                );
            }
            return (
                self.record(record.attempt_id, call).unwrap_or(record),
                Some("repository admission cleanup is pending; attempt remains queued".into()),
            );
        }

        let cancelled_before_acquire = call.cancel.is_cancelled()
            || self
                .attempt_dir(record.attempt_id, false)
                .is_ok_and(|dir| crate::store::cancel_requested(&dir, Self::scope(&record)));
        if cancelled_before_acquire {
            return (
                record,
                Some("cancellation requested before repository admission".into()),
            );
        }
        let acquired = run_phase(
            request,
            record.attempt_id,
            &binding,
            AdmissionPhase::AcquirePending,
            call.cancel,
        );
        let current = if acquired {
            match self.record_admission_phase(&record, AdmissionPhase::Acquired, call) {
                Ok(updated) => return (updated, None),
                Err(_) => self.record(record.attempt_id, call).unwrap_or(record),
            }
        } else {
            self.record(record.attempt_id, call).unwrap_or(record)
        };
        let current_phase = current
            .extra
            .get(ADMISSION_PORT_BINDING_FIELD)
            .and_then(|value| serde_json::from_value::<AdmissionPortBinding>(value.clone()).ok())
            .map(|binding| binding.phase);
        if current_phase == Some(AdmissionPhase::Acquired)
            && matches!(
                current.state,
                AttemptState::Queued { .. } | AttemptState::Starting { .. }
            )
        {
            return (current, None);
        }
        if acquired {
            let released = self.release_admission_with_phase(&current, call, run_phase);
            if !released {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: repository admission release failed after a concurrent terminal change"
                );
            }
            return (
                self.record(current.attempt_id, call).unwrap_or(current),
                Some("attempt state changed during repository admission".into()),
            );
        }
        let current =
            match self.record_admission_phase(&current, AdmissionPhase::ReleasePending, call) {
                Ok(updated) => updated,
                Err(_) => self.record(current.attempt_id, call).unwrap_or(current),
            };
        let released = self.release_admission_with_phase(&current, call, run_phase);
        if !released {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: repository admission release failed after an unsuccessful acquire"
            );
        }
        (
            self.record(current.attempt_id, call).unwrap_or(current),
            Some(
                "repository admission skipped this attempt; cleanup must complete before retry"
                    .into(),
            ),
        )
    }

    /// Release a frozen attempt binding after terminal publication. Failed
    /// release leaves the existing terminal retention marker set for reconcile.
    pub(crate) fn release_admission(&self, record: &AttemptRecord, call: &Request<'_>) -> bool {
        let released = self.release_admission_with_phase(
            record,
            call,
            &mut |request, attempt, binding, phase, cancel| {
                self.run_admission_phase(request, attempt, binding, phase, cancel)
            },
        );
        if !released && matches!(record.state, AttemptState::Terminal { .. }) {
            let _ = self.set_temporary_retained(record.attempt_id, true, call);
        }
        released
    }

    fn admission_release_receipt_confirmed(
        &self,
        attempt: AttemptId,
        binding: &AdmissionPortBinding,
    ) -> bool {
        let key = format!("{attempt}:{}", binding.lease_epoch);
        provenant_config::port_command_receipt_proof(
            &self.writer_registry,
            "admission",
            binding.source,
            "release",
            &key,
            binding.command_digest,
            &binding.command.exit_codes.success,
        )
        .ok()
        .flatten()
        .is_some_and(|proof| proof.successful_exit)
    }

    fn release_admission_with_phase(
        &self,
        record: &AttemptRecord,
        call: &Request<'_>,
        run_phase: &mut dyn FnMut(
            &CanonicalRequest,
            AttemptId,
            &AdmissionPortBinding,
            AdmissionPhase,
            &provenant_core::CancelToken,
        ) -> bool,
    ) -> bool {
        let Some(value) = record.extra.get(ADMISSION_PORT_BINDING_FIELD) else {
            return true;
        };
        let mut binding: AdmissionPortBinding = match serde_json::from_value(value.clone()) {
            Ok(binding) if binding.valid() => binding,
            _ => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: stored repository admission binding is invalid; release was not run"
                );
                return false;
            }
        };
        if binding.phase == AdmissionPhase::Released {
            return true;
        }
        let mut current = match self.record(record.attempt_id, call) {
            Ok(current) => current,
            Err(_) => return false,
        };
        let Some(current_value) = current.extra.get(ADMISSION_PORT_BINDING_FIELD) else {
            return false;
        };
        binding = match serde_json::from_value(current_value.clone()) {
            Ok(binding) if binding.valid() => binding,
            _ => return false,
        };
        if binding.phase == AdmissionPhase::Released {
            return true;
        }
        if matches!(current.state, AttemptState::Terminal { .. })
            && binding.phase == AdmissionPhase::ReleasePending
            && self.admission_release_receipt_confirmed(record.attempt_id, &binding)
        {
            return true;
        }
        if binding.phase != AdmissionPhase::ReleasePending {
            match self.record_admission_phase(&current, AdmissionPhase::ReleasePending, call) {
                Ok(updated) => current = updated,
                Err(_) => {
                    current = match self.record(record.attempt_id, call) {
                        Ok(current) => current,
                        Err(_) => return false,
                    };
                    let Some(value) = current.extra.get(ADMISSION_PORT_BINDING_FIELD) else {
                        return false;
                    };
                    binding = match serde_json::from_value(value.clone()) {
                        Ok(binding) if binding.valid() => binding,
                        _ => return false,
                    };
                    if binding.phase == AdmissionPhase::Released {
                        return true;
                    }
                    if binding.phase != AdmissionPhase::ReleasePending {
                        return false;
                    }
                }
            }
            binding.phase = AdmissionPhase::ReleasePending;
        }
        let request = match self.request(record.attempt_id, call) {
            Ok(request) => request,
            Err(_) => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: repository admission release could not read the stored request"
                );
                return false;
            }
        };
        if !run_phase(
            &request,
            record.attempt_id,
            &binding,
            AdmissionPhase::ReleasePending,
            &provenant_core::CancelToken::new(),
        ) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: repository admission release failed; reconcile will retry the idempotent command"
            );
            return false;
        }
        if matches!(current.state, AttemptState::Terminal { .. }) {
            return self.admission_release_receipt_confirmed(record.attempt_id, &binding);
        }
        match self.record_admission_phase(&current, AdmissionPhase::Released, call) {
            Ok(_) => true,
            Err(_) => {
                let latest = match self.record(record.attempt_id, call) {
                    Ok(latest) => latest,
                    Err(_) => return false,
                };
                if matches!(latest.state, AttemptState::Terminal { .. }) {
                    return self.admission_release_receipt_confirmed(record.attempt_id, &binding);
                }
                let Some(value) = latest.extra.get(ADMISSION_PORT_BINDING_FIELD) else {
                    return false;
                };
                let latest_binding: AdmissionPortBinding =
                    match serde_json::from_value(value.clone()) {
                        Ok(binding) if binding.valid() => binding,
                        _ => return false,
                    };
                latest_binding.phase == AdmissionPhase::Released
                    || (latest_binding.phase == AdmissionPhase::ReleasePending
                        && self
                            .record_admission_phase(&latest, AdmissionPhase::Released, call)
                            .is_ok())
            }
        }
    }

    fn settle_starting_refusal(
        &self,
        id: AttemptId,
        reason: &str,
        finished_at: provenant_core::WallTime,
        call: &Request<'_>,
    ) -> Result<()> {
        self.withdraw_with_preparation_failure(
            id,
            crate::store::Withdrawal {
                expected: AttemptState::Starting {
                    extra: Default::default(),
                },
                outcome: Outcome::Rejected,
                reason,
                finished_at,
                failure: None,
                terminal_cause: Some(TerminalCause::HandoffRefused),
                terminal_note: None,
            },
            call,
        )?;
        Ok(())
    }

    fn cancel_preparation_if_requested(
        &self,
        id: AttemptId,
        record: &AttemptRecord,
        attempt_dir: &provenant_os::fs::Directory,
        finished_at: provenant_core::WallTime,
        call: &Request<'_>,
    ) -> Result<bool> {
        if !call.cancel.is_cancelled()
            && !crate::store::cancel_requested(attempt_dir, Self::scope(record))
        {
            return Ok(false);
        }
        // Cancellation stops preparation, but its owner must still settle and
        // release custody under a fresh bounded store request.
        let cleanup_cancel = provenant_core::CancelToken::new();
        let cleanup_call = Request {
            clock: call.clock,
            cancel: &cleanup_cancel,
        };
        self.withdraw_with_preparation_failure(
            id,
            crate::store::Withdrawal {
                expected: AttemptState::Starting {
                    extra: Default::default(),
                },
                outcome: Outcome::Cancelled,
                reason: "cancelled before launch",
                finished_at,
                failure: None,
                terminal_cause: None,
                terminal_note: None,
            },
            &cleanup_call,
        )?;
        Ok(true)
    }

    fn start_inner(
        &self,
        attempt: AttemptId,
        providers: &dyn Providers,
        context: &dyn Fn(&AttemptRecord, ActorId) -> Result<Vec<u8>>,
        acquire_writer: &dyn Fn(
            provenant_os::fs::Directory,
            Scope,
            custody::WriterBinding,
        ) -> custody::Result<custody::CustodyLease>,
        expected: Option<provenant_core::Digest>,
        call: &Request<'_>,
    ) -> Result<Vec<Started>> {
        // Every later use reads this one value, never the store again.
        let request = self.request(attempt, call)?;
        let policy = launch_policy(
            &self.writer_registry,
            &self.context.project_id.to_string(),
            &request.task,
        );
        if let Some(digest) = expected
            && request.digest()? != digest
        {
            return Err(policy.boundary_error(
                "LAUNCH_RUNSTORE_START_INNER_AUTHORITY_2",
                provenant_records::policy::Facts::violation(
                    "stored request digest changed after launch validation",
                ),
                RunError::Validation("request changed after launch validation"),
            ));
        }
        let operation = self.attempt_operation(attempt, call)?;
        let requester = operation.actor_id;
        let resume = request
            .parent
            .filter(|_| request.fresh_continuation.is_none())
            .map(|prior| self.record(prior, call))
            .transpose()?;
        let ids: Vec<String> = {
            let mut query = self.db.prepare(
                "SELECT id FROM attempts WHERE id=?1 AND project=?2 AND terminal=0 ORDER BY sequence",
            )?;
            let rows = query.query_map(
                params![attempt.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut started = Vec::new();
        for id in ids {
            let id = AttemptId::parse(&id).map_err(|_| RunError::Validation("attempt index ID"))?;
            let mut record = self.record(id, call)?;
            if !matches!(record.state, AttemptState::Queued { .. }) {
                continue;
            }
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };

            let task = &request.task;
            // A request stored before cloud placement was removed still names
            // `cloud`. Never hand it to the local supervisor; settle it so
            // status, reconcile and gc can clear the run.
            if task.extra.contains_key("cloud") {
                let cause = TerminalCause::CloudPlacementRemoved;
                match self.withdraw_with_preparation_failure(
                    id,
                    crate::store::Withdrawal {
                        expected: AttemptState::Queued {
                            extra: Default::default(),
                        },
                        outcome: Outcome::Rejected,
                        reason: cause.detail(),
                        finished_at: now()?,
                        failure: None,
                        terminal_cause: Some(cause),
                        terminal_note: None,
                    },
                    call,
                ) {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(RunError::Conflict) => continue,
                    Err(error) => return Err(error),
                }
                started.push(Started::Refused {
                    attempt: id,
                    reason: cause.detail().to_owned(),
                    kind: RefusalKind::CloudPlacementRemoved,
                });
                continue;
            }
            if let Some(prior) = &resume
                && prior.attempt_id == record.attempt_id
                && !self.custody_released(prior, call)?
            {
                continue;
            }
            // Freeze the repository command while the attempt is still queued,
            // then publish Starting under the launcher's liveness lock before
            // running any repository command or provider preparation.
            let (bound_record, admission_binding) =
                match self.admission_binding(&request, &record, call) {
                    Ok(value) => value,
                    Err(reason) => {
                        use std::io::Write as _;
                        let _ = writeln!(
                            std::io::stderr().lock(),
                            "warning: repository admission skipped {id}: {reason}"
                        );
                        started.push(Started::Skipped {
                            attempt: id,
                            reason: reason.to_owned(),
                        });
                        continue;
                    }
                };
            record = bound_record;
            let attempt_dir = self.attempt_dir(id, true)?;
            let _launcher = match attempt_dir.try_lock(
                std::ffi::OsStr::new("launcher.lock"),
                provenant_os::fs::LockMode::Exclusive,
            ) {
                Ok(guard) => guard,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            record = match self.claim_starting_for_preparation(&record, call) {
                Ok(record) => record,
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let mut effective_task = self.effective_launch_task(&request, task);
            self.seal_parent_sandbox(&request, &mut effective_task, call);
            let task = &effective_task;
            let (admitted_record, skipped) = self.admission_skip_with_phase_bound(
                &request,
                &record,
                admission_binding,
                call,
                &mut |request, attempt, binding, phase, cancel| {
                    self.run_admission_phase(request, attempt, binding, phase, cancel)
                },
            );
            record = admitted_record;
            if let Some(reason) = skipped {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                match self.requeue_starting_after_admission_skip(&record, call) {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        if self.cancel_preparation_if_requested(
                            id,
                            &record,
                            &attempt_dir,
                            now()?,
                            call,
                        )? {
                            started.push(Started::Refused {
                                attempt: id,
                                reason: "cancelled before launch".into(),
                                kind: RefusalKind::Preparation,
                            });
                            continue;
                        }
                        continue;
                    }
                    // A failed release keeps the Starting record fenced for
                    // reconcile; it is never re-enqueued while custody is unknown.
                    Err(RunError::Conflict) => {}
                    Err(error) => return Err(error),
                }
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: repository admission skipped {id}: {reason}"
                );
                started.push(Started::Skipped {
                    attempt: id,
                    reason,
                });
                continue;
            }
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let prompt = match self
                .artifact_bytes(&record.prompt, RECORD_LIMIT)
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| RunError::Validation("prompt is not UTF-8"))
                }) {
                Ok(prompt) => prompt,
                Err(error) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let code = if matches!(
                        &error,
                        RunError::ArtifactMismatch(_) | RunError::TooLarge { .. }
                    ) {
                        "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY"
                    } else {
                        "ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_AUTHORITY"
                    };
                    let error = policy.boundary_error(
                        code,
                        provenant_records::policy::Facts::violation(
                            "stored prompt could not be reopened and verified against the attempt record",
                        ),
                        error,
                    );
                    // The live preparation owner settles unreadable input; it
                    // must not leave a stale Starting row for another launcher.
                    let reason = format!("stored prompt unreadable: {error}");
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome: Outcome::Rejected,
                            reason: &reason,
                            finished_at: now()?,
                            failure: None,
                            terminal_cause: Some(TerminalCause::StoredPromptUnreadable),
                            terminal_note: None,
                        },
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
                .join(format!("attempts/{id}"));
            let prompt = output_prompt(&directory, &prompt);
            let mut staging_failure = None;
            for child in ["scratch", "provider-state", "tmp"] {
                if let Err(error) =
                    crate::artifacts::subdir(&attempt_dir, std::ffi::OsStr::new(child), true)
                {
                    staging_failure = Some(provenant_confinement::redact::bounded_line(
                        &format!("launch staging failed: {error}"),
                        240,
                    ));
                    break;
                }
            }
            if let Some(reason) = staging_failure {
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let base = self
                .context
                .state_root
                .as_path()
                .join("attempts")
                .join(id.to_string());
            let paths = LaunchPaths {
                attempt_id: id,
                operation_id: operation.operation_id.clone(),
                task_index: 0,
                lane_depth: request.lane_depth.unwrap_or(1),
                deadline_unix_ms: u64::try_from(now()?.as_micros() / 1000)
                    .unwrap_or(0)
                    .saturating_add(task.provider_timeout_seconds.saturating_mul(1000)),
                scratch: base.join("scratch"),
                provider_state: base.join("provider-state"),
                temporary: base.join("tmp"),
                temporary_alias: crate::short_temporary::ensure(&attempt_dir),
            };
            let priority = providers.priority(task).unwrap_or_else(|reason| {
                let fallback = provenant_records::run::default_lane_priority(task.writer.is_some(), task.build_heavy);
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(),
                    "warning: lane priority unavailable ({reason}); using compiled domains.fabric.lanes.priority default {fallback}");
                fallback
            });
            let launch_policy = check_launch_policy(providers, task, &policy);
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let prepared = match launch_policy {
                Ok(()) => providers
                    .prepare(task, &prompt, &paths, resume.as_ref())
                    .map(|plan| (plan, priority))
                    .map_err(|reason| {
                        let refusal = check_preparation_refusal(&policy, reason);
                        Box::new((refusal.kind(), refusal))
                    }),
                Err(refusal) => {
                    let kind = if let Some(fact) = refusal.resource {
                        RefusalKind::PolicyResource(fact)
                    } else if refusal.boundary_class
                        != provenant_records::diagnostic::BoundaryClass::None
                    {
                        RefusalKind::PolicyBoundary(refusal.boundary_class)
                    } else {
                        RefusalKind::Policy
                    };
                    Err(Box::new((kind, refusal)))
                }
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let (mut plan, priority) = match prepared {
                Ok(prepared) => prepared,
                Err(refused) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let (kind, refusal) = *refused;
                    if !refusal.skipped_routes.is_empty()
                        && let Err(error) =
                            self.record_skipped_routes(id, &refusal.skipped_routes, call)
                    {
                        let reason = provenant_confinement::redact::bounded_line(
                            &format!("launch preparation failed: {error}"),
                            240,
                        );
                        self.settle_starting_refusal(id, &reason, now()?, call)?;
                        started.push(Started::Refused {
                            attempt: id,
                            reason,
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let outcome = if refusal.auth_required() {
                        Outcome::AuthRequired
                    } else {
                        Outcome::Rejected
                    };
                    let terminal_cause = Some(refusal.terminal_cause);
                    let terminal_note = match (kind.terminal_note(), refusal.terminal_note) {
                        (Some(guidance), Some(note)) if guidance != note => {
                            Some(format!("{note}; {guidance}"))
                        }
                        (Some(note), _) | (_, Some(note)) => Some(note),
                        (None, None) => match refusal.skipped_routes.first() {
                            Some(route) => {
                                let diagnostic = route.diagnostic();
                                Some(format!(
                                    "first candidate: {}; {}",
                                    diagnostic.detail.unwrap_or_default(),
                                    diagnostic.next.unwrap_or_default()
                                ))
                            }
                            // The refusal's own message, redacted and bounded.
                            None => Some(provenant_confinement::redact::bounded_line(
                                &refusal.message,
                                240,
                            )),
                        },
                    };
                    let reason = refusal.message;
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome,
                            reason: &reason,
                            finished_at: now()?,
                            failure: refusal.fallback_outcome,
                            terminal_cause,
                            terminal_note,
                        },
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt_dir)?,
                        Ok(None) => continue,
                        // The attempt changed while this preparation owner was
                        // settling its refusal.
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
            let sandbox_binding = plan.bind_sandbox_backend(call.cancel, &policy);
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            if let Err(error) = sandbox_binding {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                let reason = provenant_confinement::redact::bounded_line(
                    &format!("launch preparation failed: {error}"),
                    240,
                );
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            let limits = plan.limits.clone();
            set_lane_environment(&mut plan.environment, request.lane_depth.unwrap_or(1), id);
            let limit = providers.write_grant_limit(&policy).checked(&policy);
            plan.route
                .extra
                .insert("write_grant_limit".into(), limit.receipt());
            let write_grants = applied_write_grants(&plan, limit, &policy);
            let write_grants = match write_grants {
                Ok(grants) => grants,
                Err(error) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                        continue;
                    }
                    let kind = match error {
                        WriteGrantError::InvalidPath => RefusalKind::InvalidWriteGrantPath,
                        WriteGrantError::Policy(fact) => RefusalKind::WriteGrantsLimit(fact),
                    };
                    let reason = error.to_string();
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Starting {
                                extra: Default::default(),
                            },
                            outcome: Outcome::Rejected,
                            reason: &reason,
                            finished_at: now()?,
                            failure: None,
                            terminal_cause: Some(TerminalCause::WriteGrantsRefused),
                            terminal_note: kind.terminal_note(),
                        },
                        call,
                    ) {
                        Ok(Some(_)) => crate::writer::settle_launch_staging(&attempt_dir)?,
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
            if !plan.skipped_routes.is_empty()
                && let Err(error) = self.record_skipped_routes(id, &plan.skipped_routes, call)
            {
                let reason = provenant_confinement::redact::bounded_line(
                    &format!("launch preparation failed: {error}"),
                    240,
                );
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            record = self.record(id, call)?;
            let record = match self.claim_launch(
                &record,
                &plan,
                &write_grants,
                priority,
                &limits,
                call,
                &mut std::io::stderr().lock(),
            ) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => {
                    if self.cancel_preparation_if_requested(
                        id,
                        &record,
                        &attempt_dir,
                        now()?,
                        call,
                    )? {
                        started.push(Started::Refused {
                            attempt: id,
                            reason: "cancelled before launch".into(),
                            kind: RefusalKind::Preparation,
                        });
                    }
                    continue;
                }
                Err(error) => {
                    let reason = provenant_confinement::redact::bounded_line(
                        &format!("launch preparation failed: {error}"),
                        240,
                    );
                    self.settle_starting_refusal(id, &reason, now()?, call)?;
                    started.push(Started::Refused {
                        attempt: id,
                        reason,
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
            };
            if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                started.push(Started::Refused {
                    attempt: id,
                    reason: "cancelled before launch".into(),
                    kind: RefusalKind::Preparation,
                });
                continue;
            }
            if let Err(error) = self.seal_work(&record, &plan, call) {
                if self.cancel_preparation_if_requested(id, &record, &attempt_dir, now()?, call)? {
                    started.push(Started::Refused {
                        attempt: id,
                        reason: "cancelled before launch".into(),
                        kind: RefusalKind::Preparation,
                    });
                    continue;
                }
                let reason = format!("launch preparation failed: {error}");
                self.settle_starting_refusal(id, &reason, now()?, call)?;
                started.push(Started::Refused {
                    attempt: id,
                    reason,
                    kind: RefusalKind::Preparation,
                });
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
                    &limits,
                    call,
                ) {
                    Ok(()) => Started::Launched(id),
                    // Nothing reached a guardian: the attempt ends here.
                    Err(
                        error @ (HandOff::Local(_)
                        | HandOff::Boundary { .. }
                        | HandOff::Resource { .. }
                        | HandOff::TypedLocal { .. }
                        | HandOff::Busy
                        | HandOff::RetainedImport { .. }),
                    ) => {
                        let terminal_cause = match &error {
                            HandOff::TypedLocal { cause, .. } => Some(*cause),
                            HandOff::Boundary { cause, .. } => {
                                cause.or(Some(TerminalCause::HandoffRefused))
                            }
                            HandOff::Busy => Some(TerminalCause::WorktreeBusy),
                            HandOff::RetainedImport { .. } => {
                                Some(TerminalCause::RetainedWriterImport)
                            }
                            _ => Some(TerminalCause::HandoffRefused),
                        };
                        let (reason, kind) = match error {
                            HandOff::Busy => ("worktree is busy".into(), RefusalKind::WorktreeBusy),
                            HandOff::Local(error) | HandOff::TypedLocal { message: error, .. } => (
                                format!("launch preparation failed: {error}"),
                                RefusalKind::Handoff,
                            ),
                            HandOff::Boundary { message, class, .. } => (
                                format!("launch preparation failed: {message}"),
                                RefusalKind::HandoffBoundary(class),
                            ),
                            HandOff::Resource { message, fact } => (
                                format!("launch preparation failed: {message}"),
                                RefusalKind::HandoffResource(fact),
                            ),
                            HandOff::RetainedImport { fence, worktree } => (
                                RunError::RetainedWriterImport {
                                    fence: fence.clone(),
                                    worktree: worktree.clone(),
                                }
                                .to_string(),
                                RefusalKind::RetainedImport {
                                    fence: *fence,
                                    worktree,
                                },
                            ),
                            _ => unreachable!(),
                        };
                        // Cancellation decides the outcome, but an independent
                        // preparation failure remains evidence for the chair.
                        let cancelled = self.attempt_dir(id, false).is_ok_and(|attempt| {
                            crate::store::cancel_requested(&attempt, Self::scope(&record))
                        });
                        let (outcome, reason) = if cancelled {
                            (
                                Outcome::Cancelled,
                                format!("cancelled before launch; {reason}"),
                            )
                        } else {
                            (Outcome::Rejected, reason.clone())
                        };
                        match self.withdraw_with_preparation_failure(
                            id,
                            crate::store::Withdrawal {
                                expected: AttemptState::Starting {
                                    extra: Default::default(),
                                },
                                outcome,
                                reason: &reason,
                                finished_at: now()?,
                                failure: None,
                                terminal_cause,
                                terminal_note: kind.terminal_note(),
                            },
                            call,
                        ) {
                            // Cancel already settled it from the other side.
                            Ok(_) | Err(RunError::Conflict) if cancelled => {}
                            Ok(_) => {}
                            Err(error) => return Err(error),
                        }
                        Started::Refused {
                            attempt: id,
                            reason,
                            kind,
                        }
                    }
                    Err(HandOff::Uncertain(error)) => {
                        self.launch_unknown(attempt, call)?;
                        Started::Unknown { attempt: id, error }
                    }
                    Err(HandOff::Protected(error)) => {
                        self.launch_blocked(attempt, &error, call)?;
                        Started::Blocked { attempt: id, error }
                    }
                },
            );
        }
        Ok(started)
    }
    /// The attempt's operation record and whether the attempt has settled,
    /// which makes the operation terminal.
    fn run_operation(
        &self,
        tx: &rusqlite::Connection,
        attempt: AttemptId,
    ) -> Result<(OperationRecord, bool)> {
        let (raw, settled): (Vec<u8>, bool) = tx.query_row(
            "SELECT operation_record, terminal FROM attempts WHERE id=?1 AND project=?2 AND refused=0",
            params![attempt.to_string(), self.context.project_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((decode(&raw)?, settled))
    }
    fn launch_unknown(&self, attempt: AttemptId, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let (mut op, settled) = self.run_operation(tx, attempt)?;
            if settled {
                return Ok(());
            }
            op.state = OperationState::LaunchUnknown;
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE id=?2 AND project=?3",
                params![
                    crate::db::bytes(&op)?,
                    attempt.to_string(),
                    self.context.project_id.to_string()
                ],
            )?;
            Ok(())
        })
    }
    fn launch_blocked(&self, attempt: AttemptId, reason: &str, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let (mut op, settled) = self.run_operation(tx, attempt)?;
            if settled {
                return Err(RunError::Conflict);
            }
            op.state = OperationState::Blocked;
            op.blocked_reason = Some(reason.to_owned());
            tx.execute(
                "UPDATE attempts SET operation_record=?1 WHERE id=?2 AND project=?3",
                params![
                    crate::db::bytes(&op)?,
                    attempt.to_string(),
                    self.context.project_id.to_string()
                ],
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
        blocks_origin: Some(if settings.lane_port_blocks.is_some() {
            provenant_os::lane_ports::BlocksOrigin::HostConfig
        } else {
            provenant_os::lane_ports::BlocksOrigin::CompiledDefault
        }),
    }))
}

fn lane_port_plan_or_warn(
    state: &std::path::Path,
    policy: &provenant_records::policy::Policy,
) -> Option<provenant_os::lane_ports::Plan> {
    match lane_port_plan(state) {
        Ok(plan) => plan,
        Err(error) => {
            let effect = policy.check(
                "LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE",
                provenant_records::policy::Facts::violation(format!(
                    "lane port settings unavailable ({error}); continuing without a port block"
                )),
            );
            if let provenant_records::policy::Effect::Warn(message) = effect {
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
            }
            None
        }
    }
}

#[cfg(test)]
mod lane_environment_tests {
    use super::*;

    #[test]
    fn unavailable_lane_port_settings_skip_allocation() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let not_directory = temp.path().join("not-directory");
        std::fs::write(&not_directory, b"fixture")?;
        let policy = provenant_records::policy::Policy::default();
        assert!(
            !policy
                .rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE")
                .rule
                .refusable
        );
        assert_eq!(
            policy
                .rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE")
                .rule
                .strength,
            provenant_records::policy::Strength::Warn
        );
        assert!(lane_port_plan_or_warn(&not_directory, &policy).is_none());
        let mut off = policy.clone();
        off.overlay(
            &std::collections::BTreeMap::from([(
                "LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE".into(),
                provenant_records::policy::Strength::Off,
            )]),
            provenant_records::policy::Source::Project,
        );
        assert_eq!(
            off.rule("LAUNCH_LANE_PORT_PLAN_OR_WARN_NONE").source,
            provenant_records::policy::Source::Project
        );
        assert!(lane_port_plan_or_warn(&not_directory, &off).is_none());
        let defaults = lane_port_plan_or_warn(temp.path(), &policy).expect("default plan");
        assert_eq!(
            defaults.blocks_origin,
            Some(provenant_os::lane_ports::BlocksOrigin::CompiledDefault)
        );
        std::fs::write(
            temp.path().join("config.toml"),
            "schema_version=1\n[domains.fabric.lanes]\nlane_port_blocks=2\n",
        )?;
        let configured = lane_port_plan_or_warn(temp.path(), &policy).expect("configured plan");
        assert_eq!(configured.blocks, 2);
        assert_eq!(
            configured.blocks_origin,
            Some(provenant_os::lane_ports::BlocksOrigin::HostConfig)
        );
        Ok(())
    }

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
mod policy_reporting_tests {
    use super::*;
    use crate::tests::{call, clock, context, input, route};
    use provenant_confinement::permissions::{
        AccessMode, MandatoryRoots, NetworkAccess, PathGrant, PathScope, PlanPermissions,
        ProviderProfile, ProviderProfileSpec,
    };
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use provenant_core::{CancelToken, Clock as _, WorkScope};
    use std::path::Path;

    // Medium: real accepted requests and continuation through RunStore::start;
    // the provider edge inspects admission without spawning a process.
    #[test]
    fn continuation_keeps_the_sealed_parent_floor_and_policy()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        struct Inspect<'a> {
            strength: &'a str,
            prior: Option<AttemptId>,
        }
        impl Providers for Inspect<'_> {
            fn prepare(
                &self,
                task: &TaskRequest,
                _: &str,
                _: &LaunchPaths,
                prior: Option<&AttemptRecord>,
            ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
                assert_eq!(prior.map(|record| record.attempt_id), self.prior);
                assert_eq!(
                    task.extra
                        .get(provenant_records::run::PARENT_SANDBOXED_FIELD)
                        .and_then(serde_json::Value::as_bool),
                    Some(true)
                );
                let strength = task
                    .profile
                    .knobs
                    .get("lanes.sandbox_floor")
                    .and_then(serde_json::Value::as_str)
                    .unwrap();
                assert_eq!(strength, self.strength);
                let sandbox = task
                    .profile
                    .knobs
                    .get("lanes.sandbox")
                    .and_then(serde_json::Value::as_str);
                assert_eq!(
                    provenant_records::run::admission_floor(true, sandbox, strength),
                    if self.strength == "strict" {
                        (true, false)
                    } else {
                        (false, true)
                    }
                );
                Err(PrepareRefusal::boxed(
                    TerminalCause::RouteResolutionRefused,
                    "fixture inspected admission",
                ))
            }
            fn decode(
                &self,
                _: &RouteRecord,
                _: &[u8],
                _: &ExitObservation,
                _: &Path,
            ) -> std::result::Result<Decoded, String> {
                unreachable!()
            }
        }
        for strength in ["warn", "strict"] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let parent = input();
            store.create(&parent, &call)?;
            let mut applied = route();
            applied.extra.insert(
                provenant_records::run::SANDBOX_ROUTE_FIELD.into(),
                serde_json::json!({"requested":true,"effective":true,"source":"knob"}),
            );
            let starting = store.mark_starting(
                &store.record(parent.attempt_id, &call)?,
                &applied,
                &[],
                &Default::default(),
                None,
                &call,
            )?;
            store.withdraw(
                parent.attempt_id,
                starting.state,
                Outcome::Rejected,
                "fixture parent",
                clock.wall_now()?,
                &call,
            )?;
            assert!(store.attempt_floors_children(parent.attempt_id, &call));
            let mut child = input();
            child.attempt_id = AttemptId::from_bytes([7; 16]);
            child.operation_id = provenant_core::OperationId::parse("admission-child")?;
            child.request.parent_attempt = Some(parent.attempt_id);
            child
                .request
                .task
                .profile
                .knobs
                .insert("lanes.sandbox".into(), "off".into());
            child
                .request
                .task
                .profile
                .knobs
                .insert("lanes.sandbox_floor".into(), strength.into());
            crate::tests::rekey(&mut child);
            store.create(&child, &call)?;
            store.start(
                child.attempt_id,
                &Inspect {
                    strength,
                    prior: None,
                },
                &|_, _| unreachable!(),
                &call,
            )?;
            let mut continuation = input();
            continuation.request = child.request.clone();
            continuation.prompt = child.prompt.clone();
            continuation.attempt_id = AttemptId::from_bytes([8; 16]);
            continuation.operation_id =
                provenant_core::OperationId::parse("admission-continuation")?;
            continuation.request.parent = Some(child.attempt_id);
            store.create(&continuation, &call)?;
            store.start(
                continuation.attempt_id,
                &Inspect {
                    strength,
                    prior: Some(child.attempt_id),
                },
                &|_, _| unreachable!(),
                &call,
            )?;
        }
        Ok(())
    }

    #[test]
    fn claimed_launch_reports_once_when_a_prepared_attempt_is_replayed()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let expected =
            store.claim_starting_for_preparation(&store.record(run.attempt_id, &call)?, &call)?;
        let grant = |path: &str| PathGrant {
            path: path.into(),
            scope: PathScope::Subtree,
        };
        let mandatory = MandatoryRoots::new(
            Path::new("/fixture/home"),
            Path::new("/fixture/state"),
            Path::new("/fixture/source"),
        )?;
        let policy = provenant_confinement::run_config::RunConfig::default()
            .policy_for_launch(Path::new("/fixture/state"), mandatory);
        let plan = ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(store.context.project_id),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                policy: Some(policy),
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: grant("/fixture/scratch"),
                private_state: None,
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Open,
                nested_confinement: false,
            },
        })?;
        #[allow(unused_mut)]
        let mut prepared = PreparedLaunch {
            limits: Default::default(),
            argv: vec!["fixture".into()],
            environment: vec![],
            stdin: vec![],
            secret_input: None,
            cwd: "/fixture/source".into(),
            route: route(),
            skipped_routes: vec![],
            observed_head: None,
            repository: None,
            confinement: Some(crate::LaunchConfinement {
                plan,
                profile: ProviderProfile {
                    spec: ProviderProfileSpec::default(),
                    observations: Default::default(),
                },
                writer: None,
                home: "/fixture/home".into(),
                cargo_home: provenant_confinement::cargo::CargoHome::resolve(
                    std::path::Path::new("/fixture/home"),
                    |_| None,
                )
                .unwrap(),
                cargo_target_seed: false,
                state_root: "/fixture/state".into(),
                sandbox: provenant_confinement::run_config::SandboxMode {
                    requested: true,
                    effective: true,
                    source: provenant_confinement::run_config::SandboxSource::Knob,
                    warning: None,
                },
                backend: None,
            }),
        };
        // Bind a real unavailable backend before the claim is persisted.
        #[cfg(not(target_os = "macos"))]
        {
            prepared.bind_sandbox_backend(&cancel, &Default::default())?;
            let value = prepared
                .route
                .extra
                .get(provenant_records::run::SANDBOX_ROUTE_FIELD)
                .ok_or("missing sandbox receipt")?;
            let mode: provenant_confinement::run_config::SandboxMode =
                serde_json::from_value(value.clone())?;
            assert!(mode.warning.is_some());
            assert_eq!(
                mode.effective,
                provenant_confinement::run_config::current_process_is_sandboxed()
            );
            assert!(
                prepared
                    .confinement
                    .as_ref()
                    .ok_or("missing confinement")?
                    .backend
                    .is_none()
            );
        }
        let grants =
            applied_write_grants(&prepared, WriteGrantLimit::default(), &Default::default())?;
        let mut output = Vec::new();
        let claimed = store.claim_launch(
            &expected,
            &prepared,
            &grants,
            0,
            &Default::default(),
            &call,
            &mut output,
        )?;
        assert!(matches!(claimed.state, AttemptState::Starting { .. }));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            claimed
                .applied_route
                .as_ref()
                .and_then(|route| route.extra.get(provenant_records::run::SANDBOX_ROUTE_FIELD)),
            prepared
                .route
                .extra
                .get(provenant_records::run::SANDBOX_ROUTE_FIELD),
            "the claim must persist the actual backend warning and effective mode",
        );
        // Another launcher can have prepared from the same queued record.
        // The compare-and-swap must refuse its stale claim before reporting.
        assert!(matches!(
            store.claim_launch(
                &expected,
                &prepared,
                &grants,
                0,
                &Default::default(),
                &call,
                &mut output
            ),
            Err(RunError::Conflict)
        ));
        assert_eq!(
            String::from_utf8(output)?
                .lines()
                .filter(|line| line.starts_with("lane policy:"))
                .count(),
            1
        );
        Ok(())
    }
}
pub(crate) mod hand_off;

#[cfg(test)]
mod resource_status_tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::{CancelToken, Clock as _};
    use provenant_records::diagnostic::{
        LaneLimit, PressureLevel, PressureThreshold, ResourceFact, SettingSource,
    };
    use provenant_records::wire::LaneRow;

    struct Dependencies;
    impl Providers for Dependencies {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
            let mut refusal = PrepareRefusal::boxed(
                TerminalCause::RouteResolutionRefused,
                "DEPENDENCY_OUTPUT_SECRET",
            );
            refusal.resource = Some(ResourceFact::DependencyPreparation { configured: true });
            Err(refusal)
        }
        fn decode(
            &self,
            _: &RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            unreachable!()
        }
    }

    #[test]
    fn launch_resource_refusals_survive_withdrawal_in_retained_status()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        for handoff in [None, Some(false), Some(true)] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let context = context(temp.path())?;
            let store = RunStore::open(&context, &call)?;
            let run = input();
            let id = run.attempt_id;
            store.create(&run, &call)?;
            let started = store.start_inner(
                run.attempt_id,
                if handoff.is_some() {
                    &crate::dispatch_tests::Ready
                } else {
                    &Dependencies
                },
                &|_, _| Ok(Vec::new()),
                &|_, _, _| {
                    Err(custody::CustodyError::Admission(if handoff == Some(true) {
                        AdmissionError::LaneCap {
                            active: 2,
                            limit: 2,
                            kind: LaneLimit::HostTotal,
                            origin: SettingSource::Configuration,
                        }
                    } else {
                        AdmissionError::MemoryPressure {
                            level: PressureLevel::Critical,
                            threshold: PressureThreshold::Warning,
                            origin: SettingSource::Configuration,
                        }
                    }))
                },
                None,
                &call,
            )?;
            let [Started::Refused { kind, .. }] = started.as_slice() else {
                return Err("launch did not refuse".into());
            };
            let expected = kind.diagnostic().ok_or("missing resource diagnostic")?;
            assert_eq!(store.record(id, &call)?.provider_launched, Some(false));
            drop(store);
            let reopened = RunStore::open(&context, &call)?;
            let record = reopened.record(id, &call)?;
            let (state, reason) = LaneRow::state_and_reason(&record.state);
            assert_eq!(state, "rejected");
            let reason = reason.ok_or("missing retained status")?;
            assert!(
                reason.contains(expected.detail.as_deref().unwrap()),
                "{reason}"
            );
            assert!(
                reason.contains(expected.next.as_deref().unwrap()),
                "{reason}"
            );
            assert!(!reason.contains("DEPENDENCY_OUTPUT_SECRET"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod admission_port_lifecycle_tests {
    use super::*;
    use crate::tests::{call, clock, context, input};
    use provenant_core::CancelToken;
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };
    use std::{collections::VecDeque, sync::Mutex};

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    struct GateExecutor {
        exits: Mutex<VecDeque<i64>>,
        calls: Mutex<Vec<(String, String, String)>>,
    }

    impl GateExecutor {
        fn new(exits: impl IntoIterator<Item = i64>) -> Self {
            Self {
                exits: Mutex::new(exits.into_iter().collect()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Executor for GateExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> std::result::Result<ExecOutcome, ExecError> {
            let program = options
                .argv
                .first()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default();
            let environment = options.env.unwrap_or(&[]);
            let value = |name: &std::ffi::OsStr| {
                environment
                    .iter()
                    .find(|(key, _)| key.as_os_str() == name)
                    .map(|(_, value)| value.to_string_lossy().into_owned())
                    .unwrap_or_default()
            };
            let phase = value(std::ffi::OsStr::new("PROVENANT_PORT_PHASE"));
            let epoch = value(std::ffi::OsStr::new("PROVENANT_ADMISSION_EPOCH"));
            self.calls
                .lock()
                .expect("gate calls mutex")
                .push((program, phase, epoch));
            let code = self
                .exits
                .lock()
                .expect("gate exits mutex")
                .pop_front()
                .unwrap_or(0);
            Ok(ExecOutcome {
                termination: Termination::Exit(code),
                stdout: Default::default(),
                stderr: Default::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 1,
            })
        }
    }

    fn declaration(program: &str, source: &str) -> String {
        serde_json::json!({
            "schema_version": 2,
            "admission": {
                "source": source,
                "backend": {
                    "kind": "command",
                    "command": {
                        "argv": [program],
                        "exit_codes": {
                            "success": [0],
                            "unknown": [75],
                            "other": "refused"
                        }
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn admission_binding_retries_busy_release_and_survives_config_rotation() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let project = temp.path().join("project");
        let project_ports = project.join(".provenant/ports.json");
        std::fs::create_dir_all(
            project_ports
                .parent()
                .ok_or_else(|| std::io::Error::other("ports parent missing"))?,
        )?;
        std::fs::create_dir_all(context.state_root.as_path())?;
        std::fs::write(&project_ports, declaration("repo-gate-old", "project"))?;
        std::fs::write(
            context.state_root.as_path().join("ports.json"),
            r#"{"schema_version":2}"#,
        )?;

        let mut prepared = input();
        let attempt = prepared.attempt_id;
        prepared.request.task.writer = Some(provenant_records::run::WriterRequest {
            extra: crate::tests::writer_extra(),
            project: project.clone(),
            worktree: project.join("lane"),
        });
        crate::tests::rekey(&mut prepared);
        store.create(&prepared, &call)?;
        let request = store.request(attempt, &call)?;
        let record = store.record(attempt, &call)?;
        let (record, binding) = store
            .admission_binding(&request, &record, &call)
            .map_err(|error| std::io::Error::other(error.to_owned()))?;
        let binding = binding
            .ok_or_else(|| std::io::Error::other("project admission command was not bound"))?;
        assert_eq!(binding.source, provenant_config::PortSource::Project);
        assert_eq!(binding.phase, AdmissionPhase::AcquirePending);
        assert!(record.extra.contains_key(ADMISSION_PORT_BINDING_FIELD));

        // Busy acquisition is followed by a failed release. Both remain durable
        // in the same attempt binding and the attempt stays queued for retry.
        let executor = GateExecutor::new([1, 1, 0, 0, 0]);
        let (record, skipped) = store.admission_skip_with_phase(
            &request,
            &record,
            &call,
            &mut |request, id, binding, phase, cancel| {
                store.run_admission_phase_with_executor(
                    request, id, binding, phase, cancel, &executor,
                )
            },
        );
        assert!(skipped.is_some());
        assert!(matches!(record.state, AttemptState::Queued { .. }));
        let stored: AdmissionPortBinding =
            serde_json::from_value(record.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        assert_eq!(stored.phase, AdmissionPhase::ReleasePending);

        // The release retry and the next acquisition still use the bound project
        // command after the effective source rotates to the host declaration.
        std::fs::write(&project_ports, r#"{"schema_version":2}"#)?;
        std::fs::write(
            context.state_root.as_path().join("ports.json"),
            declaration("repo-gate-new", "host"),
        )?;
        let effective =
            provenant_config::PortTable::read_for(Some(&project), context.state_root.as_path())?;
        assert_eq!(
            effective.admission.source,
            provenant_config::PortSource::Host
        );
        let rotated_program = match effective.admission.backend {
            provenant_config::PortBackend::Command { command } => command.argv[0].clone(),
            _ => return Err(std::io::Error::other("host admission command was not active").into()),
        };
        assert_eq!(rotated_program, "repo-gate-new");

        let (record, skipped) = store.admission_skip_with_phase(
            &request,
            &record,
            &call,
            &mut |request, id, binding, phase, cancel| {
                store.run_admission_phase_with_executor(
                    request, id, binding, phase, cancel, &executor,
                )
            },
        );
        assert!(skipped.is_some());
        assert!(matches!(record.state, AttemptState::Queued { .. }));
        let (record, skipped) = store.admission_skip_with_phase(
            &request,
            &record,
            &call,
            &mut |request, id, binding, phase, cancel| {
                store.run_admission_phase_with_executor(
                    request, id, binding, phase, cancel, &executor,
                )
            },
        );
        assert!(skipped.is_none());
        let mut terminal_intent = record.clone();
        mark_admission_release_pending(&mut terminal_intent)?;
        let terminal_binding: AdmissionPortBinding =
            serde_json::from_value(terminal_intent.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        assert_eq!(terminal_binding.phase, AdmissionPhase::ReleasePending);
        let record =
            store.record_admission_phase(&record, AdmissionPhase::ReleasePending, &call)?;
        assert!(store.release_admission_with_phase(
            &record,
            &call,
            &mut |request, id, binding, phase, cancel| {
                store.run_admission_phase_with_executor(
                    request, id, binding, phase, cancel, &executor,
                )
            },
        ));
        let final_record = store.record(attempt, &call)?;
        let binding: AdmissionPortBinding =
            serde_json::from_value(final_record.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        assert_eq!(binding.phase, AdmissionPhase::Released);
        assert_eq!(binding.lease_epoch, 2);
        assert!(matches!(final_record.state, AttemptState::Queued { .. }));
        let calls = executor.calls.lock().expect("gate calls mutex");
        assert!(
            calls
                .iter()
                .all(|(program, _, _)| program == "repo-gate-old")
        );
        for (phase, epoch) in [
            ("acquire", "1"),
            ("release", "1"),
            ("acquire", "2"),
            ("release", "2"),
        ] {
            assert!(
                calls
                    .iter()
                    .any(|(_, observed, key)| observed == phase && key == epoch)
            );
        }
        drop(calls);

        // A release fence published while acquisition is in flight must win
        // over the late successful acquire result.
        let mut observed = Vec::new();
        let (record, skipped) = store.admission_skip_with_phase(
            &request,
            &final_record,
            &call,
            &mut |_, id, binding, phase, _| {
                observed.push((phase, binding.lease_epoch));
                if phase == AdmissionPhase::AcquirePending {
                    let current = store.record(id, &call).expect("acquiring record");
                    store
                        .record_admission_phase(&current, AdmissionPhase::ReleasePending, &call)
                        .expect("concurrent release fence");
                }
                true
            },
        );
        assert!(skipped.is_some());
        let fenced: AdmissionPortBinding =
            serde_json::from_value(record.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        assert_eq!(fenced.phase, AdmissionPhase::Released);
        assert!(observed.contains(&(AdmissionPhase::ReleasePending, fenced.lease_epoch)));
        Ok(())
    }
    #[test]
    fn starting_owner_waits_for_release_proof_and_unwinds_cancellation() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let project = temp.path().join("project");
        let project_ports = project.join(".provenant/ports.json");
        std::fs::create_dir_all(project_ports.parent().ok_or("ports parent missing")?)?;
        std::fs::create_dir_all(context.state_root.as_path())?;
        std::fs::write(&project_ports, declaration("repo-gate-bound", "project"))?;
        std::fs::write(
            context.state_root.as_path().join("ports.json"),
            r#"{"schema_version":2}"#,
        )?;

        let mut input = input();
        let attempt = input.attempt_id;
        input.request.task.writer = Some(provenant_records::run::WriterRequest {
            extra: crate::tests::writer_extra(),
            project: project.clone(),
            worktree: project.join("lane"),
        });
        crate::tests::rekey(&mut input);
        store.create(&input, &call)?;
        let request = store.request(attempt, &call)?;
        let queued = store.record(attempt, &call)?;
        let (queued, frozen) = store
            .admission_binding(&request, &queued, &call)
            .map_err(|error| std::io::Error::other(error.to_owned()))?;
        let frozen = frozen.ok_or("project admission command was not bound")?;
        let attempt_dir = store.attempt_dir(attempt, true)?;
        let launcher = attempt_dir.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let starting = store.claim_starting_for_preparation(&queued, &call)?;
        assert!(matches!(starting.state, AttemptState::Starting { .. }));
        assert!(starting.resolved_route.is_none());

        // A configuration rotation after the owner claim cannot replace the
        // declaration already bound to this attempt.
        std::fs::write(&project_ports, r#"{"schema_version":2}"#)?;
        std::fs::write(
            context.state_root.as_path().join("ports.json"),
            declaration("repo-gate-rotated", "host"),
        )?;
        let mut calls = Vec::new();
        let (pending, skipped) = store.admission_skip_with_phase_bound(
            &request,
            &starting,
            Some(frozen),
            &call,
            &mut |_, _, binding, phase, _| {
                calls.push((binding.command.argv[0].clone(), phase));
                false
            },
        );
        assert!(skipped.is_some());
        assert!(matches!(pending.state, AttemptState::Starting { .. }));
        assert!(
            calls
                .iter()
                .all(|(program, _)| program == "repo-gate-bound")
        );
        assert!(matches!(
            store.requeue_starting_after_admission_skip(&pending, &call),
            Err(RunError::Conflict)
        ));
        assert!(matches!(
            store.record(attempt, &call)?.state,
            AttemptState::Starting { .. }
        ));

        let binding: AdmissionPortBinding =
            serde_json::from_value(pending.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        let (released, skipped) = store.admission_skip_with_phase_bound(
            &request,
            &pending,
            Some(binding),
            &call,
            &mut |_, _, _, phase, _| phase == AdmissionPhase::ReleasePending,
        );
        assert!(skipped.is_some());
        let queued = store
            .requeue_starting_after_admission_skip(&released, &call)?
            .ok_or("cancellation unexpectedly won the requeue fence")?;
        assert!(matches!(queued.state, AttemptState::Queued { .. }));
        drop(launcher);

        let launcher = attempt_dir.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let starting = store.claim_starting_for_preparation(&queued, &call)?;
        custody::request_stop(
            attempt_dir.try_clone()?,
            RunStore::scope(&starting),
            starting.verified_owner.unwrap_or(context.actor_id),
            provenant_supervisor::machine::StopReason::ExplicitCancellation,
        )?;
        assert!(
            store
                .requeue_starting_after_admission_skip(&starting, &call)?
                .is_none()
        );
        assert!(matches!(
            store.record(attempt, &call)?.state,
            AttemptState::Starting { .. }
        ));
        drop(launcher);
        assert!(store.cancel_preparation_if_requested(
            attempt,
            &starting,
            &attempt_dir,
            clock.wall_now()?,
            &call,
        )?);
        assert!(matches!(
            store.record(attempt, &call)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn caller_cancellation_still_settles_the_preparation_owner() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let never = CancelToken::new();
        let inspection = call(&clock, &never);
        let store = RunStore::open(&context, &inspection)?;
        let input = input();
        let attempt = input.attempt_id;
        store.create(&input, &inspection)?;
        let queued = store.record(attempt, &inspection)?;
        let directory = store.attempt_dir(attempt, true)?;
        let _launcher = directory.try_lock(
            std::ffi::OsStr::new("launcher.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?;
        let starting = store.claim_starting_for_preparation(&queued, &inspection)?;
        let cancelled = CancelToken::new();
        cancelled.cancel();
        let cancelled_call = call(&clock, &cancelled);
        assert!(store.cancel_preparation_if_requested(
            attempt,
            &starting,
            &directory,
            clock.wall_now()?,
            &cancelled_call,
        )?);
        assert!(matches!(
            store.record(attempt, &inspection)?.state,
            AttemptState::Terminal {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn terminal_admission_release_uses_receipt_without_mutating_attempt() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = input();
        let attempt = input.attempt_id;
        store.create(&input, &call)?;
        let queued = store.record(attempt, &call)?;
        let command = provenant_config::PortCommand {
            argv: vec![
                temp.path()
                    .join("not-executed-admission-command")
                    .display()
                    .to_string(),
            ],
            exit_codes: provenant_config::CommandExitCodes {
                success: vec![0],
                unknown: vec![75],
                other: provenant_config::ExitDisposition::Refused,
            },
        };
        let binding = AdmissionPortBinding::new(provenant_config::PortSource::Project, &command);
        store.record_admission_binding(&queued, serde_json::to_value(&binding)?, &call)?;
        let terminal = store
            .withdraw(
                attempt,
                AttemptState::Queued {
                    extra: Default::default(),
                },
                Outcome::Cancelled,
                "cancel before preparation",
                clock.wall_now()?,
                &call,
            )?
            .ok_or("attempt did not settle")?;
        let terminal_binding: AdmissionPortBinding =
            serde_json::from_value(terminal.extra[ADMISSION_PORT_BINDING_FIELD].clone())?;
        assert_eq!(terminal_binding.phase, AdmissionPhase::ReleasePending);

        // A matching sidecar receipt proves an idempotent release while the
        // immutable terminal AttemptRecord remains byte-for-byte unchanged.
        let key = format!("{attempt}:{}", terminal_binding.lease_epoch);
        let execution = provenant_config::PortCommandExecution {
            command_digest: terminal_binding.command_digest,
            disposition: provenant_config::PortCommandDisposition::Succeeded,
            outcome: Some(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream::default(),
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 1,
            }),
        };
        provenant_config::record_port_command(
            context.state_root.as_path(),
            "admission",
            terminal_binding.source,
            "release",
            &key,
            &execution,
        )?;
        assert!(store.admission_release_receipt_confirmed(attempt, &terminal_binding));
        assert!(
            store.release_admission_with_phase(&terminal, &call, &mut |_, _, _, _, _| panic!(
                "confirmed receipt must avoid re-running release"
            ),)
        );
        assert!(matches!(
            store.record_admission_phase(&terminal, AdmissionPhase::Released, &call),
            Err(RunError::Conflict)
        ));
        assert_eq!(store.record(attempt, &call)?, terminal);
        Ok(())
    }
}
