//! Run's composition edge: `dispatch`, `lanes` and `output`, and the provider
//! seam that the same-binary guardian holder shares.
use crate::{Condition, Dispatch, Lanes, Mode, Output, Part, Switch, Target};
use provenant_core::{
    AttemptId, CancelToken, Clock, DeliveryId, OperationId, Roots, RunId, TaskId, WaitBudget,
    WallTime,
};
use provenant_providers::{OutcomeStatus, ProviderError, ProviderEvent, REGISTRY};
use provenant_records::{CanonicalV1, CommandError, CommandResult, Outcome as Status};
use provenant_routing::{BoundedModels, Fallback, ModelId, RouteChoice};
use provenant_run as run;
use provenant_supervisor::{clock::SystemClock, custody::ExitObservation, fs::entropy_128};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

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
struct Fail {
    status: Status,
    code: &'static str,
    message: String,
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
            E::Validation(_) | E::TooLarge { .. } => (Status::Invalid, "INVALID"),
            E::OperationConflict => (Status::Invalid, "OPERATION_CONFLICT"),
            E::NotFound => (Status::Invalid, "NOT_FOUND"),
            E::Nonterminal => (Status::Refused, "NONTERMINAL"),
            E::Refused => (Status::Refused, "REFUSED"),
            E::Capacity => (Status::Refused, "QUEUE_FULL"),
            E::Contention => (Status::Error, "CONTENTION"),
            _ => (Status::Error, "IO"),
        };
        Self::new(status, code, error.to_string())
    }
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
/// fallback; the first activated candidate is prepared. Until providers
/// expose launch specifications every adapter refuses here, task-locally.
pub struct Registry<'a> {
    pub roots: Option<&'a Roots>,
}
impl run::Providers for Registry<'_> {
    fn prepare(
        &self,
        task: &run::TaskRequest,
        _prompt: &str,
    ) -> Result<run::PreparedLaunch, String> {
        let roots = self.roots.ok_or("routing context is unavailable")?;
        let resolved = crate::route::resolve(&route_request(task)?, roots)?;
        let candidates = std::iter::once((resolved.adapter.as_str(), resolved.model.as_str()))
            .chain(
                resolved
                    .alternatives
                    .as_slice()
                    .iter()
                    .map(|a| (a.adapter.as_str(), a.model.as_str())),
            );
        let reasons: Vec<String> = candidates
            .map(|(adapter, model)| {
                let model = format!("{adapter}/{model}");
                match REGISTRY.iter().find(|r| r.adapter.id() == adapter) {
                    None => format!("{model}: unknown adapter"),
                    Some(r) if !r.adapter.execution_enabled() => format!(
                        "{model}: {}",
                        ProviderError::Disabled {
                            adapter: r.adapter.id()
                        }
                    ),
                    Some(_) => format!("{model}: provider launch specification is not wired"),
                }
            })
            .collect();
        Err(reasons.join("; "))
    }
    fn decode(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
    ) -> run::Decoded {
        let failed = |reason: &str| run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: run::UsageRecord::default(),
            reason: Some(reason.into()),
        };
        let Some(adapter) = REGISTRY
            .iter()
            .find(|r| Some(r.adapter.id()) == route.provider.as_deref())
            .map(|r| r.adapter)
        else {
            return failed("route names no registered adapter");
        };
        let Ok(mut state) = adapter.stream(None) else {
            return failed("provider stream is unavailable");
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
            return failed("provider outcome is malformed");
        };
        events.extend(outcome.final_events().iter().cloned());
        let mut decoded = run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: true,
            observed_route: outcome.model().map(|model| run::RouteRecord {
                provider: Some(adapter.id().into()),
                model: Some(if model.contains('/') {
                    model.into()
                } else {
                    format!("{}/{model}", adapter.id())
                }),
                effort: route.effort.clone(),
                account_label: None,
            }),
            usage: run::UsageRecord::default(),
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
        decoded
    }
}
fn route_request(task: &run::TaskRequest) -> Result<provenant_routing::RouteRequest, String> {
    let model = |text: &str| ModelId::parse(text).map_err(|error| error.to_string());
    let (choice, effort) = match &task.route {
        run::RequestedRoute::Model {
            model: id, effort, ..
        } => (RouteChoice::Model(model(id)?), effort.clone()),
        run::RequestedRoute::Named { name, effort } => {
            (RouteChoice::NamedRoute(name.clone()), effort.clone())
        }
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
    })
}

