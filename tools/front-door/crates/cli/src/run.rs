//! Run's composition edge: `dispatch`, `lanes`, `output`, `cancel`, `resume`,
//! `reconcile` and `telemetry stats`, and the provider seam that the
//! same-binary guardian holder shares.
use crate::{
    Cancel, Condition, Dispatch, Lanes, Mode, Output, Part, PricesRefresh, Reconcile, Resume,
    Stats, Switch, Target, Trace, limits,
};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, DeliveryId, DurationMicros, OperationId, Roots, RunId,
    TaskId, WallTime,
};
use provenant_fabric_runtime::hosts::plain_text;
use provenant_os::{clock::SystemClock, fs::entropy_128};
use provenant_providers::{OutcomeStatus, ProviderEvent, REGISTRY};
use provenant_records::{CanonicalV1, CommandError, CommandResult, Outcome as Status};
use provenant_routing::{BoundedModels, Fallback, ModelId, RouteChoice, RouteName};
use provenant_run as run;
use provenant_supervisor::custody::ExitObservation;
use run::ErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod assess;
mod cancel;
mod children;
mod dispatch;
mod output;
mod provider;
mod reconcile;
mod remote;
mod resume;
mod review;
mod status;
mod submit;
mod targets;
mod telemetry;
mod wait;
mod watch;
mod worktree;
use assess::*;
pub use cancel::*;
pub use children::HostHolder;
pub use dispatch::*;
pub use output::*;
pub(crate) use provider::find_executable as provider_executable;
#[cfg(feature = "fixture")]
pub use provider::{apply_fixture_setup, prepare_fixture, writer_fixture_plan};
pub use provider::{graph_inputs_directory, graph_node_inputs_directory};
pub use reconcile::*;
pub use resume::*;
pub use review::review;
pub use status::*;
pub use submit::*;
pub use telemetry::*;
pub use wait::*;
pub use worktree::*;

