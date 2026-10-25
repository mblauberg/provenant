//! Run's composition edge: `dispatch`, `lanes`, `output`, `cancel`, `resume`,
//! `reconcile` and `telemetry stats`, and the provider seam that the
//! same-binary guardian holder shares.
use crate::{
    Cancel, Condition, Dispatch, Lanes, Mode, Output, Part, Reconcile, Resume, Stats, Switch,
    Target, Trace,
};
use provenant_core::{
    AttemptId, CancelToken, Clock, DeliveryId, OperationId, Roots, RunId, TaskId, WaitBudget,
    WallTime,
};
use provenant_providers::{OutcomeStatus, ProviderEvent, REGISTRY};
use provenant_records::{CanonicalV1, CommandError, CommandResult, Outcome as Status};
use provenant_routing::{BoundedModels, Fallback, ModelId, RouteChoice, RouteName};
use provenant_run as run;
use provenant_supervisor::{clock::SystemClock, custody::ExitObservation, fs::entropy_128};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod children;
mod provider;
mod remote;
pub use children::HostHolder;
pub(crate) use provider::resolve_executable as provider_executable;
#[cfg(feature = "fixture")]
pub use provider::{prepare_fixture, writer_fixture_plan};

const PROMPT_LIMIT: u64 = 1024 * 1024;

/// Paths the binary resolved for this invocation.
pub struct Edge<'a> {
    pub roots: &'a Roots,
    pub home: &'a Path,
    pub assets: &'a Path,
    pub cwd: &'a Path,
}
/// A rendered owner reply.
pub struct Reply {
    pub result: CommandResult<Value>,
    pub human: String,
}
/// A refused or failed owner call: envelope status, stable code and message.
pub struct Fail {
    pub status: Status,
    pub code: &'static str,
    pub message: String,
}
impl Fail {
    fn new(status: Status, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}
impl From<run::RunError> for Fail {
    fn from(error: run::RunError) -> Self {
        use run::RunError as E;
        let (status, code) = match &error {
            E::WorktreeMissing(_) => (Status::Invalid, "WORKTREE_MISSING"),
            E::WorktreeUnregistered(_) => (Status::Refused, "WORKTREE_UNREGISTERED"),
            E::Writer(_) | E::Confinement(_) => (Status::Refused, "REFUSED"),
            E::Validation(_) | E::TooLarge { .. } => (Status::Invalid, "INVALID"),
            E::OperationConflict => (Status::Invalid, "OPERATION_CONFLICT"),
            E::NotFound => (Status::Invalid, "NOT_FOUND"),
            E::Nonterminal => (Status::Refused, "NONTERMINAL"),
            E::Conflict => (Status::Refused, "CONFLICT"),
            E::Cancelled => (Status::Error, "CANCELLED"),
            E::Refused => (Status::Refused, "REFUSED"),
            E::DepthExceeded { .. } | E::DepthOverflow => (Status::Refused, "LANE_DEPTH"),
            E::Capacity => (Status::Refused, "QUEUE_FULL"),
            E::Contention => (Status::Error, "CONTENTION"),
            _ => (Status::Error, "IO"),
        };
        Self::new(status, code, error.to_string())
    }
}
/// A reply whose reader deadline expired still reports what it saw.
fn reply_waited(command: &str, outcome: Result<(Value, String, bool), Fail>) -> Reply {
    let mut expired = false;
    let mut out = reply(
        command,
        outcome.map(|(data, human, timed_out)| {
            expired = timed_out;
            (data, human)
        }),
    );
    if expired {
        out.result.status = Status::Timeout;
    }
    out
}
fn reply(command: &str, outcome: Result<(Value, String), Fail>) -> Reply {
    match outcome {
        Ok((data, human)) => Reply {
            result: CommandResult::new(command, Status::Ok, data),
            human,
        },
        Err(fail) => {
            let mut result = CommandResult::new(command, fail.status, Value::Null);
            result.error = Some(CommandError {
                code: fail.code.into(),
                message: fail.message,
                cause: None,
                repair: None,
            });
            Reply {
                result,
                human: String::new(),
            }
        }
    }
}

/// Production provider seam. Routing resolves the requested route and its
/// fallback; the first eligible provider launch specification is prepared.
#[derive(Default)]
pub struct Registry<'a> {
    pub roots: Option<&'a Roots>,
    pub home: Option<&'a Path>,
    pub assets: Option<&'a Path>,
    pub cwd: Option<&'a Path>,
    pub state_root: Option<&'a Path>,
    pub project_id: Option<provenant_core::ProjectId>,
    pub host: Option<&'a run::HostLaunchContext>,
}
impl Registry<'_> {
    /// Each adapter supports resume; the attempt must carry a session handle.
    pub fn continues_sessions(&self) -> bool {
        !REGISTRY.is_empty()
    }
    /// Refuse before the attempt's one continuation is spent.
    fn check_continuable(&self, prior: &run::AttemptRecord) -> Result<(), Fail> {
        let unsupported = |message: String| Fail::new(Status::Refused, "UNSUPPORTED", message);
        let adapter = prior
            .applied_route
            .as_ref()
            .and_then(|route| route.provider.as_deref())
            .ok_or_else(|| unsupported("the attempt launched no provider to resume".into()))?;
        if !REGISTRY.iter().any(|entry| entry.adapter.id() == adapter) {
            return Err(unsupported(format!("unknown provider {adapter}")));
        }
        if prior.session.is_none() {
            return Err(unsupported(
                "the attempt recorded no provider session handle (it predates handle capture or the provider never reported one); dispatch a new lane with the prior result as context".into(),
            ));
        }
        Ok(())
    }
}
impl run::Providers for Registry<'_> {
    fn prepare(
        &self,
        task: &run::TaskRequest,
        prompt: &str,
        paths: &run::LaunchPaths,
        resume: Option<&run::AttemptRecord>,
    ) -> Result<run::PreparedLaunch, String> {
        let prepared = provider::prepare(self, task, prompt, paths, resume)?;
        #[cfg(feature = "fixture")]
        let prepared = {
            let mut prepared = prepared;
            if self.host.is_some_and(|host| host.fixture_state_write)
                && let Some(confinement) = &mut prepared.confinement
            {
                confinement.profile.observations.insert(
                    provenant_confinement::permissions::FixtureOperation::StateWrite,
                    provenant_confinement::permissions::FixtureOutcome::Allowed,
                );
            }
            prepared
        };
        Ok(prepared)
    }

    fn decode(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
    ) -> Result<run::Decoded, String> {
        let failed = |reason: &str| run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: run::UsageRecord::default(),
            session: None,
            reason: Some(reason.into()),
        };
        let Some(adapter) = REGISTRY
            .iter()
            .find(|r| Some(r.adapter.id()) == route.provider.as_deref())
            .map(|r| r.adapter)
        else {
            return Ok(failed("route names no registered adapter"));
        };
        let Ok(mut state) = adapter.stream(None) else {
            return Ok(failed("provider stream is unavailable"));
        };
        let mut events = adapter.observe(&mut state, stdout).unwrap_or_default();
        let code = match exit.termination {
            Some(provenant_supervisor::exec::Termination::Exit(code)) => Some(code),
            _ => None,
        };
        let timed_out =
            exit.stop_reason == Some(provenant_supervisor::machine::StopReason::WallDeadline);
        let Ok(outcome) = adapter.finish(
            state,
            provenant_providers::ExitObservation { code, timed_out },
        ) else {
            return Ok(failed("provider outcome is malformed"));
        };
        events.extend(outcome.final_events().iter().cloned());
        let rollout = (adapter.id() == "codex").then(|| {
            outcome
                .session()
                .ok_or(provenant_providers::ProviderError::UnknownIdentity)
                .and_then(|thread| provider::observe_codex_rollout(provider_state, thread))
        });
        // The typed reason survives into the terminal record for diagnosis.
        let rollout_error = rollout
            .as_ref()
            .and_then(|observed| observed.as_ref().err())
            .map(|error| format!("Codex rollout identity: {error}"));
        let rollout = rollout.and_then(Result::ok);
        let observed_model = if adapter.id() == "codex" {
            // Missing or invalid rollout evidence cannot fall back to another source.
            rollout.as_ref().map(|turn| turn.model.as_str())
        } else {
            outcome.model()
        };
        let mut decoded = run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: true,
            observed_route: observed_model.map(|model| run::RouteRecord {
                provider: Some(adapter.id().into()),
                model: Some(if model.contains('/') {
                    model.into()
                } else {
                    format!("{}/{model}", adapter.id())
                }),
                effort: rollout
                    .as_ref()
                    .map_or_else(|| route.effort.clone(), |turn| turn.effort.clone()),
                account_label: route.account_label.clone(),
                account_key: route.account_key.clone(),
                source: Some(
                    if rollout.is_some() {
                        "codex_rollout"
                    } else {
                        "provider_stream"
                    }
                    .into(),
                ),
                pool_selection: None,
            }),
            usage: run::UsageRecord::default(),
            session: outcome.session().map(str::to_owned),
            reason: None,
        };
        for event in &events {
            match event {
                ProviderEvent::Text(text) => decoded.result.push_str(text),
                ProviderEvent::Usage { input, output } => {
                    decoded.usage.uncached_input = Some(*input);
                    decoded.usage.output_excluding_reasoning = Some(*output);
                    decoded.usage.source = Some("provider".into());
                }
                _ => {}
            }
        }
        let auth = events.contains(&ProviderEvent::AuthRequired);
        (decoded.outcome, decoded.reason) = match outcome.status() {
            OutcomeStatus::Success => (run::Outcome::Succeeded, None),
            OutcomeStatus::Refused if auth => (run::Outcome::AuthRequired, None),
            OutcomeStatus::Refused => (run::Outcome::Rejected, Some("provider refused".into())),
            OutcomeStatus::Failed => (run::Outcome::Failed, Some("provider failed".into())),
            OutcomeStatus::Partial => {
                decoded.complete = false;
                (
                    run::Outcome::Failed,
                    Some("provider output is partial".into()),
                )
            }
            OutcomeStatus::Malformed => {
                decoded.complete = false;
                (
                    run::Outcome::Failed,
                    Some("provider protocol is malformed".into()),
                )
            }
        };
        if decoded.reason.is_none() {
            decoded.reason = rollout_error;
        }
        if let Some(ProviderEvent::QuotaLimited { message, pool }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::QuotaLimited { .. }))
        {
            decoded.outcome = run::Outcome::QuotaLimited;
            let mut reason = provenant_confinement::redact::known_shapes(message.as_bytes())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| {
                    "provider quota exhausted (message redaction unavailable)".into()
                });
            let repair = if pool.as_deref() == Some("opencode-free") {
                "; free tier exhausted; use a paid route or wait for its allowance reset"
            } else {
                ""
            };
            let mut end = reason.len().min(8192 - repair.len());
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
            // Routing reads the shared quota store, which a lane can neither
            // read nor write: a nested refusal stays in the attempt reason
            // rather than landing in a child store nothing routes from.
            let lane = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some();
            if let Some(pool) = pool.as_deref().filter(|_| !lane) {
                if let Some(state_root) = provider_state.ancestors().nth(5) {
                    crate::quota::persist_provider_refusal(
                        state_root,
                        route,
                        Some(pool),
                        jiff::Timestamp::now().as_second(),
                    )
                    .map_err(|e| e.to_string())?;
                } else if self.roots.is_some() {
                    return Err("quota evidence has no state root".into());
                }
            }
            reason.push_str(repair);
            decoded.reason = Some(reason);
        }
        Ok(decoded)
    }
}
fn route_request(task: &run::TaskRequest) -> Result<provenant_routing::RouteRequest, String> {
    let model = |text: &str| ModelId::parse(text).map_err(|error| error.to_string());
    let (choice, effort) = match &task.route {
        run::RequestedRoute::Model {
            model: id, effort, ..
        } => (RouteChoice::Model(model(id)?), effort.clone()),
        run::RequestedRoute::Named { name, effort } => (
            RouteChoice::NamedRoute(RouteName::parse(name).map_err(|error| error.to_string())?),
            effort.clone(),
        ),
    };
    let fallback = if task.fallback.is_empty() {
        Fallback::None
    } else {
        Fallback::Models(
            BoundedModels::new(
                task.fallback
                    .iter()
                    .map(|r| model(r.model.as_deref().unwrap_or_default()))
                    .collect::<Result<_, _>>()?,
            )
            .map_err(|error| error.to_string())?,
        )
    };
    Ok(provenant_routing::RouteRequest {
        choice,
        effort,
        fallback,
        confidential: false,
        allow_free: false,
    })
}