struct Session {
    store: run::RunStore,
    context: run::ExecutionContext,
}
fn request<'a>(clock: &'a SystemClock, cancel: &'a CancelToken) -> Result<run::Request<'a>, Fail> {
    Ok(run::Request {
        clock,
        budget: WaitBudget::cli(clock.mono_now(), 30)
            .map_err(|error| Fail::new(Status::Error, "TIME", error.to_string()))?,
        cancel,
    })
}
fn open(edge: &Edge<'_>, call: &run::Request<'_>) -> Result<Session, Fail> {
    let identity = crate::fabric::context::Context::open(edge.roots.clone(), edge.home, edge.cwd)
        .map_err(|error| Fail::new(Status::Error, "IDENTITY", error.to_string()))?
        .identity;
    let context = run::ExecutionContext {
        state_root: edge.roots.state.clone(),
        project_id: identity.project,
        actor_id: identity.inbox.actor,
        read_prompts: true,
    };
    Ok(Session {
        store: run::RunStore::open(&context, call)?,
        context,
    })
}
fn now(clock: &SystemClock) -> Result<WallTime, Fail> {
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
fn requested(
    model: Option<&str>,
    route: Option<&str>,
    effort: Option<String>,
) -> Result<run::RequestedRoute, Fail> {
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
            })
        })
        .collect()
}
/// Task IDs default to a value derived from the operation, so replaying the
/// same operation reproduces the same canonical request.
fn derived_task(operation: &OperationId, index: usize) -> Result<TaskId, Fail> {
    let digest = CanonicalV1::record_digest("run-task-id", 1, &(operation.as_str(), index))
        .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(TaskId::from_bytes(bytes))
}