/// Catalogue family of a provider-qualified model, for family-distinct routing.
pub fn model_family(model: &str) -> Option<String> {
    let catalogue = provenant_routing::Catalogue::bundled().ok()?;
    let id = ModelId::parse(model).ok()?;
    catalogue.model(&id).and_then(|m| m.family.clone())
}

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
    /// The owner's compact view; without one the renderer truncates `human`.
    pub summary: Option<crate::Summary>,
    /// A read that may end quietly if its reader goes away.
    pub read_only: bool,
}
/// A refused or failed owner call: envelope status, stable code and message.
#[derive(Debug)]
pub struct Fail {
    pub status: Status,
    pub kind: ErrorKind,
    pub message: String,
    /// Safe typed detail retained for the MCP edge, without raw source errors.
    pub detail: Option<Box<provenant_fabric_runtime::coordination::facade::FacadeError>>,
    pub projection: Option<Box<provenant_records::diagnostic::Diagnostic>>,
}
impl Fail {
    fn new(status: Status, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
            detail: None,
            projection: None,
        }
    }

    fn config(status: Status, error: provenant_config::ConfigError) -> Self {
        let kind = if error.refusal_code().is_some() {
            ErrorKind::SharedSettingsNeverSynced
        } else {
            ErrorKind::Config
        };
        let mut fail = Self::new(status, kind, error.to_string());
        if kind == ErrorKind::SharedSettingsNeverSynced {
            fail.projection = Some(Box::new(kind.diagnostic()));
        }
        fail
    }
}
impl From<run::RunError> for Fail {
    fn from(error: run::RunError) -> Self {
        let kind = ErrorKind::from(&error);
        let projection = error.diagnostic(provenant_records::diagnostic::Destination::Local);
        let message = projection.detail.as_ref().map_or_else(
            || kind.summary().to_owned(),
            |detail| {
                if matches!(
                    error,
                    run::RunError::Delivery(_) | run::RunError::DeliveryLimit { .. }
                ) {
                    detail.clone()
                } else {
                    format!("{} {detail}", kind.summary())
                }
            },
        );
        let status = match kind.category() {
            "invalid" => Status::Invalid,
            "refused" => Status::Refused,
            _ => Status::Error,
        };
        Self {
            projection: Some(Box::new(projection)),
            ..Self::new(status, kind, message)
        }
    }
}
fn command_error(fail: &Fail) -> CommandError {
    CommandError {
        diagnostic: fail.projection.as_deref().map(Into::into),
        code: fail.kind.code().into(),
        message: fail.message.clone(),
        cause: fail
            .projection
            .as_ref()
            .and_then(|projection| projection.detail.clone()),
        repair: fail
            .projection
            .as_ref()
            .and_then(|projection| projection.next.clone()),
    }
}
/// A reply whose reader deadline expired still reports what it saw.
fn reply_waited(command: &str, outcome: Result<(Value, String, bool), Fail>) -> Reply {
    let mut expired = false;
    let mut silent = false;
    let mut out = reply(
        command,
        outcome.map(|(data, human, timed_out)| {
            expired = timed_out;
            silent = data["reason"].is_string();
            (data, human)
        }),
    );
    if silent {
        out.result.status = Status::Failed;
    } else if expired {
        out.result.status = Status::Timeout;
    }
    out
}
fn reply(command: &str, outcome: Result<(Value, String), Fail>) -> Reply {
    match outcome {
        Ok((data, human)) => Reply {
            result: CommandResult::new(command, Status::Ok, data),
            human,
            summary: None,
            read_only: false,
        },
        Err(fail) => {
            let mut result = CommandResult::new(command, fail.status, Value::Null);
            result.error = Some(command_error(&fail));
            Reply {
                result,
                human: String::new(),
                summary: None,
                read_only: false,
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
    pub cancel: Option<&'a CancelToken>,
}
impl Registry<'_> {
    fn prepare_with_wait(
        &self,
        task: &run::TaskRequest,
        prompt: &str,
        paths: &run::LaunchPaths,
        resume: Option<&run::AttemptRecord>,
        wait: Option<u64>,
    ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
        let prepared = provider::prepare(self, task, prompt, paths, resume, wait)?;
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
    /// Whether any registered adapter can continue a recorded session.
    pub fn continues_sessions(&self) -> bool {
        REGISTRY.iter().any(|entry| {
            provenant_records::AdapterSpec::get(entry.adapter.id())
                .is_some_and(|spec| spec.launch.resume)
        })
    }
    /// Refuse before the attempt's one continuation is spent.
    fn check_continuable(&self, prior: &run::AttemptRecord) -> Result<(), Fail> {
        if !matches!(prior.state, run::AttemptState::Terminal { .. }) {
            return Err(run::RunError::Nonterminal.into());
        }
        let unsupported = |message: String| {
            Fail::new(Status::Refused, ErrorKind::ContinuationUnsupported, message)
        };
        if prior
            .applied_route
            .as_ref()
            .is_some_and(|route| route.extra.contains_key("host_native"))
        {
            return Err(unsupported(
                "host-native does not support resume; dispatch a fresh lane with the prior result"
                    .into(),
            ));
        }
        let adapter = prior
            .applied_route
            .as_ref()
            .and_then(|route| route.provider.as_deref())
            .ok_or_else(|| unsupported("the attempt launched no provider to resume".into()))?;
        if !REGISTRY.iter().any(|entry| entry.adapter.id() == adapter) {
            return Err(unsupported(format!("unknown provider {adapter}")));
        }
        if let Some(spec) = provenant_records::AdapterSpec::get(adapter) {
            spec.check_launch(&provenant_records::LaunchRequest {
                model: None,
                resume: true,
                ..Default::default()
            })
            .map_err(|error| unsupported(error.to_string()))?;
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
    fn runtime_cap(&self, provider: &str) -> Option<u64> {
        provenant_config::read_with_domains(self.roots?, &crate::domains::SECTIONS)
            .ok()?
            .providers
            .get(provider)?
            .runtime_deadline_seconds
    }
    fn assess_result(
        &self,
        task: &run::TaskRequest,
        record: &run::AttemptRecord,
        decoded: &run::Decoded,
        handoff: Option<&provenant_records::handoff::Handoff>,
        context: Option<&run::HolderContext>,
        policy_attempt: u64,
    ) -> Result<run::ResultAssessment, String> {
        let Some(route) = record.applied_route.as_ref() else {
            return Ok(Default::default());
        };
        let Some(model) = route.model.as_deref() else {
            return Ok(Default::default());
        };
        let model = ModelId::parse(model).map_err(|error| error.to_string())?;
        let context_roots = context
            .map(|context| {
                let mut inputs = provenant_core::RootInputs::new(
                    context.roots.home.clone(),
                    context.roots.checkout.clone(),
                );
                inputs.state_dir = Some(context.disk_roots().0.to_owned());
                inputs
                    .resolve(&provenant_os::fs::SystemPathResolver)
                    .map_err(|error| error.to_string())
            })
            .transpose()?;
        let routing = match context_roots.as_ref().or(self.roots) {
            Some(roots) => {
                provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
                    .map_err(|error| error.to_string())?
                    .domains
                    .routing
            }
            None => Default::default(),
        };
        let now = SystemClock::new()
            .wall_now()
            .map_err(|error| error.to_string())?;
        let policy = routing
            .model_policy
            .effective(&model, route.account_key.as_deref(), now)
            .policy;
        let mut assessment =
            assess_model_result(&policy, task, record, decoded, handoff, policy_attempt)?;
        if route.extra.contains_key("host_native") {
            // The host row is fresh-only. Provider-level policy retries would
            // reconstruct a model route and lose its agent and provenance.
            assessment.retry = None;
            return Ok(assessment);
        }
        if assessment.refusal.is_some() || assessment.retry.is_some() {
            return Ok(assessment);
        }
        // A quota-stopped writer with a recorded unfinished handoff may
        // continue its retained worktree on another eligible account.
        let quota_handoff_recovery =
            if quota_handoff_recovery_eligible(decoded.outcome, task, handoff) {
                if let (Some(roots), Some(adapter), Some(key)) = (
                    context_roots.as_ref().or(self.roots),
                    route.provider.as_deref(),
                    route.account_key.as_deref(),
                ) {
                    let config =
                        provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
                            .map_err(|error| error.to_string())?;
                    let accounts = config.domains.accounts.with_derived_accounts();
                    let quota =
                        crate::quota::read_cache_at(roots.state.as_path()).unwrap_or_default();
                    let (quota, policies) = crate::route::account_evidence(
                        &config.domains.quota,
                        quota,
                        now.unix_seconds(),
                        REGISTRY,
                    );
                    let catalogue = provenant_routing::Catalogue::bundled()
                        .map_err(|error| error.to_string())?;
                    unspent_alternative(
                        accounts
                            .entries
                            .values()
                            .filter(|entry| entry.adapter == adapter)
                            .map(|entry| entry.key.as_str()),
                        &quota,
                        &policies,
                        key,
                        &model,
                        &catalogue,
                        now.unix_seconds(),
                    )
                } else {
                    false
                }
            } else {
                false
            };
        Ok(assess_settlement_retry(
            &routing,
            task,
            record,
            decoded,
            policy_attempt,
            now.unix_seconds(),
            quota_handoff_recovery,
        ))
    }
    fn priority(&self, task: &run::TaskRequest) -> Result<u8, String> {
        let roots = self.roots.ok_or("routing context is unavailable")?;
        let priority = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|error| error.to_string())?
            .domains
            .fabric
            .lanes
            .priority;
        Ok(priority.for_lane(task.writer.is_some(), task.build_heavy))
    }
    fn check_policy(&self, task: &run::TaskRequest) -> Result<(), String> {
        let Some(writer) = &task.writer else {
            return Ok(());
        };
        let roots = self.roots.ok_or("routing context is unavailable")?;
        let home = self.home.ok_or("home directory is unavailable")?;
        let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
            .map_err(|error| error.to_string())?;
        check_writer_disk(
            &config.gc,
            roots.state.as_path(),
            &writer.worktree,
            &writer.project,
            home,
        )
        .map_err(String::from)
    }

    fn failure_diagnostic(&self, route: &run::RouteRecord, stderr: &[u8]) -> Option<String> {
        REGISTRY
            .iter()
            .find(|entry| Some(entry.adapter.id()) == route.provider.as_deref())?
            .adapter
            .failure_diagnostic(stderr)
    }
    fn decode_captures(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
        context: Option<&run::HolderContext>,
    ) -> Result<run::Decoded, String> {
        let Some(context) = context else {
            return self.decode_inner(route, stdout, stderr, exit, provider_state, false);
        };
        let (configuration_root, _) = context.disk_roots();
        // Every branch carries the holder home: without it the OpenCode
        // identity lookup has no `.local/share` fallback and decodes capture
        // nothing when the launch environment unset `XDG_DATA_HOME`.
        let holder_home = Some(context.roots.home.as_path());
        // A child store must never write host capacity evidence.
        if configuration_root != context.roots.state_root {
            return Registry {
                home: holder_home,
                ..Default::default()
            }
            .decode_inner(route, stdout, stderr, exit, provider_state, false);
        }
        let mut inputs = provenant_core::RootInputs::new(
            context.roots.home.clone(),
            context.roots.checkout.clone(),
        );
        inputs.state_dir = Some(configuration_root.to_path_buf());
        let roots = match inputs.resolve(&provenant_os::fs::SystemPathResolver) {
            Ok(roots) => roots,
            Err(_) => {
                // A failed sealed-root binding must not fall back to this
                // registry's unrelated roots when recording capacity evidence.
                let mut decoded = Registry {
                    home: holder_home,
                    ..Default::default()
                }
                .decode_inner(
                    route,
                    stdout,
                    stderr,
                    exit,
                    provider_state,
                    false,
                )?;
                if decoded.reason.as_deref().is_some_and(capacity_reason) {
                    let reason = decoded.reason.get_or_insert_with(String::new);
                    let mut end = reason
                        .len()
                        .min(8192 - "; capacity cooldown configuration unavailable".len());
                    while !reason.is_char_boundary(end) {
                        end -= 1;
                    }
                    reason.truncate(end);
                    reason.push_str("; capacity cooldown configuration unavailable");
                }
                return Ok(decoded);
            }
        };
        let registry = Registry {
            roots: Some(&roots),
            home: holder_home,
            state_root: Some(configuration_root),
            ..Default::default()
        };
        registry.decode_inner(route, stdout, stderr, exit, provider_state, true)
    }
    fn prepare(
        &self,
        task: &run::TaskRequest,
        prompt: &str,
        paths: &run::LaunchPaths,
        resume: Option<&run::AttemptRecord>,
    ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
        self.prepare_with_wait(task, prompt, paths, resume, None)
    }

    fn decode(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
    ) -> Result<run::Decoded, String> {
        self.decode_inner(route, stdout, &[], exit, provider_state, false)
    }
}
impl Registry<'_> {
    fn decode_inner(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        stderr: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
        host_settlement: bool,
    ) -> Result<run::Decoded, String> {
        let result_event_usage = route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.result_event_usage);
        let failed = |reason: &str| run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: false,
            observed_route: None,
            // `source` names why usage is absent; records stay readable by older builds.
            usage: run::UsageRecord {
                source: result_event_usage.then(|| "absent: no result event".into()),
                ..run::UsageRecord::default()
            },
            session: None,
            reason: Some(reason.into()),
            partial_signals: Vec::new(),
        };
        let Some(adapter) = REGISTRY
            .iter()
            .find(|r| Some(r.adapter.id()) == route.provider.as_deref())
            .map(|r| r.adapter)
        else {
            return Ok(failed("route names no registered adapter"));
        };
        let model_observation = provenant_records::AdapterSpec::get(adapter.id())
            .map(|spec| spec.model_observation)
            .unwrap_or(provenant_records::ModelObservation::Stream);
        let Ok(mut state) = adapter.stream(None) else {
            return Ok(failed("provider stream is unavailable"));
        };
        let mut events = adapter.observe(&mut state, stdout).unwrap_or_default();
        let code = match exit.termination {
            Some(provenant_os::exec::Termination::Exit(code)) => Some(code),
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
        // Exit-time stderr quota text is evidence only for a run the adapter
        // did not accept. Live structured refusals trigger cleanup separately.
        if provenant_records::AdapterSpec::get(adapter.id()).is_some_and(|spec| spec.stderr_quota)
            && !matches!(outcome.status(), OutcomeStatus::Success)
            && let Some(event) = provenant_providers::opencode::stderr_quota(stderr)
        {
            events.push(event);
        }
        let rollout =
            (model_observation == provenant_records::ModelObservation::Rollout).then(|| {
                outcome
                    .session()
                    .ok_or(provenant_providers::ProviderError::UnknownIdentity)
                    .and_then(|thread| provider::observe_codex_rollout(provider_state, thread))
            });
        // The typed reason survives into the terminal record for diagnosis.
        let mut rollout_error = rollout
            .as_ref()
            .and_then(|observed| observed.as_ref().err())
            .map(|_| {
                provenant_records::diagnostic::ProviderFailureFact::CodexIdentity
                    .reason()
                    .to_owned()
            });
        // OpenCode's stream names no model; its session record does.
        let opencode_model = (model_observation
            == provenant_records::ModelObservation::SessionDatabase)
            .then(|| {
                let data = std::env::var_os("XDG_DATA_HOME")
                    .map(std::path::PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .or_else(|| self.home.map(|home| home.join(".local/share")))
                    .map(|data| data.join(adapter.id()))
                    .ok_or(provenant_providers::ProviderError::UnknownIdentity)?;
                let session = outcome
                    .session()
                    .ok_or(provenant_providers::ProviderError::UnknownIdentity)?;
                provenant_providers::opencode::observe_model(&data, session)
            });
        let opencode_model = match opencode_model {
            Some(Ok(model)) => Some(model),
            Some(Err(_)) => {
                rollout_error = Some(
                    provenant_records::diagnostic::ProviderFailureFact::OpenCodeIdentity
                        .reason()
                        .to_owned(),
                );
                None
            }
            None => None,
        };
        let rollout = rollout.and_then(Result::ok);
        let observed_model = if model_observation == provenant_records::ModelObservation::Rollout {
            // Missing or invalid rollout evidence cannot fall back to another source.
            rollout.as_ref().map(|turn| turn.model.as_str())
        } else if model_observation == provenant_records::ModelObservation::SessionDatabase {
            opencode_model.as_deref()
        } else {
            outcome.model()
        };
        let mut decoded = run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: true,
            observed_route: observed_model.map(|model| run::RouteRecord {
                extra: Default::default(),
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
                    } else if opencode_model.is_some() {
                        "opencode_session"
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
            partial_signals: Vec::new(),
        };
        let mut has_usage = false;
        let mut early_stop = false;
        for event in &events {
            match event {
                ProviderEvent::Text(text) => decoded.result.push_str(text),
                ProviderEvent::MessageStart => {
                    if !decoded.result.is_empty() && !decoded.result.ends_with('\n') {
                        decoded.result.push('\n');
                    }
                }
                ProviderEvent::EarlyStop { .. } => {
                    decoded
                        .partial_signals
                        .push(run::PartialSignal::ProviderStop);
                    early_stop = true;
                }
                ProviderEvent::Usage {
                    input,
                    output,
                    cache_read,
                    cache_write,
                    cache_write_1h,
                } => {
                    has_usage = true;
                    decoded.usage.uncached_input = Some(*input);
                    decoded.usage.output_excluding_reasoning = Some(*output);
                    decoded.usage.cache_read = *cache_read;
                    decoded.usage.cache_write = *cache_write;
                    decoded.usage.cache_write_1h = *cache_write_1h;
                    decoded.usage.source = Some("provider".into());
                }
                _ => {}
            }
        }
        if provenant_records::AdapterSpec::get(adapter.id())
            .is_some_and(|spec| spec.deduplicate_result)
        {
            // The provider can repeat the whole answer across its streamed response and
            // result, or within the result itself. Leave distinct text intact.
            let answer = decoded.result.trim();
            let mut middle = answer.len() / 2;
            while !answer.is_char_boundary(middle) {
                middle -= 1;
            }
            let (left, right) = answer.split_at(middle);
            let first = left.trim_end();
            let second = right.trim_start();
            if !first.is_empty() && first == second && first.len() + second.len() < answer.len() {
                decoded.result = first.to_owned();
            }
        }
        if provenant_records::AdapterSpec::get(adapter.id()).is_some_and(|spec| spec.exit_route) {
            decoded.observed_route = Some(run::RouteRecord {
                source: Some("command_exit".into()),
                ..route.clone()
            });
            if exit.stdout.bytes > stdout.len() as u64 {
                decoded
                    .result
                    .insert_str(0, "[output truncated; retained tail]\n");
            }
        }
        if result_event_usage && !has_usage {
            let has_result = stdout.split(|&b| b == b'\n').any(|line| {
                serde_json::from_slice::<serde_json::Value>(line)
                    .is_ok_and(|event| event["type"] == "result")
            });
            decoded.usage.source = Some(if has_result {
                "absent: result event has no usage".into()
            } else {
                "absent: no result event".into()
            });
        }
        let failure_cause = if events.contains(&ProviderEvent::AuthRequired) {
            provenant_providers::FailureCause::AuthRequired
        } else {
            adapter.failure_cause(stderr)
        };
        let auth = failure_cause == provenant_providers::FailureCause::AuthRequired;
        let provider_failed = outcome.status() == OutcomeStatus::Failed
            && failure_cause == provenant_providers::FailureCause::Provider;
        (decoded.outcome, decoded.reason) = match outcome.status() {
            OutcomeStatus::Success => (run::Outcome::Succeeded, None),
            _ if auth && outcome.status() != OutcomeStatus::Success => (
                run::Outcome::AuthRequired,
                Some(
                    self.roots
                        .and_then(|roots| {
                            let config = provenant_config::read_with_domains(
                                roots,
                                &crate::domains::SECTIONS,
                            )
                            .ok()?;
                            Some(crate::quota::account_login_diagnostic(
                                config.domains.accounts.entries.values(),
                                &[],
                                adapter.id(),
                                route.account_key.as_deref(),
                                &provenant_records::RouteExclusionReason::AuthRequired,
                            ))
                        })
                        .unwrap_or_else(|| {
                            provenant_records::diagnostic::ProviderFailureFact::AuthRequired
                                .reason()
                                .into()
                        }),
                ),
            ),
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
        if decoded.outcome == run::Outcome::Succeeded && early_stop {
            decoded.reason = Some(
                provenant_records::diagnostic::ProviderFailureFact::EarlyStop
                    .reason()
                    .into(),
            );
        }
        if let Some(ProviderEvent::Denied { .. }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::Denied { .. }))
        {
            if decoded.outcome == run::Outcome::Failed
                && provider_failed
                && code == Some(0)
                && !timed_out
                && !events
                    .iter()
                    .any(|event| matches!(event, ProviderEvent::Error))
            {
                decoded.reason = Some(
                    provenant_records::diagnostic::ProviderFailureFact::Denied
                        .reason()
                        .into(),
                );
            }
        } else if let Some(ProviderEvent::QuotaLimited {
            message: _,
            pool,
            resets_in_seconds,
        }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::QuotaLimited { .. }))
        {
            decoded.outcome = run::Outcome::QuotaLimited;
            let fact = provenant_records::diagnostic::ProviderFailureFact::Quota {
                free_pool: pool.as_deref() == Some("opencode-free"),
            };
            // Routing reads the shared quota store, which a lane can neither
            // read nor write: a nested refusal stays in the attempt reason
            // rather than landing in a child store nothing routes from. A
            // refusal without a pool holds only this model on a known account.
            let lane = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some();
            if !lane && (pool.is_some() || route.account_key.is_some()) {
                if let Some(state_root) = provider_state.ancestors().nth(5) {
                    let now = SystemClock::new().unix_seconds();
                    let resets = resets_in_seconds
                        .and_then(|seconds| i64::try_from(seconds).ok())
                        .and_then(|seconds| now.checked_add(seconds));
                    crate::quota::persist_provider_refusal(
                        state_root,
                        route,
                        pool.as_deref(),
                        now,
                        resets,
                    )
                    .map_err(|e| e.to_string())?;
                } else if self.roots.is_some() {
                    return Err("quota evidence has no state root".into());
                }
            }
            decoded.reason = Some(fact.reason().into());
        } else if let Some((outcome, fact)) = events.iter().find_map(|event| match event {
            ProviderEvent::RateLimited { .. } => Some((
                run::Outcome::RateLimited,
                provenant_records::diagnostic::ProviderFailureFact::RateLimit,
            )),
            ProviderEvent::ModelUnavailable { .. } => Some((
                run::Outcome::ModelUnavailable,
                provenant_records::diagnostic::ProviderFailureFact::ModelUnavailable,
            )),
            ProviderEvent::ModelOverloaded { .. } => Some((
                run::Outcome::ModelUnavailable,
                provenant_records::diagnostic::ProviderFailureFact::Capacity,
            )),
            _ => None,
        }) {
            let mut capacity_config_failed = false;
            if fact == provenant_records::diagnostic::ProviderFailureFact::Capacity
                && (host_settlement
                    || std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_none())
                && let Some(roots) = self.roots
            {
                match provenant_config::read_with_domains(roots, &crate::domains::SECTIONS) {
                    Ok(config) => {
                        let duration = config.domains.routing.capacity_cooldown_seconds;
                        if duration != 0 {
                            // Capacity evidence is advisory; failure must not affect settlement.
                            let _ = crate::quota::persist_capacity_cooldown(
                                self.state_root.unwrap_or_else(|| roots.state.as_path()),
                                route,
                                SystemClock::new().unix_seconds(),
                                duration,
                            );
                        }
                    }
                    Err(_) => capacity_config_failed = true,
                }
            }
            decoded.outcome = outcome;
            let mut reason = fact.reason().to_owned();
            if capacity_config_failed {
                reason.push_str("; capacity cooldown configuration unavailable");
            }
            decoded.reason = Some(reason);
        }
        if let Some(ProviderEvent::DeniedNote { .. }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::DeniedNote { .. }))
        {
            let note = provenant_records::diagnostic::ProviderFailureFact::Denied.reason();
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note.into(),
            });
        }
        if decoded.outcome == run::Outcome::Succeeded
            && std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_none()
            && let Some(state_root) = provider_state.ancestors().nth(5)
        {
            // Housekeeping must not change a successful lane's outcome.
            let _ = crate::quota::persist_provider_success(
                state_root,
                route,
                SystemClock::new().unix_seconds(),
            );
        }
        if decoded.outcome == run::Outcome::ModelUnavailable
            && let Some(model) = route
                .model
                .as_deref()
                .and_then(|id| ModelId::parse(id).ok())
            && let Ok(catalogue) = provenant_routing::Catalogue::bundled()
            && catalogue.model(&model).is_none()
        {
            let nearest = catalogue.nearest_models(&model);
            if !nearest.is_empty() {
                let note = format!(
                    "selected model is not in the catalogue; nearest catalogued IDs: {}",
                    nearest.join(", ")
                );
                decoded.reason = Some(match decoded.reason.take() {
                    Some(reason) => format!("{reason}; {note}"),
                    None => note,
                });
            }
        }
        Ok(decoded)
    }
}
fn route_request(task: &run::TaskRequest) -> Result<provenant_routing::RouteRequest, String> {
    let model = |text: &str| ModelId::parse(text).map_err(|error| error.to_string());
    let (choice, effort) = match &task.route {
        run::RequestedRoute::Adapter { .. } => {
            return Err("model-less adapter routes are prepared directly".into());
        }
        run::RequestedRoute::Model {
            model: id, effort, ..
        } => (RouteChoice::Model(model(id)?), effort.clone()),
        run::RequestedRoute::Named { name, effort, .. }
        | run::RequestedRoute::Review { name, effort, .. } => (
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
        intent: provenant_routing::RouteIntent {
            writes: run::Service::has_writes(task.writer.is_some(), &task.access.write_roots),
            network: Some(task.access.network(task.writer.is_some())),
            image_input: false,
            skill_location: task.profile.require_skill_location,
            workload: if matches!(task.route, run::RequestedRoute::Review { .. }) {
                provenant_routing::Workload::Review
            } else {
                match task
                    .profile
                    .knobs
                    .get("routing.workload")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("read_heavy") => provenant_routing::Workload::ReadHeavy,
                    Some("edit_heavy") => provenant_routing::Workload::EditHeavy,
                    Some("review") => provenant_routing::Workload::Review,
                    _ => provenant_routing::Workload::General,
                }
            },
        },
        choice,
        effort,
        fallback,
        confidential: false,
    })
}