/// An open store and the identity it is scoped to.
pub struct Session {
    pub store: run::RunStore,
    pub context: run::ExecutionContext,
}
fn request<'a>(clock: &'a SystemClock, cancel: &'a CancelToken) -> Result<run::Request<'a>, Fail> {
    Ok(run::Request {
        clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|error| Fail::new(Status::Error, "TIME", error.to_string()))?,
        cancel,
    })
}
/// Shared Fabric identity/config, isolated Run custody for a lane's children.
pub fn execution_state(
    roots: &Roots,
    home: &Path,
    assets: &Path,
) -> Result<provenant_core::StateRoot, Fail> {
    let Some(path) = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV) else {
        return Ok(roots.state.clone());
    };
    let mut inputs = provenant_core::RootInputs::new(home.into(), assets.into());
    inputs.state_dir = Some(path.into());
    let state = inputs
        .resolve(&provenant_supervisor::fs::LocalFs)
        .map_err(|error| Fail::new(Status::Refused, "LANE_CONTEXT", error.to_string()))?
        .state;
    if !state
        .as_path()
        .starts_with(roots.state.as_path().join("children"))
    {
        return Err(Fail::new(
            Status::Refused,
            "LANE_CONTEXT",
            "child Run store is outside the lane namespace",
        ));
    }
    Ok(state)
}

fn open(edge: &Edge<'_>, call: &run::Request<'_>) -> Result<Session, Fail> {
    let (roots, project_id, actor_id) =
        crate::fabric::identity(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, "IDENTITY", error.to_string()))?;
    let context = run::ExecutionContext {
        state_root: execution_state(&roots, edge.home, edge.assets)?,
        project_id,
        actor_id,
        read_prompts: true,
    };
    Ok(Session {
        store: run::RunStore::open(&context, call)?
            .with_writer_registry(edge.roots.state.as_path()),
        context,
    })
}
fn open_read_only(
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<(run::ExecutionContext, Option<run::RunStore>), Fail> {
    let (roots, project_id, actor_id) =
        crate::fabric::identity(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, "IDENTITY", error.to_string()))?;
    let context = run::ExecutionContext {
        state_root: execution_state(&roots, edge.home, edge.assets)?,
        project_id,
        actor_id,
        read_prompts: true,
    };
    let store = run::RunStore::open_read_only(&context, call)?;
    Ok((context, store))
}
fn now(clock: &dyn Clock) -> Result<WallTime, Fail> {
    clock
        .wall_now()
        .map_err(|error| Fail::new(Status::Error, "TIME", error.to_string()))
}
fn fresh<T>(make: fn([u8; 16]) -> T) -> Result<T, Fail> {
    entropy_128()
        .map(make)
        .map_err(|error| Fail::new(Status::Error, "ENTROPY", error.to_string()))
}

