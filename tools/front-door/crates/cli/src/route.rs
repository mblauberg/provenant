//! Routing's frozen clap leaves and read-only composition/rendering edge.
use crate::Globals;
use clap::{Args, Subcommand, ValueEnum};
use provenant_core::{Roots, WallTime};
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
    /// Set a provider policy or manage a temporary pacing override.
    Pacing(RoutePacing),
}
#[derive(Debug, Args)]
pub struct RouteListRequest {
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
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
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
    /// Registered provider ID, or `override` to manage an override.
    pub target: String,
    /// Provider policy, or `clear` after `override`.
    pub value: Option<String>,
    /// Apply an override to this account key.
    #[arg(long, conflicts_with_all = ["provider", "all"])]
    pub account: Option<String>,
    /// Apply an override to this provider.
    #[arg(long, conflicts_with_all = ["account", "all"])]
    pub provider: Option<String>,
    /// Apply an override to all accounts.
    #[arg(long, conflicts_with_all = ["account", "provider"])]
    pub all: bool,
    /// Override policy.
    #[arg(long)]
    pub policy: Option<PacingChoice>,
    /// Override expiry as an RFC 3339 timestamp or duration (e.g. 2h, 1d).
    #[arg(long)]
    pub until: Option<String>,
}
#[derive(Debug, Args)]
pub struct RouteResolve {
    /// Route name, ADAPTER/ID model, or a model ID given without its adapter
    #[arg(conflicts_with_all = ["model", "route"])]
    pub name: Option<String>,
    #[command(flatten)]
    pub request: RouteRequest,
}
#[derive(Debug, Clone, Args)]
pub struct RouteRequest {
    /// Exact model in ADAPTER/ID form
    #[arg(long, conflicts_with = "route")]
    pub model: Option<Model>,
    /// Named route
    #[arg(long)]
    pub route: Option<String>,
    /// Reasoning effort
    #[arg(long)]
    pub effort: Option<String>,
    /// Require a confidential-safe route
    #[arg(long)]
    pub confidential: bool,
    /// Allow free-tier models
    #[arg(long)]
    pub allow_free: bool,
    /// Fallback models in ADAPTER/ID form, comma separated
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Set)]
    pub fallback: Vec<Model>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model(pub String);
