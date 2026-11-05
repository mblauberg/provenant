//! Routing's frozen clap leaves and read-only composition/rendering edge.
use crate::{Globals, metadata::CatalogueArg};
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{Clock, Roots, WallTime};
use provenant_os::clock::SystemClock;
use provenant_os::fs::FsError;
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{CommandError, CommandResult, Outcome, RouteReadiness};
use provenant_routing::{
    AccessContext, BoundedModels, Catalogue, Fallback, ModelId, PacingPolicy, QuotaObservation,
    RouteChoice, RouteName, RoutePoolMember, RoutingContext, RoutingError,
};
use serde::Serialize;
use std::{collections::BTreeSet, io, str::FromStr};

#[derive(Debug, Subcommand)]
pub enum Route {
    /// Show the catalogue, pools, configured weights and pacing policy.
    List(RouteListRequest),
    /// Resolve a named route or exact model for preview.
    Resolve(RouteResolve),
    /// Add or update a model in a user-configured pool.
    Set(RouteSet),
    /// Remove a model from a user-configured pool.
    Remove(RouteRemove),
    /// Restore one bundled pool and clear its override.
    Reset(RouteReset),
    /// Set a provider's default pacing policy.
    Pacing(RoutePacing),
    /// Record, list or clear scheduled pacing overrides; the list also shows
    /// scheduled model policy rules.
    Override(RouteOverride),
    /// Manage persistent exact-model exclusions from pools.
    Exclude(RouteExclude),
}
#[derive(Debug, Args)]
pub struct RouteExclude {
    #[command(subcommand)]
    pub command: RouteExcludeCommand,
}
#[derive(Debug, Subcommand)]
pub enum RouteExcludeCommand {
    /// Exclude a model until removed or expired.
    Add(ExcludeAdd),
    /// Remove an exact-model exclusion.
    Remove(ExcludeRemove),
    /// List exact-model exclusions.
    List,
}
#[derive(Debug, Args)]
pub struct ExcludeAdd {
    /// Exact model in ADAPTER/ID form.
    pub model: Model,
    /// Future RFC 3339 timestamp or duration, such as 2h.
    #[arg(long)]
    pub until: Option<String>,
}
#[derive(Debug, Args)]
pub struct ExcludeRemove {
    /// Exact model in ADAPTER/ID form.
    pub model: Model,
}
#[derive(Debug, Args)]
pub struct RouteOverride {
    #[command(subcommand)]
    pub command: RouteOverrideCommand,
}
#[derive(Debug, Subcommand)]
pub enum RouteOverrideCommand {
    /// Record a pacing override for one scope and echo the recorded rule.
    Set(OverrideSet),
    /// List recorded schedules, their current activity and remaining time.
    List,
    /// Remove pacing overrides: one scope's, or every one without a selector.
    Clear(OverrideClear),
}
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("scope").required(true).args(["account", "provider", "all"])))]
pub struct OverrideSet {
    /// Apply the override to this account (label, entry or adapter/key).
    #[arg(long)]
    pub account: Option<String>,
    /// Apply the override to this provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Apply the override to all accounts.
    #[arg(long)]
    pub all: bool,
    /// Pacing policy while the override is active.
    #[arg(long)]
    pub policy: PacingChoice,
    /// First active instant: an RFC 3339 timestamp or a duration from now (e.g. 30m).
    #[arg(long)]
    pub starts: Option<String>,
    /// Expiry: a future RFC 3339 timestamp or a duration from now (e.g. 2h, 1d).
    /// Without it the override stays until cleared.
    #[arg(long)]
    pub until: Option<String>,
    /// Hour window as [DAYS@]START-END, repeatable; DAYS lists weekdays
    /// (mon..sun or 0..6, Monday = 0) or ranges, such as mon-fri@9-17 or 23-2.
    #[arg(long, value_name = "WINDOW")]
    pub window: Vec<String>,
    /// IANA time zone for the windows. Missing means UTC.
    #[arg(long)]
    pub tz: Option<String>,
}
#[derive(Debug, Args)]
pub struct OverrideClear {
    /// Clear only this account's override.
    #[arg(long, conflicts_with = "provider")]
    pub account: Option<String>,
    /// Clear only this provider's override.
    #[arg(long, conflicts_with = "account")]
    pub provider: Option<String>,
}
#[derive(Debug, Args)]
pub struct RouteListRequest {
    /// List catalogued model/effort pairs accepted by the resolver (no live probe).
    #[arg(long)]
    pub valid: bool,
    /// Limit models, named routes and pool members to this adapter.
    #[arg(long)]
    pub adapter: Option<String>,
    /// Show only this effective pool.
    #[arg(long)]
    pub pool: Option<String>,
}
#[derive(Debug, Args)]
pub struct RouteSet {
    /// Pool name to update or create.
    pub pool: String,
    /// Exact catalogue model in ADAPTER/ID form.
    pub model: Model,
    /// Selection weight from 0 (fallback only) to 100.
    #[arg(long, catalog_u8_range = 0..=100)]
    pub weight: Option<u8>,
    /// Default effort for this member.
    #[arg(long)]
    pub effort: Option<String>,
    /// Allowed caller effort values, comma separated.
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Set)]
    pub efforts: Vec<String>,
}
#[derive(Debug, Args)]
pub struct RouteRemove {
    /// Pool name to update.
    pub pool: String,
    /// Exact catalogue model to remove.
    pub model: Model,
}
#[derive(Debug, Args)]
pub struct RouteReset {
    /// Bundled pool whose user override should be cleared.
    pub pool: String,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PacingChoice {
    Pace,
    Spend,
    Max,
    Off,
}
impl From<PacingChoice> for PacingPolicy {
    fn from(value: PacingChoice) -> Self {
        match value {
            PacingChoice::Pace => Self::Pace,
            PacingChoice::Spend => Self::Spend,
            PacingChoice::Max => Self::Max,
            PacingChoice::Off => Self::Off,
        }
    }
}
#[derive(Debug, Args)]
pub struct RoutePacing {
    /// Registered provider ID.
    pub provider: String,
    /// Default pacing policy for the provider.
    pub policy: PacingChoice,
}
#[derive(Debug, Args)]
pub struct RouteResolve {
    /// Preview a route that needs write access.
    #[arg(long)]
    pub writes: bool,
    /// Account key for policy preview.
    #[arg(long)]
    pub account: Option<String>,
    /// Route name, ADAPTER/ID model, or a model ID given without its adapter
    #[arg(conflicts_with_all = ["model", "route"])]
    pub name: Option<String>,
    #[command(flatten)]
    pub request: RouteRequest,
}
#[derive(Debug, Clone, Args)]
pub struct RouteRequest {
    /// Optimise selection for read-heavy work.
    #[arg(long)]
    pub read_heavy: bool,
    /// Exact model in ADAPTER/ID form
    #[arg(long, conflicts_with = "route")]
    pub model: Option<Model>,
    /// Named route, or ADAPTER/NAME for a model-less adapter such as command/fmt-check
    #[arg(long)]
    pub route: Option<String>,
    /// Reasoning effort
    #[arg(long)]
    pub effort: Option<String>,
    /// Require a confidential-safe route
    #[arg(long)]
    pub confidential: bool,
    /// Fallback models in ADAPTER/ID form, comma separated
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Set, catalog_bounds = (0, 2))]
    pub fallback: Vec<Model>,
    /// Keep a provider (claude), account (label, entry or ADAPTER/KEY) or
    /// exact model (ADAPTER/ID) out of a named route's pool (repeatable).
    /// A pinned --model is never affected.
    #[arg(long = "exclude", value_name = "SELECTOR")]
    pub exclude: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);
impl FromStr for Model {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ModelId::parse(text).map_err(|_| model_form_message(text))?;
        Ok(Self(text.into()))
    }
}
/// A bare word given as `--model` is usually a route name, such as a tier, so
/// the refusal names the flag it probably meant.
pub(crate) fn model_form_message(text: &str) -> String {
    if text.contains('/') {
        "model must be ADAPTER/ID".into()
    } else {
        format!("model must be ADAPTER/ID; did you mean --route {text}?")
    }
}

