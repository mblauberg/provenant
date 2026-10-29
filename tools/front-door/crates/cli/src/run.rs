//! Run's composition edge: `dispatch`, `lanes`, `output`, `cancel`, `resume`,
//! `reconcile` and `telemetry stats`, and the provider seam that the
//! same-binary guardian holder shares.
use crate::{
    Cancel, Condition, Dispatch, Lanes, Mode, Output, Part, Reconcile, Resume, Stats, Switch,
    Target, Trace, limits,
};
use provenant_core::{
    ActorId, AttemptId, CancelToken, Clock, DeliveryId, OperationId, Roots, RunId, TaskId,
    WaitBudget, WallTime,
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

mod children;
mod provider;
mod remote;
mod review;
mod targets;
mod watch;
pub use children::HostHolder;
pub(crate) use provider::find_executable as provider_executable;
#[cfg(feature = "fixture")]
pub use provider::{prepare_fixture, writer_fixture_plan};
pub use review::review;

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
    pub detail: Option<provenant_fabric_runtime::coordination::facade::FacadeError>,
}
impl Fail {
    fn new(status: Status, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            message: message.into(),
            detail: None,
        }
    }
}
impl From<run::RunError> for Fail {
    fn from(error: run::RunError) -> Self {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        let detail = match &error {
            run::RunError::WorktreeUnregistered(path) => Some(FacadeError::WorktreeUnregistered {
                path: path.to_string_lossy().into_owned(),
            }),
            run::RunError::Filesystem(error) | run::RunError::Publication(error) => {
                Some(FacadeError::filesystem(error))
            }
            run::RunError::OperationIo {
                operation, path, ..
            } => Some(FacadeError::Io {
                operation: (*operation).into(),
                path: path
                    .as_ref()
                    .filter(|path| path.is_absolute())
                    .map(|path| path.to_string_lossy().into_owned()),
            }),
            _ => None,
        };
        let kind = ErrorKind::from(&error);
        let status = match kind {
            ErrorKind::Invalid
            | ErrorKind::OperationConflict
            | ErrorKind::NotFound
            | ErrorKind::WorktreeMissing => Status::Invalid,
            ErrorKind::WorktreeUnregistered
            | ErrorKind::Refused
            | ErrorKind::Nonterminal
            | ErrorKind::ProviderStateExpired
            | ErrorKind::Conflict
            | ErrorKind::LaneDepth
            | ErrorKind::QueueFull
            | ErrorKind::SecretInput
            | ErrorKind::Unsupported
            | ErrorKind::ContinuationUnsupported => Status::Refused,
            _ => Status::Error,
        };
        Self {
            detail,
            ..Self::new(status, kind, error.to_string())
        }
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
            summary: None,
            read_only: false,
        },
        Err(fail) => {
            let mut result = CommandResult::new(command, fail.status, Value::Null);
            result.error = Some(CommandError {
                code: fail.kind.code().into(),
                message: fail.message,
                cause: None,
                repair: None,
            });
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
    /// Each adapter supports resume; the attempt must carry a session handle.
    pub fn continues_sessions(&self) -> bool {
        !REGISTRY.is_empty()
    }
    /// Refuse before the attempt's one continuation is spent.
    fn check_continuable(&self, prior: &run::AttemptRecord) -> Result<(), Fail> {
        if !matches!(prior.state, run::AttemptState::Terminal { .. }) {
            return Err(run::RunError::Nonterminal.into());
        }
        let unsupported = |message: String| {
            Fail::new(Status::Refused, ErrorKind::ContinuationUnsupported, message)
        };
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
        let (routing, accounts, quota) = match context_roots.as_ref().or(self.roots) {
            Some(roots) => {
                let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
                    .map_err(|error| error.to_string())?;
                (
                    config.domains.routing,
                    config.domains.accounts.with_derived_accounts(),
                    // Unreadable quota is unknown, and unknown is not spent.
                    crate::quota::read_cache_at(roots.state.as_path()).unwrap_or_default(),
                )
            }
            None => Default::default(),
        };
        let now = jiff::Timestamp::now().as_second();
        let policy = routing
            .model_policy
            .effective(&model, route.account_key.as_deref(), now)
            .policy;
        let assessment = assess_model_result(&policy, task, record, decoded, policy_attempt)?;
        if assessment.refusal.is_some() || assessment.retry.is_some() {
            return Ok(assessment);
        }
        let model_pool = provenant_routing::Catalogue::bundled()
            .ok()
            .and_then(|catalogue| catalogue.quota_pool(&model).map(str::to_owned));
        let other_account = |adapter: &str, key: &str| {
            unspent_alternative(
                accounts
                    .entries
                    .values()
                    .filter(|entry| entry.adapter == adapter)
                    .map(|entry| entry.key.as_str()),
                &quota,
                key,
                &model,
                model_pool.as_deref(),
                now,
            )
        };
        Ok(assess_settlement_retry(
            &routing,
            other_account,
            task,
            record,
            decoded,
            policy_attempt,
            now,
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
    }

    fn failure_diagnostic(&self, route: &run::RouteRecord, stderr: &[u8]) -> Option<String> {
        REGISTRY
            .iter()
            .find(|entry| Some(entry.adapter.id()) == route.provider.as_deref())?
            .adapter
            .failure_diagnostic(stderr)
    }
    fn decode_with_context(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
        context: &run::HolderContext,
    ) -> Result<run::Decoded, String> {
        let (configuration_root, _) = context.disk_roots();
        // A child store must never write host capacity evidence.
        if configuration_root != context.roots.state_root {
            return Registry::default().decode(route, stdout, exit, provider_state);
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
                let mut decoded =
                    Registry::default().decode(route, stdout, exit, provider_state)?;
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
            state_root: Some(configuration_root),
            ..Default::default()
        };
        registry.decode_inner(route, stdout, exit, provider_state, true)
    }
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
        self.decode_inner(route, stdout, exit, provider_state, false)
    }
}
fn assess_model_result(
    policy: &provenant_routing::ModelPolicy,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    policy_attempt: u64,
) -> Result<run::ResultAssessment, String> {
    let failed_check = !policy.result_valid(&decoded.result);
    if policy.result.check == Some(true)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Failed | run::Outcome::Uncommitted
        )
        && (failed_check || decoded.outcome == run::Outcome::Failed)
    {
        let retry = if task.writer.is_none()
            && task.access.write_roots.is_empty()
            && record.continuation.as_deref() != Some("policy_fallback")
        {
            // Caller alternatives take precedence over the bundled fallback.
            let fallback = task.fallback.first().cloned().or_else(|| {
                policy
                    .result
                    .fallback
                    .as_ref()
                    .map(|model| run::RouteRecord {
                        provider: Some(model.adapter().into()),
                        model: Some(model.as_str().into()),
                        effort: policy.result.fallback_effort.clone(),
                        ..Default::default()
                    })
            });
            fallback.and_then(|fallback| fallback.model.map(|model| run::PolicyRetry {
                schema_version: 1,
                route: run::RequestedRoute::Model { extra: Default::default(), provider: fallback.provider.unwrap_or_default(), model, effort: fallback.effort },
                resume: false, max_attempts: 2,
                instruction: "The previous read-only route failed its result check. Complete the original task and include concrete evidence in your final answer.".into(),
                not_before_unix_seconds: None,
            }))
        } else {
            None
        };
        return Ok(run::ResultAssessment {
            refusal: (failed_check && decoded.outcome == run::Outcome::Succeeded).then(|| {
                "model policy result check failed: empty answer or missing expected sections".into()
            }),
            retry,
            note: None,
        });
    }
    if policy.lane.stop_early_ok == Some(false)
        && matches!(
            decoded.outcome,
            run::Outcome::Succeeded | run::Outcome::Uncommitted
        )
        && decoded.complete
        && decoded
            .result
            .trim_end()
            .lines()
            .last()
            .is_some_and(|line| line.trim() == "Status: incomplete")
    {
        let route = record
            .applied_route
            .as_ref()
            .ok_or("continuation route missing")?;
        let model = route.model.clone().ok_or("continuation model missing")?;
        return Ok(run::ResultAssessment {
            refusal: Some("model reported unfinished work".into()),
            retry: (policy_attempt <= u64::from(policy.lane.max_continuations.unwrap_or(0))).then(|| run::PolicyRetry {
                schema_version: 1,
                route: run::RequestedRoute::Model { extra: Default::default(), provider: route.provider.clone().unwrap_or_default(), model, effort: route.effort.clone() },
                resume: true, max_attempts: u64::from(policy.lane.max_continuations.unwrap_or(0)) + 1,
                instruction: "Continue the unfinished task in this session. Carry out the remaining work and verification. End with Status: complete only when done; otherwise Status: incomplete.".into(),
                not_before_unix_seconds: None,
            }),
            note: None,
        });
    }
    Ok(Default::default())
}
/// The reason prefix of a provider capacity settlement, shared by decode,
/// retry and `lanes`.
const CAPACITY: &str = "provider capacity";
fn capacity_reason(reason: &str) -> bool {
    reason
        .strip_prefix(CAPACITY)
        .is_some_and(|rest| rest.starts_with(": "))
}
/// A lane that settled for provider capacity or a spent account is typed, then
/// retried within `domains.routing.settlement_retries`: a capacity stop resumes
/// its session after a growing backoff, or redraws when it has none, and a
/// quota stop redraws at once, where admission skips the spent account.
/// Whether another registered account for `model`'s adapter, other than
/// `spent_key`, serves the model and is not known to be spent: no live provider
/// refusal for the model, and no reading that routing's rule calls spent.
fn unspent_alternative<'k>(
    keys: impl IntoIterator<Item = &'k str>,
    quota: &[provenant_quota::AccountRecord],
    spent_key: &str,
    model: &ModelId,
    model_pool: Option<&str>,
    now: i64,
) -> bool {
    let adapter = model.adapter();
    let spent = |key: &str| {
        quota.iter().any(|record| {
            record.account.adapter == adapter
                && record.account.account_key == key
                && (record.snapshot.as_ref().is_some_and(|snapshot| {
                    provenant_routing::quota_reading_spent(snapshot, model, model_pool, now)
                }) || record.refusals.iter().any(|refusal| {
                    refusal.model == model.model()
                        && refusal
                            .expires_unix_seconds()
                            .is_ok_and(|expires| expires > now)
                }))
        })
    };
    keys.into_iter().any(|key| {
        key != spent_key
            && provenant_records::AdapterSpec::get(adapter)
                .is_none_or(|spec| spec.account_serves_model(key, model.model()))
            && !spent(key)
    })
}
fn assess_settlement_retry(
    config: &provenant_routing::RoutingConfig,
    other_account: impl Fn(&str, &str) -> bool,
    task: &run::TaskRequest,
    record: &run::AttemptRecord,
    decoded: &run::Decoded,
    policy_attempt: u64,
    now_unix_seconds: i64,
) -> run::ResultAssessment {
    let max_attempts = u64::from(config.settlement_retries) + 1;
    let capacity = decoded.outcome == run::Outcome::ModelUnavailable
        && decoded.reason.as_deref().is_some_and(capacity_reason);
    let quota = decoded.outcome == run::Outcome::QuotaLimited;
    let Some(route) = record.applied_route.as_ref() else {
        return Default::default();
    };
    if !(capacity || quota) || config.settlement_retries == 0 || policy_attempt >= max_attempts {
        return Default::default();
    }
    let writes = task.writer.is_some() || !task.access.write_roots.is_empty();
    let applied = || {
        Some(run::RequestedRoute::Model {
            extra: Default::default(),
            provider: route.provider.clone()?,
            model: route.model.clone()?,
            effort: route.effort.clone(),
        })
    };
    // Resume only where the adapter's launch table says it can.
    let session = decoded.session.as_deref().is_some_and(|s| !s.is_empty())
        && route
            .provider
            .as_deref()
            .and_then(provenant_records::AdapterSpec::get)
            .is_some_and(|spec| spec.launch.resume);
    let (route_choice, resume) = if capacity && session {
        (applied(), true)
    } else if quota {
        // A pinned account is the caller's choice. Otherwise another unspent
        // account must serve the applied model, and the retry keeps that model
        // so a redraw cannot reach the spent account through another model;
        // admission then refuses the spent account and resolution falls through.
        let elsewhere = task.account.is_none()
            && route
                .provider
                .as_deref()
                .zip(route.account_key.as_deref())
                .is_some_and(|(adapter, key)| other_account(adapter, key));
        (elsewhere.then(applied).flatten(), false)
    } else if !writes && let Some(fallback) = task.fallback.first() {
        let fallback = fallback
            .model
            .clone()
            .map(|model| run::RequestedRoute::Model {
                extra: Default::default(),
                provider: fallback.provider.clone().unwrap_or_default(),
                model,
                effort: fallback.effort.clone(),
            });
        (fallback, false)
    } else {
        (Some(task.route.clone()), false)
    };
    let Some(route_choice) = route_choice else {
        return Default::default();
    };
    // Linear backoff with up to a quarter more, drawn from the attempt ID so
    // lanes that hit the same capacity error do not retry in step.
    let backoff_seconds = if capacity {
        let base = config.capacity_retry_seconds.saturating_mul(policy_attempt);
        let draw = u64::from_le_bytes(
            record.attempt_id.as_bytes()[..8]
                .try_into()
                .unwrap_or_default(),
        );
        base.saturating_add(draw % (base / 4 + 1))
            .min(config.capacity_retry_max_seconds)
            .min(config.retry_delay_ceiling_seconds)
    } else {
        0
    };
    let cause = if capacity {
        "the provider was at capacity"
    } else {
        "the account's usage limit was reached"
    };
    let instruction = if resume {
        format!(
            "This session stopped because {cause}. Continue the original task from where it stopped and complete its verification."
        )
    } else if writes {
        format!(
            "A previous attempt at this task stopped because {cause}. The worktree may hold its partial work: inspect `git status` and `git log` first, and before pushing or opening a pull request check whether the branch was already pushed and a pull request already exists. Keep what is useful, then complete the original task."
        )
    } else {
        format!(
            "A previous attempt at this task stopped because {cause}. Complete the original task."
        )
    };
    let next = policy_attempt + 1;
    let action = match (resume, quota) {
        (true, _) => "retrying this session",
        (false, true) => "retrying on the next eligible account",
        (false, false) => "redrawing the route",
    };
    let wait = if backoff_seconds == 0 {
        String::new()
    } else {
        format!(" in {backoff_seconds} s")
    };
    run::ResultAssessment {
        refusal: None,
        note: Some(format!("{action}{wait} (attempt {next} of {max_attempts})")),
        retry: Some(run::PolicyRetry {
            schema_version: 1,
            route: route_choice,
            resume,
            max_attempts,
            instruction,
            not_before_unix_seconds: (backoff_seconds > 0).then(|| {
                now_unix_seconds.saturating_add(i64::try_from(backoff_seconds).unwrap_or(600))
            }),
        }),
    }
}
impl Registry<'_> {
    fn decode_inner(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
        host_settlement: bool,
    ) -> Result<run::Decoded, String> {
        let is_claude = route.provider.as_deref() == Some("claude");
        let failed = |reason: &str| run::Decoded {
            outcome: run::Outcome::Failed,
            result: String::new(),
            complete: false,
            observed_route: None,
            // `source` names why usage is absent; records stay readable by older builds.
            usage: run::UsageRecord {
                source: is_claude.then(|| "absent: no result event".into()),
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
        let rollout = (adapter.id() == "codex").then(|| {
            outcome
                .session()
                .ok_or(provenant_providers::ProviderError::UnknownIdentity)
                .and_then(|thread| provider::observe_codex_rollout(provider_state, thread))
        });
        // The typed reason survives into the terminal record for diagnosis.
        let mut rollout_error = rollout
            .as_ref()
            .and_then(|observed| observed.as_ref().err())
            .map(|error| format!("Codex rollout identity: {error}"));
        // OpenCode's stream names no model; its session record does.
        let opencode_model = (adapter.id() == "opencode").then(|| {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| self.home.map(|home| home.join(".local/share")))
                .map(|data| data.join("opencode"))
                .ok_or(provenant_providers::ProviderError::UnknownIdentity)?;
            let session = outcome
                .session()
                .ok_or(provenant_providers::ProviderError::UnknownIdentity)?;
            provenant_providers::opencode::observe_model(&data, session)
        });
        let opencode_model = match opencode_model {
            Some(Ok(model)) => Some(model),
            Some(Err(error)) => {
                rollout_error = Some(format!("OpenCode session identity: {error}"));
                None
            }
            None => None,
        };
        let rollout = rollout.and_then(Result::ok);
        let observed_model = if adapter.id() == "codex" {
            // Missing or invalid rollout evidence cannot fall back to another source.
            rollout.as_ref().map(|turn| turn.model.as_str())
        } else if adapter.id() == "opencode" {
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
        let mut early_stop = None;
        for event in &events {
            match event {
                ProviderEvent::Text(text) => decoded.result.push_str(text),
                ProviderEvent::EarlyStop { reason } => {
                    decoded
                        .partial_signals
                        .push(run::PartialSignal::ProviderStop);
                    early_stop = Some(reason.clone());
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
        if (is_claude || adapter.id() == "claude") && !has_usage {
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
        if decoded.outcome == run::Outcome::Succeeded
            && let Some(reason) = early_stop
        {
            decoded.reason = Some(format!(
                "provider stopped early: {}",
                provenant_confinement::redact::known_shapes(reason.as_bytes())
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .unwrap_or_default()
            ));
        }
        if let Some(ProviderEvent::Denied { action }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::Denied { .. }))
        {
            if decoded.outcome == run::Outcome::Failed
                && decoded.reason.as_deref() == Some("provider failed")
                && code == Some(0)
                && !timed_out
                && !events
                    .iter()
                    .any(|event| matches!(event, ProviderEvent::Error))
            {
                decoded.reason = Some(format!("provider denied: {action}"));
            }
        } else if let Some(ProviderEvent::QuotaLimited {
            message,
            pool,
            resets_in_seconds,
        }) = events
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
            // rather than landing in a child store nothing routes from. A
            // refusal without a pool holds only this model on a known account.
            let lane = std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some();
            if !lane && (pool.is_some() || route.account_key.is_some()) {
                if let Some(state_root) = provider_state.ancestors().nth(5) {
                    let now = jiff::Timestamp::now().as_second();
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
            reason.push_str(repair);
            decoded.reason = Some(reason);
        } else if let Some((outcome, message, description)) =
            events.iter().find_map(|event| match event {
                ProviderEvent::RateLimited { message } => {
                    Some((run::Outcome::RateLimited, message, "provider rate limited"))
                }
                ProviderEvent::ModelUnavailable { message } => Some((
                    run::Outcome::ModelUnavailable,
                    message,
                    "provider model unavailable",
                )),
                ProviderEvent::ModelOverloaded { message } => {
                    Some((run::Outcome::ModelUnavailable, message, CAPACITY))
                }
                _ => None,
            })
        {
            let mut capacity_config_failed = false;
            if description == CAPACITY
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
                                jiff::Timestamp::now().as_second(),
                                duration,
                            );
                        }
                    }
                    Err(_) => capacity_config_failed = true,
                }
            }
            decoded.outcome = outcome;
            let mut reason = provenant_confinement::redact::known_shapes(message.as_bytes())
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .unwrap_or_else(|| format!("{description} (message redaction unavailable)"));
            let prefix = if outcome == run::Outcome::ModelUnavailable {
                format!("{description}: ")
            } else {
                String::new()
            };
            let config_note = if capacity_config_failed {
                "; capacity cooldown configuration unavailable"
            } else {
                ""
            };
            let mut end = reason.len().min(8192 - prefix.len() - config_note.len());
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
            reason.insert_str(0, &prefix);
            reason.push_str(config_note);
            decoded.reason = Some(reason);
        }
        if let Some(ProviderEvent::DeniedNote { action }) = events
            .iter()
            .find(|event| matches!(event, ProviderEvent::DeniedNote { .. }))
        {
            let note = format!("provider denied: {action}");
            decoded.reason = Some(match decoded.reason.take() {
                Some(reason) => format!("{reason}; {note}"),
                None => note,
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
                jiff::Timestamp::now().as_second(),
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
                    "model {} is not in the catalogue; nearest catalogued IDs: {}",
                    model.as_str(),
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
            writes: task.writer.is_some() || !task.access.write_roots.is_empty(),
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
        allow_free: false,
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
    Ok(run::Request {
        clock,
        // Each store transaction gets its own 30-second contention allowance,
        // so provider launch or a long wait never expires a later write.
        budget: WaitBudget::per_transaction(clock.mono_now(), 30)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Time, error.to_string()))?,
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
    #[serde(default)]
    build_heavy: bool,
    title: Option<String>,
    base: Option<String>,
    task_id: Option<TaskId>,
    prompt: Option<String>,
    prompt_file: Option<PathBuf>,
    route: run::RequestedRoute,
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
            ErrorKind::Invalid,
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
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    if bytes.len() as u64 > PROMPT_LIMIT {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "input exceeds 1 MiB",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "input is not UTF-8"))
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
                ErrorKind::Invalid,
                "give the effort once: ADAPTER/ID@EFFORT or --effort",
            ));
        }
        Some((model, suffix)) => (Some(model), Some(suffix.to_owned())),
        None => (model, effort),
    };
    match (model, route) {
        (Some(model), None) => {
            let id = ModelId::parse(model).map_err(|_| {
                Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "model must be ADAPTER/ID",
                )
            })?;
            Ok(run::RequestedRoute::Model {
                extra: Default::default(),
                provider: id.as_str().split('/').next().unwrap_or_default().into(),
                model: model.into(),
                effort,
            })
        }
        (None, Some(name)) => Ok(run::RequestedRoute::Named {
            extra: Default::default(),
            name: name.into(),
            effort,
        }),
        _ => Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "each task needs exactly one of model or route",
        )),
    }
}
fn fallback(models: &[String]) -> Result<Vec<run::RouteRecord>, Fail> {
    if models.len() > 2 {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "at most two fallback models",
        ));
    }
    models
        .iter()
        .map(|model| {
            ModelId::parse(model).map_err(|_| {
                Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "fallback must be ADAPTER/ID",
                )
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
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    Ok(TaskId::from_bytes(bytes))
}

/// Refuse input that matches a secret pattern before any record exists. The
/// message never repeats the input.
fn scan(prompt: &str) -> Result<(), Fail> {
    run::Service::scan(prompt).map_err(Fail::from)
}
/// Worktree access is explicit. Submission resolves and verifies the path;
/// a worktree with read-only access is refused, never run read-only.
pub fn worktree_access(writer: bool, worktree: Option<&Path>) -> Result<(), Fail> {
    match worktree {
        None if !writer => Ok(()),
        None => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeRequired,
            "worktree-write access needs --worktree, or --branch and --base with a peer --host",
        )),
        Some(_) if !writer => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeNotApplicable,
            "--worktree requires --mode worktree-write",
        )),
        Some(_) => Ok(()),
    }
}