/// Strict manifest: `{schema_version:1,tasks:[...]}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u64,
    tasks: Vec<ManifestTask>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTask {
    task_id: Option<TaskId>,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    model: Option<String>,
    route: Option<String>,
    effort: Option<String>,
    #[serde(default)]
    fallback: Vec<String>,
    delivery_id: Option<DeliveryId>,
    deliver: Option<run::WriterDeliveryRequest>,
    account: Option<String>,
}
fn read_bounded(path: &Path) -> Result<String, Fail> {
    let invalid = |error: &dyn std::fmt::Display| {
        Fail::new(
            Status::Invalid,
            "INVALID",
            format!("{}: {error}", path.display()),
        )
    };
    // A FIFO or device would block or never end.
    if !std::fs::metadata(path).map_err(|e| invalid(&e))?.is_file() {
        return Err(invalid(&"not a regular file"));
    }
    read_text(std::fs::File::open(path).map_err(|e| invalid(&e))?)
}
fn read_text(source: impl Read) -> Result<String, Fail> {
    let mut bytes = Vec::new();
    source
        .take(PROMPT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    if bytes.len() as u64 > PROMPT_LIMIT {
        return Err(Fail::new(Status::Invalid, "INVALID", "input exceeds 1 MiB"));
    }
    String::from_utf8(bytes)
        .map_err(|_| Fail::new(Status::Invalid, "INVALID", "input is not UTF-8"))
}
/// Exactly one of `ADAPTER/ID` or a named route. `ADAPTER/ID@EFFORT`, the
/// form route receipts print, is accepted when no separate effort is given.
pub fn requested(
    model: Option<&str>,
    route: Option<&str>,
    effort: Option<String>,
) -> Result<run::RequestedRoute, Fail> {
    let (model, effort) = match model.and_then(|m| m.rsplit_once('@')) {
        Some(_) if effort.is_some() => {
            return Err(Fail::new(
                Status::Invalid,
                "INVALID",
                "give the effort once: ADAPTER/ID@EFFORT or --effort",
            ));
        }
        Some((model, suffix)) => (Some(model), Some(suffix.to_owned())),
        None => (model, effort),
    };
    match (model, route) {
        (Some(model), None) => {
            let id = ModelId::parse(model)
                .map_err(|_| Fail::new(Status::Invalid, "INVALID", "model must be ADAPTER/ID"))?;
            Ok(run::RequestedRoute::Model {
                provider: id.as_str().split('/').next().unwrap_or_default().into(),
                model: model.into(),
                effort,
            })
        }
        (None, Some(name)) => Ok(run::RequestedRoute::Named {
            name: name.into(),
            effort,
        }),
        _ => Err(Fail::new(
            Status::Invalid,
            "INVALID",
            "each task needs exactly one of model or route",
        )),
    }
}
fn fallback(models: &[String]) -> Result<Vec<run::RouteRecord>, Fail> {
    if models.len() > 2 {
        return Err(Fail::new(
            Status::Invalid,
            "INVALID",
            "at most two fallback models",
        ));
    }
    models
        .iter()
        .map(|model| {
            ModelId::parse(model).map_err(|_| {
                Fail::new(Status::Invalid, "INVALID", "fallback must be ADAPTER/ID")
            })?;
            Ok(run::RouteRecord {
                provider: model.split('/').next().map(Into::into),
                model: Some(model.clone()),
                effort: None,
                account_label: None,
                ..Default::default()
            })
        })
        .collect()
}
/// Task IDs default to a value derived from the operation, so replaying the
/// same operation reproduces the same canonical request.
pub fn derived_task(operation: &OperationId, index: usize) -> Result<TaskId, Fail> {
    let digest = CanonicalV1::record_digest("run-task-id", 1, &(operation.as_str(), index))
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(TaskId::from_bytes(bytes))
}

/// Refuse input that matches a secret pattern before any record exists. The
/// message never repeats the input.
fn scan(prompt: &str) -> Result<(), Fail> {
    provenant_confinement::disclosure::scan_snapshot(prompt.as_bytes(), &[]).map_err(|_| {
        Fail::new(
            Status::Refused,
            "SECRET_INPUT",
            "input matches a secret pattern; remove the secret and retry",
        )
    })
}
/// Worktree access is explicit. Submission resolves and verifies the path;
/// a worktree with read-only access is refused, never run read-only.
pub fn worktree_access(writer: bool, worktree: Option<&Path>) -> Result<(), Fail> {
    match worktree {
        None if !writer => Ok(()),
        None => Err(Fail::new(
            Status::Invalid,
            "WORKTREE_REQUIRED",
            "worktree-write access needs --worktree, or --branch and --base with a peer --host",
        )),
        Some(_) if !writer => Err(Fail::new(
            Status::Invalid,
            "WORKTREE_NOT_APPLICABLE",
            "--worktree requires --mode worktree-write",
        )),
        Some(_) => Ok(()),
    }
}

/// Resolve dispatch access flags into the sealed record. Roots must be
/// existing directories, stored canonically, and clear of the state root;
/// the confinement compiler refuses credential, broad and Git roots.
fn lane_access(access: &crate::Access, state_root: &Path) -> Result<run::LaneAccess, Fail> {
    let invalid = |message: String| Fail::new(Status::Invalid, "INVALID", message);
    let mut postgres = false;
    for name in &access.capability {
        match name.as_str() {
            "postgres" => postgres = true,
            other => {
                return Err(invalid(format!(
                    "unknown capability {other}; known: postgres (Rust and Node toolchains are granted to every lane)"
                )));
            }
        }
    }
    let writer = matches!(access.mode, Mode::WorktreeWrite);
    let network = match (access.network, postgres) {
        (Some(Switch::On), _) => Some(run::NetworkAccess::Open),
        (Some(Switch::Off), true) => Some(run::NetworkAccess::Local),
        (Some(Switch::Off), false) => Some(run::NetworkAccess::Provider),
        (None, true) if !writer => Some(run::NetworkAccess::Local),
        (None, _) => None,
    };
    // A first dispatch can precede the state root's creation; compare lexically then.
    let state = provenant_supervisor::fs::canonicalize(state_root)
        .unwrap_or_else(|_| state_root.to_path_buf());
    let resolve = |paths: &[PathBuf], flag: &str| {
        paths
            .iter()
            .map(|path| {
                let canonical = provenant_supervisor::fs::canonicalize(path)
                    .ok()
                    .filter(|canonical| canonical.is_dir())
                    .ok_or_else(|| {
                        invalid(format!(
                            "{flag} {} is not an existing directory",
                            path.display()
                        ))
                    })?;
                if canonical.starts_with(&state) || state.starts_with(&canonical) {
                    return Err(invalid(format!(
                        "{flag} {} overlaps the Provenant state root",
                        path.display()
                    )));
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, Fail>>()
    };
    Ok(run::LaneAccess {
        network,
        read_roots: resolve(&access.read_root, "--read-root")?,
        write_roots: resolve(&access.write_root, "--write-root")?,
    })
}

/// One task of a submission, from CLI flags, a manifest or Fabric.
pub struct Submitted {
    /// Fabric's route.task_class, retained separately from the resolved route.
    pub task_class: Option<String>,
    pub task_id: TaskId,
    pub prompt: String,
    pub route: run::RequestedRoute,
    pub account: Option<String>,
    pub fallback: Vec<String>,
    pub provider_timeout: u64,
    pub delivery_id: Option<DeliveryId>,
    pub deliver: Option<run::WriterDeliveryRequest>,
    pub worktree: Option<PathBuf>,
    pub access: run::LaneAccess,
}
pub struct Submission {
    pub operation_id: OperationId,
    pub tasks: Vec<Submitted>,
    pub concurrency: Option<u64>,
    pub knobs: Vec<String>,
}
/// Scan, record and launch work: the one path the CLI and Fabric
/// share. Replaying an operation returns its run and launches nothing new.
pub fn submit(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    let (lane_depth, parent_attempt) = nested_dispatch_context(edge.roots)?;
    let mut requests = Vec::new();
    let mut attempts = Vec::new();
    let config = provenant_config::read_with_domains(
        edge.roots,
        &provenant_supervisor::fs::LocalFs,
        &crate::domains::SECTIONS,
    )
    .map_err(|error| Fail::new(Status::Invalid, "CONFIG", error.to_string()))?;
    let profile = |class: Option<&str>| {
        config
            .dispatch_profile(class, &submission.knobs)
            .map_err(|error| Fail::new(Status::Invalid, "CONFIG", error.to_string()))
    };
    let concurrency = run::concurrency(submission.concurrency, &profile(None)?);
    for task in submission.tasks {
        scan(&task.prompt)?;
        // A lane's child launches outside the lane's sandbox, so it may not
        // name access its parent never had.
        if parent_attempt.is_some() && !task.access.is_default() {
            return Err(Fail::new(
                Status::Refused,
                "UNSUPPORTED",
                "a nested dispatch cannot set network, capability, read-root or write-root access",
            ));
        }
        if task.deliver.is_some() && task.worktree.is_none() {
            return Err(Fail::new(
                Status::Invalid,
                "INVALID",
                "delivery requires a writer worktree",
            ));
        }
        let writer = task
            .worktree
            .as_ref()
            .map(|path| {
                // A lane cannot read another worktree's registration; the host
                // verifies the proposal before it launches the child.
                let writer = if parent_attempt.is_some() {
                    run::resolve_writer_proposal(
                        session.context.state_root.as_path(),
                        edge.cwd,
                        path,
                    )?
                } else {
                    run::resolve_writer(session.context.state_root.as_path(), edge.cwd, path)?
                };
                if parent_attempt.is_none() {
                    run::prepare_writer(
                        &writer,
                        edge.roots.state.as_path(),
                        session.context.project_id,
                    )?;
                }
                Ok::<_, run::RunError>(writer)
            })
            .transpose()?;
        #[derive(serde::Serialize)]
        struct RunAuthority<'a> {
            writer: Option<&'a run::WriterRequest>,
            #[serde(skip_serializing_if = "Option::is_none")]
            deliver: Option<&'a run::WriterDeliveryRequest>,
            cwd: &'a Path,
            #[serde(skip_serializing_if = "run::LaneAccess::is_default")]
            access: &'a run::LaneAccess,
        }
        let authority = CanonicalV1::record_digest(
            "run-authority",
            2,
            &RunAuthority {
                writer: writer.as_ref(),
                deliver: task.deliver.as_ref(),
                cwd: edge.cwd,
                access: &task.access,
            },
        )
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;

        requests.push(run::TaskRequest {
            profile: profile(task.task_class.as_deref())?,
            task_id: task.task_id,
            prompt_digest: CanonicalV1::file_digest(task.prompt.as_bytes()),
            route: task.route,
            account: task.account,
            fallback: fallback(&task.fallback)?,
            authority_digest: authority,
            writer,
            provider_timeout_seconds: task.provider_timeout,
            delivery_id: task.delivery_id,
            deliver: task.deliver,
            access: task.access,
        });
        attempts.push(run::NewAttempt {
            attempt_id: fresh(AttemptId::from_bytes)?,
            task_id: task.task_id,
            prompt: task.prompt,
        });
    }
    let prepared = run::PreparedRun {
        run_id: fresh(RunId::from_bytes)?,
        operation_id: submission.operation_id,
        request: run::CanonicalRequest {
            schema_version: 1,
            tasks: requests,
            concurrency,
            parent: None,
            lane_depth: Some(lane_depth),
            parent_attempt,
        },
        attempts,
        created_at: now(call.clock)?,
    };
    let record = session.store.create(&prepared, call)?;
    let started = if parent_attempt.is_some() {
        Vec::new()
    } else {
        launch(session, record.run_id, edge, call)?
    };
    Ok((record, started))
}

pub fn dispatch(command: Dispatch, edge: &Edge<'_>) -> Reply {
    reply_waited("dispatch", dispatch_inner(command, edge))
}
fn dispatch_inner(mut command: Dispatch, edge: &Edge<'_>) -> Result<(Value, String, bool), Fail> {
    let access = &command.access;
    let unsupported = |what: &str| {
        Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            format!("{what} is not available in this build"),
        ))
    };
    worktree_access(
        matches!(access.mode, Mode::WorktreeWrite) && command.branch.is_none(),
        access.worktree.as_deref(),
    )?;
    let lane_access = lane_access(access, edge.roots.state.as_path())?;
    // Naming this host is ordinary local placement in any mode. A peer writer
    // names a new branch the peer creates, since --worktree is a local path.
    let mut new_worktree = None;
    if let Some(host) = command.host.as_deref() {
        if remote::names_local_host(edge, host)? {
            command.host = None;
        } else if matches!(command.access.mode, Mode::WorktreeWrite) {
            if !remote::names_peer(edge, host)? {
                return Err(remote::unknown_host(host));
            }
            new_worktree = Some(remote::new_worktree(&command)?);
        }
    }
    if command.branch.is_some() && new_worktree.is_none() {
        return Err(Fail::new(
            Status::Invalid,
            "INVALID",
            "--branch and --base create a writer worktree on a peer --host with \
             --mode worktree-write; on this host use `provenant worktree create` and --worktree",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let flag_route = (
        command.route.model.as_ref().map(|m| m.0.clone()),
        command.route.route.clone(),
        command.route.effort.clone(),
        command
            .route
            .fallback
            .iter()
            .map(|m| m.0.clone())
            .collect::<Vec<_>>(),
    );
    let mut tasks = Vec::new();
    let flag_delivery =
        command
            .deliver_remote
            .zip(command.deliver_branch)
            .map(|(remote, branch)| run::WriterDeliveryRequest {
                remote,
                branch,
                open_pr: command.open_pr,
            });
    if let Some(path) = &command.input.manifest {
        let manifest: Manifest = provenant_records::decode_v1(
            read_bounded(path)?.as_bytes(),
            provenant_records::Limits {
                max_bytes: PROMPT_LIMIT,
                ..Default::default()
            },
        )
        .map_err(|error| Fail::new(Status::Invalid, "INVALID", error.to_string()))?;
        if manifest.schema_version != 1 || !(1..=64).contains(&manifest.tasks.len()) {
            return Err(Fail::new(
                Status::Refused,
                "INVALID",
                "manifest needs schema_version 1 and 1-64 tasks",
            ));
        }
        let base = path.parent().unwrap_or(Path::new("."));
        for (index, task) in manifest.tasks.into_iter().enumerate() {
            let prompt = match (task.prompt, task.prompt_file) {
                (Some(prompt), None) => prompt,
                (None, Some(file))
                    if file
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))) =>
                {
                    read_bounded(&base.join(file))?
                }
                (None, Some(_)) => {
                    return Err(Fail::new(
                        Status::Invalid,
                        "INVALID",
                        "prompt_file must be relative to the manifest, without ..",
                    ));
                }
                _ => {
                    return Err(Fail::new(
                        Status::Invalid,
                        "INVALID",
                        "each task needs exactly one prompt or prompt_file",
                    ));
                }
            };
            let (model, route) = match (task.model, task.route) {
                (None, None) => (flag_route.0.clone(), flag_route.1.clone()),
                other => other,
            };
            let fallback = if task.fallback.is_empty() {
                flag_route.3.clone()
            } else {
                task.fallback
            };
            let task_id = match task.task_id {
                Some(id) => id,
                None => derived_task(&command.operation_id, index)?,
            };
            let effort = task.effort.or(flag_route.2.clone());
            tasks.push((
                task_id,
                prompt,
                requested(model.as_deref(), route.as_deref(), effort.clone())?,
                remote::RouteText {
                    model,
                    route,
                    effort,
                },
                fallback,
                task.delivery_id,
                task.account,
                task.deliver.or_else(|| flag_delivery.clone()),
            ));
        }
    } else {
        let prompt = match &command.input.prompt_file {
            Some(path) => read_bounded(path)?,
            None => read_text(std::io::stdin().lock())?,
        };
        let task_id = match command.task_id {
            Some(id) => id,
            None => derived_task(&command.operation_id, 0)?,
        };
        tasks.push((
            task_id,
            prompt,
            requested(
                flag_route.0.as_deref(),
                flag_route.1.as_deref(),
                flag_route.2.clone(),
            )?,
            remote::RouteText {
                model: flag_route.0.clone(),
                route: flag_route.1.clone(),
                effort: flag_route.2.clone(),
            },
            flag_route.3.clone(),
            command.delivery_id,
            None,
            flag_delivery,
        ));
    }
    let concurrency = if command.input.manifest.is_some() {
        command.concurrency
    } else {
        Some(1)
    };
    if let Some(host) = &command.host {
        let mut placed = Vec::new();
        for (index, (task_id, prompt, _, route, fallback, delivery_id, account, deliver)) in
            tasks.into_iter().enumerate()
        {
            // The peer derives task IDs from the operation and has no fallback field.
            if task_id != derived_task(&command.operation_id, index)? {
                return unsupported("an explicit task ID with --host");
            }
            if !fallback.is_empty() {
                return unsupported("route fallback with --host");
            }
            if !lane_access.is_default() {
                return unsupported(
                    "network, capability, read-root or write-root access with --host",
                );
            }
            placed.push(remote::Task {
                prompt,
                route,
                account,
                delivery_id,
                deliver,
            });
        }
        return remote::dispatch(
            edge,
            host,
            remote::Batch {
                operation_id: command.operation_id,
                tasks: placed,
                concurrency,
                knobs: command.knobs,
                provider_timeout: command.provider_timeout,
                new_worktree,
            },
            command.wait,
        );
    }
    let tasks = tasks
        .into_iter()
        .map(
            |(task_id, prompt, route, _, fallback, delivery_id, account, deliver)| Submitted {
                task_class: None,
                task_id,
                prompt,
                route,
                fallback,
                account,
                provider_timeout: command.provider_timeout,
                delivery_id,
                deliver,
                worktree: access.worktree.clone(),
                access: lane_access.clone(),
            },
        )
        .collect();
    let session = open(edge, &call)?;
    let (record, started) = submit(
        &session,
        edge,
        Submission {
            operation_id: command.operation_id,
            tasks,
            concurrency,
            knobs: command.knobs,
        },
        &call,
    )?;
    report(&session, &record, &started, command.wait, edge, &call)
}

fn nested_dispatch_context(roots: &Roots) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = environment_text(run::LANE_DEPTH_ENV)?;
    let parent_attempt = environment_text(run::PARENT_ATTEMPT_ENV)?;
    let config = provenant_config::read_with_domains(
        roots,
        &provenant_supervisor::fs::LocalFs,
        &crate::domains::SECTIONS,
    )
    .map_err(|error| Fail::new(Status::Error, "CONFIG", error.to_string()))?;
    let max_depth = config
        .knobs
        .get("lanes.max_depth")
        .and_then(|knob| knob.value.as_u64())
        .ok_or_else(|| {
            Fail::new(
                Status::Error,
                "CONFIG",
                "lanes.max_depth is missing or invalid",
            )
        })?;
    lane_dispatch_values(
        parent_depth.as_deref(),
        parent_attempt.as_deref(),
        max_depth,
        roots.state.as_path(),
        std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV)
            .as_deref()
            .map(Path::new),
    )
}

