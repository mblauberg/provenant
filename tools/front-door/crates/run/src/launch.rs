//! Supervisor handoff for accepted attempts. The provider seam is minimal and
//! shaped for the providers crate's launch specification: the composition edge
//! resolves routes, applies fallback and builds the exact invocation.
use crate::{artifacts::read_sealed, db::decode, *};
use provenant_core::{ActorId, AttemptId, OperationId, RunId};
use provenant_supervisor::custody::{
    self, CapturePlan, ExitObservation, OwnedLaunch, ProviderCommand, PublicationReservation, Scope,
};
use rusqlite::params;
use std::{ffi::OsString, path::PathBuf};

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

/// The token budget of one lane: a writer's, resolved from host configuration
/// with the project, and 0 (none) for any other lane. Each fresh read keeps a
/// setting changed between lanes in effect for the next one.
pub fn token_budget(
    task: &TaskRequest,
    state_root: &std::path::Path,
    project: &str,
) -> provenant_confinement::run_config::TokenBudget {
    use provenant_confinement::run_config::{RunConfig, TokenBudget, TokenBudgetSource};
    if task.writer.is_none() {
        return TokenBudget {
            tokens: 0,
            source: TokenBudgetSource::Default,
        };
    }
    RunConfig::read(state_root)
        .unwrap_or_default()
        .writer_token_budget_for(project, None)
}

pub fn capture_plan(
    task: &TaskRequest,
    runtime_seconds: u64,
    token_budget: u64,
    provider: &str,
) -> CapturePlan {
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
        token_budget: if crate::token_budget::enforces(provider) {
            token_budget
        } else {
            0
        },
        slot_wait_ms: task
            .profile
            .knob_u64("lanes.slot_wait_seconds")
            .map_or_else(
                provenant_supervisor::custody::default_slot_wait_ms,
                |seconds| seconds.saturating_mul(1000),
            ),
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
}

