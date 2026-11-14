//! Routing's frozen clap leaves and read-only composition/rendering edge.
mod config;
mod output;
mod resolution;

use crate::{Globals, metadata::CatalogueArg};
use clap::{Args, Subcommand, ValueEnum};
use config::*;
#[cfg(test)]
use output::*;
pub use output::{route_blocked, run};
use provenant_core::{Clock, Roots, WallTime};
use provenant_os::clock::SystemClock;
use provenant_os::fs::FsError;
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{CommandError, CommandResult, Outcome, RouteReadiness};
use provenant_routing::{
    AccessContext, BoundedModels, Catalogue, Fallback, ModelId, PacingPolicy, QuotaObservation,
    RouteChoice, RouteName, RoutePoolMember, RoutingContext, RoutingError,
};
#[cfg(feature = "fixture")]
pub use resolution::fixture_resolve_dispatch;
use resolution::*;
pub(crate) use resolution::{
    DispatchError, check_dispatch_candidate, recorded_model_readiness,
    resolve_dispatch_rejecting_family, selection_evidence,
};
pub use resolution::{depletion_facts, resolve_review_family};
use serde::Serialize;
use std::{collections::BTreeSet, io, str::FromStr};

/// The same effective catalogue and derived account set used by routing.
/// This read never probes a provider or filters on quota/readiness.
pub fn provider_check_routes(
    roots: &Roots,
) -> Result<
    (
        Box<provenant_routing::RouteList>,
        provenant_providers::accounts::Settings,
    ),
    String,
> {
    config::list_data_and_accounts(
        roots,
        &RouteListRequest {
            valid: false,
            adapter: None,
            pool: None,
        },
    )
    .map(|(routes, accounts)| (routes, accounts.with_derived_accounts()))
    .map_err(|error| error.to_string())
}

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
    List(OverrideList),
    /// Remove pacing overrides: one scope's, or every one without a selector.
    Clear(OverrideClear),
}
#[derive(Debug, Args)]
pub struct OverrideList {
    /// Include expired overrides.
    #[arg(long)]
    pub all: bool,
}
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("scope").args(["account", "provider", "all"])))]
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
    #[arg(long, required_unless_present = "model")]
    pub policy: Option<PacingChoice>,
    /// Exact preferred model for a risk/task stratum.
    #[arg(long, conflicts_with_all = ["account", "provider", "all", "policy", "starts", "window", "tz"], requires_all = ["risk_tier", "task_kind", "until"])]
    pub model: Option<Model>,
    #[arg(long, requires = "model")]
    pub risk_tier: Option<String>,
    #[arg(long, requires = "model")]
    pub task_kind: Option<String>,
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
    /// Revoke the model preference for a risk and task stratum.
    #[arg(long, requires = "task_kind", conflicts_with_all = ["account", "provider"])]
    pub risk_tier: Option<String>,
    #[arg(long, requires = "risk_tier", conflicts_with_all = ["account", "provider"])]
    pub task_kind: Option<String>,
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
    Admission(String),
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
impl AccountProfileError {
    fn kind(self) -> Option<provenant_run::ErrorKind> {
        match self {
            Self::Unknown => Some(provenant_run::ErrorKind::UnknownAccount),
            Self::Ambiguous => None,
            Self::Duplicate => Some(provenant_run::ErrorKind::DuplicatePacingProfile),
        }
    }

    fn code(self) -> &'static str {
        self.kind().map_or("CONFIG", |kind| kind.code())
    }
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
    fn model_choice(row: &provenant_routing::ModelChoiceOverride, now: WallTime) -> Self {
        let schedule = provenant_routing::Schedule {
            expires: row.expires,
            ..Default::default()
        };
        Self {
            kind: "model_choice",
            scope: format!("{} {} {}", row.dimension, row.risk_tier, row.task_kind),
            value: row.preferred.as_str().into(),
            active: schedule.active(now),
            remaining_seconds: schedule.remaining(now),
            schedule,
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_refusal: Option<String>,
    pub provider_refusals: Vec<String>,
}
#[derive(Serialize)]
pub struct RecordedReadiness {
    pub host: String,
    pub account: String,
    pub account_key: String,
    pub model: String,
    pub observation: provenant_quota::ReadinessRead,
}

#[cfg(test)]
mod route_tests {
    use provenant_records::{
        ResolvedRoute, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteReadiness,
        RouteSelectionCandidate, RouteSelectionReceipt,
    };
    use std::error::Error;

    include!("route/tests_1.rs");
    include!("route/tests_2.rs");
    include!("route/tests_3.rs");
}