fn environment_text(name: &str) -> Result<Option<String>, Fail> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(Fail::new(
            Status::Invalid,
            "LANE_CONTEXT",
            "lane context environment value is not valid text",
        )),
    }
}

fn lane_dispatch_values(
    parent_depth: Option<&str>,
    parent_attempt: Option<&str>,
    max_depth: u64,
    state: &Path,
    run_state: Option<&Path>,
) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = parent_depth
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| {
            Fail::new(
                Status::Invalid,
                "LANE_CONTEXT",
                "lane depth environment value must be a non-negative integer",
            )
        })?;
    let parent_attempt = parent_attempt
        .map(AttemptId::parse)
        .transpose()
        .map_err(|_| {
            Fail::new(
                Status::Invalid,
                "PARENT_ATTEMPT",
                "parent attempt environment value is invalid",
            )
        })?;
    if let Some(run_state) = run_state {
        let invalid = || {
            Fail::new(
                Status::Refused,
                "LANE_CONTEXT",
                "lane environment must match its child Run store path",
            )
        };
        let relative = run_state.strip_prefix(state).map_err(|_| invalid())?;
        let mut parts = relative.components();
        let mut depth = 0;
        let mut parent = None;
        while let Some(part) = parts.next() {
            if part.as_os_str() != "children" {
                return Err(invalid());
            }
            let id = parts
                .next()
                .and_then(|part| part.as_os_str().to_str())
                .ok_or_else(invalid)?;
            parent = Some(AttemptId::parse(id).map_err(|_| invalid())?);
            depth += 1;
        }
        if depth == 0 || parent_depth != Some(depth) || parent_attempt != parent {
            return Err(invalid());
        }
        return Ok((run::next_lane_depth(depth, max_depth)?, parent));
    }
    let parent_depth = match (parent_depth, parent_attempt) {
        (None, None) => 0,
        (Some(depth), Some(_)) if depth >= 1 => depth,
        (Some(_), Some(_)) => {
            return Err(Fail::new(
                Status::Refused,
                "LANE_CONTEXT",
                "nested lane context requires a depth of at least 1",
            ));
        }
        _ => {
            return Err(Fail::new(
                Status::Refused,
                "LANE_CONTEXT",
                "lane depth and parent attempt must be supplied together",
            ));
        }
    };
    let lane_depth = run::next_lane_depth(parent_depth, max_depth)?;
    Ok((lane_depth, parent_attempt))
}