#[allow(clippy::print_stderr)]
fn applied_write_grants(
    plan: &PreparedLaunch,
    limit: WriteGrantLimit,
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
    let encoded =
        provenant_records::CanonicalV1::bytes(&grants).map_err(|_| WriteGrantError::InvalidPath)?;
    if encoded.as_bytes().len() as u64 > limit.bytes {
        eprintln!(
            "warning: encoded write grants are {} bytes; domains.run.write_grant_limit_bytes={} (source: {})",
            encoded.as_bytes().len(),
            limit.bytes,
            limit.origin.description()
        );
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
}
/// A task-local preparation refusal, retaining candidate classifications.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct PrepareRefusal {
    /// Boundary established by the original typed preparation failure, if any.
    pub boundary_class: provenant_records::diagnostic::BoundaryClass,
    /// A configurable resource threshold that refused this launch, if known.
    pub resource: Option<provenant_records::diagnostic::ResourceFact>,
    pub terminal_cause: Option<TerminalCause>,
    /// Fixed-form text naming a configured value, never captured output.
    pub terminal_note: Option<String>,
    pub message: String,
    pub skipped_routes: Vec<SkippedRoute>,
    pub fallback_outcome: Option<provenant_records::run::PreparationFailure>,
}
impl PrepareRefusal {
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
impl From<String> for PrepareRefusal {
    fn from(message: String) -> Self {
        Self {
            boundary_class: provenant_records::diagnostic::BoundaryClass::None,
            resource: None,
            message,
            terminal_cause: Some(TerminalCause::PreparationRefused),
            terminal_note: None,
            skipped_routes: Vec::new(),
            fallback_outcome: None,
        }
    }
}
impl From<&str> for PrepareRefusal {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

#[cfg(test)]
#[test]
fn preparation_refusal_retains_its_typed_boundary() {
    use provenant_records::diagnostic::{BoundaryClass, ResourceFact, SettingSource};
    let mut refusal = PrepareRefusal::from("opaque source");
    assert_eq!(refusal.kind().boundary_class(), BoundaryClass::None);
    refusal.boundary_class = BoundaryClass::Credentials;
    assert_eq!(refusal.kind().boundary_class(), BoundaryClass::Credentials);
    refusal.boundary_class = BoundaryClass::None;
    refusal.resource = Some(ResourceFact::ArgvPromptLimit {
        limit: 3,
        origin: SettingSource::Configuration,
    });
    let diagnostic = refusal.kind().diagnostic().expect("resource diagnostic");
    assert_eq!(diagnostic.code, "INVALID");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("domains.run.argv_prompt_limit_bytes"));
    assert!(detail.contains("host configuration"));
    let inventory = include_str!("../assets/launch-guards.toml")
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert!(
        inventory["guard"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .any(|row| {
                row.get("site").and_then(|v| v.as_str())
                    == Some("providers/src/contract.rs#Prompt::check_argv")
                    && row.get("code").and_then(|v| v.as_str()) == Some(diagnostic.code.as_str())
                    && row.get("boundary").and_then(|v| v.as_str()) == Some("configurable_default")
                    && row
                        .get("setting")
                        .and_then(|v| v.as_str())
                        .is_some_and(|setting| detail.contains(setting))
            })
    );
}
#[test]
fn write_grant_limit_diagnostic_names_setting_and_source() {
    use provenant_records::diagnostic::{BoundaryClass, ResourceFact, SettingSource};
    for source in [SettingSource::Configuration, SettingSource::ShippedDefault] {
        let kind = RefusalKind::WriteGrantsLimit(ResourceFact::WriteGrantLimit {
            limit: 1024,
            origin: source,
        });
        assert_eq!(kind.boundary_class(), BoundaryClass::None);
        let diagnostic = kind.diagnostic().unwrap();
        assert_eq!(diagnostic.code, "LIMIT");
        let detail = diagnostic.detail.unwrap();
        assert!(detail.contains("domains.run.write_grant_limit_bytes"));
        assert!(detail.contains("1024"));
        assert!(detail.contains(source.description()));
        let inventory = include_str!("../assets/launch-guards.toml")
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(
            inventory["guard"]
                .as_array_of_tables()
                .unwrap()
                .iter()
                .any(|row| {
                    row.get("site").and_then(|v| v.as_str())
                        == Some("run/src/launch.rs#applied_write_grants")
                        && row.get("code").and_then(|v| v.as_str())
                            == Some(diagnostic.code.as_str())
                        && row.get("boundary").and_then(|v| v.as_str())
                            == Some("configurable_default")
                        && row
                            .get("setting")
                            .and_then(|v| v.as_str())
                            .is_some_and(|setting| detail.contains(setting))
                })
        );
    }
}

#[test]
fn dependency_preparation_diagnostic_names_setting_and_source() {
    let inventory = include_str!("../assets/launch-guards.toml")
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let warning = inventory["guard"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .find(|row| {
            row.get("site").and_then(|v| v.as_str())
                == Some("cli/src/run/provider/preparation.rs#dependency_preparation_event")
        })
        .expect("dependency preparation warning is inventoried");
    assert_eq!(
        warning.get("boundary").and_then(|v| v.as_str()),
        Some("warning")
    );
    let reason = warning.get("reason").and_then(|v| v.as_str()).unwrap();
    assert!(reason.contains("lanes.prepare_deps"));
    assert!(reason.contains("compiled default"));
    assert!(reason.contains("sealed profile"));
    assert!(!reason.contains("TOKEN_SENTINEL"));
}

#[test]
fn lane_port_limit_diagnostic_matches_inventory() {
    use provenant_records::diagnostic::{PortSettingSource, ResourceFact};
    let inventory = include_str!("../assets/launch-guards.toml")
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    for source in [
        PortSettingSource::Configuration,
        PortSettingSource::ShippedDefault,
        PortSettingSource::RecordedPlan,
    ] {
        let fact = ResourceFact::LanePorts {
            blocks: 2,
            origin: source,
        };
        let diagnostic = RefusalKind::HandoffResource(fact).diagnostic().unwrap();
        let detail = diagnostic.detail.unwrap();
        assert!(detail.contains("domains.fabric.lanes.lane_port_blocks"));
        assert!(detail.contains(source.description()));
        assert!(
            inventory["guard"]
                .as_array_of_tables()
                .unwrap()
                .iter()
                .any(|row| {
                    row.get("site").and_then(|v| v.as_str())
                        == Some("supervisor/src/guardian.rs#Driver::run")
                        && row.get("boundary").and_then(|v| v.as_str())
                            == Some("configurable_default")
                        && row.get("code").and_then(|v| v.as_str())
                            == Some(diagnostic.code.as_str())
                        && row
                            .get("setting")
                            .and_then(|v| v.as_str())
                            .is_some_and(|setting| detail.contains(setting))
                })
        );
    }
}

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
    let mut refusal = PrepareRefusal::from("opaque source");
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
    pub run_id: RunId,
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
    runs: provenant_os::fs::Directory,
    run: RunId,
    retained: bool,
}
impl Drop for PreparedStart {
    fn drop(&mut self) {
        if !self.retained {
            // No attempt record survives for retention to find the alias by,
            // so unlink it before its attempt directory is deleted.
            let attempt = crate::artifacts::subdir(
                &self.runs,
                std::ffi::OsStr::new(&self.run.to_string()),
                false,
            )
            .and_then(|run| crate::artifacts::subdir(&run, std::ffi::OsStr::new("attempts"), false))
            .and_then(|attempts| {
                crate::artifacts::subdir(
                    &attempts,
                    std::ffi::OsStr::new(&self.attempt.to_string()),
                    false,
                )
            });
            if let Ok(attempt) = attempt {
                crate::short_temporary::remove(&attempt);
            }
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
    assert!(prompt.contains("$TMPDIR"));
    assert!(prompt.contains("not in the worktree"));
}

impl RunStore {
    /// Only the launcher that commits Queued -> Starting reports the resolved
    /// policy. Preparation and sealing can retry or race without duplicating
    /// this attempt's report, including a retained fresh-start plan.
    #[allow(clippy::too_many_arguments)]
    fn claim_launch(
        &self,
        expected: &AttemptRecord,
        plan: &PreparedLaunch,
        grants: &admission::AppliedWriteGrants,
        priority: u8,
        runtime: u64,
        call: &Request<'_>,
        output: &mut impl std::io::Write,
    ) -> Result<AttemptRecord> {
        let writable = plan
            .confinement
            .as_ref()
            .and_then(|confinement| confinement.plan.permissions().read_only.as_ref())
            .map(|grants| grants.writable_paths())
            .unwrap_or_default();
        let record = self.mark_starting_with_priority(
            expected,
            &plan.route,
            writable,
            grants,
            plan.observed_head,
            Some(priority),
            runtime,
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
    pub(crate) fn prepare_fresh_start(
        &self,
        input: &PreparedRun,
        providers: &dyn Providers,
        call: &Request<'_>,
    ) -> Result<PreparedStart> {
        let [task] = input.request.tasks.as_slice() else {
            return Err(RunError::Validation("continuation task count"));
        };
        let effective = self.effective_launch_task(&input.request, task);
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
            temporary_alias: crate::short_temporary::ensure(&directory),
        };
        let prompt = output_prompt(&base, &attempt.prompt);
        let plan = providers
            .check_policy(task)
            .map_err(|error| match error.terminal_cause {
                Some(cause) => RunError::ContinuationRefused { cause },
                None => RunError::Input {
                    kind: ErrorKind::ContinuationUnsupported,
                    message: error.to_string(),
                },
            })
            .and_then(|()| {
                providers
                    .prepare(task, &prompt, &paths, None)
                    .map_err(|error| match error.terminal_cause {
                        Some(cause) => RunError::ContinuationRefused { cause },
                        None => RunError::Input {
                            kind: ErrorKind::ContinuationUnsupported,
                            message: error.to_string(),
                        },
                    })
            })
            .and_then(|mut plan| {
                let limit = providers.write_grant_limit().checked();
                applied_write_grants(&plan, limit).map_err(|error| RunError::Input {
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

    pub(crate) fn retain_prepared_start(&self, ready: &mut PreparedStart, run: RunId) {
        ready.retained = ready.run == run;
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
    fn checked(self) -> Self {
        if (1..=provenant_records::run::MAX_WRITE_GRANT_BYTES_CEILING).contains(&self.bytes) {
            self
        } else {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: invalid domains.run.write_grant_limit_bytes; using compiled default"
            );
            Self::default()
        }
    }
    fn receipt(self) -> serde_json::Value {
        serde_json::json!({"key": "domains.run.write_grant_limit_bytes", "value": self.bytes, "origin": match self.origin { provenant_records::diagnostic::SettingSource::Configuration => "host configuration", provenant_records::diagnostic::SettingSource::ShippedDefault => "compiled default" }})
    }
}

pub trait Providers {
    /// Host cap for the encoded applied write-grant ledger.
    fn write_grant_limit(&self) -> WriteGrantLimit {
        WriteGrantLimit::default()
    }
    /// A host cap, in seconds, on the runtime deadline of lanes on `provider`.
    /// The task's own timeout applies when it is shorter or there is no cap.
    fn runtime_cap(&self, _provider: &str) -> Option<u64> {
        None
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
    fn check_policy(&self, _task: &TaskRequest) -> std::result::Result<(), PrepareRefusal> {
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
    ) -> std::result::Result<PreparedLaunch, PrepareRefusal>;
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
        run: RunId,
        attempt: AttemptId,
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
    InvalidWriteGrantPath,
    WriteGrantsLimit(provenant_records::diagnostic::ResourceFact),
    Handoff,
    HandoffBoundary(provenant_records::diagnostic::BoundaryClass),
    HandoffResource(provenant_records::diagnostic::ResourceFact),
    RetainedImport {
        run: RunId,
        attempt: AttemptId,
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
            Self::StoredPromptUnreadable => B::Authority,
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
        run: RunId,
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
            run,
            providers,
            context,
            acquire_writer,
            expected.map(CanonicalRequest::digest).transpose()?,
            call,
        )
    }
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
        let requester = self.run(run, call)?.actor_id;
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
            let record = self.record(id, call)?;
            if !matches!(record.state, AttemptState::Queued { .. }) {
                continue;
            }
            // Never hand a cloud attempt to the local supervisor, including
            // during recovery after the submitting process exits.
            if request
                .tasks
                .iter()
                .any(|task| task.task_id == record.task_id && task.cloud.is_some())
            {
                continue;
            }
            let now = || {
                call.clock
                    .wall_now()
                    .map_err(|_| RunError::Validation("wall clock"))
            };

            let (task_index, task) = request
                .tasks
                .iter()
                .enumerate()
                .find(|(_, task)| task.task_id == record.task_id)
                .ok_or(RunError::Validation("attempt task missing from request"))?;
            let effective_task = self.effective_launch_task(&request, task);
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
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Queued {
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
                temporary_alias: crate::short_temporary::ensure(&attempt),
            };
            let priority = providers.priority(task).unwrap_or_else(|reason| {
                let fallback = provenant_records::run::default_lane_priority(task.writer.is_some(), task.build_heavy);
                use std::io::Write;
                let _ = writeln!(std::io::stderr().lock(),
                    "warning: lane priority unavailable ({reason}); using compiled domains.fabric.lanes.priority default {fallback}");
                fallback
            });
            let prepared = providers
                .check_policy(task)
                .map(|()| priority)
                .map_err(|refusal| {
                    let kind = if let Some(fact) = refusal.resource {
                        RefusalKind::PolicyResource(fact)
                    } else if refusal.boundary_class
                        != provenant_records::diagnostic::BoundaryClass::None
                    {
                        RefusalKind::PolicyBoundary(refusal.boundary_class)
                    } else {
                        RefusalKind::Policy
                    };
                    (kind, Box::new(refusal))
                })
                .and_then(|priority| {
                    providers
                        .prepare(task, &prompt, &paths, resume.as_ref())
                        .map(|plan| (plan, priority))
                        .map_err(|reason| (reason.kind(), Box::new(reason)))
                });
            let (mut plan, priority) = match prepared {
                Ok(prepared) => prepared,
                Err((kind, refusal)) => {
                    if !refusal.skipped_routes.is_empty() {
                        self.record_skipped_routes(id, &refusal.skipped_routes, call)?;
                    }
                    let outcome = if refusal.auth_required() {
                        Outcome::AuthRequired
                    } else {
                        Outcome::Rejected
                    };
                    let terminal_cause =
                        refusal
                            .terminal_cause
                            .or(Some(if outcome == Outcome::AuthRequired {
                                TerminalCause::AccountLoginRequired
                            } else {
                                TerminalCause::PreparationRefused
                            }));
                    let terminal_note = match (kind.terminal_note(), refusal.terminal_note) {
                        (Some(guidance), Some(note)) if guidance != note => {
                            Some(format!("{note}; {guidance}"))
                        }
                        (Some(note), _) | (_, Some(note)) => Some(note),
                        (None, None) => refusal.skipped_routes.first().map(|route| {
                            let diagnostic = route.diagnostic();
                            format!(
                                "first candidate: {}; {}",
                                diagnostic.detail.unwrap_or_default(),
                                diagnostic.next.unwrap_or_default()
                            )
                        }),
                    };
                    let reason = refusal.message;
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Queued {
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
            let limit = providers.write_grant_limit().checked();
            plan.route
                .extra
                .insert("write_grant_limit".into(), limit.receipt());
            let write_grants = applied_write_grants(&plan, limit);
            let write_grants = match write_grants {
                Ok(grants) => grants,
                Err(error) => {
                    let kind = match error {
                        WriteGrantError::InvalidPath => RefusalKind::InvalidWriteGrantPath,
                    };
                    let reason = error.to_string();
                    match self.withdraw_with_preparation_failure(
                        id,
                        crate::store::Withdrawal {
                            expected: AttemptState::Queued {
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
            self.seal_work(&record, &plan, call)?;
            // Establish launcher liveness before publishing Starting. Reaping can
            // then distinguish a live pre-custody handoff from an abandoned one.
            let _launcher = match attempt.try_lock(
                std::ffi::OsStr::new("launcher.lock"),
                provenant_os::fs::LockMode::Exclusive,
            ) {
                Ok(guard) => guard,
                Err(provenant_os::fs::FsError::LockTimeout { .. }) => continue,
                Err(error) => return Err(error.into()),
            };
            let record = match self.claim_launch(
                &record,
                &plan,
                &write_grants,
                priority,
                runtime,
                call,
                &mut std::io::stderr().lock(),
            ) {
                Ok(record) => record,
                // A concurrent start already took this attempt.
                Err(RunError::Conflict) => continue,
                Err(error) => return Err(error),
            };
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
                            HandOff::RetainedImport {
                                run,
                                attempt,
                                worktree,
                            } => (
                                RunError::RetainedWriterImport {
                                    run,
                                    attempt,
                                    worktree: worktree.clone(),
                                }
                                .to_string(),
                                RefusalKind::RetainedImport {
                                    run,
                                    attempt,
                                    worktree,
                                },
                            ),
                            _ => unreachable!(),
                        };
                        // Cancellation decides the outcome, but an independent
                        // preparation failure remains evidence for the chair.
                        let cancelled = self.attempt_dir(run, id, false).is_ok_and(|attempt| {
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
                        self.launch_unknown(run, call)?;
                        Started::Unknown { attempt: id, error }
                    }
                    Err(HandOff::Protected(error)) => {
                        self.launch_blocked(run, &error, call)?;
                        Started::Blocked { attempt: id, error }
                    }
                },
            );
        }
        Ok(started)
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
    fn launch_blocked(&self, run: RunId, reason: &str, call: &Request<'_>) -> Result<()> {
        self.write(call, |tx| {
            let raw: Vec<u8> = tx.query_row(
                "SELECT record FROM operations WHERE run=?1 AND project=?2",
                params![run.to_string(), self.context.project_id.to_string()],
                |r| r.get(0),
            )?;
            let mut op: OperationRecord = decode(&raw)?;
            if op.state == OperationState::Terminal {
                return Err(RunError::Conflict);
            }
            op.state = OperationState::Blocked;
            op.blocked_reason = Some(reason.to_owned());
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

fn lane_port_plan_or_warn(state: &std::path::Path) -> Option<provenant_os::lane_ports::Plan> {
    match lane_port_plan(state) {
        Ok(plan) => plan,
        Err(error) => {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: lane port settings unavailable ({error}); continuing without a port block (domains.fabric.lanes.lane_port_base, domains.fabric.lanes.lane_port_block, domains.fabric.lanes.lane_port_blocks)"
            );
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
        assert!(lane_port_plan_or_warn(&not_directory).is_none());
        let defaults = lane_port_plan_or_warn(temp.path()).expect("default plan");
        assert_eq!(
            defaults.blocks_origin,
            Some(provenant_os::lane_ports::BlocksOrigin::CompiledDefault)
        );
        std::fs::write(
            temp.path().join("config.toml"),
            "schema_version=1\n[domains.fabric.lanes]\nlane_port_blocks=2\n",
        )?;
        let configured = lane_port_plan_or_warn(temp.path()).expect("configured plan");
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
    use provenant_core::{CancelToken, WorkScope};
    use std::path::Path;

    #[test]
    fn claimed_launch_reports_once_when_a_prepared_attempt_is_replayed()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let run = input();
        store.create(&run, &call)?;
        let expected = store.record(run.attempts[0].attempt_id, &call)?;
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
        let prepared = PreparedLaunch {
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
                cargo_registry_seed: false,
                state_root: "/fixture/state".into(),
            }),
        };
        let grants = applied_write_grants(&prepared, WriteGrantLimit::default())?;
        let mut output = Vec::new();
        let claimed =
            store.claim_launch(&expected, &prepared, &grants, 0, 60, &call, &mut output)?;
        assert!(matches!(claimed.state, AttemptState::Starting { .. }));
        // Another launcher can have prepared from the same queued record.
        // The compare-and-swap must refuse its stale claim before reporting.
        assert!(matches!(
            store.claim_launch(&expected, &prepared, &grants, 0, 60, &call, &mut output),
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
    use provenant_core::CancelToken;
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
        ) -> std::result::Result<PreparedLaunch, PrepareRefusal> {
            let mut refusal = PrepareRefusal::from("DEPENDENCY_OUTPUT_SECRET");
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
            let id = run.attempts[0].attempt_id;
            store.create(&run, &call)?;
            let started = store.start_inner(
                run.run_id,
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