/// An open store and the identity it is scoped to.
pub struct Session {
    pub store: run::RunStore,
    pub context: run::ExecutionContext,
}
impl Session {
    pub fn service(&self) -> run::Service<'_> {
        run::Service::new(&self.store)
    }
}
fn request<'a>(clock: &'a SystemClock, cancel: &'a CancelToken) -> Result<run::Request<'a>, Fail> {
    Ok(run::Request { clock, cancel })
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
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|error| Fail::new(Status::Refused, ErrorKind::LaneContext, error.to_string()))?
        .state;
    if !state
        .as_path()
        .starts_with(roots.state.as_path().join("children"))
    {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::LaneContext,
            "child Run store is outside the lane namespace",
        ));
    }
    Ok(state)
}

fn execution_context(edge: &Edge<'_>) -> Result<run::ExecutionContext, Fail> {
    let (roots, project_id, actor_id) =
        crate::fabric::identity(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Identity, error.to_string()))?;
    Ok(run::ExecutionContext {
        state_root: execution_state(&roots, edge.home, edge.assets)?,
        project_id,
        actor_id,
        read_prompts: true,
    })
}
fn open(edge: &Edge<'_>, call: &run::Request<'_>) -> Result<Session, Fail> {
    let context = execution_context(edge)?;
    let mut store =
        run::RunStore::open(&context, call)?.with_writer_registry(edge.roots.state.as_path());
    // Inside a lane the host binding may be unreadable; the trace then omits it.
    if let Ok(host) = provenant_config::bind_host(edge.roots) {
        store = store.with_host(host);
    }
    Ok(Session { store, context })
}
fn open_read_only(
    edge: &Edge<'_>,
    call: &run::Request<'_>,
) -> Result<(run::ExecutionContext, Option<run::RunStore>), Fail> {
    let context = execution_context(edge)?;
    let store = read_only_store(&context, edge.roots.state.as_path(), call)?;
    Ok((context, store))
}
/// The read-only open of `context`'s store. Like the writable open, it reads
/// the host admission ledger from the host state root: in a nested lane
/// the store is a child directory with no ledger of its own, and without the
/// host root `lanes` reads no spacing and calls every spacing wait host capacity.
fn read_only_store(
    context: &run::ExecutionContext,
    host_state: &Path,
    call: &run::Request<'_>,
) -> Result<Option<run::RunStore>, Fail> {
    Ok(run::RunStore::open_read_only(context, call)?
        .map(|store| store.with_writer_registry(host_state)))
}
fn now(clock: &dyn Clock) -> Result<WallTime, Fail> {
    clock
        .wall_now()
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Time, error.to_string()))
}
fn fresh<T>(make: fn([u8; 16]) -> T) -> Result<T, Fail> {
    entropy_128()
        .map(make)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Entropy, error.to_string()))
}