/// Hand every queued attempt of `run` to its own guardian. Fabric's facade
/// shares this path with the CLI leaves.
pub fn launch(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<Vec<run::Started>, Fail> {
    // Every lane-facing launch path is submission-only. Only its host holder
    // prepares providers, including credentials and the child's OS profile.
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Ok(Vec::new());
    }
    launch_on_host(session, run, edge, call, None)
}
fn launch_on_host(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    inherited: Option<&run::HostLaunchContext>,
) -> Result<Vec<run::Started>, Fail> {
    let host = match inherited {
        Some(host) => host.clone(),
        None => run::HostLaunchContext {
            configuration_root: edge.roots.state.as_path().into(),
            cwd: edge.cwd.into(),
            executables: REGISTRY
                .iter()
                .map(|entry| entry.adapter.id())
                .filter_map(|name| {
                    provider::resolve_executable(name)
                        .ok()
                        .map(|path| (name.into(), path))
                })
                .collect(),
            git: provenant_supervisor::git::git_executable()
                .ok_or_else(|| Fail::new(Status::Refused, "GIT", "Git unavailable"))?,
            codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        },
    };
    let submission = session.store.request(run, call)?;
    let requester = session.store.run(run, call)?.actor_id;
    let seed = run::RootSeed {
        home: edge.home.into(),
        checkout: edge.assets.into(),
        state_root: session.context.state_root.as_path().into(),
    };
    let request = session.store.request(run, call)?;
    let holder = |attempt: &run::AttemptRecord| {
        let mut host = host.clone();
        if let Some(writer) = submission
            .tasks
            .iter()
            .find(|task| task.task_id == attempt.task_id)
            .and_then(|task| task.writer.as_ref())
        {
            host.cwd = writer.worktree.clone();
        }
        run::HolderContext {
            schema_version: 1,
            roots: seed.clone(),
            project_id: attempt.project_id,
            actor_id: requester,
            run_id: attempt.run_id,
            attempt_id: attempt.attempt_id,
            host_delivery: request
                .tasks
                .iter()
                .find(|task| task.task_id == attempt.task_id)
                .and_then(|task| task.deliver.as_ref())
                .map(|_| run::HostDeliveryContext::capture()),
            launch: Some(host),
        }
        .encode()
    };
    let registry = Registry {
        roots: Some(edge.roots),
        home: Some(edge.home),
        assets: Some(edge.assets),
        cwd: Some(edge.cwd),
        state_root: Some(session.context.state_root.as_path()),
        project_id: Some(session.context.project_id),
        host: Some(&host),
    };
    // Fixture guardians observe a recorded boot key, so the reservation must be
    // taken against the same key as on the lane-launch path of the test host.
    #[cfg(feature = "fixture")]
    if host.fixture_state_write {
        return Ok(session.store.start_with_writer_lease(
            run,
            &registry,
            &holder,
            &|directory, scope, binding| {
                provenant_supervisor::fixture::acquire_writer_with_binding(
                    directory,
                    scope,
                    binding,
                    provenant_core::WaitBudget::nonblocking(call.clock.mono_now()),
                    call.cancel,
                    call.clock,
                )
            },
            call,
        )?);
    }
    Ok(session.store.start(run, &registry, &holder, call)?)
}
fn launch_unknown(started: &[run::Started]) -> Vec<String> {
    started
        .iter()
        .filter_map(|s| match s {
            run::Started::Unknown { attempt, error } => Some(format!("{attempt}: {error}")),
            _ => None,
        })
        .collect()
}
/// The lanes of a just-accepted run, optionally after waiting for all of them.
fn report(
    session: &Session,
    record: &run::RunRecord,
    started: &[run::Started],
    wait: Option<u64>,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<(Value, String, bool), Fail> {
    let target = Target {
        run: record.run_id,
        task: None,
        attempt: None,
    };
    let (mut data, human, timed_out) = match wait {
        Some(seconds) => wait_lanes(session, &[target], Condition::All, seconds, edge)?,
        None => {
            let page = session
                .store
                .lanes(&lane_query(&[target], true, 100, 0), call)?;
            let (data, human) =
                render_lanes(&page.lanes, None, session.context.state_root.as_path());
            (data, human, false)
        }
    };
    data["run_id"] = json!(record.run_id);
    data["operation_id"] = json!(record.operation_id);
    let unknown = launch_unknown(started);
    if !unknown.is_empty() {
        data["launch_unknown"] = json!(unknown);
    }
    Ok((data, format!("run {}\n{human}", record.run_id), timed_out))
}

fn lane_query(targets: &[Target], all: bool, limit: u64, after: u64) -> run::LaneQuery {
    run::LaneQuery {
        targets: targets
            .iter()
            .map(|t| run::LaneTarget {
                run: t.run,
                task: t.task,
                attempt: t.attempt,
            })
            .collect(),
        all,
        limit,
        after,
        newest_first: false,
        owner: None,
        unseen_after: None,
    }
}
/// Artifact paths live under the session's execution root, which a nested
/// lane keeps apart from the global configuration root.
fn artifact(root: &Path, record: &run::AttemptRecord, name: &str) -> PathBuf {
    root.join(format!(
        "runs/{}/attempts/{}/{name}",
        record.run_id, record.attempt_id
    ))
}
fn state_name(state: &run::AttemptState) -> String {
    match state {
        run::AttemptState::Terminal { outcome, .. } => serde_json::to_value(outcome)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default(),
    }
}
fn render_lanes(lanes: &[run::AttemptRecord], next: Option<u64>, root: &Path) -> (Value, String) {
    let mut human = String::new();
    let rows: Vec<Value> = lanes
        .iter()
        .map(|lane| {
            let state = state_name(&lane.state);
            let route = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.model.clone())
                .unwrap_or_else(|| "unresolved".into());
            let (exit_code, finished_at, reason) = match &lane.state {
                run::AttemptState::Terminal {
                    exit_code,
                    finished_at,
                    reason,
                    ..
                } => (*exit_code, Some(*finished_at), reason.clone()),
                _ => (None, None, None),
            };
            let result = lane
                .result
                .as_ref()
                .map(|_| artifact(root, lane, "result.md"));
            let account = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.account_label.as_deref())
                .unwrap_or("unknown");
            human.push_str(&format!(
                "{}/{}/{} {state} {route} account={account}{}\n",
                lane.run_id,
                lane.task_id,
                lane.attempt_id,
                reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ));
            for skipped in &lane.skipped_routes {
                human.push_str(&format!(
                    "  skipped {} {:?}: {}\n",
                    skipped.route, skipped.kind, skipped.detail
                ));
            }
            if let Some(delivery) = &lane.delivery {
                human.push_str(&format!(
                    "  delivery {}:{} {:?} head={}\n",
                    delivery.remote,
                    delivery.branch,
                    delivery.state,
                    delivery
                        .head
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ));
            }
            json!({
                "run_id": lane.run_id,
                "task_id": lane.task_id,
                "attempt_id": lane.attempt_id,
                "attempt_number": lane.attempt_number,
                "state": state,
                "exit_code": exit_code,
                "reason": reason,
                "requested_route": lane.requested_route,
                "applied_route": lane.applied_route,
                "skipped_routes": lane.skipped_routes,
                "account": account,
                "observed_route": lane.observed_route,
                "queued_at": lane.queued_at,
                "started_at": lane.started_at,
                "finished_at": finished_at,
                "last_output_at": lane.last_output_at,
                "delivery_id": lane.delivery_id,
                "head": lane.head,
                "delivery": lane.delivery,
                "result_path": result,
            })
        })
        .collect();
    if rows.is_empty() {
        human.push_str("no matching lanes\n");
    }
    (
        json!({ "lanes": rows, "next_cursor": next.map(|n| n.to_string()) }),
        human,
    )
}
fn settled(state: &run::AttemptState) -> (bool, bool) {
    match state {
        run::AttemptState::Terminal { outcome, .. } => {
            (true, *outcome == run::Outcome::InputRequired)
        }
        _ => (false, false),
    }
}
/// Whether a lane selection satisfies a wait condition. Input required ends
/// an all-lanes wait because it cannot progress unattended.
fn condition_met(lanes: &[run::AttemptRecord], condition: Condition) -> bool {
    let states: Vec<(bool, bool)> = lanes.iter().map(|l| settled(&l.state)).collect();
    states.is_empty()
        || match condition {
            Condition::Any => states.iter().any(|(done, _)| *done),
            Condition::All => {
                states.iter().all(|(done, _)| *done) || states.iter().any(|(_, input)| *input)
            }
        }
}
/// Snapshot every attempt the targets name. Without targets, the selection is
/// this actor's nonterminal work; host caps bound it. With `seen`, terminal
/// lanes at or before that event cursor are already acknowledged and dropped.
fn select(
    session: &Session,
    targets: &[Target],
    seen: Option<u64>,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let mut selected = Vec::new();
    let mut after = 0;
    loop {
        let mut query = lane_query(targets, !targets.is_empty() || seen.is_some(), 100, after);
        query.owner = targets.is_empty().then_some(session.context.actor_id);
        query.unseen_after = seen;
        let page = session.store.lanes(&query, call)?;
        selected.extend(page.lanes.iter().map(|lane| lane.attempt_id));
        match page.next {
            Some(next) if selected.len() < 1000 => after = next,
            Some(_) => {
                return Err(Fail::new(
                    Status::Invalid,
                    "INVALID",
                    "selection exceeds 1000 lanes",
                ));
            }
            None => return Ok(selected),
        }
    }
}
/// The seen-cursor key for a wait selection.
fn selection_key(targets: &[Target]) -> String {
    let mut keys: Vec<String> = targets
        .iter()
        .map(|t| {
            let part = |v: Option<String>| v.unwrap_or_else(|| "*".into());
            format!(
                "{}/{}/{}",
                t.run,
                part(t.task.map(|v| v.to_string())),
                part(t.attempt.map(|v| v.to_string()))
            )
        })
        .collect();
    keys.sort();
    if keys.is_empty() {
        "default".into()
    } else {
        keys.join(",")
    }
}
/// Snapshot the selection at entry and poll until the condition holds or the
/// reader deadline expires. Expiry never stops a provider. A default or `Any`
/// wait skips terminal lanes the actor has already seen and acknowledges the
/// ones it returns, so repeated waits neither miss nor replay a completion.
fn wait_lanes(
    session: &Session,
    targets: &[Target],
    condition: Condition,
    seconds: u64,
    edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    // Explicit `All` waits report the whole named set, seen or not.
    let selection =
        (targets.is_empty() || matches!(condition, Condition::Any)).then(|| selection_key(targets));
    let entry = request(&clock, &cancel)?;
    let seen = selection
        .as_deref()
        .map(|key| session.store.seen_cursor(key, targets.is_empty(), &entry))
        .transpose()?;
    let selected = select(session, targets, seen, &entry)?;
    loop {
        let call = request(&clock, &cancel)?;
        let lanes = selected
            .iter()
            .map(|id| session.store.record(*id, &call))
            .collect::<Result<Vec<_>, _>>()?;
        let met = condition_met(&lanes, condition);
        let expired = Instant::now() >= deadline;
        if met || expired {
            if let Some(key) = selection.as_deref().filter(|_| met) {
                let mut latest = 0;
                for lane in lanes.iter().filter(|l| settled(&l.state).0) {
                    latest = latest.max(session.store.last_event_sequence(lane.attempt_id, &call)?);
                }
                if latest > 0 {
                    open(edge, &call)?.store.ack_seen(key, latest, &call)?;
                }
            }
            let (mut data, human) =
                render_lanes(&lanes, None, session.context.state_root.as_path());
            data["condition_met"] = json!(met);
            return Ok((data, human, !met));
        }
        std::thread::park_timeout(
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub fn lanes(command: Lanes, edge: &Edge<'_>) -> Reply {
    reply_waited(
        "lanes",
        (|| {
            if let Some(placed) = remote::lanes(&command, edge)? {
                return Ok(placed);
            }
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let (context, store) = open_read_only(edge, &call)?;
            let Some(store) = store else {
                let root = execution_state(edge.roots, edge.home, edge.assets)?;
                let (data, human) = render_lanes(&[], None, root.as_path());
                return Ok((data, human, false));
            };
            let session = Session { store, context };
            if let Some(seconds) = command.wait {
                return wait_lanes(&session, &command.id, command.condition, seconds, edge);
            }
            let after = match &command.cursor {
                Some(cursor) => cursor
                    .parse()
                    .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid cursor"))?,
                None => 0,
            };
            let mut query = lane_query(&command.id, command.all, command.limit, after);
            let filtered = !command.states.is_empty() || command.since.is_some();
            query.newest_first = filtered;
            let threshold = command
                .since
                .map(|since| WallTime::from_micros(since.as_microsecond()))
                .transpose()
                .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid --since value"))?;
            let (lanes, next) = if filtered {
                query.limit = 100;
                let mut matches = Vec::new();
                let mut next = None;
                loop {
                    let page = session.store.lanes(&query, &call)?;
                    for lane in page.lanes {
                        let state_matches = command.states.is_empty()
                            || command
                                .states
                                .iter()
                                .any(|state| state.as_str() == state_name(&lane.state));
                        let since_matches = threshold.is_none_or(|since| lane.queued_at >= since);
                        if state_matches && since_matches {
                            matches.push(lane);
                        }
                        if matches.len() > command.limit as usize {
                            let last =
                                matches.get(command.limit as usize - 1).ok_or_else(|| {
                                    Fail::new(Status::Error, "INTERNAL", "lane page invariant")
                                })?;
                            next = Some(session.store.attempt_sequence(last.attempt_id, &call)?);
                            matches.truncate(command.limit as usize);
                            break;
                        }
                    }
                    if next.is_some() {
                        break;
                    }
                    match page.next {
                        Some(cursor) => query.after = cursor,
                        None => break,
                    }
                }
                (matches, next)
            } else {
                let page = session.store.lanes(&query, &call)?;
                (page.lanes, page.next)
            };
            let (data, human) = render_lanes(&lanes, next, session.context.state_root.as_path());
            Ok((data, human, false))
        })(),
    )
}

pub fn output(command: Output, edge: &Edge<'_>) -> Reply {
    reply("output", output_inner(command, edge))
}
fn output_inner(command: Output, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    if let Some(placement) = remote::find(edge.roots, command.attempt.0.run)? {
        return remote::output(&command, &placement, edge);
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (context, store) = open_read_only(edge, &call)?;
    let store = store.ok_or(Fail::new(
        Status::Invalid,
        "NOT_FOUND",
        "run store is not initialised",
    ))?;
    let session = Session { store, context };
    let target = command.attempt.0;
    let id = target
        .attempt
        .ok_or(Fail::new(Status::Invalid, "INVALID", "attempt required"))?;
    let record = session.store.record(id, &call)?;
    if record.run_id != target.run || target.task.is_some_and(|task| task != record.task_id) {
        return Err(Fail::new(
            Status::Invalid,
            "NOT_FOUND",
            "attempt is not in that run/task",
        ));
    }
    let part = match command.part {
        Part::Result => run::ArtifactPart::Result,
        Part::Prompt => run::ArtifactPart::Prompt,
        Part::Stderr => run::ArtifactPart::Stderr,
        Part::Events => {
            // An attempt has a handful of revisions; offset is an event cursor.
            let events = session.store.events(command.offset, 64, Some(id), &call)?;
            let human = events
                .iter()
                .map(|e| format!("{} {}\n", e.revision, state_name(&e.change)))
                .collect();
            return Ok((json!({ "attempt_id": id, "events": events }), human));
        }
    };
    let mut range = run::ByteRange {
        offset: command.offset,
        max_bytes: command.max_bytes,
    };
    let mut chunk = session
        .store
        .read_output(id, part, range, &session.context, &call)?;
    if let Some(lines) = command.tail {
        range.offset = chunk.len.saturating_sub(command.max_bytes);
        chunk = session
            .store
            .read_output(id, part, range, &session.context, &call)?;
        let text = &chunk.bytes;
        let body = text.strip_suffix(b"\n").unwrap_or(text);
        let start = body
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, b)| **b == b'\n')
            .nth(usize::try_from(lines - 1).unwrap_or(usize::MAX))
            .map_or(0, |(i, _)| i + 1);
        chunk.offset += start as u64;
        chunk.bytes.drain(..start);
    }
    align_utf8_tail(&mut chunk);
    let text = String::from_utf8_lossy(&chunk.bytes).into_owned();
    Ok((
        json!({
            "attempt_id": id,
            "part": format!("{:?}", command.part).to_lowercase(),
            "offset": chunk.offset,
            "next_offset": chunk.next_offset,
            "length": chunk.len,
            "eof": chunk.eof,
            "clipped": chunk.clipped,
            "complete": chunk.complete,
            "encoding": chunk.encoding,
            "digest": chunk.digest,
            "text": text,
        }),
        text,
    ))
}

/// A clipped page that ends inside a UTF-8 character stops before it, so the
/// next offset starts the character whole and paged text concatenates exactly.
fn align_utf8_tail(chunk: &mut run::OutputChunk) {
    if !chunk.clipped {
        return;
    }
    let Err(error) = std::str::from_utf8(&chunk.bytes) else {
        return;
    };
    let valid = error.valid_up_to();
    if error.error_len().is_none() && valid > 0 {
        chunk.next_offset -= (chunk.bytes.len() - valid) as u64;
        chunk.bytes.truncate(valid);
        chunk.encoding = "utf8";
    }
}

/// Cancel every attempt the target names. Queued work ends at once; started
/// work gets a stop request its guardian acts on. Requested is not exit.
pub fn cancel(command: Cancel, edge: &Edge<'_>) -> Reply {
    match remote::find(edge.roots, command.target.run) {
        Ok(Some(placement)) => return remote::cancel(&command, &placement, edge),
        Ok(None) => {}
        Err(fail) => return reply("cancel", Err(fail)),
    }
    let mut status = Status::Ok;
    let mut out = reply(
        "cancel",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let session = open(edge, &call)?;
            let reason = command.reason.as_deref().unwrap_or("cancelled by request");
            let targets = [command.target];
            let selected = select(&session, &targets, None, &call)?;
            if selected.is_empty() {
                return Err(Fail::new(
                    Status::Invalid,
                    "NOT_FOUND",
                    "no matching attempts",
                ));
            }
            let target = &targets[0];
            let bound = match (target.attempt, target.task) {
                (Some(attempt), _) => attempt.to_string(),
                (None, Some(task)) => format!("{}/{task}", target.run),
                (None, None) => target.run.to_string(),
            };
            session
                .store
                .bind_cancel(&command.operation_id, &bound, &call)?;
            let mut receipts = Vec::new();
            let mut human = String::new();
            let mut requested = false;
            for id in selected {
                // One failure must not hide the receipts already earned.
                let receipt = request(&clock, &cancel)
                    .and_then(|call| Ok(session.store.cancel(id, reason, &call)?));
                let row = match receipt {
                    Ok(receipt) => {
                        if matches!(receipt, run::CancelReceipt::Unobservable { .. }) {
                            status = Status::Failed;
                        }
                        requested |= matches!(
                            receipt,
                            run::CancelReceipt::Requested | run::CancelReceipt::AlreadyRequested
                        );
                        human.push_str(&format!("{id} {}\n", state_tag(&receipt)));
                        json!({ "attempt_id": id, "receipt": receipt })
                    }
                    Err(fail) => {
                        status = Status::Failed;
                        human.push_str(&format!("{id} error: {}\n", fail.message));
                        json!({ "attempt_id": id, "error": { "code": fail.code, "message": fail.message } })
                    }
                };
                receipts.push(row);
            }
            let mut data = json!({ "operation_id": command.operation_id, "receipts": receipts });
            if requested {
                // A request has no actor once the guardian is gone, so recover
                // the run now: ending proven orphans and settling is the same
                // path `reconcile` takes, and a live guardian is left alone.
                let registry = Registry {
                    roots: Some(edge.roots),
                    home: Some(edge.home),
                    assets: Some(edge.assets),
                    cwd: Some(edge.cwd),
                    state_root: Some(session.context.state_root.as_path()),
                    project_id: Some(session.context.project_id),
                    host: None,
                };
                let recovered = request(&clock, &cancel)
                    .and_then(|call| Ok(session.store.reconcile(target.run, &registry, &call)?));
                match recovered {
                    Ok(found) => {
                        let mut rows = Vec::new();
                        for (id, state) in &found {
                            if matches!(
                                state,
                                run::Reconciled::Uncertain { .. } | run::Reconciled::Escaped { .. }
                            ) {
                                status = Status::Failed;
                            }
                            human.push_str(&format!("{id} {}\n", state_tag(state)));
                            rows.push(json!({ "attempt_id": id, "found": state }));
                        }
                        data["recovered"] = Value::Array(rows);
                    }
                    Err(fail) => {
                        status = Status::Failed;
                        human.push_str(&format!("recovery error: {}\n", fail.message));
                    }
                }
            }
            if let Some(seconds) = command.wait {
                let (lanes, rendered, timed_out) =
                    wait_lanes(&session, &targets, Condition::All, seconds, edge)?;
                if timed_out && status == Status::Ok {
                    status = Status::Timeout;
                }
                data["lanes"] = lanes["lanes"].clone();
                data["condition_met"] = lanes["condition_met"].clone();
                human.push_str(&rendered);
            }
            Ok((data, human))
        })(),
    );
    if out.result.status == Status::Ok {
        out.result.status = status;
    }
    out
}
fn state_tag(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default()
}

