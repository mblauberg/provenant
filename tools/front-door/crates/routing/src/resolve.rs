use crate::*;
use provenant_providers::{AvailabilityRecord, Registration};
use provenant_quota::QuotaStatus;
use provenant_records::{
    RouteAlternative, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteExclusion,
    RouteExclusionReason, RouteReadiness, RouteSelectionCandidate, RouteSelectionReceipt,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Inputs are validated by their owning composition edges. Preview has no
/// disclosure/confinement grant; dispatch supplies the separately validated refs.
pub struct AccessContext<'a> {
    pub adapter: &'a str,
    pub disclosure_ref: &'a str,
    pub confinement_ref: &'a str,
}
pub struct RoutingContext<'a> {
    pub catalogue: &'a Catalogue,
    pub config: &'a RoutingConfig,
    pub registry: &'a [Registration],
    pub availability: &'a [AvailabilityRecord],
    pub quota: &'a [QuotaObservation],
    pub access: &'a [AccessContext<'a>],
    pub now: provenant_core::WallTime,
    pub host: &'a str,
}
impl RoutingContext<'_> {
    fn now_seconds(&self) -> i64 {
        self.now.as_timestamp().as_second()
    }
    fn validate(&self) -> Result<(), RoutingError> {
        self.catalogue.validate()?;
        if effective_pool_names(self)
            .iter()
            .any(|name| self.config.routes.contains_key(name))
        {
            return Err(RoutingError::InvalidValue);
        }
        if !provenant_config::DomainConfig::validate(self.config).is_empty() {
            return Err(RoutingError::InvalidValue);
        }
        if self.availability.len() > 128
            || self.quota.len() > 256
            || self.registry.len() > 32
            || self.host.len() > 128
        {
            return Err(RoutingError::Availability);
        }
        let mut seen = std::collections::BTreeSet::new();
        for record in self.availability {
            validate_availability(record, &record.adapter)?;
            if !seen.insert((&record.adapter, &record.account_key)) {
                return Err(RoutingError::Availability);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
pub struct RouteList {
    pub catalogue_revision: String,
    pub models: Vec<ModelEntry>,
    pub named_routes: std::collections::BTreeMap<String, NamedRoute>,
    pub pools: std::collections::BTreeMap<String, Vec<RoutePoolMemberView>>,
    pub pacing: std::collections::BTreeMap<String, PacingPolicy>,
    pub availability: Vec<AvailabilityView>,
}
#[derive(Debug, Serialize)]
pub struct RoutePoolMemberView {
    pub model: ModelId,
    pub weight: u8,
    pub effort: Option<String>,
    pub efforts: Vec<String>,
    pub free: bool,
    pub factor_basis_points: u32,
    pub pacing: PacingPolicy,
    pub readiness: RouteReadiness,
}
pub fn list(
    query: &RouteListQuery,
    context: &RoutingContext<'_>,
) -> Result<RouteList, RoutingError> {
    context.validate()?;
    if query
        .adapter
        .as_ref()
        .is_some_and(|id| !context.registry.iter().any(|r| r.adapter.id() == id))
    {
        return Err(RoutingError::UnknownAdapter);
    }
    if query
        .pool
        .as_ref()
        .is_some_and(|name| effective_pool(context, name).is_none())
    {
        return Err(RoutingError::UnknownRoute);
    }
    let matches = |adapter: &str| query.adapter.as_deref().is_none_or(|id| id == adapter);
    let mut pools = BTreeMap::new();
    for name in effective_pool_names(context) {
        if query
            .pool
            .as_deref()
            .is_some_and(|pool| pool != name.as_str())
        {
            continue;
        }
        let Some(members) = effective_pool(context, &name) else {
            continue;
        };
        let mut views = Vec::new();
        for member in members
            .iter()
            .filter(|member| matches(member.model.adapter()))
        {
            if context.catalogue.model(&member.model).is_none() {
                return Err(RoutingError::Catalogue);
            }
            let pacing = pacing_policy(context.catalogue, context.config, &member.model);
            let factor_basis_points = pacing_factor_basis_points(
                pacing,
                &member.model,
                context.quota,
                context.now_seconds(),
            );
            let readiness = candidate(&member.model, member.effort.as_deref(), context)?.readiness;
            views.push(RoutePoolMemberView {
                model: member.model.clone(),
                weight: member.weight,
                effort: member.effort.clone(),
                efforts: member.efforts.clone(),
                free: context.catalogue.is_free_model(&member.model),
                factor_basis_points,
                pacing,
                readiness,
            });
        }
        if !views.is_empty() || query.pool.as_deref() == Some(name.as_str()) {
            pools.insert(name, views);
        }
    }
    let pacing = effective_pacing(context.catalogue, context.config);
    let availability = context
        .registry
        .iter()
        .filter(|r| matches(r.adapter.id()))
        .map(|r| {
            let record = context
                .availability
                .iter()
                .find(|a| a.adapter == r.adapter.id());
            let reason = availability_reason(r, record, context);
            AvailabilityView {
                adapter: r.adapter.id().into(),
                record: record.map(AvailabilitySummary::from),
                reason,
                age_seconds: record
                    .and_then(|r| context.now_seconds().checked_sub(r.observed_unix_seconds))
                    .and_then(|age| u64::try_from(age).ok()),
            }
        })
        .collect();
    Ok(RouteList {
        catalogue_revision: context.catalogue.revision.clone(),
        models: context
            .catalogue
            .models
            .iter()
            .filter(|m| matches(m.model.adapter()))
            .cloned()
            .collect(),
        named_routes: context
            .config
            .routes
            .iter()
            .filter(|(_, r)| matches(r.model.adapter()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pools,
        pacing,
        availability,
    })
}

fn effective_pool_names(context: &RoutingContext<'_>) -> BTreeSet<String> {
    context
        .catalogue
        .pools
        .keys()
        .chain(context.config.pools.keys())
        .cloned()
        .collect()
}
fn effective_pool<'a>(
    context: &'a RoutingContext<'_>,
    name: &str,
) -> Option<&'a Vec<RoutePoolMember>> {
    context
        .config
        .pools
        .get(name)
        .or_else(|| context.catalogue.pools.get(name))
}
fn effective_pacing(
    catalogue: &Catalogue,
    config: &RoutingConfig,
) -> BTreeMap<String, PacingPolicy> {
    let mut policies = catalogue.pacing.clone();
    policies.extend(
        config
            .pacing
            .iter()
            .map(|(provider, policy)| (provider.clone(), *policy)),
    );
    policies
}
fn pacing_policy(catalogue: &Catalogue, config: &RoutingConfig, model: &ModelId) -> PacingPolicy {
    if let Some(policy) = config.pacing.get(model.adapter()) {
        return *policy;
    }
    let key = if model.adapter() == "opencode" {
        if catalogue.is_free_model(model) {
            "opencode_free"
        } else {
            "opencode_paid"
        }
    } else {
        model.adapter()
    };
    catalogue
        .pacing
        .get(key)
        .copied()
        .unwrap_or(PacingPolicy::Pace)
}
const PACE_MIN_BASIS_POINTS: u32 = 2_500;
const PACE_MAX_BASIS_POINTS: u32 = 20_000;
const NEUTRAL_FACTOR_BASIS_POINTS: u32 = 10_000;

/// Pace against the healthiest fresh account snapshot for this provider.
fn pacing_factor_basis_points(
    policy: PacingPolicy,
    model: &ModelId,
    quota: &[QuotaObservation],
    now_seconds: i64,
) -> u32 {
    if policy != PacingPolicy::Pace {
        return NEUTRAL_FACTOR_BASIS_POINTS;
    }
    quota
        .iter()
        .filter(|observation| {
            observation.model.adapter() == model.adapter()
                && observation.snapshot.schema_version == 1
                && observation.snapshot.account.adapter == model.adapter()
                && observation.snapshot.observed_unix_seconds <= now_seconds
                && observation.expires_unix_seconds > now_seconds
                && matches!(
                    observation.snapshot.status,
                    QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
                )
        })
        .filter_map(|observation| pace_snapshot_factor(&observation.snapshot, now_seconds))
        .max()
        .unwrap_or(NEUTRAL_FACTOR_BASIS_POINTS)
}

fn pace_snapshot_factor(
    snapshot: &provenant_quota::QuotaSnapshot,
    now_seconds: i64,
) -> Option<u32> {
    let window = snapshot
        .windows
        .iter()
        .filter_map(|window| {
            let window_seconds = window.window_seconds?;
            let reset_seconds = window.resets_unix_seconds?;
            if window_seconds == 0 || reset_seconds <= now_seconds {
                return None;
            }
            Some((window_seconds, window.used_basis_points, reset_seconds))
        })
        .min_by_key(|(window_seconds, _, _)| *window_seconds)?;
    let (window_seconds, used_basis_points, reset_seconds) = window;
    let remaining_seconds = u64::try_from(reset_seconds.checked_sub(now_seconds)?).ok()?;
    let remaining_seconds = remaining_seconds.min(window_seconds);
    if remaining_seconds == 0 {
        return None;
    }
    let headroom_basis_points = 10_000_u64.saturating_sub(used_basis_points.min(10_000));
    let factor = (u128::from(headroom_basis_points) * u128::from(window_seconds))
        / u128::from(remaining_seconds);
    let factor = factor.clamp(
        u128::from(PACE_MIN_BASIS_POINTS),
        u128::from(PACE_MAX_BASIS_POINTS),
    );
    u32::try_from(factor).ok()
}

fn availability_reason(
    reg: &Registration,
    record: Option<&AvailabilityRecord>,
    ctx: &RoutingContext<'_>,
) -> Option<RouteExclusionReason> {
    if !reg.adapter.execution_enabled() {
        return Some(RouteExclusionReason::Disabled);
    }
    let Some(record) = record else {
        return Some(RouteExclusionReason::UnknownAvailability);
    };
    if record.catalogue_revision != ctx.catalogue.revision
        || record.observed_unix_seconds > ctx.now_seconds()
        || record.expires_unix_seconds <= ctx.now_seconds()
    {
        return Some(RouteExclusionReason::StaleAvailability);
    }
    if let Some(error) = record.last_preflight_error {
        use provenant_providers::PreflightFailure;
        return Some(match error {
            PreflightFailure::MissingExecutable => RouteExclusionReason::MissingExecutable,
            PreflightFailure::AuthRequired => RouteExclusionReason::AuthRequired,
            PreflightFailure::Disabled
            | PreflightFailure::UnsupportedVersion
            | PreflightFailure::MalformedProtocol => RouteExclusionReason::Disabled,
        });
    }
    if record.account_key.is_none() {
        return Some(RouteExclusionReason::UnknownAccount);
    }
    None
}
fn account_reason<'q>(
    reg: &Registration,
    model: &ModelId,
    listed: bool,
    availability: &AvailabilityRecord,
    access: Option<&AccessContext<'_>>,
    context: &'q RoutingContext<'_>,
) -> (Option<RouteExclusionReason>, Option<&'q QuotaObservation>) {
    let mut reason = availability_reason(reg, Some(availability), context);
    if reason.is_none() && access.is_none_or(|a| a.disclosure_ref.is_empty()) {
        reason = Some(RouteExclusionReason::DisclosureDenied);
    }
    if reason.is_none() && access.is_none_or(|a| a.confinement_ref.is_empty()) {
        reason = Some(RouteExclusionReason::ConfinementUnavailable);
    }
    // Known catalogue controls can use the fresh executable/account preflight.
    // An unlisted exact ID additionally needs a current provider observation.
    if reason.is_none()
        && !listed
        && !availability
            .observed_models
            .iter()
            .any(|m| m == model.model())
    {
        reason = Some(RouteExclusionReason::UnverifiedModel);
    }
    let quota = context
        .quota
        .iter()
        .filter(|q| {
            q.model == *model
                && q.snapshot.schema_version == 1
                && q.snapshot.account.adapter == model.adapter()
                && Some(&q.snapshot.account.account_key) == availability.account_key.as_ref()
                && q.snapshot.observed_unix_seconds <= context.now_seconds()
                && q.snapshot.observed_unix_seconds < q.expires_unix_seconds
                && context.now_seconds() < q.expires_unix_seconds
        })
        .max_by_key(|q| (q.snapshot.observed_unix_seconds, q.snapshot.revision));
    if reason.is_none()
        && let Some(q) = quota
    {
        reason = match q.snapshot.status {
            QuotaStatus::Exhausted if !exhaustion_lapsed(&q.snapshot, context.now_seconds()) => {
                Some(RouteExclusionReason::Exhausted)
            }
            QuotaStatus::AuthRequired => Some(RouteExclusionReason::AuthRequired),
            _ => None,
        };
    }
    (reason, quota)
}
/// An exhausted snapshot stops binding once every full window has reset.
fn exhaustion_lapsed(snapshot: &provenant_quota::QuotaSnapshot, now_seconds: i64) -> bool {
    let mut full = snapshot
        .windows
        .iter()
        .filter(|w| w.used_basis_points >= 10_000)
        .peekable();
    full.peek().is_some()
        && full.all(|w| {
            w.resets_unix_seconds
                .is_some_and(|reset| reset <= now_seconds)
        })
}
fn candidate(
    model: &ModelId,
    effort: Option<&str>,
    context: &RoutingContext<'_>,
) -> Result<RouteAlternative, RoutingError> {
    let reg = context
        .registry
        .iter()
        .find(|r| r.adapter.id() == model.adapter())
        .ok_or(RoutingError::UnknownAdapter)?;
    let entry = context.catalogue.model(model);
    if effort.is_some_and(|e| {
        !crate::request::effort_valid(e)
            || entry.is_none_or(|row| !row.efforts.iter().any(|v| v == e))
    }) {
        return Err(RoutingError::UnsupportedEffort);
    }
    let access = context.access.iter().find(|a| a.adapter == model.adapter());
    // Several registered accounts may serve one adapter: take a ready one with
    // the most pacing headroom, so exhausted accounts are skipped rather than
    // chosen by registration order.
    let evaluated: Vec<_> = context
        .availability
        .iter()
        .filter(|a| a.adapter == model.adapter())
        .map(|a| {
            let (reason, quota) = account_reason(reg, model, entry.is_some(), a, access, context);
            let headroom = quota
                .and_then(|q| pace_snapshot_factor(&q.snapshot, context.now_seconds()))
                .unwrap_or(NEUTRAL_FACTOR_BASIS_POINTS);
            (a, reason, headroom)
        })
        .collect();
    let best = evaluated
        .iter()
        .filter(|(_, reason, _)| reason.is_none())
        .max_by_key(|(_, _, headroom)| *headroom)
        .or_else(|| evaluated.first());
    let availability = best.map(|(a, _, _)| *a);
    let reason = match best {
        Some((_, reason, _)) => reason.clone(),
        None => availability_reason(reg, None, context),
    };
    let account = availability.and_then(|a| a.account_key.clone());
    let readiness = match (reason, availability) {
        (Some(reason), _) => RouteReadiness::Unavailable { reason },
        (None, Some(a)) => RouteReadiness::Ready {
            availability_ref: a.fixture_ref.clone(),
            expires_unix_seconds: a.expires_unix_seconds,
        },
        _ => RouteReadiness::Unavailable {
            reason: RouteExclusionReason::UnknownAvailability,
        },
    };
    Ok(RouteAlternative {
        adapter: model.adapter().into(),
        model: model.model().into(),
        effort: effort.map(str::to_owned),
        account_key: account,
        host: context.host.into(),
        eligibility: if entry.is_some() {
            RouteEligibility::Eligible
        } else {
            RouteEligibility::UnverifiedModel
        },
        readiness,
        confidentiality: RouteConfidentiality {
            evidence_ref: access.map_or("", |a| a.disclosure_ref).into(),
        },
    })
}
fn privacy_allowed(model: &ModelId, confidential: bool, catalogue: &Catalogue) -> bool {
    !confidential
        || catalogue
            .model(model)
            .is_some_and(|row| row.trains_on_prompts == Some(false))
}
/// Resolve a route with a dispatch seed. The operation ID and task index make
/// retries repeat the same weighted choice without storing rotation state.
pub fn resolve_with_seed(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    operation_id: &str,
    task_index: u64,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_inner(request, context, Some((operation_id, task_index)), &[])
}

/// Dispatch-only account filter; its routing decision also uses the stable
/// operation/task seed carried by the attempt.
pub fn resolve_for_account(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_for_account_with_seed(request, context, account, "route-preview", 0)
}

pub fn resolve_for_account_with_seed(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
    operation_id: &str,
    task_index: u64,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_for_account_with_seed_excluding(
        request,
        context,
        account,
        operation_id,
        task_index,
        &[],
    )
}

/// Resolve a seeded pool while excluding members rejected by a later dispatch
/// gate. The operation and task seed stay fixed across each redraw.
pub fn resolve_for_account_with_seed_excluding(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
    operation_id: &str,
    task_index: u64,
    excluded_members: &[ModelId],
) -> Result<ResolvedRoute, RoutingError> {
    let mut filtered = None;
    if let Some(account) = account {
        if !crate::request::identifier(account) || account == "unknown" {
            return Err(RoutingError::InvalidValue);
        }
        context.validate()?;
        let adapters: BTreeSet<&str> = match &request.choice {
            RouteChoice::Model(model) => [model.adapter()].into_iter().collect(),
            RouteChoice::NamedRoute(name) => {
                if let Some(pool) = effective_pool(context, name) {
                    pool.iter().map(|member| member.model.adapter()).collect()
                } else {
                    [context
                        .config
                        .routes
                        .get(name)
                        .ok_or(RoutingError::UnknownRoute)?
                        .model
                        .adapter()]
                    .into_iter()
                    .collect()
                }
            }
        };
        let availability: Vec<_> = context
            .availability
            .iter()
            .filter(|record| {
                !adapters.contains(record.adapter.as_str())
                    || record.account_key.as_deref() == Some(account)
            })
            .cloned()
            .collect();
        filtered = Some(availability);
    }
    match filtered.as_ref() {
        Some(availability) => resolve_inner(
            request,
            &RoutingContext {
                availability,
                ..*context
            },
            Some((operation_id, task_index)),
            excluded_members,
        ),
        None => resolve_inner(
            request,
            context,
            Some((operation_id, task_index)),
            excluded_members,
        ),
    }
}

pub fn resolve(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_inner(request, context, None, &[])
}

struct PoolPick {
    route: RouteAlternative,
    receipt: RouteSelectionReceipt,
}

fn resolve_inner(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: &[ModelId],
) -> Result<ResolvedRoute, RoutingError> {
    context.validate()?;
    let (primary, pool_selection) = match &request.choice {
        RouteChoice::Model(model) => {
            if !privacy_allowed(model, request.confidential, context.catalogue) {
                return Err(RoutingError::ConfidentialModel);
            }
            (candidate(model, request.effort.as_deref(), context)?, None)
        }
        RouteChoice::NamedRoute(name) => {
            if !crate::request::identifier(name) {
                return Err(RoutingError::InvalidValue);
            }
            if let Some(pool) = effective_pool(context, name) {
                let selected = select_pool(name, pool, request, context, seed, excluded_members)?;
                (selected.route, Some(selected.receipt))
            } else {
                let named = context
                    .config
                    .routes
                    .get(name)
                    .ok_or(RoutingError::UnknownRoute)?;
                if !privacy_allowed(&named.model, request.confidential, context.catalogue) {
                    return Err(RoutingError::ConfidentialModel);
                }
                (
                    candidate(
                        &named.model,
                        request.effort.as_deref().or(named.effort.as_deref()),
                        context,
                    )?,
                    None,
                )
            }
        }
    };
    let models = match &request.fallback {
        Fallback::None => &[][..],
        Fallback::Models(models) => models.as_slice(),
    };
    let primary_model = ModelId::parse(&format!("{}/{}", primary.adapter, primary.model))?;
    if models.contains(&primary_model) {
        return Err(RoutingError::Alternatives);
    }
    let alternatives = models
        .iter()
        .filter(|model| privacy_allowed(model, request.confidential, context.catalogue))
        .map(|model| candidate(model, primary.effort.as_deref(), context))
        .collect::<Result<Vec<_>, _>>()?;
    let exclusions = std::iter::once(&primary)
        .chain(alternatives.iter())
        .filter_map(|candidate| {
            if let RouteReadiness::Unavailable { reason } = &candidate.readiness {
                Some(RouteExclusion {
                    adapter: candidate.adapter.clone(),
                    model: candidate.model.clone(),
                    reason: reason.clone(),
                })
            } else {
                None
            }
        })
        .collect();
    Ok(ResolvedRoute {
        adapter: primary.adapter,
        model: primary.model,
        effort: primary.effort,
        account_key: primary.account_key,
        host: primary.host,
        eligibility: primary.eligibility,
        readiness: primary.readiness,
        confidentiality: primary.confidentiality,
        catalogue_revision: context.catalogue.revision.clone(),
        exclusions,
        alternatives: RouteAlternatives::new(alternatives)
            .map_err(|_| RoutingError::Alternatives)?,
        pool_selection,
    })
}

struct ClassifiedPool {
    candidates: Vec<RouteSelectionCandidate>,
    routes: Vec<Option<RouteAlternative>>,
    weighted: Vec<(usize, u64)>,
    first_zero_ready: Option<usize>,
    first_recoverable_unavailable: Option<usize>,
    first_other_unavailable: Option<usize>,
    exclusions: Vec<String>,
}
struct PoolDraw {
    index: usize,
    roll: u64,
    total_weight: u64,
}

fn classify_pool(
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    excluded_members: &[ModelId],
) -> Result<ClassifiedPool, RoutingError> {
    let mut classified = ClassifiedPool {
        candidates: Vec::with_capacity(members.len()),
        routes: Vec::with_capacity(members.len()),
        weighted: Vec::new(),
        first_zero_ready: None,
        first_recoverable_unavailable: None,
        first_other_unavailable: None,
        exclusions: Vec::new(),
    };
    for member in members {
        let effort = request.effort.clone().or(member.effort.clone());
        let policy = pacing_policy(context.catalogue, context.config, &member.model);
        let factor_basis_points =
            pacing_factor_basis_points(policy, &member.model, context.quota, context.now_seconds());
        if excluded_members.contains(&member.model) {
            classified
                .exclusions
                .push(format!("{}: admission rejected", member.model.as_str()));
            classified.candidates.push(RouteSelectionCandidate {
                model: member.model.as_str().into(),
                weight: member.weight,
                factor_basis_points,
                effort,
                readiness: None,
                exclusion: Some("admission rejected".into()),
            });
            classified.routes.push(None);
            continue;
        }
        let exclusion = if !privacy_allowed(&member.model, request.confidential, context.catalogue)
        {
            Some(("confidential", "confidential model"))
        } else if context.catalogue.is_free_model(&member.model) && !request.allow_free {
            Some(("free", "free model disabled"))
        } else if effort
            .as_deref()
            .is_some_and(|value| !member.efforts.iter().any(|allowed| allowed == value))
        {
            Some(("effort", "unsupported effort"))
        } else {
            None
        };
        let mut readiness = None;
        let mut option = None;
        let exclusion = if let Some((kind, detail)) = exclusion {
            classified.exclusions.push(detail.to_owned());
            Some(kind.to_owned())
        } else {
            match candidate(&member.model, effort.as_deref(), context) {
                Ok(resolved) => {
                    readiness = Some(resolved.readiness.clone());
                    if matches!(resolved.readiness, RouteReadiness::Ready { .. }) {
                        if member.weight == 0 {
                            if classified.first_zero_ready.is_none() {
                                classified.first_zero_ready = Some(classified.candidates.len());
                            }
                        } else {
                            let weight = u64::from(member.weight)
                                .saturating_mul(u64::from(factor_basis_points));
                            classified
                                .weighted
                                .push((classified.candidates.len(), weight));
                        }
                    } else if let RouteReadiness::Unavailable { reason } = &resolved.readiness {
                        match reason {
                            RouteExclusionReason::Disabled => classified
                                .exclusions
                                .push(format!("{}: disabled", member.model.as_str())),
                            RouteExclusionReason::StaleAvailability
                            | RouteExclusionReason::AuthRequired
                            | RouteExclusionReason::Exhausted => {
                                if classified.first_recoverable_unavailable.is_none() {
                                    classified.first_recoverable_unavailable =
                                        Some(classified.candidates.len());
                                }
                            }
                            _ => {
                                if classified.first_other_unavailable.is_none() {
                                    classified.first_other_unavailable =
                                        Some(classified.candidates.len());
                                }
                            }
                        }
                    }
                    option = Some(resolved);
                    None
                }
                Err(RoutingError::UnsupportedEffort) => {
                    classified.exclusions.push("unsupported effort".into());
                    Some("effort".into())
                }
                Err(error) => return Err(error),
            }
        };
        classified.candidates.push(RouteSelectionCandidate {
            model: member.model.as_str().into(),
            weight: member.weight,
            factor_basis_points,
            effort,
            readiness,
            exclusion,
        });
        classified.routes.push(option);
    }
    Ok(classified)
}

fn draw_pool(
    name: &str,
    classified: &ClassifiedPool,
    seed: Option<(&str, u64)>,
) -> Result<PoolDraw, RoutingError> {
    let total_weight = classified
        .weighted
        .iter()
        .fold(0_u64, |total, (_, weight)| total.saturating_add(*weight));
    let roll = if total_weight > 0 {
        let (operation_id, task_index) = seed.unwrap_or(("route-preview", 0));
        stable_roll(operation_id, task_index, name, total_weight)
    } else {
        0
    };
    let selected_index = if total_weight > 0 {
        let mut remainder = roll;
        classified.weighted.iter().find_map(|(index, weight)| {
            if remainder < *weight {
                Some(*index)
            } else {
                remainder = remainder.saturating_sub(*weight);
                None
            }
        })
    } else {
        classified
            .first_zero_ready
            .or(classified.first_recoverable_unavailable)
            .or(classified.first_other_unavailable)
    };
    let index = selected_index.ok_or_else(|| RoutingError::NoEligiblePoolMember {
        exclusions: classified.exclusions.clone(),
    })?;
    Ok(PoolDraw {
        index,
        roll,
        total_weight,
    })
}

fn selection_receipt(
    name: &str,
    classified: &ClassifiedPool,
    draw: &PoolDraw,
    route: &RouteAlternative,
) -> RouteSelectionReceipt {
    RouteSelectionReceipt {
        pool: name.into(),
        candidates: classified.candidates.clone(),
        roll: draw.roll,
        total_weight: draw.total_weight,
        pick: format!("{}/{}", route.adapter, route.model),
        redraws: Vec::new(),
    }
}

fn select_pool(
    name: &str,
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: &[ModelId],
) -> Result<PoolPick, RoutingError> {
    let classified = classify_pool(members, request, context, excluded_members)?;
    let draw = draw_pool(name, &classified, seed)?;
    let route = classified
        .routes
        .get(draw.index)
        .and_then(Option::as_ref)
        .cloned()
        .ok_or(RoutingError::Catalogue)?;
    let receipt = selection_receipt(name, &classified, &draw, &route);
    Ok(PoolPick { route, receipt })
}

fn stable_roll(operation_id: &str, task_index: u64, pool: &str, total: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in operation_id
        .bytes()
        .chain([0])
        .chain(task_index.to_le_bytes())
        .chain([0])
        .chain(pool.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    hash ^= hash >> 33;
    hash % total
}