/// Resolve dispatch access flags into the sealed record. Roots must be
/// existing directories, stored canonically, and clear of the state root;
/// the confinement compiler refuses credential, broad and Git roots.
pub fn lane_access(access: &crate::Access, state_root: &Path) -> Result<run::LaneAccess, Fail> {
    let invalid = |message: String| Fail::new(Status::Invalid, ErrorKind::Invalid, message);
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
    let network = match (access.network, postgres) {
        (Some(Switch::On), _) => Some(run::NetworkAccess::Open),
        (Some(Switch::Off), true) => Some(run::NetworkAccess::Local),
        (Some(Switch::Off), false) => Some(run::NetworkAccess::Provider),
        (None, _) => None,
    };
    // A first dispatch can precede the state root's creation; compare lexically then.
    let state =
        provenant_os::fs::canonicalize(state_root).unwrap_or_else(|_| state_root.to_path_buf());
    let resolve = |paths: &[PathBuf], flag: &str| {
        paths
            .iter()
            .map(|path| {
                let canonical = provenant_os::fs::canonicalize(path)
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
    let write_roots = resolve(&access.write_root, "--write-root")?;
    // Worktree-write protects `.git` inside the worktree, so a write root that
    // is, or contains, the worktree overlaps it and the plan is invalid. A read
    // root on the worktree is valid and stays allowed.
    if let Some(worktree) = access.worktree.as_deref() {
        let worktree =
            provenant_os::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
        if let Some(root) = write_roots.iter().find(|root| worktree.starts_with(root)) {
            return Err(invalid(format!(
                "--write-root {} overlaps the --worktree; worktree-write already grants the worktree",
                root.display()
            )));
        }
    }
    Ok(run::LaneAccess {
        extra: Default::default(),
        network,
        read_roots: resolve(&access.read_root, "--read-root")?,
        write_roots,
    })
}

/// One task of a submission, from CLI flags, a manifest or Fabric.
pub struct Submitted {
    pub build_heavy: bool,
    pub title: Option<String>,
    pub task_id: TaskId,
    pub prompt: String,
    pub base: Option<String>,
    pub pinned_base: Option<provenant_core::GitOid>,
    pub head: Option<String>,
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
    pub chair: Option<run::ChairBinding>,
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
    submit_with_host(session, edge, submission, call, None)
}

fn check_writer_disk(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    project: &Path,
    home: &Path,
) -> Result<(), String> {
    check_writer_disk_with_collection(
        settings,
        state,
        worktree,
        home,
        |mut path| {
            loop {
                match provenant_os::fs::Directory::open_anchor(path) {
                    Ok(directory) => {
                        return directory
                            .available_bytes()
                            .map_err(|error| error.to_string());
                    }
                    Err(error) if error.is_not_found() => {
                        path = path.parent().ok_or_else(|| error.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        },
        || pressure_collection(state, home, project),
    )
}

/// Run the same pressure classes as GC in a separate bounded process. A killed
/// collector leaves identity-bearing tombstones for the next sweep.
fn pressure_collection(state: &Path, home: &Path, project: &Path) -> u64 {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    let collect = || -> Result<u64, Box<dyn std::error::Error>> {
        let program = std::env::current_exe()?;
        let argv = vec![
            program.into_os_string(),
            "--state-dir".into(),
            state.as_os_str().to_owned(),
            "--project".into(),
            project.as_os_str().to_owned(),
            "gc".into(),
            "--pressure".into(),
            "--apply".into(),
            "--json".into(),
        ];
        let mut options = ExecOptions::new(&argv, home, Duration::from_secs(5));
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 1024 * 1024 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1)?.execute(options)?;
        if output.stdout.truncated || output.drain_truncated || output.stdout.sink_lost_bytes != 0 {
            return Ok(0);
        }
        let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
        let data = &value["data"];
        let temporary = data["provider_state"]["attempt_tmp"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row["bytes"].as_u64())
            .fold(0_u64, u64::saturating_add);
        Ok(data["bytes_freed"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(temporary))
    };
    collect().unwrap_or(0)
}

#[cfg(test)]
fn check_writer_disk_with(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    home: &Path,
    available: impl FnMut(&Path) -> Result<u64, String>,
) -> Result<(), String> {
    check_writer_disk_with_collection(settings, state, worktree, home, available, || 0)
}

fn check_writer_disk_with_collection(
    settings: &provenant_config::GcSettings,
    state: &Path,
    worktree: &Path,
    home: &Path,
    mut available: impl FnMut(&Path) -> Result<u64, String>,
    mut collect: impl FnMut() -> u64,
) -> Result<(), String> {
    let floor = settings
        .free_floor_bytes(state)
        .map_err(|error| error.to_string())?;
    let cache = home.join("Library/Caches/provenant-lane-cargo");
    let (mut limiting, mut minimum) = (worktree, u64::MAX);
    for path in [worktree, state, cache.as_path()] {
        let bytes = available(path)?;
        if bytes < minimum {
            (limiting, minimum) = (path, bytes);
        }
    }
    if minimum < floor {
        let reclaimed = if settings.schedule { collect() } else { 0 };
        minimum = u64::MAX;
        for path in [worktree, state, cache.as_path()] {
            let bytes = available(path)?;
            if bytes < minimum {
                (limiting, minimum) = (path, bytes);
            }
        }
        if minimum < floor {
            return Err(format!(
                "insufficient disk space on {}: {minimum} bytes available, floor {floor} bytes; bounded GC reported {reclaimed} bytes reclaimed; run `provenant gc --free-floor {floor}B` before dispatching a writer",
                limiting.display()
            ));
        }
    }
    Ok(())
}

/// Reuse the host's pinned launch inputs after its guardian has settled.
pub fn submit_with_host(
    session: &Session,
    edge: &Edge<'_>,
    submission: Submission,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    let (lane_depth, parent_attempt) = nested_dispatch_context(edge.roots)?;
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    // Nested proposals are checked by the host before provider preparation.
    // A confined parent cannot open a sibling worktree to sample its volume.
    if parent_attempt.is_none() {
        for task in &submission.tasks {
            if let Some(path) = &task.worktree {
                let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
                check_writer_disk(
                    &config.gc,
                    edge.roots.state.as_path(),
                    &writer.worktree,
                    &writer.project,
                    edge.home,
                )
                .map_err(|message| {
                    let mut fail = Fail::new(Status::Refused, ErrorKind::Refused, message.clone());
                    // Keep the volume, available bytes and floor for MCP callers.
                    fail.detail = Some(
                        provenant_fabric_runtime::coordination::facade::FacadeError::DispatchRefused {
                            reason: message,
                        },
                    );
                    fail
                })?;
            }
        }
    }
    let profile = config
        .dispatch_profile(&submission.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    let read_only = submission.tasks.iter().all(|task| task.worktree.is_none());
    let bound = (read_only
        && config.knob_is_set("lanes.max_concurrency", &submission.knobs)
        && !config.knob_is_set("lanes.max_concurrency_read_only", &submission.knobs))
    .then(|| profile.knob_u64("lanes.max_concurrency"))
    .flatten();
    let concurrency = run::concurrency(submission.concurrency, &profile, read_only, bound);
    let tasks = submission
        .tasks
        .into_iter()
        .map(|task| {
            Ok(run::DispatchTask {
                context: None,
                build_heavy: task.build_heavy,
                profile: profile.clone(),
                title: task.title,
                task_id: task.task_id,
                prompt: task.prompt,
                base: task.base,
                pinned_base: task.pinned_base,
                head: task.head,
                route: task.route,
                account: task.account,
                fallback: fallback(&task.fallback)?,
                timeout_seconds: task.provider_timeout,
                delivery_id: task.delivery_id,
                deliver: task.deliver,
                worktree: task.worktree,
                access: task.access,
            })
        })
        .collect::<Result<Vec<_>, Fail>>()?;
    let chair = parent_attempt
        .is_none()
        .then_some(submission.chair)
        .flatten();
    let record = run::Service::new(&session.store).dispatch(
        run::DispatchRequest {
            chair,
            operation_id: submission.operation_id,
            tasks,
            concurrency,
            lane_depth,
            parent_attempt,
        },
        edge.cwd,
        call,
    )?;
    if parent_attempt.is_none() {
        let canonical = session.store.request(record.run_id, call)?;
        with_chair_index(edge.roots, call, |index, call| {
            index.submit(session.context.project_id, record.run_id, &canonical, call)
        })?;
        let pending = session.store.pending_attempts(record.run_id, call)?;
        for task in &canonical.tasks {
            if let Some(writer) = &task.writer {
                provenant_confinement::worktrees::with_context(
                    edge.roots,
                    edge.home,
                    &writer.project,
                    call.clock,
                    call.cancel,
                    |context| {
                        use provenant_confinement::worktrees;
                        let tree =
                            provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                        if let Some(mut owner) = worktrees::load_owner(&tree)?
                            && owner.origin == worktrees::WorktreeOrigin::Lane
                            && owner.operation_id.as_ref() == Some(&record.operation_id)
                            && let Some(attempt) = pending
                                .iter()
                                .find(|attempt| attempt.task_id == task.task_id)
                        {
                            owner.attempt_id = Some(attempt.attempt_id);
                            worktrees::save_owner(&tree, &owner)?;
                        }
                        Ok(())
                    },
                )
                .map_err(run::RunError::from)?;
            }
        }
    }
    let started = if parent_attempt.is_some() {
        Vec::new()
    } else if let Some(host) = host {
        launch_on_host(session, record.run_id, edge, call, Some(host), None, None)?
    } else {
        launch(session, record.run_id, edge, call)?
    };
    Ok((record, started))
}

/// The main CLI path reuses admission, managed worktrees and bounded waits.
pub fn run(mut command: Dispatch, edge: &Edge<'_>) -> Reply {
    if command.host.is_none()
        && let Some(path) = &command.access.worktree
    {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            edge.cwd.join(path)
        };
        if !absolute.exists() {
            if path.is_absolute() || path.starts_with(".worktrees") {
                return reply(
                    "run",
                    Err(Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        format!(
                            "worktree {} does not exist; use --branch BRANCH to create an owned worktree",
                            path.display()
                        ),
                    )),
                );
            }
            command.worktree_branch = Some(path.to_string_lossy().into_owned());
            command.access.worktree = None;
        }
    }
    if command.route.effort.is_none()
        && let Some(model) = command.route.model.as_ref()
        && !model.0.contains('@')
        && let Ok(id) = ModelId::parse(&model.0)
    {
        let config =
            match provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS) {
                Ok(config) => config,
                Err(error) => {
                    return reply(
                        "run",
                        Err(Fail::new(
                            Status::Error,
                            ErrorKind::Config,
                            error.to_string(),
                        )),
                    );
                }
            };
        if let Ok(catalogue) = provenant_routing::Catalogue::bundled() {
            command.route.effort = catalogue.model_default_effort(&config.domains.routing, &id);
        }
    }
    render_run(dispatch(command, edge), edge)
}

fn render_run(mut result: Reply, edge: &Edge<'_>) -> Reply {
    result.result.command = "run".into();
    if result.result.error.is_some() {
        return result;
    }
    let rows = result.result.data["lanes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut human = format!(
        "operation {}\n",
        result.result.data["operation_id"]
            .as_str()
            .unwrap_or("unknown")
    );
    if let (Some(path), Some(branch)) = (
        result.result.data["worktree"]["path"].as_str(),
        result.result.data["worktree"]["branch"].as_str(),
    ) {
        human.push_str(&format!(
            "worktree {} ({})\n",
            plain_text(path, usize::MAX),
            plain_text(branch, usize::MAX)
        ));
    }
    let timed_out = result.result.status == Status::Timeout;
    let mut next = None;
    for row in rows {
        let Some(id) = row["attempt_id"].as_str() else {
            continue;
        };
        let state = row["state"].as_str().unwrap_or("unknown");
        human.push_str(&format!(
            "{} {state} {} (effort: {}, network: {})\n",
            &id[..id.len().min(8)],
            row["applied_route"]["model"]
                .as_str()
                .unwrap_or("unresolved"),
            row["applied_route"]["effort"].as_str().unwrap_or(
                if row["applied_route"].is_object() {
                    "provider default"
                } else {
                    "unresolved"
                }
            ),
            row["network"].as_str().unwrap_or("unknown")
        ));
        if let Some(reason) = row["reason"].as_str() {
            human.push_str(&format!("{}\n", plain_text(reason, usize::MAX)));
        }
        if (row["result_path"].is_string()
            || (row["host"].is_string()
                && !matches!(state, "queued" | "starting" | "running" | "settling")))
            && let Ok(attempt) = id.parse()
        {
            let tail = output(
                Output {
                    attempt,
                    host: None,
                    part: Part::Result,
                    offset: 0,
                    tail: Some(20),
                    max_bytes: 8192,
                    trace: false,
                },
                edge,
            );
            if tail.result.error.is_none() {
                human.push_str(&tail.human);
                if !human.ends_with('\n') {
                    human.push('\n');
                }
                result.result.data["result_tails"][id] = tail.result.data;
            } else {
                human.push_str(&format!(
                    "result unavailable; run `provenant output {id}`\n"
                ));
            }
        }
        // One typed next step: the first lane that needs one.
        if next.is_none() {
            if timed_out && matches!(state, "queued" | "starting" | "running" | "settling") {
                next = Some(format!("provenant lanes --id {id} --wait 4m"));
            } else if state == "input_required" {
                next = Some(format!("provenant resume {id} --prompt-file ANSWER"));
            } else if !matches!(state, "succeeded") {
                next = Some(format!("provenant output {id}"));
            }
        }
        if !timed_out
            && !row["superseded_by"].is_string()
            && !matches!(state, "succeeded" | "review_required" | "input_required")
        {
            result.result.status = Status::Failed;
        }
    }
    result.summary = Some(crate::Summary::verbatim(&human, next));
    result.human = human;
    result.read_only = false;
    result
}

/// Active attempts plus the twenty most recent settled lanes.
pub fn status(command: crate::Status, _json_output: bool, edge: &Edge<'_>) -> Reply {
    let mut summary = None;
    let mut out = reply(
        "status",
        (|| {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let resolved = command
                .id
                .as_ref()
                .map(|target| targets::resolve(edge, target, true))
                .transpose()?;
            let targets: Vec<Target> = resolved
                .as_ref()
                .map(|candidate| vec![candidate.target.clone()])
                .unwrap_or_default();
            let (context, store) = if let Some(candidate) = resolved {
                let (session, _) = targets::read_resolved(edge, candidate, &call)?;
                (session.context, Some(session.store))
            } else {
                open_read_only(edge, &call)?
            };
            let (exempt, exempt_line) = review_exempt_view(&context);
            let Some(store) = store else {
                return Ok((
                    json!({"lanes": [], "review_exempt": exempt}),
                    format!("no lanes\n{exempt_line}"),
                ));
            };
            let session = Session { store, context };
            let full = command.all || command.id.is_some();
            let mut query = lane_query(
                &session,
                &targets,
                full,
                if full { command.limit } else { 100 },
                command.cursor.unwrap_or(0),
                &call,
            )?;
            query.newest_first = true;
            let mut rows = Vec::new();
            let mut next = None;
            if full {
                let page = session.service().lanes(&query, &call)?;
                rows = page.lanes;
                next = page.next;
            } else {
                // Active lanes remain visible even when old; settled history
                // needs only enough pages to find the twenty newest results.
                loop {
                    let page = session.service().lanes(&query, &call)?;
                    rows.extend(page.lanes);
                    match page.next {
                        Some(after) => query.after = after,
                        None => break,
                    }
                }
                query.all = true;
                query.after = 0;
                let mut recent = 0;
                loop {
                    let page = session.service().lanes(&query, &call)?;
                    for lane in page.lanes {
                        if settled(&lane.state).0 {
                            rows.push(lane);
                            recent += 1;
                            if recent == 20 {
                                break;
                            }
                        }
                    }
                    if recent == 20 {
                        break;
                    }
                    match page.next {
                        Some(after) => query.after = after,
                        None => break,
                    }
                }
                rows.sort_by_key(|lane| std::cmp::Reverse(lane.queued_at));
            }
            let waits = session.store.lane_waits(&rows, &call);
            let now = now(&clock)?;
            let human = if command.all {
                let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
                render_lanes(
                    &rows,
                    &waits,
                    next,
                    session.context.state_root.as_path(),
                    lane_cooldown_view(&cooldowns),
                )
                .1
            } else {
                compact_status(&rows, &waits, now)
            };
            let continuation = next.map(|cursor| {
                let target = targets
                    .first()
                    .map(|target| {
                        [
                            target.run.map(|id| id.to_string()),
                            target.task.map(|id| id.to_string()),
                            target.attempt.map(|id| id.to_string()),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("/")
                    })
                    .unwrap_or_default();
                format!("provenant status {target} --all --cursor {cursor}")
            });
            let lines: Vec<LaneLine> = rows
                .iter()
                .map(|lane| {
                    let (state, reason) = state_and_reason(&lane.state);
                    let reason = reason.or_else(|| {
                        waits
                            .iter()
                            .find(|wait| wait.attempt_id == lane.attempt_id)
                            .map(|wait| wait.waiting_for.clone())
                    });
                    LaneLine {
                        id: lane.attempt_id.to_string(),
                        state,
                        model: lane
                            .applied_route
                            .as_ref()
                            .and_then(|route| route.model.clone())
                            .unwrap_or_else(|| "unresolved".into()),
                        reason: reason.unwrap_or_default(),
                    }
                })
                .collect();
            let mut compact = lane_summary(&lines, continuation);
            // The compact view keeps the paths a writer may change unreviewed.
            compact.lines.push(exempt_line.trim_end().to_owned());
            summary = Some(compact);
            let human = format!("{human}{exempt_line}");
            Ok((
                json!({"lanes": rows, "next_cursor": next, "review_exempt": exempt}),
                human,
            ))
        })(),
    );
    out.summary = summary;
    out.read_only = true;
    out
}

/// One lane as the compact views show it.
struct LaneLine {
    id: String,
    state: String,
    model: String,
    reason: String,
}
/// Needs action (0), still moving (1), or finished well (2).
fn lane_rank(state: &str) -> u8 {
    match state {
        "queued" | "starting" | "running" | "settling" => 1,
        "succeeded" | "cancelled" => 2,
        _ => 0,
    }
}
/// Counts by state, then the lanes that need action, then those still moving,
/// then the one typed next command. Finished lanes are counted, not listed.
fn lane_summary(lanes: &[LaneLine], continuation: Option<String>) -> crate::Summary {
    if lanes.is_empty() {
        return crate::Summary {
            lines: vec!["no lanes".into()],
            next: None,
            omitted: 0,
        };
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    for lane in lanes {
        match counts.iter_mut().find(|(state, _)| *state == lane.state) {
            Some((_, count)) => *count += 1,
            None => counts.push((lane.state.clone(), 1)),
        }
    }
    counts.sort_by_key(|(state, count)| (lane_rank(state), std::cmp::Reverse(*count)));
    let tally = crate::Summary::tally(
        &counts
            .iter()
            .map(|(state, count)| (state.as_str(), *count))
            .collect::<Vec<_>>(),
    );
    let headline = format!(
        "{} lane{}: {tally}",
        lanes.len(),
        if lanes.len() == 1 { "" } else { "s" }
    );
    let mut listed: Vec<&LaneLine> = lanes
        .iter()
        .filter(|lane| lane_rank(&lane.state) < 2)
        .collect();
    listed.sort_by_key(|lane| lane_rank(&lane.state));
    let rows = listed
        .iter()
        .map(|lane| {
            format!(
                "{} {} {} {}",
                &lane.id[..lane.id.len().min(8)],
                lane.state,
                plain_text(&lane.model, usize::MAX),
                plain_text(&lane.reason, usize::MAX)
            )
            .trim_end()
            .to_owned()
        })
        .collect();
    let next = continuation.or_else(|| {
        listed.first().map(|lane| {
            if lane_rank(&lane.state) == 0 {
                format!("provenant output {}", lane.id)
            } else {
                format!("provenant lanes --id {} --wait 4m", lane.id)
            }
        })
    });
    crate::Summary::new(headline, rows, next)
}
/// The lane rows of a `lanes` or `run` reply, in the shape `lane_summary` reads.
fn lane_lines(data: &Value) -> Vec<LaneLine> {
    data["lanes"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(LaneLine {
                        id: row["attempt_id"].as_str()?.to_owned(),
                        state: row["state"].as_str()?.to_owned(),
                        model: row["applied_route"]["model"]
                            .as_str()
                            .unwrap_or("unresolved")
                            .to_owned(),
                        reason: row["reason"]
                            .as_str()
                            .or_else(|| row["waiting_for"].as_str())
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}
/// The paths a writer may change without review in this project, as JSON and
/// as the line `status` ends with, so the chair sees what will not stop a lane.
fn review_exempt_view(context: &run::ExecutionContext) -> (serde_json::Value, String) {
    use provenant_confinement::run_config::RunConfig;
    match RunConfig::read(context.state_root.as_path()) {
        Ok(config) => {
            let paths = config.review_exempt_for(&context.project_id.to_string());
            let line = if paths.is_empty() {
                "review-exempt paths: none\n".to_owned()
            } else {
                format!("review-exempt paths: {}\n", paths.join(", "))
            };
            (json!(paths), line)
        }
        Err(_) => (
            json!([]),
            "review-exempt paths: none (config unreadable; every protected change needs review)\n"
                .to_owned(),
        ),
    }
}
fn compact_status(rows: &[run::AttemptRecord], waits: &[run::LaneWait], now: WallTime) -> String {
    if rows.is_empty() {
        return "no lanes\n".into();
    }
    rows.iter()
        .map(|lane| {
            let id = lane.attempt_id.to_string();
            let (state, reason) = state_and_reason(&lane.state);
            let model = lane
                .applied_route
                .as_ref()
                .and_then(|route| route.model.as_deref())
                .unwrap_or("unresolved");
            let age = now
                .as_micros()
                .saturating_sub(lane.queued_at.as_micros())
                .max(0)
                / 1_000_000;
            let age = if age < 60 {
                format!("{age}s")
            } else if age < 3600 {
                format!("{}m", age / 60)
            } else {
                format!("{}h", age / 3600)
            };
            let reason = reason
                .or_else(|| {
                    waits
                        .iter()
                        .find(|wait| wait.attempt_id == lane.attempt_id)
                        .map(|wait| wait.waiting_for.clone())
                })
                .unwrap_or_default();
            format!(
                "{} {state} {} {age} {}\n",
                &id[..8],
                plain_text(model, usize::MAX),
                plain_text(&reason, usize::MAX)
            )
        })
        .collect()
}

pub fn dispatch(command: Dispatch, edge: &Edge<'_>) -> Reply {
    dispatch_with_workers(
        command,
        edge,
        &provenant_confinement::worktrees::SystemWorkerObserver,
    )
}

fn dispatch_with_workers(
    command: Dispatch,
    edge: &Edge<'_>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Reply {
    let operation_id = command.operation_id.clone();
    let mut created = None;
    let mut result = dispatch_inner(command, edge, &mut created, workers);
    if let (Err(fail), Some((path, branch))) = (&mut result, created) {
        // A failed launch can follow admission. Those trees belong to the
        // accepted run and must remain available for resume.
        let cleanup = (|| -> Result<(), Fail> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let session = open(edge, &call)?;
            if session.store.operation(&operation_id, &call)?.is_some() {
                return Ok(());
            }
            provenant_confinement::worktrees::with_context(
                edge.roots,
                edge.home,
                edge.cwd,
                &clock,
                &cancel,
                |context| {
                    use provenant_confinement::worktrees;
                    let context = &worktrees::WorktreeContext {
                        workers,
                        ..*context
                    };
                    let info = worktrees::list(context, &cancel)?
                        .into_iter()
                        .find(|tree| tree.path == path)
                        .ok_or(worktrees::WorktreeError::NotFound)?;
                    if info.dirty != worktrees::WorktreeCondition::Clean
                        || info.writer != worktrees::WriterState::Free
                    {
                        return Err(worktrees::WorktreeError::Dirty);
                    }
                    let tree = provenant_confinement::verify_worktree(context.repo, &path)?;
                    if worktrees::load_owner(&tree)?.is_none_or(|owner| {
                        owner.origin != worktrees::WorktreeOrigin::Lane
                            || owner.operation_id.as_ref() != Some(&operation_id)
                    }) {
                        return Err(worktrees::WorktreeError::UnknownWriter);
                    }
                    worktrees::remove(
                        context,
                        worktrees::WorktreeRemove::Name {
                            name: branch,
                            discard: false,
                            integration: None,
                            expected_head: Some(info.head),
                        },
                        &cancel,
                    )?;
                    Ok(())
                },
            )
            .map_err(run::RunError::from)?;
            Ok(())
        })();
        if let Err(error) = cleanup {
            fail.message.push_str(&format!(
                "; new managed worktree retained for gc at {}: {}",
                path.display(),
                error.message
            ));
        }
    }
    let mut out = reply_waited("dispatch", result);
    operation_reply(&mut out, &operation_id);
    out
}
fn operation_reply(reply: &mut Reply, id: &OperationId) {
    if reply.result.data.is_null() {
        reply.result.data = json!({});
    }
    reply.result.data["operation_id"] = json!(id);
    if let Some(error) = &mut reply.result.error {
        error.message.push_str(&format!("; operation {id}"));
    } else {
        reply.human = format!("operation {id}\n{}", reply.human);
    }
}
fn check_delivery_branch(project: &Path, branch: &str) -> Result<(), Fail> {
    run::HostDeliveryContext::check_delivery_branch(project, branch)
        .map_err(|error| Fail::new(Status::Refused, ErrorKind::Refused, error.to_string()))
}

/// Refuse flags outside the named adapters' launch tables before the first
/// side effect, including `--worktree-branch` creation. Manifest tasks and
/// named routes are checked again where their adapter is known.
fn refused(error: provenant_records::UnsupportedLaunch) -> Fail {
    Fail::new(Status::Refused, ErrorKind::Unsupported, error.to_string())
}
/// A pool is refused an effort only when no member's adapter can take it.
pub(crate) fn pool_effort_refusal(
    adapters: &[&str],
    effort: &str,
) -> Option<provenant_records::UnsupportedLaunch> {
    let request = provenant_records::LaunchRequest {
        effort: Some(effort),
        ..Default::default()
    };
    let refusals: Vec<_> = adapters
        .iter()
        .filter_map(|name| provenant_records::AdapterSpec::get(name))
        .map(|spec| spec.check_launch(&request))
        .collect();
    let refused: Vec<_> = refusals.into_iter().filter_map(Result::err).collect();
    let known = adapters
        .iter()
        .filter(|name| provenant_records::AdapterSpec::get(name).is_some())
        .count();
    (known > 0 && refused.len() == known)
        .then(|| refused.into_iter().next())
        .flatten()
}

pub(crate) fn check_flag_capabilities(
    command: &Dispatch,
    roots: Option<&Roots>,
) -> Result<(), Fail> {
    let route = requested(
        command.route.model.as_ref().map(|m| m.0.as_str()),
        command.route.route.as_deref(),
        command.route.effort.clone(),
    );
    let network = match command.access.network {
        Some(Switch::On) => Some(run::NetworkAccess::Open),
        Some(Switch::Off) => Some(run::NetworkAccess::Provider),
        None => None,
    };
    let write = matches!(command.access.mode, Mode::WorktreeWrite);
    let check = |provider: &str, effort: Option<&str>| {
        let Some(spec) = provenant_records::AdapterSpec::get(provider) else {
            return Ok(());
        };
        spec.check_launch(&provenant_records::LaunchRequest {
            effort,
            resume: false,
            network,
            write,
        })
        .map_err(refused)
    };
    let check_route = |route: run::RequestedRoute| -> Result<(), Fail> {
        match run::Service::route(route) {
            Ok(run::RequestedRoute::Model {
                provider, effort, ..
            }) => check(&provider, effort.as_deref())?,
            // A pool whose every member's adapter takes no effort cannot serve one.
            Ok(run::RequestedRoute::Named {
                name,
                effort: Some(effort),
                ..
            }) => {
                if let Some(roots) = roots
                    && let Ok(members) = crate::route::effective_members(roots, &name)
                {
                    let adapters: Vec<&str> = members
                        .iter()
                        .map(|member| member.model.adapter())
                        .collect();
                    pool_effort_refusal(&adapters, &effort)
                        .map_or(Ok(()), |error| Err(refused(error)))?;
                }
            }
            _ => {}
        }
        Ok(())
    };
    let check_fallback = |model: &str, effort: Option<&str>| -> Result<(), Fail> {
        match model.split_once('/') {
            Some((provider, _)) => check(provider, effort),
            None => Ok(()),
        }
    };
    let effort_of = |route: &run::RequestedRoute| match route {
        run::RequestedRoute::Model { effort, .. }
        | run::RequestedRoute::Named { effort, .. }
        | run::RequestedRoute::Review { effort, .. } => effort.clone(),
    };
    if let Ok(route) = route {
        let effort = effort_of(&route);
        check_route(route)?;
        for model in &command.route.fallback {
            check_fallback(&model.0, effort.as_deref())?;
        }
    }
    // Manifest tasks carry their own routes; read them before any worktree exists.
    // A malformed manifest is reported later by the normal path.
    if let Some(path) = &command.input.manifest
        && let Ok(text) = read_bounded(path)
        && let Ok(manifest) = provenant_records::decode_v1::<Manifest, _>(
            text.as_bytes(),
            provenant_records::Limits {
                max_bytes: PROMPT_LIMIT,
                ..Default::default()
            },
        )
    {
        for task in manifest.tasks {
            let effort = effort_of(&task.route);
            check_route(task.route)?;
            for model in &task.fallback {
                check_fallback(model, effort.as_deref())?;
            }
        }
    }
    Ok(())
}
fn dispatch_inner(
    mut command: Dispatch,
    edge: &Edge<'_>,
    created: &mut Option<(PathBuf, String)>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
) -> Result<(Value, String, bool), Fail> {
    // Check explicit placement before local aliases are normalised away and
    // before writer validation or peer discovery can act on the request.
    if command.host.is_some() && crate::fabric::in_lane() {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::LaneCoordinationRefused,
            "coordination mutations are not allowed inside a lane",
        ));
    }
    check_flag_capabilities(&command, Some(edge.roots))?;
    // Preflight. An owned worktree is the first side effect, so every check
    // that does not depend on the tree runs before it and a refusal leaves
    // no tree behind.
    let owned = if let Some(branch) = command.worktree_branch.clone() {
        if !matches!(command.access.mode, Mode::WorktreeWrite) || crate::fabric::in_lane() {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Refused,
                "--worktree-branch requires a host worktree-write dispatch",
            ));
        }
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        // Initialise the state before entering the shared confinement context.
        let call = request(&clock, &cancel)?;
        let session = open(edge, &call)?;
        if let Some(replayed) = replay_owned_dispatch(&command, edge, &session, &call)? {
            return Ok(replayed);
        }
        if let Some(mode) = &command.deliver {
            if let Some(delivery) = run::HostDeliveryContext::destination(mode.clone(), &branch)? {
                check_delivery_branch(edge.cwd, &delivery.branch)?;
            }
        } else if let Some(target) = &command.deliver_branch {
            check_delivery_branch(edge.cwd, target)?;
        }
        let host = provenant_config::bind_host(edge.roots)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
        Some(OwnedWorktree {
            branch,
            host,
            created_at: now(&clock)?,
        })
    } else {
        worktree_access(
            matches!(command.access.mode, Mode::WorktreeWrite) && command.branch.is_none(),
            command.access.worktree.as_deref(),
        )?;
        None
    };
    let unsupported = |what: &str| {
        Err(Fail::new(
            Status::Refused,
            ErrorKind::Unsupported,
            format!("{what} is not available in this build"),
        ))
    };
    // An owned tree's write-root overlap is checked again once it exists.
    let mut lane_access = lane_access(&command.access, edge.roots.state.as_path())?;
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
            ErrorKind::Invalid,
            "--branch and --base create a writer worktree on a peer --host with \
             --mode worktree-write; on this host use `provenant worktree create` and --worktree",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    if command.route.read_heavy {
        command.knobs.push("routing.workload=read_heavy".into());
    }
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
    let mut flag_delivery =
        command
            .deliver_remote
            .zip(command.deliver_branch)
            .map(|(remote, branch)| run::WriterDeliveryRequest {
                extra: Default::default(),
                mode: Default::default(),
                remote,
                branch,
                open_pr: command.open_pr,
            });
    if let Some(mode) = &command.deliver {
        let branch = if let Some(path) = &command.access.worktree {
            let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
            run::prepare_writer(
                &writer,
                edge.roots.state.as_path(),
                execution_context(edge)?.project_id,
            )?
            .branch
            .as_str()
            .trim_start_matches("refs/heads/")
            .to_owned()
        } else if let Some(branch) = command.branch.as_ref().or(command.worktree_branch.as_ref()) {
            branch.clone()
        } else if matches!(mode, run::DeliveryMode::None) {
            String::new()
        } else {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Refused,
                "delivery needs a writer worktree",
            ));
        };
        flag_delivery = run::HostDeliveryContext::destination(mode.clone(), &branch)?;
    }
    if let Some(path) = &command.input.manifest {
        let manifest: Manifest = provenant_records::decode_v1(
            read_bounded(path)?.as_bytes(),
            provenant_records::Limits {
                max_bytes: PROMPT_LIMIT,
                ..Default::default()
            },
        )
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
        if manifest.schema_version != 1 || !(1..=64).contains(&manifest.tasks.len()) {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::Invalid,
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
                        ErrorKind::Invalid,
                        "prompt_file must be relative to the manifest, without ..",
                    ));
                }
                _ => {
                    return Err(Fail::new(
                        Status::Invalid,
                        ErrorKind::Invalid,
                        "each task needs exactly one prompt or prompt_file",
                    ));
                }
            };
            let route = run::Service::route(task.route)?;
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
                task.title,
                prompt,
                route,
                fallback,
                task.delivery_id,
                task.account,
                task.deliver.or_else(|| flag_delivery.clone()),
                task.base.or_else(|| command.base.clone()),
                task.build_heavy || command.build_heavy,
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
            command.title.clone(),
            prompt,
            requested(
                flag_route.0.as_deref(),
                flag_route.1.as_deref(),
                flag_route.2.clone(),
            )?,
            flag_route.3.clone(),
            command.delivery_id,
            command.account.clone(),
            flag_delivery,
            command.base.clone(),
            command.build_heavy,
        ));
    }
    let concurrency = if command.input.manifest.is_some() {
        command.concurrency
    } else {
        Some(1)
    };
    if let Some(host) = &command.host {
        let mut placed = Vec::new();
        for (
            index,
            (
                task_id,
                title,
                prompt,
                route,
                fallback,
                delivery_id,
                account,
                deliver,
                base,
                build_heavy,
            ),
        ) in tasks.into_iter().enumerate()
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
                build_heavy,
                title,
                base,
                head: command.head.clone(),
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
    let session = open(edge, &call)?;
    // Protection governs new admission. Service::dispatch still compares the
    // immutable inputs when an existing operation is replayed.
    if session
        .store
        .operation(&command.operation_id, &call)?
        .is_none()
    {
        for (_, _, _, _, _, _, _, delivery, _, _) in &tasks {
            if let Some(delivery) = delivery {
                check_delivery_branch(edge.cwd, &delivery.branch)?;
            }
        }
    }
    let chair = if crate::fabric::in_lane() {
        None
    } else {
        let context = crate::fabric::Context::open(edge.roots.clone(), edge.home, edge.cwd)
            .map_err(|error| Fail::new(Status::Error, ErrorKind::Identity, error.to_string()))?;
        Some(run::ChairBinding {
            extra: Default::default(),
            host: context.identity.inbox.host,
            actor: crate::workflow::session_actor(&context, None)
                .map_err(|error| Fail::new(Status::Error, ErrorKind::Identity, error))?,
        })
    };
    // Validate deterministic admission inputs before creating or claiming a tree.
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    config
        .dispatch_profile(&command.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    for (_, title, prompt, _, models, _, _, _, _, _) in &tasks {
        run::Service::scan(prompt)?;
        if let Some(title) = title {
            provenant_core::validate_task_title(title)
                .map_err(|message| Fail::new(Status::Invalid, ErrorKind::Invalid, message))?;
        }
        fallback(models)?;
    }
    // Preflight is complete: create or claim the owned tree, then admit.
    let mut pinned_base = None;
    let worktree = if let Some(owned) = owned {
        let (info, base) = own_worktree(
            &owned,
            command.base.as_deref(),
            &command.operation_id,
            edge,
            workers,
            created,
            &clock,
            &cancel,
        )?;
        pinned_base = Some(base);
        command.access.worktree = Some(info.path.clone());
        lane_access = self::lane_access(&command.access, edge.roots.state.as_path())?;
        Some(json!({"path": info.path, "branch": owned.branch, "owned": true}))
    } else if let Some(path) = &command.access.worktree {
        let writer = run::resolve_writer(edge.roots.state.as_path(), edge.cwd, path)?;
        Some(provenant_confinement::worktrees::with_context(
            edge.roots, edge.home, &writer.project, &clock, &cancel, |context| {
                use provenant_confinement::worktrees;
                let info = worktrees::list(context, &cancel)?.into_iter()
                    .find(|tree| tree.path == writer.worktree)
                    .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                let branch = info.branch.ok_or(worktrees::WorktreeError::InvalidRequest)?;
                Ok(json!({"path": info.path, "branch": branch.trim_start_matches("refs/heads/"), "owned": false}))
            },
        ).map_err(run::RunError::from)?)
    } else {
        None
    };
    let tasks = tasks
        .into_iter()
        .map(
            |(
                task_id,
                title,
                prompt,
                route,
                fallback,
                delivery_id,
                account,
                deliver,
                base,
                build_heavy,
            )| {
                Submitted {
                    build_heavy,
                    title,
                    base,
                    pinned_base,
                    head: command.head.clone(),
                    task_id,
                    prompt,
                    route,
                    fallback,
                    account,
                    provider_timeout: command.provider_timeout,
                    delivery_id,
                    deliver,
                    worktree: command.access.worktree.clone(),
                    access: lane_access.clone(),
                }
            },
        )
        .collect();
    let (record, started) = submit(
        &session,
        edge,
        Submission {
            chair,
            operation_id: command.operation_id,
            tasks,
            concurrency,
            knobs: command.knobs,
        },
        &call,
    )?;
    let (mut data, human, timed_out) =
        report(&session, &record, &started, command.wait, edge, &call)?;
    if let Some(worktree) = worktree {
        data["worktree"] = worktree;
    }
    Ok((data, human, timed_out))
}

/// A `--worktree-branch` dispatch's owned tree, as preflight settled it.
struct OwnedWorktree {
    branch: String,
    host: provenant_core::HostId,
    created_at: WallTime,
}

/// Create or claim the owned tree and publish its lane ownership. A new
/// tree's owner records the commit creation pinned, so nothing is resolved
/// again after the side effect and a replay finds the same base.
#[allow(clippy::too_many_arguments)]
fn own_worktree(
    owned: &OwnedWorktree,
    requested_base: Option<&str>,
    operation_id: &OperationId,
    edge: &Edge<'_>,
    workers: &dyn provenant_confinement::worktrees::WorkerObserver,
    created: &mut Option<(PathBuf, String)>,
    clock: &SystemClock,
    cancel: &CancelToken,
) -> Result<
    (
        provenant_confinement::worktrees::WorktreeInfo,
        provenant_core::GitOid,
    ),
    Fail,
> {
    let OwnedWorktree {
        branch,
        host,
        created_at,
    } = owned;
    let mut owner_refused = false;
    provenant_confinement::worktrees::with_context(
        edge.roots,
        edge.home,
        edge.cwd,
        clock,
        cancel,
        |context| {
            use provenant_confinement::worktrees::{
                self, WorktreeCondition, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
                WriterState,
            };
            let context = &worktrees::WorktreeContext {
                workers,
                ..*context
            };
            let existing = worktrees::list(context, cancel)?.into_iter().find(|tree| {
                tree.branch.as_deref() == Some(format!("refs/heads/{branch}").as_str())
            });
            let (info, pinned) = match existing {
                Some(info) => {
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    if worktrees::load_owner(&tree)?
                        .is_some_and(|owner| owner.origin != WorktreeOrigin::Lane)
                    {
                        owner_refused = true;
                        return Err(worktrees::WorktreeError::UnknownWriter);
                    }
                    if info.dirty != WorktreeCondition::Clean || info.writer != WriterState::Free {
                        return Err(worktrees::WorktreeError::InvalidRequest);
                    }
                    (info, None)
                }
                None => {
                    // The creation receipt: the tree's head is the commit
                    // creation pinned the base to.
                    let info = worktrees::create(
                        context,
                        WorktreeCreate {
                            branch: branch.clone(),
                            base: requested_base.map(str::to_owned),
                        },
                        cancel,
                    )?;
                    *created = Some((info.path.clone(), branch.clone()));
                    let pinned = info.head.to_string();
                    (info, Some(pinned))
                }
            };
            let verified = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            let prior = worktrees::load_owner(&verified)?;
            if prior
                .as_ref()
                .is_some_and(|owner| owner.origin != WorktreeOrigin::Lane)
            {
                owner_refused = true;
                return Err(worktrees::WorktreeError::UnknownWriter);
            }
            let owner = match prior {
                Some(owner) if owner.operation_id.as_ref() == Some(operation_id) => owner,
                prior => WorktreeOwner {
                    attempt_id: None,
                    operation_id: Some(operation_id.clone()),
                    branch: branch.clone(),
                    base: match (pinned, prior) {
                        (Some(pinned), _) => pinned,
                        (None, Some(prior)) => prior.base,
                        // An unowned existing tree: nothing was created, so
                        // a base that does not resolve leaves no side effect.
                        (None, None) => {
                            let git = provenant_os::git::git_executable()
                                .ok_or(worktrees::WorktreeError::InvalidRequest)?;
                            run::resolve_git_base(
                                &git,
                                context.repo.root_path(),
                                requested_base.unwrap_or("HEAD"),
                            )
                            .map_err(|_| worktrees::WorktreeError::InvalidRequest)?
                        }
                    },
                    host: *host,
                    created_at: *created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            };
            if created.as_ref().is_some_and(|(path, _)| path == &info.path) {
                worktrees::save_owner(&verified, &owner)?;
            } else {
                worktrees::save_lane_owner_when_free(context, &verified, &owner, cancel)?;
            }
            let base = provenant_core::GitOid::parse(&owner.base)
                .map_err(|_| worktrees::WorktreeError::InvalidRequest)?;
            Ok((info, base))
        },
    )
    .map_err(|error| {
        if owner_refused {
            Fail::new(
                Status::Refused,
                ErrorKind::Refused,
                "--worktree-branch cannot reuse an adopted or foreign owner; its ownership, label and base are unchanged",
            )
        } else {
            run::RunError::from(error).into()
        }
    })
}

fn nested_dispatch_context(roots: &Roots) -> Result<(u64, Option<AttemptId>), Fail> {
    let parent_depth = environment_text(run::LANE_DEPTH_ENV)?;
    let parent_attempt = environment_text(run::PARENT_ATTEMPT_ENV)?;
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
    let max_depth = config
        .knobs
        .get("lanes.max_depth")
        .and_then(|knob| knob.value.as_u64())
        .ok_or_else(|| {
            Fail::new(
                Status::Error,
                ErrorKind::Config,
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

fn worktree_was_pruned(receipt: &Value, path: &Path) -> bool {
    receipt["state"] == "pruned"
        || receipt["state"] == "pruning"
            && path
                .to_str()
                .is_some_and(|path| receipt["worktree"] == path)
            && std::fs::symlink_metadata(path)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn replay_owned_dispatch(
    command: &Dispatch,
    edge: &Edge<'_>,
    session: &Session,
    call: &run::Request<'_>,
) -> Result<Option<(Value, String, bool)>, Fail> {
    let Some(operation) = session.store.operation(&command.operation_id, call)? else {
        return Ok(None);
    };
    let canonical = session.store.request(operation.run_id, call)?;
    let task = canonical
        .tasks
        .first()
        .filter(|_| canonical.tasks.len() == 1)
        .ok_or(run::RunError::OperationConflict)?;
    let branch = command
        .worktree_branch
        .as_deref()
        .ok_or(run::RunError::OperationConflict)?;
    let writer = task
        .writer
        .as_ref()
        .ok_or(run::RunError::OperationConflict)?;
    let prompt = match &command.input.prompt_file {
        Some(path) => read_bounded(path)?,
        None => read_text(std::io::stdin().lock())?,
    };
    let route = requested(
        command.route.model.as_ref().map(|model| model.0.as_str()),
        command.route.route.as_deref(),
        command.route.effort.clone(),
    )?;
    let delivery = match &command.deliver {
        Some(mode) => run::HostDeliveryContext::destination(mode.clone(), branch)?,
        None => command
            .deliver_remote
            .clone()
            .zip(command.deliver_branch.clone())
            .map(|(remote, branch)| run::WriterDeliveryRequest {
                extra: Default::default(),
                mode: Default::default(),
                remote,
                branch,
                open_pr: command.open_pr,
            }),
    };
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    let profile = config
        .dispatch_profile(&command.knobs)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    if task.title != command.title
        || task.build_heavy != command.build_heavy
        || task.prompt_digest != CanonicalV1::file_digest(prompt.as_bytes())
        || task.route != route
        || task.account != command.account
        || task.head != command.head
        || task.base_ref != command.base
        || task.deliver != delivery
        || task.access != lane_access(&command.access, edge.roots.state.as_path())?
        || task.profile != profile
        || task.provider_timeout_seconds != command.provider_timeout
        || task.delivery_id != command.delivery_id
        || command.task_id.is_some_and(|id| id != task.task_id)
    {
        return Err(run::RunError::OperationConflict.into());
    }
    let mut settlement = None;
    let owned = if writer.worktree.exists() {
        provenant_confinement::worktrees::with_context(
            edge.roots,
            edge.home,
            &writer.project,
            call.clock,
            call.cancel,
            |context| {
                let tree = provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                Ok(
                    provenant_confinement::worktrees::load_owner(&tree)?.is_some_and(|owner| {
                        owner.branch == branch
                            && owner.operation_id.as_ref() == Some(&command.operation_id)
                    }),
                )
            },
        )
        .map_err(run::RunError::from)?
    } else {
        let page = session.store.lanes(
            &run::LaneQuery {
                targets: vec![run::LaneTarget {
                    run: operation.run_id,
                    task: None,
                    attempt: None,
                }],
                all: true,
                limit: 2,
                after: 0,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            call,
        )?;
        if let Some(attempt) = page.lanes.first() {
            settlement = session
                .store
                .worktree_settlement(attempt.attempt_id, call)?;
        }
        settlement.as_ref().is_some_and(|receipt| {
            worktree_was_pruned(receipt, &writer.worktree)
                && (receipt["result"]["removed"][0]["branch"] == format!("refs/heads/{branch}")
                    || receipt["branch"] == format!("refs/heads/{branch}"))
        })
    };
    if !owned {
        return Err(run::RunError::OperationConflict.into());
    }
    let run = session.store.run(operation.run_id, call)?;
    let started = launch(session, run.run_id, edge, call)?;
    let (mut data, human, timed_out) = report(session, &run, &started, command.wait, edge, call)?;
    data["worktree"] = json!({"path": writer.worktree, "branch": branch, "owned":true});
    if let Some(settlement) = settlement {
        data["worktree_settlement"] = settlement;
    }
    Ok(Some((data, human, timed_out)))
}

fn environment_text(name: &str) -> Result<Option<String>, Fail> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(Fail::new(
            Status::Invalid,
            ErrorKind::LaneContext,
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
                ErrorKind::LaneContext,
                "lane depth environment value must be a non-negative integer",
            )
        })?;
    let parent_attempt = parent_attempt
        .map(AttemptId::parse)
        .transpose()
        .map_err(|_| {
            Fail::new(
                Status::Invalid,
                ErrorKind::ParentAttempt,
                "parent attempt environment value is invalid",
            )
        })?;
    if let Some(run_state) = run_state {
        let invalid = || {
            Fail::new(
                Status::Refused,
                ErrorKind::LaneContext,
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
                ErrorKind::LaneContext,
                "nested lane context requires a depth of at least 1",
            ));
        }
        _ => {
            return Err(Fail::new(
                Status::Refused,
                ErrorKind::LaneContext,
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
    launch_prepared(session, run, edge, call, None)
}
fn launch_prepared(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    ready: Option<&run::PreparedStart>,
) -> Result<Vec<run::Started>, Fail> {
    // Every lane-facing launch path is submission-only. Only its host holder
    // prepares providers, including credentials and the child's OS profile.
    if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() {
        return Ok(Vec::new());
    }
    launch_on_host(session, run, edge, call, None, None, ready)
}

struct PreparedProvider<'a> {
    fallback: &'a dyn run::Providers,
    ready: Option<&'a run::PreparedStart>,
}
impl run::Providers for PreparedProvider<'_> {
    fn runtime_cap(&self, provider: &str) -> Option<u64> {
        self.fallback.runtime_cap(provider)
    }
    fn priority(&self, task: &run::TaskRequest) -> Result<u8, String> {
        self.fallback.priority(task)
    }
    fn check_policy(&self, task: &run::TaskRequest) -> Result<(), String> {
        if let Some(ready) = self.ready {
            if ready.task != *task {
                return Err("prepared continuation task changed".into());
            }
            return Ok(());
        }
        self.fallback.check_policy(task)
    }
    fn prepare(
        &self,
        task: &run::TaskRequest,
        prompt: &str,
        paths: &run::LaunchPaths,
        resume: Option<&run::AttemptRecord>,
    ) -> Result<run::PreparedLaunch, String> {
        if let Some(ready) = self.ready {
            if ready.attempt != paths.attempt_id || ready.task != *task || resume.is_some() {
                return Err("prepared continuation identity changed".into());
            }
            return ready
                .plan
                .borrow_mut()
                .take()
                .ok_or_else(|| "prepared continuation consumed".into());
        }
        self.fallback.prepare(task, prompt, paths, resume)
    }
    fn decode(
        &self,
        route: &run::RouteRecord,
        stdout: &[u8],
        exit: &ExitObservation,
        provider_state: &Path,
    ) -> Result<run::Decoded, String> {
        self.fallback.decode(route, stdout, exit, provider_state)
    }
    fn failure_diagnostic(&self, route: &run::RouteRecord, stderr: &[u8]) -> Option<String> {
        self.fallback.failure_diagnostic(route, stderr)
    }
}
fn with_chair_index<T>(
    roots: &Roots,
    call: &run::Request<'_>,
    action: impl FnOnce(
        &provenant_fabric_runtime::chairs::ChairIndex,
        &provenant_fabric_runtime::chairs::Request<'_>,
    ) -> Result<T, provenant_fabric_runtime::chairs::Error>,
) -> Result<T, Fail> {
    use provenant_fabric_runtime::chairs::{ChairIndex, Request, ThreadWait};
    let index_call = Request {
        clock: call.clock,
        budget: call.transaction_budget(),
        cancel: call.cancel,
        backoff: &ThreadWait,
    };
    ChairIndex::open(&roots.state, &index_call)
        .and_then(|index| action(&index, &index_call))
        .map_err(|error| {
            Fail::new(
                match &error {
                    provenant_fabric_runtime::chairs::Error::Conflict
                    | provenant_fabric_runtime::chairs::Error::NotFound
                    | provenant_fabric_runtime::chairs::Error::Invalid(_) => Status::Refused,
                    _ => Status::Error,
                },
                match &error {
                    provenant_fabric_runtime::chairs::Error::Conflict => ErrorKind::Conflict,
                    provenant_fabric_runtime::chairs::Error::NotFound => ErrorKind::NotFound,
                    provenant_fabric_runtime::chairs::Error::Invalid(_) => ErrorKind::Refused,
                    provenant_fabric_runtime::chairs::Error::Busy => ErrorKind::Contention,
                    provenant_fabric_runtime::chairs::Error::Cancelled => ErrorKind::Cancelled,
                    _ => ErrorKind::Fabric,
                },
                format!("chair association: {error}"),
            )
        })
}

/// New lane-local associations need the same namespace proof as WorkIndex.
/// A legacy row can be replayed, but a lane cannot create or repair one.
fn check_chair_run_id(
    session: &Session,
    roots: &Roots,
    run: RunId,
    request: &run::CanonicalRequest,
    call: &run::Request<'_>,
) -> Result<(), Fail> {
    session
        .store
        .check_child_run_id(run, request, &session.store.run(run, call)?.operation_id)?;
    if request.child_attempt_id_version.is_none() {
        let digest = request.digest().map_err(run::RunError::from)?;
        with_chair_index(roots, call, |index, c| {
            match index.read(session.context.project_id, run, c)? {
                Some(value) if value.request_digest == digest => Ok(()),
                Some(_) => Err(provenant_fabric_runtime::chairs::Error::Conflict),
                None => Err(provenant_fabric_runtime::chairs::Error::Invalid(
                    "new chair associations require host-derived child run IDs",
                )),
            }
        })?;
    }
    Ok(())
}

fn seal_launch_task(
    request_digest: provenant_core::Digest,
    task: &run::TaskRequest,
) -> run::SealedTask {
    run::SealedTask {
        request_digest,
        task_id: task.task_id,
        effective_network: task.access.network(task.writer.is_some()),
    }
}

/// Called after guardian custody is released. The successor was queued in the
/// terminal transaction; a crashed launcher leaves it for ordinary recovery.
/// When the staged retry must wait, `before_wait` runs first so other work,
/// such as graph advancement, is not held behind the backoff.
pub fn launch_policy_successor(
    context: &run::HolderContext,
    before_wait: impl FnOnce(),
) -> Result<(), String> {
    let Some(host) = context.launch.as_ref() else {
        return Ok(());
    };
    if host.configuration_root != context.roots.state_root {
        return Ok(());
    }
    let mut inputs =
        provenant_core::RootInputs::new(context.roots.home.clone(), context.roots.checkout.clone());
    inputs.state_dir = Some(context.roots.state_root.clone());
    let roots = inputs
        .resolve(&provenant_os::fs::SystemPathResolver)
        .map_err(|error| error.to_string())?;
    let (_, store) =
        run::HolderContext::open(&context.encode().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let mut call = request(&clock, &cancel).map_err(|error| error.message)?;
    let pending = store
        .pending_attempts(context.run_id, &call)
        .map_err(|error| error.to_string())?;
    let successor_pending = |pending: &[run::AttemptRecord]| {
        pending.iter().any(|attempt| {
            attempt.retry_of == Some(context.attempt_id)
                && matches!(
                    attempt.continuation.as_deref(),
                    Some("policy_fallback" | "policy_continuation")
                )
        })
    };
    if !successor_pending(&pending) {
        return Ok(());
    }
    // A capacity retry waits until its recorded time here, in the settled
    // attempt's guardian, and gives up the wait once the successor is no
    // longer queued. The time is sealed with the retry and every launcher
    // defers the successor until then, so a later sweep can launch it if this
    // guardian dies.
    use provenant_core::Clock;
    let backoff = store
        .record(context.attempt_id, &call)
        .ok()
        .and_then(|record| store.staged_policy_retry(&record).ok().flatten())
        .and_then(|retry| retry.not_before_unix_seconds)
        .zip(clock.wall_now().ok())
        .map_or(0, |(not_before, now)| {
            let now = now.as_micros().div_euclid(1_000_000);
            u64::try_from(not_before.saturating_sub(now)).unwrap_or(0)
        });
    if backoff > 0 {
        before_wait();
        let start = clock.mono_now();
        let deadline = provenant_core::DurationMicros::from_seconds(backoff)
            .and_then(|wait| start.checked_add(wait))
            .map_err(|error| error.to_string())?;
        let mut checked = start;
        while clock.mono_now() < deadline {
            provenant_os::sys::wait_pause(std::time::Duration::from_millis(50));
            let now = clock.mono_now();
            if now.as_micros().saturating_sub(checked.as_micros()) >= 1_000_000 {
                checked = now;
                // Each check, and the launch after the wait, gets its own budget.
                call = request(&clock, &cancel).map_err(|error| error.message)?;
                let pending = store
                    .pending_attempts(context.run_id, &call)
                    .map_err(|error| error.to_string())?;
                if !successor_pending(&pending) {
                    return Ok(());
                }
            }
        }
        call = request(&clock, &cancel).map_err(|error| error.message)?;
    }
    let session = Session {
        context: run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: context.project_id,
            actor_id: context.actor_id,
            read_prompts: true,
        },
        store,
    };
    let edge = Edge {
        roots: &roots,
        home: &context.roots.home,
        assets: &context.roots.checkout,
        cwd: &host.cwd,
    };
    launch_on_host(
        &session,
        context.run_id,
        &edge,
        &call,
        Some(host),
        None,
        None,
    )
    .map_err(|error| error.message)?;
    Ok(())
}

/// A child carries the request `validate_child` returned, which is the only
/// copy launched from. The store carries its parent's sealed requester.
fn launch_on_host(
    session: &Session,
    run: RunId,
    edge: &Edge<'_>,
    call: &run::Request<'_>,
    host: Option<&run::HostLaunchContext>,
    validated: Option<&run::ValidatedChild>,
    ready: Option<&run::PreparedStart>,
) -> Result<Vec<run::Started>, Fail> {
    let inherited_host = host.is_some();
    let host = match host {
        Some(host) => host.clone(),
        None => run::HostLaunchContext {
            configuration_root: edge.roots.state.as_path().into(),
            cwd: edge.cwd.into(),
            executables: std::collections::BTreeMap::new(),
            executable_search_path: std::env::var_os("PATH")
                .map(|path| {
                    std::env::split_paths(&path)
                        .filter(|path| path.is_absolute())
                        .collect()
                })
                .unwrap_or_default(),
            git: provenant_os::git::git_executable()
                .ok_or_else(|| Fail::new(Status::Refused, ErrorKind::Git, "Git unavailable"))?,
            codex_home: std::env::var_os("CODEX_HOME").map(PathBuf::from),
            cargo_build_jobs: std::env::var("CARGO_BUILD_JOBS").ok(),
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        },
    };
    let read;
    let request = match validated {
        Some(request) => &**request,
        None => {
            read = session.store.request(run, call)?;
            &read
        }
    };
    let request_digest = request.digest().map_err(run::RunError::from)?;
    if validated.is_none() {
        if request.parent.is_some() && session.context.state_root != edge.roots.state {
            check_chair_run_id(session, edge.roots, run, request, call)?;
        }
        with_chair_index(edge.roots, call, |index, index_call| {
            if let Some(prior) = request.parent {
                let lane_local = session.context.state_root != edge.roots.state;
                if lane_local
                    && let Some(sealed) = index.read(session.context.project_id, run, index_call)?
                {
                    return if sealed.request_digest == request_digest {
                        Ok(())
                    } else {
                        Err(provenant_fabric_runtime::chairs::Error::Conflict)
                    };
                }
                let parent = session.store.record(prior, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation parent")
                })?;
                let original = session.store.request(parent.run_id, call).map_err(|_| {
                    provenant_fabric_runtime::chairs::Error::Invalid("continuation request")
                })?;
                if lane_local {
                    session
                        .store
                        .check_child_attempt_id(&original, &parent)
                        .map_err(|_| {
                            provenant_fabric_runtime::chairs::Error::Invalid(
                                "continuation source identity does not match host-derived IDs",
                            )
                        })?;
                    if index
                        .read(session.context.project_id, parent.run_id, index_call)?
                        .is_none()
                    {
                        return Err(provenant_fabric_runtime::chairs::Error::NotFound);
                    }
                }
                index.inherit(
                    session.context.project_id,
                    (parent.run_id, original.digest()?),
                    run,
                    request,
                    index_call,
                )
            } else {
                match index.read(session.context.project_id, run, index_call)? {
                    Some(value) if value.request_digest != request_digest => {
                        Err(provenant_fabric_runtime::chairs::Error::Conflict)
                    }
                    // Only the host root's immutable request may repair an
                    // interrupted submission, including owned-worktree replay.
                    None if session.context.state_root == edge.roots.state => {
                        index.submit(session.context.project_id, run, request, index_call)
                    }
                    None if request.chair.is_some() => {
                        Err(provenant_fabric_runtime::chairs::Error::NotFound)
                    }
                    _ => Ok(()),
                }
            }
        })?;
    }
    let seed = run::RootSeed {
        home: edge.home.into(),
        checkout: edge.assets.into(),
        state_root: session.context.state_root.as_path().into(),
    };
    let holder = |attempt: &run::AttemptRecord, requester| {
        let task = request
            .tasks
            .iter()
            .find(|task| task.task_id == attempt.task_id)
            .ok_or(run::RunError::Validation(
                "attempt task missing from request",
            ))?;
        let mut host = host.clone();
        if !inherited_host {
            // Called after preparation, while its selected provider lease is held.
            for registration in REGISTRY {
                let name = registration.adapter.id();
                if let Ok(path) = provider::find_executable(name) {
                    host.executables.insert(name.into(), path);
                }
            }
        }
        if let Some(writer) = request
            .tasks
            .iter()
            .find(|task| task.task_id == attempt.task_id)
            .and_then(|task| task.writer.as_ref())
        {
            host.cwd = writer.worktree.clone();
            provenant_confinement::worktrees::with_context(
                edge.roots,
                edge.home,
                &writer.project,
                call.clock,
                call.cancel,
                |context| {
                    use provenant_confinement::worktrees;
                    let tree =
                        provenant_confinement::verify_worktree(context.repo, &writer.worktree)?;
                    if let Some(mut owner) = worktrees::load_owner(&tree)?
                        && owner.origin == worktrees::WorktreeOrigin::Lane
                        && (owner.operation_id.as_ref() == Some(&attempt.operation_id)
                            || owner.attempt_id.is_some() && owner.attempt_id == attempt.retry_of)
                    {
                        owner.attempt_id = Some(attempt.attempt_id);
                        owner.operation_id = Some(attempt.operation_id.clone());
                        worktrees::save_owner(&tree, &owner)?;
                    }
                    Ok(())
                },
            )
            .map_err(run::RunError::from)?;
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
            sealed: Some(seal_launch_task(
                request_digest,
                &session
                    .store
                    .effective_launch_task(request, task, validated),
            )),
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
        // Top-level discovery runs in prepare under its lease. Child launches
        // continue to use their sealed host executable authority.
        host: inherited_host.then_some(&host),
        cancel: Some(call.cancel),
    };
    let providers = PreparedProvider {
        fallback: &registry,
        ready,
    };
    // Fixture guardians observe a recorded boot key, so the reservation must be
    // taken against the same key as on the lane-launch path of the test host.
    #[cfg(feature = "fixture")]
    if host.fixture_state_write {
        let started = session.store.start_with_writer_lease(
            run,
            &providers,
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
            validated,
            Some(request),
            call,
        )?;
        check_launch_policy(&started)?;
        return Ok(started);
    }
    let started = session
        .service()
        .start(run, &providers, &holder, validated, request, call)?;
    check_launch_policy(&started)?;
    Ok(started)
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

fn check_launch_policy(started: &[run::Started]) -> Result<(), Fail> {
    // Preserve the full receipt whenever a sibling can still make progress
    // or needs unknown-handoff recovery, including Fabric's operation replay.
    if started
        .iter()
        .any(|started| !matches!(started, run::Started::Refused { .. }))
    {
        return Ok(());
    }
    if let Some((attempt, reason)) = started.iter().find_map(|started| match started {
        run::Started::Refused {
            attempt,
            reason,
            kind: run::RefusalKind::Policy,
        } => Some((attempt, reason)),
        _ => None,
    }) {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Refused,
            format!("{attempt}: {reason}"),
        ));
    }
    Ok(())
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
        selection: None,
        run: Some(record.run_id),
        task: None,
        attempt: None,
    };
    let (mut data, human, timed_out) = match wait {
        Some(seconds) => wait_lanes(
            session,
            &[target],
            Condition::All,
            Duration::from_secs(seconds),
            edge,
        )?,
        None => {
            let page = session
                .service()
                .lanes(&lane_query(session, &[target], true, 100, 0, call)?, call)?;
            let waits = session.store.lane_waits(&page.lanes, call);
            let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
            let (data, human) = render_lanes(
                &page.lanes,
                &waits,
                None,
                session.context.state_root.as_path(),
                lane_cooldown_view(&cooldowns),
            );
            (data, human, false)
        }
    };
    let canonical = session.store.request(record.run_id, call)?;
    if let Some(rows) = data["lanes"].as_array_mut() {
        for row in rows {
            if let Some(id) = row["attempt_id"].as_str().and_then(|id| id.parse().ok()) {
                let tail = session.store.policy_tail(id, call)?;
                if tail.attempt_id != id {
                    row["superseded_by"] = json!(tail.attempt_id);
                }
            }
            if let Some(task) = canonical
                .tasks
                .iter()
                .find(|task| row["task_id"] == task.task_id.to_string())
            {
                row["network"] = json!(task.access.network(task.writer.is_some()));
            }
        }
    }
    data["run_id"] = json!(record.run_id);
    data["operation_id"] = json!(record.operation_id);
    let unknown = launch_unknown(started);
    if !unknown.is_empty() {
        data["launch_unknown"] = json!(unknown);
    }
    Ok((data, format!("run {}\n{human}", record.run_id), timed_out))
}

fn lane_query(
    session: &Session,
    targets: &[Target],
    all: bool,
    limit: u64,
    after: u64,
    call: &run::Request<'_>,
) -> Result<run::LaneQuery, Fail> {
    Ok(run::LaneQuery {
        targets: targets
            .iter()
            .map(|target| targets::local(session, target, call))
            .collect::<Result<_, _>>()?,
        all,
        limit,
        after,
        newest_first: false,
        owner: None,
        unseen_after: None,
    })
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
/// The state name and, for a terminal attempt, its recorded reason.
pub fn state_and_reason(state: &run::AttemptState) -> (String, Option<String>) {
    let reason = match state {
        run::AttemptState::Terminal { reason, .. } => reason.clone(),
        _ => None,
    };
    (state_name(state), reason)
}
fn delivery_state_text(state: &provenant_records::run::WriterDeliveryState) -> String {
    use provenant_records::run::WriterDeliveryState;
    match state {
        WriterDeliveryState::Delivered { pull_request, .. } => format!(
            "Delivered {{ pull_request: {:?} }}",
            pull_request
                .as_deref()
                .map(|value| plain_text(value, usize::MAX))
        ),
        WriterDeliveryState::NoNewCommits { .. } => "NoNewCommits".into(),
        WriterDeliveryState::NotAttempted { .. } => "NotAttempted".into(),
        WriterDeliveryState::Failed { stage, .. } => format!("Failed {{ stage: {stage:?} }}"),
    }
}

fn render_lanes(
    lanes: &[run::AttemptRecord],
    waits: &[run::LaneWait],
    next: Option<u64>,
    root: &Path,
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
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
                .map(|_| artifact(root, lane, "result.md"));
            let resolved_account = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.account_label.as_deref());
            let account = resolved_account.unwrap_or("unknown");
            let account_field = resolved_account
                .map(|a| format!(" account={}", plain_text(a, usize::MAX)))
                .unwrap_or_default();
            let route_text = plain_text(&route, usize::MAX);
            // A lane that has not started says what it is waiting for.
            let wait = waits.iter().find(|w| w.attempt_id == lane.attempt_id);
            let reason = reason.or_else(|| wait.map(|w| w.waiting_for.clone()));
            let reason_text = reason
                .as_deref()
                .map(|r| format!(" ({})", plain_text(r, usize::MAX)))
                .unwrap_or_default();
            let title_text = lane
                .title
                .as_deref()
                .map(|title| format!(" {}", plain_text(title, usize::MAX)))
                .unwrap_or_default();
            // A partial finish keeps its outcome; the header says so beside it.
            let state_text = if lane.partial_signals.is_empty() {
                state.clone()
            } else {
                format!("{state} (partial)")
            };
            // Peer records remain intact in JSON; terminal text excludes controls.
            human.push_str(&format!(
                "{}/{}/{}{title_text} {state_text} {route_text}{account_field}{reason_text}\n",
                lane.run_id, lane.task_id, lane.attempt_id,
            ));
            if !lane.partial_signals.is_empty() {
                // The outcome is untouched; the chair decides whether to resume.
                human.push_str(&format!(
                    "  partial finish: {}; the chair decides whether to resume\n",
                    lane.partial_signals
                        .iter()
                        .map(|signal| signal.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                if lane
                    .session
                    .as_deref()
                    .is_some_and(|session| !session.is_empty())
                {
                    human.push_str(&format!(
                        "  continue: provenant resume {} --prompt-file PROMPT\n",
                        lane.attempt_id
                    ));
                }
            }
            if matches!(lane.state, run::AttemptState::Running { .. })
                && let Some(deadline) = lane.runtime_deadline
            {
                human.push_str(&format!("  runtime deadline {deadline}\n"));
            }
            if matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::TimedOut,
                    ..
                }
            ) && lane
                .session
                .as_deref()
                .is_some_and(|session| !session.is_empty())
            {
                human.push_str(&format!(
                    "  continue: provenant resume {} --prompt-file PROMPT\n",
                    lane.attempt_id
                ));
            }
            if matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::ModelUnavailable,
                    ..
                }
            ) {
                let capacity = reason
                    .as_deref()
                    .is_some_and(capacity_reason);
                if capacity
                    && lane
                        .session
                        .as_deref()
                        .is_some_and(|session| !session.is_empty())
                {
                    human.push_str(&format!(
                        "  continue: provenant resume {} --prompt-file PROMPT\n",
                        lane.attempt_id
                    ));
                } else if capacity {
                    human.push_str("  redispatch with another model\n");
                } else {
                    human.push_str("  redispatch with a corrected model\n");
                }
            }
            for skipped in &lane.skipped_routes {
                human.push_str(&format!(
                    "  skipped {} {:?}: {}\n",
                    plain_text(&skipped.route, usize::MAX),
                    skipped.kind,
                    plain_text(&skipped.detail, usize::MAX)
                ));
            }
            let review_required = matches!(
                lane.state,
                run::AttemptState::Terminal {
                    outcome: run::Outcome::ReviewRequired,
                    ..
                }
            );
            if review_required || !lane.protected_paths.is_empty() {
                human.push_str(&format!(
                    "  review head={} paths={}\n",
                    lane.head
                        .map(|head| head.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                    lane.protected_paths
                        .iter()
                        .map(|path| {
                            plain_text(path.as_path_text().unwrap_or("[native path]"), usize::MAX)
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if let Some(delivery) = &lane.delivery {
                human.push_str(&format!(
                    "  delivery {}:{} {} head={}\n",
                    plain_text(&delivery.remote, usize::MAX),
                    plain_text(&delivery.branch, usize::MAX),
                    delivery_state_text(&delivery.state),
                    delivery
                        .head
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ));
            }
            // A pointer to the provider's own transcript; never a copy.
            let transcript = lane
                .applied_route
                .as_ref()
                .and_then(|r| r.provider.as_deref())
                .zip(lane.session.as_deref())
                .map(|(adapter, session)| {
                    let attempt = artifact(root, lane, "");
                    let path = run::locate_transcript(&attempt, session, &lane.write_grants)
                        .map(|path| attempt.join(path));
                    json!({ "adapter": adapter, "session": session, "path": path })
                });
            let mut row = json!({
                "run_id": lane.run_id,
                "task_id": lane.task_id,
                "attempt_id": lane.attempt_id,
                "attempt_number": lane.attempt_number,
                "state": state,
                "exit_code": exit_code,
                "reason": reason,
                "lane_class": wait.map(|w| w.lane_class),
                "waiting_for": wait.map(|w| &w.waiting_for),
                "requested_route": lane.requested_route,
                "applied_route": lane.applied_route,
                "skipped_routes": lane.skipped_routes,
                "account": account,
                "observed_route": lane.observed_route,
                "queued_at": lane.queued_at,
                "started_at": lane.started_at,
                "priority": lane.priority,
                "finished_at": finished_at,
                "last_output_at": lane.last_output_at,
                "runtime_deadline": lane.runtime_deadline,
                "partial_signals": lane.partial_signals,
                "delivery_id": lane.delivery_id,
                "write_grants": lane.write_grants,
                "read_only_writable": lane.read_only_writable,
                "git_grants_digest": lane.git_grants_digest,
                "head": lane.head,
                "protected_paths": lane.protected_paths,
                "delivery": lane.delivery,
                "continuation": lane.continuation,
                "cache_read_share": provenant_routing::cache_read_share(lane.usage.uncached_input, lane.usage.cache_read, lane.usage.cache_write),
                "result_path": result,
                "transcript": transcript,
            });
            if let Some(title) = &lane.title {
                row["title"] = json!(title);
            }
            if lane.requested_priority.map(i32::from) != lane.priority
                && let Some(priority) = lane.requested_priority
            {
                row["requested_priority"] = json!(priority);
            }
            row
        })
        .collect();
    if rows.is_empty() {
        human.push_str("no matching lanes\n");
    }
    let cooldowns = cooldowns.map_or_else(Vec::new, |(records, now)| {
        render_cooldowns(records, now, &mut human)
    });
    (
        json!({ "lanes": rows, "next_cursor": next.map(|n| n.to_string()), "capacity_cooldowns": cooldowns }),
        human,
    )
}

/// Read optional host cooldowns before composing a lane reply.
fn read_lane_cooldowns(root: &Path) -> Option<(Vec<provenant_quota::AccountRecord>, i64)> {
    crate::quota::read_cache_at(root).ok().zip(
        SystemClock::new()
            .wall_now()
            .ok()
            .map(|now| now.as_timestamp().as_second()),
    )
}
fn lane_cooldown_view(
    cooldowns: &Option<(Vec<provenant_quota::AccountRecord>, i64)>,
) -> Option<(&[provenant_quota::AccountRecord], i64)> {
    cooldowns
        .as_ref()
        .map(|(records, now)| (records.as_slice(), *now))
}
fn render_cooldowns(
    records: &[provenant_quota::AccountRecord],
    now: i64,
    human: &mut String,
) -> Vec<Value> {
    let mut shown = Vec::new();
    for record in records {
        for cooldown in &record.capacity_cooldowns {
            if cooldown.observed_unix_seconds <= now && cooldown.expires_unix_seconds > now {
                let remaining = cooldown.expires_unix_seconds.saturating_sub(now);
                human.push_str(&format!(
                    "capacity cooldown {}/{} [{}]: {remaining}s remaining\n",
                    plain_text(&record.account.adapter, usize::MAX),
                    plain_text(&cooldown.model, usize::MAX),
                    plain_text(&record.account.label, usize::MAX)
                ));
                shown.push(json!({"account": record.account, "model": cooldown.model, "observed_unix_seconds": cooldown.observed_unix_seconds, "expires_unix_seconds": cooldown.expires_unix_seconds, "remaining_seconds": remaining}));
            }
        }
    }
    shown
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
/// `owner`'s nonterminal work; host caps bound it. With `seen`, terminal
/// lanes at or before that event cursor are already acknowledged and dropped.
fn select(
    session: &Session,
    targets: &[Target],
    seen: Option<u64>,
    owner: ActorId,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let mut selected = Vec::new();
    let mut after = 0;
    loop {
        let mut query = lane_query(
            session,
            targets,
            !targets.is_empty() || seen.is_some(),
            100,
            after,
            call,
        )?;
        query.owner = targets.is_empty().then_some(owner);
        query.unseen_after = seen;
        let page = session.service().lanes(&query, call)?;
        selected.extend(page.lanes.iter().map(|lane| lane.attempt_id));
        match page.next {
            Some(next) if selected.len() < 1000 => after = next,
            Some(_) => {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "selection exceeds 1000 lanes",
                ));
            }
            None => return Ok(selected),
        }
    }
}
/// The seen-cursor key for a default wait selection.
fn selection_key(label: Option<&str>) -> String {
    label.map_or_else(|| "default".into(), |label| format!("session:{label}"))
}
/// What a wait or watch selects: the lanes of named targets, or those of one
/// session when none are named.
struct Picks<'a> {
    targets: &'a [Target],
    /// The session whose lanes count; this session unless `--session` names one.
    owner: ActorId,
    /// Set when `--session` names the session, which then also narrows targets.
    label: Option<&'a str>,
    since: Option<WallTime>,
}
impl Picks<'_> {
    fn scope(&self) -> Result<watch::Scope, Fail> {
        Ok(watch::Scope {
            exact: self.targets.iter().filter_map(|t| t.attempt).collect(),
            since: self.since,
            entered: watch::entered()?,
            explicit: !self.targets.is_empty(),
            snapshot: !self.targets.is_empty(),
        })
    }
}
/// The agent label's actor on this host: the identity that submitted its lanes.
fn session_actor(edge: &Edge<'_>, label: &str) -> Result<ActorId, Fail> {
    let identity = |error: String| Fail::new(Status::Error, ErrorKind::Identity, error);
    let host = provenant_config::bind_host(edge.roots).map_err(|e| identity(e.to_string()))?;
    provenant_fabric_runtime::identity::Address::new(host, label)
        .map(|address| address.actor)
        .map_err(|e| identity(e.to_string()))
}
/// Lanes still unsettled, or settled at or after `--since`, found by scanning
/// the session's lanes newest-queued first. A lane queued before the window can
/// still settle inside it, so the scan reads every retained lane up to its cap
/// rather than stopping at the first older queue time.
fn select_since(
    session: &Session,
    owner: ActorId,
    since: WallTime,
    call: &run::Request<'_>,
) -> Result<Vec<AttemptId>, Fail> {
    let ids = select(session, &[], None, owner, call)?;
    watch::settled_since(
        ids,
        |after| {
            let mut query = lane_query(session, &[], true, 100, after, call)?;
            query.newest_first = true;
            query.owner = Some(owner);
            let page = session.service().lanes(&query, call)?;
            Ok((page.lanes, page.next))
        },
        since,
    )
}
/// The candidate lanes for a wait or watch, read afresh on each call. With
/// `seen`, an unnamed selection holds the unsettled lanes and those settled
/// after that event cursor.
fn candidates<'a>(
    session: &'a Session,
    picks: &'a Picks<'a>,
    seen: Option<u64>,
) -> impl FnMut(Duration) -> Result<Vec<run::AttemptRecord>, Fail> + 'a {
    move |_| {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        // The outer wait owns the deadline and retries contention between polls.
        // Do not let an individual store read spend a separate 30-second budget.
        let call = run::Request {
            clock: &clock,
            budget: WaitBudget::nonblocking(clock.mono_now()),
            cancel: &cancel,
        };
        let ids = match picks.since {
            Some(since) if picks.targets.is_empty() => {
                select_since(session, picks.owner, since, &call)?
            }
            _ => select(session, picks.targets, seen, picks.owner, &call)?,
        };
        let mut lanes = Vec::with_capacity(ids.len());
        let mut included = std::collections::BTreeSet::new();
        for mut id in ids {
            // Settlement may have queued a successor after the selection read.
            // Keep the original receipt and wait for its current policy tail.
            for _ in 0..9 {
                let lane = session.service().status_for_wait(id, &call)?;
                if included.insert(id)
                    && (picks.label.is_none() || lane.verified_owner == Some(picks.owner))
                {
                    lanes.push(lane);
                }
                // Read the tail after the state: a terminal state must already
                // have committed any successor in the same transaction.
                let tail = session.store.policy_tail(id, &call)?.attempt_id;
                if tail == id {
                    break;
                }
                id = tail;
            }
        }
        Ok(lanes)
    }
}
const LOCAL_POLL: Duration = Duration::from_millis(500);
/// The lanes a wait or watch ended on, each unstarted one with what it waits for.
fn render_settled(
    session: &Session,
    lanes: &[run::AttemptRecord],
    cooldowns: Option<(&[provenant_quota::AccountRecord], i64)>,
) -> Result<(Value, String), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let waits = settled_waits(&clock, &cancel, |call| {
        session.store.lane_waits(lanes, call)
    });
    Ok(render_lanes(
        lanes,
        &waits,
        None,
        session.context.state_root.as_path(),
        cooldowns,
    ))
}
/// Wait details are optional, and must not extend a completed wait or watch.
fn settled_waits<T>(
    clock: &SystemClock,
    cancel: &CancelToken,
    read: impl FnOnce(&run::Request<'_>) -> T,
) -> T {
    let call = run::Request {
        clock,
        budget: WaitBudget::nonblocking(clock.mono_now()),
        cancel,
    };
    read(&call)
}
fn waited_reply(session: &Session, waited: watch::Waited) -> Result<(Value, String, bool), Fail> {
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let (mut data, human) = render_settled(session, &waited.lanes, lane_cooldown_view(&cooldowns))?;
    data["condition_met"] = json!(waited.met);
    if waited.ignored > 0 {
        data["ignored_settled"] = json!(waited.ignored);
    }
    Ok((data, human, !waited.met))
}
/// Wait for every lane a run, task or attempt names, settled or not: the
/// whole named set, for commands that act on a lane they just created.
fn wait_lanes(
    session: &Session,
    targets: &[Target],
    condition: Condition,
    duration: Duration,
    _edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let picks = Picks {
        targets,
        owner: session.context.actor_id,
        label: None,
        since: None,
    };
    let scope = watch::Scope {
        explicit: false,
        snapshot: false,
        ..picks.scope()?
    };
    let waited = watch::wait(
        candidates(session, &picks, None),
        scope,
        condition,
        duration,
        LOCAL_POLL,
    )?;
    waited_reply(session, waited)
}
/// `lanes --wait`: settlements after the wait starts, or after `--since`. Exact
/// attempt IDs count whenever they settled. Without named lanes, a session's
/// wait resumes after the settlements it last acknowledged.
#[allow(clippy::disallowed_methods)]
fn wait_command(
    session: &Session,
    picks: &Picks<'_>,
    condition: Condition,
    deadline: Instant,
    entered: WallTime,
    edge: &Edge<'_>,
) -> Result<(Value, String, bool), Fail> {
    let key = selection_key(picks.label);
    let cursor = picks.targets.is_empty() && picks.since.is_none();
    let mut seen = None;
    let waited = watch::wait(
        |remaining| {
            let cursor_value = if cursor {
                if seen.is_none() {
                    let clock = SystemClock::new();
                    let cancel = CancelToken::new();
                    let call = run::Request {
                        clock: &clock,
                        budget: WaitBudget::nonblocking(clock.mono_now()),
                        cancel: &cancel,
                    };
                    seen = Some(session.store.seen_cursor(&key, true, &call)?);
                }
                seen
            } else {
                None
            };
            candidates(session, picks, cursor_value)(remaining)
        },
        watch::Scope {
            entered,
            ..picks.scope()?
        },
        condition,
        deadline.saturating_duration_since(Instant::now()),
        LOCAL_POLL,
    )?;
    let mut waited = waited;
    if cursor && waited.met {
        let acknowledged = watch::retry_contention(deadline, LOCAL_POLL, || {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let entry = run::Request {
                clock: &clock,
                budget: WaitBudget::nonblocking(clock.mono_now()),
                cancel: &cancel,
            };
            let mut latest = 0;
            for lane in waited.lanes.iter().filter(|l| settled(&l.state).0) {
                latest = latest.max(session.store.last_event_sequence(lane.attempt_id, &entry)?);
            }
            if latest > 0 {
                open(edge, &entry)?.store.ack_seen(&key, latest, &entry)?;
            }
            Ok(())
        })?;
        if !acknowledged {
            waited.met = false;
        }
    }
    waited_reply(session, waited)
}
/// One line per call, already newline-terminated, for a watch to stream.
type Emit<'a> = &'a mut dyn FnMut(&str) -> Result<(), Fail>;
fn watched_reply(
    session: &Session,
    watched: &watch::Watched,
    json: bool,
) -> Result<(Value, String, bool), Fail> {
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let (mut data, _) = render_settled(session, &watched.lanes, lane_cooldown_view(&cooldowns))?;
    data["condition_met"] = json!(watched.settled);
    let human = if json {
        String::new()
    } else {
        watch::end_line(watched)
    };
    Ok((data, human, !watched.settled))
}
/// `lanes --watch`: a line for each state a counted lane enters, until all
/// have settled or the budget passes.
fn watch_command(
    session: &Session,
    picks: &Picks<'_>,
    seconds: u64,
    json: bool,
    emit: Emit<'_>,
) -> Result<(Value, String, bool), Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let entry = request(&clock, &cancel)?;
    // A watch reports from its start, so it never reads the acknowledged cursor.
    let base = (picks.targets.is_empty() && picks.since.is_none())
        .then(|| {
            let key = format!("watch:{}", selection_key(picks.label));
            session.store.seen_cursor(&key, true, &entry)
        })
        .transpose()?;
    let watched = watch::watch(
        candidates(session, picks, base),
        picks.scope()?,
        Duration::from_secs(seconds),
        LOCAL_POLL,
        &mut |lane| {
            let text = if json {
                watch::json_line(lane, None)
            } else {
                watch::line(lane, None)
            };
            emit(&format!("{text}\n"))
        },
    )?;
    watched_reply(session, &watched, json)
}

/// Write one watch line to standard output and flush it, so a pipe or monitor
/// sees each event as it happens.
pub fn write_line(line: &str) -> Result<(), Fail> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(line.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|_| Fail::new(Status::Error, ErrorKind::Io, "cannot write watch output"))
}
/// `lanes`, buffering any watch lines into the reply for callers that cannot
/// stream them.
pub fn lanes(command: Lanes, edge: &Edge<'_>) -> Reply {
    let mut lines = String::new();
    let mut reply = lanes_streaming(command, edge, false, &mut |line| {
        lines.push_str(line);
        Ok(())
    });
    reply.human.insert_str(0, &lines);
    reply
}
/// `lanes` with a watch writing each line to `out` and flushing it as it
/// happens. With `json` each line is a JSON object and the reply is the last.
pub fn lanes_streaming(
    command: Lanes,
    edge: &Edge<'_>,
    json: bool,
    out: &mut dyn FnMut(&str) -> Result<(), Fail>,
) -> Reply {
    let result = if let Some(seconds) = command.wait {
        lanes_wait_entry(&command, edge, json, out, seconds)
    } else {
        lanes_inner(&command, edge, json, out, None)
    };
    let mut out = reply_waited("lanes", result);
    // A watch already printed its events; its final line is its own summary.
    if out.result.error.is_none()
        && out.result.data["lanes"].is_array()
        && !(command.watch.is_some() && !json)
    {
        out.summary = Some(lane_summary(&lane_lines(&out.result.data), None));
    }
    out.read_only = true;
    out
}
#[allow(clippy::disallowed_methods)]
fn lanes_wait_entry(
    command: &Lanes,
    edge: &Edge<'_>,
    json: bool,
    emit: Emit<'_>,
    seconds: u64,
) -> Result<(Value, String, bool), Fail> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let entered = watch::entered()?;
    loop {
        match lanes_inner(command, edge, json, &mut *emit, Some((deadline, entered))) {
            Err(fail) if fail.kind == ErrorKind::Contention => {
                if Instant::now() >= deadline {
                    let cooldowns = read_lane_cooldowns(edge.roots.state.as_path());
                    let (mut data, human) = render_lanes(
                        &[],
                        &[],
                        None,
                        edge.roots.state.as_path(),
                        lane_cooldown_view(&cooldowns),
                    );
                    data["condition_met"] = json!(false);
                    return Ok((data, human, true));
                }
                watch::pause(LOCAL_POLL, deadline);
                if Instant::now() >= deadline {
                    let cooldowns = read_lane_cooldowns(edge.roots.state.as_path());
                    let (mut data, human) = render_lanes(
                        &[],
                        &[],
                        None,
                        edge.roots.state.as_path(),
                        lane_cooldown_view(&cooldowns),
                    );
                    data["condition_met"] = json!(false);
                    return Ok((data, human, true));
                }
            }
            result => return result,
        }
    }
}
fn lanes_inner(
    command: &Lanes,
    edge: &Edge<'_>,
    json: bool,
    emit: Emit<'_>,
    wait_entry: Option<(Instant, WallTime)>,
) -> Result<(Value, String, bool), Fail> {
    let mut normalised = command.clone();
    let mut projects = std::collections::BTreeSet::new();
    let mut first = None;
    for target in &mut normalised.id {
        let candidate = targets::resolve_with_budget(edge, target, true, wait_entry.is_some())?;
        if let Some(project) = candidate.project {
            projects.insert(project);
        }
        *target = candidate.target.clone();
        if first.is_none() {
            first = Some(candidate);
        }
    }
    if projects.len() > 1 {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::Invalid,
            "targets belong to different projects; run one `provenant lanes --id ID --wait 4m` command per project",
        ));
    }
    let command = &normalised;
    if let Some(host) = command.host.as_deref()
        && !remote::names_local_host(edge, host)?
    {
        return remote::host_lanes(command, host, edge, json, emit);
    }
    if let Some(placed) = remote::lanes(command, edge, json, emit)? {
        return Ok(placed);
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = if wait_entry.is_some() {
        run::Request {
            clock: &clock,
            budget: WaitBudget::nonblocking(clock.mono_now()),
            cancel: &cancel,
        }
    } else {
        request(&clock, &cancel)?
    };
    let (context, store) = if let Some(candidate) = first {
        let (session, _) = targets::read_resolved(edge, candidate, &call)?;
        (session.context, Some(session.store))
    } else {
        open_read_only(edge, &call)?
    };
    let Some(store) = store else {
        let root = execution_state(edge.roots, edge.home, edge.assets)?;
        let cooldowns = read_lane_cooldowns(root.as_path());
        let (mut data, human) = render_lanes(
            &[],
            &[],
            None,
            root.as_path(),
            lane_cooldown_view(&cooldowns),
        );
        if command.wait.is_some() || command.watch.is_some() {
            // No store means no lanes, so there is nothing left to wait for.
            data["condition_met"] = json!(true);
        }
        let human = if command.watch.is_some() && !json {
            "end settled lanes=0\n".into()
        } else {
            human
        };
        return Ok((data, human, false));
    };
    let session = Session { store, context };
    let owner = match command.session.as_deref() {
        Some(label) => session_actor(edge, label)?,
        None => session.context.actor_id,
    };
    let since = command
        .since
        .map(|since| WallTime::from_micros(since.as_microsecond()))
        .transpose()
        .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid --since value"))?;
    let picks = Picks {
        targets: &command.id,
        owner,
        label: command.session.as_deref(),
        since,
    };
    if let Some(seconds) = command.watch {
        return watch_command(&session, &picks, seconds, json, emit);
    }
    if let Some((deadline, entered)) = wait_entry {
        return wait_command(&session, &picks, command.condition, deadline, entered, edge);
    }
    let after = match &command.cursor {
        Some(cursor) => cursor
            .parse()
            .map_err(|_| Fail::new(Status::Invalid, ErrorKind::Invalid, "invalid cursor"))?,
        None => 0,
    };
    let mut query = lane_query(
        &session,
        &command.id,
        command.all,
        command.limit,
        after,
        &call,
    )?;
    if command.session.is_some() && command.id.is_empty() {
        query.owner = Some(owner);
    }
    let by_session =
        |lane: &run::AttemptRecord| command.session.is_none() || lane.verified_owner == Some(owner);
    let filtered = !command.states.is_empty() || command.since.is_some();
    query.newest_first = filtered;
    let (lanes, next) = if filtered {
        query.limit = 100;
        let mut matches = Vec::new();
        let mut next = None;
        loop {
            let page = session.service().lanes(&query, &call)?;
            for lane in page.lanes {
                let state_matches = command.states.is_empty()
                    || command
                        .states
                        .iter()
                        .any(|state| state.as_str() == state_name(&lane.state));
                let since_matches = since.is_none_or(|since| lane.queued_at >= since);
                if state_matches && since_matches && by_session(&lane) {
                    matches.push(lane);
                }
                if matches.len() > command.limit as usize {
                    let last = matches.get(command.limit as usize - 1).ok_or_else(|| {
                        Fail::new(Status::Error, ErrorKind::Internal, "lane page invariant")
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
        let page = session.service().lanes(&query, &call)?;
        let lanes: Vec<_> = page.lanes.into_iter().filter(by_session).collect();
        (lanes, page.next)
    };
    let waits = session.store.lane_waits(&lanes, &call);
    let cooldowns = read_lane_cooldowns(session.context.state_root.as_path());
    let (data, human) = render_lanes(
        &lanes,
        &waits,
        next,
        session.context.state_root.as_path(),
        lane_cooldown_view(&cooldowns),
    );
    Ok((data, human, false))
}

/// What a pull request body needs from one local attempt's own record.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptFacts {
    pub attempt_id: AttemptId,
    pub run_id: RunId,
    pub task_id: TaskId,
    pub state: String,
    pub exit_code: Option<i64>,
    /// `adapter/model@effort`, as the attempt ran.
    pub route: String,
    pub head: Option<provenant_core::GitOid>,
    /// The branch the attempt's host delivery pushed.
    pub branch: Option<String>,
    pub delivery_id: Option<DeliveryId>,
    /// The attempt's receipt directory, relative to the state root.
    pub receipt_path: String,
    /// The attempt's delivery receipt: risk tier and review attempts.
    pub risk: Option<String>,
    pub reviews: Vec<AttemptId>,
}
fn route_label(record: &run::AttemptRecord) -> String {
    let ran = record
        .observed_route
        .as_ref()
        .or(record.applied_route.as_ref())
        .or(record.resolved_route.as_ref());
    let (requested_model, requested_effort) = match &record.requested_route {
        run::RequestedRoute::Model { model, effort, .. } => (Some(model.clone()), effort.clone()),
        run::RequestedRoute::Named { effort, .. } | run::RequestedRoute::Review { effort, .. } => {
            (None, effort.clone())
        }
    };
    let Some(model) = ran
        .and_then(|route| {
            let model = route.model.clone()?;
            Some(match &route.provider {
                Some(provider) if !model.starts_with(&format!("{provider}/")) => {
                    format!("{provider}/{model}")
                }
                _ => model,
            })
        })
        .or(requested_model)
    else {
        return "unresolved".into();
    };
    match ran
        .and_then(|route| route.effort.clone())
        .or(requested_effort)
    {
        Some(effort) => format!("{model}@{effort}"),
        None => model,
    }
}
/// The recorded facts of local attempts, for the pull request body. An attempt
/// that ran on a peer is refused: its record lives on that host.
pub fn attempt_facts(
    edge: &Edge<'_>,
    attempts: &[crate::Attempt],
) -> Result<Vec<AttemptFacts>, Fail> {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    attempts
        .iter()
        .map(|attempt| {
            let candidate = targets::resolve(edge, &attempt.0, false)?;
            if targets::placement(edge, &candidate.target, false)?.is_some() {
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Unsupported,
                    "that attempt ran on a peer; its record is read on that host",
                ));
            }
            let (session, selected) = targets::read_resolved(edge, candidate, &call)?;
            let id = targets::local(&session, &selected, &call)?
                .attempt
                .ok_or(Fail::new(
                    Status::Invalid,
                    ErrorKind::Invalid,
                    "attempt required",
                ))?;
            let record = session.service().status(id, &call)?;
            let (state, _) = state_and_reason(&record.state);
            let exit_code = match &record.state {
                run::AttemptState::Terminal { exit_code, .. } => *exit_code,
                _ => None,
            };
            let (risk, reviews) = match record.delivery_id {
                Some(delivery) => {
                    let receipt = provenant_workflow::store::ReceiptStore::new(
                        session.context.state_root.clone(),
                        session.context.project_id,
                        delivery,
                    )
                    .read()
                    .map_err(|error| {
                        Fail::new(Status::Error, ErrorKind::StateCorrupt, error.to_string())
                    })?;
                    match receipt {
                        Some(receipt) => (
                            serde_json::to_value(receipt.risk().tier())
                                .ok()
                                .and_then(|tier| tier.as_str().map(str::to_owned)),
                            receipt.review_attempts().to_vec(),
                        ),
                        None => (None, Vec::new()),
                    }
                }
                None => (None, Vec::new()),
            };
            Ok(AttemptFacts {
                attempt_id: record.attempt_id,
                run_id: record.run_id,
                task_id: record.task_id,
                state,
                exit_code,
                route: route_label(&record),
                head: record
                    .head
                    .or_else(|| record.delivery.as_ref().and_then(|d| d.head)),
                branch: record.delivery.as_ref().map(|d| d.branch.clone()),
                delivery_id: record.delivery_id,
                receipt_path: format!("runs/{}/attempts/{}", record.run_id, record.attempt_id),
                risk,
                reviews,
            })
        })
        .collect()
}

pub fn output(command: Output, edge: &Edge<'_>) -> Reply {
    let receipt_tail = matches!(command.part, Part::Receipt)
        .then_some(command.tail)
        .flatten();
    let headed = matches!(command.part, Part::Result) && !command.trace;
    let mut out = reply(
        "output",
        output_inner(command, edge).map(|(mut data, mut text)| {
            if let Some(lines) = receipt_tail {
                text =
                    text[tail_start(text.as_bytes(), lines.min(limits::TAIL_LINES))..].to_owned();
                data["tail"] = json!(lines.min(limits::TAIL_LINES));
                data["tail_text"] = json!(text);
            }
            (data, text)
        }),
    );
    out.read_only = true;
    if out.result.error.is_none() {
        out.summary = Some(output_summary(&out.result.data, &out.human, headed));
    }
    out
}

/// What an agent needs from a read: how the attempt settled, what it changed,
/// and the whole final text up to a generous bound, then its head and tail.
/// Missing changed-file evidence stays null; it must never read as an empty diff.
fn output_summary(data: &Value, text: &str, headed: bool) -> crate::Summary {
    let mut lines = Vec::new();
    if headed && let Some(state) = data["state"].as_str() {
        let reason = data["reason"].as_str();
        lines.push(match reason {
            Some(reason) => format!(
                "settlement: {} ({})",
                crate::bounded_text(state, 64),
                crate::bounded_text(reason, 160)
            ),
            None => format!("settlement: {}", crate::bounded_text(state, 64)),
        });
    }
    if headed && let Some(files) = data.get("changed_files") {
        lines.push(match files.as_array() {
            Some(paths) => {
                let total = data["changed_files_total"]
                    .as_u64()
                    .map_or(paths.len(), |total| total as usize);
                let more = total.saturating_sub(paths.len());
                let listed = paths
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = if more > 0 {
                    format!(" (+{more} more)")
                } else {
                    String::new()
                };
                format!("changed files ({total}): {listed}{more}")
            }
            None => "changed files: unavailable".into(),
        });
    }
    let body = crate::head_and_tail(text, limits::OUTPUT_BYTES);
    lines.extend(body.lines().map(str::to_owned));
    let target = data["attempt_id"].as_str().map(|id| {
        data["host"]
            .as_str()
            .map_or_else(|| id.to_owned(), |host| format!("{host}/{id}"))
    });
    let state = data["state"].as_str().unwrap_or_default();
    let next = target.and_then(|target| {
        if data["eof"] == json!(false) {
            let offset = data["next_offset"].as_u64()?;
            Some(format!(
                "provenant output {target} --part {} --offset {offset}",
                data["part"].as_str().unwrap_or("result")
            ))
        } else if matches!(state, "queued" | "starting" | "running" | "settling") {
            Some(format!("provenant lanes --id {target} --wait 4m"))
        } else {
            None
        }
    });
    crate::Summary {
        lines,
        next,
        omitted: 0,
    }
}
fn receipt_model(adapter: Option<&str>, model: Option<&str>) -> Option<String> {
    let adapter = adapter.filter(|value| !value.is_empty())?;
    let model = model.filter(|value| !value.is_empty())?;
    Some(if model.starts_with(&format!("{adapter}/")) {
        model.to_owned()
    } else {
        format!("{adapter}/{model}")
    })
}

fn receipt_family(model: Option<&str>) -> String {
    provenant_routing::Catalogue::bundled()
        .ok()
        .and_then(|catalogue| {
            ModelId::parse(model?)
                .ok()
                .and_then(|id| catalogue.model(&id).and_then(|entry| entry.family.clone()))
        })
        .unwrap_or_else(|| "unknown".into())
}

fn receipt_resolution(record: &run::AttemptRecord, fallback: &[run::RouteRecord]) -> String {
    let Some(route) = record.applied_route.as_ref() else {
        return "unknown".into();
    };
    let Some(applied) = receipt_model(route.provider.as_deref(), route.model.as_deref()) else {
        return "unknown".into();
    };
    // The recorded pick proves a pool result even when other candidates were unavailable.
    if let Some(pool) = &route.pool_selection {
        return if pool.pick == applied {
            format!("pool {}", pool.pool)
        } else {
            "unknown".into()
        };
    }
    let run::RequestedRoute::Model {
        provider, model, ..
    } = &record.requested_route
    else {
        return "unknown".into();
    };
    let Some(requested) = receipt_model(Some(provider), Some(model)) else {
        return "unknown".into();
    };
    if applied == requested {
        return "explicit".into();
    }
    if record
        .skipped_routes
        .iter()
        .any(|skip| skip.route == requested)
        && fallback.iter().any(|route| {
            receipt_model(route.provider.as_deref(), route.model.as_deref()).as_ref()
                == Some(&applied)
        })
    {
        "fallback".into()
    } else {
        "unknown".into()
    }
}

fn reviewer_receipt(
    record: &provenant_records::run::AttemptRecord,
    writer: bool,
    fallback: &[run::RouteRecord],
) -> Result<(Value, String), Fail> {
    use provenant_records::run::AttemptState;
    if !matches!(record.state, AttemptState::Terminal { .. }) {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Nonterminal,
            "receipt requires a settled attempt",
        ));
    }
    let route = record.applied_route.as_ref();
    let adapter = route
        .and_then(|r| r.provider.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let model = route
        .and_then(|r| r.model.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let qualified_model =
        route.and_then(|r| receipt_model(r.provider.as_deref(), r.model.as_deref()));
    let effort = route
        .and_then(|r| r.effort.as_deref())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let family = receipt_family(qualified_model.as_deref());
    let resolved_from = receipt_resolution(record, fallback);
    let reviewer = run::fact(record.clone())?.reviewer;
    let reviewer_model = record
        .observed_route
        .as_ref()
        .and_then(|route| receipt_model(route.provider.as_deref(), route.model.as_deref()));
    let reviewer_family = receipt_family(reviewer_model.as_deref());
    let provenance = reviewer
        .as_ref()
        .map_or("unknown", |reviewer| match reviewer.provenance {
            provenant_records::ReviewerProvenance::Observed => "observed",
            provenant_records::ReviewerProvenance::ChairAttested => "chair_attested",
            provenant_records::ReviewerProvenance::Unknown => "unknown",
        });
    let actor = reviewer
        .as_ref()
        .map_or_else(|| "unknown".into(), |r| r.actor_id.to_string());
    let reviewer_model = reviewer
        .as_ref()
        .and_then(|r| r.model.as_deref())
        .unwrap_or("unknown");
    let reviewer_family = if reviewer.is_some() {
        reviewer_family
    } else {
        "unknown".into()
    };
    let role = if writer { "writer" } else { "reviewer" };
    let label = if writer { "Writer" } else { "Reviewer" };
    let qualified_model = qualified_model.as_deref().unwrap_or("unknown");
    let line = format!(
        "{label}: {qualified_model}@{effort} ({family}; resolved from {resolved_from}), run {}/{}/{}",
        record.run_id, record.task_id, record.attempt_id
    );
    let attribution = format!(
        "Reviewer provenance: {provenance}, actor {actor}, model {reviewer_model}, family {reviewer_family}"
    );
    Ok((
        json!({"run_id": record.run_id, "task_id": record.task_id, "attempt_id": record.attempt_id, "adapter": adapter, "model": model, "effort": effort, "family": family, "role": role, "resolved_from": resolved_from, "receipt": line, "reviewer": {"provenance": provenance, "actor_id": actor, "model": reviewer_model, "family": reviewer_family}, "reviewer_provenance": attribution}),
        format!("{line}\n{attribution}\n"),
    ))
}
/// The most lines `output --tail` returns; a larger request is clamped.
const TAIL_MAX: u64 = limits::TAIL_LINES;

fn output_inner(mut command: Output, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    let site = match command.host.as_deref() {
        Some(host) => Some(remote::site(edge, host)?),
        None => command
            .attempt
            .0
            .run
            .or_else(|| {
                command
                    .attempt
                    .0
                    .selection
                    .as_deref()?
                    .split_once('/')?
                    .0
                    .parse()
                    .ok()
            })
            .and_then(|host| remote::qualifier_site(edge, host)),
    };
    if let Some(remote::Site::Peer(host)) = site {
        if command.attempt.0.selection.is_some() {
            command.attempt.0 = remote::resolve_prefix(edge, host, &command.attempt.0)?;
        }
        return remote::output_on_peer(&command, host, edge);
    }
    // `HOST/ATTEMPT` is the form a graph `attempt_ref` has; a host qualifier
    // is not a run, so it never has to match the record.
    if command.attempt.is_qualified_pair()
        && let Some(qualifier) = command.attempt.0.run
    {
        match remote::qualifier_site(edge, qualifier) {
            Some(remote::Site::Peer(host)) if site.is_none() => {
                return remote::output_on_peer(&command, host, edge);
            }
            Some(remote::Site::Local) => command.attempt.0.run = None,
            _ => {}
        }
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let receipt = matches!(command.part, Part::Receipt);
    let candidate = targets::resolve(edge, &command.attempt.0, receipt)?;
    command.attempt.0 = candidate.target.clone();
    if site.is_none()
        && let Some(placement) = targets::placement(edge, &candidate.target, false)?
    {
        return remote::output(&command, &placement, edge);
    }
    let (session, target) = targets::read_resolved(edge, candidate, &call)?;
    let id = match target.attempt {
        Some(id) => id,
        None if receipt => {
            let page = session.service().lanes(
                &lane_query(&session, std::slice::from_ref(&target), true, 2, 0, &call)?,
                &call,
            )?;
            if page.lanes.len() != 1 || page.next.is_some() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::Conflict,
                    "receipt target must identify exactly one attempt; specify RUN/TASK/ATTEMPT",
                ));
            }
            page.lanes[0].attempt_id
        }
        None => {
            return Err(Fail::new(
                Status::Invalid,
                ErrorKind::Invalid,
                "attempt required",
            ));
        }
    };
    let record = session.service().status(id, &call)?;
    if target.run.is_some_and(|run| record.run_id != run)
        || target.task.is_some_and(|task| task != record.task_id)
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            "attempt is not in that run/task",
        ));
    }
    if command.trace {
        let grants = json!({
            "read_only_writable": record.read_only_writable,
            "write_grants": record.write_grants,
            "git_grants_digest": record.git_grants_digest,
        });
        let grant_lines = grants_lines(&record);
        let trace = trace_chain(&session.store, record, &call)?;
        let human = trace
            .iter()
            .map(trace_line)
            .chain([grant_lines])
            .collect::<String>();
        return Ok((
            json!({ "attempt_id": id, "trace": trace, "grants": grants }),
            human,
        ));
    }
    let part = match command.part {
        Part::Receipt => {
            let task = session
                .store
                .request(record.run_id, &call)?
                .tasks
                .into_iter()
                .find(|task| task.task_id == record.task_id)
                .ok_or(run::RunError::NotFound)?;
            return reviewer_receipt(&record, task.writer.is_some(), &task.fallback);
        }
        Part::Result => run::ArtifactPart::Result,
        Part::Prompt => run::ArtifactPart::Prompt,
        Part::Stderr => run::ArtifactPart::Stderr,
        Part::Transcript => run::ArtifactPart::Transcript,
        Part::Events => {
            // An attempt has a handful of revisions; offset is an event cursor.
            let mut events = session
                .service()
                .events(command.offset, 64, Some(id), &call)?;
            if let Some(lines) = command.tail {
                let mut cursor = events.last().map_or(command.offset, |event| event.sequence);
                let mut more = events.len() == 64;
                while more {
                    let page = session.service().events(cursor, 64, Some(id), &call)?;
                    more = page.len() == 64;
                    let next = page.last().map_or(cursor, |event| event.sequence);
                    if more && next <= cursor {
                        return Err(Fail::new(
                            Status::Error,
                            ErrorKind::StateCorrupt,
                            "event cursor did not advance",
                        ));
                    }
                    cursor = next;
                    events.extend(page);
                    let excess = events.len().saturating_sub(lines.min(TAIL_MAX) as usize);
                    events.drain(..excess);
                }
                let excess = events.len().saturating_sub(lines.min(TAIL_MAX) as usize);
                events.drain(..excess);
                while !events.is_empty()
                    && serde_json::to_vec(&events)
                        .map_err(|error| {
                            Fail::new(Status::Error, ErrorKind::Io, error.to_string())
                        })?
                        .len() as u64
                        > command.max_bytes
                {
                    events.remove(0);
                }
            }
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
    let mut chunk = session.service().output(id, part, range, &call)?;
    let tail = command.tail.map(|lines| lines.min(TAIL_MAX));
    if let Some(lines) = tail {
        range.offset = chunk.len.saturating_sub(command.max_bytes);
        // A short transcript's first page already includes the requested tail.
        if !matches!(part, run::ArtifactPart::Transcript) || range.offset != chunk.offset {
            chunk = session.service().output(id, part, range, &call)?;
        }
        let start = tail_start(&chunk.bytes, lines);
        chunk.offset += start as u64;
        chunk.bytes.drain(..start);
    }
    align_utf8_tail(&mut chunk);
    let mut text = String::from_utf8_lossy(&chunk.bytes).into_owned();
    if matches!(command.part, Part::Stderr) && !chunk.complete {
        text.insert_str(0, "[stderr truncated; retained tail]\n");
    }
    // The text alone cannot tell a clean success from one that needs the chair.
    let (state, reason) = state_and_reason(&record.state);
    let mut data = json!({
            "attempt_id": id,
            "state": state,
            "reason": reason,
            "part": format!("{:?}", command.part).to_lowercase(),
            "offset": chunk.offset,
            "next_offset": chunk.next_offset,
            "length": chunk.len,
            "eof": chunk.eof,
            "clipped": chunk.clipped,
            "complete": chunk.complete,
            "digest": chunk.digest,
            "bytes": provenant_fabric_runtime::coordination::facade::OutputBytes::from_bytes(chunk.bytes),
    });
    if let Some(lines) = tail {
        data["tail"] = json!(lines);
        data["tail_clamped"] = json!(command.tail != tail);
    }
    if matches!(command.part, Part::Result) && command.tail.is_none() && command.offset == 0 {
        let (paths, total) = changed_file_evidence(&session, &record, &call);
        data["changed_files"] = paths;
        data["changed_files_total"] = total;
    }
    Ok((data, text))
}

/// Use the recorded base and final head, never the caller's current checkout
/// diff. Pruned/private objects and read-only attempts may have no evidence.
fn changed_file_evidence(
    session: &Session,
    record: &run::AttemptRecord,
    call: &run::Request<'_>,
) -> (Value, Value) {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let read = || -> Option<(Vec<String>, usize)> {
        let task = session
            .store
            .request(record.run_id, call)
            .ok()?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)?;
        let project = task.writer?.project;
        let base: provenant_core::GitOid = task.base?.parse().ok()?;
        let head = record.head?;
        let git = provenant_os::git::git_executable()?;
        let argv = [
            git.into_os_string(),
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--name-only".into(),
            "-z".into(),
            base.to_string().into(),
            head.to_string().into(),
            "--".into(),
        ];
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(5));
        options.stdout = StreamPolicy::Capture { limit: 65536 };
        options.stderr = StreamPolicy::Discard;
        options.stop_on_output_limit = true;
        let output = SystemExecutor::new(1).ok()?.execute(options).ok()?;
        if output.termination != Termination::Exit(0)
            || output.timed_out
            || output.cancelled
            || output.stdout.truncated
            || output.stdout.read_failed
            || output.drain_truncated
        {
            return None;
        }
        let text = String::from_utf8(output.stdout.bytes).ok()?;
        let all: Vec<_> = text.split('\0').filter(|path| !path.is_empty()).collect();
        Some((
            all.iter()
                .take(limits::OUTPUT_PATHS)
                .map(|path| crate::bounded_text(path, limits::PATH_BYTES))
                .collect(),
            all.len(),
        ))
    };
    read().map_or((Value::Null, Value::Null), |(paths, total)| {
        (json!(paths), json!(total))
    })
}

