//! Run's composition edge: `dispatch`, `lanes`, `output`, `cancel`, `resume`,
//! `reconcile` and `telemetry stats`, and the provider seam that the
//! same-binary guardian holder shares.
use crate::{
    Cancel, Condition, Dispatch, Lanes, Mode, Output, Part, Reconcile, Resume, Stats, Switch,
    Target,
};
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
            E::Validation(_) | E::TooLarge { .. } => (Status::Invalid, "INVALID"),
            E::OperationConflict => (Status::Invalid, "OPERATION_CONFLICT"),
            E::NotFound => (Status::Invalid, "NOT_FOUND"),
            E::Nonterminal => (Status::Refused, "NONTERMINAL"),
            E::Conflict => (Status::Refused, "CONFLICT"),
            E::Cancelled => (Status::Error, "CANCELLED"),
            E::Refused => (Status::Refused, "REFUSED"),
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
/// fallback; the first activated candidate is prepared. Until providers
/// expose launch specifications every adapter refuses here, task-locally.
pub struct Registry<'a> {
    pub roots: Option<&'a Roots>,
}
impl Registry<'_> {
    /// Whether providers can continue an exact prior session. None can yet,
    /// so `resume` refuses before it spends the attempt's one continuation.
    pub const fn continues_sessions(&self) -> bool {
        false
    }
}
impl run::Providers for Registry<'_> {
    fn prepare(
        &self,
        task: &run::TaskRequest,
        _prompt: &str,
        resume: Option<&run::AttemptRecord>,
    ) -> Result<run::PreparedLaunch, String> {
        if resume.is_some() {
            return Err(
                "continuation needs a provider session; providers do not expose sessions yet"
                    .into(),
            );
        }
        let roots = self.roots.ok_or("routing context is unavailable")?;
        let resolved =
            crate::route::resolve(&route_request(task)?, roots, task.account.as_deref())?;
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
fn open(edge: &Edge<'_>, call: &run::Request<'_>) -> Result<Session, Fail> {
    let (roots, project_id, actor_id) =
        crate::fabric::identity(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, "IDENTITY", error.to_string()))?;
    let context = run::ExecutionContext {
        state_root: roots.state,
        project_id,
        actor_id,
        read_prompts: true,
    };
    Ok(Session {
        store: run::RunStore::open(&context, call)?,
        context,
    })
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
/// Exactly one of `ADAPTER/ID` or a named route.
pub fn requested(
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
/// Worktree access is explicit. A missing local worktree is named first; a
/// worktree with read-only access is refused, never run read-only.
pub fn worktree_access(writer: bool, worktree: Option<&Path>, cwd: &Path) -> Result<(), Fail> {
    match worktree {
        None if !writer => Ok(()),
        None => Err(Fail::new(
            Status::Invalid,
            "WORKTREE_REQUIRED",
            "worktree-write access needs --worktree",
        )),
        Some(path) if !cwd.join(path).is_dir() => Err(Fail::new(
            Status::Invalid,
            "WORKTREE_MISSING",
            format!(
                "local worktree {} does not exist; create it with `provenant worktree create <name>`",
                path.display()
            ),
        )),
        Some(_) if !writer => Err(Fail::new(
            Status::Invalid,
            "WORKTREE_NOT_APPLICABLE",
            "--worktree requires --mode worktree-write",
        )),
        Some(_) => Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            "worktree-write dispatch is not available in this build",
        )),
    }
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
}
pub struct Submission {
    pub operation_id: OperationId,
    pub tasks: Vec<Submitted>,
    pub concurrency: Option<u64>,
    pub knobs: Vec<String>,
}
/// Scan, record and launch read-only work: the one path the CLI and Fabric
/// share. Replaying an operation returns its run and launches nothing new.
pub fn submit(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
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
        requests.push(run::TaskRequest {
            profile: profile(task.task_class.as_deref())?,
            task_id: task.task_id,
            prompt_digest: CanonicalV1::file_digest(task.prompt.as_bytes()),
            route: task.route,
            account: task.account,
            fallback: fallback(&task.fallback)?,
            authority_digest: authority,
            provider_timeout_seconds: task.provider_timeout,
            delivery_id: task.delivery_id,
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
        },
        attempts,
        created_at: now(call.clock)?,
    };
    let record = session.store.create(&prepared, call)?;
    let started = launch(session, record.run_id, edge, call)?;
    Ok((record, started))
}

