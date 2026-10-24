use crate::*;
use provenant_providers::{AvailabilityRecord, Registration};
use provenant_quota::QuotaStatus;
use provenant_records::{
    RouteAlternative, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteExclusion,
    RouteExclusionReason, RouteReadiness,
};
use serde::Serialize;

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
        if self
            .catalogue
            .pools
            .keys()
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
    pub pools: std::collections::BTreeMap<String, Vec<RoutePoolMember>>,
    pub availability: Vec<AvailabilityView>,
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
    let matches = |adapter: &str| query.adapter.as_deref().is_none_or(|id| id == adapter);
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
        pools: context
            .catalogue
            .pools
            .iter()
            .filter_map(|(name, members)| {
                let members: Vec<_> = members
                    .iter()
                    .filter(|member| matches(member.model.adapter()))
                    .cloned()
                    .collect();
                (!members.is_empty()).then(|| (name.clone(), members))
            })
            .collect(),
        availability,
    })
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
    let availability = context
        .availability
        .iter()
        .find(|a| a.adapter == model.adapter());
    let access = context.access.iter().find(|a| a.adapter == model.adapter());
    let mut reason = availability_reason(reg, availability, context);
    if reason.is_none() && access.is_none_or(|a| a.disclosure_ref.is_empty()) {
        reason = Some(RouteExclusionReason::DisclosureDenied);
    }
    if reason.is_none() && access.is_none_or(|a| a.confinement_ref.is_empty()) {
        reason = Some(RouteExclusionReason::ConfinementUnavailable);
    }
    // An unknown exact ID is retained as unverified. Even a catalogue row cannot
    // replace an exact observation in current preflight evidence.
    if reason.is_none()
        && availability.is_none_or(|a| !a.observed_models.iter().any(|m| m == model.model()))
    {
        reason = Some(RouteExclusionReason::UnverifiedModel);
    }
    let account = availability.and_then(|a| a.account_key.clone());
    let quota = context
        .quota
        .iter()
        .filter(|q| {
            q.model == *model
                && q.snapshot.schema_version == 1
                && q.snapshot.account.adapter == model.adapter()
                && Some(&q.snapshot.account.account_key) == account.as_ref()
                && q.snapshot.observed_unix_seconds <= context.now_seconds()
                && q.snapshot.observed_unix_seconds < q.expires_unix_seconds
                && context.now_seconds() < q.expires_unix_seconds
        })
        .max_by_key(|q| (q.snapshot.observed_unix_seconds, q.snapshot.revision));
    if reason.is_none()
        && let Some(q) = quota
    {
        reason = match q.snapshot.status {
            QuotaStatus::Exhausted => Some(RouteExclusionReason::Exhausted),
            QuotaStatus::AuthRequired => Some(RouteExclusionReason::AuthRequired),
            _ => None,
        };
    }
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
/// Dispatch-only override. Filter the immutable provider observations before
/// resolving, so quota and readiness are checked for the selected account.
/// Without an override the first registered observation remains the selection;
/// this does not implement quota-driven account switching (#1115).
pub fn resolve_for_account(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
) -> Result<ResolvedRoute, RoutingError> {
    let Some(account) = account else {
        return resolve(request, context);
    };
    if !crate::request::identifier(account) || account == "unknown" {
        return Err(RoutingError::InvalidValue);
    }
    context.validate()?;
    let adapters: std::collections::BTreeSet<&str> = match &request.choice {
        RouteChoice::Model(model) => [model.adapter()].into_iter().collect(),
        RouteChoice::NamedRoute(name) => {
            if let Some(pool) = context.catalogue.pools.get(name) {
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
        .filter(|a| {
            !adapters.contains(a.adapter.as_str()) || a.account_key.as_deref() == Some(account)
        })
        .cloned()
        .collect();
    resolve(
        request,
        &RoutingContext {
            availability: &availability,
            ..*context
        },
    )
}

pub fn resolve(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
) -> Result<ResolvedRoute, RoutingError> {
    context.validate()?;
    let (model, effort, pool_primary) = match &request.choice {
        RouteChoice::Model(model) => {
            if !privacy_allowed(model, request.confidential, context.catalogue) {
                return Err(RoutingError::ConfidentialModel);
            }
            (model, request.effort.as_deref(), None)
        }
        RouteChoice::NamedRoute(name) => {
            if !crate::request::identifier(name) {
                return Err(RoutingError::InvalidValue);
            }
            if let Some(pool) = context.catalogue.pools.get(name) {
                let mut first_unavailable = None;
                let mut selected = None;
                let mut exclusions = Vec::new();
                for member in pool {
                    if !privacy_allowed(&member.model, request.confidential, context.catalogue) {
                        exclusions.push("confidential model".into());
                        continue;
                    }
                    if member.free && !request.allow_free {
                        exclusions.push("free model disabled".into());
                        continue;
                    }
                    let effort = request.effort.as_deref().or(member.effort.as_deref());
                    let option = candidate(&member.model, effort, context)?;
                    if matches!(&option.readiness, RouteReadiness::Ready { .. }) {
                        selected = Some((&member.model, effort, option));
                        break;
                    }
                    if first_unavailable.is_none() {
                        first_unavailable = Some((&member.model, effort, option));
                    }
                }
                let (model, effort, primary) = selected
                    .or(first_unavailable)
                    .ok_or(RoutingError::NoEligiblePoolMember { exclusions })?;
                (model, effort, Some(primary))
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
                    &named.model,
                    request.effort.as_deref().or(named.effort.as_deref()),
                    None,
                )
            }
        }
    };
    let primary = match pool_primary {
        Some(primary) => primary,
        None => candidate(model, effort, context)?,
    };
    let models = match &request.fallback {
        Fallback::None => &[][..],
        Fallback::Models(models) => models.as_slice(),
    };
    if models.contains(model) {
        return Err(RoutingError::Alternatives);
    }
    let alternatives = models
        .iter()
        .filter(|model| privacy_allowed(model, request.confidential, context.catalogue))
        .map(|model| candidate(model, effort, context))
        .collect::<Result<Vec<_>, _>>()?;
    let exclusions = std::iter::once(&primary)
        .chain(alternatives.iter())
        .filter_map(|c| {
            if let RouteReadiness::Unavailable { reason } = &c.readiness {
                Some(RouteExclusion {
                    adapter: c.adapter.clone(),
                    model: c.model.clone(),
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
    })
}
