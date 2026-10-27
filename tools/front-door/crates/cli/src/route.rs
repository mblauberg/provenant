//! Routing's frozen clap leaves and read-only composition/rendering edge.
use crate::{Globals, RenderMode};
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
    /// Set one provider's routing pacing policy.
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
}
impl From<PacingChoice> for PacingPolicy {
    fn from(value: PacingChoice) -> Self {
        match value {
            PacingChoice::Pace => Self::Pace,
            PacingChoice::Spend => Self::Spend,
            PacingChoice::Max => Self::Max,
        }
    }
}
#[derive(Debug, Args)]
pub struct RoutePacing {
    /// Registered provider ID.
    pub provider: String,
    /// Provider pacing policy.
    pub policy: PacingChoice,
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
    Config(#[from] provenant_config::ConfigError),
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
}
#[derive(Serialize)]
#[serde(untagged)]
pub enum Data {
    List(provenant_routing::RouteList),
    Resolved(Box<provenant_records::ResolvedRoute>),
}
/// The OpenCode snapshot reports the Go plan's pools, which only govern models
/// served by the `opencode-go` provider. Free (Zen) and other configured
/// providers must not inherit its account-level exhaustion.
fn snapshot_governs(adapter: &str, model: &ModelId) -> bool {
    provenant_records::AdapterSpec::get(adapter)
        .and_then(|spec| spec.quota_model_prefix)
        .is_none_or(|prefix| model.model().starts_with(prefix))
}
fn quota_observations(
    catalogue: &Catalogue,
    availability: &[AvailabilityRecord],
    records: &[provenant_quota::AccountRecord],
) -> Result<Vec<QuotaObservation>, EdgeError> {
    let policies = provenant_providers::quota_policies(REGISTRY);
    let mut observations = Vec::new();
    for available in availability {
        let Some(account_key) = available.account_key.as_deref() else {
            continue;
        };
        let Some(record) = records.iter().find(|record| {
            record.account.adapter == available.adapter && record.account.account_key == account_key
        }) else {
            continue;
        };
        let Some(snapshot) = record.snapshot.as_ref() else {
            continue;
        };
        let Some(ttl) = provenant_quota::schedule::interval(
            &policies,
            &available.adapter,
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
            .filter(|entry| entry.model.adapter() == available.adapter)
            .map(|entry| entry.model.clone())
            .collect();
        for observed_model in &available.observed_models {
            models.insert(ModelId::parse(&format!(
                "{}/{}",
                available.adapter, observed_model
            ))?);
        }
        models.retain(|model| snapshot_governs(&available.adapter, model));
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
fn with_context<T>(
    roots: &Roots,
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
    action: impl FnOnce(&RoutingContext<'_>, &[provenant_quota::AccountRecord]) -> Result<T, EdgeError>,
) -> Result<T, EdgeError> {
    let config = provenant_config::read_with_domains(roots, &crate::domains::SECTIONS)?
        .domains
        .routing;
    let catalogue = Catalogue::bundled()?;
    // A lane holds no grant on the shared quota store: route without a
    // snapshot there rather than refusing the nested dispatch.
    let quota_records = match crate::quota::read_cache(roots) {
        Ok(records) => records,
        Err(crate::quota::EdgeError::Fs(error))
            if error.is_permission_denied()
                && std::env::var_os(provenant_core::RUN_STATE_DIRECTORY_ENV).is_some() =>
        {
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    let quota = quota_observations(&catalogue, availability, &quota_records)?;
    let timestamp = jiff::Timestamp::now();
    let now = WallTime::from_micros(timestamp.as_microsecond())?;
    action(
        &RoutingContext {
            catalogue: &catalogue,
            config: &config,
            registry: REGISTRY,
            availability,
            quota: &quota,
            access,
            now,
            host,
        },
        &quota_records,
    )
}
/// Resolve one production dispatch against fresh provider facts and validated
/// task access. Account aliases must already have been mapped to registered keys.
pub(crate) fn resolve_dispatch(
    request: &provenant_routing::RouteRequest,
    roots: &Roots,
    account: Option<&str>,
    seed: (&str, u64),
    availability: &[AvailabilityRecord],
    access: &[AccessContext<'_>],
    host: &str,
) -> Result<provenant_records::ResolvedRoute, String> {
    with_context(roots, availability, access, host, |context, records| {
        let mut excluded_members = Vec::new();
        let mut resolved = provenant_routing::resolve_for_account_with_seed_excluding(
            request,
            context,
            account,
            seed.0,
            seed.1,
            &excluded_members,
        )?;
        loop {
            if !matches!(&resolved.readiness, RouteReadiness::Ready { .. }) {
                break;
            }
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
            if let Err(error) = admission.check(&resolved) {
                if !matches!(&request.choice, RouteChoice::NamedRoute(_))
                    || resolved.pool_selection.is_none()
                    || excluded_members.contains(&model)
                {
                    return Err(error.into());
                }
                let reason = error.to_string();
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
                        Ok(route) if matches!(&route.readiness, RouteReadiness::Ready { .. }) => {
                            route
                        }
                        Ok(_) | Err(_) => return Err(error.into()),
                    };
                if replacement.adapter == resolved.adapter && replacement.model == resolved.model {
                    return Err(error.into());
                }
                record_pool_redraw(&resolved, &mut replacement, &reason)?;
                resolved = replacement;
                continue;
            }
            break;
        }
        Ok(resolved)
    })
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
    let pools = free_pools(&route.adapter, || catalogue.is_free_model(&model));
    provenant_quota::Admission {
        policies: &provenant_providers::quota_policies(REGISTRY),
        now_unix_seconds: jiff::Timestamp::now().as_second(),
        records: &crate::quota::read_cache(roots).map_err(|e| e.to_string())?,
        pools: &pools,
    }
    .check(route)
    .map_err(|e| e.to_string())
}

fn list_data(
    roots: &Roots,
    query: &RouteListRequest,
) -> Result<provenant_routing::RouteList, EdgeError> {
    with_context(roots, &[], &[], "local-preview", |context, _| {
        Ok(provenant_routing::list(
            &provenant_routing::RouteListQuery {
                adapter: query.adapter.clone(),
                pool: query.pool.clone(),
            },
            context,
        )?)
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
    if !REGISTRY
        .iter()
        .any(|provider| provider.adapter.id() == request.provider)
    {
        return Err(RoutingError::UnknownAdapter.into());
    }
    edit_config(roots, |document| {
        let routing = routing_table(document)?;
        let pacing = routing
            .entry("pacing")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
        let pacing = config_table(pacing).ok_or("domains.routing.pacing must be a table")?;
        pacing.insert(
            &request.provider,
            toml_edit::value(match request.policy {
                PacingChoice::Pace => "pace",
                PacingChoice::Spend => "spend",
                PacingChoice::Max => "max",
            }),
        );
        Ok(())
    })
}

fn execute(command: &Route, roots: &Roots) -> Result<Data, EdgeError> {
    match command {
        Route::List(query) => Ok(Data::List(list_data(roots, query)?)),
        Route::Resolve(query) => with_context(roots, &[], &[], "local-preview", |context, _| {
            Ok(Data::Resolved(Box::new(provenant_routing::resolve(
                &resolve_request(query, context)?,
                context,
            )?)))
        }),
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
                EdgeError::Routing(_) | EdgeError::Unresolved { .. } => {
                    (Outcome::Invalid, "INVALID")
                }
                EdgeError::Config(_) => (Outcome::Invalid, "CONFIG"),
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
            Some(Data::Resolved(route)) => format!(
                "{}/{} effort={} {:?}{}\n",
                route.adapter,
                route.model,
                route.effort.as_deref().unwrap_or("unspecified"),
                route.readiness,
                route
                    .pool_selection
                    .as_ref()
                    .map_or_else(String::new, |receipt| format!(
                        " pool={} roll={} pick={}",
                        receipt.pool, receipt.roll, receipt.pick
                    ))
            ),
            None => String::new(),
        }
    };
    let mode = if globals.json {
        RenderMode::Json
    } else {
        RenderMode::Human {
            text: &human,
            read_only: matches!(command, Route::List(_) | Route::Resolve(_)),
        }
    };
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
        let observed = super::quota_observations(&catalogue, &availability, &[record])?;
        let bound: Vec<_> = observed.iter().map(|o| o.model.as_str()).collect();
        assert!(bound.contains(&"opencode/opencode-go/glm-5.1"));
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
}