pub fn dispatch(command: Dispatch, edge: &Edge<'_>) -> Reply {
    reply_waited("dispatch", dispatch_inner(command, edge))
}
fn dispatch_inner(command: Dispatch, edge: &Edge<'_>) -> Result<(Value, String, bool), Fail> {
    let access = &command.access;
    let unsupported = |what: &str| {
        Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            format!("{what} is not available in this build"),
        ))
    };
    worktree_access(
        matches!(access.mode, Mode::WorktreeWrite),
        access.worktree.as_deref(),
        edge.cwd,
    )?;
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
                task.account,
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
            None,
        ));
    }
    let concurrency = if command.input.manifest.is_some() {
        command.concurrency
    } else {
        Some(1)
    };
    let tasks = tasks
        .into_iter()
        .map(
            |(task_id, prompt, route, fallback, delivery_id, account)| Submitted {
                task_class: None,
                task_id,
                prompt,
                route,
                fallback,
                account,
                provider_timeout: command.provider_timeout,
                delivery_id,
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

/// Hand every queued attempt of `run` to its own guardian. Fabric's facade
/// shares this path with the CLI leaves.
pub fn launch(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<Vec<run::Started>, Fail> {
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
    let registry = Registry {
        roots: Some(edge.roots),
    };
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
            let (data, human) = render_lanes(&page.lanes, None, edge);
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
                "account": account,
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
/// Snapshot every attempt the targets name. Without targets, the selection is
/// the current nonterminal work; host caps bound it.
fn select(
    session: &Session,
    targets: &[Target],
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let mut selected = Vec::new();
    let mut after = 0;
    loop {
        let page = session
            .store
            .lanes(&lane_query(targets, !targets.is_empty(), 100, after), call)?;
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
    let selected = select(session, targets, &request(&clock, &cancel)?)?;
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
    reply_waited(
        "lanes",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let session = open(edge, &call)?;
            if let Some(seconds) = command.wait {
                return wait_lanes(&session, &command.id, command.condition, seconds, edge);
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
            let (data, human) = render_lanes(&page.lanes, page.next, edge);
            Ok((data, human, false))
        })(),
    )
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

/// Cancel every attempt the target names. Queued work ends at once; started
/// work gets a stop request its guardian acts on. Requested is not exit.
pub fn cancel(command: Cancel, edge: &Edge<'_>) -> Reply {
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
            let selected = select(&session, &targets, &call)?;
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
            for id in selected {
                // One failure must not hide the receipts already earned.
                let receipt = request(&clock, &cancel)
                    .and_then(|call| Ok(session.store.cancel(id, reason, &call)?));
                let row = match receipt {
                    Ok(receipt) => {
                        if matches!(receipt, run::CancelReceipt::Unobservable { .. }) {
                            status = Status::Failed;
                        }
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
    if !(Registry { roots: None }).continues_sessions() {
        return Err(Fail::new(
            Status::Refused,
            "UNSUPPORTED",
            "resume needs provider sessions, which providers do not expose yet",
        ));
    }
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
                if matches!(state, run::Reconciled::Uncertain { .. }) {
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
    Ok((json!({ "runs": rows, "truncated": truncated }), human))
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
    let session = open(edge, &call)?;
    let window = provenant_telemetry::Window {
        since: instant(command.since)?,
        until: instant(command.until)?,
        project: Some(session.context.project_id),
        detail: command.detail,
    };
    let facts = session
        .store
        .attempts(
            run::AttemptFilter {
                run_id: None,
                task_id: None,
            },
            &call,
        )?
        .collect::<Result<Vec<_>, _>>()?;
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