fn tail_start(bytes: &[u8], lines: u64) -> usize {
    bytes
        .strip_suffix(b"\n")
        .unwrap_or(bytes)
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, b)| **b == b'\n')
        .nth(usize::try_from(lines.saturating_sub(1)).unwrap_or(usize::MAX))
        .map_or(0, |(i, _)| i + 1)
}

/// The attempt's trace, then each parent's, nearest first. The chain ends at an
/// attempt with no parent, or at a parent this store does not hold (a nested
/// run's submitter lives in its parent's store), which appears as a hop that
/// knows only its attempt ID. A parent that repeats is a corrupt store and
/// fails.
fn trace_chain(
    store: &run::RunStore,
    record: run::AttemptRecord,
    call: &run::Request<'_>,
) -> Result<Vec<provenant_records::Trace>, Fail> {
    let mut seen = std::collections::HashSet::from([record.attempt_id]);
    let mut chain = vec![record.trace];
    while let Some(parent) = chain.last().and_then(|t| t.parent) {
        if !seen.insert(parent) {
            return Err(Fail::new(
                Status::Error,
                ErrorKind::Io,
                format!("trace parents loop back to attempt {parent}"),
            ));
        }
        match run::Service::new(store).status(parent, call) {
            Ok(record) => chain.push(record.trace),
            Err(run::RunError::NotFound) => {
                chain.push(provenant_records::Trace {
                    attempt: Some(parent),
                    ..Default::default()
                });
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(chain)
}
/// The grants the launch applied, one line each. Older attempts show none.
fn grants_lines(record: &run::AttemptRecord) -> String {
    let mut text = String::new();
    for grant in &record.write_grants {
        let path = grant
            .path
            .to_path_buf()
            .map_or_else(|_| "-".to_owned(), |p| p.display().to_string());
        let [source, scope] = [
            serde_json::to_value(grant.source),
            serde_json::to_value(grant.scope),
        ]
        .map(|v| {
            v.ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        });
        text.push_str(&format!(
            "grant write source={source} scope={scope} path={}\n",
            plain_text(&path, usize::MAX)
        ));
    }
    for path in &record.read_only_writable {
        text.push_str(&format!(
            "grant read_only_writable path={}\n",
            plain_text(path, usize::MAX)
        ));
    }
    if let Some(digest) = record.git_grants_digest {
        text.push_str(&format!("grant git digest={digest}\n"));
    }
    text
}
fn trace_line(trace: &provenant_records::Trace) -> String {
    fn field(name: &str, value: Option<impl std::fmt::Display>) -> String {
        format!(
            "{name}={}",
            value.map_or_else(|| "-".to_owned(), |v| v.to_string())
        )
    }
    format!(
        "{} {} {} {} {} {} {}\n",
        field("host", trace.host),
        field("project", trace.project),
        field("run", trace.run),
        field("attempt", trace.attempt),
        field("operation", trace.operation.as_ref().map(|o| o.as_str())),
        field("actor", trace.actor),
        field("parent", trace.parent),
    )
}

/// A clipped page that ends inside a UTF-8 character stops before it, so the
/// next offset starts the character whole and paged text concatenates exactly.
pub fn align_utf8_tail(chunk: &mut run::OutputChunk) {
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
    let id = command.operation_id.clone();
    let mut out = cancel_inner(command, edge);
    operation_reply(&mut out, &id);
    out
}
fn cancel_inner(mut command: Cancel, edge: &Edge<'_>) -> Reply {
    let candidate = match targets::resolve(edge, &command.target, true) {
        Ok(candidate) => candidate,
        Err(fail) => return reply("cancel", Err(fail)),
    };
    command.target = candidate.target.clone();
    match targets::placement(edge, &candidate.target, true) {
        Ok(Some(placement)) => {
            if crate::fabric::in_lane() {
                return reply(
                    "cancel",
                    Err(Fail::new(
                        Status::Refused,
                        ErrorKind::LaneCoordinationRefused,
                        "coordination mutations are not allowed inside a lane",
                    )),
                );
            }
            return remote::cancel(&command, &placement, edge);
        }
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
            let (session, selected_target) = targets::open_resolved(edge, candidate, &call)?;
            let reason = command.reason.as_deref().unwrap_or("cancelled by request");
            let targets = [selected_target];
            let selected = select(&session, &targets, None, session.context.actor_id, &call)?;
            if selected.is_empty() {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "no matching attempts",
                ));
            }
            let target = &targets[0];
            let resolved = targets::local(&session, target, &call)?;
            let bound = match (resolved.attempt, resolved.task) {
                (Some(attempt), _) => attempt.to_string(),
                (None, Some(task)) => format!("{}/{task}", resolved.run),
                (None, None) => resolved.run.to_string(),
            };
            session
                .service()
                .bind_cancel(&command.operation_id, &bound, &call)?;
            let mut receipts = Vec::new();
            let mut human = String::new();
            let mut requested = false;
            for id in selected {
                // One failure must not hide the receipts already earned.
                let receipt = request(&clock, &cancel)
                    .and_then(|call| Ok(session.service().cancel(id, reason, &call)?));
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
                        json!({ "attempt_id": id, "error": { "code": fail.kind.code(), "message": fail.message } })
                    }
                };
                receipts.push(row);
            }
            let mut data = json!({ "operation_id": command.operation_id, "receipts": receipts });
            if requested {
                // A request has no actor once the guardian is gone, so recover
                // the run now: ending proven orphans and settling is the same
                // path `reconcile` takes, and a live guardian is left alone.
                let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
                let registry = Registry {
                    roots: Some(edge.roots),
                    home: Some(edge.home),
                    assets: Some(edge.assets),
                    cwd: Some(&scoped_cwd),
                    state_root: Some(session.context.state_root.as_path()),
                    project_id: Some(session.context.project_id),
                    host: None,
                    cancel: Some(&cancel),
                };
                let recovered = request(&clock, &cancel).and_then(|call| {
                    Ok(session
                        .service()
                        .reconcile(resolved.run, &registry, &call)?)
                });
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
                            human.push_str(&format!("{id} {}\n", reconcile_summary(state)));
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
                let (lanes, rendered, timed_out) = wait_lanes(
                    &session,
                    &targets,
                    Condition::All,
                    Duration::from_secs(seconds),
                    edge,
                )?;
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

fn reconcile_summary(state: &run::Reconciled) -> String {
    match state {
        run::Reconciled::TemporaryRetained { path, reason } => format!(
            "temporary_retained {}: {}",
            plain_text(&path.to_string_lossy(), usize::MAX),
            plain_text(reason, usize::MAX)
        ),
        run::Reconciled::Uncertain { reason } => {
            format!("uncertain: {}", plain_text(&reason.to_string(), usize::MAX))
        }
        _ => state_tag(state),
    }
}

/// Continue one terminal attempt with new input as a new single-task run on
/// the same route and access.
pub fn resume(command: Resume, edge: &Edge<'_>) -> Reply {
    let id = command.operation_id.clone();
    let mut out = resume_reply(command, edge);
    operation_reply(&mut out, &id);
    out
}
fn resume_reply(command: Resume, edge: &Edge<'_>) -> Reply {
    reply_waited("resume", resume_inner(command, edge))
}
pub fn lane(command: crate::Lane, edge: &Edge<'_>) -> Reply {
    reply(
        "lane deliver",
        (|| {
            if crate::fabric::in_lane() {
                return Err(Fail::new(
                    Status::Refused,
                    ErrorKind::Refused,
                    "delivery is host-only",
                ));
            }
            let crate::Lane::Deliver {
                attempt,
                deliver,
                accept_review,
            } = command;
            let clock = SystemClock;
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel)?;
            let (session, selected) = targets::open(edge, &attempt.0, false, &call)?;
            let attempt = crate::Attempt(selected);
            let id = attempt.0.attempt.ok_or_else(|| {
                Fail::new(Status::Invalid, ErrorKind::Invalid, "attempt required")
            })?;
            let record = session.store.record(id, &call)?;
            if attempt.0.run.is_some_and(|run| run != record.run_id)
                || attempt.0.task.is_some_and(|task| task != record.task_id)
            {
                return Err(Fail::new(
                    Status::Invalid,
                    ErrorKind::NotFound,
                    "attempt is not in that run/task",
                ));
            }
            if matches!(deliver, Some(run::DeliveryMode::None)) {
                return Ok((
                    json!({"delivery": "none"}),
                    "No delivery requested.\n".into(),
                ));
            }
            let task = session
                .store
                .request(record.run_id, &call)?
                .tasks
                .into_iter()
                .find(|task| task.task_id == record.task_id)
                .ok_or(run::RunError::NotFound)?;
            let writer = task
                .writer
                .as_ref()
                .ok_or(run::RunError::Validation("delivery needs a writer"))?;
            if !writer.worktree.exists()
                && session
                    .store
                    .worktree_settlement(id, &call)?
                    .is_some_and(|receipt| worktree_was_pruned(&receipt, &writer.worktree))
                && let Some((request, receipt)) = session.store.delivered(id, &call)?
                && deliver.as_ref().is_none_or(|mode| {
                    mode == &request.mode
                        || matches!(mode, run::DeliveryMode::Pr) && request.open_pr
                })
            {
                return Ok((
                    json!({"attempt_id": id, "delivery": receipt, "replayed":true}),
                    format!("{id} already delivered; owned worktree pruned\n"),
                ));
            }
            let launch =
                run::prepare_writer(writer, edge.roots.state.as_path(), record.project_id)?;
            let branch = launch.branch.as_str().trim_start_matches("refs/heads/");
            let destination = match deliver {
                Some(mode) => run::HostDeliveryContext::destination(mode, branch)?,
                None if task.deliver.is_none() => {
                    run::HostDeliveryContext::destination(run::DeliveryMode::Push, branch)?
                }
                None => None,
            };
            let receipt = session.store.deliver(
                id,
                destination,
                run::HostDeliveryContext::capture(),
                accept_review,
                &call,
            )?;
            if let run::WriterDeliveryState::Failed { stage, .. } = &receipt.state {
                return Err(run::RunError::Delivery(*stage).into());
            }
            Ok((
                json!({"attempt_id": id, "delivery": receipt}),
                format!("{id} delivered\n"),
            ))
        })(),
    )
}
fn fallback_history(cwd: &Path, base: &str, displaced: bool) -> String {
    if displaced {
        return "[history unavailable: prior worktree is missing or reassigned]".into();
    }
    run::continuation_history(cwd, base)
        .unwrap_or_else(|error| format!("[history unavailable: {error}]"))
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
    continue_attempt_mode(
        session,
        edge,
        prior,
        operation_id,
        prompt,
        expected_generation,
        call,
        true,
    )
}
#[allow(clippy::too_many_arguments)]
pub fn continue_attempt_mode(
    session: &Session,
    edge: &Edge<'_>,
    prior: AttemptId,
    operation_id: OperationId,
    mut prompt: String,
    expected_generation: u64,
    call: &run::Request<'_>,
    fallback: bool,
) -> Result<(run::RunRecord, Vec<run::Started>), Fail> {
    scan(&prompt)?;
    let record = session.service().status(prior, call)?;
    if !matches!(record.state, run::AttemptState::Terminal { .. }) {
        return Err(run::RunError::Nonterminal.into());
    }
    // Replay the already accepted choice before checking state that retention
    // may have expired since the first submission.
    if let Some(operation) = session.store.operation(&operation_id, call)? {
        let existing = session.store.request(operation.run_id, call)?;
        if existing.parent != Some(prior) {
            return Err(run::RunError::OperationConflict.into());
        }
        let page = session.store.lanes(
            &run::LaneQuery {
                targets: vec![run::LaneTarget {
                    run: operation.run_id,
                    task: None,
                    attempt: None,
                }],
                all: true,
                limit: 2,
                after: 0,
                newest_first: false,
                owner: None,
                unseen_after: None,
            },
            call,
        )?;
        if let Some(accepted) = page.lanes.first() {
            let retained = read_full_prompt(session, accepted.attempt_id, call)?;
            let matches = if existing.fresh_continuation.is_some() {
                retained.ends_with(&format!("\n\nNew prompt:\n{prompt}"))
            } else {
                retained == prompt
            };
            if !matches {
                return Err(run::RunError::OperationConflict.into());
            }
            let run = session.store.run(operation.run_id, call)?;
            let started = launch(session, run.run_id, edge, call)?;
            return Ok((run, started));
        }
    }
    let ownership = session.store.check_continuation_writer(prior, call);
    let displaced = ownership.is_err();
    let owner_base = ownership
        .as_ref()
        .ok()
        .and_then(|owner| owner.as_ref())
        .map(|owner| owner.base.clone());
    let unavailable = ownership.err().map(|error| error.to_string()).or_else(|| {
        Registry::default()
            .check_continuable(&record)
            .err()
            .map(|fail| fail.message)
            .or_else(|| {
                provider::continuation_available(&record, session.context.state_root.as_path())
                    .err()
            })
    });
    if !fallback && let Some(reason) = &unavailable {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::ContinuationUnsupported,
            format!("continuation unavailable; fresh fallback disabled by --no-fallback: {reason}"),
        ));
    }
    if let Some(reason) = &unavailable {
        let task = session
            .store
            .request(record.run_id, call)?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(run::RunError::NotFound)?;
        let mut source = record.clone();
        let mut seen = std::collections::HashSet::new();
        while let Some(parent) = source.retry_of {
            if !seen.insert(parent) || seen.len() > 128 {
                return Err(run::RunError::Validation("continuation parent chain").into());
            }
            source = session.store.record(parent, call)?;
        }
        let original = read_full_prompt(session, source.attempt_id, call)?;
        let result = session
            .service()
            .output(
                prior,
                run::ArtifactPart::Result,
                run::ByteRange::default(),
                call,
            )
            .ok();
        let tail = if let Some(result) = result {
            let result = if result.len > 16_384 {
                session.service().output(
                    prior,
                    run::ArtifactPart::Result,
                    run::ByteRange {
                        offset: result.len - 16_384,
                        max_bytes: 16_384,
                    },
                    call,
                )?
            } else {
                result
            };
            String::from_utf8_lossy(&result.bytes).into_owned()
        } else {
            "[prior attempt has no result]".into()
        };
        let cwd = task.writer.as_ref().map_or(edge.cwd, |writer| {
            if displaced {
                writer.project.as_path()
            } else {
                writer.worktree.as_path()
            }
        });
        let history_base = owner_base
            .as_deref()
            .or(task.base.as_deref())
            .map(str::to_owned)
            .or_else(|| record.head.map(|head| head.to_string()))
            .unwrap_or_else(|| "HEAD".into());
        let log = fallback_history(cwd, &history_base, displaced);
        let digest = record
            .result
            .as_ref()
            .map(|artifact| provenant_core::hex(artifact.digest.bytes()))
            .unwrap_or_else(|| "unavailable".into());
        prompt = format!(
            "Continuation: fresh\nFallback from {prior}: {reason}\n\nOriginal prompt:\n{original}\n\nPrior result digest: {digest}\nPrior result tail:\n{tail}\n\nGit log from recorded base {history_base}:\n{log}\n\nNew prompt:\n{prompt}"
        );
        scan(&prompt)?;
    }
    let input = run::Resume {
        prior,
        expected_generation,
        operation_id,
        run_id: fresh(RunId::from_bytes)?,
        attempt_id: fresh(AttemptId::from_bytes)?,
        prompt,
        created_at: now(call.clock)?,
    };
    let fresh_writer = if displaced {
        Some(fresh_continuation_worktree(
            session, edge, &record, &input, call,
        )?)
    } else {
        None
    };
    let mut ready = None;
    let accepted = if let Some(reason) = &unavailable {
        if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_none() {
            let registry = Registry {
                roots: Some(edge.roots),
                home: Some(edge.home),
                assets: Some(edge.assets),
                cwd: Some(edge.cwd),
                state_root: Some(session.context.state_root.as_path()),
                project_id: Some(session.context.project_id),
                host: None,
                cancel: Some(call.cancel),
            };
            session
                .store
                .resume_fresh_prepared(
                    &input,
                    reason,
                    fresh_writer.as_ref().map(|(writer, _)| writer),
                    &registry,
                    call,
                )
                .map(|(run, prepared)| {
                    ready = prepared;
                    run
                })
        } else if let Some((writer, _)) = &fresh_writer {
            session.store.resume_fresh_in(&input, reason, writer, call)
        } else {
            session.store.resume_fresh(&input, reason, call)
        }
    } else {
        session.service().resume(&input, call)
    };
    let cleanup_failure = if accepted.is_err()
        && let Some((writer, head)) = &fresh_writer
    {
        // A refused continuation has not launched. Roll back only the new,
        // clean owned checkout, using the same ownership and custody fences.
        provenant_confinement::worktrees::with_context(
            edge.roots,
            edge.home,
            &writer.project,
            call.clock,
            call.cancel,
            |context| {
                use provenant_confinement::worktrees::{self, WorktreeGcPolicy};
                worktrees::prune_owned(
                    context,
                    &writer.worktree,
                    input.attempt_id,
                    *head,
                    *head,
                    &WorktreeGcPolicy {
                        upstream: None,
                        terminal: &|_| true,
                        no_live_lanes: &|_| true,
                        regenerable: &[],
                        keep: &[],
                        written_before_ns: i128::MAX,
                        foreign_merged_clean: false,
                        foreign_idle_before_ns: None,
                        dry_run: false,
                    },
                    call.cancel,
                )
            },
        )
        .err()
        .map(|error| {
            format!(
                "; cleanup retained fresh worktree at {} for gc: {error}",
                writer.worktree.display()
            )
        })
    } else {
        None
    };
    let run = accepted.map_err(|error| {
        Fail::new(
            Status::Refused,
            ErrorKind::ContinuationUnsupported,
            format!(
                "continuation {}: {error}{}",
                if unavailable.is_some() {
                    "fresh fallback refused"
                } else {
                    "provider resume refused"
                },
                cleanup_failure.as_deref().unwrap_or("")
            ),
        )
    })?;
    let started =
        launch_prepared(session, run.run_id, edge, call, ready.as_ref()).map_err(|fail| {
            if unavailable.is_some() {
                Fail::new(
                    Status::Failed,
                    fail.kind,
                    format!("accepted fresh fallback launch failed: {}", fail.message),
                )
            } else {
                fail
            }
        })?;
    // Admission has succeeded. Later custody or handoff failures are execution
    // outcomes of that accepted attempt, as they are for dispatch.
    Ok((run, started))
}
fn fresh_continuation_worktree(
    session: &Session,
    edge: &Edge<'_>,
    prior: &run::AttemptRecord,
    input: &run::Resume,
    call: &run::Request<'_>,
) -> Result<(run::WriterRequest, provenant_core::GitOid), Fail> {
    let task = session
        .store
        .request(prior.run_id, call)?
        .tasks
        .into_iter()
        .find(|task| task.task_id == prior.task_id)
        .ok_or(run::RunError::NotFound)?;
    let writer = task
        .writer
        .ok_or(run::RunError::Validation("fresh writer missing"))?;
    let base = prior.head.map(|head| head.to_string()).or(task.base);
    let base = match base {
        Some(base) => base,
        None => {
            let git = provenant_os::git::git_executable()
                .ok_or(run::RunError::Validation("Git unavailable"))?;
            run::resolve_git_base(&git, &writer.project, "HEAD")?
        }
    };
    let host = provenant_config::bind_host(edge.roots)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Config, error.to_string()))?;
    let mut retained = None;
    let info = provenant_confinement::worktrees::with_context(
        edge.roots,
        edge.home,
        &writer.project,
        call.clock,
        call.cancel,
        |context| {
            use provenant_confinement::worktrees::{
                self, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
            };
            let branch = format!("lane/resume-{}", input.attempt_id);
            let info = worktrees::create(
                context,
                WorktreeCreate {
                    branch: branch.clone(),
                    base: Some(base.clone()),
                },
                call.cancel,
            )?;
            let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
            let publication = worktrees::save_owner(
                &tree,
                &WorktreeOwner {
                    attempt_id: Some(input.attempt_id),
                    operation_id: Some(input.operation_id.clone()),
                    branch,
                    base: base.clone(),
                    host,
                    created_at: input.created_at,
                    origin: WorktreeOrigin::Lane,
                    label: None,
                },
            );
            if let Err(error) = publication {
                if worktrees::remove(
                    context,
                    worktrees::WorktreeRemove::Name {
                        name: format!("lane/resume-{}", input.attempt_id),
                        discard: false,
                        integration: None,
                        expected_head: Some(info.head),
                    },
                    call.cancel,
                )
                .is_err()
                {
                    retained = Some(info.path.clone());
                }
                return Err(error);
            }
            Ok(info)
        },
    )
    .map_err(|error| {
        let mut fail = Fail::from(run::RunError::from(error));
        if let Some(path) = retained {
            fail.message.push_str(&format!(
                "; cleanup retained fresh worktree at {} for gc",
                path.display()
            ));
        }
        fail
    })?;
    Ok((
        run::WriterRequest {
            extra: Default::default(),
            project: writer.project,
            worktree: info.path,
        },
        info.head,
    ))
}
fn read_full_prompt(
    session: &Session,
    id: AttemptId,
    call: &run::Request<'_>,
) -> Result<String, Fail> {
    let mut bytes = Vec::new();
    loop {
        let chunk = session.service().output(
            id,
            run::ArtifactPart::Prompt,
            run::ByteRange {
                offset: bytes.len() as u64,
                max_bytes: run::OUTPUT_LIMIT,
            },
            call,
        )?;
        if chunk.len > PROMPT_LIMIT || chunk.bytes.is_empty() && !chunk.eof {
            return Err(run::RunError::Validation("original prompt exceeds fallback bound").into());
        }
        bytes.extend(chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| run::RunError::Validation("original prompt is not UTF-8").into())
}
fn resume_inner(command: Resume, edge: &Edge<'_>) -> Result<(Value, String, bool), Fail> {
    if crate::fabric::in_lane() {
        return Err(Fail::new(
            Status::Refused,
            ErrorKind::Unsupported,
            "a lane cannot continue an attempt",
        ));
    }
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let candidate = targets::resolve(edge, &command.attempt.0, false)?;
    let prior = candidate.target.attempt.ok_or(Fail::new(
        Status::Invalid,
        ErrorKind::Invalid,
        "attempt required",
    ))?;
    let prompt = match &command.prompt.prompt_file {
        Some(path) => read_bounded(path)?,
        None => read_text(std::io::stdin().lock())?,
    };
    let (session, target) = targets::open_resolved(edge, candidate, &call)?;
    let prior = target.attempt.unwrap_or(prior);
    let record = session.service().status(prior, &call)?;
    if target.run.is_some_and(|run| record.run_id != run)
        || target.task.is_some_and(|task| task != record.task_id)
    {
        return Err(Fail::new(
            Status::Invalid,
            ErrorKind::NotFound,
            "attempt is not in that run/task",
        ));
    }
    // The CLI continues an attempt once; a second continuation of the same
    // attempt is a conflict, not a new branch.
    let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
    let scoped_edge = Edge {
        cwd: &scoped_cwd,
        ..*edge
    };
    let (run, started) = continue_attempt_mode(
        &session,
        &scoped_edge,
        prior,
        command.operation_id,
        prompt,
        0,
        &call,
        !command.no_fallback,
    )?;
    let (mut data, human, timed_out) =
        report(&session, &run, &started, command.wait, &scoped_edge, &call)?;
    data["resumed_from"] = json!(prior);
    let continuation = session.store.request(run.run_id, &call)?.fresh_continuation;
    data["continuation"] = json!(if continuation.is_some() {
        "fresh"
    } else {
        "provider"
    });
    data["fallback_reason"] = json!(continuation);
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
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let (runs, truncated) = if command.id.is_empty() {
        let session = open(edge, &request(&clock, &cancel)?)?;
        let mut runs = session
            .store
            .open_runs(SCAN + 1, &request(&clock, &cancel)?)?;
        let truncated = runs.len() as u64 > SCAN;
        runs.truncate(SCAN as usize);
        (
            runs.into_iter()
                .map(|run| Target {
                    selection: None,
                    run: Some(run),
                    task: None,
                    attempt: None,
                })
                .collect(),
            truncated,
        )
    } else {
        (command.id, false)
    };
    let mut rows = Vec::new();
    let mut human = String::new();
    for target in runs {
        let (session, selected) = targets::open(edge, &target, true, &request(&clock, &cancel)?)?;
        let run = selected.run.ok_or(run::RunError::NotFound)?;
        let scoped_cwd = targets::cwd(edge, session.context.project_id)?;
        let scoped_edge = Edge {
            cwd: &scoped_cwd,
            ..*edge
        };
        let registry = Registry {
            roots: Some(edge.roots),
            home: Some(edge.home),
            assets: Some(edge.assets),
            cwd: Some(&scoped_cwd),
            state_root: Some(session.context.state_root.as_path()),
            project_id: Some(session.context.project_id),
            host: None,
            cancel: Some(&cancel),
        };
        // Each step gets a fresh budget; recovery waits on custody locks.
        let found = match request(&clock, &cancel)
            .and_then(|call| Ok(session.service().reconcile(run, &registry, &call)?))
        {
            Ok(found) => found,
            Err(fail) => {
                *fenced = true;
                human.push_str(&format!("{run} error: {}\n", fail.message));
                rows.push(json!({ "run_id": run, "error": { "code": fail.kind.code(), "message": fail.message } }));
                continue;
            }
        };
        let started = if found.iter().any(|(_, r)| *r == run::Reconciled::Queued) {
            request(&clock, &cancel).and_then(|call| launch(&session, run, &scoped_edge, &call))
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
                human.push_str(&format!("{run}/{id} {}\n", reconcile_summary(state)));
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
    let reservations = open(edge, &request(&clock, &cancel)?)?
        .store
        .writer_fence_listing()?;
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

fn cache_kpi_rows(
    facts: &[&provenant_records::AttemptFact],
    domains: &crate::domains::Domains,
    now: i64,
) -> Value {
    let mut grouped: BTreeMap<(Option<String>, Option<String>), Vec<&provenant_records::Usage>> =
        BTreeMap::new();
    let share = |usage: &provenant_records::Usage| {
        provenant_routing::cache_read_share(
            usage.uncached_input,
            usage.cache_read,
            usage.cache_write,
        )
    };
    let lanes: Vec<_> = facts.iter().map(|fact| {
        let route = fact.applied_route.as_ref();
        let model = route.and_then(|route| route.model.clone());
        let account = route.and_then(|route| route.account_label.clone());
        grouped.entry((model.clone(), account.clone())).or_default().push(&fact.usage);
        json!({"attempt_id": fact.attempt_id, "model": model, "account": account, "share": share(&fact.usage)})
    }).collect();
    let routes: Vec<_> = grouped.into_iter().map(|((model, account), usages)| {
        let sum = |select: fn(&provenant_records::Usage) -> Option<u64>| usages.iter().try_fold(0u64, |sum, usage| sum.checked_add(select(usage)?));
        let target = model.as_deref().and_then(|model| ModelId::parse(model).ok()).and_then(|model| {
            let key = account.as_deref().and_then(|label| domains.accounts.entries.values().find(|entry| entry.adapter == model.adapter() && (entry.label == label || entry.key == label))).map(|entry| entry.key.as_str()).or(account.as_deref());
            domains.routing.model_policy.effective(&model, key, now).policy.cache.read_share_target
        });
        json!({"model": model, "account": account, "share": provenant_routing::cache_read_share(sum(|u| u.uncached_input), sum(|u| u.cache_read), sum(|u| u.cache_write)), "target": target})
    }).collect();
    json!({"scope": "project terminal attempts", "routes": routes, "lanes": lanes})
}
fn stats_inner(command: Stats, edge: &Edge<'_>) -> Result<(Value, String), Fail> {
    use run::AttemptReader;
    let path = edge.assets.join("config/telemetry-prices.toml");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        Fail::new(
            Status::Error,
            ErrorKind::Prices,
            format!("{}: {error}", path.display()),
        )
    })?;
    let prices = toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text)
        .map_err(|error| error.to_string())
        .and_then(|config| {
            provenant_telemetry::validate_prices(&config).map_err(|error| error.to_string())
        })
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Prices, error))?;
    if let Some(path) = command.claude_subagents.as_deref() {
        let native =
            provenant_telemetry::claude_subagents(path, &prices).map_err(|error| match error {
                provenant_telemetry::NativeError::Input(message) => {
                    Fail::new(Status::Invalid, ErrorKind::Invalid, message)
                }
                provenant_telemetry::NativeError::Io(message) => {
                    Fail::new(Status::Error, ErrorKind::Io, message)
                }
            })?;
        let mut human = format!("{} native Claude subagents\n", native.agents);
        if native.skipped_incomplete_lines > 0 {
            human.push_str(&format!(
                "{} incomplete final lines skipped\n",
                native.skipped_incomplete_lines
            ));
        }
        for row in &native.models {
            let cost = row.cost_usd.as_ref().map_or_else(
                || "unknown".to_owned(),
                |cost| format!("est. API-equivalent ${}", cost.usd),
            );
            let billing = match row.price_table_billing {
                Some(provenant_telemetry::BillingKind::Subscription) => {
                    " (price-table subscription model)"
                }
                _ => "",
            };
            human.push_str(&format!(
                "  {}: {} agents, {} messages, cost {}{}\n",
                plain_text(&row.model, usize::MAX),
                row.agents,
                row.messages,
                cost,
                billing
            ));
        }
        return Ok((json!({ "native_claude_subagents": native }), human));
    }
    let instant = |time: Option<jiff::Timestamp>| {
        time.map(|t| WallTime::from_micros(t.as_microsecond()))
            .transpose()
            .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = request(&clock, &cancel)?;
    let (context, store) = open_read_only(edge, &call)?;
    let selected_run = command
        .run_id
        .as_ref()
        .map(|target| targets::run_id(edge, target))
        .transpose()?;
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
                        run_id: selected_run,
                        task_id: None,
                    },
                    &call,
                )?
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Config, error.to_string()))?;
    let selected: Vec<_> = facts
        .iter()
        .filter(|fact| {
            window.since.is_none_or(|s| fact.finished_at >= s)
                && window.until.is_none_or(|u| fact.finished_at < u)
        })
        .collect();
    let cache_kpis = cache_kpi_rows(
        &selected,
        &config.domains,
        jiff::Timestamp::now().as_second(),
    );
    // Chair sessions span projects, so the host's whole window is read. They are
    // best effort: an unreadable usage log must not hide the lane figures.
    let chair_window = (
        window
            .since
            .map_or(jiff::Timestamp::MIN, |t| t.as_timestamp()),
        window
            .until
            .map_or(jiff::Timestamp::MAX, |t| t.as_timestamp()),
    );
    let (chairs, chair_error) = match crate::quota::chair_sessions_with_usage(
        edge.roots,
        Some(edge.home),
        chair_window.0,
        chair_window.1,
    ) {
        Ok(chairs) => (chairs, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let report = provenant_telemetry::stats_with_chair(facts, chairs, &prices, window)
        .map_err(|error| Fail::new(Status::Invalid, ErrorKind::Invalid, error.to_string()))?;
    let mut human = stats_summary(&report);
    let total_share = totals_cache_read_share(&report.totals);
    human.push_str(&format!(
        "cache-read share: {}\n",
        total_share.map_or_else(
            || "unknown (incomplete input categories)".into(),
            |share| format!("{:.1}%", share * 100.0)
        )
    ));
    let mut data = serde_json::to_value(&report)
        .map_err(|error| Fail::new(Status::Error, ErrorKind::Io, error.to_string()))?;
    data["cache_read_share"] = cache_kpis;
    data["cache_read_share"]["overall"] = json!(total_share);
    if let Some(error) = chair_error {
        human.push_str(&format!(
            "chair sessions not read: {}\n",
            plain_text(&error, usize::MAX)
        ));
        data["chair_error"] = Value::String(error);
    }
    Ok((data, human))
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
        budget: WaitBudget::cli(clock.mono_now(), 1)
            .map_err(|e| Fail::new(Status::Error, ErrorKind::Time, e.to_string()))?,
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
mod writer_disk_tests {
    use super::*;

    #[test]
    fn dispatch_policy_refusal_preserves_launched_sibling() -> Result<(), String> {
        check_launch_policy(&[
            run::Started::Refused {
                attempt: AttemptId::from_bytes([1; 16]),
                kind: run::RefusalKind::Policy,
                reason: "insufficient disk space".into(),
            },
            run::Started::Launched(AttemptId::from_bytes([2; 16])),
        ])
        .map_err(|error| error.message)
    }

    #[test]
    fn policy_refusal_preserves_uncertain_sibling_handoff() -> Result<(), String> {
        let refused = || run::Started::Refused {
            attempt: AttemptId::from_bytes([1; 16]),
            kind: run::RefusalKind::Policy,
            reason: "insufficient disk space".into(),
        };
        let error = check_launch_policy(&[refused()])
            .err()
            .ok_or("policy refusal was ignored")?;
        assert_eq!(error.status, Status::Refused);
        assert_eq!(error.kind, ErrorKind::Refused);
        // Let the existing unknown-handoff handling retain the full receipt.
        check_launch_policy(&[
            refused(),
            run::Started::Unknown {
                attempt: AttemptId::from_bytes([2; 16]),
                error: "handoff unconfirmed".into(),
            },
        ])
        .map_err(|error| error.message)?;
        Ok(())
    }

    #[test]
    fn writer_requires_space_on_every_output_volume() -> Result<(), String> {
        let settings = provenant_config::GcSettings {
            free_floor: Some("100B".into()),
            ..Default::default()
        };
        let state = Path::new("/state");
        let worktree = Path::new("/worktree");
        let home = Path::new("/home");
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        for low in [worktree, state, cache.as_path()] {
            let mut observed = Vec::new();
            let result = check_writer_disk_with(&settings, state, worktree, home, |path| {
                observed.push(path.to_path_buf());
                Ok(if path == low { 99 } else { 1000 })
            });
            let error = result
                .err()
                .ok_or_else(|| format!("writer accepted low volume {}", low.display()))?;
            assert!(error.contains("insufficient disk space"), "{error}");
            assert!(observed.contains(&low.to_path_buf()));
        }
        check_writer_disk_with(&settings, state, worktree, home, |_| Ok(100))?;
        Ok(())
    }

    #[test]
    fn writer_rechecks_once_after_collection_and_reports_reclaimed_bytes() -> Result<(), String> {
        use std::cell::Cell;
        let settings = provenant_config::GcSettings {
            free_floor: Some("100B".into()),
            ..Default::default()
        };
        for remaining in [99, 100] {
            let collected = Cell::new(false);
            let samples = Cell::new(0);
            let result = check_writer_disk_with_collection(
                &settings,
                Path::new("/state"),
                Path::new("/tree"),
                Path::new("/home"),
                |_| {
                    samples.set(samples.get() + 1);
                    Ok(if collected.get() { remaining } else { 50 })
                },
                || {
                    assert!(!collected.replace(true));
                    49
                },
            );
            assert_eq!(samples.get(), 6);
            if remaining == 100 {
                result?;
            } else {
                let error = result.err().ok_or("writer accepted low disk")?;
                assert!(error.contains("reported 49 bytes reclaimed"), "{error}");
            }
        }
        let calls = Cell::new(0);
        check_writer_disk_with_collection(
            &settings,
            Path::new("/state"),
            Path::new("/tree"),
            Path::new("/home"),
            |_| Ok(100),
            || {
                calls.set(calls.get() + 1);
                0
            },
        )?;
        assert_eq!(calls.get(), 0);
        let disabled = provenant_config::GcSettings {
            schedule: false,
            ..settings
        };
        assert!(
            check_writer_disk_with_collection(
                &disabled,
                Path::new("/state"),
                Path::new("/tree"),
                Path::new("/home"),
                |_| Ok(1),
                || {
                    calls.set(calls.get() + 1);
                    99
                }
            )
            .is_err()
        );
        assert_eq!(calls.get(), 0);

        Ok(())
    }

    #[test]
    fn writer_samples_missing_roots_without_creating_them() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let settings = provenant_config::GcSettings {
            free_floor: Some("1B".into()),
            ..Default::default()
        };
        let state = base.join("missing/state");
        let home = base.join("missing/home");
        check_writer_disk(&settings, &state, &base, &base, &home)?;
        assert!(!base.join("missing").exists());
        Ok(())
    }
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
        assert_eq!(failure.kind.code(), "LANE_DEPTH");
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
            assert_eq!(failure.kind.code(), "LANE_CONTEXT");
        }
        for (depth, parent_attempt) in [
            (Some("1"), None),
            (None, Some(parent.as_str())),
            (Some("0"), Some(parent.as_str())),
        ] {
            let failure = lane_dispatch_values(depth, parent_attempt, 2, Path::new("/state"), None)
                .expect_err("incomplete or zero nested context must be refused");
            assert_eq!(failure.status, Status::Refused);
            assert_eq!(failure.kind.code(), "LANE_CONTEXT");
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
    #[test]
    fn model_policy_results_check_sections_and_bound_continuations()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = trace_store(&temp, &call)?;
        let input = trace_run(40, AttemptId::from_bytes([41; 16]), clock.wall_now()?)?;
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        let task = &input.request.tasks[0];
        let policies = provenant_routing::ModelPolicies::default();
        let mut decoded = run::Decoded {
            outcome: run::Outcome::Succeeded,
            result: "unstructured answer".into(),
            complete: true,
            observed_route: None,
            usage: Default::default(),
            session: Some("fixture".into()),
            reason: None,
            partial_signals: Vec::new(),
        };
        let gemini = policies
            .effective(&ModelId::parse("agy/gemini-3.8-flash")?, None, 0)
            .policy;
        let decision =
            assess_model_result(&gemini, task, &record, &decoded, record.attempt_number)?;
        assert!(decision.refusal.is_some());
        let fallback = decision.retry.ok_or("missing fallback")?;
        assert!(!fallback.resume);
        assert_eq!(fallback.max_attempts, 2);
        record.continuation = Some("policy_fallback".into());
        assert!(
            assess_model_result(&gemini, task, &record, &decoded, record.attempt_number)?
                .retry
                .is_none()
        );
        record.continuation = None;
        decoded.result = "## Findings\nClean.\n## Evidence\nChecked.".into();
        let decision =
            assess_model_result(&gemini, task, &record, &decoded, record.attempt_number)?;
        assert!(decision.refusal.is_none() && decision.retry.is_none());
        // Typed quota and stop outcomes retain their own recovery semantics,
        // even when their result is empty.
        decoded.result.clear();
        for outcome in [
            run::Outcome::QuotaLimited,
            run::Outcome::Cancelled,
            run::Outcome::TimedOut,
            run::Outcome::Lost,
        ] {
            decoded.outcome = outcome;
            let decision = assess_model_result(&gemini, task, &record, &decoded, 1)?;
            assert!(decision.refusal.is_none() && decision.retry.is_none());
        }
        decoded.outcome = run::Outcome::Succeeded;
        let terra = policies
            .effective(&ModelId::parse("codex/gpt-6.1-terra")?, None, 0)
            .policy;
        record.applied_route = Some(run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-terra".into()),
            effort: Some("high".into()),
            ..Default::default()
        });
        decoded.result = "More work remains.\nStatus: incomplete".into();
        let continuation =
            assess_model_result(&terra, task, &record, &decoded, record.attempt_number)?
                .retry
                .ok_or("missing continuation")?;
        assert!(continuation.resume);
        assert_eq!(continuation.max_attempts, 3);
        // Trailing blank lines do not hide the marker.
        decoded.result = "More work remains.\nStatus: incomplete\n\n".into();
        assert!(
            assess_model_result(&terra, task, &record, &decoded, record.attempt_number)?
                .retry
                .is_some()
        );
        decoded.result = "More work remains.\nStatus: incomplete".into();
        decoded.outcome = run::Outcome::Uncommitted;
        assert!(
            assess_model_result(&terra, task, &record, &decoded, 1)?
                .retry
                .is_some()
        );
        decoded.outcome = run::Outcome::Succeeded;
        record.attempt_number = 3;
        let decision = assess_model_result(&terra, task, &record, &decoded, record.attempt_number)?;
        assert!(decision.refusal.is_some() && decision.retry.is_none());
        decoded.result = "Done.\nStatus: complete".into();
        assert!(
            assess_model_result(&terra, task, &record, &decoded, record.attempt_number)?
                .retry
                .is_none()
        );
        Ok(())
    }
    #[test]
    fn an_alternate_account_is_spent_only_by_routing_rule_or_a_live_refusal()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_quota::{
            AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus, QuotaWindow,
        };
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().canonicalize()?.join("state");
        std::fs::create_dir_all(&state)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        let model = ModelId::parse("codex/gpt-6-luna")?;
        let now = 1_800_000_000;
        let scope = |key: &str| AccountScope {
            adapter: "codex".into(),
            account_key: key.into(),
            label: key.into(),
        };
        let exhausted = |key: &str, resets: i64| {
            let mut record = AccountRecord::empty(scope(key));
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account: scope(key),
                revision: 0,
                status: QuotaStatus::Exhausted,
                observed_unix_seconds: now - 7_200,
                source_ref: "fixture".into(),
                authority_ref: None,
                windows: vec![QuotaWindow {
                    pool: "seven_day".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 10_000,
                    resets_unix_seconds: Some(resets),
                }],
                details: None,
            });
            record
        };
        let keys = ["work", "other"];
        let alternative =
            |quota: &[AccountRecord]| unspent_alternative(keys, quota, "work", &model, None, now);
        // Unknown is not spent.
        assert!(alternative(&[]));
        // An exhausted reading binds until its full windows reset, as routing has it.
        assert!(!alternative(&[exhausted("other", now + 3_600)]));
        assert!(alternative(&[exhausted("other", now - 60)]));
        // A live refusal recorded by a settlement, read back from the cache.
        crate::quota::persist_provider_refusal(
            &state,
            &run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-6-luna".into()),
                account_key: Some("other".into()),
                account_label: Some("other".into()),
                ..Default::default()
            },
            None,
            now - 60,
            None,
        )?;
        let cache = crate::quota::read_cache_at(&state)?;
        assert!(!alternative(&cache));
        // It holds only that model, and only for its hour.
        let other = ModelId::parse("codex/gpt-6.1-sol")?;
        assert!(unspent_alternative(keys, &cache, "work", &other, None, now));
        assert!(unspent_alternative(
            keys,
            &cache,
            "work",
            &model,
            None,
            now + 3_600
        ));
        // The spent account itself never counts.
        assert!(!unspent_alternative(
            ["work"],
            &[],
            "work",
            &model,
            None,
            now
        ));
        Ok(())
    }
    #[test]
    fn capacity_and_quota_settlements_retry_within_their_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|error| error.message)?;
        let store = trace_store(&temp, &call)?;
        let input = trace_run(42, AttemptId::from_bytes([43; 16]), clock.wall_now()?)?;
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        record.applied_route = Some(run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            effort: Some("low".into()),
            account_key: Some("work".into()),
            ..Default::default()
        });
        let mut task = input.request.tasks[0].clone();
        task.route = run::RequestedRoute::Named {
            extra: Default::default(),
            name: "implement".into(),
            effort: None,
        };
        let config = provenant_routing::RoutingConfig::default();
        let decoded = |outcome, session: Option<&str>, reason: &str| run::Decoded {
            outcome,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: Default::default(),
            session: session.map(str::to_owned),
            reason: Some(reason.into()),
            partial_signals: Vec::new(),
        };
        let capacity = decoded(
            run::Outcome::ModelUnavailable,
            Some("thread"),
            "provider capacity: Selected model is at capacity",
        );
        const NOW: i64 = 1_800_000_000;
        let assess_at = |record: &run::AttemptRecord,
                         task: &run::TaskRequest,
                         decoded: &run::Decoded,
                         attempt: u64,
                         others: bool| {
            assess_settlement_retry(&config, |_, _| others, task, record, decoded, attempt, NOW)
        };
        let assess =
            |task: &run::TaskRequest, decoded: &run::Decoded, attempt: u64, others: bool| {
                assess_at(&record, task, decoded, attempt, others)
            };
        let wait = |retry: &run::PolicyRetry| retry.not_before_unix_seconds.map(|t| t - NOW);
        // Capacity with a session resumes it on the applied model, waiting
        // longer on each retry, with up to a quarter of jitter.
        let first = assess(&task, &capacity, 1, false);
        let retry = first.retry.ok_or("capacity retry")?;
        assert!(retry.resume && first.refusal.is_none());
        assert_eq!(retry.max_attempts, 3);
        let waited = wait(&retry).ok_or("not before")?;
        assert!((30..=37).contains(&waited), "{waited}");
        assert!(
            matches!(&retry.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
        );
        assert_eq!(
            first.note.as_deref(),
            Some(format!("retrying this session in {waited} s (attempt 2 of 3)").as_str())
        );
        let second = wait(&assess(&task, &capacity, 2, false).retry.ok_or("second")?)
            .ok_or("second wait")?;
        assert!((60..=75).contains(&second), "{second}");
        // The jitter differs between lanes, so they do not retry in step.
        let draws: std::collections::BTreeSet<_> = (0..8u8)
            .map(|byte| {
                let mut other = record.clone();
                other.attempt_id = AttemptId::from_bytes([byte.wrapping_mul(37); 16]);
                assess_at(&other, &task, &capacity, 1, false)
                    .retry
                    .and_then(|retry| wait(&retry))
            })
            .collect();
        assert!(draws.len() > 1, "{draws:?}");
        assert!(assess(&task, &capacity, 3, false).retry.is_none());
        // Without a session a pool route is drawn again, fresh.
        let sessionless = decoded(
            run::Outcome::ModelUnavailable,
            None,
            "provider capacity: Selected model is at capacity",
        );
        let redraw = assess(&task, &sessionless, 1, false)
            .retry
            .ok_or("redraw")?;
        assert!(!redraw.resume);
        assert_eq!(redraw.route, task.route);
        // A read-only lane takes its first fallback; a writer keeps its route.
        let mut fallback = task.clone();
        fallback.fallback = vec![run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/claude-sonnet-5-5".into()),
            ..Default::default()
        }];
        let read_only = assess(&fallback, &sessionless, 1, false)
            .retry
            .ok_or("fallback")?;
        assert!(
            matches!(&read_only.route, run::RequestedRoute::Model { model, .. } if model == "claude/claude-sonnet-5-5")
        );
        fallback.access.write_roots.push(temp.path().to_owned());
        let writer = assess(&fallback, &sessionless, 1, false)
            .retry
            .ok_or("writer")?;
        assert_eq!(writer.route, fallback.route);
        assert!(writer.instruction.contains("git status"));
        assert!(writer.instruction.contains("already pushed"));
        // Other model failures are not capacity.
        let unavailable = decoded(
            run::Outcome::ModelUnavailable,
            Some("thread"),
            "provider model unavailable: no route",
        );
        assert!(assess(&task, &unavailable, 1, true).retry.is_none());
        // A spent account redraws at once; an exact model needs another
        // registered account, and a pinned account is never swapped.
        let quota = decoded(
            run::Outcome::QuotaLimited,
            None,
            "You've hit your weekly limit",
        );
        // A named route on its only account, or every other account spent,
        // has nowhere to go: a redraw could reach the spent account again
        // through another model.
        assert!(assess(&task, &quota, 1, false).retry.is_none());
        let spent = assess(&task, &quota, 1, true);
        let retry = spent.retry.ok_or("quota retry")?;
        assert!(!retry.resume && retry.not_before_unix_seconds.is_none());
        // It keeps the applied model, so admission refuses the spent account
        // and resolution falls through to another for that model.
        assert!(
            matches!(&retry.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
        );
        assert_eq!(
            spent.note.as_deref(),
            Some("retrying on the next eligible account (attempt 2 of 3)")
        );
        let mut pinned = task.clone();
        pinned.account = Some("work".into());
        assert!(assess(&pinned, &quota, 1, true).retry.is_none());
        let mut exact = task.clone();
        exact.route = input.request.tasks[0].route.clone();
        assert!(assess(&exact, &quota, 1, false).retry.is_none());
        assert!(
            matches!(&assess(&exact, &quota, 1, true).retry.ok_or("exact")?.route, run::RequestedRoute::Model { model, .. } if model == "codex/gpt-6-luna")
        );
        exact.account = Some("work".into());
        assert!(assess(&exact, &quota, 1, true).retry.is_none());
        // Zero retries turns the behaviour off.
        let off = provenant_routing::RoutingConfig {
            settlement_retries: 0,
            ..Default::default()
        };
        assert!(
            assess_settlement_retry(&off, |_, _| true, &task, &record, &quota, 1, NOW)
                .retry
                .is_none()
        );
        // The limit, base, cap and ceiling are all configuration.
        let tuned = provenant_routing::RoutingConfig {
            settlement_retries: 5,
            capacity_retry_seconds: 400,
            capacity_retry_max_seconds: 900,
            retry_delay_ceiling_seconds: 7_200,
            ..Default::default()
        };
        let capped =
            assess_settlement_retry(&tuned, |_, _| false, &task, &record, &capacity, 4, NOW)
                .retry
                .ok_or("tuned retry")?;
        assert_eq!(capped.max_attempts, 6);
        assert_eq!(wait(&capped), Some(900));
        let ceiling = provenant_routing::RoutingConfig {
            retry_delay_ceiling_seconds: 45,
            ..tuned
        };
        let ceiled =
            assess_settlement_retry(&ceiling, |_, _| false, &task, &record, &capacity, 1, NOW)
                .retry
                .ok_or("ceiled retry")?;
        assert_eq!(wait(&ceiled), Some(45));
        // Every configurable retry limit fits the run store's chain bound.
        assert!(u64::from(provenant_routing::MAX_SETTLEMENT_RETRIES) < run::POLICY_ATTEMPT_CEILING);
        Ok(())
    }
    #[test]
    fn unsafe_store_path_reason_survives_cli_conversion() {
        let failure = super::Fail::from(provenant_run::RunError::UnsafePath {
            path: "runs/example/process.json".into(),
            check: provenant_run::PathCheck::Links,
        });
        assert_eq!(
            failure.message,
            "run-store path runs/example/process.json is not private or contained: link count"
        );
    }

    #[test]
    fn stats_summary_reports_cost_coverage() {
        use provenant_telemetry::{CostTotal, Money, Totals};
        let mut totals = Totals {
            attempts: 3,
            usage: Default::default(),
            cache: Default::default(),
            cost: CostTotal {
                cost_basis: "api_equivalent_estimate",
                subscription_attempts: 0,
                complete: None,
                known_lower_bound: None,
                priced_attempts: 0,
                missing_attempts: 3,
            },
            outcomes: Default::default(),
        };
        assert_eq!(totals_cache_read_share(&totals), None);
        totals.usage.uncached_input.known = 10;
        totals.usage.cache_read.known = 90;
        assert_eq!(totals_cache_read_share(&totals), Some(0.9));
        totals.usage.cache_write.missing = 1;
        assert_eq!(totals_cache_read_share(&totals), None);
        assert_eq!(
            cost_summary(&totals),
            "3 attempts, cost unknown (3 attempts missing cost data)\n"
        );
        totals.cost.known_lower_bound = Some(Money {
            microdollars: 2_000_000,
            usd: "2.000000".into(),
        });
        totals.cost.priced_attempts = 1;
        totals.cost.missing_attempts = 2;
        assert_eq!(
            cost_summary(&totals),
            "3 attempts, est. API-equivalent at least $2.000000 (1 of 3 attempts priced)\n"
        );
        totals.cost.complete = totals.cost.known_lower_bound.clone();
        assert_eq!(
            cost_summary(&totals),
            "3 attempts, est. API-equivalent $2.000000\n"
        );
    }

    #[test]
    fn stats_text_lists_cache_share_only_where_a_provider_reported_it() {
        use provenant_telemetry::{ChairModelUsage, ConfiguredPrices, usage::ChairSession};
        let prices = provenant_telemetry::validate_prices(&ConfiguredPrices {
            schema_version: 1,
            id: "empty".into(),
            reviewed_at: "2026-10-29".into(), // timebomb: allow fixed price-table review date, never compared with the clock
            units: "usd_microdollars_per_million_tokens".into(),
            models: Vec::new(),
        })
        .unwrap();
        let chair = |usage| ChairSession {
            adapter: "claude".into(),
            account: "work".into(),
            session: "s".into(),
            project: None,
            config: None,
            usage,
            unread: None,
        };
        let usage = |read| {
            vec![ChairModelUsage {
                model: "claude-opus-5-5".into(),
                usage: provenant_records::Usage {
                    uncached_input: Some(10),
                    cache_read: read,
                    cache_write: Some(0),
                    cache_write_1h: None,
                    output_excluding_reasoning: Some(1),
                    reasoning: None,
                },
            }]
        };
        let report = |usage| {
            let report = provenant_telemetry::stats_with_chair(
                [],
                [chair(usage)],
                &prices,
                Default::default(),
            )
            .unwrap();
            stats_summary(&report)
        };
        let text = report(usage(Some(90)));
        assert!(
            text.contains(
                "cache read by source [chair]: 90.0% of 100 input tokens (1 records reported, 0 did not)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("cache read by model [chair] claude claude/claude-opus-5-5: 90.0%"),
            "{text}"
        );
        assert!(
            text.contains("chair: 1 sessions on this host (0 unread)"),
            "{text}"
        );
        let text = report(usage(None));
        assert!(!text.contains("cache read"), "{text}");
    }

    #[test]
    fn core_run_status_uses_policy_successor_and_retains_receipts()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let edge = Edge {
            roots: &roots,
            home: &root.join("home"),
            assets: root,
            cwd: root,
        };
        for (tail, expected) in [("succeeded", Status::Ok), ("failed", Status::Failed)] {
            let result = super::reply(
                "dispatch",
                Ok((
                    json!({
                        "lanes": [
                            {"attempt_id":"01010101010101010101010101010101", "state":"failed", "superseded_by":"02020202020202020202020202020202"},
                            {"attempt_id":"02020202020202020202020202020202", "state":tail}
                        ]
                    }),
                    String::new(),
                )),
            );
            let report = super::render_run(result, &edge);
            assert_eq!(report.result.status, expected);
            assert_eq!(
                report.result.data["lanes"].as_array().ok_or("lanes")?.len(),
                2
            );
            assert!(report.human.contains("01010101 failed"));
            assert!(report.human.contains(&format!("02020202 {tail}")));
        }
        Ok(())
    }

    #[test]
    fn core_run_timeout_only_offers_waiting_for_unsettled_lanes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let edge = Edge {
            roots: &roots,
            home: &root.join("home"),
            assets: root,
            cwd: root,
        };
        let result = super::reply_waited(
            "dispatch",
            Ok((
                serde_json::json!({
                    "operation_id": "timeout",
                    "lanes": [
                        {"attempt_id":"01010101010101010101010101010101", "state":"running", "applied_route":{"model":"codex/test", "effort":"high"}, "network":"open"},
                        {"attempt_id":"02020202020202020202020202020202", "state":"succeeded"},
                        {"attempt_id":"03030303030303030303030303030303", "state":"input_required"}
                    ]
                }),
                String::new(),
                true,
            )),
        );
        let report = super::render_run(result, &edge);
        assert!(
            report
                .human
                .contains("codex/test (effort: high, network: open)")
        );
        assert_eq!(report.result.status, Status::Timeout);
        let next = |reply: &super::Reply| {
            reply
                .summary
                .as_ref()
                .and_then(|summary| summary.next.clone())
        };
        assert!(!report.human.contains("keep waiting:"), "{}", report.human);
        assert_eq!(
            next(&report).as_deref(),
            Some("provenant lanes --id 01010101010101010101010101010101 --wait 4m")
        );
        let input = super::reply(
            "dispatch",
            Ok((
                serde_json::json!({
                    "operation_id": "answer",
                    "lanes": [{"attempt_id":"03030303030303030303030303030303", "state":"input_required"}]
                }),
                String::new(),
            )),
        );
        let input = super::render_run(input, &edge);
        assert_eq!(input.result.status, Status::Ok);
        assert_eq!(
            next(&input).as_deref(),
            Some("provenant resume 03030303030303030303030303030303 --prompt-file ANSWER")
        );
        assert!(!input.human.contains("keep waiting:"));
        Ok(())
    }

    #[test]
    fn displaced_fallback_reports_unavailable_history() {
        let history = super::fallback_history(Path::new(env!("CARGO_MANIFEST_DIR")), "HEAD", true);
        assert_eq!(
            history,
            "[history unavailable: prior worktree is missing or reassigned]"
        );
    }

    #[test]
    fn dispatch_reuses_only_free_lane_or_unowned_worktree() -> Result<(), Box<dyn std::error::Error>>
    {
        use provenant_confinement::worktrees::{
            self, WorkerObserver, WorkerState, WorktreeCreate, WorktreeOrigin, WorktreeOwner,
        };
        use provenant_core::RootInputs;
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::ffi::{OsStr, OsString};
        struct Free;
        impl WorkerObserver for Free {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, WorkerState> {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Free))
                    .collect()
            }
        }
        let executor = SystemExecutor::new(1)?;
        if crate::fabric::in_lane() {
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::tests::dispatch_reuses_only_free_lane_or_unowned_worktree".into(),
            ];
            let removed = [
                OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
                OsStr::new(run::LANE_DEPTH_ENV),
                OsStr::new(run::PARENT_ATTEMPT_ENV),
            ];
            let env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| !removed.contains(&key.as_os_str()))
                .collect();
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            let out = executor.execute(options)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&project)?;
        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        ] {
            let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.into_iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(10));
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            let out = executor.execute(options)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        }
        let edge = Edge {
            roots: &roots,
            home: &home,
            assets: &project,
            cwd: &project,
        };
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        // Opening dispatch's real session initialises the state root first.
        let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
        let _session = open(&edge, &call).map_err(|fail| fail.message)?;
        let host = provenant_config::bind_host(&roots)?;
        let prompt = root.join("protected-prompt.md");
        std::fs::write(&prompt, "reply OK")?;
        let existing =
            worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                worktrees::create(
                    &worktrees::WorktreeContext {
                        workers: &Free,
                        ..*context
                    },
                    WorktreeCreate {
                        branch: "protected-existing".into(),
                        base: Some("HEAD".into()),
                    },
                    &cancel,
                )
            })?;
        let primary = {
            let argv = vec![
                git.as_os_str().to_owned(),
                "symbolic-ref".into(),
                "--short".into(),
                "HEAD".into(),
            ];
            let output =
                executor.execute(ExecOptions::new(&argv, &project, Duration::from_secs(5)))?;
            assert_eq!(output.termination, Termination::Exit(0));
            String::from_utf8(output.stdout.bytes)?.trim().to_owned()
        };
        for (index, (target, placement, delivery)) in ["main", "master", primary.as_str()]
            .into_iter()
            .flat_map(|target| {
                [
                    (target, "owned", "integrate"),
                    (target, "existing", "integrate"),
                    (target, "manifest", "integrate"),
                    (target, "owned", "legacy-push"),
                    (target, "existing", "legacy-push"),
                    (target, "manifest", "push"),
                    (target, "owned", "push"),
                ]
            })
            .enumerate()
        {
            let operation = format!("protected-integration-{index}");
            let mut args: Vec<OsString> =
                ["dispatch", "--operation-id", &operation, "--base", "HEAD"]
                    .map(OsString::from)
                    .into();
            let mode = if delivery == "integrate" {
                run::DeliveryMode::Integrate(target.into())
            } else {
                run::DeliveryMode::Push
            };
            if placement != "manifest" {
                args.extend(
                    ["--model", "codex/gpt-6-luna", "--mode", "worktree-write"].map(OsString::from),
                );
            }
            if placement == "owned" {
                args.extend(
                    [
                        "--worktree-branch",
                        if delivery == "push" {
                            target
                        } else {
                            "refused-lane"
                        },
                    ]
                    .map(OsString::from),
                );
            } else if placement == "existing" {
                args.extend([
                    OsString::from("--worktree"),
                    existing.path.as_os_str().to_owned(),
                ]);
            }
            if placement == "manifest" {
                let manifest = root.join("protected-manifest.json");
                std::fs::write(
                    &manifest,
                    serde_json::to_vec(&json!({"schema_version":1,"tasks":[{
                        "prompt":"reply OK", "route":run::RequestedRoute::Model {extra: Default::default(),  provider:"codex".into(), model:"codex/gpt-6-luna".into(), effort:None },
                        "deliver":run::HostDeliveryContext::destination(mode, target)?
                    }]}))?,
                )?;
                args.extend([OsString::from("--manifest"), manifest.into_os_string()]);
            } else {
                if delivery == "legacy-push" {
                    args.extend(
                        ["--deliver-remote", "origin", "--deliver-branch", target]
                            .map(OsString::from),
                    );
                } else {
                    args.extend([
                        OsString::from("--deliver"),
                        if delivery == "integrate" {
                            format!("integrate:{target}")
                        } else {
                            "push".into()
                        }
                        .into(),
                    ]);
                }
                args.extend([
                    OsString::from("--prompt-file"),
                    prompt.as_os_str().to_owned(),
                ]);
            }
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
                return Err("parse".into());
            };
            let crate::Command::Dispatch(command) = invocation.command else {
                return Err("dispatch".into());
            };
            let reply = dispatch_with_workers(*command, &edge, &Free);
            assert!(
                _session
                    .store
                    .operation(&operation.parse()?, &call)?
                    .is_none(),
                "protected delivery was admitted"
            );
            assert_eq!(reply.result.status, Status::Refused);
            assert!(
                reply
                    .result
                    .error
                    .ok_or("refusal")?
                    .message
                    .contains("default and primary checkout branches")
            );
            let branch = if delivery == "push" && placement == "owned" {
                target
            } else {
                "refused-lane"
            };
            assert!(!project.join(".worktrees").join(branch).exists());
        }
        let pr =
            run::HostDeliveryContext::destination(run::DeliveryMode::Pr, "protected-existing")?
                .ok_or("PR destination")?;
        check_delivery_branch(&project, &pr.branch).map_err(|error| error.message)?;
        assert!(pr.open_pr);
        for origin in [
            None,
            Some(WorktreeOrigin::Lane),
            Some(WorktreeOrigin::Adopted),
        ] {
            let branch = match origin {
                None => "unowned",
                Some(WorktreeOrigin::Lane) => "lane",
                Some(WorktreeOrigin::Adopted) => "adopted",
            };
            let (path, base) =
                worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                    let context = &worktrees::WorktreeContext {
                        workers: &Free,
                        ..*context
                    };
                    let info = worktrees::create(
                        context,
                        WorktreeCreate {
                            branch: branch.into(),
                            base: Some("HEAD".into()),
                        },
                        &cancel,
                    )?;
                    let tree = provenant_confinement::verify_worktree(context.repo, &info.path)?;
                    if let Some(origin) = origin {
                        worktrees::save_owner(
                            &tree,
                            &WorktreeOwner {
                                attempt_id: None,
                                operation_id: Some(
                                    OperationId::parse("previous-lane")
                                        .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                                ),
                                branch: branch.into(),
                                base: info.head.to_string(),
                                host,
                                created_at: WallTime::from_micros(1)
                                    .map_err(|_| worktrees::WorktreeError::InvalidRequest)?,
                                origin,
                                label: Some("preserved-label".into()),
                            },
                        )?;
                    }
                    Ok((info.path, info.head.to_string()))
                })?;
            let missing = root.join("missing-prompt");
            let args: Vec<OsString> = [
                "dispatch",
                "--operation-id",
                "replacement-lane",
                "--model",
                "codex/gpt-6-luna",
                "--mode",
                "worktree-write",
                "--worktree-branch",
                branch,
                "--base",
                "HEAD",
                "--prompt-file",
                missing.to_str().ok_or("path")?,
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
                return Err("parse".into());
            };
            let crate::Command::Dispatch(command) = invocation.command else {
                return Err("dispatch".into());
            };
            let reply = dispatch_with_workers(*command, &edge, &Free);
            assert_ne!(reply.result.status, Status::Ok);
            let load = || {
                worktrees::with_context(&roots, &home, &project, &clock, &cancel, |context| {
                    worktrees::load_owner(&provenant_confinement::verify_worktree(
                        context.repo,
                        &path,
                    )?)
                })
            };
            // The unreadable prompt fails preflight, before any ownership change.
            let previous = OperationId::parse("previous-lane")?;
            assert_eq!(
                load()?.map(|owner| owner.operation_id),
                origin.map(|_| Some(previous.clone()))
            );
            let claimed = super::own_worktree(
                &super::OwnedWorktree {
                    branch: branch.into(),
                    host,
                    created_at: WallTime::from_micros(2)?,
                },
                Some("HEAD"),
                &OperationId::parse("replacement-lane")?,
                &edge,
                &Free,
                &mut None,
                &clock,
                &cancel,
            );
            let owner = load()?.ok_or("owner")?;
            assert_eq!(owner.base, base);
            if origin == Some(WorktreeOrigin::Adopted) {
                assert_eq!(owner.origin, WorktreeOrigin::Adopted);
                assert_eq!(owner.operation_id, Some(previous));
                assert_eq!(owner.label.as_deref(), Some("preserved-label"));
                assert!(
                    claimed
                        .err()
                        .ok_or("adopted owner was claimed")?
                        .message
                        .contains("adopted or foreign owner")
                );
            } else {
                assert_eq!(claimed.map_err(|fail| fail.message)?.0.path, path);
                assert_eq!(owner.origin, WorktreeOrigin::Lane);
                assert_eq!(
                    owner.operation_id,
                    Some(OperationId::parse("replacement-lane")?)
                );
            }
        }
        Ok(())
    }

    #[test]
    fn failed_preflight_creates_no_worktree() -> Result<(), Box<dyn std::error::Error>> {
        use provenant_confinement::worktrees::{WorkerObserver, WorkerState};
        use provenant_core::RootInputs;
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::ffi::{OsStr, OsString};
        // A census that cannot clear any tree: cleanup after a late failure
        // would retain whatever creation left behind.
        struct Unknown;
        impl WorkerObserver for Unknown {
            fn observe_roots(
                &self,
                roots: &[PathBuf],
            ) -> std::collections::BTreeMap<PathBuf, WorkerState> {
                roots
                    .iter()
                    .cloned()
                    .map(|root| (root, WorkerState::Unknown))
                    .collect()
            }
        }
        let executor = SystemExecutor::new(1)?;
        if crate::fabric::in_lane() {
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::tests::failed_preflight_creates_no_worktree".into(),
            ];
            let removed = [
                OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
                OsStr::new(run::LANE_DEPTH_ENV),
                OsStr::new(run::PARENT_ATTEMPT_ENV),
            ];
            let env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| !removed.contains(&key.as_os_str()))
                .collect();
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            let out = executor.execute(options)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&project)?;
        let mut inputs = RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let run_git = |args: &[&str]| -> Result<_, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(args.iter().map(OsString::from))
                .collect();
            let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(10));
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            Ok(executor.execute(options)?)
        };
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ][..],
        ] {
            let out = run_git(args)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        }
        let edge = Edge {
            roots: &roots,
            home: &home,
            assets: &project,
            cwd: &project,
        };
        let prompt = root.join("prompt");
        let secret = root.join("secret-prompt");
        std::fs::write(&prompt, "review changes")?;
        std::fs::write(&secret, "sk-abcdefghijklmnopqrstuvwxyz123456")?;
        let missing = root.join("missing-prompt");
        for (input, extra, invalid) in [
            (&missing, vec![], None),
            (&prompt, vec!["--knob", "not.a.knob=1"], None),
            (&prompt, vec![], Some("title")),
            (&prompt, vec![], Some("fallback")),
            (&secret, vec![], None),
        ] {
            let mut args: Vec<OsString> = [
                "dispatch",
                "--operation-id",
                "preflight-lane",
                "--model",
                "codex/gpt-6-luna",
                "--mode",
                "worktree-write",
                "--worktree-branch",
                "preflight-lane",
                "--base",
                "HEAD",
                "--prompt-file",
                input.to_str().ok_or("path")?,
            ]
            .into_iter()
            .map(OsString::from)
            .collect();
            args.extend(extra.into_iter().map(OsString::from));
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(args) else {
                return Err("parse".into());
            };
            let crate::Command::Dispatch(mut command) = invocation.command else {
                return Err("dispatch".into());
            };
            match invalid {
                Some("title") => command.title = Some("invalid\ntitle".into()),
                Some("fallback") => {
                    command.route.fallback = vec![crate::route::Model("invalid".into())]
                }
                _ => {}
            }
            let reply = dispatch_with_workers(*command, &edge, &Unknown);
            assert_ne!(reply.result.status, Status::Ok);
            assert!(
                !project.join(".worktrees").join("preflight-lane").exists(),
                "{:?}",
                reply.result
            );
            let branch = run_git(&[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/heads/preflight-lane",
            ])?;
            assert_ne!(branch.termination, Termination::Exit(0), "{branch:?}");
        }
        // A relative base is resolved in the source checkout exactly once.
        // The new tree has no HEAD^, so admission must use creation's receipt.
        let out = run_git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "second",
        ])?;
        assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
        let session = open(&edge, &call).map_err(|fail| fail.message)?;
        let operation = OperationId::parse("relative-base-lane")?;
        let (info, pinned) = own_worktree(
            &OwnedWorktree {
                branch: "relative-base-lane".into(),
                host: provenant_config::bind_host(&roots)?,
                created_at: clock.wall_now()?,
            },
            Some("HEAD^"),
            &operation,
            &edge,
            &Unknown,
            &mut None,
            &clock,
            &cancel,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(info.head, pinned);
        let dispatch = |base: &str| run::DispatchRequest {
            chair: None,
            operation_id: operation.clone(),
            concurrency: 1,
            lane_depth: 0,
            parent_attempt: None,
            tasks: vec![run::DispatchTask {
                context: None,
                build_heavy: false,
                title: None,
                task_id: TaskId::from_bytes([3; 16]),
                prompt: "review changes".into(),
                base: Some(base.into()),
                pinned_base: Some(pinned),
                head: None,
                route: run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                profile: Default::default(),
                timeout_seconds: 10,
                delivery_id: None,
                deliver: None,
                worktree: Some(info.path.clone()),
                access: Default::default(),
            }],
        };
        let service = run::Service::new(&session.store);
        let record = service.dispatch(dispatch("HEAD^"), &project, &call)?;
        let canonical = session.store.request(record.run_id, &call)?;
        assert_eq!(
            canonical.tasks[0].base.as_deref(),
            Some(pinned.to_string().as_str())
        );
        assert_eq!(canonical.tasks[0].base_ref.as_deref(), Some("HEAD^"));
        assert_eq!(
            service.dispatch(dispatch("HEAD^"), &project, &call)?.run_id,
            record.run_id
        );
        let changed_base = service.dispatch(dispatch("HEAD"), &project, &call);
        assert!(
            matches!(changed_base, Err(run::RunError::OperationConflict)),
            "{changed_base:?}"
        );
        let mut with_head = dispatch("HEAD^");
        with_head.operation_id = OperationId::parse("relative-base-with-head")?;
        with_head.tasks[0].head = Some(pinned.to_string());
        service.dispatch(with_head, &project, &call)?;
        Ok(())
    }

    #[test]
    fn pruning_intent_replays_only_for_its_missing_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let tree = temp.path().join("owned");
        let receipt = serde_json::json!({"state": "pruning", "worktree": tree});
        assert!(super::worktree_was_pruned(&receipt, &tree));
        assert!(!super::worktree_was_pruned(
            &receipt,
            &temp.path().join("other")
        ));
        std::fs::create_dir(&tree)?;
        assert!(!super::worktree_was_pruned(&receipt, &tree));
        std::fs::remove_dir(&tree)?;
        std::os::unix::fs::symlink(temp.path().join("missing-target"), &tree)?;
        assert!(!super::worktree_was_pruned(&receipt, &tree));
        Ok(())
    }

    use super::*;
    use provenant_run::Providers;
    use provenant_supervisor::custody::CaptureTotals;
    use provenant_supervisor::custody::PublicationState;
    use provenant_supervisor::custody::Scope;
    use provenant_supervisor::custody::SettlementEvidence;

    #[test]
    fn manifest_accepts_task_git_base() {
        let manifest: Manifest = serde_json::from_value(json!({
            "schema_version": 1,
            "tasks": [
                {"prompt": "review", "title": "Review changes", "base": "main", "route": {"kind": "named", "name": "senior"}},
                {"prompt": "review", "route": {"kind": "model", "provider": "codex", "model": "gpt-6-luna"}}
            ]
        }))
        .unwrap();
        assert_eq!(manifest.tasks[0].base.as_deref(), Some("main"));
        assert_eq!(manifest.tasks[1].base, None);
        assert_eq!(manifest.tasks[0].title.as_deref(), Some("Review changes"));
        assert_eq!(manifest.tasks[1].title, None);
        assert!(
            matches!(&manifest.tasks[0].route, run::RequestedRoute::Named { name, .. } if name == "senior")
        );
        assert!(
            matches!(&manifest.tasks[1].route, run::RequestedRoute::Model { provider, model, .. } if provider == "codex" && model == "gpt-6-luna")
        );
    }

    #[test]
    fn manifest_rejects_flat_route_fields() {
        for task in [
            json!({"prompt": "review", "model": "codex/gpt-6-luna", "effort": "high"}),
            json!({"prompt": "review", "route": "senior"}),
            json!({"prompt": "review", "route": {"kind": "named", "name": "senior"}, "effort": "high"}),
            json!({"prompt": "review"}),
        ] {
            assert!(
                serde_json::from_value::<Manifest>(json!({"schema_version": 1, "tasks": [task]}))
                    .is_err()
            );
        }
    }

    #[test]
    fn operation_replays_after_its_context_ref_moves_or_is_deleted()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{ActorId, RootInputs};
        use provenant_os::exec::ExecOptions;
        use provenant_os::exec::Executor;
        use provenant_os::exec::StreamPolicy;
        use provenant_os::exec::SystemExecutor;
        use provenant_os::exec::Termination;

        use std::ffi::{OsStr, OsString};
        let executor = SystemExecutor::new(1)?;
        // The regression exercises a top-level submission, even when the
        // contributor running it is itself inside a nested lane.
        if std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some()
            || std::env::var_os(run::LANE_DEPTH_ENV).is_some()
            || std::env::var_os(run::PARENT_ATTEMPT_ENV).is_some()
        {
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::tests::operation_replays_after_its_context_ref_moves_or_is_deleted".into(),
                "--nocapture".into(),
            ];
            let removed = [
                OsStr::new(provenant_core::RUN_STATE_DIRECTORY_ENV),
                OsStr::new(run::LANE_DEPTH_ENV),
                OsStr::new(run::PARENT_ATTEMPT_ENV),
            ];
            let env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| !removed.contains(&key.as_os_str()))
                .collect();
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65536 };
            options.stderr = StreamPolicy::Capture { limit: 65536 };
            let out = executor.execute(options)?;
            assert!(
                out.termination == Termination::Exit(0) && !out.timed_out,
                "{out:?}"
            );
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let cwd = root.join("checkout");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&cwd)?;
        let mut inputs = RootInputs::new(home.clone(), cwd.clone());
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let command = |args: &[&str]| -> Result<String, Box<dyn std::error::Error>> {
            let argv: Vec<OsString> = std::iter::once(git.as_os_str().to_owned())
                .chain(
                    [
                        "-c",
                        "core.hooksPath=/dev/null",
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.test",
                    ]
                    .into_iter()
                    .chain(args.iter().copied())
                    .map(OsString::from),
                )
                .collect();
            let env = vec![
                (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
                (
                    OsString::from("GIT_CONFIG_GLOBAL"),
                    OsString::from("/dev/null"),
                ),
            ];
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(10));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65536 };
            options.stderr = StreamPolicy::Capture { limit: 65536 };
            let out = executor.execute(options)?;
            assert!(
                out.termination == Termination::Exit(0) && !out.timed_out,
                "{out:?}"
            );
            Ok(String::from_utf8(out.stdout.bytes)?.trim().to_owned())
        };
        command(&["init", "-q"])?;
        command(&["commit", "--allow-empty", "-qm", "initial"])?;
        command(&["branch", "context-base"])?;
        let original_base = command(&["rev-parse", "context-base"])?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let context = run::ExecutionContext {
            state_root: roots.state.clone(),
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        let session = Session {
            store: run::RunStore::open(&context, &call)?,
            context,
        };
        let edge = Edge {
            roots: &roots,
            home: &home,
            assets: &cwd,
            cwd: &cwd,
        };
        let chair = Some(run::ChairBinding {
            extra: Default::default(),
            host: provenant_core::HostId::from_bytes([4; 16]),
            actor: ActorId::from_bytes([5; 16]),
        });
        let submission = |base: &str| Submission {
            chair: chair.clone(),
            operation_id: OperationId::parse("context-ref-replay").unwrap(),
            tasks: vec![Submitted {
                build_heavy: false,
                title: None,
                task_id: TaskId::from_bytes([3; 16]),
                prompt: "review".into(),
                base: Some(base.into()),
                pinned_base: None,
                head: None,
                route: run::RequestedRoute::Model {
                    extra: Default::default(),
                    provider: "fixture".into(),
                    model: "fixture/model".into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                provider_timeout: 10,
                delivery_id: None,
                deliver: None,
                worktree: None,
                access: Default::default(),
            }],
            concurrency: Some(1),
            knobs: vec![],
        };
        let (first, _) =
            submit(&session, &edge, submission("context-base"), &call).map_err(|f| f.message)?;
        with_chair_index(&roots, &call, |index, index_call| {
            let binding = index
                .read(session.context.project_id, first.run_id, index_call)?
                .unwrap();
            assert_eq!(binding.chair, chair);
            assert_eq!(
                binding.request_digest,
                session
                    .store
                    .request(first.run_id, &call)
                    .unwrap()
                    .digest()
                    .unwrap()
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        std::fs::rename(
            roots.state.as_path().join("fabric/host.sqlite3"),
            roots.state.as_path().join("fabric/interrupted.sqlite3"),
        )?;
        let mut forged = session.store.request(first.run_id, &call)?;
        forged.concurrency = 2;
        with_chair_index(&roots, &call, |index, c| {
            index.submit(session.context.project_id, first.run_id, &forged, c)
        })
        .map_err(|f| f.message)?;
        assert_eq!(
            launch(&session, first.run_id, &edge, &call)
                .unwrap_err()
                .status,
            Status::Refused
        );
        std::fs::rename(
            roots.state.as_path().join("fabric/host.sqlite3"),
            roots.state.as_path().join("fabric/forged.sqlite3"),
        )?;
        // Reusing an operation from another chair cannot rebind even a missing
        // host index row. Repair must use the immutable original request.
        let mut replay = submission("context-base");
        replay.chair = Some(run::ChairBinding {
            extra: Default::default(),
            host: provenant_core::HostId::from_bytes([6; 16]),
            actor: ActorId::from_bytes([7; 16]),
        });
        let (replayed, _) = submit(&session, &edge, replay, &call).map_err(|f| f.message)?;
        assert_eq!(replayed.run_id, first.run_id);
        let expired = run::Request {
            budget: WaitBudget::nonblocking(clock.mono_now()),
            ..call
        };
        let failure = with_chair_index(&roots, &expired, |_, _| Ok(())).unwrap_err();
        assert_eq!(failure.status, Status::Error);
        with_chair_index(&roots, &call, |index, call| {
            assert_eq!(
                index
                    .read(session.context.project_id, first.run_id, call)?
                    .unwrap()
                    .chair,
                chair
            );
            Ok(())
        })
        .map_err(|f| f.message)?;
        command(&["commit", "--allow-empty", "-qm", "moved"])?;
        command(&["branch", "-f", "context-base", "HEAD"])?;
        for deleted in [false, true] {
            if deleted {
                command(&["branch", "-D", "context-base"])?;
            }
            let (replayed, _) = submit(&session, &edge, submission("context-base"), &call)
                .map_err(|f| f.message)?;
            assert_eq!(replayed.run_id, first.run_id);
            assert_eq!(
                session.store.request(first.run_id, &call)?.tasks[0]
                    .base
                    .as_deref(),
                Some(original_base.as_str())
            );
        }
        assert!(
            matches!(submit(&session, &edge, submission("HEAD"), &call), Err(fail) if fail.kind == ErrorKind::OperationConflict)
        );
        Ok(())
    }

    pub(super) fn trace_run(
        n: u8,
        parent_attempt: AttemptId,
        created_at: WallTime,
    ) -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
        let task_id = TaskId::from_bytes([n; 16]);
        Ok(run::PreparedRun {
            run_id: RunId::from_bytes([n + 1; 16]),
            operation_id: OperationId::parse(&format!("trace-{n}"))?,
            created_at,
            request: run::CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    context: None,
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    base_ref: None,
                    task_id,
                    prompt_digest: CanonicalV1::file_digest(b"p"),
                    route: run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "fixture".into(),
                        model: "fixture/model".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    profile: Default::default(),
                    authority_digest: provenant_core::Digest::from_bytes([5; 32]),
                    provider_timeout_seconds: 3600,
                    writer: None,
                    delivery_id: None,
                    deliver: None,
                    access: Default::default(),
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: Some(parent_attempt),
            },
            attempts: vec![run::NewAttempt {
                task_id,
                attempt_id: AttemptId::from_bytes([n + 2; 16]),
                prompt: "p".into(),
            }],
        })
    }

    fn trace_store(
        temp: &tempfile::TempDir,
        call: &run::Request<'_>,
    ) -> Result<run::RunStore, Box<dyn std::error::Error>> {
        let root = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(root.join("home"))?;
        std::fs::create_dir_all(root.join("checkout"))?;
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        Ok(run::RunStore::open(&context, call)?)
    }

    #[test]
    fn a_nested_lane_reads_the_host_ledger_when_it_opens_read_only()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let host = root.join("state");
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(host.join("children").join("lane"));
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        // The store must exist to be opened; the writable open creates it.
        run::RunStore::open(&context, &call)?;
        let store = read_only_store(&context, &host, &call)
            .map_err(|f| f.message)?
            .ok_or("nested store")?;
        assert_eq!(store.writer_registry(), host);
        assert_ne!(store.writer_registry(), context.state_root.as_path());
        Ok(())
    }

    #[test]
    fn trace_chain_follows_parents_and_ends_at_a_parent_this_store_lacks()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let store = trace_store(&temp, &call)?;
        let missing = AttemptId::from_bytes([9; 16]);
        let prepared =
            |n: u8, parent_attempt: AttemptId| trace_run(n, parent_attempt, clock.wall_now()?);
        let first = prepared(10, missing)?;
        let second = prepared(20, first.attempts[0].attempt_id)?;
        store.create(&first, &call)?;
        store.create(&second, &call)?;
        let leaf = store.record(second.attempts[0].attempt_id, &call)?;
        let chain = trace_chain(&store, leaf, &call).map_err(|f| f.message)?;
        let attempts: Vec<_> = chain.iter().map(|t| t.attempt).collect();
        assert_eq!(
            attempts,
            [
                Some(second.attempts[0].attempt_id),
                Some(first.attempts[0].attempt_id),
                Some(missing)
            ]
        );
        assert_eq!(chain[1].run, Some(first.run_id));
        assert_eq!(
            chain[2],
            provenant_records::Trace {
                attempt: Some(missing),
                ..Default::default()
            }
        );
        let line = trace_line(&chain[2]);
        assert_eq!(line.lines().count(), 1);
        assert!(line.contains("host=- ") && line.contains(&format!("attempt={missing} ")));
        Ok(())
    }

    #[test]
    fn refused_provider_continuation_keeps_a_durable_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        struct Refusing {
            prior: Option<AttemptId>,
        }
        impl run::Providers for Refusing {
            fn prepare(
                &self,
                _: &run::TaskRequest,
                _: &str,
                _: &run::LaunchPaths,
                resume: Option<&run::AttemptRecord>,
            ) -> Result<run::PreparedLaunch, String> {
                assert_eq!(resume.map(|prior| prior.attempt_id), self.prior);
                Err("fixture continuation refused; run provenant run".into())
            }
            fn decode(
                &self,
                _: &run::RouteRecord,
                _: &[u8],
                _: &ExitObservation,
                _: &Path,
            ) -> Result<run::Decoded, String> {
                unreachable!("a refused launch never reaches a provider")
            }
        }
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
        let store = trace_store(&temp, &call)?;
        let prior = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        store.create(&prior, &call)?;
        store.start(
            prior.run_id,
            &Refusing { prior: None },
            &|_, _| unreachable!(),
            &call,
        )?;
        let prior_id = prior.attempts[0].attempt_id;
        let mut resumed = trace_run(20, prior_id, clock.wall_now()?)?;
        resumed.request.parent = Some(prior_id);
        resumed.request.tasks[0].task_id = prior.attempts[0].task_id;
        resumed.attempts[0].task_id = prior.attempts[0].task_id;
        store.create(&resumed, &call)?;
        let started = store.start(
            resumed.run_id,
            &Refusing {
                prior: Some(prior_id),
            },
            &|_, _| unreachable!(),
            &call,
        )?;
        let [run::Started::Refused { attempt, .. }] = started.as_slice() else {
            return Err("continuation must be refused".into());
        };
        assert_eq!(*attempt, resumed.attempts[0].attempt_id);
        // Reopen the store: every ID printed by the refusal must remain
        // addressable after the command exits.
        drop(store);
        let reopened = trace_store(&temp, &call)?;
        let record = run::Service::new(&reopened).status(*attempt, &call)?;
        assert_eq!(record.run_id, resumed.run_id);
        assert!(matches!(
            record.state,
            run::AttemptState::Terminal { outcome: run::Outcome::Rejected, reason: Some(ref reason), .. }
                if reason.contains("fixture continuation refused")
        ));
        Ok(())
    }

    #[test]
    fn grants_lines_list_each_applied_grant_and_stay_empty_for_older_attempts()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let store = trace_store(&temp, &call)?;
        let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        store.create(&prepared, &call)?;
        let mut record = store.record(prepared.attempts[0].attempt_id, &call)?;
        assert_eq!(grants_lines(&record), "");
        record.write_grants = vec![run::WriteGrantRecord {
            extra: Default::default(),
            source: run::WriteSource::WriteRoot,
            path: provenant_records::AbsolutePath::new(std::path::Path::new("/work/shared"))?,
            scope: run::PathScope::Subtree,
        }];
        record.read_only_writable = vec!["docs".into()];
        record.git_grants_digest = Some(provenant_core::Digest::from_bytes([6; 32]));
        let lines = grants_lines(&record);
        let mut lines = lines.lines();
        assert_eq!(
            lines.next(),
            Some("grant write source=write_root scope=subtree path=/work/shared")
        );
        assert_eq!(lines.next(), Some("grant read_only_writable path=docs"));
        assert!(
            lines
                .next()
                .is_some_and(|l| l.starts_with("grant git digest="))
        );
        assert_eq!(lines.next(), None);
        Ok(())
    }

    #[test]
    fn trace_chain_keeps_every_ancestor_of_a_long_chain() -> Result<(), Box<dyn std::error::Error>>
    {
        const LEN: u8 = 70;
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let store = trace_store(&temp, &call)?;
        let missing = AttemptId::from_bytes([9; 16]);
        let mut parent = missing;
        let mut ids = Vec::new();
        for link in 1..=LEN {
            // Each run uses three bytes of ID space, so step by three.
            let prepared = trace_run(3 * link, parent, clock.wall_now()?)?;
            store.create(&prepared, &call)?;
            parent = prepared.attempts[0].attempt_id;
            ids.push(parent);
        }
        let leaf = store.record(parent, &call)?;
        let chain = trace_chain(&store, leaf, &call).map_err(|f| f.message)?;
        let attempts: Vec<_> = chain.iter().filter_map(|t| t.attempt).collect();
        ids.reverse();
        ids.push(missing);
        assert_eq!(attempts, ids);
        Ok(())
    }

    #[test]
    fn trace_chain_fails_when_parents_loop() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|f| f.message)?;
        let store = trace_store(&temp, &call)?;
        // Attempt IDs are n + 2, so 12 and 22 name each other.
        let first = trace_run(10, AttemptId::from_bytes([22; 16]), clock.wall_now()?)?;
        let second = trace_run(20, AttemptId::from_bytes([12; 16]), clock.wall_now()?)?;
        store.create(&first, &call)?;
        store.create(&second, &call)?;
        let leaf = store.record(second.attempts[0].attempt_id, &call)?;
        let Err(fail) = trace_chain(&store, leaf, &call) else {
            return Err("a looping chain must fail".into());
        };
        assert_eq!(fail.kind.code(), "IO");
        assert!(fail.message.contains("loop back"), "{}", fail.message);
        Ok(())
    }

    #[test]
    fn check_continuable_refuses_nonterminal_before_session()
    -> Result<(), Box<dyn std::error::Error>> {
        for state in ["running", "queued"] {
            let prior: run::AttemptRecord = serde_json::from_value(json!({
                "schema_version": 1,
                "attempt_id": AttemptId::from_bytes([1; 16]),
                "run_id": RunId::from_bytes([2; 16]),
                "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
                "task_id": TaskId::from_bytes([4; 16]),
                "operation_id": "resume-nonterminal",
                "attempt_number": 1,
                "revision": 1,
                "generation": 0,
                "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
                "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
                "queued_at": WallTime::from_micros(100)?,
                "state": {"state":state},
                "usage": {},
                "prompt": {
                    "path": {"encoding":"utf8","path":"prompt.txt"},
                    "digest": CanonicalV1::file_digest(b""),
                    "bytes": 0,
                    "complete": true,
                    "redacted": false
                }
            }))?;
            let result = Registry::default().check_continuable(&prior);
            assert!(
                matches!(result, Err(fail) if fail.kind == ErrorKind::Nonterminal && fail.status == Status::Refused),
                "{state} attempt must be refused as nonterminal"
            );
        }
        Ok(())
    }

    #[test]
    fn cancel_and_reconcile_report_leftover_paths_and_reasons_without_terminal_controls() {
        let state = run::Reconciled::TemporaryRetained {
            path: PathBuf::from("/state/runs/run/attempts/attempt/tmp"),
            reason: "permission denied\n\x1b[31mretry later\x1b[0m".into(),
        };
        let rendered = reconcile_summary(&state);
        assert!(rendered.contains("/state/runs/run/attempts/attempt/tmp"));
        assert!(rendered.contains("permission denied"));
        assert!(rendered.contains("retry later"));
        assert!(!rendered.chars().any(char::is_control));
    }

    fn render_lanes_without_cooldowns(
        lanes: &[run::AttemptRecord],
        waits: &[run::LaneWait],
        next: Option<u64>,
        root: &Path,
    ) -> (Value, String) {
        render_lanes(lanes, waits, next, root, None)
    }

    #[test]
    fn local_lanes_read_and_show_capacity_cooldowns() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let route = run::RouteRecord {
            extra: Default::default(),
            provider: Some("codex".into()),
            model: Some("codex/model".into()),
            effort: None,
            account_label: Some("work".into()),
            account_key: Some("key".into()),
            source: None,
            pool_selection: None,
        };
        let now = SystemClock::new().wall_now()?.as_timestamp().as_second();
        crate::quota::persist_capacity_cooldown(&state, &route, now, 600)?;
        let cooldowns = read_lane_cooldowns(&state);
        let (data, human) = render_lanes(&[], &[], None, &state, lane_cooldown_view(&cooldowns));
        assert_eq!(data["capacity_cooldowns"][0]["model"], "model");
        assert_eq!(data["capacity_cooldowns"][0]["account"]["label"], "work");
        assert!(
            data["capacity_cooldowns"][0]["remaining_seconds"]
                .as_i64()
                .is_some_and(|seconds| seconds > 0 && seconds <= 600)
        );
        assert!(human.contains("capacity cooldown codex/model [work]:"));
        Ok(())
    }

    #[test]
    fn lanes_show_only_live_capacity_cooldowns() {
        let account = provenant_quota::AccountScope {
            adapter: "codex".into(),
            account_key: "key".into(),
            label: "work".into(),
        };
        let mut record = provenant_quota::AccountRecord::empty(account.clone());
        record.capacity_cooldowns = vec![provenant_quota::CapacityCooldown {
            extra: Default::default(),
            account,
            model: "model".into(),
            observed_unix_seconds: 100,
            expires_unix_seconds: 700,
        }];
        let mut human = String::new();
        let shown = render_cooldowns(&[record.clone()], 400, &mut human);
        assert_eq!(shown[0]["remaining_seconds"], 300);
        assert!(human.contains("codex/model [work]: 300s remaining"));
        assert!(render_cooldowns(&[record.clone()], 700, &mut String::new()).is_empty());
        assert!(render_cooldowns(&[record], 99, &mut String::new()).is_empty());
    }

    #[test]
    fn delivery_state_text_retains_known_details_without_extensions() {
        for (wire, expected) in [
            (json!({"state":"not_attempted"}), "NotAttempted"),
            (json!({"state":"no_new_commits"}), "NoNewCommits"),
            (
                json!({"state":"failed","stage":"push"}),
                "Failed { stage: Push }",
            ),
            (
                json!({"state":"delivered","pull_request":null}),
                "Delivered { pull_request: None }",
            ),
            (
                json!({"state":"delivered","pull_request":"https://example.invalid/pr/1"}),
                "Delivered { pull_request: Some(\"https://example.invalid/pr/1\") }",
            ),
            (
                json!({"state":"delivered","pull_request":"pr\u{001b}\n1"}),
                "Delivered { pull_request: Some(\"pr1\") }",
            ),
        ] {
            let mut wire = wire;
            wire["future_note"] = json!("future-secret");
            let state = serde_json::from_value(wire).unwrap();
            assert_eq!(delivery_state_text(&state), expected);
        }
    }

    #[test]
    fn render_lanes_removes_peer_terminal_controls() -> Result<(), Box<dyn std::error::Error>> {
        let reason = "\x1b[31mreason\x1b[0m";
        let account = "\x1b]0;account\x07";
        let detail = "skip\n\r\t\u{0008}\u{202e}detail";
        let remote = "or\x1bigin";
        let branch = "main\u{009b}line";
        let now = WallTime::from_micros(100)?;
        let mut lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "peer-lane-render",
            "attempt_number": 1,
            "title": "Review\nchanges",
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"fixture","model":"fixture/model"},
            "applied_route": {"model":"fixture/\x1b[32mmodel","account_label":account},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "write_grants": [{"source":"write_root","path":{"relative_to_root":{"encoding":"utf8","path":"work/shared"}},"scope":"subtree"}],
            "read_only_writable": ["locks"],
            "git_grants_digest": provenant_core::Digest::from_bytes([6; 32]),
            "queued_at": now,
            "priority": 10,
            "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":reason},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            },
            "skipped_routes": [{"route":"fixture/\x1b[33mfallback","kind":"unavailable","detail":detail}],
            "delivery": {"schema_version":1,"remote":remote,"branch":branch,"state":{"state":"not_attempted","future_note":"future-secret"}}
        }))?;
        let (data, human) = render_lanes_without_cooldowns(
            std::slice::from_ref(&lane),
            &[],
            None,
            Path::new("/state"),
        );
        assert_eq!(
            human,
            format!(
                "{}/{}/{} Reviewchanges failed fixture/[32mmodel account=]0;account ([31mreason[0m)\n  skipped fixture/[33mfallback Unavailable: skipdetail\n  delivery origin:mainline NotAttempted head=unknown\n",
                RunId::from_bytes([2; 16]),
                TaskId::from_bytes([4; 16]),
                AttemptId::from_bytes([1; 16])
            )
        );
        assert!(!human.chars().any(|c| c.is_control() && c != '\n'));
        assert!(!human.contains('\u{202e}'));
        assert_eq!(data["lanes"][0]["title"], "Review\nchanges");
        assert_eq!(data["lanes"][0]["priority"], 10);
        assert!(data["lanes"][0].get("requested_priority").is_none());
        lane.requested_priority = Some(10);
        lane.priority = Some(15);
        let (different, _) = render_lanes_without_cooldowns(
            std::slice::from_ref(&lane),
            &[],
            None,
            Path::new("/state"),
        );
        assert_eq!(different["lanes"][0]["priority"], 15);
        assert_eq!(different["lanes"][0]["requested_priority"], 10);
        assert_eq!(data["lanes"][0]["reason"], json!(reason));
        assert_eq!(data["lanes"][0]["write_grants"][0]["source"], "write_root");
        assert_eq!(data["lanes"][0]["write_grants"][0]["scope"], "subtree");
        assert_eq!(
            data["lanes"][0]["write_grants"][0]["path"]["relative_to_root"]["path"],
            "work/shared"
        );
        assert_eq!(data["lanes"][0]["read_only_writable"], json!(["locks"]));
        assert_eq!(
            data["lanes"][0]["git_grants_digest"],
            json!(provenant_core::Digest::from_bytes([6; 32]))
        );
        assert_eq!(data["lanes"][0]["account"], json!(account));
        assert_eq!(
            data["lanes"][0]["skipped_routes"][0]["detail"],
            json!(detail)
        );
        assert_eq!(data["lanes"][0]["delivery"]["remote"], json!(remote));
        assert_eq!(data["lanes"][0]["delivery"]["branch"], json!(branch));
        assert_eq!(
            data["lanes"][0]["delivery"]["state"]["future_note"],
            "future-secret"
        );
        assert!(!human.contains("future-secret"));
        lane.state = run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::ModelUnavailable,
            exit_code: Some(1),
            finished_at: now,
            reason: Some("provider capacity: Selected model is at capacity".into()),
        };
        lane.session = Some("session".into());
        let (data, human) = render_lanes_without_cooldowns(
            std::slice::from_ref(&lane),
            &[],
            None,
            Path::new("/state"),
        );
        assert_eq!(data["lanes"][0]["state"], "model_unavailable");
        assert!(human.contains("(provider capacity: Selected model is at capacity)"));
        assert!(human.contains(&format!(
            "continue: provenant resume {} --prompt-file PROMPT",
            lane.attempt_id
        )));
        lane.session = None;
        let (_, human) = render_lanes_without_cooldowns(
            std::slice::from_ref(&lane),
            &[],
            None,
            Path::new("/state"),
        );
        assert!(!human.contains("provenant resume"));
        assert!(human.contains("redispatch with another model"));
        lane.session = Some("session".into());
        if let run::AttemptState::Terminal { reason, .. } = &mut lane.state {
            *reason = Some("provider model unavailable: no route".into());
        }
        let (_, human) = render_lanes_without_cooldowns(
            std::slice::from_ref(&lane),
            &[],
            None,
            Path::new("/state"),
        );
        assert!(!human.contains("provenant resume"));
        assert!(human.contains("redispatch with a corrected model"));
        lane.state = run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::ReviewRequired,
            exit_code: Some(0),
            finished_at: now,
            reason: None,
        };
        lane.head = Some(provenant_core::GitOid::parse(&"a".repeat(40))?);
        lane.protected_paths = vec![provenant_records::PathWire::from_relative(
            &provenant_core::RelativePath::parse(".github/unsafe\npath.yml")?,
        )?];
        lane.delivery = None;
        let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
        assert_eq!(data["lanes"][0]["state"], "review_required");
        assert_eq!(data["lanes"][0]["head"], "a".repeat(40));
        assert_eq!(
            data["lanes"][0]["protected_paths"][0]["path"],
            ".github/unsafe\npath.yml"
        );
        assert!(human.contains(&format!(
            "review head={} paths=.github/unsafepath.yml",
            "a".repeat(40)
        )));
        Ok(())
    }

    #[test]
    fn render_lanes_shows_review_line_when_review_required_without_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = WallTime::from_micros(100)?;
        let mut lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "review-no-paths",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": null,
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": now,
            "state": {"state":"terminal","outcome":"failed","exit_code":1,"finished_at":now,"reason":null},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))?;
        lane.state = run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: run::Outcome::ReviewRequired,
            exit_code: Some(0),
            finished_at: now,
            reason: None,
        };
        lane.protected_paths = vec![];
        let (_, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
        assert!(human.contains("  review head=unknown paths=\n"));
        Ok(())
    }

    #[test]
    fn render_lanes_points_at_the_provider_transcript() -> Result<(), Box<dyn std::error::Error>> {
        let now = WallTime::from_micros(100)?;
        let mut lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "transcript-render",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": {"provider":"claude","model":"claude/claude-sonnet-5"},
            "session": "sess-1848",
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": now,
            "state": {"state": "terminal", "outcome": "rejected", "finished_at": now},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 0,
                "complete": true,
                "redacted": false
            }
        }))?;
        let root = tempfile::tempdir()?;
        let state = root.path().canonicalize()?;
        let attempt = artifact(&state, &lane, "");
        let transcript = attempt.join("provider-state/candidate-0/claude-config/projects/k");
        std::fs::create_dir_all(&transcript)?;
        std::fs::write(transcript.join("sess-1848.jsonl"), "{}\n")?;
        let (data, _) = render_lanes_without_cooldowns(&[lane.clone()], &[], None, &state);
        let found = &data["lanes"][0]["transcript"];
        assert_eq!(found["adapter"], "claude");
        assert_eq!(found["session"], "sess-1848");
        assert_eq!(
            found["path"],
            transcript
                .join("sess-1848.jsonl")
                .to_string_lossy()
                .as_ref()
        );
        // Read-only Claude records its selected account's exact state-write path.
        // The lookup never follows links, so build the grant from the resolved temp root.
        let account = root
            .path()
            .canonicalize()?
            .join("accounts/claude/projects/project-key");
        std::fs::create_dir_all(&account)?;
        let path = account.join("sess-1848.jsonl");
        std::fs::write(&path, "{}\n")?;
        lane.write_grants = vec![run::WriteGrantRecord {
            extra: Default::default(),
            source: run::WriteSource::StateWrite,
            scope: run::PathScope::Exact,
            path: provenant_records::AbsolutePath::new(&path)?,
        }];
        let other = tempfile::tempdir()?;
        std::fs::create_dir_all(artifact(other.path(), &lane, ""))?;
        let (data, _) = render_lanes_without_cooldowns(&[lane], &[], None, other.path());
        assert_eq!(
            data["lanes"][0]["transcript"]["path"],
            path.to_string_lossy().as_ref()
        );
        Ok(())
    }

    #[test]
    fn render_lanes_omits_account_when_unresolved() -> Result<(), Box<dyn std::error::Error>> {
        let reason = "argv prompt exceeds 64 KiB";
        let now = WallTime::from_micros(100)?;
        let lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "prompt-refusal-render",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": null,
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": now,
            "state": {
                "state": "terminal",
                "outcome": "rejected",
                "finished_at": now,
                "reason": reason
            },
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 65537,
                "complete": true,
                "redacted": false
            }
        }))?;
        let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
        assert_eq!(
            human,
            format!(
                "{}/{}/{} rejected unresolved ({reason})\n",
                RunId::from_bytes([2; 16]),
                TaskId::from_bytes([4; 16]),
                AttemptId::from_bytes([1; 16])
            )
        );
        assert!(!human.contains("account="));
        assert!(data["lanes"][0].get("title").is_none());
        assert_eq!(data["lanes"][0]["account"], json!("unknown"));
        assert_eq!(data["lanes"][0]["state"], "rejected");
        assert_eq!(data["lanes"][0]["reason"], json!(reason));
        assert!(data["lanes"][0]["applied_route"].is_null());
        Ok(())
    }
    #[test]
    fn local_wait_observes_settlement_and_renders_pending_details()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
        let store = trace_store(&temp, &call)?;
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let context = run::ExecutionContext {
            state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            read_prompts: true,
        };
        let session = Session { store, context };
        let prepared = trace_run(10, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        session.store.create(&prepared, &call)?;
        let id = prepared.attempts[0].attempt_id;
        let targets = [Target {
            selection: None,
            run: Some(prepared.run_id),
            task: None,
            attempt: Some(id),
        }];
        let picks = Picks {
            targets: &targets,
            owner: session.context.actor_id,
            label: None,
            since: None,
        };
        let mut fetch = candidates(&session, &picks, None);
        let first = fetch(Duration::from_secs(1)).map_err(|fail| fail.message)?;
        assert_eq!(first.len(), 1);
        let (data, _) = render_settled(&session, &first, None).map_err(|fail| fail.message)?;
        assert_eq!(
            data["lanes"][0]["waiting_for"],
            "launch: not yet handed to a guardian"
        );
        let mut reads = 0;
        let finished_at = clock.wall_now()?;
        let attempt_path = session
            .context
            .state_root
            .as_path()
            .join("runs")
            .join(prepared.run_id.to_string())
            .join("attempts")
            .join(id.to_string());
        let directory = provenant_os::fs::Directory::open_anchor(&attempt_path)?;
        let mut custody = Some(directory.try_lock(
            std::ffi::OsStr::new("custody.lock"),
            provenant_os::fs::LockMode::Exclusive,
        )?);
        let waited = watch::wait(
            |remaining| {
                reads += 1;
                if reads == 2 {
                    session.store.withdraw(
                        id,
                        run::AttemptState::Queued {
                            extra: Default::default(),
                        },
                        run::Outcome::Cancelled,
                        "fixture settlement",
                        finished_at,
                        &call,
                    )?;
                }
                if reads == 3 {
                    drop(custody.take());
                }
                fetch(remaining)
            },
            picks.scope().map_err(|fail| fail.message)?,
            Condition::All,
            Duration::from_secs(1),
            Duration::from_millis(1),
        )
        .map_err(|fail| fail.message)?;
        assert!(waited.met);
        assert_eq!(waited.lanes.len(), 1);
        assert!(matches!(
            waited.lanes[0].state,
            run::AttemptState::Terminal {
                outcome: run::Outcome::Cancelled,
                ..
            }
        ));
        assert_eq!(reads, 3, "terminal state must wait for custody release");
        assert!(session.store.custody_released(&waited.lanes[0], &call)?);
        Ok(())
    }

    #[test]
    fn timeout_render_reads_wait_details_without_a_retry_budget() {
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let waits: Vec<run::LaneWait> = settled_waits(&clock, &cancel, |call| {
            assert_eq!(call.budget.remaining(call.clock.mono_now()).as_micros(), 0);
            Vec::new()
        });
        assert!(waits.is_empty());
    }
    #[test]
    fn render_lanes_says_what_an_unstarted_lane_waits_for() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = WallTime::from_micros(100)?;
        let lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "wait-render",
            "attempt_number": 1,
            "revision": 1,
            "generation": 1,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": null,
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": now,
            "state": {"state": "starting"},
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 1,
                "complete": true,
                "redacted": false
            }
        }))?;
        let wait = run::LaneWait {
            extra: Default::default(),
            attempt_id: lane.attempt_id,
            lane_class: run::LaneClass::ReadOnly,
            waiting_for: "read-only startup spacing: 1.2 s left of 2.0 s".into(),
        };
        let (data, human) =
            render_lanes_without_cooldowns(&[lane], &[wait], None, Path::new("/state"));
        assert!(
            human.contains("starting unresolved (read-only startup spacing: 1.2 s left of 2.0 s)"),
            "{human}"
        );
        let row = &data["lanes"][0];
        assert_eq!(row["lane_class"], "read_only");
        assert_eq!(
            row["waiting_for"],
            "read-only startup spacing: 1.2 s left of 2.0 s"
        );
        assert_eq!(row["reason"], row["waiting_for"]);
        Ok(())
    }
    #[test]
    fn render_lanes_shows_uncommitted_with_its_dirty_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let reason = "writer left uncommitted edits and no new commit: src/a.rs, b.txt";
        let now = WallTime::from_micros(100)?;
        let lane: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]),
            "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]),
            "operation_id": "uncommitted-render",
            "attempt_number": 1,
            "revision": 1,
            "generation": 0,
            "requested_route": {"kind":"model","provider":"claude","model":"claude/claude-sonnet-5"},
            "applied_route": null,
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]),
            "queued_at": now,
            "state": {
                "state": "terminal",
                "outcome": "uncommitted",
                "finished_at": now,
                "reason": reason
            },
            "usage": {},
            "prompt": {
                "path": {"encoding":"utf8","path":"prompt.txt"},
                "digest": CanonicalV1::file_digest(b""),
                "bytes": 1,
                "complete": true,
                "redacted": false
            }
        }))?;
        assert_eq!(settled(&lane.state), (true, false));
        let (data, human) = render_lanes_without_cooldowns(&[lane], &[], None, Path::new("/state"));
        assert!(
            human.contains(&format!(" uncommitted unresolved ({reason})\n")),
            "{human}"
        );
        assert_eq!(data["lanes"][0]["state"], "uncommitted");
        assert_eq!(data["lanes"][0]["reason"], json!(reason));
        Ok(())
    }
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
        assert_eq!(network(Mode::ReadOnly, None, &["postgres"]), Ok(None));
        assert_eq!(
            network(Mode::WorktreeWrite, Some(Switch::Off), &["postgres"]),
            Ok(Some(run::NetworkAccess::Local))
        );
        assert_eq!(
            network(Mode::ReadOnly, Some(Switch::Off), &["postgres"]),
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
            [provenant_os::fs::canonicalize(&project)?]
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
    fn lane_access_refuses_write_root_overlapping_the_worktree()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let worktree = temp.path().join("work").join("tree");
        let inside = worktree.join("src");
        std::fs::create_dir_all(&state)?;
        std::fs::create_dir_all(&inside)?;
        let access = |read_root: Vec<PathBuf>, write_root: Vec<PathBuf>| crate::Access {
            mode: Mode::WorktreeWrite,
            worktree: Some(worktree.clone()),
            read_root,
            write_root,
            network: None,
            capability: Vec::new(),
        };
        for overlapping in [worktree.clone(), temp.path().join("work")] {
            let message = lane_access(&access(Vec::new(), vec![overlapping]), &state)
                .err()
                .map(|fail| fail.message)
                .unwrap_or_default();
            assert!(
                message.contains("--write-root") && message.contains("already grants"),
                "{message}"
            );
        }
        // A read root on the worktree and a write root beneath it stay valid.
        assert!(lane_access(&access(vec![worktree.clone()], vec![inside]), &state).is_ok());
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
            termination: Some(provenant_os::exec::Termination::Exit(1)),
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
            cancel: None,
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
            termination: Some(provenant_os::exec::Termination::Exit(0)),
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
    fn opencode_unavailable_model_reaches_the_decoded_outcome() {
        let route = run::RouteRecord {
            provider: Some("opencode".into()),
            model: Some("opencode/opencode/glm-5.3-flas-free".into()),
            ..Default::default()
        };
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let decoded = Registry::default()
            .decode(
                &route,
                include_bytes!(
                    "../../../tests/fixtures/providers/opencode/model-unavailable.jsonl"
                ),
                &exit,
                Path::new("/nonexistent"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        let reason = decoded.reason.as_deref().unwrap();
        assert!(
            reason.starts_with(
                "provider model unavailable: Model unavailable: opencode/misspelt-free;"
            ),
            "{reason}"
        );
        assert!(reason.contains("nearest catalogued IDs:"));
        assert!(decoded.observed_route.is_none());
    }

    #[test]
    fn unlisted_codex_generic_failure_does_not_suggest_catalogued_models() {
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-luna".into()),
            ..Default::default()
        };
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let decoded = Registry::default()
            .decode(&route, b"", &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_ne!(decoded.outcome, run::Outcome::Succeeded);
        assert!(
            !decoded
                .reason
                .unwrap_or_default()
                .contains("nearest catalogued IDs")
        );
    }

    #[test]
    fn codex_capacity_reaches_the_decoded_outcome() {
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            ..Default::default()
        };
        let stream = format!(
            "{}\n{}\n",
            json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
            json!({"type":"turn.failed","error":{"message":"Selected model is at capacity. Please try a different model.","codex_error_info":"server_overloaded"}})
        );
        let decoded = Registry::default()
            .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider capacity: Selected model is at capacity. Please try a different model.")
        );
        assert_eq!(
            decoded.session.as_deref(),
            Some("01234567-89ab-cdef-0123-456789abcdef")
        );
    }

    #[test]
    fn capacity_store_failure_does_not_change_settlement() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let account_dir = state.join("quota/key");
        std::fs::create_dir_all(&account_dir)?;
        for dir in [&state, &state.join("quota"), &account_dir] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let bad_record = account_dir.join("codex.json");
        std::fs::write(&bad_record, b"invalid quota record")?;
        std::fs::set_permissions(&bad_record, std::fs::Permissions::from_mode(0o600))?;
        let context = run::HolderContext {
            schema_version: 1,
            roots: run::RootSeed {
                home: temp.path().join("home"),
                checkout: temp.path().join("checkout"),
                state_root: state.clone(),
            },
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            run_id: RunId::from_bytes([3; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            account_key: Some("key".into()),
            account_label: Some("label".into()),
            ..Default::default()
        };
        let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        // The malformed on-disk record makes the store update fail after the
        // settlement path creates the lock. The lock proves it attempted the write.
        assert!(!state.join("quota/quota.lock").exists());
        let decoded = Registry::default().decode_with_context(
            &route,
            stream,
            &exit,
            Path::new("/nonexistent"),
            &context,
        )?;
        assert!(state.join("quota/quota.lock").is_file());
        assert_eq!(std::fs::read(&bad_record)?, b"invalid quota record");
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider capacity: at capacity")
        );
        Ok(())
    }

    #[test]
    fn nested_capacity_settlement_does_not_write_shared_cooldown()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::ffi::OsString;
        const MARKER: &str = "PROVENANT_TEST_NESTED_CAPACITY_CHILD";
        if std::env::var_os(MARKER).is_none() {
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::tests::nested_capacity_settlement_does_not_write_shared_cooldown".into(),
                "--nocapture".into(),
            ];
            let mut env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
                .collect();
            env.push((OsString::from(MARKER), OsString::from("1")));
            env.push((
                OsString::from(provenant_core::RUN_STATE_DIRECTORY_ENV),
                OsString::from("nested"),
            ));
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            let out = SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
            assert!(!out.timed_out, "{out:?}");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let registry = Registry {
            roots: Some(&roots),
            ..Default::default()
        };
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            account_key: Some("key".into()),
            account_label: Some("label".into()),
            ..Default::default()
        };
        let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
        let mut exit = clean_exit();
        exit.termination = Some(Termination::Exit(1));
        let decoded = registry.decode(&route, stream, &exit, Path::new("/nonexistent"))?;
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider capacity: at capacity")
        );
        assert!(!roots.state.as_path().join("quota").exists());
        Ok(())
    }

    #[test]
    fn invalid_sealed_root_never_writes_to_registry_root() -> Result<(), Box<dyn std::error::Error>>
    {
        use provenant_os::exec::{
            ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination,
        };
        use std::ffi::OsString;
        const MARKER: &str = "PROVENANT_TEST_INVALID_CAPACITY_ROOT";
        if std::env::var_os(MARKER).is_none() {
            let argv = vec![
                std::env::current_exe()?.into_os_string(),
                "--exact".into(),
                "run::tests::invalid_sealed_root_never_writes_to_registry_root".into(),
            ];
            let mut env: Vec<_> = std::env::vars_os()
                .filter(|(key, _)| key != MARKER && key != provenant_core::RUN_STATE_DIRECTORY_ENV)
                .collect();
            env.push((OsString::from(MARKER), OsString::from("1")));
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(30));
            options.env = Some(&env);
            options.stdout = StreamPolicy::Capture { limit: 65_536 };
            options.stderr = StreamPolicy::Capture { limit: 65_536 };
            let out = SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(out.termination, Termination::Exit(0), "{out:?}");
            assert!(!out.timed_out, "{out:?}");
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("registry"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let registry = Registry {
            roots: Some(&roots),
            ..Default::default()
        };
        let protected = temp.path().join("protected");
        let context = run::HolderContext {
            schema_version: 1,
            roots: run::RootSeed {
                home: temp.path().join("home"),
                checkout: protected.clone(),
                state_root: protected,
            },
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            run_id: RunId::from_bytes([3; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            account_key: Some("key".into()),
            account_label: Some("label".into()),
            ..Default::default()
        };
        let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
        let mut exit = clean_exit();
        exit.termination = Some(Termination::Exit(1));
        let decoded = registry.decode_with_context(
            &route,
            stream,
            &exit,
            Path::new("/nonexistent"),
            &context,
        )?;
        assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
        assert!(
            decoded
                .reason
                .unwrap_or_default()
                .contains("capacity cooldown configuration unavailable")
        );
        assert!(!roots.state.as_path().join("quota").exists());
        Ok(())
    }

    #[test]
    fn capacity_guardian_context_records_and_reports_config_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::DirBuilderExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::DirBuilder::new().mode(0o700).create(&state)?;
        let context = run::HolderContext {
            schema_version: 1,
            roots: run::RootSeed {
                home: temp.path().join("home"),
                checkout: temp.path().join("checkout"),
                state_root: state.clone(),
            },
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: ActorId::from_bytes([2; 16]),
            run_id: RunId::from_bytes([3; 16]),
            attempt_id: AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-sol".into()),
            account_key: Some("key".into()),
            account_label: Some("label".into()),
            ..Default::default()
        };
        let stream = br#"{"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}}"#;
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let registry = Registry::default();
        let decode = || {
            registry.decode_with_context(&route, stream, &exit, Path::new("/nonexistent"), &context)
        };
        std::fs::write(state.join("config.toml"), "invalid = [")?;
        let failed = decode()?;
        assert_eq!(failed.outcome, run::Outcome::ModelUnavailable);
        assert!(
            failed
                .reason
                .unwrap_or_default()
                .contains("capacity cooldown configuration unavailable")
        );
        std::fs::write(
            state.join("config.toml"),
            "schema_version = 1\nprofile = 'balanced'\n[domains.routing]\ncapacity_cooldown_seconds = 0\n",
        )?;
        let disabled = decode()?;
        assert_eq!(
            disabled.reason.as_deref(),
            Some("provider capacity: at capacity")
        );
        assert!(!state.join("quota").exists());
        std::fs::remove_file(state.join("config.toml"))?;
        decode()?;
        let record = provenant_quota::AccountRecord::decode(&std::fs::read(
            state.join("quota/key/codex.json"),
        )?)?;
        assert_eq!(record.capacity_cooldowns.len(), 1);
        let before = std::fs::read(state.join("quota/key/codex.json"))?;
        let mut nested = context.clone();
        nested.roots.state_root = state.join("children/child");
        nested.launch = Some(run::HostLaunchContext {
            configuration_root: state.clone(),
            cwd: temp.path().join("checkout"),
            executables: Default::default(),
            executable_search_path: Vec::new(),
            git: PathBuf::from("/usr/bin/git"),
            codex_home: None,
            cargo_build_jobs: None,
            #[cfg(feature = "fixture")]
            fixture_state_write: false,
        });
        let nested_result = registry.decode_with_context(
            &route,
            stream,
            &exit,
            Path::new("/nonexistent"),
            &nested,
        )?;
        assert_eq!(nested_result.outcome, run::Outcome::ModelUnavailable);
        assert_eq!(std::fs::read(state.join("quota/key/codex.json"))?, before);
        assert!(!nested.roots.state_root.join("quota").exists());
        Ok(())
    }

    #[test]
    fn claude_and_opencode_overload_reach_the_decoded_outcome() {
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        for (provider, model, stream) in [
            (
                "claude",
                "claude/claude-sonnet-5-5",
                r#"{"type":"result","session_id":"s","subtype":"error_during_execution","is_error":true,"api_error_status":529,"result":"Overloaded"}"#,
            ),
            (
                "opencode",
                "opencode/anthropic/test",
                r#"{"type":"error","sessionID":"ses_x","error":{"type":"provider.internal","status":529,"message":"Overloaded"}}"#,
            ),
        ] {
            let route = run::RouteRecord {
                provider: Some(provider.into()),
                model: Some(model.into()),
                ..Default::default()
            };
            let decoded = Registry::default()
                .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::ModelUnavailable);
            assert!(
                decoded
                    .reason
                    .as_deref()
                    .unwrap()
                    .starts_with("provider capacity: Overloaded")
            );
            assert!(decoded.session.is_some());
        }
    }

    #[test]
    fn codex_rate_limit_reaches_the_decoded_outcome() {
        let mut exit = clean_exit();
        exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6-luna".into()),
            ..Default::default()
        };
        let registry = Registry::default();
        for (message, expected) in [
            (
                "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
                "exceeded retry limit, last status: 429 Too Many Requests".to_owned(),
            ),
            (
                "429 Too Many Requests for ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
                "429 Too Many Requests for [REDACTED]".into(),
            ),
            (
                format!("429 Too Many Requests {}", "☀".repeat(3000)),
                format!("429 Too Many Requests {}", "☀".repeat(2723)),
            ),
        ] {
            let stream = [
                json!({"type":"thread.started","thread_id":"01234567-89ab-cdef-0123-456789abcdef"}),
                json!({"type":"error","message":message}),
                json!({"type":"turn.failed","error":{"message":message}}),
            ]
            .map(|frame| format!("{frame}\n"))
            .concat();
            let decoded = registry
                .decode(&route, stream.as_bytes(), &exit, Path::new("/nonexistent"))
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::RateLimited);
            assert_eq!(decoded.reason.as_deref(), Some(expected.as_str()));
            assert!(decoded.reason.unwrap().len() <= 8192);
        }
    }

    #[test]
    fn agy_denials_preserve_answers_and_prior_failures() {
        let route = run::RouteRecord {
            provider: Some("agy".into()),
            model: Some("agy/gemini-3.8-flash".into()),
            ..Default::default()
        };
        let fixture = include_str!("../../../tests/fixtures/providers/agy/denied-result.jsonl");
        let recovered = fixture.replace("\"response\":\"\"", "\"response\":\"Recovered answer\"");
        let registry = Registry::default();
        let decoded = registry
            .decode(
                &route,
                recovered.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::Succeeded);
        assert_eq!(decoded.result, "Recovered answer");
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider denied: RunCommand")
        );
        let decoded = registry
            .decode(
                &route,
                fixture.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::Failed);
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider denied: RunCommand")
        );
        let mut failed_exit = clean_exit();
        failed_exit.termination = Some(provenant_os::exec::Termination::Exit(1));
        let decoded = registry
            .decode(
                &route,
                fixture.as_bytes(),
                &failed_exit,
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.outcome, run::Outcome::Failed);
        assert_eq!(decoded.reason.as_deref(), Some("provider failed"));
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
            cancel: None,
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
    fn reviewer_receipts_cover_provenance_and_resolution_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = WallTime::from_micros(100)?;
        let mut record: run::AttemptRecord = serde_json::from_value(json!({
            "schema_version": 1, "attempt_id": AttemptId::from_bytes([1; 16]),
            "run_id": RunId::from_bytes([2; 16]), "project_id": provenant_core::ProjectId::from_bytes([3; 16]),
            "task_id": TaskId::from_bytes([4; 16]), "operation_id": "receipt-test",
            "attempt_number": 1, "revision": 1, "generation": 0,
            "requested_route": {"kind":"model","provider":"codex","model":"codex/gpt-6.1-sol"},
            "applied_route": {"provider":"codex","model":"codex/gpt-6.1-sol","effort":"high","account_label":null},
            "authority_digest": provenant_core::Digest::from_bytes([5; 32]), "queued_at": now,
            "state": {"state":"terminal","outcome":"lost","exit_code":null,"finished_at":now,"reason":null},
            "usage": {}, "prompt": {"path":{"encoding":"utf8","path":"prompt.txt"}, "digest":CanonicalV1::file_digest(b""),"bytes":0,"complete":true,"redacted":false}
        }))?;
        let (data, line) = reviewer_receipt(&record, false, &[]).unwrap();
        assert_eq!(data["resolved_from"], "explicit");
        assert_eq!(data["reviewer"]["provenance"], "unknown");
        assert_eq!(data["reviewer"]["actor_id"], "unknown");
        assert_eq!(
            data["receipt"],
            format!(
                "Reviewer: codex/gpt-6.1-sol@high (openai; resolved from explicit), run {}/{}/{}",
                record.run_id, record.task_id, record.attempt_id
            )
        );
        assert!(line.ends_with(
            "Reviewer provenance: unknown, actor unknown, model unknown, family unknown\n"
        ));
        record.verified_owner = Some(ActorId::from_bytes([6; 16]));
        let unobserved = reviewer_receipt(&record, false, &[]).unwrap().0;
        assert_eq!(unobserved["reviewer"]["provenance"], "unknown");
        assert_ne!(unobserved["reviewer"]["actor_id"], "unknown");
        record.observed_route = record.applied_route.clone();
        let observed_fact = run::fact(record.clone())?.reviewer.ok_or("reviewer fact")?;
        let observed = reviewer_receipt(&record, false, &[]).unwrap();
        assert_eq!(observed.0["reviewer"]["provenance"], "observed");
        assert_eq!(
            observed.0["reviewer"]["actor_id"],
            observed_fact.actor_id.to_string()
        );
        assert_eq!(observed.0["reviewer"]["family"], "openai");
        assert!(observed.1.contains(&format!(
            "Reviewer provenance: observed, actor {},",
            observed_fact.actor_id
        )));
        record.applied_route.as_mut().unwrap().model = Some("gpt-6.1-sol".into());
        let (writer_data, writer_line) = reviewer_receipt(&record, true, &[]).unwrap();
        assert_eq!(writer_data["role"], "writer");
        assert_eq!(writer_data["family"], "openai");
        assert!(writer_line.starts_with("Writer: codex/gpt-6.1-sol@high (openai;"));
        record.applied_route.as_mut().unwrap().effort = None;
        assert!(
            reviewer_receipt(&record, false, &[])
                .unwrap()
                .1
                .contains("@unknown")
        );
        record.applied_route.as_mut().unwrap().model = Some("codex/uncatalogued".into());
        let unknown_family = reviewer_receipt(&record, false, &[]).unwrap().0;
        assert_eq!(unknown_family["family"], "unknown");
        assert_eq!(unknown_family["resolved_from"], "unknown");
        // A differing legacy route does not prove fallback without canonical task evidence.
        let fallback = record.applied_route.clone().ok_or("fallback route")?;
        assert_eq!(
            reviewer_receipt(&record, false, std::slice::from_ref(&fallback))
                .unwrap()
                .0["resolved_from"],
            "unknown"
        );
        record.skipped_routes.push(serde_json::from_value(
            json!({"route":"codex/gpt-6.1-sol","kind":"unavailable","detail":"unavailable"}),
        )?);
        assert_eq!(
            reviewer_receipt(&record, false, &[fallback]).unwrap().0["resolved_from"],
            "fallback"
        );
        record.applied_route.as_mut().unwrap().model = Some("codex/gpt-6.1-sol".into());
        record.applied_route.as_mut().unwrap().pool_selection = Some(serde_json::from_value(
            json!({"pool":"junior","candidates":[],"roll":0,"total_weight":1,"pick":"codex/gpt-6.1-sol"}),
        )?);
        // An unavailable candidate is still a pool selection, not a fallback.
        assert_eq!(
            reviewer_receipt(&record, false, &[]).unwrap().0["resolved_from"],
            "pool junior"
        );
        record.applied_route.as_mut().unwrap().model = Some("codex/other".into());
        assert_eq!(
            reviewer_receipt(&record, false, &[]).unwrap().0["resolved_from"],
            "unknown"
        );
        record.applied_route = None;
        record.observed_route = None;
        let unknown = reviewer_receipt(&record, false, &[]).unwrap().0;
        for field in ["adapter", "model", "effort", "family", "resolved_from"] {
            assert_eq!(unknown[field], "unknown", "{field}");
        }
        record.applied_route = Some(Default::default());
        assert_eq!(
            reviewer_receipt(&record, false, &[]).unwrap().0["model"],
            "unknown"
        );
        record.state = run::AttemptState::Running {
            extra: Default::default(),
        };
        let refused = reviewer_receipt(&record, false, &[]).unwrap_err();
        assert_eq!(refused.status, Status::Refused);
        assert_eq!(refused.kind, ErrorKind::Nonterminal);
        Ok(())
    }

    #[test]
    fn opencode_without_a_session_record_observes_no_model() {
        let route = run::RouteRecord {
            provider: Some("opencode".into()),
            model: Some("opencode/opencode/glm-5.3-flash-free".into()),
            ..Default::default()
        };
        let registry = Registry::default();
        let stream = [
            json!({"type":"text","sessionID":"ses_a","part":{"text":"OK"}}),
            json!({"type":"step_finish","sessionID":"ses_a","part":{"reason":"stop"}}),
        ]
        .map(|frame| format!("{frame}\n"))
        .concat();
        let decoded = registry
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/nonexistent"),
            )
            .unwrap();
        assert!(decoded.observed_route.is_none());
        assert_eq!(
            decoded.reason.as_deref(),
            Some("OpenCode session identity: provider continuation identity is unknown")
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

    #[test]
    fn claude_usage_records_cache_read_and_cache_write() {
        let route = run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/claude-sonnet-5".into()),
            ..Default::default()
        };
        let stream = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\",\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":40,\"cache_creation\":{\"ephemeral_1h_input_tokens\":25}}}\n",
        );
        let decoded = Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.usage.uncached_input, Some(10));
        assert_eq!(decoded.usage.output_excluding_reasoning, Some(20));
        assert_eq!(decoded.usage.cache_read, Some(30));
        assert_eq!(decoded.usage.cache_write, Some(40));
        assert_eq!(decoded.usage.cache_write_1h, Some(25));
        assert_eq!(decoded.usage.source.as_deref(), Some("provider"));
    }

    #[test]
    fn a_claude_success_that_hit_its_token_limit_is_flagged_not_failed() {
        let route = run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/claude-sonnet-5".into()),
            ..Default::default()
        };
        let stream = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"half\",\"stop_reason\":\"max_tokens\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}\n",
        );
        let decoded = Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.partial_signals, [run::PartialSignal::ProviderStop]);
        assert_eq!(
            decoded.reason.as_deref(),
            Some("provider stopped early: stop_reason max_tokens")
        );
        assert_ne!(decoded.outcome, run::Outcome::Failed);
    }

    #[test]
    fn codex_usage_records_cache_read_apart_from_uncached_input() {
        let route = run::RouteRecord {
            provider: Some("codex".into()),
            model: Some("codex/gpt-6.1-terra".into()),
            ..Default::default()
        };
        let stream = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"answer\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":7}}\n",
        );
        let decoded = Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.usage.uncached_input, Some(20));
        assert_eq!(decoded.usage.cache_read, Some(80));
        assert_eq!(decoded.usage.cache_write, None);
        assert_eq!(decoded.usage.output_excluding_reasoning, Some(7));
    }

    #[test]
    fn claude_absent_usage_names_its_reason() {
        let route = run::RouteRecord {
            provider: Some("claude".into()),
            model: Some("claude/claude-sonnet-5".into()),
            ..Default::default()
        };
        let stream = "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n";
        let decoded = Registry::default()
            .decode(
                &route,
                stream.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded.usage.uncached_input, None);
        assert_eq!(
            decoded.usage.source.as_deref(),
            Some("absent: no result event")
        );

        let stream_no_usage = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}\n",
            "{\"type\" : \"result\",\"subtype\":\"success\",\"is_error\":false,\"session_id\":\"s\",\"result\":\"answer\"}\n",
        );
        let decoded_no_usage = Registry::default()
            .decode(
                &route,
                stream_no_usage.as_bytes(),
                &clean_exit(),
                Path::new("/unused"),
            )
            .unwrap();
        assert_eq!(decoded_no_usage.usage.uncached_input, None);
        assert_eq!(
            decoded_no_usage.usage.source.as_deref(),
            Some("absent: result event has no usage")
        );
    }
}