impl RouteRequest {
    pub fn to_domain(&self) -> Result<provenant_routing::RouteRequest, RoutingError> {
        let choice = match (&self.model, &self.route) {
            (Some(model), None) => RouteChoice::Model(ModelId::parse(&model.0)?),
            (None, Some(name)) => RouteChoice::NamedRoute(RouteName::parse(name)?),
            _ => return Err(RoutingError::InvalidValue),
        };
        let fallback = if self.fallback.is_empty() {
            Fallback::None
        } else {
            Fallback::Models(BoundedModels::new(
                self.fallback
                    .iter()
                    .map(|m| ModelId::parse(&m.0))
                    .collect::<Result<_, _>>()?,
            )?)
        };
        Ok(provenant_routing::RouteRequest {
            intent: provenant_routing::RouteIntent {
                workload: if self.read_heavy {
                    provenant_routing::Workload::ReadHeavy
                } else {
                    provenant_routing::Workload::General
                },
                writes: false,
                ..Default::default()
            },
            choice,
            effort: self.effort.clone(),
            confidential: self.confidential,
            fallback,
        })
    }
}
/// Parse `--exclude` selectors into routing rules, refusing an unknown one
/// with the valid forms. Accounts resolve like `route override --account`.
pub(crate) fn exclusion_rules(
    texts: &[String],
    source: provenant_routing::ExclusionSource,
    roots: &Roots,
) -> Result<Vec<provenant_routing::ExclusionRule>, EdgeError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let models: Vec<ModelId> = Catalogue::bundled()?
        .models()
        .iter()
        .map(|entry| entry.model.clone())
        .collect();
    let providers: Vec<&str> = REGISTRY
        .iter()
        .map(|provider| provider.adapter.id())
        .collect();
    texts
        .iter()
        .map(|text| {
            let selector =
                provenant_routing::ExclusionSelector::parse(text, &providers, &models, |name| {
                    match resolve_account_reference(name, "--exclude", &settings) {
                        Ok(key) => Ok(key
                            .split_once('/')
                            .map(|(adapter, key)| (adapter.to_owned(), key.to_owned()))),
                        Err(EdgeError::AccountProfile {
                            kind: AccountProfileError::Unknown,
                            ..
                        }) => Ok(None),
                        Err(error) => Err(error.to_string()),
                    }
                })?;
            Ok::<_, EdgeError>(provenant_routing::ExclusionRule { selector, source })
        })
        .collect()
}
/// The exclusions a task carries: the standing `routing.exclude` list from the
/// configuration, the dispatch's `--exclude` flags and the graph's list, as a
/// union. Each rule keeps the source it came from.
pub(crate) fn task_exclusion_rules(
    profile: &provenant_records::run::HarnessProfile,
    roots: &Roots,
) -> Result<Vec<provenant_routing::ExclusionRule>, EdgeError> {
    let texts = |knob: &str| -> Vec<String> {
        profile
            .knobs
            .get(knob)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    // A list set with `--knob routing.exclude=...` was chosen for this dispatch.
    let standing = if profile
        .overrides
        .iter()
        .any(|entry| entry.starts_with("routing.exclude="))
    {
        provenant_routing::ExclusionSource::Flag
    } else {
        provenant_routing::ExclusionSource::Config
    };
    let mut rules = Vec::new();
    for (knob, source) in [
        ("routing.exclude", standing),
        (
            "routing.flag_exclude",
            provenant_routing::ExclusionSource::Flag,
        ),
        (
            "routing.graph_exclude",
            provenant_routing::ExclusionSource::Graph,
        ),
    ] {
        for rule in exclusion_rules(&texts(knob), source, roots)? {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
    }
    Ok(rules)
}
/// The rules a dispatch with these `--exclude` flags would apply, through the
/// same profile and reader as the dispatch itself, so `--explain` and
/// `route resolve` show what the dispatch would do.
pub(crate) fn effective_exclusion_rules(
    flags: &[String],
    roots: &Roots,
) -> Result<Vec<provenant_routing::ExclusionRule>, EdgeError> {
    let overrides: Vec<String> = if flags.is_empty() {
        Vec::new()
    } else {
        vec![format!(
            "routing.flag_exclude={}",
            serde_json::Value::from(flags.to_vec())
        )]
    };
    let profile = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .dispatch_profile(&overrides)?;
    task_exclusion_rules(&profile, roots)
}
/// Refuse a selector that is none of the valid forms, for callers that carry
/// the list to a later dispatch.
pub fn validate_exclusions(texts: &[String], roots: &Roots) -> Result<(), String> {
    exclusion_rules(texts, provenant_routing::ExclusionSource::Flag, roots)
        .map(|_| ())
        .map_err(|error| error.to_string())
}
/// What a `route resolve` name stands for.
#[derive(Debug, PartialEq, Eq)]
enum Named {
    Route(String),
    Model(String),
}
/// Resolve a name to a route or model. A route name wins, then an exact model
/// (anything with an adapter, which resolve itself judges), then the one
/// catalogue model whose ID is, or contains, the name. A miss lists the close
/// matches.
fn match_name(
    name: &str,
    routes: &BTreeSet<String>,
    models: &[String],
) -> Result<Named, Vec<String>> {
    if routes.contains(name) {
        return Ok(Named::Route(name.to_owned()));
    }
    if name.contains('/') {
        return Ok(Named::Model(name.to_owned()));
    }
    let lower = name.to_ascii_lowercase();
    let id = |model: &str| {
        model
            .split_once('/')
            .map_or(model, |(_, id)| id)
            .to_ascii_lowercase()
    };
    let exact: Vec<&String> = models.iter().filter(|model| id(model) == lower).collect();
    if let [only] = exact[..] {
        return Ok(Named::Model(only.clone()));
    }
    if exact.is_empty() {
        let mut containing: Vec<Named> = routes
            .iter()
            .filter(|route| route.to_ascii_lowercase().contains(&lower))
            .map(|route| Named::Route(route.clone()))
            .chain(
                models
                    .iter()
                    .filter(|model| id(model).contains(&lower))
                    .map(|model| Named::Model(model.clone())),
            )
            .collect();
        if containing.len() == 1 {
            return Ok(containing.remove(0));
        }
    }
    Err(close_matches(&lower, routes, models))
}
/// Up to eight routes and models that share the name as a substring or a
/// prefix, or are one or two edits away.
fn close_matches(lower: &str, routes: &BTreeSet<String>, models: &[String]) -> Vec<String> {
    let near = |candidate: &str| {
        let candidate = candidate.to_ascii_lowercase();
        let id = candidate.split_once('/').map_or(&*candidate, |(_, id)| id);
        let prefix = lower
            .chars()
            .zip(id.chars())
            .take_while(|(a, b)| a == b)
            .count();
        id.contains(lower)
            || lower.contains(id)
            || prefix >= 3
            || crate::edit_distance(lower, id) <= 2
            || crate::edit_distance(lower, &candidate) <= 2
    };
    routes
        .iter()
        .chain(models)
        .filter(|candidate| near(candidate))
        .take(8)
        .cloned()
        .collect()
}
fn close(matches: &[String]) -> String {
    if matches.is_empty() {
        String::new()
    } else {
        format!(" (close matches: {})", matches.join(", "))
    }
}
fn example<'a>(found: &'a Option<String>, fallback: &'a str) -> &'a str {
    found.as_deref().unwrap_or(fallback)
}
/// The request the positional name or the flags stand for.
fn resolve_request(
    query: &RouteResolve,
    context: &RoutingContext<'_>,
) -> Result<provenant_routing::RouteRequest, EdgeError> {
    let listed = provenant_routing::list(&provenant_routing::RouteListQuery::default(), context)?;
    let routes: BTreeSet<String> = listed
        .named_routes
        .keys()
        .chain(listed.pools.keys())
        .cloned()
        .collect();
    let models: Vec<String> = listed
        .models
        .iter()
        .map(|entry| entry.model.as_str().to_owned())
        .collect();
    let unresolved = |name: &str, matches: Vec<String>| EdgeError::Unresolved {
        name: name.to_owned(),
        matches,
        example_route: routes.iter().next().cloned(),
        example_model: models.first().cloned(),
    };
    let mut request = query.request.clone();
    if let Some(name) = &query.name {
        match match_name(name, &routes, &models).map_err(|close| unresolved(name, close))? {
            Named::Route(route) => request.route = Some(route),
            Named::Model(model) => {
                request.model =
                    Some(Model::from_str(&model).map_err(|_| unresolved(name, Vec::new()))?)
            }
        }
    } else if let Some(route) = request.route.as_deref()
        && !routes.contains(route)
    {
        return Err(unresolved(
            route,
            close_matches(&route.to_ascii_lowercase(), &routes, &models),
        ));
    }
    let mut request = request.to_domain()?;
    request.intent.writes = query.writes;
    Ok(request)
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum EdgeError {
    #[error(
        "no route or model matches `{name}`{}; try `provenant route resolve {}`, \
         `provenant route resolve {}` or `provenant route list`",
        close(.matches),
        example(.example_route, "utility"),
        example(.example_model, "ADAPTER/ID"),
    )]
    Unresolved {
        name: String,
        matches: Vec<String>,
        example_route: Option<String>,
        example_model: Option<String>,
    },
    #[error(transparent)]
    Config(provenant_config::ConfigError),
    #[error(
        "{0}; valid pacing keys: accounts, free or a registered provider; profile keys: policy, windows, reserve, deadline, after_deadline; policies: spend, pace, max, off; windows: five_hour, seven_day, monthly, rolling; deadline table keys: reset_of, window; after_deadline: standby, continue"
    )]
    PacingConfig(#[source] provenant_config::ConfigError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error("invalid current time")]
    Time(#[from] provenant_core::TimeError),
    #[error(transparent)]
    QuotaCache(#[from] crate::quota::EdgeError),
    #[error(transparent)]
    Quota(#[from] provenant_quota::QuotaError),
    #[error("{0}")]
    Pacing(&'static str),
    #[error("{0}")]
    GoAllowance(String),
    #[error("{message}")]
    AccountProfile {
        kind: AccountProfileError,
        message: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountProfileError {
    Unknown,
    Ambiguous,
    Duplicate,
}
impl From<provenant_config::ConfigError> for EdgeError {
    fn from(error: provenant_config::ConfigError) -> Self {
        let is_pacing = match &error {
            provenant_config::ConfigError::Schema { location, .. }
            | provenant_config::ConfigError::Domain { location, .. } => {
                location.key_path == "domains.routing.pacing"
                    || location.key_path.starts_with("domains.routing.pacing.")
            }
            _ => false,
        };
        if is_pacing {
            Self::PacingConfig(error)
        } else {
            Self::Config(error)
        }
    }
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum Data {
    List(Box<RouteListDisplay>),
    Resolved(Box<RoutePreview>),
    Pacing(Box<PacingReport>),
    Overrides(Box<OverrideReport>),
}
#[derive(Serialize)]
pub struct RouteListDisplay {
    #[serde(flatten)]
    pub routes: Box<provenant_routing::RouteList>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recorded_readiness: Vec<RecordedReadiness>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub readiness_truncated: bool,
}

impl std::ops::Deref for RouteListDisplay {
    type Target = provenant_routing::RouteList;
    fn deref(&self) -> &Self::Target {
        &self.routes
    }
}

/// What a `route override` command recorded or cleared, and every scheduled
/// override afterwards. Fields are always present; `recorded` is null and
/// `cleared` empty when the command did neither.
#[derive(Serialize)]
pub struct OverrideReport {
    /// Persisted exclusion rows that were repaired or ignored on reading.
    pub notes: Vec<String>,
    pub recorded: Option<OverrideRow>,
    pub cleared: Vec<OverrideRow>,
    pub overrides: Vec<OverrideRow>,
}
#[derive(Serialize)]
pub struct OverrideRow {
    /// `pacing` or `model_policy`.
    pub kind: &'static str,
    pub scope: String,
    /// The pacing policy, or the model policy rule's index.
    pub value: String,
    pub schedule: provenant_routing::Schedule,
    pub active: bool,
    /// Seconds until expiry, zero once expired, null without an expiry.
    pub remaining_seconds: Option<i64>,
}
impl OverrideRow {
    fn model_exclusion(row: &provenant_routing::ModelExclusion, now: WallTime) -> Self {
        Self {
            kind: "model_exclusion",
            scope: format!("model {}", row.model.as_str()),
            value: "excluded".into(),
            schedule: row.schedule.clone(),
            active: row.schedule.active(now),
            remaining_seconds: row.schedule.remaining(now),
        }
    }
    fn pacing(row: &provenant_routing::PacingOverride, now: WallTime) -> Self {
        Self {
            kind: "pacing",
            scope: match (&row.account, &row.provider) {
                (Some(account), _) => format!("account {account}"),
                (None, Some(provider)) => format!("provider {provider}"),
                (None, None) => "all accounts".to_owned(),
            },
            value: policy_name(row.policy).into(),
            schedule: row.schedule.clone(),
            active: row.schedule.active(now),
            remaining_seconds: row.schedule.remaining(now),
        }
    }
    fn line(&self) -> String {
        let expiry = match (self.schedule.expires, self.remaining_seconds) {
            (Some(expiry), Some(remaining)) => {
                format!("until {expiry} ({remaining}s remaining)")
            }
            _ => "no expiry".to_owned(),
        };
        let starts = self
            .schedule
            .starts
            .map(|starts| format!(" from {starts}"))
            .unwrap_or_default();
        format!(
            "{} {} {} {}{starts} {expiry}; {}",
            self.kind,
            self.scope,
            self.value,
            if self.active { "active" } else { "inactive" },
            schedule_windows(&self.schedule)
        )
    }
}
/// What `route pacing` recorded and the pacing overrides now in force.
#[derive(Serialize)]
pub struct PacingReport {
    pub recorded: String,
    pub active_overrides: Vec<provenant_routing::PacingOverride>,
}
impl PacingReport {
    fn human(&self, now: WallTime) -> String {
        let mut lines = vec![
            format!("recorded: {}", self.recorded),
            format!("active overrides: {}", self.active_overrides.len()),
        ];
        lines.extend(
            self.active_overrides
                .iter()
                .map(|row| OverrideRow::pacing(row, now).line()),
        );
        format!("{}\n", lines.join("\n"))
    }
}

fn policy_name(policy: PacingPolicy) -> &'static str {
    match policy {
        PacingPolicy::Pace => "pace",
        PacingPolicy::Spend => "spend",
        PacingPolicy::Max => "max",
        PacingPolicy::Off => "off",
    }
}
fn schedule_windows(schedule: &provenant_routing::Schedule) -> String {
    if schedule.windows.is_empty() {
        return "all hours".into();
    }
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    schedule
        .windows
        .iter()
        .map(|window| {
            let days = if window.weekdays.is_empty() {
                "every day".to_owned()
            } else {
                window
                    .weekdays
                    .iter()
                    .map(|day| DAYS.get(usize::from(*day)).copied().unwrap_or("?"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            format!(
                "{days} {:02}-{:02} {}",
                window.start_hour,
                window.end_hour,
                schedule.tz.as_deref().unwrap_or("UTC")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}
/// Parse `[DAYS@]START-END`, where DAYS is a comma list of weekday names,
/// Monday-based indexes or ranges of either.
fn parse_window(spec: &str) -> Option<provenant_routing::ScheduleWindow> {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let day = |text: &str| -> Option<u8> {
        let text = text.trim().to_ascii_lowercase();
        match text.parse::<u8>() {
            Ok(index) => (index <= 6).then_some(index),
            Err(_) => DAYS
                .iter()
                .position(|name| *name == text)
                .and_then(|index| u8::try_from(index).ok()),
        }
    };
    let (days, hours) = spec
        .rsplit_once('@')
        .map_or((None, spec), |(d, h)| (Some(d), h));
    let mut weekdays = Vec::new();
    for item in days.into_iter().flat_map(|days| days.split(',')) {
        let (first, last) = item.split_once('-').unwrap_or((item, item));
        let (first, last) = (day(first)?, day(last)?);
        if first > last {
            return None;
        }
        for index in first..=last {
            if !weekdays.contains(&index) {
                weekdays.push(index);
            }
        }
    }
    let (start, end) = hours.split_once('-')?;
    Some(provenant_routing::ScheduleWindow {
        weekdays,
        start_hour: start.trim().parse().ok()?,
        end_hour: end.trim().parse().ok()?,
    })
}
/// The inline TOML form of a pacing override, with only the keys it sets.
fn override_value(row: &provenant_routing::PacingOverride) -> toml_edit::Value {
    let mut table = toml_edit::InlineTable::new();
    if let Some(account) = &row.account {
        table.insert("account", account.as_str().into());
    }
    if let Some(provider) = &row.provider {
        table.insert("provider", provider.as_str().into());
    }
    table.insert("policy", policy_name(row.policy).into());
    let mut schedule = toml_edit::InlineTable::new();
    if let Some(starts) = row.schedule.starts {
        schedule.insert("starts", starts.to_string().into());
    }
    if let Some(expires) = row.schedule.expires {
        schedule.insert("expires", expires.to_string().into());
    }
    if let Some(tz) = &row.schedule.tz {
        schedule.insert("tz", tz.as_str().into());
    }
    if !row.schedule.windows.is_empty() {
        let mut windows = toml_edit::Array::new();
        for window in &row.schedule.windows {
            let mut value = toml_edit::InlineTable::new();
            if !window.weekdays.is_empty() {
                value.insert(
                    "weekdays",
                    window
                        .weekdays
                        .iter()
                        .map(|day| i64::from(*day))
                        .collect::<toml_edit::Array>()
                        .into(),
                );
            }
            value.insert("start_hour", i64::from(window.start_hour).into());
            value.insert("end_hour", i64::from(window.end_hour).into());
            windows.push(value);
        }
        schedule.insert("windows", windows.into());
    }
    if !schedule.is_empty() {
        table.insert("schedule", schedule.into());
    }
    table.into()
}
#[derive(Serialize)]
pub struct RoutePreview {
    pub allow_unknown_partial_headroom: provenant_config::KnobValue,
    /// Persisted exclusion rows that were repaired or ignored on reading.
    pub exclusion_notes: Vec<String>,
    pub model_policy: provenant_routing::EffectiveModelPolicy,
    pub compaction_break_even_turns: Option<f64>,
    #[serde(flatten)]
    pub route: provenant_records::ResolvedRoute,
    pub supported_efforts: Option<Vec<String>>,
    pub nearest_models: Vec<String>,
    pub pacing: Vec<provenant_routing::PacingPreview>,
    /// Recorded model-scoped evidence, separate from live executable availability.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recorded_readiness: Vec<RecordedReadiness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<provenant_quota::QuotaSnapshot>,
}
#[derive(Serialize)]
pub struct RecordedReadiness {
    pub host: String,
    pub account: String,
    pub account_key: String,
    pub model: String,
    pub observation: provenant_quota::ReadinessRead,
}

pub(crate) fn recorded_model_readiness(
    settings: &provenant_providers::accounts::Settings,
    model: &ModelId,
    pools: &[String],
    selected: Option<&str>,
    hint: Option<&str>,
    lookup: impl Fn(&provenant_quota::ReadinessScope) -> provenant_quota::ReadinessRead,
) -> Vec<RecordedReadiness> {
    let mut seen = BTreeSet::new();
    settings
        .with_derived_accounts()
        .entries
        .into_iter()
        .filter_map(|(id, entry)| {
            if entry.adapter != model.adapter()
                || selected.is_some_and(|key| key != entry.key)
                || hint.is_some_and(|hint| {
                    hint != id
                        && hint != entry.label
                        && hint != entry.key
                        && hint != format!("{}/{}", entry.adapter, entry.key)
                })
                || !seen.insert((entry.adapter.clone(), entry.key.clone()))
                || provenant_records::AdapterSpec::get(&entry.adapter)
                    .is_some_and(|spec| !spec.account_serves_model(&entry.key, model.model()))
            {
                return None;
            }
            let mut scope = provenant_quota::ReadinessScope {
                host: provenant_quota::LOCAL_HOST.into(),
                adapter: entry.adapter,
                account_key: entry.key.clone(),
                route: model.as_str().into(),
            };
            let mut observation = lookup(&scope);
            if observation.state != provenant_quota::ReadinessState::Unavailable {
                for pool in pools {
                    scope.route.clone_from(pool);
                    let pool_observation = lookup(&scope);
                    if pool_observation.state == provenant_quota::ReadinessState::Unavailable {
                        observation = pool_observation;
                        break;
                    }
                }
            }
            Some(RecordedReadiness {
                host: scope.host.clone(),
                account: entry.label,
                account_key: entry.key,
                model: model.as_str().into(),
                observation,
            })
        })
        .collect()
}

/// The OpenCode snapshot reports the Go plan's pools, which only govern models
/// served by the `opencode-go` provider. Free (Zen) and other configured
/// providers must not inherit its account-level exhaustion.
fn snapshot_governs(adapter: &str, account: &str, model: &ModelId) -> bool {
    provenant_records::AdapterSpec::get(adapter).is_none_or(|spec| {
        if let Some((free_account, prefix)) = spec.free_account
            && account == free_account
        {
            return model.model().starts_with(prefix);
        }
        spec.quota_model_prefix
            .is_none_or(|prefix| model.model().starts_with(prefix))
    })
}
fn quota_observations(
    catalogue: &Catalogue,
    availability: &[AvailabilityRecord],
    records: &[provenant_quota::AccountRecord],
    policies: &[provenant_quota::QuotaPolicy],
    requested_models: &[ModelId],
) -> Result<Vec<QuotaObservation>, EdgeError> {
    let mut observations = Vec::new();
    for record in records {
        let adapter = record.account.adapter.as_str();
        let Some(snapshot) = record.snapshot.as_ref() else {
            continue;
        };
        let Some(ttl) = provenant_quota::schedule::interval(
            policies,
            adapter,
            provenant_quota::schedule::Trigger::Timer,
            provenant_quota::schedule::Power::Ac,
        ) else {
            continue;
        };
        let expires_unix_seconds = snapshot
            .observed_unix_seconds
            .checked_add(i64::try_from(ttl).map_err(|_| provenant_quota::QuotaError::TimeRange)?)
            .ok_or(provenant_quota::QuotaError::TimeRange)?;
        let mut models: BTreeSet<ModelId> = catalogue
            .models()
            .iter()
            .filter(|entry| entry.model.adapter() == adapter)
            .map(|entry| entry.model.clone())
            .collect();
        for observed_model in availability
            .iter()
            .filter(|a| {
                a.adapter == adapter
                    && a.account_key.as_deref() == Some(record.account.account_key.as_str())
            })
            .flat_map(|a| &a.observed_models)
        {
            models.insert(ModelId::parse(&format!("{}/{}", adapter, observed_model))?);
        }
        models.extend(
            requested_models
                .iter()
                .filter(|model| model.adapter() == adapter)
                .cloned(),
        );
        models.retain(|model| snapshot_governs(adapter, &record.account.account_key, model));
        if observations.len().saturating_add(models.len()) > 256 {
            return Err(provenant_quota::QuotaError::Invalid("quota observation bound").into());
        }
        for model in models {
            observations.push(QuotaObservation {
                model,
                snapshot: snapshot.clone(),
                expires_unix_seconds,
            });
        }
    }
    Ok(observations)
}
fn resolve_account_reference(
    name: &str,
    field: &str,
    settings: &provenant_providers::accounts::Settings,
) -> Result<String, EdgeError> {
    let derived = settings.with_derived_accounts();
    let valid = derived
        .entries
        .iter()
        .map(|(id, entry)| {
            format!(
                "{} (adapter/key: {}/{}, entry: {})",
                entry.label, entry.adapter, entry.key, id
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let matches_for = |settings: &provenant_providers::accounts::Settings| {
        settings
            .entries
            .iter()
            .filter(|(id, entry)| {
                name == format!("{}/{}", entry.adapter, entry.key)
                    || *id == name
                    || entry.label == name
                    || entry.key == name
            })
            .map(|(id, entry)| (id.clone(), entry.clone()))
            .collect::<Vec<_>>()
    };
    let mut matches = matches_for(settings);
    if matches.is_empty() {
        matches = matches_for(&derived);
    }
    match matches.as_slice() {
        [(_, entry)] => Ok(format!("{}/{}", entry.adapter, entry.key)),
        [] => Err(EdgeError::AccountProfile {
            kind: AccountProfileError::Unknown,
            message: format!(
                "{field}: unknown account `{name}`; valid accounts: {}",
                if valid.is_empty() {
                    "(none registered)"
                } else {
                    &valid
                }
            ),
        }),
        _ => Err(EdgeError::AccountProfile {
            kind: AccountProfileError::Ambiguous,
            message: format!(
                "{field}: account `{name}` is ambiguous: {}; use adapter/key or a unique label",
                matches
                    .iter()
                    .map(|(id, entry)| format!(
                        "{}/{} (label: {}, entry: {})",
                        entry.adapter, entry.key, entry.label, id
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

// Resolve configured labels once, so all routing paths use the same account keys.
fn resolve_pacing_accounts(
    config: &mut provenant_routing::RoutingConfig,
    settings: &provenant_providers::accounts::Settings,
) -> Result<(), EdgeError> {
    let resolve = |name: &str, field: &str| resolve_account_reference(name, field, settings);
    let mut accounts = std::collections::BTreeMap::new();
    for (name, mut profile) in std::mem::take(&mut config.pacing.accounts) {
        let key = resolve(&name, &format!("pacing.accounts.{name}"))?;
        if let Some(provenant_routing::PacingDeadline::ResetOf(reference)) = &mut profile.deadline {
            reference.reset_of = resolve(
                &reference.reset_of,
                &format!("pacing.accounts.{name}.deadline.reset_of"),
            )?;
        }
        if accounts.insert(key.clone(), profile).is_some() {
            return Err(EdgeError::AccountProfile {
                kind: AccountProfileError::Duplicate,
                message: format!("multiple pacing profiles resolve to account key `{key}`"),
            });
        }
    }
    config.pacing.accounts = accounts;
    for (provider, profile) in &mut config.pacing.free {
        if let Some(provenant_routing::PacingDeadline::ResetOf(reference)) = &mut profile.deadline {
            reference.reset_of = resolve(
                &reference.reset_of,
                &format!("pacing.free.{provider}.deadline.reset_of"),
            )?;
        }
    }
    for (index, override_) in config.pacing_overrides.iter_mut().enumerate() {
        if let Some(account) = &mut override_.account {
            *account = resolve(account, &format!("pacing_overrides[{index}].account"))?;
        }
    }
    Ok(())
}
fn with_context<T>(
    roots: &Roots,
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    requested_models: &[ModelId],
    revalidated_auth: bool,
    action: impl FnOnce(
        &RoutingContext<'_>,
        &[provenant_quota::AccountRecord],
        &[provenant_quota::QuotaPolicy],
        &provenant_providers::accounts::Settings,
    ) -> Result<T, EdgeError>,
) -> Result<T, EdgeError> {
    let resolved_config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?;
    let partial_headroom_source = resolved_config
        .knobs
        .get("routing.allow_unknown_partial_headroom")
        .cloned()
        .ok_or(RoutingError::InvalidValue)?;
    let allow_unknown = partial_headroom_source
        .value
        .as_bool()
        .ok_or(RoutingError::InvalidValue)?;
    let domains = resolved_config.domains;
    let policies = provenant_providers::scheduling_policies(&domains.quota, REGISTRY);
    let mut config = domains.routing;
    config.allow_unknown_partial_headroom = allow_unknown;
    config.partial_headroom_source = Some(partial_headroom_source);
    resolve_pacing_accounts(&mut config, &domains.accounts)?;
    let catalogue = Catalogue::bundled()?;
    // A lane holds no grant on the shared quota store: route without a
    // snapshot there rather than refusing the nested dispatch.
    let mut quota_records = match crate::quota::read_cache(roots) {
        Ok(records) => records,
        Err(crate::quota::EdgeError::Fs(error))
            if error.is_permission_denied()
                && std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() =>
        {
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    // Cached accounts also supply reset_of references outside the probed route.
    // Quota facts never grant executable or account readiness.
    let timestamp = SystemClock::new().wall_now()?;
    let timestamp = timestamp.as_timestamp();
    domains
        .quota
        .apply_calibration(&mut quota_records, timestamp.as_second());
    // Fresh login evidence can replace older sign-out or expiry evidence.
    // Only an account-identifying check can clear an identity mismatch.
    let login_evidence = |adapter: &str, key: &str, now: i64| {
        if !revalidated_auth {
            return None;
        }
        availability
            .iter()
            .filter(|record| {
                record.adapter == adapter
                    && record.account_key.as_deref() == Some(key)
                    && record.observed_unix_seconds <= now
                    && record.expires_unix_seconds > now
            })
            .filter_map(|record| {
                let at = record.observed_unix_seconds;
                let spec = provenant_records::AdapterSpec::get(adapter)?;
                let observed = match spec.login_observation {
                    provenant_records::LoginObservation::AccountAvailability => true,
                    provenant_records::LoginObservation::Reprobe => {
                        record.fixture_ref == format!("installed/{adapter}/login-status")
                    }
                    provenant_records::LoginObservation::ExecutableOnly => {
                        record.fixture_ref == format!("installed/{adapter}/login-unverified")
                    }
                    provenant_records::LoginObservation::None => false,
                };
                observed.then_some(if spec.login_check_names_account {
                    crate::quota::LoginEvidence::Verified(at)
                } else {
                    crate::quota::LoginEvidence::Unverified(at)
                })
            })
            .max_by_key(|probe| probe.observed_at())
    };
    for record in &mut quota_records {
        let probe = login_evidence(
            &record.account.adapter,
            &record.account.account_key,
            timestamp.as_second(),
        );
        if record.last_error == Some(provenant_quota::ProbeFailure::AccountMismatch)
            && matches!(probe, Some(crate::quota::LoginEvidence::Verified(at)) if record.last_attempt_unix_seconds.is_none_or(|attempt| at >= attempt))
        {
            record.last_error = None;
        }
    }
    let quota = quota_observations(
        &catalogue,
        availability,
        &quota_records,
        &policies,
        requested_models,
    )?;
    let capacity_cooldowns: Vec<_> = quota_records
        .iter()
        .flat_map(|record| record.capacity_cooldowns.iter().cloned())
        .collect();
    let now = WallTime::from_micros(timestamp.as_microsecond())?;
    let state = roots.state.as_path();
    let readiness = |scope: &provenant_quota::ReadinessScope, now: i64| match login_evidence(
        &scope.adapter,
        &scope.account_key,
        now,
    ) {
        Some(reprobe) => crate::quota::read_readiness_after_probe(state, scope, now, Some(reprobe)),
        None => crate::quota::read_readiness(state, scope, now),
    };
    action(
        &RoutingContext {
            policy_account_hint: None,
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability,
            quota: &quota,
            capacity_cooldowns: &capacity_cooldowns,
            accounts: &quota_records,
            quota_policies: &policies,
            readiness: Some(&readiness),
            access,
            now,
            host,
        },
        &quota_records,
        &policies,
        &domains.accounts,
    )
}
/// Resolve one production dispatch against fresh provider facts and validated
/// task access. Account aliases must already have been mapped to registered keys.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DispatchError {
    #[error("no route outside model families {} and family separation is required{}", .0.join(", "), exclusion_suffix(.1))]
    FamilySeparation(Vec<String>, Vec<String>),
    #[error(transparent)]
    Edge(Box<EdgeError>),
    #[error(transparent)]
    Catalogue(#[from] RoutingError),
}

/// The exclusions that took part in a refusal, for its message.
fn exclusion_suffix(rules: &[String]) -> String {
    if rules.is_empty() {
        String::new()
    } else {
        format!(" (with {})", rules.join("; "))
    }
}

impl From<EdgeError> for DispatchError {
    fn from(error: EdgeError) -> Self {
        Self::Edge(Box::new(error))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
) -> Result<provenant_records::ResolvedRoute, DispatchError> {
    resolve_dispatch_rejecting(
        request,
        roots,
        account,
        seed,
        availability,
        access,
        host,
        excluded_families,
        rules,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_dispatch_rejecting(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
    rejected: &[ModelId],
) -> Result<provenant_records::ResolvedRoute, DispatchError> {
    let catalogue = Catalogue::bundled()?;
    let (excluded_families, required, preferred) = match excluded_families {
        Some((families, required, preferred)) => (Some(families), required, preferred),
        None => (None, false, &[][..]),
    };
    let excluded: Vec<ModelId> = catalogue
        .models()
        .iter()
        .filter(|model| {
            excluded_families
                .is_some_and(|families| review_family_excluded(model.family.as_deref(), families))
        })
        .map(|model| model.model.clone())
        .collect();
    let preferred_excluded: Vec<ModelId> = catalogue
        .models()
        .iter()
        .filter(|model| review_family_excluded(model.family.as_deref(), preferred))
        .map(|model| model.model.clone())
        .collect();
    let combined: Vec<ModelId> = excluded
        .iter()
        .chain(&preferred_excluded)
        .cloned()
        .collect();
    let with_rejected =
        |members: &[ModelId]| members.iter().chain(rejected).cloned().collect::<Vec<_>>();
    let selected = resolve_dispatch_excluding(
        request,
        roots,
        account,
        seed,
        availability,
        access,
        host,
        &with_rejected(&combined),
        rules,
    );
    // The unused families have no member to take: none is eligible, or the one
    // chosen is blocked by persisted account evidence (mismatch or refusal). A
    // snapshot-exhausted or unavailable one is not: it stays selected rather
    // than relaxing to a used family, as it did before account evidence.
    let exhausted = match &selected {
        Err(EdgeError::Routing(error)) => error.code() == "no_eligible_pool_member",
        Ok(route) => {
            !combined.is_empty()
                && matches!(
                    &route.readiness,
                    RouteReadiness::Unavailable { reason, .. } if reason.is_account_block()
                )
        }
        Err(_) => false,
    };
    let selected = if exhausted && !preferred_excluded.is_empty() {
        let relaxed = resolve_dispatch_excluding(
            request,
            roots,
            account,
            seed,
            availability,
            access,
            host,
            &with_rejected(&excluded),
            rules,
        );
        match (relaxed, selected) {
            // Keep the preferred-stage route when relaxing would only trade one
            // unavailable route for another.
            (Ok(route), Ok(spent)) if !matches!(route.readiness, RouteReadiness::Ready { .. }) => {
                Ok(spent)
            }
            (relaxed, _) => relaxed,
        }
    } else {
        selected
    };
    let exhausted = match &selected {
        Err(EdgeError::Routing(error)) => error.code() == "no_eligible_pool_member",
        Ok(route) => {
            !excluded.is_empty()
                && matches!(&route.readiness, RouteReadiness::Unavailable { reason, .. } if reason.is_account_block())
        }
        Err(_) => false,
    };
    if required && exhausted {
        Err(DispatchError::FamilySeparation(
            excluded_families.unwrap_or_default().to_vec(),
            rules.iter().map(ToString::to_string).collect(),
        ))
    } else if !excluded.is_empty() && exhausted {
        let relaxed = resolve_dispatch_excluding(
            request,
            roots,
            account,
            seed,
            availability,
            access,
            host,
            rejected,
            rules,
        );
        match (relaxed, selected) {
            // Relaxing must not trade a spent route for another one.
            (Ok(route), Ok(spent)) if !matches!(route.readiness, RouteReadiness::Ready { .. }) => {
                Ok(spent)
            }
            (relaxed, _) => relaxed.map_err(DispatchError::from),
        }
    } else {
        selected.map_err(DispatchError::from)
    }
}

/// Resolve a dispatch using synthetic availability and catalogue fixtures.
#[cfg(feature = "fixture")]
#[allow(clippy::too_many_arguments)]
pub fn fixture_resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<(&[String], bool, &[String])>,
    rules: &[provenant_routing::ExclusionRule],
) -> Result<provenant_records::ResolvedRoute, String> {
    resolve_dispatch(
        request,
        roots,
        account,
        seed,
        availability,
        access,
        host,
        excluded_families,
        rules,
    )
    .map_err(|error| error.to_string())
}

fn review_family_excluded(family: Option<&str>, families: &[String]) -> bool {
    !families.is_empty() && family.is_none_or(|family| families.iter().any(|used| used == family))
}

#[allow(clippy::too_many_arguments)]
fn resolve_dispatch_excluding(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    initial_exclusions: &[ModelId],
    rules: &[provenant_routing::ExclusionRule],
) -> Result<provenant_records::ResolvedRoute, EdgeError> {
    let mut requested_models = Vec::new();
    if let RouteChoice::Model(model) = &request.choice {
        requested_models.push(model.clone());
    }
    if let Fallback::Models(models) = &request.fallback {
        requested_models.extend_from_slice(models.as_slice());
    }
    with_context(
        roots,
        availability,
        access,
        host,
        &requested_models,
        true,
        |context, records, policies, _| {
            let mut excluded_members = initial_exclusions.to_vec();
            let mut refusals = Vec::new();
            // Resolution already skips persisted mismatch and refusals, so this
            // narrowing retry now mostly catches admission-only failures (a fresh
            // AuthRequired, an exhausted allowance).
            let mut refused_accounts: Vec<(String, String)> = Vec::new();
            let mut resolved = provenant_routing::resolve_for_account_with_seed_excluding(
                request,
                context,
                account,
                seed.0,
                seed.1,
                &excluded_members,
                rules,
            )?;
            loop {
                let model = ModelId::parse(&format!("{}/{}", resolved.adapter, resolved.model))?;
                let pools = context.catalogue.admission_pools(&model);
                let now_unix_seconds = context.now.as_timestamp().as_second();
                let admission = provenant_quota::Admission {
                    policies,
                    now_unix_seconds,
                    records,
                    pools: &pools,
                };
                let gate = provenant_routing::check_go_allowance(
                    &model,
                    context.config,
                    context.catalogue,
                )
                .map_err(EdgeError::GoAllowance)
                .and_then(|()| {
                    if matches!(&resolved.readiness, RouteReadiness::Ready { .. }) {
                        admission
                            .check_with_partial_headroom(
                                &resolved,
                                context.config.allow_unknown_partial_headroom,
                            )
                            .map_err(EdgeError::from)
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = gate {
                    let is_go_refusal = matches!(&error, EdgeError::GoAllowance(_));
                    // A spent account leaves the model servable by another
                    // registered account. Unless the caller pinned one, try the
                    // same model there before giving it up.
                    if !is_go_refusal
                        && account.is_none()
                        && let Some(key) = resolved.account_key.clone()
                    {
                        refused_accounts.push((resolved.adapter.clone(), key));
                        let remaining: Vec<_> = context
                            .availability
                            .iter()
                            .filter(|record| {
                                !refused_accounts.iter().any(|(adapter, key)| {
                                    record.adapter == *adapter
                                        && record.account_key.as_deref() == Some(key)
                                })
                            })
                            .cloned()
                            .collect();
                        let narrowed = RoutingContext {
                            availability: &remaining,
                            ..*context
                        };
                        if let Ok(replacement) =
                            provenant_routing::resolve_for_account_with_seed_excluding(
                                request,
                                &narrowed,
                                None,
                                seed.0,
                                seed.1,
                                &excluded_members,
                                rules,
                            )
                            && matches!(&replacement.readiness, RouteReadiness::Ready { .. })
                            && replacement.adapter == resolved.adapter
                            && replacement.model == resolved.model
                            && replacement.account_key != resolved.account_key
                        {
                            resolved = replacement;
                            continue;
                        }
                    }
                    let has_fallback = matches!(&request.fallback, Fallback::Models(_));
                    if !matches!(&request.choice, RouteChoice::NamedRoute(_))
                        || resolved.pool_selection.is_none()
                        || excluded_members.contains(&model)
                    {
                        if is_go_refusal && has_fallback {
                            return Ok(resolved);
                        }
                        return Err(error);
                    }
                    let reason = error.to_string();
                    refusals.push(reason.clone());
                    excluded_members.push(model);
                    let mut replacement =
                        match provenant_routing::resolve_for_account_with_seed_excluding(
                            request,
                            context,
                            account,
                            seed.0,
                            seed.1,
                            &excluded_members,
                            rules,
                        ) {
                            Ok(route)
                                if matches!(&route.readiness, RouteReadiness::Ready { .. }) =>
                            {
                                route
                            }
                            Ok(_) | Err(_) => {
                                if is_go_refusal && has_fallback {
                                    return Ok(resolved);
                                }
                                if refusals.len() == 1 {
                                    return Err(error);
                                }
                                return Err(EdgeError::GoAllowance(refusals.join("; ")));
                            }
                        };
                    if replacement.adapter == resolved.adapter
                        && replacement.model == resolved.model
                    {
                        return Err(error);
                    }
                    record_pool_redraw(&resolved, &mut replacement, &reason)?;
                    resolved = replacement;
                    continue;
                }
                break;
            }
            Ok(resolved)
        },
    )
}

fn record_pool_redraw(
    rejected: &provenant_records::ResolvedRoute,
    replacement: &mut provenant_records::ResolvedRoute,
    reason: &str,
) -> Result<(), RoutingError> {
    let previous = rejected
        .pool_selection
        .as_ref()
        .ok_or(RoutingError::Catalogue)?;
    let current = replacement
        .pool_selection
        .as_mut()
        .ok_or(RoutingError::Catalogue)?;
    if previous.pool != current.pool {
        return Err(RoutingError::Catalogue);
    }
    for rejected_candidate in previous
        .candidates
        .iter()
        .filter(|candidate| candidate.exclusion.as_deref() == Some("admission rejected"))
    {
        if let Some(candidate) = current
            .candidates
            .iter_mut()
            .find(|candidate| candidate.model == rejected_candidate.model)
        {
            candidate.exclusion = Some("admission rejected".into());
            candidate.readiness = rejected_candidate.readiness.clone();
        } else {
            current.candidates.push(rejected_candidate.clone());
        }
    }
    if let Some(previous_candidate) = previous
        .candidates
        .iter()
        .find(|candidate| candidate.model == previous.pick)
    {
        if let Some(candidate) = current
            .candidates
            .iter_mut()
            .find(|candidate| candidate.model == previous_candidate.model)
        {
            candidate.exclusion = Some("admission rejected".into());
            candidate.readiness = previous_candidate.readiness.clone();
        } else {
            let mut candidate = previous_candidate.clone();
            candidate.exclusion = Some("admission rejected".into());
            current.candidates.push(candidate);
        }
    }
    let mut redraws = previous.redraws.clone();
    redraws.push(provenant_records::RouteSelectionRedraw {
        extra: Default::default(),
        rejected_pick: previous.pick.clone(),
        roll: previous.roll,
        total_weight: previous.total_weight,
        reason: reason.into(),
    });
    current.redraws = redraws;
    Ok(())
}
/// The account records and polling policies every account-block assessment
/// reads: resolution, the final admission recheck and settlement alternates.
/// Calibrated at `now` and dated by the configured polling intervals, so the
/// three agree on what a refusal's clearing reading and freshness mean.
pub(crate) fn account_evidence(
    settings: &provenant_quota::Settings,
    mut records: Vec<provenant_quota::AccountRecord>,
    now: i64,
) -> (
    Vec<provenant_quota::AccountRecord>,
    Vec<provenant_quota::QuotaPolicy>,
) {
    settings.apply_calibration(&mut records, now);
    (
        records,
        provenant_providers::scheduling_policies(settings, REGISTRY),
    )
}
/// Recheck each selected fallback against the latest persisted refusals.
pub(crate) fn check_dispatch_candidate(
    roots: &Roots,
    route: &provenant_records::ResolvedRoute,
) -> Result<(), String> {
    let catalogue = Catalogue::bundled().map_err(|e| e.to_string())?;
    let model =
        ModelId::parse(&format!("{}/{}", route.adapter, route.model)).map_err(|e| e.to_string())?;
    let resolved_config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?;
    let allow_unknown = resolved_config
        .knobs
        .get("routing.allow_unknown_partial_headroom")
        .and_then(|v| v.value.as_bool())
        .unwrap_or(false);
    let domains = resolved_config.domains;
    provenant_routing::check_go_allowance(&model, &domains.routing, &catalogue)?;
    let pools = catalogue.admission_pools(&model);
    let now = SystemClock::new().unix_seconds();
    let records = crate::quota::read_cache(roots).map_err(|e| e.to_string())?;
    let (records, policies) = account_evidence(&domains.quota, records, now);
    provenant_quota::Admission {
        policies: &policies,
        now_unix_seconds: now,
        records: &records,
        pools: &pools,
    }
    .check_with_partial_headroom(route, allow_unknown)
    .map_err(|e| e.to_string())
}

fn list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<Box<provenant_routing::RouteList>, EdgeError> {
    list_data_and_accounts(roots, query).map(|(routes, _)| routes)
}

fn list_data_and_accounts(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<
    (
        Box<provenant_routing::RouteList>,
        provenant_providers::accounts::Settings,
    ),
    EdgeError,
> {
    with_context(
        roots,
        &[],
        &[],
        "local-preview",
        &[],
        false,
        |context, _, _, accounts| {
            Ok((
                Box::new(provenant_routing::list(
                    &provenant_routing::RouteListQuery {
                        adapter: query.adapter.clone(),
                        pool: query.pool.clone(),
                        valid: query.valid,
                    },
                    context,
                )?),
                accounts.clone(),
            ))
        },
    )
}

fn display_list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<Box<RouteListDisplay>, EdgeError> {
    let (routes, accounts) = list_data_and_accounts(roots, query)?;
    let catalogue = Catalogue::bundled()?;
    let now = SystemClock::new().unix_seconds();
    let mut models = BTreeSet::new();
    models.extend(routes.models.iter().map(|entry| entry.model.clone()));
    models.extend(routes.valid_pairs.iter().map(|pair| pair.model.clone()));
    models.extend(
        routes
            .named_routes
            .values()
            .map(|route| route.model.clone()),
    );
    models.extend(
        routes
            .pools
            .values()
            .flatten()
            .map(|member| member.model.clone()),
    );
    // Read each account's bounded file once for this display, without probing or writes.
    let files = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let lookup = |scope: &provenant_quota::ReadinessScope| {
        let key = (scope.adapter.clone(), scope.account_key.clone());
        let mut files = files.borrow_mut();
        let file = files.entry(key).or_insert_with(|| {
            crate::quota::recorded_readiness_file(
                roots.state.as_path(),
                &scope.adapter,
                &scope.account_key,
            )
            .map_err(|_| ())
        });
        match file {
            Ok(Some(file)) => file.get(scope, now),
            Ok(None) => provenant_quota::ReadinessRead::absent(),
            Err(()) => provenant_quota::ReadinessRead::unknown(
                provenant_quota::ReadinessReason::InvalidEvidence,
            ),
        }
    };
    let mut recorded_readiness = Vec::new();
    let mut readiness_truncated = false;
    for model in models {
        let rows = recorded_model_readiness(
            &accounts,
            &model,
            &catalogue.admission_pools(&model),
            None,
            None,
            lookup,
        );
        for row in rows {
            if recorded_readiness.len() == 256 {
                readiness_truncated = true;
                break;
            }
            recorded_readiness.push(row);
        }
        if readiness_truncated {
            break;
        }
    }
    Ok(Box::new(RouteListDisplay {
        routes,
        recorded_readiness,
        readiness_truncated,
    }))
}

pub(crate) fn effective_members(
    roots: &Roots,
    pool: &str,
) -> Result<Vec<RoutePoolMember>, EdgeError> {
    let query = RouteListRequest {
        valid: false,
        adapter: None,
        pool: Some(pool.to_owned()),
    };
    let listed = list_data(roots, &query)?;
    let members = listed.pools.get(pool).ok_or(RoutingError::UnknownRoute)?;
    Ok(members
        .iter()
        .map(|member| RoutePoolMember {
            model: member.model.clone(),
            weight: member.weight,
            effort: member.effort.clone(),
            efforts: member.efforts.clone(),
            free: member.free,
        })
        .collect())
}

fn write_pool(
    document: &mut toml_edit::DocumentMut,
    pool: &str,
    members: &[RoutePoolMember],
) -> Result<(), &'static str> {
    let routing = routing_table(document)?;
    let pools = routing
        .entry("pools")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let pools = config_table(pools).ok_or("domains.routing.pools must be a table")?;
    let mut values = toml_edit::Array::new();
    for member in members {
        let mut row = toml_edit::InlineTable::new();
        row.insert("model", toml_edit::Value::from(member.model.as_str()));
        row.insert("weight", toml_edit::Value::from(i64::from(member.weight)));
        if let Some(effort) = &member.effort {
            row.insert("effort", toml_edit::Value::from(effort.as_str()));
        }
        let mut efforts = toml_edit::Array::new();
        for effort in &member.efforts {
            efforts.push(effort.as_str());
        }
        row.insert("efforts", toml_edit::Value::Array(efforts));
        row.insert("free", toml_edit::Value::from(member.free));
        values.push(toml_edit::Value::InlineTable(row));
    }
    pools.insert(
        pool,
        toml_edit::Item::Value(toml_edit::Value::Array(values)),
    );
    Ok(())
}

fn routing_table(
    document: &mut toml_edit::DocumentMut,
) -> Result<&mut toml_edit::Table, &'static str> {
    let domains = document.entry("domains").or_insert_with(|| {
        let mut domains = toml_edit::Table::new();
        domains.set_implicit(true);
        toml_edit::Item::Table(domains)
    });
    let routing = config_table(domains)
        .ok_or("domains must be a TOML table")?
        .entry("routing")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    config_table(routing).ok_or("domains.routing must be a TOML table")
}

fn config_table(item: &mut toml_edit::Item) -> Option<&mut toml_edit::Table> {
    if let toml_edit::Item::Value(toml_edit::Value::InlineTable(table)) = item {
        *item = toml_edit::Item::Table(std::mem::take(table).into_table());
    }
    item.as_table_mut()
}

fn edit_config(
    roots: &Roots,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), &'static str>,
) -> Result<(), EdgeError> {
    let _ = provenant_config::edit_config::<crate::domains::Domains>(
        roots,
        &crate::domains::SECTIONS,
        &provenant_config::EditOptions::new("cli"),
        |document, _| {
            edit(document).map_err(|reason| provenant_config::ConfigError::Edit { reason })
        },
    )?;
    Ok(())
}

fn set_pool(roots: &Roots, request: &RouteSet) -> Result<(), EdgeError> {
    let catalogue = Catalogue::bundled()?;
    let model = ModelId::parse(&request.model.0)?;
    let entry = catalogue.model(&model).ok_or(RoutingError::InvalidModel)?;
    if !REGISTRY
        .iter()
        .any(|provider| provider.adapter.id() == model.adapter())
    {
        return Err(RoutingError::UnknownAdapter.into());
    }
    let mut members = match effective_members(roots, &request.pool) {
        Ok(members) => members,
        Err(EdgeError::Routing(RoutingError::UnknownRoute)) => Vec::new(),
        Err(error) => return Err(error),
    };
    let existing = members.iter().position(|member| member.model == model);
    let old = existing.map(|index| &members[index]);
    let efforts = if request.efforts.is_empty() {
        old.map_or_else(|| entry.efforts.clone(), |member| member.efforts.clone())
    } else {
        request.efforts.clone()
    };
    let member = RoutePoolMember {
        model: model.clone(),
        weight: request
            .weight
            .or_else(|| old.map(|member| member.weight))
            .unwrap_or(1),
        effort: request
            .effort
            .clone()
            .or_else(|| old.and_then(|member| member.effort.clone())),
        efforts,
        free: old.map_or_else(|| catalogue.is_free_model(&model), |member| member.free),
    };
    if let Some(index) = existing {
        members[index] = member;
    } else {
        members.push(member);
    }
    edit_config(roots, |document| {
        write_pool(document, &request.pool, &members)
    })
}

fn remove_pool_member(roots: &Roots, request: &RouteRemove) -> Result<(), EdgeError> {
    let model = ModelId::parse(&request.model.0)?;
    let mut members = effective_members(roots, &request.pool)?;
    let before = members.len();
    members.retain(|member| member.model != model);
    if members.len() == before {
        return Err(RoutingError::UnknownRoute.into());
    }
    edit_config(roots, |document| {
        write_pool(document, &request.pool, &members)
    })
}

fn reset_pool(roots: &Roots, request: &RouteReset) -> Result<(), EdgeError> {
    let catalogue = Catalogue::bundled()?;
    if !catalogue.has_pool(&request.pool) {
        return Err(RoutingError::UnknownRoute.into());
    }
    edit_config(roots, |document| {
        if let Some(domains) = document.get_mut("domains").and_then(config_table)
            && let Some(routing) = domains.get_mut("routing").and_then(config_table)
            && let Some(pools) = routing.get_mut("pools").and_then(config_table)
        {
            pools.remove(&request.pool);
            if pools.is_empty() {
                routing.remove("pools");
            }
        }
        Ok(())
    })
}

/// Refuse a provider the registry does not know before anything is written.
fn known_provider(id: &str) -> Result<(), EdgeError> {
    if REGISTRY.iter().any(|provider| provider.adapter.id() == id) {
        Ok(())
    } else {
        Err(RoutingError::UnknownAdapter.into())
    }
}

/// Record a provider's default pacing policy and describe what was recorded.
fn set_pacing(roots: &Roots, request: &RoutePacing) -> Result<String, EdgeError> {
    let policy = policy_name(request.policy.into());
    known_provider(&request.provider)?;
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let pacing = routing
            .entry("pacing")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let pacing = config_table(pacing).ok_or("domains.routing.pacing must be a table")?;
        pacing.insert(&request.provider, toml_edit::value(policy));
        Ok(())
    })?;
    Ok(format!("provider {} pacing {policy}", request.provider))
}

/// An RFC 3339 timestamp, or a positive duration (s, m, h, d or w) from `now`.
fn instant(value: &str, now: WallTime) -> Option<WallTime> {
    if let Ok(at) = WallTime::parse_rfc3339(value) {
        return Some(at);
    }
    let seconds =
        provenant_providers::duration::suffix_seconds(value.strip_prefix('+').unwrap_or(value))?;
    if seconds == 0 {
        return None;
    }
    now.checked_add(provenant_core::DurationMicros::from_seconds(seconds).ok()?)
        .ok()
}

/// Take the recorded pacing override rows as inline tables, whichever TOML
/// array form the file used.
fn take_override_rows(
    routing: &mut toml_edit::Table,
) -> Result<Vec<toml_edit::InlineTable>, &'static str> {
    match routing.remove("pacing_overrides") {
        None => Ok(Vec::new()),
        Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
            .into_iter()
            .map(|value| match value {
                toml_edit::Value::InlineTable(row) => Ok(row),
                _ => Err("domains.routing.pacing_overrides must be an array of tables"),
            })
            .collect(),
        Some(toml_edit::Item::ArrayOfTables(tables)) => Ok(tables
            .into_iter()
            .map(toml_edit::Table::into_inline_table)
            .collect()),
        _ => Err("domains.routing.pacing_overrides must be an array of tables"),
    }
}

/// Rewrite the pacing overrides, removing each row `remove` selects and every
/// expired one, then appending `append`. Returns the removed rows.
fn rewrite_overrides(
    roots: &Roots,
    now: WallTime,
    remove: impl Fn(&provenant_routing::PacingOverride) -> bool,
    append: Option<&provenant_routing::PacingOverride>,
) -> Result<Vec<provenant_routing::PacingOverride>, EdgeError> {
    let mut removed = Vec::new();
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let mut kept = toml_edit::Array::new();
        for row in take_override_rows(routing)? {
            let decoded = toml_edit::de::from_document::<provenant_routing::PacingOverride>(
                toml_edit::DocumentMut::from(row.clone().into_table()),
            )
            .ok();
            match decoded {
                Some(decoded)
                    if remove(&decoded)
                        || decoded.schedule.expires.is_some_and(|end| end <= now) =>
                {
                    removed.push(decoded);
                }
                _ => kept.push(row),
            }
        }
        if let Some(row) = append {
            kept.push(override_value(row));
        }
        if !kept.is_empty() {
            routing.insert(
                "pacing_overrides",
                toml_edit::Item::Value(toml_edit::Value::Array(kept)),
            );
        }
        Ok(())
    })?;
    Ok(removed)
}

/// Whether a recorded row has the scope given by a resolved account and a
/// provider. Recorded account names resolve the same way as the request's.
fn same_scope(
    row: &provenant_routing::PacingOverride,
    account: Option<&str>,
    provider: Option<&str>,
    settings: &provenant_providers::accounts::Settings,
) -> bool {
    let recorded = row.account.as_deref().map(|name| {
        resolve_account_reference(name, "account", settings).unwrap_or_else(|_| name.to_owned())
    });
    recorded.as_deref() == account && row.provider.as_deref() == provider
}

fn set_override(roots: &Roots, request: &OverrideSet, now: WallTime) -> Result<Data, EdgeError> {
    if let Some(provider) = &request.provider {
        known_provider(provider)?;
    }
    let starts = request
        .starts
        .as_deref()
        .map(|value| {
            instant(value, now).ok_or(EdgeError::Pacing(
                "--starts must be an RFC 3339 timestamp or positive duration",
            ))
        })
        .transpose()?;
    let expires = request
        .until
        .as_deref()
        .map(|value| {
            instant(value, now)
                .filter(|end| *end > now)
                .ok_or(EdgeError::Pacing(
                    "--until must be a future RFC 3339 timestamp or positive duration",
                ))
        })
        .transpose()?;
    let windows = request
        .window
        .iter()
        .map(|spec| parse_window(spec))
        .collect::<Option<Vec<_>>>()
        .ok_or(EdgeError::Pacing(
            "--window must be [DAYS@]START-END, such as mon-fri@9-17 or 23-2",
        ))?;
    let schedule = provenant_routing::Schedule {
        starts,
        expires,
        tz: request.tz.clone(),
        windows,
    };
    if !schedule.valid() {
        return Err(EdgeError::Pacing(
            "invalid schedule: --starts must precede --until, --tz must be an IANA time zone, windows need distinct start and end hours from 0 to 24, and at most 16 windows",
        ));
    }
    let settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let account = request
        .account
        .as_deref()
        .map(|name| resolve_account_reference(name, "--account", &settings))
        .transpose()?;
    let recorded = provenant_routing::PacingOverride {
        account,
        provider: request.provider.clone(),
        policy: request.policy.into(),
        schedule,
    };
    let removed = rewrite_overrides(
        roots,
        now,
        |row| {
            same_scope(
                row,
                recorded.account.as_deref(),
                recorded.provider.as_deref(),
                &settings,
            )
        },
        Some(&recorded),
    )?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: Some(OverrideRow::pacing(&recorded, now)),
        cleared: removed
            .iter()
            .map(|row| OverrideRow::pacing(row, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

fn clear_override(
    roots: &Roots,
    request: &OverrideClear,
    now: WallTime,
) -> Result<Data, EdgeError> {
    let settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let account = request
        .account
        .as_deref()
        .map(|name| resolve_account_reference(name, "--account", &settings))
        .transpose()?;
    let every = account.is_none() && request.provider.is_none();
    let removed = rewrite_overrides(
        roots,
        now,
        |row| {
            every
                || same_scope(
                    row,
                    account.as_deref(),
                    request.provider.as_deref(),
                    &settings,
                )
        },
        None,
    )?;
    Ok(Data::Overrides(Box::new(OverrideReport {
        notes: exclusion_notes(roots)?,
        recorded: None,
        cleared: removed
            .iter()
            .map(|row| OverrideRow::pacing(row, now))
            .collect(),
        overrides: list_overrides(roots, now)?,
    })))
}

/// What was repaired or ignored in the persisted model exclusions.
fn exclusion_notes(roots: &Roots) -> Result<Vec<String>, EdgeError> {
    Ok(
        provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
            .domains
            .routing
            .model_exclusions
            .notes()
            .to_vec(),
    )
}
/// Every recorded pacing override, then every scheduled model policy rule.
fn list_overrides(roots: &Roots, now: WallTime) -> Result<Vec<OverrideRow>, EdgeError> {
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .routing;
    let mut overrides: Vec<OverrideRow> = config
        .pacing_overrides
        .iter()
        .map(|row| OverrideRow::pacing(row, now))
        .collect();
    overrides.extend(
        config
            .model_exclusions
            .iter()
            .map(|row| OverrideRow::model_exclusion(row, now)),
    );
    for (index, row) in config.model_policy.rules.iter().enumerate() {
        let scope = [
            row.adapter.as_deref().map(|v| format!("adapter {v}")),
            row.model.as_ref().map(|v| format!("model {}", v.as_str())),
            row.account.as_deref().map(|v| format!("account {v}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        overrides.push(OverrideRow {
            kind: "model_policy",
            scope: if scope.is_empty() {
                "all routes".into()
            } else {
                scope
            },
            value: format!("rules[{index}]"),
            schedule: row.schedule.clone(),
            active: row.schedule.active(now),
            remaining_seconds: row.schedule.remaining(now),
        });
    }
    Ok(overrides)
}

fn edit_model_exclusion(
    roots: &Roots,
    model: &Model,
    until: Option<WallTime>,
    add: bool,
    now: WallTime,
) -> Result<(), EdgeError> {
    let id = ModelId::parse(&model.0)?;
    if add && Catalogue::bundled()?.model(&id).is_none() {
        return Err(RoutingError::InvalidModel.into());
    }
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let rows = match routing.remove("model_exclusions") {
            None => Vec::new(),
            Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values
                .into_iter()
                .map(|row| match row {
                    toml_edit::Value::InlineTable(row) => Ok(row),
                    _ => Err("domains.routing.model_exclusions must be an array of tables"),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(toml_edit::Item::ArrayOfTables(rows)) => rows
                .into_iter()
                .map(toml_edit::Table::into_inline_table)
                .collect(),
            _ => return Err("domains.routing.model_exclusions must be an array of tables"),
        };
        let mut kept = toml_edit::Array::new();
        for row in rows {
            // Adding also drops the rows that have expired, so lapsed
            // exclusions cannot fill the limit. A row whose expiry is not
            // readable is kept.
            let lapsed = add
                && row
                    .get("schedule")
                    .and_then(toml_edit::Value::as_inline_table)
                    .and_then(|schedule| schedule.get("expires"))
                    .and_then(toml_edit::Value::as_str)
                    .and_then(|text| WallTime::parse_rfc3339(text).ok())
                    .is_some_and(|end| end <= now);
            if !lapsed && row.get("model").and_then(toml_edit::Value::as_str) != Some(id.as_str()) {
                kept.push(row);
            }
        }
        if add {
            let mut row = toml_edit::InlineTable::new();
            row.insert("model", id.as_str().into());
            if let Some(until) = until {
                let mut schedule = toml_edit::InlineTable::new();
                schedule.insert("expires", until.to_string().into());
                row.insert("schedule", schedule.into());
            }
            kept.push(row);
        }
        if !kept.is_empty() {
            routing.insert("model_exclusions", toml_edit::Item::Value(kept.into()));
        }
        Ok(())
    })
}

fn execute(command: &Route, roots: &Roots, now: WallTime) -> Result<Data, EdgeError> {
    match command {
        Route::List(query) => Ok(Data::List(display_list_data(roots, query)?)),
        Route::Resolve(query) => with_context(
            roots,
            &[],
            &[],
            "local-preview",
            &[],
            false,
            |context, records, _, accounts| {
                let context = &RoutingContext {
                    policy_account_hint: query.account.as_deref(),
                    ..*context
                };
                let request = resolve_request(query, context)?;
                let rules = effective_exclusion_rules(&query.request.exclude, roots)?;
                let route = provenant_routing::resolve_for_account_with_seed_excluding(
                    &request,
                    context,
                    None,
                    "route-preview",
                    0,
                    &[],
                    &rules,
                )?;
                let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
                let model_policy = context.config.model_policy.effective(
                    &model,
                    query.account.as_deref().or(route.account_key.as_deref()),
                    context.now,
                );
                provenant_routing::validate_intent(&model_policy.policy, request.intent)?;
                let compaction_break_even_turns = model_policy
                    .policy
                    .compaction
                    .window
                    .and_then(|window| model_policy.policy.break_even_turns(window, 15_000, 0));
                provenant_routing::check_go_allowance(&model, context.config, context.catalogue)
                    .map_err(EdgeError::GoAllowance)?;
                let quota = records
                    .iter()
                    .find(|r| {
                        r.account.adapter == route.adapter
                            && (route.account_key.as_deref()
                                == Some(r.account.account_key.as_str())
                                || (route.account_key.is_none()
                                    && provenant_records::AdapterSpec::get(&route.adapter)
                                        .is_some_and(|spec| spec.allowance_prefix.is_some())
                                    && snapshot_governs(
                                        &route.adapter,
                                        &r.account.account_key,
                                        &model,
                                    )))
                    })
                    .and_then(|r| r.snapshot.clone());
                let login_diagnostic = match &route.readiness {
                    RouteReadiness::Unavailable {
                        reason:
                            reason @ (provenant_records::RouteExclusionReason::AuthRequired
                            | provenant_records::RouteExclusionReason::LoginExpired
                            | provenant_records::RouteExclusionReason::AccountMismatch),
                        ..
                    } => Some(crate::quota::account_login_diagnostic(
                        accounts.entries.values(),
                        records,
                        &route.adapter,
                        route.account_key.as_deref(),
                        reason,
                    )),
                    _ => None,
                };
                let pacing = provenant_routing::pacing_preview(&model, context);
                let recorded_readiness = recorded_model_readiness(
                    accounts,
                    &model,
                    &context.catalogue.admission_pools(&model),
                    route.account_key.as_deref(),
                    query.account.as_deref(),
                    |scope| {
                        crate::quota::read_readiness(
                            roots.state.as_path(),
                            scope,
                            now.unix_seconds(),
                        )
                    },
                );
                let entry = context.catalogue.model(&model);
                let allow_unknown_partial_headroom = context
                    .config
                    .partial_headroom_source
                    .clone()
                    .ok_or(RoutingError::InvalidValue)?;
                Ok(Data::Resolved(Box::new(RoutePreview {
                    allow_unknown_partial_headroom,
                    exclusion_notes: context.config.model_exclusions.notes().to_vec(),
                    model_policy,
                    compaction_break_even_turns,
                    route,
                    supported_efforts: entry.map(|entry| entry.efforts.clone()),
                    nearest_models: if entry.is_none() {
                        context.catalogue.nearest_models(&model)
                    } else {
                        Vec::new()
                    },
                    pacing,
                    recorded_readiness,
                    login_diagnostic,
                    quota,
                })))
            },
        ),
        Route::Set(request) => {
            set_pool(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Remove(request) => {
            remove_pool_member(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Reset(request) => {
            reset_pool(roots, request)?;
            Ok(Data::List(display_list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Pacing(request) => {
            let recorded = set_pacing(roots, request)?;
            let list = list_data(
                roots,
                &RouteListRequest {
                    valid: false,
                    adapter: None,
                    pool: None,
                },
            )?;
            Ok(Data::Pacing(Box::new(PacingReport {
                recorded,
                active_overrides: list.active_overrides,
            })))
        }
        Route::Override(request) => match &request.command {
            RouteOverrideCommand::Set(set) => set_override(roots, set, now),
            RouteOverrideCommand::List => Ok(Data::Overrides(Box::new(OverrideReport {
                notes: exclusion_notes(roots)?,
                recorded: None,
                cleared: Vec::new(),
                overrides: list_overrides(roots, now)?,
            }))),
            RouteOverrideCommand::Clear(clear) => clear_override(roots, clear, now),
        },
        Route::Exclude(request) => {
            match &request.command {
                RouteExcludeCommand::Add(add) => {
                    let until = add.until.as_deref().map(|value| instant(value, now).filter(|end| *end > now).ok_or(EdgeError::Pacing("--until must be a future RFC 3339 timestamp or positive duration"))).transpose()?;
                    edit_model_exclusion(roots, &add.model, until, true, now)?;
                }
                RouteExcludeCommand::Remove(remove) => {
                    edit_model_exclusion(roots, &remove.model, None, false, now)?
                }
                RouteExcludeCommand::List => {}
            }
            Ok(Data::Overrides(Box::new(OverrideReport {
                notes: exclusion_notes(roots)?,
                recorded: None,
                cleared: Vec::new(),
                overrides: list_overrides(roots, now)?
                    .into_iter()
                    .filter(|row| row.kind == "model_exclusion")
                    .collect(),
            })))
        }
    }
}
/// Preview a dispatch's route without launching: the pick, each pool candidate
/// and every exclusion that removed one, with its source.
pub(crate) fn explain_dispatch(
    request: &RouteRequest,
    roots: &Roots,
) -> Result<(serde_json::Value, String), EdgeError> {
    let now = SystemClock::new().wall_now()?;
    let query = RouteResolve {
        writes: false,
        account: None,
        name: None,
        request: request.clone(),
    };
    let data = execute(&Route::Resolve(query), roots, now)?;
    let Data::Resolved(preview) = &data else {
        return Err(RoutingError::Catalogue.into());
    };
    let text = resolved_text(preview);
    Ok((
        serde_json::to_value(&data).map_err(|_| RoutingError::Catalogue)?,
        text,
    ))
}
/// Whether the account's reading says a window is spent and has not reset.
/// Admission gates only models with a quota pool; native providers surface a
/// spent allowance as an exhausted snapshot, which a preview reads directly.
fn spent(record: &provenant_quota::AccountRecord, now: i64) -> bool {
    record.snapshot.as_ref().is_some_and(|snapshot| {
        snapshot.status == provenant_quota::QuotaStatus::Exhausted
            && snapshot.observed_unix_seconds <= now
            && snapshot.windows.iter().any(|window| {
                window.used_basis_points >= 10_000
                    && window.resets_unix_seconds.is_none_or(|reset| reset > now)
            })
    })
}

/// Why a route alias cannot serve work now, resolved the way dispatch resolves
/// it: the configured pool, the exclusions (flags and configuration), and the
/// cached account evidence and quota routing already holds. `None` means a
/// member can run, or readiness is only known once a provider is probed at
/// dispatch. A planner never falls through to a weaker route, so the preset
/// front-end degrades on a `Some`.
pub fn route_blocked(alias: &str, exclude: &[String], roots: &Roots) -> Option<String> {
    let query = RouteResolve {
        writes: false,
        account: None,
        name: None,
        request: RouteRequest {
            read_heavy: false,
            model: None,
            route: Some(alias.into()),
            effort: None,
            confidential: false,
            fallback: Vec::new(),
            exclude: exclude.to_vec(),
        },
    };
    let blocked = with_context(
        roots,
        &[],
        &[],
        "local-preview",
        &[],
        false,
        |context, records, policies, _| {
            let request = resolve_request(&query, context)?;
            let rules = effective_exclusion_rules(exclude, roots)?;
            let now = context.now.as_timestamp().as_second();
            // Dispatch retries the next pool member when one is refused, so a
            // route is blocked only when every member is. The resolver picks
            // each candidate in turn; a refused member is excluded from the
            // next pick, exactly as dispatch narrows its own retry.
            let mut excluded: Vec<ModelId> = Vec::new();
            let mut refusals: Vec<String> = Vec::new();
            loop {
                let route = match provenant_routing::resolve_for_account_with_seed_excluding(
                    &request,
                    context,
                    None,
                    "route-preview",
                    0,
                    &excluded,
                    &rules,
                ) {
                    Ok(route) => route,
                    Err(error) if refusals.is_empty() => return Err(error.into()),
                    // Every member has been judged and refused.
                    Err(_) => return Ok::<_, EdgeError>(Some(refusals.join("; "))),
                };
                let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
                // The resolver ignores exclusions for a route that names one
                // model, and returns it again: every candidate has been seen.
                if excluded.contains(&model) {
                    return Ok(Some(refusals.join("; ")));
                }
                let pools = context.catalogue.admission_pools(&model);
                let admission = provenant_quota::Admission {
                    policies,
                    now_unix_seconds: now,
                    records,
                    pools: &pools,
                };
                // Judge each cached account of the member's provider as if its
                // provider were ready: a spent, refused or unauthenticated
                // account cannot serve. With no cached account, nothing is
                // known and the member may run once dispatch probes it.
                let mut member = Vec::new();
                for record in records
                    .iter()
                    .filter(|record| record.account.adapter == route.adapter)
                {
                    let mut probe = route.clone();
                    probe.account_key = Some(record.account.account_key.clone());
                    probe.readiness = RouteReadiness::Ready {
                        extra: Default::default(),
                        availability_ref: "preview".into(),
                        expires_unix_seconds: now + 60,
                    };
                    match admission.check_with_partial_headroom(
                        &probe,
                        context.config.allow_unknown_partial_headroom,
                    ) {
                        Ok(()) if !spent(record, now) => return Ok(None),
                        Ok(()) => member.push("allowance exhausted".to_owned()),
                        Err(error) => member.push(error.to_string()),
                    }
                }
                if member.is_empty() {
                    return Ok(None);
                }
                refusals.push(format!(
                    "{}/{}: {}",
                    route.adapter,
                    route.model,
                    member.join("; ")
                ));
                // Dispatch narrows only a pool; a single model has no next one.
                if route.pool_selection.is_none() {
                    return Ok(Some(refusals.join("; ")));
                }
                excluded.push(model);
            }
        },
    );
    match blocked {
        Ok(reason) => reason,
        Err(error) => Some(error.to_string()),
    }
}
fn readiness_text(readiness: &RouteReadiness) -> String {
    use provenant_records::RouteExclusionReason;
    match readiness {
        RouteReadiness::Ready {
            expires_unix_seconds,
            ..
        } => format!("ready (observation expires at {expires_unix_seconds})"),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
            ..
        } => "unknown (not probed; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::StaleAvailability,
            ..
        } => "unknown (observation expired; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownPartialHeadroom,
            ..
        } => "unknown (partial headroom is uncalibrated; run `provenant quota tick` to refresh quota)".into(),
        RouteReadiness::Unavailable { reason, .. } => {
            let label = serde_json::to_value(reason)
                .ok()
                .and_then(|value| value.as_str().map(|text| text.replace('_', " ")))
                .unwrap_or_else(|| "unknown reason".into());
            format!("unavailable ({label})")
        }
    }
}

fn unobserved_model(model: &ModelId, list: &RouteListDisplay) -> &'static str {
    if list
        .recorded_readiness
        .iter()
        .any(|record| record.model == model.as_str())
    {
        ""
    } else if list.readiness_truncated {
        "; readiness not shown (display limit reached)"
    } else {
        "; readiness unknown (not probed; run `provenant quota tick` to refresh quota)"
    }
}

/// The human report for a resolved route preview, naming each exclusion that
/// removed a pool candidate and where it came from.
pub(crate) fn resolved_text(preview: &RoutePreview) -> String {
    let route = &preview.route;
    let diagnostics = preview
        .pacing
        .iter()
        .map(|p| {
            format!(
                "\n  account={} policy={:?} factor={} {}",
                p.account.as_deref().unwrap_or("unknown"),
                p.policy,
                p.factor_basis_points,
                p.detail
            )
        })
        .collect::<String>();
    let mut text = format!(
        "{}/{} effort={} {}{}{}\n",
        route.adapter,
        route.model,
        route.effort.as_deref().unwrap_or("unspecified"),
        readiness_text(&route.readiness),
        route
            .pool_selection
            .as_ref()
            .map_or_else(String::new, |receipt| {
                let details = receipt
                    .candidates
                    .iter()
                    .flat_map(|candidate| {
                        candidate
                            .exclusion
                            .iter()
                            .chain(candidate.pacing_detail.iter())
                            .map(|detail| format!("\n  {}: {detail}", candidate.model))
                    })
                    .collect::<String>();
                format!(
                    " pool={} roll={} pick={}{}",
                    receipt.pool, receipt.roll, receipt.pick, details
                )
            }),
        diagnostics
    );
    for record in &preview.recorded_readiness {
        text.push_str(&format!(
            "recorded readiness {} [{}] for {}: {}\n",
            record.account.escape_debug(),
            record.account_key.escape_debug(),
            record.model,
            crate::quota::readiness_words(&record.observation)
        ));
    }
    if let Some(diagnostic) = &preview.login_diagnostic {
        text.push_str(&format!("login: {diagnostic}\n"));
    }
    text.push_str(&format!(
        "model policy sources={}\ncompaction break-even turns (15k summary, zero rework): {:?}\n",
        preview.model_policy.sources.join(", "),
        preview.compaction_break_even_turns
    ));
    for note in &preview.exclusion_notes {
        text.push_str(&format!("note: {note}\n"));
    }
    match &preview.supported_efforts {
        Some(efforts) if efforts.is_empty() => text.push_str("supported efforts: none\n"),
        Some(efforts) => text.push_str(&format!("supported efforts: {}\n", efforts.join(", "))),
        None => {
            text.push_str("model is not in the catalogue; effort support is unrecorded\n");
            if !preview.nearest_models.is_empty() {
                text.push_str(&format!(
                    "nearest catalogued IDs: {}\n",
                    preview.nearest_models.join(", ")
                ));
            }
        }
    }
    text.push_str(&format!(
        "allow unknown partial headroom: {} (source: {:?})\n",
        preview.allow_unknown_partial_headroom.value, preview.allow_unknown_partial_headroom.source
    ));
    if let Some(details) = preview.quota.as_ref().and_then(|q| q.details.as_ref())
        && let (Some(source), Some(coverage)) = (&details.source, &details.coverage)
    {
        text.push_str(&format!("quota source: {source}, coverage: {coverage}\n"));
    }
    text
}
pub fn run(
    command: Route,
    globals: &Globals,
    roots: &Roots,
    publish: impl FnOnce(&CommandResult<Option<Data>>) -> io::Result<provenant_records::AbsolutePath>,
) -> u8 {
    let name = match &command {
        Route::List(_) => "route list",
        Route::Resolve(_) => "route resolve",
        Route::Set(_) => "route set",
        Route::Remove(_) => "route remove",
        Route::Reset(_) => "route reset",
        Route::Pacing(_) => "route pacing",
        Route::Exclude(request) => match request.command {
            RouteExcludeCommand::Add(_) => "route exclude add",
            RouteExcludeCommand::Remove(_) => "route exclude remove",
            RouteExcludeCommand::List => "route exclude list",
        },
        Route::Override(request) => match request.command {
            RouteOverrideCommand::Set(_) => "route override set",
            RouteOverrideCommand::List => "route override list",
            RouteOverrideCommand::Clear(_) => "route override clear",
        },
    };
    let now = SystemClock::new().wall_now();
    let at = now.as_ref().ok().copied();
    let result = match now
        .map_err(EdgeError::from)
        .and_then(|now| execute(&command, roots, now))
    {
        Ok(data) => CommandResult::new(name, Outcome::Ok, Some(data)),
        Err(error) => {
            let (status, code) = match &error {
                EdgeError::Config(provenant_config::ConfigError::Read { .. }) => {
                    (Outcome::Error, "IO")
                }
                EdgeError::QuotaCache(crate::quota::EdgeError::Quota(_)) | EdgeError::Quota(_) => {
                    (Outcome::Invalid, "QUOTA")
                }
                EdgeError::Routing(_) | EdgeError::Unresolved { .. } | EdgeError::Pacing(_) => {
                    (Outcome::Invalid, "INVALID")
                }
                EdgeError::Config(_) | EdgeError::PacingConfig(_) => (Outcome::Invalid, "CONFIG"),
                EdgeError::AccountProfile { kind, .. } => match kind {
                    AccountProfileError::Unknown
                    | AccountProfileError::Ambiguous
                    | AccountProfileError::Duplicate => (Outcome::Invalid, "CONFIG"),
                },
                _ => (Outcome::Error, "IO"),
            };
            let mut result = CommandResult::new(name, status, None);
            result.error = Some(CommandError {
                diagnostic: None,
                code: code.into(),
                message: error.to_string(),
                cause: None,
                repair: None,
            });
            result
        }
    };
    let human = if let Some(error) = &result.error {
        format!("provenant: {}: {}\n", error.code, error.message)
    } else {
        match &result.data {
            Some(Data::List(list)) => {
                let mut lines = vec![format!("catalogue {}", list.catalogue_revision)];
                lines.extend(list.valid_pairs.iter().map(|pair| {
                    format!(
                        "{} effort={}",
                        pair.model.as_str(),
                        pair.effort.as_deref().unwrap_or("unspecified")
                    )
                }));
                lines.extend(list.models.iter().map(|m| {
                    format!(
                        "{} efforts=[{}]{}",
                        m.model.as_str(),
                        m.efforts.join(","),
                        unobserved_model(&m.model, list)
                    )
                }));
                lines.extend(list.named_routes.iter().map(|(name, r)| {
                    format!(
                        "route {name} = {}{}",
                        r.model.as_str(),
                        unobserved_model(&r.model, list)
                    )
                }));
                lines.extend(list.account_pacing.iter().map(|entry| {
                    format!(
                        "account {}/{} pacing={:?} source={} profile={:?} expires={:?}",
                        entry.adapter,
                        entry.account,
                        entry.policy,
                        entry.source,
                        entry.profile,
                        entry.expires.map(|at| at.to_string()),
                    )
                }));
                lines.extend(list.configured_accounts.iter().map(|(account, profile)| {
                    format!("account {account} pacing profile={profile:?}")
                }));
                lines.extend(list.configured_free.iter().map(|(provider, profile)| {
                    format!("free {provider} pacing profile={profile:?}")
                }));
                lines.extend(
                    list.active_overrides
                        .iter()
                        .map(|override_| format!("pacing override {override_:?}")),
                );
                lines.extend(
                    list.exclusion_notes
                        .iter()
                        .map(|note| format!("note: {note}")),
                );
                if let Some(now) = at {
                    lines.extend(
                        list.model_exclusions
                            .iter()
                            .map(|row| OverrideRow::model_exclusion(row, now).line()),
                    );
                }
                if list.recorded_readiness.is_empty() {
                    lines.push("readiness unknown (not probed; run `provenant quota tick` to refresh quota)".into());
                }
                lines.extend(list.recorded_readiness.iter().map(|record| {
                    format!(
                        "recorded readiness {} [{}] for {}: {}",
                        record.account.escape_debug(),
                        record.account_key.escape_debug(),
                        record.model,
                        crate::quota::readiness_words(&record.observation)
                    )
                }));
                if list.readiness_truncated {
                    lines.push(
                        "warning: readiness display limited to 256 account/model rows".into(),
                    );
                }
                lines.extend(list.pools.iter().map(|(name, members)| {
                    let models = members
                        .iter()
                        .map(|member| {
                            format!(
                                "{} weight={} pacing={:?}{}{}",
                                member.model.as_str(),
                                member.weight,
                                member.pacing,
                                member
                                    .effort
                                    .as_ref()
                                    .map_or(String::new(), |effort| format!(" effort={effort}")),
                                unobserved_model(&member.model, list),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("route pool {name} = {models}")
                }));
                format!("{}\n", lines.join("\n"))
            }
            Some(Data::Resolved(preview)) => resolved_text(preview),
            Some(Data::Pacing(report)) => at.map(|now| report.human(now)).unwrap_or_default(),
            Some(Data::Overrides(report)) => {
                let mut lines = Vec::new();
                if let Some(row) = &report.recorded {
                    lines.push(format!("recorded: {}", row.line()));
                }
                lines.extend(
                    report
                        .cleared
                        .iter()
                        .map(|row| format!("removed: {}", row.line())),
                );
                lines.push(format!("overrides: {}", report.overrides.len()));
                lines.extend(report.overrides.iter().map(OverrideRow::line));
                lines.extend(report.notes.iter().map(|note| format!("note: {note}")));
                format!("{}\n", lines.join("\n"))
            }
            None => String::new(),
        }
    };
    let summary = match &result.data {
        Some(Data::List(list)) => {
            let counts = [
                ("models", list.models.len()),
                ("routes", list.named_routes.len()),
                ("pools", list.pools.len()),
                ("pacing entries", list.account_pacing.len()),
                ("overrides", list.active_overrides.len()),
                ("model exclusions", list.model_exclusions.len()),
                ("exclusion notes", list.exclusion_notes.len()),
            ];
            let mut rows = Vec::new();
            if list.readiness_truncated {
                rows.push("warning: readiness display limited to 256 account/model rows".into());
            }
            if list.recorded_readiness.is_empty() {
                rows.push(
                    "readiness unknown (not probed; run `provenant quota tick` to refresh quota)"
                        .into(),
                );
            }
            rows.extend(list.recorded_readiness.iter().map(|record| {
                format!(
                    "recorded readiness {} [{}] for {}: {}",
                    record.account.escape_debug(),
                    record.account_key.escape_debug(),
                    record.model,
                    crate::quota::readiness_words(&record.observation)
                )
            }));
            rows.extend(list.named_routes.iter().map(|(name, r)| {
                format!(
                    "route {name} = {}{}",
                    r.model.as_str(),
                    unobserved_model(&r.model, list)
                )
            }));
            rows.extend(list.pools.keys().map(|name| format!("pool {name}")));
            Some(crate::Summary::new(
                format!(
                    "catalogue {}: {}",
                    list.catalogue_revision,
                    crate::Summary::tally(&counts)
                ),
                rows,
                Some("provenant route resolve MODEL".to_owned()),
            ))
        }
        Some(Data::Pacing(report)) => Some(crate::Summary::new(
            format!(
                "recorded {}; {} active pacing overrides",
                report.recorded,
                report.active_overrides.len()
            ),
            Vec::new(),
            Some("provenant route override list".to_owned()),
        )),
        Some(Data::Overrides(report)) => {
            let active = report.overrides.iter().filter(|row| row.active).count();
            let tally = format!(
                "{} scheduled overrides, {active} active",
                report.overrides.len()
            );
            let headline = match (&report.recorded, report.cleared.len()) {
                (Some(row), 0) => format!("recorded {}; {tally}", row.line()),
                (Some(row), cleared) => {
                    format!("recorded {}; removed {cleared}; {tally}", row.line())
                }
                (None, _)
                    if matches!(
                        command,
                        Route::Override(RouteOverride {
                            command: RouteOverrideCommand::Clear(_)
                        })
                    ) =>
                {
                    format!("cleared {} pacing overrides; {tally}", report.cleared.len())
                }
                (None, _) => tally,
            };
            let next = if matches!(command, Route::Exclude(_)) {
                if report.overrides.is_empty() {
                    "provenant route exclude add ADAPTER/ID --until 2h"
                } else {
                    "provenant route exclude remove ADAPTER/ID"
                }
            } else if report.overrides.is_empty() {
                "provenant route override set --account ACCOUNT --policy POLICY --until 2h"
            } else if report.recorded.is_some() {
                "provenant route override list"
            } else {
                "provenant route override clear"
            };
            Some(crate::Summary::new(
                headline,
                report
                    .overrides
                    .iter()
                    .map(OverrideRow::line)
                    .chain(report.notes.iter().map(|note| format!("note: {note}")))
                    .collect(),
                Some(next.to_owned()),
            ))
        }
        _ => None,
    };
    let listing = matches!(
        command,
        Route::Override(RouteOverride {
            command: RouteOverrideCommand::List
        })
    );
    let mode = globals.render_mode(
        &human,
        listing || matches!(command, Route::List(_) | Route::Resolve(_)),
        summary.as_ref(),
    );
    let outcome = crate::render_or_publish(
        &result,
        mode,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        publish,
    );
    outcome.map_or(3, |outcome| crate::exit_status(&outcome))
}

#[cfg(test)]
mod route_tests {
    use provenant_records::{
        ResolvedRoute, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteReadiness,
        RouteSelectionCandidate, RouteSelectionReceipt,
    };
    use std::error::Error;

    #[test]
    fn route_list_does_not_call_omitted_evidence_unprobed() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let query = super::RouteListRequest {
            valid: false,
            adapter: Some("codex".into()),
            pool: None,
        };
        let mut list = super::display_list_data(&roots, &query)?;
        let model = super::ModelId::parse("codex/gpt-6-luna")?;
        assert!(super::unobserved_model(&model, &list).contains("not probed"));
        list.readiness_truncated = true;
        assert!(super::unobserved_model(&model, &list).contains("not shown"));
        Ok(())
    }

    #[test]
    fn recorded_route_readiness_keeps_model_and_account_scope() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{
            ReadinessFile, ReadinessReason, ReadinessScope, ReadinessState, RouteReadiness,
        };
        let settings: provenant_providers::accounts::Settings =
            serde_json::from_value(serde_json::json!({"entries":{
                "one":{"adapter":"codex","label":"one","key":"key-one","directory":"/fixture/one"},
                "two":{"adapter":"codex","label":"two","key":"key-two","directory":"/fixture/two"}
            }}))?;
        let model = super::ModelId::parse("codex/gpt-6-luna")?;
        let scope = ReadinessScope {
            host: provenant_quota::LOCAL_HOST.into(),
            adapter: "codex".into(),
            account_key: "key-one".into(),
            route: model.as_str().into(),
        };
        let file = ReadinessFile {
            records: vec![
                RouteReadiness {
                    schema_version: 1,
                    scope: scope.clone(),
                    state: ReadinessState::Unavailable,
                    reason: Some(ReadinessReason::ProviderRefusal),
                    observed_unix_seconds: 10,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
                RouteReadiness {
                    schema_version: 1,
                    scope: ReadinessScope {
                        account_key: "key-two".into(),
                        ..scope.clone()
                    },
                    state: ReadinessState::Ready,
                    reason: None,
                    observed_unix_seconds: 11,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
                RouteReadiness {
                    schema_version: 1,
                    scope: ReadinessScope {
                        account_key: "key-two".into(),
                        route: "verified-pool".into(),
                        ..scope.clone()
                    },
                    state: ReadinessState::Unavailable,
                    reason: Some(ReadinessReason::Exhausted),
                    observed_unix_seconds: 12,
                    expires_unix_seconds: 20,
                    reset_unix_seconds: None,
                    evidence_ref: Some("fixture".into()),
                },
            ],
            ..Default::default()
        };
        let rows = super::recorded_model_readiness(
            &settings,
            &model,
            &[],
            None,
            Some("one"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].host, provenant_quota::LOCAL_HOST);
        assert_eq!(rows[0].account_key, "key-one");
        assert_eq!(rows[0].model, model.as_str());
        assert_eq!(
            rows[0].observation.reason,
            Some(ReadinessReason::ProviderRefusal)
        );
        assert_eq!(rows[0].observation.observed_scope.as_ref(), Some(&scope));
        assert_eq!(rows[0].observation.observed_unix_seconds, Some(10));
        let other = super::recorded_model_readiness(
            &settings,
            &model,
            &[],
            None,
            Some("two"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(other[0].observation.state, ReadinessState::Ready);
        assert_eq!(other[0].observation.observed_unix_seconds, Some(11));
        let pooled = super::recorded_model_readiness(
            &settings,
            &model,
            &["verified-pool".into()],
            None,
            Some("two"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(
            pooled[0].observation.reason,
            Some(ReadinessReason::Exhausted)
        );
        assert_eq!(
            pooled[0].observation.observed_scope.as_ref().unwrap().route,
            "verified-pool"
        );
        let other_model = super::ModelId::parse("codex/gpt-6.1-terra")?;
        let other = super::recorded_model_readiness(
            &settings,
            &other_model,
            &[],
            None,
            Some("one"),
            |requested| file.get(requested, 15),
        );
        assert_eq!(other[0].observation.state, ReadinessState::Unknown);
        Ok(())
    }

    fn exclusion_roots(temp: &tempfile::TempDir) -> Result<provenant_core::Roots, Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        Ok(roots)
    }

    #[test]
    fn route_readiness_is_rendered_in_words() {
        use provenant_records::RouteExclusionReason;
        let ready = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 120,
        };
        assert!(super::readiness_text(&ready).starts_with("ready "));
        let unknown = super::readiness_text(&RouteReadiness::unknown());
        assert!(
            unknown.contains("unknown (not probed") && unknown.contains("provenant quota tick"),
            "{unknown}"
        );
        let stale = super::readiness_text(&RouteReadiness::stale());
        assert!(stale.contains("unknown (observation expired"), "{stale}");
        let partial = RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::UnknownPartialHeadroom,
        };
        assert!(super::readiness_text(&partial).starts_with("unknown (partial headroom"));
        let refused = RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::AuthRequired,
        };
        assert_eq!(
            super::readiness_text(&refused),
            "unavailable (auth required)"
        );
    }

    #[test]
    fn route_resolve_human_output_excludes_policy_prompt() -> Result<(), Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[domains.routing.model_policy.defaults.lane]\nstop_early_ok=false\n[domains.routing.model_policy.defaults.result]\nsections=['PRIVATE_SENTINEL']\n",
        )?;
        let command = super::Route::Resolve(super::RouteResolve {
            writes: false,
            account: None,
            name: None,
            request: super::RouteRequest {
                read_heavy: false,
                model: Some(super::Model("codex/gpt-6-luna".into())),
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        });
        let super::Data::Resolved(preview) = super::execute(
            &command,
            &roots,
            provenant_core::WallTime::from_unix_seconds(100)?,
        )?
        else {
            panic!("expected route preview")
        };
        assert!(
            preview
                .model_policy
                .policy
                .guidance()
                .contains("PRIVATE_SENTINEL")
        );
        let text = super::resolved_text(&preview);
        assert!(!text.contains("PRIVATE_SENTINEL"), "{text}");
        assert!(!text.contains("Carry the task to completion"), "{text}");
        Ok(())
    }

    #[test]
    fn dispatch_rechecks_recorded_auth_with_a_healthy_account_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_quota::{
            AccountRecord, AccountScope, LOCAL_HOST, ReadinessFile, ReadinessReason,
        };
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let dir = roots.state.as_path().join("readiness/key");
        std::fs::create_dir_all(&dir)?;
        for path in [
            roots.state.as_path(),
            roots.state.as_path().join("readiness").as_path(),
            dir.as_path(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let account = AccountRecord::empty(AccountScope {
            adapter: "codex".into(),
            account_key: "key".into(),
            label: "work".into(),
        });
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            now - 10,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let path = dir.join("codex.json");
        std::fs::write(&path, serde_json::to_vec(&file)?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let mut available = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "codex".into(),
            executable: "/fixture/codex".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "installed/codex/version".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("key".into()),
            observed_models: Vec::new(),
            observed_unix_seconds: now,
            expires_unix_seconds: now + 300,
        };
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/unlisted-exact".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let resolve = |record: &provenant_providers::AvailabilityRecord| {
            super::resolve_dispatch(
                &request,
                &roots,
                Some("key"),
                ("fixture", 0),
                std::slice::from_ref(record),
                &access,
                "fixture",
                None,
                &[],
            )
        };
        let before = resolve(&available)?;
        assert!(matches!(
            before.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::AuthRequired,
                ..
            }
        ));
        available.fixture_ref = "installed/codex/login-status".into();
        let after = resolve(&available)?;
        assert!(matches!(
            after.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        Ok(())
    }

    #[test]
    fn opencode_unverified_login_allows_selected_account_to_try()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_quota::{
            AccountRecord, AccountScope, LOCAL_HOST, ReadinessFile, ReadinessReason,
        };
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let dir = roots.state.as_path().join("readiness/go");
        std::fs::create_dir_all(&dir)?;
        for path in [
            roots.state.as_path(),
            roots.state.as_path().join("readiness").as_path(),
            dir.as_path(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let account = AccountRecord::empty(AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        });
        let mut file = ReadinessFile::default();
        file.sync_probe_result(
            &account,
            LOCAL_HOST,
            now - 10,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let path = dir.join("opencode.json");
        std::fs::write(&path, serde_json::to_vec(&file)?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let mut available = provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "installed/opencode/version".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: Vec::new(),
            observed_unix_seconds: now,
            expires_unix_seconds: now + 300,
        };
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model(
                "opencode/opencode-go/deepseek-v4.1-flash".into(),
            )),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let resolve = |record: &provenant_providers::AvailabilityRecord| {
            super::resolve_dispatch(
                &request,
                &roots,
                Some("go"),
                ("fixture", 0),
                std::slice::from_ref(record),
                &access,
                "fixture",
                None,
                &[],
            )
        };
        assert!(matches!(
            resolve(&available)?.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::AuthRequired,
                ..
            }
        ));
        available.fixture_ref = "installed/opencode/login-unverified".into();
        assert!(matches!(
            resolve(&available)?.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        // The probe does not erase the recorded failure.
        assert_eq!(
            file.get(
                &provenant_quota::ReadinessScope {
                    host: LOCAL_HOST.into(),
                    adapter: "opencode".into(),
                    account_key: "go".into(),
                    route: "opencode/opencode-go/deepseek-v4.1-flash".into()
                },
                now
            )
            .reason,
            Some(ReadinessReason::SignedOut)
        );
        let zen_dir = roots.state.as_path().join("readiness/zen");
        std::fs::create_dir_all(&zen_dir)?;
        std::fs::set_permissions(&zen_dir, std::fs::Permissions::from_mode(0o700))?;
        let zen = AccountRecord::empty(AccountScope {
            adapter: "opencode".into(),
            account_key: "zen".into(),
            label: "zen".into(),
        });
        let mut zen_readiness = ReadinessFile::default();
        zen_readiness.sync_probe_result(
            &zen,
            LOCAL_HOST,
            now - 10,
            300,
            Some(ReadinessReason::SignedOut),
        )?;
        let zen_path = zen_dir.join("opencode.json");
        std::fs::write(&zen_path, serde_json::to_vec(&zen_readiness)?)?;
        std::fs::set_permissions(&zen_path, std::fs::Permissions::from_mode(0o600))?;
        let mut zen_available = available.clone();
        zen_available.account_key = Some("zen".into());
        zen_available.fixture_ref = "installed/opencode/version".into();
        let zen_request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model(
                "opencode/opencode/deepseek-v4.1-flash-free".into(),
            )),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let zen_result = super::resolve_dispatch(
            &zen_request,
            &roots,
            Some("zen"),
            ("fixture", 0),
            &[available.clone(), zen_available],
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert!(matches!(
            zen_result.readiness,
            provenant_records::RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::AuthRequired,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn exclude_selectors_parse_and_unknown_ones_list_the_valid_forms() -> Result<(), Box<dyn Error>>
    {
        use provenant_routing::{ExclusionSelector, ExclusionSource};
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let parse =
            |text: &str| super::exclusion_rules(&[text.to_owned()], ExclusionSource::Flag, &roots);
        assert_eq!(
            parse("claude")?[0].selector,
            ExclusionSelector::Provider("claude".into())
        );
        assert!(matches!(
            parse("codex/gpt-6-luna")?[0].selector,
            ExclusionSelector::Model(_)
        ));
        let message = parse("nonesuch").unwrap_err().to_string();
        assert!(
            message.contains("unknown exclusion `nonesuch`"),
            "{message}"
        );
        assert!(message.contains("valid forms: a provider"), "{message}");
        Ok(())
    }

    #[test]
    fn explain_names_exclusions_and_never_picks_the_excluded_provider() -> Result<(), Box<dyn Error>>
    {
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        let request = super::RouteRequest {
            read_heavy: false,
            model: None,
            route: Some("junior".into()),
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec!["claude".into()],
        };
        let (data, text) = super::explain_dispatch(&request, &roots)?;
        assert_ne!(data["adapter"], "claude");
        assert_eq!(data["allow_unknown_partial_headroom"]["value"], false);
        assert_eq!(data["allow_unknown_partial_headroom"]["source"], "default");
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.allow_unknown_partial_headroom'=true\n",
        )?;
        let (override_data, override_text) = super::explain_dispatch(&request, &roots)?;
        assert_eq!(
            override_data["allow_unknown_partial_headroom"]["value"],
            true
        );
        assert_eq!(
            override_data["allow_unknown_partial_headroom"]["source"],
            "override"
        );
        assert!(override_text.contains("allow unknown partial headroom: true (source: Override)"));
        assert!(
            text.contains("claude/claude-haiku-5-5: excluded by flag: provider claude"),
            "{text}"
        );
        let all = super::RouteRequest {
            exclude: ["claude", "codex", "opencode", "agy", "cursor"]
                .map(String::from)
                .to_vec(),
            ..request
        };
        let message = super::explain_dispatch(&all, &roots)
            .unwrap_err()
            .to_string();
        assert!(message.contains("excluded by flag: provider"), "{message}");
        Ok(())
    }

    #[test]
    fn model_exclusion_commands_record_list_and_remove() -> Result<(), Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let now = provenant_core::WallTime::from_unix_seconds(100)?;
        let model = super::Model("codex/gpt-6-luna".into());
        super::edit_model_exclusion(
            &roots,
            &model,
            Some(provenant_core::WallTime::from_unix_seconds(200)?),
            true,
            now,
        )?;
        let rows = super::list_overrides(&roots, now)?;
        assert!(rows.iter().any(|row| row.kind == "model_exclusion"
            && row.active
            && row.remaining_seconds == Some(100)));
        super::edit_model_exclusion(&roots, &model, None, false, now)?;
        assert!(
            !super::list_overrides(&roots, now)?
                .iter()
                .any(|row| row.kind == "model_exclusion")
        );
        Ok(())
    }

    #[test]
    fn adding_an_exclusion_prunes_expired_rows_and_notes_bad_ones() -> Result<(), Box<dyn Error>> {
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[domains.routing]\nmodel_exclusions=[\
             {model='claude/old-model',schedule={expires='1970-01-01T00:00:01Z'}},\
             {model='claude/not-a-row',schedule={expires='garbage'}},\
             {model='not a model'}]\n", // timebomb: allow fixed epoch fixture, not the live clock
        )?;
        let now = provenant_core::WallTime::from_unix_seconds(100)?;
        let notes = super::exclusion_notes(&roots)?;
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("stays excluded with no schedule"),
            "{notes:?}"
        );
        assert!(notes[1].contains("is ignored"), "{notes:?}");
        let report = super::execute(
            &super::Route::Exclude(super::RouteExclude {
                command: super::RouteExcludeCommand::List,
            }),
            &roots,
            now,
        )?;
        let super::Data::Overrides(report) = report else {
            return Err("overrides".into());
        };
        assert_eq!(report.notes.len(), 2);
        let model = super::Model("codex/gpt-6-luna".into());
        super::edit_model_exclusion(&roots, &model, None, true, now)?;
        let text = std::fs::read_to_string(roots.state.as_path().join("config.toml"))?;
        // The lapsed row is gone; the unreadable ones are kept for the owner.
        assert!(!text.contains("old-model"), "{text}");
        assert!(
            text.contains("not-a-row") && text.contains("not a model"),
            "{text}"
        );
        assert!(text.contains("gpt-6-luna"), "{text}");
        Ok(())
    }

    #[test]
    fn pacing_report_prints_every_active_override() -> Result<(), Box<dyn Error>> {
        use provenant_core::{Clock, MonoTime, TestClock, WallTime};
        let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
        let report = super::PacingReport {
            recorded: "codex".into(),
            active_overrides: serde_json::from_value(serde_json::json!([
                {"provider":"codex", "policy":"pace"},
                {"provider":"claude", "policy":"off"}
            ]))?,
        };
        let text = report.human(clock.wall_now()?);
        assert_eq!(text.lines().count(), 4);
        assert!(text.contains("active overrides: 2"));
        assert!(text.contains("pacing provider codex pace active"));
        assert!(text.contains("pacing provider claude off active"));
        Ok(())
    }

    #[test]
    fn override_instants_and_windows_parse_relative_to_the_supplied_instant() {
        use super::{instant, parse_window};

        let at = |seconds| provenant_core::WallTime::from_unix_seconds(seconds).ok();
        let epoch = provenant_core::WallTime::from_micros(0).expect("epoch");
        assert_eq!(instant("1s", at(99).expect("instant")), at(100));
        assert_eq!(instant("+5m", at(99).expect("instant")), at(399));
        assert_eq!(instant("+0m", epoch), None);
        assert_eq!(instant("++5m", epoch), None);
        assert_eq!(instant("2h", epoch), at(7200));
        assert_eq!(instant("1w", epoch), at(604_800));
        assert_eq!(instant("1h30m", epoch), at(5_400));
        assert_eq!(instant("0h", epoch), None);
        assert_eq!(instant("1970-01-01T00:01:00Z", epoch), at(60)); // timebomb: allow parse-only fixture with an injected instant
        let weekdays = parse_window("mon-wed,fri,6@23-2").expect("window");
        assert_eq!(weekdays.weekdays, [0, 1, 2, 4, 6]);
        assert_eq!((weekdays.start_hour, weekdays.end_hour), (23, 2));
        let every_day = parse_window("9-17").expect("window");
        assert!(every_day.weekdays.is_empty());
        for invalid in ["fri-mon@9-17", "funday@9-17", "7@9-17", "9", "a-b"] {
            assert!(parse_window(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn override_set_replaces_its_scope_and_prunes_expired_rows() -> Result<(), Box<dyn Error>> {
        use super::{OverrideSet, PacingChoice};
        use std::path::Path;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing]\npacing_overrides=[{account='codex/removed',policy='pace',schedule={expires='1970-01-01T00:00:01Z'}},{provider='codex',policy='off'},{provider='claude',policy='off'}]\n", // timebomb: allow fixed epoch fixture, not the live clock
        )?;

        let data = super::set_override(
            &roots,
            &OverrideSet {
                account: None,
                provider: Some("codex".into()),
                all: false,
                policy: PacingChoice::Max,
                starts: None,
                until: Some("1h".into()),
                window: vec!["mon-fri@23-7".into()],
                tz: Some("Australia/Sydney".into()),
            },
            provenant_core::WallTime::from_unix_seconds(2)?,
        )?;

        let report = serde_json::to_value(&data)?;
        assert_eq!(report["recorded"]["scope"], "provider codex");
        assert_eq!(report["recorded"]["value"], "max");
        assert_eq!(report["recorded"]["remaining_seconds"], 3600);
        assert_eq!(report["cleared"].as_array().map(Vec::len), Some(2));
        assert_eq!(report["overrides"].as_array().map(Vec::len), Some(2));
        let saved = std::fs::read_to_string(path)?;
        assert!(!saved.contains("codex/removed"), "{saved}");
        assert!(!saved.contains("provider='codex'"), "{saved}");
        assert!(saved.contains("provider='claude'"), "{saved}");
        assert!(
            saved.contains("schedule = { expires = \"1970-01-01T01:00:02.000000Z\", tz = \"Australia/Sydney\", windows = [{ weekdays = [0, 1, 2, 3, 4], start_hour = 23, end_hour = 7 }] }"), // timebomb: allow fixed epoch fixture, not the live clock
            "{saved}"
        );
        Ok(())
    }

    #[test]
    fn pacing_account_references_reject_collisions_and_keep_adapter() {
        use provenant_providers::accounts::Settings;
        use provenant_routing::RoutingConfig;
        let settings: Settings = serde_json::from_value(serde_json::json!({"entries": {
            "first": {"adapter":"claude", "label":"work", "key":"shared", "directory":"/tmp/a"},
            "shared": {"adapter":"codex", "label":"shared", "key":"other", "directory":"/tmp/b"},
            "third": {"adapter":"agy", "label":"personal", "key":"shared", "directory":"/tmp/c"}
        }}))
        .unwrap();
        for config in [
            serde_json::json!({"pacing":{"accounts":{"shared":{"policy":"pace"}}}}),
            serde_json::json!({"pacing":{"free":{"codex":{"deadline":{"reset_of":"shared"}}}}}),
        ] {
            let mut config: RoutingConfig = serde_json::from_value(config).unwrap();
            let error = super::resolve_pacing_accounts(&mut config, &settings).unwrap_err();
            assert!(matches!(
                error,
                super::EdgeError::AccountProfile {
                    kind: super::AccountProfileError::Ambiguous,
                    ..
                }
            ));
            let error = error.to_string();
            for expected in ["claude/shared", "codex/other", "agy/shared", "adapter/key"] {
                assert!(error.contains(expected), "{error}");
            }
        }
        let mut config: RoutingConfig = serde_json::from_value(serde_json::json!({"pacing":{
            "accounts":{"work":{"deadline":{"reset_of":"agy/shared"}}}
        }}))
        .unwrap();
        super::resolve_pacing_accounts(&mut config, &settings).unwrap();
        let profile = &config.pacing.accounts["claude/shared"];
        assert!(
            matches!(&profile.deadline, Some(provenant_routing::PacingDeadline::ResetOf(r)) if r.reset_of == "agy/shared")
        );
    }
    #[test]
    fn override_references_resolve_and_registered_labels_precede_derived_accounts() {
        use provenant_providers::accounts::Settings;
        use provenant_routing::RoutingConfig;
        let settings: Settings = serde_json::from_value(serde_json::json!({"entries": {
            "one": {"adapter":"claude", "label":"office", "key":"shared", "directory":"/tmp/a"},
            "two": {"adapter":"codex", "label":"other", "key":"shared", "directory":"/tmp/b"},
            "three": {"adapter":"opencode", "label":"zen", "key":"work", "directory":"/tmp/c"}
        }}))
        .unwrap();
        assert_eq!(
            super::resolve_account_reference("zen", "test", &settings).unwrap(),
            "opencode/work"
        );
        assert_eq!(
            super::resolve_account_reference("opencode/zen", "test", &settings).unwrap(),
            "opencode/zen"
        );
        assert!(matches!(
            super::resolve_account_reference("shared", "test", &settings),
            Err(super::EdgeError::AccountProfile {
                kind: super::AccountProfileError::Ambiguous,
                ..
            })
        ));
        for (input, expected) in [
            ("office", "claude/shared"),
            ("claude/shared", "claude/shared"),
        ] {
            let mut config: RoutingConfig =
                serde_json::from_value(serde_json::json!({"pacing_overrides": [{
                    "account": input, "policy": "pace", "schedule": {"expires": 2000000000_i64}
                }]}))
                .unwrap();
            super::resolve_pacing_accounts(&mut config, &settings).unwrap();
            assert_eq!(
                config.pacing_overrides[0].account.as_deref(),
                Some(expected)
            );
        }
    }

    #[test]
    fn names_resolve_to_routes_then_exact_then_unique_models() {
        use super::{Named, match_name};
        let routes = ["junior", "utility"].map(String::from).into();
        let models =
            ["codex/gpt-6-luna", "codex/gpt-6.1-terra", "agy/gpt-6-luna"].map(String::from);
        let model = |name: &str| Named::Model(name.into());
        assert_eq!(
            match_name("utility", &routes, &models),
            Ok(Named::Route("utility".into()))
        );
        assert_eq!(
            match_name("terra", &routes, &models),
            Ok(model("codex/gpt-6.1-terra"))
        );
        assert_eq!(
            match_name("GPT-6.1-TERRA", &routes, &models),
            Ok(model("codex/gpt-6.1-terra"))
        );
        // An adapter-qualified name goes to resolve as the exact model.
        assert_eq!(
            match_name("codex/other", &routes, &models),
            Ok(model("codex/other"))
        );
        // The same ID under two adapters, or a fragment of several, is not guessed.
        let Err(both) = match_name("gpt-6-luna", &routes, &models) else {
            panic!("one ID under two adapters must not resolve");
        };
        assert!(
            both.contains(&"codex/gpt-6-luna".to_owned())
                && both.contains(&"agy/gpt-6-luna".to_owned())
        );
        let Err(close) = match_name("gpt", &routes, &models) else {
            panic!("a fragment of several models must not resolve");
        };
        assert_eq!(close.len(), 3);
        assert!(match_name("zzzzzz", &routes, &models).is_err_and(|close| close.is_empty()));
    }

    #[test]
    fn only_opencode_routes_draw_on_the_free_pool() {
        assert_eq!(
            provenant_routing::free_pools("opencode", || true),
            ["opencode-free"]
        );
        assert!(provenant_routing::free_pools("opencode", || false).is_empty());
        assert!(provenant_routing::free_pools("codex", || true).is_empty());
    }

    #[test]
    fn go_floor_gates_explicit_pool_and_fallback_dispatch() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/glm-5.3-flash',weight=1},{model='opencode/opencode-go/deepseek-v4.1-flash',weight=0}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=61\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            fixture_ref: "fixture".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: vec![
                "opencode-go/glm-5.3-flash".into(),
                "opencode-go/deepseek-v4.1-flash".into(),
            ],
            observed_unix_seconds: now,
            expires_unix_seconds: now + 300,
            last_update: None,
        }];
        let access = [provenant_routing::AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let request = |model: Option<&str>, route: Option<&str>| {
            super::RouteRequest {
                read_heavy: false,
                model: model.map(|m| super::Model(m.into())),
                route: route.map(str::to_owned),
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            }
            .to_domain()
        };
        let resolve = |request: &provenant_routing::RouteRequest| {
            super::resolve_dispatch(
                request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                None,
                &[],
            )
        };
        let preview = super::Route::Resolve(super::RouteResolve {
            writes: false,
            account: None,
            name: None,
            request: super::RouteRequest {
                read_heavy: false,
                model: Some(super::Model("opencode/opencode-go/glm-5.3-flash".into())),
                route: None,
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            },
        });
        match super::execute(&preview, &roots, provenant_core::WallTime::from_micros(0)?) {
            Err(error) => assert!(error.to_string().contains("$60")),
            Ok(_) => panic!("preview must refuse the model"),
        }
        let error =
            resolve(&request(Some("opencode/opencode-go/glm-5.3-flash"), None)?).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("glm-5.3-flash") && error.contains("$60"));
        let error = resolve(&request(Some("opencode/opencode-go/unlisted"), None)?).unwrap_err();
        let error = error.to_string();
        assert!(
            error.contains("opencode-go/unlisted")
                && error.contains("not an admitted OpenCode route")
                && error.contains("opencode/opencode-go/glm-5.3-flash")
        );
        let error = resolve(&request(Some("opencode/opencode-go/glm-5.2"), None)?).unwrap_err();
        let error = error.to_string();
        assert!(error.contains("glm-5.2") && error.contains("opencode/opencode-go/glm-5.3-flash"));
        let fallback = |model: Option<&str>, route: Option<&str>| -> Result<(), Box<dyn Error>> {
            let request = super::RouteRequest {
                read_heavy: false,
                model: model.map(|m| super::Model(m.into())),
                route: route.map(str::to_owned),
                effort: None,
                confidential: false,
                fallback: vec![super::Model(
                    "opencode/opencode-go/deepseek-v4.1-flash".into(),
                )],
                exclude: vec![],
            };
            let resolved = resolve(&request.to_domain()?)?;
            assert!(super::check_dispatch_candidate(&roots, &resolved).is_err());
            assert_eq!(
                resolved.alternatives.as_slice()[0].model,
                "opencode-go/deepseek-v4.1-flash"
            );
            Ok(())
        };
        fallback(Some("opencode/opencode-go/glm-5.3-flash"), None)?;
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/glm-5.3-flash',weight=1}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=61\n",
        )?;
        fallback(None, Some("test"))?;
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/glm-5.3-flash',weight=1},{model='opencode/opencode-go/deepseek-v4.1-flash',weight=1}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=100\n",
        )?;
        let error = resolve(&request(None, Some("test"))?)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("glm-5.3-flash") && error.contains("deepseek-v4.1-flash"),
            "{error}"
        );
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/glm-5.3-flash',weight=1},{model='opencode/opencode-go/deepseek-v4.1-flash',weight=0}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=61\n",
        )?;
        let selected = resolve(&request(None, Some("test"))?)?;
        assert_eq!(selected.model, "opencode-go/deepseek-v4.1-flash");
        assert!(
            selected.pool_selection.as_ref().unwrap().redraws[0]
                .reason
                .contains("$60")
        );
        let mut candidate = selected.clone();
        candidate.model = "opencode-go/unlisted".into();
        assert!(
            super::check_dispatch_candidate(&roots, &candidate)
                .unwrap_err()
                .contains("unknown")
        );
        candidate.model = "opencode-go/glm-5.3-flash".into();
        assert!(
            super::check_dispatch_candidate(&roots, &candidate)
                .unwrap_err()
                .contains("$60")
        );
        Ok(())
    }

    #[test]
    fn unlisted_exact_models_keep_account_quota_gates() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        use provenant_records::RouteExclusionReason;
        use std::os::unix::fs::PermissionsExt;

        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let catalogue = super::Catalogue::bundled()?;
        let mut availability = Vec::new();
        for (key, status) in [
            ("exhausted", QuotaStatus::Exhausted),
            ("fresh", QuotaStatus::Available),
        ] {
            let account = AccountScope {
                adapter: "codex".into(),
                account_key: key.into(),
                label: key.into(),
            };
            let mut record = AccountRecord::empty(account.clone());
            record.snapshot = Some(QuotaSnapshot {
                schema_version: 1,
                account,
                revision: 0,
                status,
                observed_unix_seconds: now,
                source_ref: "fixture".into(),
                authority_ref: None,
                details: None,
                windows: Vec::new(),
            });
            let quota = roots.state.as_path().join("quota");
            let directory = quota.join(key);
            std::fs::create_dir_all(&directory)?;
            for directory in [roots.state.as_path(), quota.as_path(), directory.as_path()] {
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
            }
            let path = directory.join("codex.json");
            std::fs::write(&path, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            availability.push(provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: "codex".into(),
                executable: "/fixture/codex".into(),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(key.into()),
                observed_models: Vec::new(),
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            });
        }
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/unlisted-exact".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![super::Model("codex/unlisted-fallback".into())],
            exclude: vec![],
        }
        .to_domain()?;
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let exhausted = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability[..1],
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert_eq!(
            exhausted.readiness,
            RouteReadiness::Unavailable {
                extra: Default::default(),
                reason: RouteExclusionReason::Exhausted,
            }
        );
        assert_eq!(
            exhausted.alternatives.as_slice()[0].readiness,
            RouteReadiness::Unavailable {
                extra: Default::default(),
                reason: RouteExclusionReason::Exhausted,
            }
        );
        let fresh = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            None,
            &[],
        )?;
        assert!(matches!(fresh.readiness, RouteReadiness::Ready { .. }));
        assert_eq!(fresh.account_key.as_deref(), Some("fresh"));
        assert_eq!(
            fresh.alternatives.as_slice()[0].account_key.as_deref(),
            Some("fresh")
        );
        Ok(())
    }

    #[test]
    fn refused_account_falls_through_to_another_for_the_same_model() -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let catalogue = super::Catalogue::bundled()?;
        let availability: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|key| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: "codex".into(),
                executable: "/fixture/codex".into(),
                version: "fixture".into(),
                last_update: None,
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(key.into()),
                observed_models: vec!["gpt-6-luna".into()],
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
            })
            .collect();
        let access = [provenant_routing::AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        }];
        let request = super::RouteRequest {
            read_heavy: false,
            model: Some(super::Model("codex/gpt-6-luna".into())),
            route: None,
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let resolve = |account: Option<&str>| {
            super::resolve_dispatch(
                &request,
                &roots,
                account,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                None,
                &[],
            )
        };
        let drawn = resolve(None)?;
        let spent = drawn.account_key.clone().ok_or("account")?;
        assert!(super::check_dispatch_candidate(&roots, &drawn).is_ok());
        // The provider refused the drawn account: the next dispatch keeps the
        // model and takes the other account instead of failing.
        crate::quota::persist_provider_refusal(
            roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-6-luna".into()),
                account_key: Some(spent.clone()),
                account_label: Some(spent.clone()),
                ..Default::default()
            },
            None,
            now,
            Some(now + 3_600),
        )?;
        // A refusal written after resolution still stops the launch.
        let error = super::check_dispatch_candidate(&roots, &drawn).unwrap_err();
        assert!(error.contains("scoped provider refusal"), "{error}");
        let redrawn = resolve(None)?;
        assert!(super::check_dispatch_candidate(&roots, &redrawn).is_ok());
        assert_eq!(redrawn.model, "gpt-6-luna");
        assert!(matches!(redrawn.readiness, RouteReadiness::Ready { .. }));
        assert_ne!(redrawn.account_key.as_deref(), Some(spent.as_str()));
        // A pinned account is the caller's choice and is never swapped.
        // It resolves as unavailable with the typed reason rather than ready.
        let RouteReadiness::Unavailable { reason, .. } = resolve(Some(&spent))?.readiness else {
            return Err("a pinned refused account must be unavailable".into());
        };
        assert_eq!(
            reason,
            provenant_records::RouteExclusionReason::ProviderRefused
        );
        // The unavailable reason keeps the repair text, not a debug name.
        assert_eq!(
            provenant_routing::exclusion_detail(&reason),
            "scoped provider refusal"
        );
        Ok(())
    }

    #[test]
    fn exhausted_go_plan_binds_only_go_models() -> Result<(), Box<dyn Error>> {
        use provenant_quota::{AccountRecord, AccountScope, QuotaSnapshot, QuotaStatus};
        let account = AccountScope {
            adapter: "opencode".into(),
            account_key: "go".into(),
            label: "go".into(),
        };
        let mut record = AccountRecord::empty(account.clone());
        record.snapshot = Some(QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: QuotaStatus::Exhausted,
            observed_unix_seconds: 100,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: Vec::new(),
        });
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "opencode".into(),
            version: "fixture".into(),
            last_update: None,
            fixture_ref: "fixture".into(),
            catalogue_revision: "fixture".into(),
            account_key: Some("go".into()),
            observed_models: vec![
                "opencode-go/glm-5.1".into(),
                "opencode/glm-5.3-flash-free".into(),
                "openrouter/some-model".into(),
            ],
            observed_unix_seconds: 100,
            expires_unix_seconds: 200,
        }];
        let catalogue = super::Catalogue::bundled()?;
        let requested = [
            super::ModelId::parse("opencode/opencode-go/unlisted")?,
            super::ModelId::parse("opencode/opencode/unlisted")?,
            super::ModelId::parse("opencode/openrouter/unlisted")?,
        ];
        let policies = provenant_providers::quota_policies(provenant_providers::REGISTRY);
        let observed =
            super::quota_observations(&catalogue, &availability, &[record], &policies, &requested)?;
        let bound: Vec<_> = observed.iter().map(|o| o.model.as_str()).collect();
        assert!(bound.contains(&"opencode/opencode-go/glm-5.1"));
        assert!(bound.contains(&"opencode/opencode-go/unlisted"));
        assert!(!bound.contains(&"opencode/opencode/glm-5.3-flash-free"));
        assert!(!bound.contains(&"opencode/openrouter/some-model"));
        assert!(bound.iter().all(|m| m.starts_with("opencode/opencode-go/")));
        Ok(())
    }

    fn resolved(adapter: &str, model: &str, roll: u64) -> ResolvedRoute {
        let readiness = RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 200,
        };
        ResolvedRoute {
            extra: Default::default(),
            adapter: adapter.into(),
            model: model.into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: None,
            host: "fixture".into(),
            alternatives: RouteAlternatives::default(),
            exclusions: Vec::new(),
            eligibility: RouteEligibility::Eligible,
            readiness: readiness.clone(),
            confidentiality: RouteConfidentiality {
                extra: Default::default(),
                evidence_ref: "fixture".into(),
            },
            pool_selection: Some(RouteSelectionReceipt {
                extra: Default::default(),
                pool: "junior".into(),
                candidates: vec![RouteSelectionCandidate {
                    extra: Default::default(),
                    pacing_detail: None,
                    model: format!("{adapter}/{model}"),
                    weight: 1,
                    factor_basis_points: 10_000,
                    effort: None,
                    readiness: Some(readiness),
                    exclusion: None,
                }],
                roll,
                total_weight: 10_000,
                pick: format!("{adapter}/{model}"),
                reason: None,
                redraws: Vec::new(),
            }),
        }
    }

    #[test]
    fn admission_redraw_history_is_retained_in_the_pool_receipt() -> Result<(), Box<dyn Error>> {
        let first = resolved("codex", "paid", 17);
        let mut second = resolved("claude", "ready", 3);
        super::record_pool_redraw(&first, &mut second, "allowance exhausted")?;

        let mut third = resolved("agy", "ready", 9);
        third
            .pool_selection
            .as_mut()
            .ok_or("third receipt")?
            .candidates
            .push(RouteSelectionCandidate {
                extra: Default::default(),
                pacing_detail: None,
                model: "codex/paid".into(),
                weight: 1,
                factor_basis_points: 10_000,
                effort: None,
                readiness: None,
                exclusion: Some("admission rejected".into()),
            });
        super::record_pool_redraw(&second, &mut third, "scoped provider refusal")?;
        let value = serde_json::to_value(third)?;
        let redraws = &value["pool_selection"]["redraws"];
        assert_eq!(redraws[0]["rejected_pick"], "codex/paid");
        assert_eq!(redraws[0]["roll"], 17);
        assert_eq!(redraws[0]["reason"], "allowance exhausted");
        assert_eq!(redraws[1]["rejected_pick"], "claude/ready");
        assert_eq!(redraws[1]["roll"], 3);
        assert_eq!(redraws[1]["reason"], "scoped provider refusal");
        let rejected = value["pool_selection"]["candidates"]
            .as_array()
            .and_then(|candidates| candidates.iter().find(|row| row["model"] == "codex/paid"))
            .ok_or("rejected candidate")?;
        assert_eq!(rejected["exclusion"], "admission rejected");
        assert_eq!(rejected["readiness"]["status"], "ready");
        Ok(())
    }
    #[test]
    fn review_uses_unused_families_and_substitutes_when_exhausted() -> Result<(), Box<dyn Error>> {
        assert!(!super::review_family_excluded(None, &[]));
        assert!(super::review_family_excluded(None, &["openai".into()]));
        assert!(!super::review_family_excluded(
            Some("anthropic"),
            &["openai".into()]
        ));
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl provenant_core::PathResolver for Resolver {
            fn resolve_existing_ancestor(
                &self,
                path: &std::path::Path,
            ) -> std::io::Result<std::path::PathBuf> {
                Ok(path.into())
            }
        }
        let temp = tempfile::tempdir()?;
        let mut inputs =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("checkout"));
        inputs.state_dir = Some(temp.path().join("state"));
        let roots = inputs.resolve(&Resolver)?;
        std::fs::create_dir_all(roots.state.as_path())?;
        std::fs::set_permissions(
            roots.state.as_path(),
            std::fs::Permissions::from_mode(0o700),
        )?;
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']},{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        let availability: Vec<_> = [("codex", "gpt-6-luna"), ("claude", "claude-haiku-5-5")]
            .into_iter()
            .map(|(adapter, model)| provenant_providers::AvailabilityRecord {
                schema_version: 1,
                adapter: adapter.into(),
                executable: "/fixture/provider".into(),
                version: "fixture".into(),
                fixture_ref: "fixture".into(),
                catalogue_revision: catalogue.revision().into(),
                account_key: Some(format!("{adapter}-account")),
                observed_models: vec![model.into()],
                observed_unix_seconds: now,
                expires_unix_seconds: now + 300,
                last_update: None,
            })
            .collect();
        let access = [
            provenant_routing::AccessContext {
                adapter: "codex",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
            provenant_routing::AccessContext {
                adapter: "claude",
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            },
        ];
        let request = super::RouteRequest {
            read_heavy: false,
            model: None,
            route: Some("test".into()),
            effort: None,
            confidential: false,
            fallback: vec![],
            exclude: vec![],
        }
        .to_domain()?;
        let resolve = |excluded: &[String]| {
            super::resolve_dispatch(
                &request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                Some((excluded, false, &[])),
                &[],
            )
        };
        let first = resolve(&[])?;
        let first_id =
            provenant_routing::ModelId::parse(&format!("{}/{}", first.adapter, first.model))?;
        let family = catalogue
            .model(&first_id)
            .and_then(|row| row.family.clone())
            .ok_or("family")?;
        let second = resolve(std::slice::from_ref(&family))?;
        let second_id =
            provenant_routing::ModelId::parse(&format!("{}/{}", second.adapter, second.model))?;
        let other = catalogue
            .model(&second_id)
            .and_then(|row| row.family.clone())
            .ok_or("family")?;
        assert_ne!(family, other);
        let all = [family.clone(), other.clone()];
        let refused = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            Some((&all, true, &[])),
            &[],
        )
        .err()
        .ok_or("a required separation must not substitute")?;
        assert!(matches!(
            refused,
            super::DispatchError::FamilySeparation(..)
        ));
        // A sibling preference can relax while the writer exclusion remains hard.
        let composed = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability,
            &access,
            "fixture",
            Some((
                std::slice::from_ref(&family),
                true,
                std::slice::from_ref(&other),
            )),
            &[],
        )?;
        assert_eq!(composed.adapter, second.adapter);
        // The next family's provider is excluded: a required separation reports
        // none, and an optional one never substitutes the excluded provider.
        let rules = [provenant_routing::ExclusionRule {
            selector: provenant_routing::ExclusionSelector::Provider(second.adapter.clone()),
            source: provenant_routing::ExclusionSource::Flag,
        }];
        let with_rules = |required: bool| {
            super::resolve_dispatch(
                &request,
                &roots,
                None,
                ("fixture", 0),
                &availability,
                &access,
                "fixture",
                Some((std::slice::from_ref(&family), required, &[])),
                &rules,
            )
        };
        assert!(matches!(
            with_rules(true)
                .err()
                .ok_or("excluded family must not substitute")?,
            super::DispatchError::FamilySeparation(..)
        ));
        assert_ne!(with_rules(false)?.adapter, second.adapter);
        let fallback = resolve(&[family, other])?;
        assert!(matches!(
            fallback.readiness,
            provenant_records::RouteReadiness::Ready { .. }
        ));
        // The unused family remains selected when its availability is stale,
        // instead of silently relaxing to the ready, already-used family.
        let deferred = super::resolve_dispatch(
            &request,
            &roots,
            None,
            ("fixture", 0),
            &availability[..1],
            &access,
            "fixture",
            Some((&["openai".into()], false, &[])),
            &[],
        )?;
        assert_eq!(deferred.adapter, "claude");
        assert!(matches!(
            deferred.readiness,
            provenant_records::RouteReadiness::Unavailable { .. }
        ));
        Ok(())
    }

    /// A codex family already used by a review and an unused claude one, each
    /// with a single ready account.
    struct ReviewFixture {
        _temp: tempfile::TempDir,
        roots: provenant_core::Roots,
        availability: Vec<provenant_providers::AvailabilityRecord>,
        access: [provenant_routing::AccessContext<'static>; 2],
        request: provenant_routing::RouteRequest,
        now: i64,
    }
    impl ReviewFixture {
        fn new() -> Result<Self, Box<dyn Error>> {
            use std::os::unix::fs::PermissionsExt;
            struct Resolver;
            impl provenant_core::PathResolver for Resolver {
                fn resolve_existing_ancestor(
                    &self,
                    path: &std::path::Path,
                ) -> std::io::Result<std::path::PathBuf> {
                    Ok(path.into())
                }
            }
            let temp = tempfile::tempdir()?;
            let mut inputs = provenant_core::RootInputs::new(
                temp.path().join("home"),
                temp.path().join("checkout"),
            );
            inputs.state_dir = Some(temp.path().join("state"));
            let roots = inputs.resolve(&Resolver)?;
            std::fs::create_dir_all(roots.state.as_path())?;
            std::fs::set_permissions(
                roots.state.as_path(),
                std::fs::Permissions::from_mode(0o700),
            )?;
            let path = roots.state.as_path().join("config.toml");
            std::fs::write(
                &path,
                "schema_version=1\n[domains.routing.pools]\ntest=[{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']},{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
            )?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            let catalogue = super::Catalogue::bundled()?;
            let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
            let availability = [("codex", "gpt-6-luna"), ("claude", "claude-haiku-5-5")]
                .into_iter()
                .map(|(adapter, model)| provenant_providers::AvailabilityRecord {
                    schema_version: 1,
                    adapter: adapter.into(),
                    executable: "/fixture/provider".into(),
                    version: "fixture".into(),
                    fixture_ref: "fixture".into(),
                    catalogue_revision: catalogue.revision().into(),
                    account_key: Some(format!("{adapter}-account")),
                    observed_models: vec![model.into()],
                    observed_unix_seconds: now,
                    expires_unix_seconds: now + 300,
                    last_update: None,
                })
                .collect();
            let access = ["codex", "claude"].map(|adapter| provenant_routing::AccessContext {
                adapter,
                disclosure_ref: "fixture",
                confinement_ref: "fixture",
            });
            let request = super::RouteRequest {
                read_heavy: false,
                model: None,
                route: Some("test".into()),
                effort: None,
                confidential: false,
                fallback: vec![],
                exclude: vec![],
            }
            .to_domain()?;
            Ok(Self {
                _temp: temp,
                roots,
                availability,
                access,
                request,
                now,
            })
        }
        /// Resolve with the openai family already used by the review.
        fn resolve(
            &self,
            required: bool,
        ) -> Result<provenant_records::ResolvedRoute, super::DispatchError> {
            let used = [String::from("openai")];
            super::resolve_dispatch(
                &self.request,
                &self.roots,
                None,
                ("fixture", 0),
                &self.availability,
                &self.access,
                "fixture",
                Some((&used, required, &[])),
                &[],
            )
        }
        /// Persist the claude account's record, as the quota cache holds it.
        fn seed_claude(
            &self,
            edit: impl FnOnce(&mut provenant_quota::AccountRecord),
        ) -> Result<(), Box<dyn Error>> {
            self.seed("claude", edit)
        }
        /// Persist one provider account's record, as the quota cache holds it.
        fn seed(
            &self,
            adapter: &str,
            edit: impl FnOnce(&mut provenant_quota::AccountRecord),
        ) -> Result<(), Box<dyn Error>> {
            use std::os::unix::fs::PermissionsExt;
            let key = format!("{adapter}-account");
            let scope = provenant_quota::AccountScope {
                adapter: adapter.into(),
                account_key: key.clone(),
                label: key.clone(),
            };
            let mut record = provenant_quota::AccountRecord::empty(scope);
            edit(&mut record);
            let quota = self.roots.state.as_path().join("quota");
            let account = quota.join(&key);
            std::fs::create_dir_all(&account)?;
            for dir in [&quota, &account] {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            let file = account.join(format!("{adapter}.json"));
            std::fs::write(&file, serde_json::to_vec(&record)?)?;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
            Ok(())
        }
    }

    #[test]
    fn the_non_graph_run_path_carries_the_flag_into_rules_that_drop_the_provider()
    -> Result<(), Box<dyn Error>> {
        use provenant_routing::{ExclusionSelector, ExclusionSource};
        let fixture = ReviewFixture::new()?;
        // `run --route test --exclude claude` pushes this override; the task's
        // profile is built from it exactly as the dispatch builds it.
        let overrides = vec![format!(
            "routing.flag_exclude={}",
            serde_json::Value::from(vec!["claude"])
        )];
        let profile =
            provenant_config::read_with_domains(&fixture.roots, &crate::domains::SECTIONS)?
                .dispatch_profile(&overrides)?;
        let rules = super::task_exclusion_rules(&profile, &fixture.roots)?;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].source, ExclusionSource::Flag);
        assert_eq!(
            rules[0].selector,
            ExclusionSelector::Provider("claude".into())
        );
        let route = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            None,
            &rules,
        )?;
        assert_eq!(route.adapter, "codex");
        let receipt = route.pool_selection.ok_or("pool selection")?;
        assert!(receipt.candidates.iter().any(|candidate| {
            candidate.model == "claude/claude-haiku-5-5"
                && candidate.exclusion.as_deref() == Some("excluded by flag: provider claude")
        }));
        // Required family separation names the exclusions that left no member.
        let used = [String::from("openai")];
        let refused = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&used, true, &[])),
            &rules,
        )
        .unwrap_err();
        assert!(matches!(
            refused,
            super::DispatchError::FamilySeparation(..)
        ));
        assert!(
            refused
                .to_string()
                .contains("excluded by flag: provider claude"),
            "{refused}"
        );
        Ok(())
    }

    #[test]
    fn explain_reads_the_same_union_of_standing_and_flag_exclusions() -> Result<(), Box<dyn Error>>
    {
        use provenant_routing::ExclusionSource;
        let temp = tempfile::tempdir()?;
        let roots = exclusion_roots(&temp)?;
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['codex']\n",
        )?;
        let rules = super::effective_exclusion_rules(&["claude".to_owned()], &roots)?;
        let texts: Vec<_> = rules.iter().map(ToString::to_string).collect();
        assert_eq!(
            texts,
            [
                "excluded by config: provider codex",
                "excluded by flag: provider claude"
            ]
        );
        assert!(
            rules
                .iter()
                .any(|rule| rule.source == ExclusionSource::Config)
        );
        // A bad standing selector is refused the same as on the flag.
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['nosuchprovider']\n",
        )?;
        let error = super::effective_exclusion_rules(&[], &roots).unwrap_err();
        assert!(error.to_string().contains("valid forms"), "{error}");
        // A malformed list is refused rather than recovered to empty.
        std::fs::write(
            roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['claude', 5]\n",
        )?;
        assert!(super::effective_exclusion_rules(&[], &roots).is_err());
        Ok(())
    }

    #[test]
    fn review_substitution_skips_refused_unused_families() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        // The used family is the one codex serves, so Claude is the unused one.
        let unused = fixture.resolve(false)?;
        assert_eq!(unused.adapter, "claude");
        assert!(matches!(unused.readiness, RouteReadiness::Ready { .. }));
        // Its only account is refused: review relaxes to the ready, used family
        // rather than picking the refused member.
        crate::quota::persist_provider_refusal(
            fixture.roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("claude".into()),
                model: Some("claude/claude-haiku-5-5".into()),
                account_key: Some("claude-account".into()),
                account_label: Some("claude-account".into()),
                ..Default::default()
            },
            None,
            fixture.now,
            Some(fixture.now + 3_600),
        )?;
        let relaxed = fixture.resolve(false)?;
        assert_eq!(relaxed.adapter, "codex");
        assert!(matches!(relaxed.readiness, RouteReadiness::Ready { .. }));
        // A required separation never relaxes.
        assert!(matches!(
            fixture.resolve(true),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        // Relaxing the sibling preference must not exchange one refused
        // route for another unavailable route.
        crate::quota::persist_provider_refusal(
            fixture.roots.state.as_path(),
            &provenant_records::run::RouteRecord {
                provider: Some("codex".into()),
                model: Some("codex/gpt-6-luna".into()),
                account_key: Some("codex-account".into()),
                account_label: Some("codex-account".into()),
                ..Default::default()
            },
            None,
            fixture.now,
            Some(fixture.now + 3_600),
        )?;
        let selected = super::resolve_dispatch(
            &fixture.request,
            &fixture.roots,
            None,
            ("fixture", 0),
            &fixture.availability,
            &fixture.access,
            "fixture",
            Some((&[], false, &["openai".into()])),
            &[],
        )?;
        assert_eq!(selected.adapter, "claude");
        Ok(())
    }

    #[test]
    fn review_substitution_skips_a_mismatched_unused_family() -> Result<(), Box<dyn Error>> {
        let fixture = ReviewFixture::new()?;
        fixture.seed_claude(|record| {
            record.last_error = Some(provenant_quota::ProbeFailure::AccountMismatch);
        })?;
        let relaxed = fixture.resolve(false)?;
        assert_eq!(relaxed.adapter, "codex");
        assert!(matches!(relaxed.readiness, RouteReadiness::Ready { .. }));
        assert!(matches!(
            fixture.resolve(true),
            Err(super::DispatchError::FamilySeparation(..))
        ));
        Ok(())
    }

    #[test]
    fn review_substitution_keeps_a_snapshot_exhausted_unused_family() -> Result<(), Box<dyn Error>>
    {
        // Snapshot exhaustion is not an account block: an unused family whose
        // reading is spent stays selected, as it did before account evidence.
        let fixture = ReviewFixture::new()?;
        let now = fixture.now;
        fixture.seed_claude(|record| {
            let account = record.account.clone();
            record.snapshot = Some(provenant_quota::QuotaSnapshot {
                schema_version: 1,
                account,
                revision: 0,
                status: provenant_quota::QuotaStatus::Exhausted,
                observed_unix_seconds: now,
                source_ref: "fixture".into(),
                authority_ref: None,
                windows: vec![provenant_quota::QuotaWindow {
                    pool: "seven_day".into(),
                    window_seconds: Some(604_800),
                    used_basis_points: 10_000,
                    resets_unix_seconds: Some(now + 3_600),
                }],
                details: None,
            });
        })?;
        let route = fixture.resolve(false)?;
        assert_eq!(route.adapter, "claude");
        assert!(matches!(
            route.readiness,
            RouteReadiness::Unavailable {
                reason: provenant_records::RouteExclusionReason::Exhausted,
                ..
            }
        ));
        Ok(())
    }

    /// A one-member `principal` pool, with the member's account record seeded.
    fn blocked_fixture(
        edit: impl FnOnce(&mut provenant_quota::AccountRecord),
    ) -> Result<ReviewFixture, Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ReviewFixture::new()?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        fixture.seed_claude(edit)?;
        Ok(fixture)
    }

    #[test]
    fn route_blocked_is_quiet_for_a_usable_route() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(|_| {})?;
        assert_eq!(super::route_blocked("principal", &[], &fixture.roots), None);
        Ok(())
    }

    #[test]
    fn route_blocked_names_an_excluded_principal() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(|_| {})?;
        let flag = super::route_blocked("principal", &["claude".to_owned()], &fixture.roots);
        assert!(flag.is_some(), "{flag:?}");
        // A configured exclusion blocks the same way.
        std::fs::write(
            fixture.roots.state.as_path().join("config.toml"),
            "schema_version=1\n[knobs]\n'routing.exclude'=['claude']\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        let configured = super::route_blocked("principal", &[], &fixture.roots);
        assert!(configured.is_some(), "{configured:?}");
        Ok(())
    }

    /// Mark the account's allowance spent until it resets in an hour.
    fn exhaust(record: &mut provenant_quota::AccountRecord) {
        let account = record.account.clone();
        let now = provenant_core::Clock::unix_seconds(&provenant_os::clock::SystemClock::new());
        record.snapshot = Some(provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account,
            revision: 0,
            status: provenant_quota::QuotaStatus::Exhausted,
            observed_unix_seconds: now,
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![provenant_quota::QuotaWindow {
                pool: "seven_day".into(),
                window_seconds: Some(604_800),
                used_basis_points: 10_000,
                resets_unix_seconds: Some(now + 3_600),
            }],
            details: None,
        });
    }

    /// A two-member `principal` pool, one member per provider, each with its
    /// account record seeded. Whichever the resolver picks first, the other
    /// is the retry.
    fn pair_fixture(
        claude: impl FnOnce(&mut provenant_quota::AccountRecord),
        codex: impl FnOnce(&mut provenant_quota::AccountRecord),
    ) -> Result<ReviewFixture, Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = ReviewFixture::new()?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\nprincipal=[{model='claude/claude-haiku-5-5',weight=1,effort='high',efforts=['high']},{model='codex/gpt-6-luna',weight=1,effort='high',efforts=['high']}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        fixture.seed("claude", claude)?;
        fixture.seed("codex", codex)?;
        Ok(fixture)
    }

    #[test]
    fn route_blocked_looks_past_an_exhausted_member_to_a_usable_one() -> Result<(), Box<dyn Error>>
    {
        // Dispatch retries the next member, so either one spent is no block.
        for spent_claude in [true, false] {
            let fixture = pair_fixture(
                |record| {
                    if spent_claude {
                        exhaust(record)
                    }
                },
                |record| {
                    if !spent_claude {
                        exhaust(record)
                    }
                },
            )?;
            assert_eq!(
                super::route_blocked("principal", &[], &fixture.roots),
                None,
                "claude spent: {spent_claude}"
            );
        }
        Ok(())
    }

    #[test]
    fn route_blocked_names_every_member_when_all_are_exhausted() -> Result<(), Box<dyn Error>> {
        let fixture = pair_fixture(exhaust, exhaust)?;
        let reason = super::route_blocked("principal", &[], &fixture.roots)
            .ok_or("a pool with every member spent must block")?;
        assert!(reason.contains("claude/claude-haiku-5-5"), "{reason}");
        assert!(reason.contains("codex/gpt-6-luna"), "{reason}");
        Ok(())
    }

    #[test]
    fn route_blocked_ends_for_a_single_model_route_with_every_account_spent()
    -> Result<(), Box<dyn Error>> {
        use std::os::unix::fs::PermissionsExt;
        let fixture = blocked_fixture(exhaust)?;
        let path = fixture.roots.state.as_path().join("config.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.routes.solo]\nmodel='claude/claude-haiku-5-5'\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        // A resolver that returns the same model for ever would hang this
        // call, so bound it.
        let (sender, receiver) = std::sync::mpsc::channel();
        let roots = fixture.roots.clone();
        std::thread::spawn(move || {
            let _ = sender.send(super::route_blocked("solo", &[], &roots));
        });
        let blocked = receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .map_err(|_| "route_blocked did not end")?;
        let reason = blocked.ok_or("a spent single-model route must block")?;
        assert!(reason.contains("allowance exhausted"), "{reason}");
        assert_eq!(
            reason.matches("claude/claude-haiku-5-5").count(),
            1,
            "{reason}"
        );
        Ok(())
    }

    #[test]
    fn route_blocked_names_an_exhausted_principal() -> Result<(), Box<dyn Error>> {
        let fixture = blocked_fixture(exhaust)?;
        let blocked = super::route_blocked("principal", &[], &fixture.roots);
        let reason = blocked.ok_or("an exhausted principal must block")?;
        assert!(reason.contains("claude"), "{reason}");
        assert!(reason.contains("allowance exhausted"), "{reason}");
        Ok(())
    }
}