#[cfg(test)]
#[path = "run/tests/pipeline.rs"]
mod pipeline_tests;

/// Report the sheet's review age in JSON and, once stale, in the human text.
fn attach_freshness(
    prices: &provenant_telemetry::PriceTable,
    data: &mut Value,
    human: &mut String,
) -> Result<(), Fail> {
    let today = jiff::Timestamp::from_microsecond(now(&SystemClock::new())?.as_micros())
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Time, error.to_string()))?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let freshness = prices.freshness(today);
    if freshness.stale {
        human.push_str(&format!(
            "price sheet reviewed {} days ago ({}), over the {}-day limit\nnext: review and refresh with `provenant telemetry prices refresh`\n",
            freshness.age_days,
            freshness.reviewed_at,
            freshness.max_age_days.unwrap_or(0)
        ));
    }
    data["price_freshness"] = json!(freshness);
    Ok(())
}

fn grouped_token_count(
    first: &provenant_telemetry::TokenTotal,
    second: &provenant_telemetry::TokenTotal,
    third: &provenant_telemetry::TokenTotal,
) -> String {
    let known = u128::from(first.known) + u128::from(second.known) + u128::from(third.known);
    let missing =
        u128::from(first.missing) + u128::from(second.missing) + u128::from(third.missing);
    if missing == 0 {
        known.to_string()
    } else {
        format!(">={known} ({missing} unknown)")
    }
}

