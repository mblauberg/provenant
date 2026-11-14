//! Provider checks compose routing inventory and the ordinary graph/run owner.
use crate::fabric;
use provenant_cli::{Globals, check::Providers};
use provenant_confinement::worktrees::WorkerObserver;
use provenant_core::{AttemptId, CancelToken, Clock};
use provenant_fabric_runtime::{
    coordination::facade::GraphInput,
    graph::{AttemptOutcome, FileJournal, GraphRun, NodeState},
};
use provenant_providers::host::LocalSignin;
use provenant_quota::{QuotaStatus, ReadinessReason};
use provenant_records::{CommandResult, Outcome, StopCause};
use provenant_routing::{Catalogue, ModelId};
use serde::Serialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

const PROMPT: &str = "Reply with exactly one word: OK. Do not use tools, read files, change files, run commands or perform any other work.";

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    Default,
    Set,
}
impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Set => "set",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
struct Setting {
    value: u64,
    source: Source,
}
#[derive(Clone, Copy, Debug, Serialize)]
struct Settings {
    timeout_seconds: Setting,
    concurrency: Setting,
}
impl Settings {
    fn parse(overrides: &[String]) -> Result<Self, String> {
        let mut settings = Self {
            timeout_seconds: Setting {
                value: 60,
                source: Source::Default,
            },
            concurrency: Setting {
                value: 2,
                source: Source::Default,
            },
        };
        for entry in overrides {
            let (name, value) = entry.split_once('=').ok_or("--set expects NAME=VALUE")?;
            let number = value
                .parse::<u64>()
                .map_err(|_| "--set value must be a positive integer")?;
            let (setting, maximum) = match name {
                "timeout_seconds" => (&mut settings.timeout_seconds, 14_400),
                "concurrency" => (&mut settings.concurrency, 16),
                _ => return Err(format!("unknown provider check setting: {name}")),
            };
            if number == 0 || number > maximum {
                return Err(format!("{name} must be in 1..={maximum}"));
            }
            *setting = Setting {
                value: number,
                source: Source::Set,
            };
        }
        Ok(settings)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Pass,
    Fail,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Coverage {
    Known,
    Partial,
    Unknown,
}
impl Coverage {
    fn label(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckCause {
    Completed,
    Ready,
    HarnessMissing,
    AuthRequired,
    TimedOut,
    LaunchRefused,
    SettlementFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum Cause {
    Stop(StopCause),
    Check(CheckCause),
}
#[derive(Clone, Debug, Serialize)]
struct Quota {
    coverage: Coverage,
    source: String,
    status: QuotaStatus,
}
#[derive(Clone, Debug, Serialize)]
struct Row {
    route: String,
    model: ModelId,
    account: String,
    account_key: String,
    harness_found: bool,
    signed_in: LocalSignin,
    signin_source: String,
    quota: Quota,
    pools: Vec<String>,
    status: Verdict,
    cause: Cause,
    attempt_id: Option<AttemptId>,
    graph: Option<String>,
    error_code: Option<String>,
    #[serde(skip)]
    effort: Option<String>,
}

fn inventory(roots: &provenant_core::Roots) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    let (routes, accounts) = provenant_cli::route::provider_check_routes(roots)?;
    let logins = provenant_cli::quota::recorded_login_rows(roots)?;
    let catalogue = Catalogue::bundled()?;
    let mut models: BTreeMap<_, _> = routes
        .models
        .into_iter()
        .map(|entry| (entry.model.clone(), entry.efforts))
        .collect();
    for member in routes.pools.into_values().flatten() {
        models.entry(member.model).or_insert(member.efforts);
    }
    for route in routes.named_routes.into_values() {
        models.entry(route.model).or_default();
    }
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for account in accounts.entries.into_values() {
        if !seen.insert((account.adapter.clone(), account.key.clone())) {
            continue;
        }
        let Some(registration) = provenant_providers::REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == account.adapter)
        else {
            continue;
        };
        let Some(spec) = provenant_records::AdapterSpec::get(registration.adapter.id()) else {
            continue;
        };
        let harness_found = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|path| path.is_absolute())
            .any(|path| provenant_os::sys::is_executable(&path.join(spec.binary)));
        let login = logins.iter().find(|row| {
            row.account.adapter == account.adapter && row.account.account_key == account.key
        });
        let signed_in = match login
            .and_then(|row| row.auth_state.as_ref())
            .and_then(|auth| auth.signed_in)
        {
            Some(true) => LocalSignin::SignedIn,
            Some(false) => LocalSignin::SignedOut,
            None if login.is_some_and(|row| {
                row.status == QuotaStatus::AuthRequired
                    || row.readiness.as_ref().is_some_and(|readiness| {
                        matches!(
                            readiness.reason,
                            Some(ReadinessReason::SignedOut | ReadinessReason::LoginExpired)
                        )
                    })
            }) =>
            {
                LocalSignin::SignedOut
            }
            None => LocalSignin::Unknown,
        };
        for (model, efforts) in &models {
            if model.adapter() != account.adapter
                || !spec.account_serves_model(&account.key, model.model())
            {
                continue;
            }
            let snapshot = login.and_then(|row| row.snapshot.as_ref());
            // Namespaced free accounts and subscription pools have only partial
            // allowance coverage today. Never promote a login's quota to Zen.
            let governed = spec
                .quota_model_prefix
                .is_none_or(|prefix| model.model().starts_with(prefix));
            let snapshot = snapshot.filter(|_| governed);
            let known = snapshot.is_some_and(|snapshot| {
                snapshot.windows.iter().any(|window| {
                    spec.window_serves_pool(&window.pool, catalogue.quota_pool(model))
                })
            }) && login.is_some_and(|row| {
                !matches!(
                    row.status,
                    QuotaStatus::Unknown | QuotaStatus::Stale | QuotaStatus::Unsupported
                )
            });
            let quota = Quota {
                coverage: if !known {
                    Coverage::Unknown
                } else if spec.quota_model_prefix.is_some()
                    || snapshot
                        .and_then(|snapshot| snapshot.details.as_ref())
                        .and_then(|details| details.coverage.as_deref())
                        == Some("partial")
                {
                    Coverage::Partial
                } else {
                    Coverage::Known
                },
                source: snapshot.map_or_else(
                    || "not_observed".into(),
                    |snapshot| snapshot.source_ref.clone(),
                ),
                status: if governed {
                    login.map_or(QuotaStatus::Unknown, |row| row.status)
                } else {
                    QuotaStatus::Unknown
                },
            };
            let cause = if !harness_found {
                CheckCause::HarnessMissing
            } else if signed_in == LocalSignin::SignedOut {
                CheckCause::AuthRequired
            } else {
                CheckCause::Ready
            };
            rows.push(Row {
                route: format!("{}/{}/{}", model.adapter(), account.label, model.model()),
                model: model.clone(),
                account: account.label.clone(),
                account_key: account.key.clone(),
                harness_found,
                signed_in,
                signin_source: if login.is_some_and(|row| {
                    row.auth_state
                        .as_ref()
                        .is_some_and(|auth| auth.signed_in.is_some())
                        || row
                            .readiness
                            .as_ref()
                            .is_some_and(|readiness| readiness.observed_unix_seconds.is_some())
                        || row.status == QuotaStatus::AuthRequired
                }) {
                    "recorded_login"
                } else {
                    "not_observed"
                }
                .into(),
                quota,
                pools: catalogue.admission_pools(model),
                status: if cause == CheckCause::Ready {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                },
                cause: Cause::Check(cause),
                attempt_id: None,
                graph: None,
                error_code: None,
                effort: efforts
                    .iter()
                    .find(|effort| effort.as_str() == "low")
                    .or_else(|| efforts.first())
                    .cloned(),
            });
        }
    }
    rows.sort_by(|a, b| a.route.cmp(&b.route));
    Ok(rows)
}

fn select(rows: Vec<Row>, filters: &[String]) -> Result<Vec<Row>, String> {
    for filter in filters {
        if !rows.iter().any(|row| &row.route == filter) {
            return Err(format!("unknown configured provider route: {filter}"));
        }
    }
    Ok(rows
        .into_iter()
        .filter(|row| filters.is_empty() || filters.contains(&row.route))
        .collect())
}

fn settlement(row: &mut Row, run: &GraphRun) -> bool {
    let Some(node) = run.checkpoint.nodes.first() else {
        return false;
    };
    let attempt = node.attempts.first();
    row.attempt_id = attempt
        .and_then(|attempt| attempt.attempt_ref.as_deref())
        .and_then(|reference| reference.rsplit('/').next())
        .and_then(|id| id.parse().ok());
    if let Some(attempt) = attempt.filter(|attempt| attempt.outcome.is_some()) {
        row.status = if attempt.outcome == Some(AttemptOutcome::Succeeded) && attempt.stop.is_none()
        {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        row.cause = match attempt.stop {
            Some(StopCause::Deadline) => Cause::Check(CheckCause::TimedOut),
            Some(stop) => Cause::Stop(stop),
            None => match attempt.outcome {
                Some(AttemptOutcome::Succeeded) => Cause::Check(CheckCause::Completed),
                Some(AttemptOutcome::AuthRequired) => Cause::Stop(StopCause::AuthRequired),
                Some(AttemptOutcome::UsageLimited) => Cause::Stop(StopCause::UsageLimited),
                Some(AttemptOutcome::AdapterUnavailable) => {
                    Cause::Stop(StopCause::AdapterUnavailable)
                }
                Some(AttemptOutcome::PreparationRefused(cause)) => Cause::Stop(cause.into()),
                Some(AttemptOutcome::DeadlineExceeded) => Cause::Check(CheckCause::TimedOut),
                _ => Cause::Check(CheckCause::SettlementFailed),
            },
        };
        return true;
    }
    if matches!(
        node.state,
        NodeState::Failed | NodeState::Blocked | NodeState::Skipped
    ) && row.attempt_id.is_none()
    {
        row.status = Verdict::Fail;
        row.cause = Cause::Check(CheckCause::LaunchRefused);
        row.error_code = node
            .advance_owner_code
            .clone()
            .or_else(|| node.advance_error_code.clone());
        return true;
    }
    false
}

fn live(
    row: &mut Row,
    settings: Settings,
    globals: &Globals,
    workers: &'static dyn WorkerObserver,
) -> Result<(), Box<dyn std::error::Error>> {
    let factory = fabric::factory(globals, workers)?;
    let cancel = CancelToken::new();
    let entropy = provenant_os::fs::entropy_128()?;
    let name = format!("provider-check-{:032x}", u128::from_le_bytes(entropy));
    row.graph = Some(name.clone());
    let budget =
        provenant_core::WaitBudget::cli(factory.clock.mono_now(), settings.timeout_seconds.value)?;
    let on_stop = provenant_records::OnStop::new(
        provenant_records::STOP_TABLE
            .iter()
            .map(|rule| (rule.cause, provenant_records::NextStep::Chair))
            .collect(),
    );
    // One concrete route and no continuations: the check never silently tests
    // a fallback or spends a second attempt on the same route.
    let launch_error = factory.graph_document(json!({"op":"run","document":{
        "schema_version":2,"graph_id":name,"limits":{"continuations":0,"concurrency":1},
        "nodes":[{"id":"smoke","use":"task","brief":PROMPT,"access":"read_only",
            "on_stop":on_stop,"attempt":{"provider_timeout":settings.timeout_seconds.value},
            "route":{"adapter":row.model.adapter(),"model":row.model.model(),"account":row.account_key,"effort":row.effort}}]
    }}), budget, &cancel).err();
    if let Some(error) = &launch_error {
        row.error_code = Some(error.code().to_owned());
    }
    let mut changes = FileJournal::watch_changes(
        &factory.context.roots.state,
        factory.context.identity.project,
    )?;
    // Arm before the first read: a very fast provider cannot fall between
    // sampling its terminal state and subscribing to the owner's events.
    let wait = provenant_core::WaitBudget::cli(
        factory.clock.mono_now(),
        settings
            .timeout_seconds
            .value
            .saturating_add(factory.context.config.graph.launch_wait_seconds)
            .min(14_400),
    )?;
    loop {
        let run = factory.graph(
            GraphInput::Status {
                extra: Default::default(),
                id: name.clone(),
            },
            wait,
            &cancel,
        )?;
        if settlement(row, &run) {
            return Ok(());
        }
        let remaining = Duration::from_micros(wait.remaining(factory.clock.mono_now()).as_micros());
        if !changes.wait(remaining)? {
            row.status = Verdict::Fail;
            row.cause = Cause::Check(CheckCause::SettlementFailed);
            row.error_code = Some("settlement_wait_expired".into());
            return Ok(());
        }
    }
}

#[derive(Serialize)]
struct Report {
    live: bool,
    settings: Settings,
    rows: Vec<Row>,
}
impl Report {
    fn exit(&self) -> u8 {
        u8::from(self.rows.iter().any(|row| row.status == Verdict::Fail))
    }
}

pub(crate) fn run(args: Providers, globals: &Globals, workers: &'static dyn WorkerObserver) -> u8 {
    let result = (|| -> Result<Report, Box<dyn std::error::Error>> {
        let settings = Settings::parse(&args.set)?;
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        )?;
        let mut rows = select(inventory(&resolved.roots)?, &args.routes)?;
        if args.live {
            // Factories and SQLite connections belong to their worker, never
            // shared across threads. Each route stays on the normal run path.
            let queue = std::sync::Mutex::new(rows.iter_mut());
            std::thread::scope(|scope| {
                for _ in 0..settings.concurrency.value {
                    let queue = &queue;
                    scope.spawn(move || loop {
                        let row = match queue.lock() { Ok(mut queue) => queue.next(), Err(_) => None };
                        let Some(row) = row else { break; };
                        if let Err(error) = live(row, settings, globals, workers) {
                            row.status = Verdict::Fail;
                            row.cause = Cause::Check(CheckCause::LaunchRefused);
                            row.error_code.get_or_insert_with(|| error.downcast_ref::<provenant_fabric_runtime::coordination::facade::FacadeError>().map_or("provider_check_error", |error| error.code()).to_owned());
                        }
                    });
                }
            });
        }
        Ok(Report {
            live: args.live,
            settings,
            rows,
        })
    })();
    match result {
        Err(error) => crate::fail(
            "check providers",
            Outcome::Invalid,
            "PROVIDER_CHECK",
            &error.to_string(),
            globals,
        ),
        Ok(report) => {
            let exit = report.exit();
            let mut human = format!(
                "timeout_seconds={} ({}) concurrency={} ({})\n",
                report.settings.timeout_seconds.value,
                report.settings.timeout_seconds.source.label(),
                report.settings.concurrency.value,
                report.settings.concurrency.source.label()
            );
            for row in &report.rows {
                let cause = serde_json::to_value(row.cause)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "settlement_failed".into());
                human += &format!(
                    "{} {} {} attempt={} harness={} signed_in={} signin_source={} quota={} quota_source={} pools={}\n",
                    row.route,
                    if row.status == Verdict::Pass {
                        "pass"
                    } else {
                        "fail"
                    },
                    cause,
                    row.attempt_id
                        .map_or_else(|| "-".into(), |id| id.to_string()),
                    row.harness_found,
                    match row.signed_in {
                        LocalSignin::SignedIn => "signed_in",
                        LocalSignin::SignedOut => "signed_out",
                        LocalSignin::Unknown => "unknown",
                    },
                    row.signin_source,
                    row.quota.coverage.label(),
                    row.quota.source,
                    row.pools.join(",")
                );
            }
            crate::emit(
                &CommandResult::new(
                    "check providers",
                    if exit == 0 {
                        Outcome::Ok
                    } else {
                        Outcome::Failed
                    },
                    report,
                ),
                globals,
                &human,
                !args.live,
            )
        }
    }
}