/// Continue one terminal attempt with new input as a new single-task run on
/// the same route and access.
pub fn resume(command: Resume, edge: &Edge<'_>) -> Reply {
    reply_waited("resume", resume_inner(command, edge))
}
/// Continue one terminal attempt, scanned and launched on the shared path.
/// `expected_generation` counts the attempt's prior continuations.
pub fn continue_attempt(
    session: &Session,
    edge: &Edge<'_>,
    prior: AttemptId,
    operation_id: OperationId,
    prompt: String,
    expected_generation: u64,
    call: &run::Request<'_>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    scan(&prompt)?;
    (Registry::default()).check_continuable(&session.store.record(prior, call)?)?;
    let run = session.store.resume(
        &run::Resume {
            prior,
            expected_generation,
            operation_id,
            run_id: fresh(RunId::from_bytes)?,
            attempt_id: fresh(AttemptId::from_bytes)?,
            prompt,
            created_at: now(call.clock)?,
        },
        call,
    )?;
    let started = launch(session, run.run_id, edge, call)?;
    Ok((run, started))
}
fn resume_inner(command: Resume, edge: &Edge<'_>) -> Result<(Value, String, bool), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let target = command.attempt.0;
    let prior = target
        .attempt
        .ok_or(Fail::new(Status::Invalid, "INVALID", "attempt required"))?;
    let prompt = match &command.prompt.prompt_file {
        Some(path) => read_bounded(path)?,
        None => read_text(std::io::stdin().lock())?,
    };
    let session = open(edge, &call)?;
    let record = session.store.record(prior, &call)?;
    if record.run_id != target.run || target.task.is_some_and(|task| task != record.task_id) {
        return Err(Fail::new(
            Status::Invalid,
            "NOT_FOUND",
            "attempt is not in that run/task",
        ));
    }
    // The CLI continues an attempt once; a second continuation of the same
    // attempt is a conflict, not a new branch.
    let (run, started) = continue_attempt(
        &session,
        edge,
        prior,
        command.operation_id,
        prompt,
        0,
        &call,
    )?;
    let (mut data, human, timed_out) = report(&session, &run, &started, command.wait, edge, &call)?;
    data["resumed_from"] = json!(prior);
    Ok((data, human, timed_out))
}