fn summary_verdict_key(verdict: &str) -> String {
    provenant_records::workflow::ReviewVerdict::from_token(verdict).map_or_else(
        || verdict.to_ascii_lowercase(),
        |verdict| verdict.as_token().to_owned(),
    )
}

#[cfg(test)]
#[test]
fn graph_summary_uses_canonical_review_tokens() {
    for (stored, key) in [
        ("VERDICT: PASS", "APPROVE"),
        ("PASS", "APPROVE"),
        ("VERDICT: APPROVE", "APPROVE"),
        ("VERDICT: FAIL", "REQUEST_CHANGES"),
        ("FAIL", "REQUEST_CHANGES"),
        ("VERDICT: APPROVE_WITH_NITS", "APPROVE_WITH_NITS"),
        ("VERDICT: INCONCLUSIVE", "INCONCLUSIVE"),
        ("repair", "repair"),
        ("LANDED", "landed"),
    ] {
        assert_eq!(summary_verdict_key(stored), key);
    }
}

fn graph_quality_rows(
    facts: &[&provenant_records::AttemptFact],
    store: Option<&run::RunStore>,
    call: &run::Request<'_>,
    state_root: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
) -> Result<Vec<Value>, Fail> {
    use provenant_fabric_runtime::graph::FileJournal;
    let graph_ids: std::collections::BTreeSet<_> = facts
        .iter()
        .filter_map(|fact| fact.context.as_ref()?.graph_id.clone())
        .collect();
    let mut rows = Vec::new();
    for graph_id in graph_ids {
        let checkpoint =
            match FileJournal::read_materialised_checkpoint(state_root, project, &graph_id) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    rows.push(json!({"graph_id": graph_id, "checkpoint": "missing"}));
                    continue;
                }
                Err(_) => {
                    rows.push(json!({"graph_id": graph_id, "checkpoint": "unavailable"}));
                    continue;
                }
            };
        let mut verdicts = BTreeMap::<String, u64>::new();
        let (findings, initial_rounds, repair_rounds) = graph_quality_counts(&checkpoint.nodes);
        for node in &checkpoint.nodes {
            if let Some(gate) = &node.gate {
                *verdicts
                    .entry(
                        if gate.passed {
                            "gate_pass"
                        } else {
                            "gate_fail"
                        }
                        .into(),
                    )
                    .or_default() += 1;
            }
            if let Some(verdict) = node
                .outputs
                .as_ref()
                .and_then(|outputs| outputs.verdict.as_ref())
            {
                *verdicts.entry(summary_verdict_key(verdict)).or_default() += 1;
            }
        }
        let in_graph: Vec<_> = facts
            .iter()
            .copied()
            .filter(|fact| {
                fact.context
                    .as_ref()
                    .and_then(|context| context.graph_id.as_deref())
                    == Some(graph_id.as_str())
            })
            .collect();
        let partial = partial_signal_counts(&in_graph, store, call)?;
        rows.push(json!({
            "graph_id": graph_id,
            "checkpoint": "read",
            "checkpoint_scope": "latest_snapshot",
            "successful": checkpoint.successful(),
            "verdicts": verdicts,
            "findings": findings,
            "initial_rounds": initial_rounds,
            "repair_rounds": repair_rounds,
            "partial_signals": partial,
        }));
    }
    Ok(rows)
}