impl FromStr for Model {
    type Err = &'static str;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        ModelId::parse(text).map_err(|_| "model must be ADAPTER/ID")?;
        Ok(Self(text.into()))
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
            choice,
            effort: self.effort.clone(),
            confidential: self.confidential,
            allow_free: self.allow_free,
            fallback,
        })
    }
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
    fn edits(a: &str, b: &str) -> usize {
        let b: Vec<char> = b.chars().collect();
        let mut row: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.chars().enumerate() {
            let mut diagonal = row[0];
            row[0] = i + 1;
            for (j, cb) in b.iter().enumerate() {
                let above = row[j + 1];
                row[j + 1] = (diagonal + usize::from(ca != *cb))
                    .min(row[j] + 1)
                    .min(above + 1);
                diagonal = above;
            }
        }
        row[b.len()]
    }
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
            || edits(lower, id) <= 2
            || edits(lower, &candidate) <= 2
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
    Ok(request.to_domain()?)
}
#[derive(Debug, thiserror::Error)]
enum EdgeError {
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
    #[error("{0}")]
    AccountProfile(String),
}
impl From<provenant_config::ConfigError> for EdgeError {
    fn from(error: provenant_config::ConfigError) -> Self {
        let is_pacing = match &error {
            provenant_config::ConfigError::Schema { location, .. }
            | provenant_config::ConfigError::Domain { location, .. } => {
                location.key_path.starts_with("domains.routing.pacing")
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
    List(Box<provenant_routing::RouteList>),
    Resolved(Box<RoutePreview>),
}
#[derive(Serialize)]
pub struct RoutePreview {
    #[serde(flatten)]
    pub route: provenant_records::ResolvedRoute,
    pub supported_efforts: Option<Vec<String>>,
    pub nearest_models: Vec<String>,
    pub pacing: Vec<provenant_routing::PacingPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<provenant_quota::QuotaSnapshot>,
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
    requested_models: &[ModelId],
) -> Result<Vec<QuotaObservation>, EdgeError> {
    let policies = provenant_providers::quota_policies(REGISTRY);
    let mut observations = Vec::new();
    for record in records {
        let adapter = record.account.adapter.as_str();
        let Some(snapshot) = record.snapshot.as_ref() else {
            continue;
        };
        let Some(ttl) = provenant_quota::schedule::interval(
            &policies,
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
        [] => Err(EdgeError::AccountProfile(format!(
            "{field}: unknown account `{name}`; valid accounts: {}",
            if valid.is_empty() {
                "(none registered)"
            } else {
                &valid
            }
        ))),
        _ => Err(EdgeError::AccountProfile(format!(
            "{field}: account `{name}` is ambiguous: {}; use adapter/key or a unique label",
            matches
                .iter()
                .map(|(id, entry)| format!(
                    "{}/{} (label: {}, entry: {})",
                    entry.adapter, entry.key, entry.label, id
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
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
            return Err(EdgeError::AccountProfile(format!(
                "multiple pacing profiles resolve to account key `{key}`"
            )));
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
    action: impl FnOnce(&RoutingContext<'_>, &[provenant_quota::AccountRecord]) -> Result<T, EdgeError>,
) -> Result<T, EdgeError> {
    let domains = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?.domains;
    let mut config = domains.routing;
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
    let timestamp = jiff::Timestamp::now();
    domains
        .quota
        .apply_calibration(&mut quota_records, timestamp.as_second());
    let quota = quota_observations(&catalogue, availability, &quota_records, requested_models)?;
    let capacity_cooldowns: Vec<_> = quota_records
        .iter()
        .flat_map(|record| record.capacity_cooldowns.iter().cloned())
        .collect();
    let now = WallTime::from_micros(timestamp.as_microsecond())?;
    action(
        &RoutingContext {
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability,
            quota: &quota,
            capacity_cooldowns: &capacity_cooldowns,
            access,
            now,
            host,
        },
        &quota_records,
    )
}
/// Resolve one production dispatch against fresh provider facts and validated
/// task access. Account aliases must already have been mapped to registered keys.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    excluded_families: Option<&[String]>,
) -> Result<provenant_records::ResolvedRoute, String> {
    let catalogue = Catalogue::bundled().map_err(|error| error.to_string())?;
    let excluded: Vec<ModelId> = catalogue
        .models()
        .iter()
        .filter(|model| {
            excluded_families
                .is_some_and(|families| review_family_excluded(model.family.as_deref(), families))
        })
        .map(|model| model.model.clone())
        .collect();
    let selected = resolve_dispatch_excluding(
        request,
        roots,
        account,
        seed,
        availability,
        access,
        host,
        &excluded,
    );
    let exhausted =
        matches!(&selected, Err(error) if error.starts_with("no eligible pool member:"));
    if !excluded.is_empty() && exhausted {
        resolve_dispatch_excluding(
            request,
            roots,
            account,
            seed,
            availability,
            access,
            host,
            &[],
        )
    } else {
        selected
    }
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
) -> Result<provenant_records::ResolvedRoute, String> {
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
        |context, records| {
            let mut excluded_members = initial_exclusions.to_vec();
            let mut refusals = Vec::new();
            let mut resolved = provenant_routing::resolve_for_account_with_seed_excluding(
                request,
                context,
                account,
                seed.0,
                seed.1,
                &excluded_members,
            )?;
            loop {
                let model = ModelId::parse(&format!("{}/{}", resolved.adapter, resolved.model))?;
                let pools = free_pools(&resolved.adapter, || {
                    context.catalogue.is_free_model(&model)
                });
                let now_unix_seconds = context.now.as_timestamp().as_second();
                let policies = provenant_providers::quota_policies(context.registry);
                let admission = provenant_quota::Admission {
                    policies: &policies,
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
                        admission.check(&resolved).map_err(EdgeError::from)
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = gate {
                    let is_go_refusal = matches!(&error, EdgeError::GoAllowance(_));
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
    .map_err(|error| error.to_string())
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
        rejected_pick: previous.pick.clone(),
        roll: previous.roll,
        total_weight: previous.total_weight,
        reason: reason.into(),
    });
    current.redraws = redraws;
    Ok(())
}
/// Only OpenCode draws on the keyless free allowance, whatever a pool lists.
fn free_pools(adapter: &str, free: impl FnOnce() -> bool) -> Vec<String> {
    provenant_records::AdapterSpec::get(adapter)
        .and_then(|spec| spec.free_pool)
        .filter(|_| free())
        .map(|pool| vec![pool.to_owned()])
        .unwrap_or_default()
}

/// Recheck each selected fallback against the latest persisted refusals.
pub(crate) fn check_dispatch_candidate(
    roots: &Roots,
    route: &provenant_records::ResolvedRoute,
) -> Result<(), String> {
    let catalogue = Catalogue::bundled().map_err(|e| e.to_string())?;
    let model =
        ModelId::parse(&format!("{}/{}", route.adapter, route.model)).map_err(|e| e.to_string())?;
    let domains = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)
        .map_err(|e| e.to_string())?
        .domains;
    provenant_routing::check_go_allowance(&model, &domains.routing, &catalogue)?;
    let pools = free_pools(&route.adapter, || catalogue.is_free_model(&model));
    let now = jiff::Timestamp::now().as_second();
    let mut records = crate::quota::read_cache(roots).map_err(|e| e.to_string())?;
    domains.quota.apply_calibration(&mut records, now);
    provenant_quota::Admission {
        policies: &provenant_providers::quota_policies(REGISTRY),
        now_unix_seconds: now,
        records: &records,
        pools: &pools,
    }
    .check(route)
    .map_err(|e| e.to_string())
}

fn list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<Box<provenant_routing::RouteList>, EdgeError> {
    with_context(roots, &[], &[], "local-preview", &[], |context, _| {
        Ok(Box::new(provenant_routing::list(
            &provenant_routing::RouteListQuery {
                adapter: query.adapter.clone(),
                pool: query.pool.clone(),
            },
            context,
        )?))
    })
}

fn effective_members(roots: &Roots, pool: &str) -> Result<Vec<RoutePoolMember>, EdgeError> {
    let query = RouteListRequest {
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
        |document| edit(document).map_err(|reason| provenant_config::ConfigError::Edit { reason }),
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

fn set_pacing(roots: &Roots, request: &RoutePacing) -> Result<(), EdgeError> {
    if request.target == "override" {
        return set_pacing_override(roots, request);
    }
    if request.account.is_some()
        || request.provider.is_some()
        || request.all
        || request.policy.is_some()
        || request.until.is_some()
    {
        return Err(EdgeError::Pacing(
            "override options require `route pacing override`",
        ));
    }
    let policy = match request.value.as_deref() {
        Some("pace") => "pace",
        Some("spend") => "spend",
        Some("max") => "max",
        Some("off") => "off",
        _ => {
            return Err(EdgeError::Pacing(
                "expected a pacing policy: pace, spend, max or off",
            ));
        }
    };
    if !REGISTRY
        .iter()
        .any(|provider| provider.adapter.id() == request.target)
    {
        return Err(RoutingError::UnknownAdapter.into());
    }
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let pacing = routing
            .entry("pacing")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let pacing = config_table(pacing).ok_or("domains.routing.pacing must be a table")?;
        pacing.insert(&request.target, toml_edit::value(policy));
        Ok(())
    })
}

fn override_expiry(value: &str) -> Option<i64> {
    if let Ok(timestamp) = value.parse::<jiff::Timestamp>() {
        return Some(timestamp.as_second());
    }
    let (digits, multiplier) = match value.as_bytes().last()? {
        b's' => (&value[..value.len() - 1], 1),
        b'm' => (&value[..value.len() - 1], 60),
        b'h' => (&value[..value.len() - 1], 3600),
        b'd' => (&value[..value.len() - 1], 86_400),
        _ => return None,
    };
    let seconds = digits.parse::<i64>().ok()?.checked_mul(multiplier)?;
    (seconds > 0)
        .then(|| jiff::Timestamp::now().as_second().checked_add(seconds))
        .flatten()
}

fn set_pacing_override(roots: &Roots, request: &RoutePacing) -> Result<(), EdgeError> {
    if request.value.as_deref() == Some("clear") {
        if request.account.is_some()
            || request.provider.is_some()
            || request.all
            || request.policy.is_some()
            || request.until.is_some()
        {
            return Err(EdgeError::Pacing("override clear takes no options"));
        }
        return edit_config(roots, |document| {
            routing_table(document)?.remove("pacing_overrides");
            Ok(())
        });
    }
    if request.value.is_some()
        || usize::from(request.account.is_some())
            + usize::from(request.provider.is_some())
            + usize::from(request.all)
            != 1
    {
        return Err(EdgeError::Pacing(
            "select exactly one of --account, --provider or --all",
        ));
    }
    let policy = match request
        .policy
        .ok_or(EdgeError::Pacing("override requires --policy"))?
    {
        PacingChoice::Pace => "pace",
        PacingChoice::Spend => "spend",
        PacingChoice::Max => "max",
        PacingChoice::Off => "off",
    };
    let expiry = request
        .until
        .as_deref()
        .and_then(override_expiry)
        .filter(|expiry| *expiry > jiff::Timestamp::now().as_second())
        .ok_or(EdgeError::Pacing(
            "--until must be a future RFC 3339 timestamp or positive duration",
        ))?;
    let settings = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .accounts;
    let account = request
        .account
        .as_deref()
        .map(|name| resolve_account_reference(name, "--account", &settings))
        .transpose()?;
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let mut values = match routing.remove("pacing_overrides") {
            None => toml_edit::Array::new(),
            Some(toml_edit::Item::Value(toml_edit::Value::Array(values))) => values,
            Some(toml_edit::Item::ArrayOfTables(tables)) => {
                let mut values = toml_edit::Array::new();
                for table in tables.iter() {
                    values.push(toml_edit::Value::InlineTable(
                        table.clone().into_inline_table(),
                    ));
                }
                values
            }
            _ => return Err("domains.routing.pacing_overrides must be an array of tables"),
        };
        values.retain(|value| {
            let Some(row) = value.as_inline_table() else {
                return true;
            };
            let existing = row
                .get("account")
                .and_then(toml_edit::Value::as_str)
                .map(|name| {
                    resolve_account_reference(name, "account", &settings)
                        .unwrap_or_else(|_| name.to_owned())
                });
            existing.as_deref() != account.as_deref()
                || row.get("provider").and_then(toml_edit::Value::as_str)
                    != request.provider.as_deref()
        });
        let mut row = toml_edit::InlineTable::new();
        if let Some(account) = &account {
            row.insert("account", toml_edit::Value::from(account.as_str()));
        }
        if let Some(provider) = &request.provider {
            row.insert("provider", toml_edit::Value::from(provider.as_str()));
        }
        row.insert("policy", toml_edit::Value::from(policy));
        row.insert("expires_unix_seconds", toml_edit::Value::from(expiry));
        values.push(toml_edit::Value::InlineTable(row));
        routing.insert(
            "pacing_overrides",
            toml_edit::Item::Value(toml_edit::Value::Array(values)),
        );
        Ok(())
    })
}

fn execute(command: &Route, roots: &Roots) -> Result<Data, EdgeError> {
    match command {
        Route::List(query) => Ok(Data::List(list_data(roots, query)?)),
        Route::Resolve(query) => {
            with_context(roots, &[], &[], "local-preview", &[], |context, records| {
                let route = provenant_routing::resolve(&resolve_request(query, context)?, context)?;
                let model = ModelId::parse(&format!("{}/{}", route.adapter, route.model))?;
                provenant_routing::check_go_allowance(&model, context.config, context.catalogue)
                    .map_err(EdgeError::GoAllowance)?;
                let quota = records
                    .iter()
                    .find(|r| {
                        r.account.adapter == route.adapter
                            && (route.account_key.as_deref()
                                == Some(r.account.account_key.as_str())
                                || (route.account_key.is_none()
                                    && route.adapter == "opencode"
                                    && snapshot_governs(
                                        &route.adapter,
                                        &r.account.account_key,
                                        &model,
                                    )))
                    })
                    .and_then(|r| r.snapshot.clone());
                let pacing = provenant_routing::pacing_preview(&model, context);
                let entry = context.catalogue.model(&model);
                Ok(Data::Resolved(Box::new(RoutePreview {
                    route,
                    supported_efforts: entry.map(|entry| entry.efforts.clone()),
                    nearest_models: if entry.is_none() {
                        context.catalogue.nearest_models(&model)
                    } else {
                        Vec::new()
                    },
                    pacing,
                    quota,
                })))
            })
        }
        Route::Set(request) => {
            set_pool(roots, request)?;
            Ok(Data::List(list_data(
                roots,
                &RouteListRequest {
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Remove(request) => {
            remove_pool_member(roots, request)?;
            Ok(Data::List(list_data(
                roots,
                &RouteListRequest {
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Reset(request) => {
            reset_pool(roots, request)?;
            Ok(Data::List(list_data(
                roots,
                &RouteListRequest {
                    adapter: None,
                    pool: Some(request.pool.clone()),
                },
            )?))
        }
        Route::Pacing(request) => {
            set_pacing(roots, request)?;
            Ok(Data::List(list_data(
                roots,
                &RouteListRequest {
                    adapter: None,
                    pool: None,
                },
            )?))
        }
    }
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
    };
    let result = match execute(&command, roots) {
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
                EdgeError::Config(_)
                | EdgeError::PacingConfig(_)
                | EdgeError::AccountProfile(_) => (Outcome::Invalid, "CONFIG"),
                _ => (Outcome::Error, "IO"),
            };
            let mut result = CommandResult::new(name, status, None);
            result.error = Some(CommandError {
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
                lines.extend(
                    list.models
                        .iter()
                        .map(|m| format!("{} efforts=[{}]", m.model.as_str(), m.efforts.join(","))),
                );
                lines.extend(
                    list.named_routes
                        .iter()
                        .map(|(name, r)| format!("route {name} = {}", r.model.as_str())),
                );
                lines.extend(list.account_pacing.iter().map(|entry| {
                    format!(
                        "account {}/{} pacing={:?} source={} profile={:?} expires={:?}",
                        entry.adapter,
                        entry.account,
                        entry.policy,
                        entry.source,
                        entry.profile,
                        entry.expires_unix_seconds,
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
                lines.extend(list.pools.iter().map(|(name, members)| {
                    let models = members
                        .iter()
                        .map(|member| {
                            format!(
                                "{} weight={} pacing={:?}{}",
                                member.model.as_str(),
                                member.weight,
                                member.pacing,
                                member
                                    .effort
                                    .as_ref()
                                    .map_or(String::new(), |effort| format!(" effort={effort}")),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("route pool {name} = {models}")
                }));
                format!("{}\n", lines.join("\n"))
            }
            Some(Data::Resolved(preview)) => {
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
                    "{}/{} effort={} {:?}{}{}\n",
                    route.adapter,
                    route.model,
                    route.effort.as_deref().unwrap_or("unspecified"),
                    route.readiness,
                    route
                        .pool_selection
                        .as_ref()
                        .map_or_else(String::new, |receipt| {
                            let details = receipt
                                .candidates
                                .iter()
                                .filter_map(|candidate| {
                                    candidate
                                        .pacing_detail
                                        .as_ref()
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
                match &preview.supported_efforts {
                    Some(efforts) if efforts.is_empty() => {
                        text.push_str("supported efforts: none\n")
                    }
                    Some(efforts) => {
                        text.push_str(&format!("supported efforts: {}\n", efforts.join(", ")))
                    }
                    None => {
                        text.push_str(
                            "model is not in the catalogue; effort support is unrecorded\n",
                        );
                        if !preview.nearest_models.is_empty() {
                            text.push_str(&format!(
                                "nearest catalogued IDs: {}\n",
                                preview.nearest_models.join(", ")
                            ));
                        }
                    }
                }
                if let Some(details) = preview.quota.as_ref().and_then(|q| q.details.as_ref())
                    && let (Some(source), Some(coverage)) = (&details.source, &details.coverage)
                {
                    text.push_str(&format!("quota source: {source}, coverage: {coverage}\n"));
                }
                text
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
            ];
            let rows = list
                .named_routes
                .iter()
                .map(|(name, r)| format!("route {name} = {}", r.model.as_str()))
                .chain(list.pools.keys().map(|name| format!("pool {name}")))
                .collect();
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
        _ => None,
    };
    let mode = globals.render_mode(
        &human,
        matches!(command, Route::List(_) | Route::Resolve(_)),
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
            let error = super::resolve_pacing_accounts(&mut config, &settings)
                .unwrap_err()
                .to_string();
            for expected in [
                "ambiguous",
                "claude/shared",
                "codex/other",
                "agy/shared",
                "adapter/key",
            ] {
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
        assert!(
            super::resolve_account_reference("shared", "test", &settings)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        for (input, expected) in [
            ("office", "claude/shared"),
            ("claude/shared", "claude/shared"),
        ] {
            let mut config: RoutingConfig =
                serde_json::from_value(serde_json::json!({"pacing_overrides": [{
                    "account": input, "policy": "pace", "expires_unix_seconds": 2000000000_i64
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
        assert_eq!(super::free_pools("opencode", || true), ["opencode-free"]);
        assert!(super::free_pools("opencode", || false).is_empty());
        assert!(super::free_pools("codex", || true).is_empty());
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
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/claude-haiku-5-5',weight=1},{model='opencode/opencode-go/glm-5.3-flash',weight=0}]\n",
        )?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let catalogue = super::Catalogue::bundled()?;
        let now = jiff::Timestamp::now().as_second();
        let availability = [provenant_providers::AvailabilityRecord {
            schema_version: 1,
            adapter: "opencode".into(),
            executable: "/fixture/opencode".into(),
            version: "fixture".into(),
            fixture_ref: "fixture".into(),
            catalogue_revision: catalogue.revision().into(),
            account_key: Some("go".into()),
            observed_models: vec![
                "opencode-go/claude-haiku-5-5".into(),
                "opencode-go/glm-5.3-flash".into(),
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
                model: model.map(|m| super::Model(m.into())),
                route: route.map(str::to_owned),
                effort: None,
                confidential: false,
                allow_free: false,
                fallback: vec![],
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
            )
        };
        let preview = super::Route::Resolve(super::RouteResolve {
            name: None,
            request: super::RouteRequest {
                model: Some(super::Model("opencode/opencode-go/claude-haiku-5-5".into())),
                route: None,
                effort: None,
                confidential: false,
                allow_free: false,
                fallback: vec![],
            },
        });
        match super::execute(&preview, &roots) {
            Err(error) => assert!(error.to_string().contains("$15")),
            Ok(_) => panic!("preview must refuse the model"),
        }
        let error = resolve(&request(
            Some("opencode/opencode-go/claude-haiku-5-5"),
            None,
        )?)
        .unwrap_err();
        assert!(error.contains("claude-haiku-5-5") && error.contains("$15"));
        let error = resolve(&request(Some("opencode/opencode-go/unlisted"), None)?).unwrap_err();
        assert!(error.contains("opencode-go/unlisted") && error.contains("unknown"));
        let fallback = |model: Option<&str>, route: Option<&str>| -> Result<(), Box<dyn Error>> {
            let request = super::RouteRequest {
                model: model.map(|m| super::Model(m.into())),
                route: route.map(str::to_owned),
                effort: None,
                confidential: false,
                allow_free: false,
                fallback: vec![super::Model("opencode/opencode-go/glm-5.3-flash".into())],
            };
            let resolved = resolve(&request.to_domain()?)?;
            assert!(super::check_dispatch_candidate(&roots, &resolved).is_err());
            assert_eq!(
                resolved.alternatives.as_slice()[0].model,
                "opencode-go/glm-5.3-flash"
            );
            Ok(())
        };
        fallback(Some("opencode/opencode-go/claude-haiku-5-5"), None)?;
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/claude-haiku-5-5',weight=1}]\n",
        )?;
        fallback(None, Some("test"))?;
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/claude-haiku-5-5',weight=1},{model='opencode/opencode-go/glm-5.3-flash',weight=1}]\n[domains.routing.opencode_go]\nmin_monthly_allowance_usd=100\n",
        )?;
        let error = resolve(&request(None, Some("test"))?).unwrap_err();
        assert!(
            error.contains("claude-haiku-5-5") && error.contains("glm-5.3-flash"),
            "{error}"
        );
        std::fs::write(
            &path,
            "schema_version=1\n[domains.routing.pools]\ntest=[{model='opencode/opencode-go/claude-haiku-5-5',weight=1},{model='opencode/opencode-go/glm-5.3-flash',weight=0}]\n",
        )?;
        let selected = resolve(&request(None, Some("test"))?)?;
        assert_eq!(selected.model, "opencode-go/glm-5.3-flash");
        assert!(
            selected.pool_selection.as_ref().unwrap().redraws[0]
                .reason
                .contains("$15")
        );
        let mut candidate = selected.clone();
        candidate.model = "opencode-go/unlisted".into();
        assert!(
            super::check_dispatch_candidate(&roots, &candidate)
                .unwrap_err()
                .contains("unknown")
        );
        candidate.model = "opencode-go/claude-haiku-5-5".into();
        assert!(
            super::check_dispatch_candidate(&roots, &candidate)
                .unwrap_err()
                .contains("$15")
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
        let now = jiff::Timestamp::now().as_second();
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
            model: Some(super::Model("codex/unlisted-exact".into())),
            route: None,
            effort: None,
            confidential: false,
            allow_free: false,
            fallback: vec![super::Model("codex/unlisted-fallback".into())],
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
        )?;
        assert_eq!(
            exhausted.readiness,
            RouteReadiness::Unavailable {
                reason: RouteExclusionReason::Exhausted,
            }
        );
        assert_eq!(
            exhausted.alternatives.as_slice()[0].readiness,
            RouteReadiness::Unavailable {
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
        let observed = super::quota_observations(&catalogue, &availability, &[record], &requested)?;
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
            availability_ref: "fixture".into(),
            expires_unix_seconds: 200,
        };
        ResolvedRoute {
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
                evidence_ref: "fixture".into(),
            },
            pool_selection: Some(RouteSelectionReceipt {
                pool: "junior".into(),
                candidates: vec![RouteSelectionCandidate {
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
        let now = jiff::Timestamp::now().as_second();
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
            model: None,
            route: Some("test".into()),
            effort: None,
            confidential: false,
            allow_free: false,
            fallback: vec![],
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
                Some(excluded),
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
            Some(&["openai".into()]),
        )?;
        assert_eq!(deferred.adapter, "claude");
        assert!(matches!(
            deferred.readiness,
            provenant_records::RouteReadiness::Unavailable { .. }
        ));
        Ok(())
    }
}