/// Recover runs whose guardian or submitter died: commit durable exits,
/// withdraw launches that never reached a guardian, and start queued work.
/// Uncertain custody stays fenced, is reported, and fails the command.
pub fn reconcile(command: Reconcile, edge: &Edge<'_>) -> Reply {
    let mut fenced = false;
    let mut out = reply("reconcile", reconcile_inner(command, edge, &mut fenced));
    if fenced && out.result.status == Status::Ok {
        out.result.status = Status::Failed;
    }
    out
}
const SCAN: u64 = 320;
fn reconcile_inner(
    command: Reconcile,
    edge: &Edge<'_>,
    fenced: &mut bool,
) -> Result<(Value, String), Fail> {
    if command.abandon_uncertain {
        return Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            "abandoning uncertain custody is not available in this build",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let session = open(edge, &request(&clock, &cancel)?)?;
    let (runs, truncated) = if command.id.is_empty() {
        let mut runs = session
            .store
            .open_runs(SCAN + 1, &request(&clock, &cancel)?)?;
        let truncated = runs.len() as u64 > SCAN;
        runs.truncate(SCAN as usize);
        (runs, truncated)
    } else {
        (command.id, false)
    };
    let registry = Registry {
        roots: Some(edge.roots),
        home: Some(edge.home),
        assets: Some(edge.assets),
        cwd: Some(edge.cwd),
        state_root: Some(session.context.state_root.as_path()),
        project_id: Some(session.context.project_id),
        host: None,
    };
    let mut rows = Vec::new();
    let mut human = String::new();
    for run in runs {
        // Each step gets a fresh budget; recovery waits on custody locks.
        let found = match request(&clock, &cancel)
            .and_then(|call| Ok(session.store.reconcile(run, &registry, &call)?))
        {
            Ok(found) => found,
            Err(fail) => {
                *fenced = true;
                human.push_str(&format!("{run} error: {}\n", fail.message));
                rows.push(json!({ "run_id": run, "error": { "code": fail.code, "message": fail.message } }));
                continue;
            }
        };
        let started = if found.iter().any(|(_, r)| *r == run::Reconciled::Queued) {
            request(&clock, &cancel).and_then(|call| launch(&session, run, edge, &call))
        } else {
            Ok(Vec::new())
        };
        let (launched, unknown, launch_error) = match &started {
            Ok(started) => (
                started
                    .iter()
                    .filter(|s| matches!(s, run::Started::Launched(_)))
                    .count(),
                launch_unknown(started),
                None,
            ),
            Err(fail) => (0, Vec::new(), Some(fail.message.clone())),
        };
        let attempts: Vec<Value> = found
            .iter()
            .map(|(id, state)| {
                if matches!(
                    state,
                    run::Reconciled::Uncertain { .. } | run::Reconciled::Escaped { .. }
                ) {
                    *fenced = true;
                }
                human.push_str(&format!("{run}/{id} {}\n", state_tag(state)));
                json!({ "attempt_id": id, "found": state })
            })
            .collect();
        *fenced |= !unknown.is_empty() || launch_error.is_some();
        rows.push(json!({
            "run_id": run,
            "attempts": attempts,
            "launched": launched,
            "launch_unknown": unknown,
            "launch_error": launch_error,
        }));
    }
    if rows.is_empty() {
        human.push_str("no open runs\n");
    }
    if truncated {
        human.push_str(&format!(
            "more than {SCAN} open runs; name the rest with --id\n"
        ));
    }
    let reservations = session.store.writer_fence_listing()?;
    *fenced |= !reservations.issues.is_empty();
    for issue in &reservations.issues {
        human.push_str(&format!(
            "writer reservation unreadable: {}: {}\n",
            issue.reservation.display(),
            issue.reason
        ));
    }
    Ok((
        json!({ "runs": rows, "truncated": truncated, "writer_reservations": reservations.reservations, "writer_reservation_errors": reservations.issues }),
        human,
    ))
}

/// Usage and cost over this project's terminal attempts, priced from the
/// reviewed table shipped with the product.
pub fn stats(command: Stats, edge: &Edge<'_>) -> Reply {
    reply("telemetry stats", stats_inner(command, edge))
}
fn stats_inner(command: Stats, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let path = edge.assets.join("config/telemetry-prices.toml");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        Fail::new(
            Status::Error,
            "PRICES",
            format!("{}: {error}", path.display()),
        )
    })?;
    let prices = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            provenant_telemetry::validate_prices(&config).map_err(|error| error.to_string())
        })
        .map_err(|error| Fail::new(Status::Error, "PRICES", error))?;
    let instant = |time: Option<jiff::Timestamp>| {
        time.map(|t| WallTime::from_micros(t.as_microsecond()))
            .transpose()
            .map_err(|error| Fail::new(Status::Invalid, "INVALID", error.to_string()))
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (context, store) = open_read_only(edge, &call)?;
    let window = provenant_telemetry::Window {
        since: instant(command.since)?,
        until: instant(command.until)?,
        project: Some(context.project_id),
        detail: command.detail,
    };
    let facts = store
        .map(|store| {
            store
                .attempts(
                    run::AttemptFilter {
                        run_id: command.run_id,
                        task_id: None,
                    },
                    &call,
                )?
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let report = provenant_telemetry::stats(facts, &prices, window)
        .map_err(|error| Fail::new(Status::Invalid, "INVALID", error.to_string()))?;
    let cost = report.totals.cost.complete.as_ref().map_or_else(
        || format!("at least ${}", report.totals.cost.known_lower_bound.usd),
        |money| format!("${}", money.usd),
    );
    let human = format!("{} attempts, {cost}\n", report.totals.attempts);
    let data = serde_json::to_value(&report)
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    Ok((data, human))
}

/// Offline lane timeline: terminal attempts as Chrome Trace Event JSON.
pub fn trace(command: Trace, edge: &Edge<'_>) -> Reply {
    reply("telemetry trace", trace_inner(command, edge))
}
fn trace_inner(command: Trace, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let since = command
        .since
        .map(|t| WallTime::from_micros(t.as_microsecond()))
        .transpose()
        .map_err(|error| Fail::new(Status::Invalid, "INVALID", error.to_string()))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (_, store) = open_read_only(edge, &call)?;
    let facts = store
        .map(|store| {
            store
                .attempts(
                    run::AttemptFilter {
                        run_id: command.run,
                        task_id: None,
                    },
                    &call,
                )?
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let trace = provenant_telemetry::trace(&facts, since);
    let events = trace["traceEvents"].as_array().map_or(0, Vec::len);
    let bytes = serde_json::to_vec(&trace)
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    std::fs::write(&command.out, bytes).map_err(|error| {
        Fail::new(
            Status::Error,
            "IO",
            format!("{}: {error}", command.out.display()),
        )
    })?;
    let out = command.out.display().to_string();
    Ok((
        json!({ "out": out, "events": events }),
        format!("{events} events written to {out}; open in ui.perfetto.dev\n"),
    ))
}

/// A switch handoff is a snapshot of active lane IDs, never a parallel queue.
pub fn chair_snapshot(edge: &Edge<'_>) -> Result<(provenant_core::ProjectId, Vec<String>), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
        budget: WaitBudget::cli(clock.mono_now(), 1)
            .map_err(|e| Fail::new(Status::Error, "TIME", e.to_string()))?,
    };
    let session = open(edge, &call)?;
    let mut lanes = Vec::new();
    let mut after = 0;
    loop {
        let page = session.store.lanes(
            &run::LaneQuery {
                targets: vec![],
                all: false,
                limit: 100,
                after,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            &call,
        )?;
        lanes.extend(
            page.lanes
                .iter()
                .map(|lane| format!("{}/{}/{}", lane.run_id, lane.task_id, lane.attempt_id)),
        );
        if lanes.len() > 1024 {
            return Err(Fail::new(
                Status::Error,
                "LIMIT",
                "too many active lanes for a handoff",
            ));
        }
        match page.next {
            Some(next) => after = next,
            None => break,
        }
    }
    Ok((session.context.project_id, lanes))
}

#[cfg(test)]
mod lane_depth_tests {
    use super::*;

    #[test]
    fn shared_submit_context_refuses_depth_above_the_limit() {
        let parent = AttemptId::from_bytes([7; 16]).to_string();
        let state = Path::new("/state");
        let child = state
            .join("children")
            .join(&parent)
            .join("children")
            .join(&parent);
        let result = lane_dispatch_values(Some("2"), Some(&parent), 2, state, Some(&child));
        let Err(failure) = result else {
            panic!("depth above the configured maximum must be refused");
        };
        assert_eq!(failure.status, Status::Refused);
        assert_eq!(failure.code, "LANE_DEPTH");
    }

    #[test]
    fn shared_submit_context_assigns_root_depth_and_parent() {
        let parent = AttemptId::from_bytes([8; 16]);
        let parent_text = parent.to_string();
        let state = Path::new("/state");
        let child = state.join("children").join(&parent_text);
        let result = lane_dispatch_values(Some("1"), Some(&parent_text), 2, state, Some(&child));
        assert!(matches!(result, Ok((2, Some(found))) if found == parent));
    }

    #[test]
    fn shared_submit_context_refuses_incomplete_or_zero_nested_context() {
        let parent = AttemptId::from_bytes([9; 16]).to_string();
        let state = Path::new("/state");
        let child = state.join("children").join(&parent);
        for (depth, parent_attempt) in [
            (None, None),
            (Some("2"), Some(parent.as_str())),
            (Some("1"), Some("00000000000000000000000000000000")),
        ] {
            let failure = lane_dispatch_values(depth, parent_attempt, 3, state, Some(&child))
                .expect_err("stripped or forged lineage must be refused");
            assert_eq!(failure.code, "LANE_CONTEXT");
        }
        for (depth, parent_attempt) in [
            (Some("1"), None),
            (None, Some(parent.as_str())),
            (Some("0"), Some(parent.as_str())),
        ] {
            let failure = lane_dispatch_values(depth, parent_attempt, 2, Path::new("/state"), None)
                .expect_err("incomplete or zero nested context must be refused");
            assert_eq!(failure.status, Status::Refused);
            assert_eq!(failure.code, "LANE_CONTEXT");
        }
    }

    #[test]
    fn shared_submit_context_assigns_root_depth_without_parent() {
        assert!(matches!(
            lane_dispatch_values(None, None, 2, Path::new("/state"), None),
            Ok((1, None))
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_run::Providers;
    use provenant_supervisor::custody::{
        CaptureTotals, PublicationState, Scope, SettlementEvidence,
    };
    #[test]
    fn lane_access_maps_network_capability_and_roots() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&state)?;
        std::fs::create_dir_all(&project)?;
        let access = |mode, network, capability: &[&str], write_root: Vec<PathBuf>| crate::Access {
            mode,
            worktree: None,
            read_root: Vec::new(),
            write_root,
            network,
            capability: capability.iter().map(|name| name.to_string()).collect(),
        };
        let network = |mode, switch, capability: &[&str]| {
            lane_access(&access(mode, switch, capability, Vec::new()), &state)
                .map(|access| access.network)
                .map_err(|fail| fail.message)
        };
        assert_eq!(network(Mode::WorktreeWrite, None, &[]), Ok(None));
        assert_eq!(
            network(Mode::ReadOnly, None, &["postgres"]),
            Ok(Some(run::NetworkAccess::Local))
        );
        assert_eq!(
            network(Mode::WorktreeWrite, Some(Switch::Off), &["postgres"]),
            Ok(Some(run::NetworkAccess::Local))
        );
        assert_eq!(
            network(Mode::ReadOnly, Some(Switch::On), &[]),
            Ok(Some(run::NetworkAccess::Open))
        );
        assert!(
            network(Mode::ReadOnly, None, &["rust"])
                .is_err_and(|message| message.contains("known: postgres"))
        );
        let granted = lane_access(
            &access(Mode::WorktreeWrite, None, &[], vec![project.clone()]),
            &state,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(
            granted.write_roots,
            [provenant_supervisor::fs::canonicalize(&project)?]
        );
        for refused in [
            state.join("projects"),
            temp.path().to_path_buf(),
            temp.path().join("absent"),
        ] {
            std::fs::create_dir_all(state.join("projects"))?;
            assert!(
                lane_access(
                    &access(Mode::WorktreeWrite, None, &[], vec![refused]),
                    &state
                )
                .is_err()
            );
        }
        Ok(())
    }
    #[test]
    fn paged_output_never_splits_a_character() {
        let bytes = "a☀b".as_bytes();
        let mut chunk = run::OutputChunk {
            bytes: bytes[..3].to_vec(),
            len: bytes.len() as u64,
            offset: 0,
            next_offset: 3,
            eof: false,
            clipped: true,
            complete: true,
            encoding: "utf8-fragment",
            digest: provenant_records::CanonicalV1::file_digest(b""),
        };
        align_utf8_tail(&mut chunk);
        assert_eq!(chunk.bytes, b"a");
        assert_eq!(chunk.next_offset, 1);
        assert_eq!(chunk.encoding, "utf8");
    }
    #[test]
    fn quota_reason_uses_provider_message_without_exposing_known_secret_shapes() {
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                run: RunId::from_bytes([1; 16]),
                attempt: AttemptId::from_bytes([2; 16]),
                generation: 1,
            },
            termination: Some(provenant_supervisor::exec::Termination::Exit(1)),
            never_spawned: false,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::NeverSpawned,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
        };
        let route = run::RouteRecord {
            provider: Some("opencode".into()),
            model: Some("opencode/a-free".into()),
            ..Default::default()
        };
        let registry = Registry {
            roots: None,
            home: None,
            assets: None,
            cwd: None,
            state_root: None,
            project_id: None,
            host: None,
        };
        for (message, expected) in [
            (
                "Free allowance exhausted. See https://opencode.ai/go\\quota",
                "Free allowance exhausted. See https://opencode.ai/go\\quota",
            ),
            (
                "Free allowance exhausted for ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                "Free allowance exhausted for [REDACTED]",
            ),
        ] {
            let event = json!({"type":"provider.quota","response":{"body":{"error":{"type":"FreeUsageLimitError","message":message}}}});
            let decoded = registry
                .decode(
                    &route,
                    event.to_string().as_bytes(),
                    &exit,
                    Path::new("/unused"),
                )
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
            assert_eq!(decoded.reason.as_deref(), Some(format!("{expected}; free tier exhausted; use a paid route or wait for its allowance reset").as_str()));
        }
        let event = json!({"type":"provider.quota","message":"☀".repeat(300_000)});
        let decoded = registry
            .decode(
                &route,
                event.to_string().as_bytes(),
                &exit,
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::QuotaLimited);
        let reason = decoded.reason.unwrap();
        assert!(
            reason.len() <= 8192,
            "reason must leave room in the terminal record"
        );
        assert!(reason.ends_with('☀'));
    }

    fn clean_exit() -> ExitObservation {
        ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                run: RunId::from_bytes([1; 16]),
                attempt: AttemptId::from_bytes([2; 16]),
                generation: 1,
            },
            termination: Some(provenant_supervisor::exec::Termination::Exit(0)),
            never_spawned: false,
            stop_reason: None,
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: false,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::NeverSpawned,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
        }
    }

    #[test]
    fn codex_rollout_error_reaches_the_decoded_reason() {
        let exit = clean_exit();
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            ..Default::default()
        };
        let registry = Registry {
            roots: None,
            home: None,
            assets: None,
            cwd: None,
            state_root: None,
            project_id: None,
            host: None,
        };
        let stream = [
            json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
            json!({"type":"item.completed","item":{"type":"agent_message","text":"OK"}}),
            json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
        ]
        .map(|frame| format!("{frame}\n"))
        .concat();
        let decoded = registry
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::Succeeded);
        assert_eq!(
            decoded.session.as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
        assert!(decoded.observed_route.is_none());
        assert_eq!(
            decoded.reason.as_deref(),
            Some("Codex rollout identity: provider continuation identity is unknown")
        );
    }

    #[test]
    fn claude_session_reaches_the_decoded_attempt() {
        let route = run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/claude-sonnet-5".into()),
            ..Default::default()
        };
        let stream = include_str!("../../../tests/fixtures/providers/claude/fresh.jsonl");
        let decoded = Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(
            decoded.session.as_deref(),
            Some("FIXTURE_SESSION_IDENTIFIER_003")
        );
    }
}