/// Advisory partial-finish signals by name over the given attempts.
fn partial_signal_counts(
    facts: &[&provenant_records::AttemptFact],
    store: Option<&run::RunStore>,
    call: &run::Request<'_>,
) -> Result<BTreeMap<String, u64>, Fail> {
    let mut partial = BTreeMap::new();
    if let Some(store) = store {
        for fact in facts {
            for signal in store.record(fact.attempt_id, call)?.partial_signals {
                *partial.entry(signal.as_str().to_owned()).or_default() += 1;
            }
        }
    }
    Ok(partial)
}

fn graph_quality_counts(
    nodes: &[provenant_fabric_runtime::graph::NodeProgress],
) -> (u64, usize, usize) {
    let findings = nodes
        .iter()
        .filter(|node| node.children.is_empty())
        .map(|node| {
            node.outputs
                .as_ref()
                .map_or(0, |outputs| outputs.findings.len() as u64)
        })
        .sum();
    let mut executed = std::collections::BTreeSet::new();
    for node in nodes {
        let Some(round) = &node.round else {
            continue;
        };
        let attempted = node.attempts.iter().any(|attempt| attempt.launched);
        if attempted || node.gate.is_some() {
            executed.insert((round.loop_id.clone(), round.round));
        }
    }
    let initial = executed.iter().filter(|(_, round)| *round == 1).count();
    let repairs = executed.iter().filter(|(_, round)| *round > 1).count();
    (findings, initial, repairs)
}

fn totals_cache_read_share(totals: &provenant_telemetry::Totals) -> Option<f64> {
    let known =
        |total: &provenant_telemetry::TokenTotal| (total.missing == 0).then_some(total.known);
    provenant_routing::cache_read_share(
        known(&totals.usage.uncached_input),
        known(&totals.usage.cache_read),
        known(&totals.usage.cache_write),
    )
}

fn cost_summary(totals: &provenant_telemetry::Totals) -> String {
    let cost = &totals.cost;
    let human = if let Some(money) = &cost.complete {
        format!("est. API-equivalent ${}", money.usd)
    } else if let Some(money) = &cost.known_lower_bound {
        format!(
            "est. API-equivalent at least ${} ({} of {} attempts priced)",
            money.usd, cost.priced_attempts, totals.attempts
        )
    } else {
        format!(
            "cost unknown ({} attempts missing cost data)",
            cost.missing_attempts
        )
    };
    format!("{} attempts, {human}\n", totals.attempts)
}