pub fn dispatch(command: Dispatch, edge: &Edge<'_>) -> Reply {
    reply("dispatch", dispatch_inner(command, edge))
}
fn dispatch_inner(command: Dispatch, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    let access = &command.access;
    let unsupported = |what: &str| {
        Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            format!("{what} is not available in this build"),
        ))
    };
    if matches!(access.mode, Mode::WorktreeWrite) || access.worktree.is_some() {
        return unsupported("worktree-write dispatch");
    }
    if matches!(access.network, Switch::On)
        || !access.capability.is_empty()
        || !access.read_root.is_empty()
    {
        return unsupported("network, capability or read-root access");
    }
    if command.host.is_some() {
        return unsupported("remote host dispatch");
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let created_at = now(&clock)?;
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
                Status::Invalid,
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
            tasks.push((
                task_id,
                prompt,
                requested(
                    model.as_deref(),
                    route.as_deref(),
                    task.effort.or(flag_route.2.clone()),
                )?,
                fallback,
                task.delivery_id,
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
            flag_route.3.clone(),
            command.delivery_id,
        ));
    }
    #[derive(Serialize)]
    struct Authority<'a> {
        mode: &'static str,
        network: bool,
        cwd: &'a Path,
    }
    let authority = CanonicalV1::record_digest(
        "run-authority",
        1,
        &Authority {
            mode: "read_only",
            network: false,
            cwd: edge.cwd,
        },
    )
    .map_err(|error| Fail::new(Status::Error, "IO", error.to_string()))?;
    let mut requests = Vec::new();
    let mut attempts = Vec::new();
    for (task_id, prompt, route, fallback_models, delivery_id) in tasks {
        requests.push(run::TaskRequest {
            task_id,
            prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
            route,
            fallback: fallback(&fallback_models)?,
            authority_digest: authority,
            provider_timeout_seconds: command.provider_timeout,
            delivery_id,
        });
        attempts.push(run::NewAttempt {
            attempt_id: fresh(AttemptId::from_bytes)?,
            task_id,
            prompt,
        });
    }
    let concurrency = if command.input.manifest.is_some() {
        command.concurrency
    } else {
        1
    };
    let prepared = run::PreparedRun {
        run_id: fresh(RunId::from_bytes)?,
        operation_id: command.operation_id.clone(),
        request: run::CanonicalRequest {
            schema_version: 1,
            tasks: requests,
            concurrency,
            parent: None,
        },
        attempts,
        created_at,
    };
    let session = open(edge, &call)?;
    let record = session.store.create(&prepared, &call)?;
    let seed = run::RootSeed {
        home: edge.home.into(),
        checkout: edge.assets.into(),
        state_root: session.context.state_root.as_path().into(),
    };
    let holder = |attempt: &run::AttemptRecord| {
        run::HolderContext {
            schema_version: 1,
            roots: seed.clone(),
            project_id: attempt.project_id,
            actor_id: session.context.actor_id,
            run_id: attempt.run_id,
            attempt_id: attempt.attempt_id,
        }
        .encode()
    };
    let started = session.store.start(
        record.run_id,
        &Registry {
            roots: Some(edge.roots),
        },
        &holder,
        &call,
    )?;
    let unknown: Vec<String> = started
        .iter()
        .filter_map(|s| match s {
            run::Started::Unknown { attempt, error } => Some(format!("{attempt}: {error}")),
            _ => None,
        })
        .collect();
    let target = Target {
        run: record.run_id,
        task: None,
        attempt: None,
    };
    let (mut data, human, _) = match command.wait {
        Some(seconds) => wait_lanes(&session, &[target], Condition::All, seconds, edge)?,
        None => {
            let page = session
                .store
                .lanes(&lane_query(&[target], true, 100, 0), &call)?;
            let (data, human) = render_lanes(&page.lanes, None, edge);
            (data, human, false)
        }
    };
    data["run_id"] = json!(record.run_id);
    data["operation_id"] = json!(record.operation_id);
    if !unknown.is_empty() {
        data["launch_unknown"] = json!(unknown);
    }
    Ok((data, format!("run {}\n{human}", record.run_id)))
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
    }
}
fn artifact(edge: &Edge<'_>, record: &run::AttemptRecord, name: &str) -> PathBuf {
    edge.roots.state.as_path().join(format!(
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
fn render_lanes(
    lanes: &[run::AttemptRecord],
    next: Option<u64>,
    edge: &Edge<'_>,
) -> (Value, String) {
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
                .map(|_| artifact(edge, lane, "result.md"));
            human.push_str(&format!(
                "{}/{}/{} {state} {route}{}\n",
                lane.run_id,
                lane.task_id,
                lane.attempt_id,
                reason
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ));
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
                "observed_route": lane.observed_route,
                "queued_at": lane.queued_at,
                "started_at": lane.started_at,
                "finished_at": finished_at,
                "delivery_id": lane.delivery_id,
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
/// Snapshot the selection at entry and poll until the condition holds or the
/// reader deadline expires. Expiry never stops a provider.
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
    let call = request(&clock, &cancel)?;
    // Without targets, the selection is the current nonterminal work. The
    // host caps bound it, so every page is snapshotted.
    let mut selected = Vec::new();
    let mut after = 0;
    loop {
        let page = session
            .store
            .lanes(&lane_query(targets, !targets.is_empty(), 100, after), &call)?;
        selected.extend(page.lanes.iter().map(|lane| lane.attempt_id));
        match page.next {
            Some(next) if selected.len() < 1000 => after = next,
            Some(_) => {
                return Err(Fail::new(
                    Status::Invalid,
                    "INVALID",
                    "wait selection exceeds 1000 lanes",
                ));
            }
            None => break,
        }
    }
    loop {
        let call = request(&clock, &cancel)?;
        let lanes = selected
            .iter()
            .map(|id| session.store.record(*id, &call))
            .collect::<Result<Vec<_>, _>>()?;
        let states: Vec<(bool, bool)> = lanes.iter().map(|l| settled(&l.state)).collect();
        let met = states.is_empty()
            || match condition {
                Condition::Any => states.iter().any(|(done, _)| *done),
                Condition::All => {
                    states.iter().all(|(done, _)| *done) || states.iter().any(|(_, input)| *input)
                }
            };
        let expired = Instant::now() >= deadline;
        if met || expired {
            let (mut data, human) = render_lanes(&lanes, None, edge);
            data["condition_met"] = json!(met);
            return Ok((data, human, !met));
        }
        std::thread::park_timeout(
            Duration::from_millis(500).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub fn lanes(command: Lanes, edge: &Edge<'_>) -> Reply {
    let mut expired = false;
    let mut out = reply(
        "lanes",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let session = open(edge, &call)?;
            if let Some(seconds) = command.wait {
                let (data, human, timed_out) =
                    wait_lanes(&session, &command.id, command.condition, seconds, edge)?;
                expired = timed_out;
                return Ok((data, human));
            }
            let after = match &command.cursor {
                Some(cursor) => cursor
                    .parse()
                    .map_err(|_| Fail::new(Status::Invalid, "INVALID", "invalid cursor"))?,
                None => 0,
            };
            let page = session.store.lanes(
                &lane_query(&command.id, command.all, command.limit, after),
                &call,
            )?;
            Ok(render_lanes(&page.lanes, page.next, edge))
        })(),
    );
    // A reader deadline still reports the lanes it saw.
    if expired {
        out.result.status = Status::Timeout;
    }
    out
}

pub fn output(command: Output, edge: &Edge<'_>) -> Reply {
    reply("output", output_inner(command, edge))
}
fn output_inner(command: Output, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let session = open(edge, &call)?;
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