/// Compare API-equivalent lane usage with configured account plans, anchored
/// to the latest quota-history reading in the selected time window.
fn plan_value_rows(
    report: &provenant_telemetry::StatsReport,
    accounts: &provenant_providers::accounts::Settings,
    edge: &Edge<'_>,
) -> Result<Vec<Value>, Fail> {
    let until = report
        .window
        .until
        .map(|time| time.as_timestamp().as_second().saturating_sub(1));
    let history = crate::quota_history::read(
        edge.roots.state.as_path(),
        &crate::quota_history::Query {
            // The effective plan may have been recorded just before the
            // selected attempt window began.
            since: None,
            until,
            account: None,
        },
    )
    .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    let mut latest = BTreeMap::new();
    for record in history {
        latest.insert((record.provider.clone(), record.label.clone()), record);
    }
    Ok(latest
        .into_iter()
        .filter_map(|((provider, label), reading)| {
            let entry = accounts.entries.values().find(|entry| {
                entry.adapter == provider
                    && entry.label == label
                    && entry.key == reading.account_key
            })?;
            let price =
                provenant_quota::price_in_force(&entry.plans, reading.observed_unix_seconds)?;
            let (api_equivalent, complete) = api_equivalent_for_account(report, &provider, &label);
            Some(json!({
                "account": label,
                "provider": provider,
                "plan": reading.plan.as_deref().or(Some(price.name.as_str())),
                "observed_unix_seconds": reading.observed_unix_seconds,
                "monthly_price": {"amount": price.price, "currency": price.currency},
                "notional_usd_month": price.notional_usd,
                "api_equivalent_usd_in_selected_window": api_equivalent,
                "api_equivalent_complete": complete,
                "quota_windows": reading.windows.iter().map(|window| json!({
                    "pool": window.pool,
                    "used_basis_points": window.used_basis_points,
                    "window_seconds": window.window_seconds,
                })).collect::<Vec<_>>(),
                "comparison_basis": "selected_window_api_equivalent_vs_monthly_plan_price"
            }))
        })
        .collect::<Vec<_>>())
}

fn api_equivalent_for_account(
    report: &provenant_telemetry::StatsReport,
    provider: &str,
    account: &str,
) -> (Option<String>, bool) {
    let mut micros = 0u128;
    let mut found = false;
    let mut complete = true;
    for route in &report.routes {
        if route.route.provider.as_deref() != Some(provider)
            || route.route.account_label.as_deref() != Some(account)
        {
            continue;
        }
        found = true;
        if let Some(cost) = &route.totals.cost.complete {
            micros += u128::from(cost.microdollars);
        } else if let Some(cost) = &route.totals.cost.known_lower_bound {
            micros += u128::from(cost.microdollars);
            complete = false;
        } else {
            complete = false;
        }
        if route.totals.cost.complete.is_none() {
            complete = false;
        }
    }
    let dollars = format!("{}.{:06}", micros / 1_000_000, micros % 1_000_000);
    (found.then_some(dollars), complete)
}

fn stats_summary(report: &provenant_telemetry::StatsReport) -> String {
    let totals = &report.totals;
    let mut text = cost_summary(totals);
    if totals.attempts == 0
        && let Some(project) = report.window.project
    {
        text.push_str(&format!(
            "no terminal attempts recorded for project {project}; run from its checkout to see its lanes\n"
        ));
    }
    // Only figures that exist: a source with no cache split adds no line, and
    // the JSON carries the counts of what was not reported.
    let mut share = |label: &str, rows: &[provenant_telemetry::CacheGroup]| {
        for row in rows.iter().filter(|row| row.cache.reporting > 0) {
            let name = [&row.provider, &row.model, &row.account]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" ");
            let name = if name.is_empty() {
                String::new()
            } else {
                format!(" {}", plain_text(&name, usize::MAX))
            };
            text.push_str(&format!(
                "cache read {label} [{}]{name}: {}\n",
                row.source,
                crate::quota::cache_share(&row.cache)
            ));
        }
    };
    share("by source", &report.cache.by_source);
    share("by model", &report.cache.by_model);
    share("by account", &report.cache.by_account);
    if let Some(chair) = &report.chair {
        let unread: u64 = chair.unread.values().sum();
        text.push_str(&format!(
            "chair: {} sessions on this host ({unread} unread), {}\n",
            chair.sessions,
            chair.cost.complete.as_ref().map_or_else(
                || chair.cost.known_lower_bound.as_ref().map_or_else(
                    || "cost unknown".to_owned(),
                    |money| format!("est. API-equivalent at least ${}", money.usd),
                ),
                |money| format!("est. API-equivalent ${}", money.usd)
            )
        ));
    }
    text
}

/// Offline lane timeline: terminal attempts as Chrome Trace Event JSON.
/// Load and validate the bundled price sheet text and table.
fn load_price_sheet(
    edge: &Edge<'_>,
) -> Result<
    (
        provenant_telemetry::ConfiguredPrices,
        provenant_telemetry::PriceTable,
    ),
    Fail,
> {
    let path = edge.assets.join("config/telemetry-prices.toml");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        Fail::new(
            Status::Error,
            ErrorKind::Prices,
            format!("{}: {error}", path.display()),
        )
    })?;
    let config = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Prices, error.to_string()))?;
    let table = provenant_telemetry::validate_prices(&config)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Prices, error.to_string()))?;
    Ok((config, table))
}

/// Read one price document from a file or fetch it over HTTPS.
fn price_document(
    source: provenant_telemetry::refresh::Source,
    file: Option<&Path>,
    home: &Path,
) -> Result<(Value, String), Fail> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    const LIMIT: usize = 64 * 1024 * 1024;
    let io = |message: String| Fail::new(Status::Error, ErrorKind::Io, message);
    let (bytes, origin) = if let Some(file) = file {
        let bytes =
            std::fs::read(file).map_err(|error| io(format!("{}: {error}", file.display())))?;
        (bytes, format!("file:{}", file.display()))
    } else {
        let url = source.url();
        let argv: Vec<std::ffi::OsString> = [
            "curl",
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "60",
            url,
        ]
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
        let mut options = ExecOptions::new(&argv, home, Duration::from_secs(65));
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: LIMIT };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let out = SystemExecutor::new(1)
            .and_then(|executor| executor.execute(options))
            .map_err(|error| io(format!("{url}: {error}")))?;
        if out.timed_out || out.stdout.truncated || out.termination != Termination::Exit(0) {
            return Err(io(format!("{url}: fetch failed")));
        }
        (out.stdout.bytes, url.to_owned())
    };
    let document = serde_json::from_slice(&bytes).map_err(|error| {
        Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            format!("{origin}: {error}"),
        )
    })?;
    Ok((document, origin))
}

pub fn prices_refresh(command: PricesRefresh, edge: &Edge<'_>) -> Reply {
    let mut out = reply(
        "telemetry prices refresh",
        prices_refresh_inner(command, edge),
    );
    // A proposal awaiting review is not a failure, but is not "nothing to do".
    if out.result.status == Status::Ok && out.result.data["clean"] == false {
        out.result.status = Status::Pending;
    }
    out
}
/// True when both paths name one file, through symlinks, `..` or hard links.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let resolve = |path: &Path| {
        let name = path.file_name()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::canonicalize(parent).ok().map(|p| p.join(name))
    };
    let resolved = |path: &Path| std::fs::canonicalize(path).ok().or_else(|| resolve(path));
    if resolved(a).is_some_and(|a| Some(a) == resolved(b)) {
        return true;
    }
    matches!((std::fs::metadata(a), std::fs::metadata(b)), (Ok(a), Ok(b)) if a.dev() == b.dev() && a.ino() == b.ino())
}
fn prices_refresh_inner(command: PricesRefresh, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use provenant_telemetry::refresh::{Source, propose};
    let (sheet, _) = load_price_sheet(edge)?;
    let out_path = edge.cwd.join(&command.out);
    if same_file(&out_path, &edge.assets.join("config/telemetry-prices.toml")) {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "--out must not be the price sheet; the proposal never replaces it",
        ));
    }
    let (primary, cross) = match command.source {
        crate::PriceSource::Openrouter => (Source::Openrouter, Source::Litellm),
        crate::PriceSource::Litellm => (Source::Litellm, Source::Openrouter),
    };
    let file = |source| match source {
        Source::Openrouter => command.openrouter_file.as_deref(),
        Source::Litellm => command.litellm_file.as_deref(),
    };
    let home = edge.home;
    let (primary_doc, primary_origin) = price_document(primary, file(primary), home)?;
    let (cross_doc, cross_origin) = price_document(cross, file(cross), home)?;
    let clock = SystemClock::new();
    let fetched_at = jiff::Timestamp::from_microsecond(now(&clock)?.as_micros())
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Time, error.to_string()))?;
    let parse = |source: Source, document: &Value, origin: &str| {
        source.parse(document).map_err(|error| {
            Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                format!("{origin}: {error}"),
            )
        })
    };
    let primary_rates = parse(primary, &primary_doc, &primary_origin)?;
    let cross_rates = parse(cross, &cross_doc, &cross_origin)?;
    let proposal = propose(&sheet, (primary, &primary_rates), (cross, &cross_rates));
    let data = json!({
        "primary": {"source": primary, "url": primary_origin},
        "cross_check": {"source": cross, "url": cross_origin},
        "fetched_at": fetched_at.to_string(),
        "effective_from": fetched_at.to_zoned(jiff::tz::TimeZone::UTC).date().to_string(),
        "proposal": proposal,
        "applied": false,
        "proposal_file": command.out,
    });
    let text = serde_json::to_string_pretty(&data)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    std::fs::write(&out_path, format!("{text}\n")).map_err(|error| {
        Fail::new(
            Status::Error,
            ErrorKind::Io,
            format!("{}: {error}", out_path.display()),
        )
    })?;
    let mut human = format!(
        "{} proposed rate changes, {} discrepancies above threshold, {} models missing from {}\nproposal written to {} (price sheet not edited)\n",
        proposal.changes.len(),
        proposal.discrepancies.len(),
        proposal.missing.len(),
        plain_text(&primary_origin, 256),
        command.out.display()
    );
    if proposal.is_clean() {
        human.push_str("next: nothing to review\n");
    } else {
        human.push_str("next: review the proposal, then edit config/telemetry-prices.toml\n");
    }
    let mut data = data;
    data["clean"] = json!(proposal.is_clean());
    Ok((data, human))
}

pub fn trace(command: Trace, edge: &Edge<'_>) -> Reply {
    reply("telemetry trace", trace_inner(command, edge))
}
fn trace_inner(command: Trace, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let since = command
        .since
        .map(|t| WallTime::from_micros(t.as_microsecond()))
        .transpose()
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let selected_run = command
        .run
        .as_ref()
        .map(|target| targets::run_id(edge, target))
        .transpose()?;
    let (_, store) = open_read_only(edge, &call)?;
    let mut facts = store
        .map(|store| {
            store
                .attempts(
                    run::AttemptFilter {
                        run_id: selected_run,
                        task_id: None,
                    },
                    &call,
                )?
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut hosts = provenant_telemetry::Hosts::default();
    let mut peers = Vec::new();
    if command.peers
        && let Some(found) = remote::peer_facts(edge, selected_run)?
    {
        facts.extend(found.facts);
        hosts.local = Some(found.local);
        hosts.names = found.names;
        hosts.names.insert(found.local, "this host".to_owned());
        peers = found.hosts;
    }
    let trace = provenant_telemetry::trace(&facts, &hosts, since);
    let events = trace["traceEvents"].as_array().map_or(0, Vec::len);
    let bytes = serde_json::to_vec(&trace)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    std::fs::write(&command.out, bytes).map_err(|error| {
        Fail::new(
            Status::Error,
            ErrorKind::Io,
            format!("{}: {error}", command.out.display()),
        )
    })?;
    let out = command.out.display().to_string();
    let mut human = format!("{events} events written to {out}; open in ui.perfetto.dev\n");
    for row in &peers {
        if let Some(error) = row["error"].as_str() {
            human.push_str(&format!(
                "peer {} not read: {}\n",
                row["host"].as_str().unwrap_or("?"),
                plain_text(error, usize::MAX)
            ));
        }
    }
    Ok((
        json!({ "out": out, "events": events, "peers": peers }),
        human,
    ))
}

/// A switch handoff is a snapshot of active lane IDs, never a parallel queue.
pub fn chair_snapshot(edge: &Edge<'_>) -> Result<(provenant_core::ProjectId, Vec<String>), Fail> {
    // Identity discovery has its own bounded Git and binding calls. Do not
    // charge that setup to the short store-read budget, especially on first use.
    let context = execution_context(edge)?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    // A handoff only reads lanes. A fresh store has none; avoid paying for
    // database creation, WAL setup and schema migrations just to discover that.
    let Some(store) = run::RunStore::open_read_only(&context, &call)? else {
        return Ok((context.project_id, Vec::new()));
    };
    let mut lanes = Vec::new();
    let mut after = 0;
    loop {
        let page = store.lanes(
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
                ErrorKind::Limit,
                "too many active lanes for a handoff",
            ));
        }
        match page.next {
            Some(next) => after = next,
            None => break,
        }
    }
    Ok((context.project_id, lanes))
}

#[cfg(test)]
#[path = "run/tests/writer_disk.rs"]
mod writer_disk_tests;

#[cfg(test)]
#[path = "run/tests/lane_depth.rs"]
mod lane_depth_tests;

#[cfg(test)]
mod tests;
