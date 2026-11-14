use crate::*;
use provenant_providers::{AvailabilityRecord, Registration};
use provenant_quota::QuotaStatus;
use provenant_records::{
    RouteAlternative, RouteAlternatives, RouteConfidentiality, RouteEligibility, RouteExclusion,
    RouteExclusionReason, RouteReadiness, RouteSelectionCandidate, RouteSelectionReceipt,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// The readiness of one route at a time, in unix seconds.
pub type ReadinessLookup<'a> =
    &'a dyn Fn(&provenant_quota::ReadinessScope, i64) -> provenant_quota::ReadinessRead;

/// Inputs are validated by their owning composition edges. Preview has no
/// disclosure/confinement grant; dispatch supplies the separately validated refs.
pub struct AccessContext<'a> {
    pub adapter: &'a str,
    pub disclosure_ref: &'a str,
    pub confinement_ref: &'a str,
}
#[cfg(test)]
type LaunchSpecLookup<'a> = &'a dyn Fn(&str) -> Option<provenant_records::AdapterSpec>;

pub struct RoutingContext<'a> {
    /// Preview-only account policy selection. It never supplies availability.
    pub policy_account_hint: Option<&'a str>,
    /// Risk and task stratum, when supplied by the dispatch.
    pub selection_evidence: Option<&'a crate::SelectionEvidence>,
    pub choice_context: Option<(&'a str, &'a str)>,
    pub catalogue: &'a Catalogue,
    pub config: &'a RoutingConfig,
    pub registry: &'a [Registration],
    pub availability: &'a [AvailabilityRecord],
    pub quota: &'a [QuotaObservation],
    pub capacity_cooldowns: &'a [provenant_quota::CapacityCooldown],
    /// Calibrated account records and the policies that date their freshness.
    /// They supply persisted mismatch and refusal evidence only.
    pub accounts: &'a [provenant_quota::AccountRecord],
    pub quota_policies: &'a [provenant_quota::QuotaPolicy],
    /// Readiness lookup supplied by the composition edge, called with the
    /// scope and `now` in unix seconds. Routing performs no I/O; `None` reads
    /// every route as unknown. Current positive authentication evidence also excludes a route.
    pub readiness: Option<ReadinessLookup<'a>>,
    /// Fixture-only capability facts; production always uses the adapter table.
    #[cfg(test)]
    pub launch_specs: Option<LaunchSpecLookup<'a>>,
    pub access: &'a [AccessContext<'a>],
    pub now: provenant_core::WallTime,
    pub host: &'a str,
}
impl RoutingContext<'_> {
    fn now_seconds(&self) -> i64 {
        self.now.as_timestamp().as_second()
    }
    fn validate(&self) -> Result<(), RoutingError> {
        if self.policy_account_hint.is_some_and(|hint| {
            hint.len() > 128
                || hint.split('/').count() > 2
                || hint
                    .split('/')
                    .any(|part| !crate::request::identifier(part))
        }) {
            return Err(RoutingError::InvalidValue);
        }
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

struct ValidatedContext<'a>(RoutingContext<'a>);
impl<'a> ValidatedContext<'a> {
    fn new(context: &RoutingContext<'a>) -> Result<Self, RoutingError> {
        context.validate()?;
        Ok(Self(RoutingContext { ..*context }))
    }
}
impl<'a> std::ops::Deref for ValidatedContext<'a> {
    type Target = RoutingContext<'a>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

enum AccountSelection<'a> {
    Any,
    Known(&'a str),
}
impl<'a> AccountSelection<'a> {
    fn parse(account: Option<&'a str>) -> Result<Self, RoutingError> {
        match account {
            None => Ok(Self::Any),
            Some("unknown") => Err(RoutingError::InvalidValue),
            Some(account) if crate::request::identifier(account) => Ok(Self::Known(account)),
            Some(_) => Err(RoutingError::InvalidValue),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct RouteList {
    pub valid_pairs: Vec<ValidRoutePair>,
    pub catalogue_revision: String,
    pub models: Vec<ModelEntry>,
    pub named_routes: std::collections::BTreeMap<String, NamedRoute>,
    pub pools: std::collections::BTreeMap<String, Vec<RoutePoolMemberView>>,
    pub pacing: std::collections::BTreeMap<String, PacingPolicy>,
    pub account_pacing: Vec<AccountPacingView>,
    pub configured_accounts: BTreeMap<String, PacingProfile>,
    pub configured_free: BTreeMap<String, PacingProfile>,
    pub active_overrides: Vec<PacingOverride>,
    pub model_exclusions: Vec<ModelExclusion>,
    /// Rows that were repaired or ignored while reading the configuration.
    pub exclusion_notes: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct ValidRoutePair {
    pub model: ModelId,
    pub effort: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct AccountPacingView {
    pub adapter: String,
    pub account: String,
    pub policy: PacingPolicy,
    pub source: String,
    pub profile: Option<PacingProfile>,
    pub expires: Option<provenant_core::WallTime>,
}
#[derive(Debug, Serialize)]
pub struct RoutePoolMemberView {
    pub model: ModelId,
    pub weight: u8,
    pub effort: Option<String>,
    pub efforts: Vec<String>,
    pub free: bool,
    pub pacing: PacingPolicy,
}
pub fn list(
    query: &RouteListQuery,
    context: &RoutingContext<'_>,
) -> Result<RouteList, RoutingError> {
    let context = &ValidatedContext::new(context)?;
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
            admissible(&member.model, member.effort.as_deref(), context)?;
            views.push(RoutePoolMemberView {
                model: member.model.clone(),
                weight: member.weight,
                effort: member.effort.clone(),
                efforts: member.efforts.clone(),
                free: context.catalogue.is_free_model(&member.model),
                pacing: pacing_policy(context.catalogue, context.config, &member.model),
            });
        }
        if !views.is_empty() || query.pool.as_deref() == Some(name.as_str()) {
            pools.insert(name, views);
        }
    }
    let pacing = effective_pacing(context.catalogue, context.config);
    let mut valid_pairs = Vec::new();
    if query.valid {
        for entry in context.catalogue.models.iter().filter(|entry| {
            matches(entry.model.adapter())
                && (query.pool.is_none()
                    || pools
                        .values()
                        .flatten()
                        .any(|member| member.model == entry.model))
        }) {
            let efforts = std::iter::once(None).chain(entry.efforts.iter().cloned().map(Some));
            for effort in efforts {
                if admissible(&entry.model, effort.as_deref(), context).is_ok() {
                    valid_pairs.push(ValidRoutePair {
                        model: entry.model.clone(),
                        effort,
                    });
                }
            }
        }
        pools.clear();
    }
    Ok(RouteList {
        valid_pairs,
        catalogue_revision: context.catalogue.revision.clone(),
        models: context
            .catalogue
            .models
            .iter()
            .filter(|m| !query.valid && matches(m.model.adapter()))
            .cloned()
            .collect(),
        named_routes: context
            .config
            .routes
            .iter()
            .filter(|(_, r)| !query.valid && matches(r.model.adapter()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pools,
        pacing,
        configured_accounts: context.config.pacing.accounts.clone(),
        configured_free: context.config.pacing.free.clone(),
        model_exclusions: context.config.model_exclusions.to_vec(),
        exclusion_notes: context.config.model_exclusions.notes().to_vec(),
        active_overrides: context
            .config
            .pacing_overrides
            .iter()
            .filter(|o| o.schedule.active(context.now))
            .cloned()
            .collect(),
        account_pacing: context
            .availability
            .iter()
            .filter_map(|a| {
                a.account_key
                    .as_deref()
                    .map(|account| (a.adapter.as_str(), account))
            })
            .chain(context.quota.iter().map(|q| {
                (
                    q.snapshot.account.adapter.as_str(),
                    q.snapshot.account.account_key.as_str(),
                )
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|(adapter, account)| {
                if !matches(adapter) {
                    return None;
                }
                let model = context
                    .catalogue
                    .models
                    .iter()
                    .find(|m| {
                        m.model.adapter() == adapter
                            && provenant_records::AdapterSpec::get(adapter).is_none_or(|spec| {
                                spec.account_serves_model(account, m.model.model())
                            })
                    })?
                    .model
                    .clone();
                let (policy, profile, source, expiry) =
                    effective_profile(context, &model, Some(account));
                Some(AccountPacingView {
                    adapter: adapter.into(),
                    account: account.into(),
                    policy,
                    source: source.into(),
                    profile: profile.cloned(),
                    expires: expiry,
                })
            })
            .collect(),
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
fn raw_pool<'a>(context: &'a RoutingContext<'_>, name: &str) -> Option<&'a Vec<RoutePoolMember>> {
    crate::ladder::pool(context.catalogue, context.config, name)
}
fn effective_pool(context: &RoutingContext<'_>, name: &str) -> Option<Vec<RoutePoolMember>> {
    let members = raw_pool(context, name)?;
    Some(
        members
            .iter()
            .map(|member| member.reconciled(context.catalogue.model(&member.model)).0)
            .collect(),
    )
}
/// Providers named by the primary route or pool and its explicit fallbacks.
/// Discovery must not run commands for unrelated installed providers.
pub fn probe_adapters(
    request: &RouteRequest,
    catalogue: &Catalogue,
    config: &RoutingConfig,
    registry: &[Registration],
) -> Result<Vec<&'static str>, RoutingError> {
    let mut adapters = BTreeSet::new();
    match &request.choice {
        RouteChoice::Model(model) => {
            adapters.insert(model.adapter());
        }
        RouteChoice::NamedRoute(name) => {
            if let Some(pool) = config
                .pools
                .get(name.as_str())
                .or_else(|| catalogue.pools.get(name.as_str()))
            {
                adapters.extend(pool.iter().map(|member| member.model.adapter()));
            } else {
                let route = config
                    .routes
                    .get(name.as_str())
                    .ok_or(RoutingError::UnknownRoute)?;
                adapters.insert(route.model.adapter());
            }
        }
    }
    if let Fallback::Models(models) = &request.fallback {
        adapters.extend(models.as_slice().iter().map(ModelId::adapter));
    }
    Ok(registry
        .iter()
        .map(|registration| registration.adapter.id())
        .filter(|id| adapters.contains(id))
        .collect())
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
    let key = provenant_records::AdapterSpec::get(model.adapter())
        .and_then(|spec| spec.pacing_keys)
        .map_or(model.adapter(), |(free, paid)| {
            if catalogue.is_free_model(model) {
                free
            } else {
                paid
            }
        });
    catalogue
        .pacing
        .get(key)
        .copied()
        .unwrap_or(PacingPolicy::Pace)
}
const PACE_MIN_BASIS_POINTS: u32 = 2_500;
const PACE_MAX_BASIS_POINTS: u32 = 20_000;
const NEUTRAL_FACTOR_BASIS_POINTS: u32 = 10_000;

fn effective_profile<'a>(
    context: &'a RoutingContext<'_>,
    model: &ModelId,
    account: Option<&str>,
) -> (
    PacingPolicy,
    Option<&'a PacingProfile>,
    &'static str,
    Option<provenant_core::WallTime>,
) {
    let account_profile = account.and_then(|a| {
        context
            .config
            .pacing
            .accounts
            .get(&format!("{}/{a}", model.adapter()))
    });
    let free_profile = context
        .catalogue
        .is_free_model(model)
        .then(|| context.config.pacing.free.get(model.adapter()))
        .flatten();
    // The account profile governs free models too; the free profile is the
    // provider-level default beneath it.
    let profile = account_profile.or(free_profile);
    let active = context.config.pacing_overrides.iter().rev().find(|o| {
        o.schedule.active(context.now)
            && o.account.as_deref().is_none_or(|selector| {
                account.is_some_and(|key| {
                    selector
                        .split_once('/')
                        .map_or(selector == key, |(adapter, selected)| {
                            adapter == model.adapter() && selected == key
                        })
                })
            })
            && o.provider.as_deref().is_none_or(|p| p == model.adapter())
    });
    if let Some(o) = active {
        return (o.policy, profile, "override", o.schedule.expires);
    }
    if let Some(policy) = account_profile.and_then(|p| p.policy) {
        return (policy, profile, "account", None);
    }
    if let Some(policy) = free_profile.and_then(|p| p.policy) {
        return (policy, profile, "free", None);
    }
    let source = if profile.is_some() {
        "account"
    } else if context.config.pacing.contains_key(model.adapter()) {
        "provider"
    } else {
        "bundled"
    };
    (
        pacing_policy(context.catalogue, context.config, model),
        profile,
        source,
        None,
    )
}
fn window_matches(
    window: &provenant_quota::QuotaWindow,
    kind: PacingWindow,
    adapter: &str,
) -> bool {
    let pool = window.pool.to_ascii_lowercase();
    let name = pool.split(':').next().unwrap_or(&pool);
    match kind {
        PacingWindow::FiveHour => {
            window.window_seconds == Some(18_000)
                || name.ends_with("five_hour")
                || name.ends_with(".5h")
        }
        PacingWindow::SevenDay => {
            window.window_seconds == Some(604_800)
                || name.ends_with("seven_day")
                || name.ends_with("weekly")
        }
        PacingWindow::Monthly => {
            name.ends_with("monthly")
                || name.ends_with("thirty_day")
                || name.ends_with("billing_cycle")
        }
        PacingWindow::Daily => window.window_seconds == Some(86_400) || name.ends_with("daily"),
        PacingWindow::Rolling => {
            provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| {
                spec.quota_probe == provenant_records::QuotaProbeKind::LocalLedger
            }) && (name.ends_with("five_hour") || name.ends_with("rolling"))
        }
    }
}
fn selected_window(
    window: &provenant_quota::QuotaWindow,
    profile: Option<&PacingProfile>,
    adapter: &str,
    model_pool: Option<&str>,
) -> bool {
    window_serves(adapter, &window.pool, model_pool)
        && profile
            .and_then(|p| p.windows.as_ref())
            .is_none_or(|windows| {
                windows
                    .iter()
                    .any(|kind| window_matches(window, *kind, adapter))
            })
}
/// Returns the instant and, when a monthly day was clamped to a shorter month,
/// the requested and the actual day.
fn window_deadline(
    deadline: &FixedDeadline,
    window: &provenant_quota::QuotaWindow,
) -> Option<(i64, Option<(i8, i8)>)> {
    let zone = jiff::tz::TimeZone::system();
    if let FixedDeadline::Absolute(datetime) = deadline {
        return zone
            .to_ambiguous_zoned(*datetime)
            .compatible()
            .ok()
            .map(|z| (z.timestamp().as_second(), None));
    }
    let reset = window.resets_unix_seconds?;
    let duration = i64::try_from(window.window_seconds?).ok()?;
    let mut date = jiff::Timestamp::from_second(reset)
        .ok()?
        .to_zoned(zone.clone())
        .date();
    for _ in 0..=62 {
        let mut clamped = None;
        let (matches, hour, minute) = match deadline {
            FixedDeadline::Monthly { day } => {
                // A day beyond the month's end falls on its last day.
                let last = date.days_in_month();
                if *day > last {
                    clamped = Some((*day, last));
                }
                (date.day() == (*day).min(last), 0, 0)
            }
            FixedDeadline::Weekly {
                weekday,
                hour,
                minute,
            } => (
                date.weekday().to_monday_zero_offset() == i8::try_from(*weekday).ok()?,
                *hour,
                *minute,
            ),
            FixedDeadline::Absolute(_) => unreachable!("handled above"),
        };
        if matches {
            let instant = zone
                .to_ambiguous_zoned(date.at(hour, minute, 0, 0))
                .compatible()
                .ok()?
                .timestamp()
                .as_second();
            if instant <= reset && instant > reset.saturating_sub(duration) {
                return Some((instant, clamped));
            }
        }
        date = date.yesterday().ok()?;
    }
    None
}
/// A reference is evidence about another account, not a recursive profile lookup.
fn reference_observation<'a>(
    deadline: &ResetDeadline,
    context: &'a RoutingContext<'_>,
) -> Option<&'a QuotaObservation> {
    // The latest fresh snapshot wins whatever its status: a newer unknown
    // snapshot supersedes any older reset.
    context
        .quota
        .iter()
        .filter(|o| {
            // `reset_of` is adapter-qualified, like every pacing account key.
            deadline
                .reset_of
                .split_once('/')
                .is_some_and(|(adapter, key)| {
                    o.snapshot.account.adapter == adapter && o.snapshot.account.account_key == key
                })
                && o.snapshot.schema_version == 1
                && o.snapshot.observed_unix_seconds <= context.now_seconds()
                && o.snapshot.observed_unix_seconds < o.expires_unix_seconds
                && o.expires_unix_seconds > context.now_seconds()
        })
        .max_by_key(|o| (o.snapshot.observed_unix_seconds, o.snapshot.revision))
}
fn fallback_reason(deadline: &ResetDeadline, context: &RoutingContext<'_>) -> String {
    match reference_observation(deadline, context) {
        Some(o) if !reference_status_usable(o.snapshot.status) => format!(
            "reset_of={} latest snapshot is {:?}",
            deadline.reset_of, o.snapshot.status
        ),
        _ => format!("reset_of={} unknown or stale", deadline.reset_of),
    }
}
fn reference_status_usable(status: QuotaStatus) -> bool {
    matches!(
        status,
        QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
    )
}
fn reference_deadline(
    deadline: &ResetDeadline,
    window: Option<&provenant_quota::QuotaWindow>,
    model_pool: Option<&str>,
    context: &RoutingContext<'_>,
) -> Option<i64> {
    let observation = reference_observation(deadline, context)
        .filter(|o| reference_status_usable(o.snapshot.status))?;
    observation
        .snapshot
        .windows
        .iter()
        .filter(|w| window_matches(w, deadline.window, &observation.snapshot.account.adapter))
        .filter(|w| window_serves(&observation.snapshot.account.adapter, &w.pool, model_pool))
        .filter_map(|peer| {
            let reset = peer.resets_unix_seconds?;
            if reset <= observation.snapshot.observed_unix_seconds {
                return None;
            }
            // Keep a handover in this account's current cycle when a freshly
            // observed peer reset advances into the following cycle.
            if let Some(window) = window
                && let (Some(own_reset), Some(own_duration), Some(peer_duration)) = (
                    window.resets_unix_seconds,
                    window.window_seconds,
                    peer.window_seconds,
                )
                && reset > own_reset
                && let Some(previous) = reset.checked_sub(i64::try_from(peer_duration).ok()?)
                && previous > own_reset.saturating_sub(i64::try_from(own_duration).ok()?)
            {
                return Some(previous);
            }
            Some(reset)
        })
        .min()
}

fn pace_snapshot_factor(
    snapshot: &provenant_quota::QuotaSnapshot,
    now: i64,
    profile: Option<&PacingProfile>,
    adapter: &str,
    model_pool: Option<&str>,
    context: &RoutingContext<'_>,
) -> Option<(u32, String, bool)> {
    snapshot
        .windows
        .iter()
        .filter(|w| selected_window(w, profile, adapter, model_pool))
        .filter_map(|window| {
            if snapshot.details.as_ref().is_some_and(|details| {
                details.coverage.as_deref() == Some("partial")
                    && !details.calibration_active(&window.pool, now)
            }) {
                return None;
            }
            let duration = window.window_seconds?;
            if duration == 0 {
                return None;
            }
            let calibrated = snapshot
                .details
                .as_ref()
                .is_some_and(|d| d.calibration_active(&window.pool, now));
            // An active usage floor remains binding after a ledger reset. With
            // no future reset, use a full remaining window conservatively.
            let reset_known = window.resets_unix_seconds.is_some_and(|reset| reset > now);
            let reset = match window.resets_unix_seconds {
                Some(reset) if reset > now => reset,
                None if provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| {
                    spec.quota_probe == provenant_records::QuotaProbeKind::LocalLedger
                }) && snapshot
                    .details
                    .as_ref()
                    .is_some_and(|d| d.source.as_deref() == Some("local-ledger")) =>
                {
                    now.checked_add(i64::try_from(duration).ok()?)?
                }
                _ if calibrated => now.checked_add(i64::try_from(duration).ok()?)?,
                _ => return None,
            };
            let configured = profile.and_then(|p| p.deadline.as_ref());
            let resolved = configured.and_then(|d| match d {
                PacingDeadline::ResetOf(reference) => {
                    reference_deadline(reference, Some(window), model_pool, context)
                        .map(|d| (d, None))
                }
                PacingDeadline::Fixed(fixed) => window_deadline(fixed, window),
            });
            let clamped = resolved.and_then(|(_, clamped)| clamped);
            let deadline = resolved.map(|(d, _)| d.min(reset));
            let fallback =
                matches!(configured, Some(PacingDeadline::ResetOf(_))) && deadline.is_none();
            let passed = deadline.is_some_and(|d| d <= now);
            let standby =
                passed && profile.is_some_and(|p| p.after_deadline == AfterDeadline::Standby);
            let target = deadline.filter(|d| *d > now).unwrap_or(reset).min(reset);
            let remaining = u64::try_from(target.checked_sub(now)?).ok()?.min(duration);
            if remaining == 0 {
                return None;
            }
            let headroom = 10_000_u64.saturating_sub(window.used_basis_points.min(10_000));
            let factor = (u128::from(headroom) * u128::from(duration)) / u128::from(remaining);
            let factor = u32::try_from(factor.clamp(
                u128::from(PACE_MIN_BASIS_POINTS),
                u128::from(PACE_MAX_BASIS_POINTS),
            ))
            .ok()?;
            Some((
                factor,
                format!(
                    "window={} deadline={}{}{}{}{}",
                    window.pool,
                    deadline.unwrap_or(reset),
                    if reset_known {
                        ""
                    } else if calibrated {
                        " calibration; reset unknown; full window remaining"
                    } else {
                        " ledger; reset unknown; full window remaining"
                    },
                    if standby { " standby" } else { "" },
                    match (fallback, configured) {
                        (true, Some(PacingDeadline::ResetOf(reference))) => format!(
                            " {}; falling back to window reset",
                            fallback_reason(reference, context)
                        ),
                        _ => String::new(),
                    },
                    clamped.map_or_else(String::new, |(day, last)| format!(
                        " day {day} clamped to the month's last day ({last})"
                    )),
                ),
                standby,
            ))
        })
        .min_by_key(|(factor, _, standby)| (!*standby, *factor))
}
fn pacing_factor_basis_points(
    policy: PacingPolicy,
    model: &ModelId,
    account: Option<&str>,
    context: &RoutingContext<'_>,
) -> (u32, String, bool) {
    let (_, profile, source, expiry) = effective_profile(context, model, account);
    let detail = format!(
        "source={source} policy={policy:?}{}",
        expiry.map(|e| format!(" until={e}")).unwrap_or_default()
    );
    let detail = match (
        provenant_records::AdapterSpec::get(model.adapter()).is_some_and(|s| s.grouped_quota_pools),
        context.catalogue.quota_pool(model),
    ) {
        (true, Some(pool)) => format!("{detail} quota_pool={pool}"),
        (true, None) => {
            format!("{detail} quota_pool unset; all quota windows apply (conservative)")
        }
        _ => detail,
    };
    let detail = if provenant_records::AdapterSpec::get(model.adapter())
        .and_then(|spec| spec.allowance_prefix)
        .is_some_and(|prefix| model.model().starts_with(prefix))
    {
        let observation = go_quota_observation(model, account, context);
        let details = observation.and_then(|o| o.snapshot.details.as_ref());
        if details.is_some_and(|d| d.coverage.as_deref() == Some("complete")) {
            detail
        } else {
            let missing: Vec<_> = ["five_hour", "seven_day", "thirty_day"]
                .into_iter()
                .filter(|pool| {
                    !details.is_some_and(|d| d.calibration_active(pool, context.now_seconds()))
                })
                .collect();
            if missing.is_empty() {
                format!("{detail}; Go quota source: local-ledger, coverage: partial")
            } else if account.is_none() {
                // Calibration is keyed by account; with none resolved there is
                // nothing to match, so claiming it is missing would mislead.
                format!(
                    "{detail}; Go quota source: local-ledger, coverage: partial; no account resolved, so Go calibration was not checked"
                )
            } else {
                format!(
                    "{detail}; Go quota source: local-ledger, coverage: partial; Go calibration missing or stale: {} (calibration key must match account key {:?}); calibration would tighten pacing",
                    missing.join(", "),
                    account.unwrap_or_default()
                )
            }
        }
    } else {
        detail
    };
    if policy != PacingPolicy::Pace {
        return (NEUTRAL_FACTOR_BASIS_POINTS, detail, false);
    }
    let result = context
        .quota
        .iter()
        .filter(|o| {
            o.model == *model
                && o.snapshot.schema_version == 1
                && o.snapshot.account.adapter == model.adapter()
                && Some(o.snapshot.account.account_key.as_str()) == account
                && o.snapshot.observed_unix_seconds <= context.now_seconds()
                && o.snapshot.observed_unix_seconds < o.expires_unix_seconds
                && o.expires_unix_seconds > context.now_seconds()
        })
        .max_by_key(|o| (o.snapshot.observed_unix_seconds, o.snapshot.revision))
        .filter(|o| {
            matches!(
                o.snapshot.status,
                QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
            )
        })
        .and_then(|o| {
            pace_snapshot_factor(
                &o.snapshot,
                context.now_seconds(),
                profile,
                model.adapter(),
                context.catalogue.quota_pool(model),
                context,
            )
        });
    let reserve_detail = context
        .quota
        .iter()
        .filter(|o| {
            o.model == *model
                && Some(o.snapshot.account.account_key.as_str()) == account
                && o.snapshot.observed_unix_seconds <= context.now_seconds()
                && o.snapshot.observed_unix_seconds < o.expires_unix_seconds
                && o.expires_unix_seconds > context.now_seconds()
        })
        .max_by_key(|o| (o.snapshot.observed_unix_seconds, o.snapshot.revision))
        .and_then(|o| {
            pacing_reserve_window(
                &o.snapshot,
                context.now_seconds(),
                context
                    .config
                    .pacing_reserve
                    .get(model.adapter())
                    .copied()
                    .unwrap_or_default(),
                profile,
                model.adapter(),
                context.catalogue.quota_pool(model),
            )
        })
        .map(|window| format!(" reserve warning window={}", window.pool))
        .unwrap_or_default();
    let detail = format!("{detail}{reserve_detail}");
    let neutral_detail = match profile.and_then(|p| p.deadline.as_ref()) {
        Some(PacingDeadline::ResetOf(reference))
            if reference_deadline(
                reference,
                None,
                context.catalogue.quota_pool(model),
                context,
            )
            .is_none() =>
        {
            format!(
                "{detail} {}; falling back to window reset; unobserved windows neutral",
                fallback_reason(reference, context)
            )
        }
        _ => format!("{detail} unobserved windows neutral"),
    };
    // An account-level exhausted record dominates unprobed per-model windows.
    let account_exhausted = result.is_none()
        && context.quota.iter().any(|o| {
            o.snapshot.schema_version == 1
                && o.snapshot.account.adapter == model.adapter()
                && Some(o.snapshot.account.account_key.as_str()) == account
                && o.snapshot.status == QuotaStatus::Exhausted
                && o.snapshot.observed_unix_seconds <= context.now_seconds()
                && o.snapshot.observed_unix_seconds < o.expires_unix_seconds
                && o.expires_unix_seconds > context.now_seconds()
                && quota_reading_spent(
                    &o.snapshot,
                    model,
                    context.catalogue.quota_pool(model),
                    context.now_seconds(),
                )
        });
    if account_exhausted {
        return (
            0,
            format!("{detail} account exhausted; model windows unobserved standby"),
            true,
        );
    }
    result.map_or(
        (NEUTRAL_FACTOR_BASIS_POINTS, neutral_detail, false),
        |(factor, window, standby)| {
            (
                if standby { 0 } else { factor },
                format!("{detail} {window}"),
                standby,
            )
        },
    )
}
#[derive(Debug, Serialize)]
pub struct PacingPreview {
    pub account: Option<String>,
    pub policy: PacingPolicy,
    pub factor_basis_points: u32,
    pub detail: String,
}
/// Explain cached account profiles without probing or granting dispatch readiness.
pub fn pacing_preview(model: &ModelId, context: &RoutingContext<'_>) -> Vec<PacingPreview> {
    let mut accounts: BTreeSet<_> = context
        .quota
        .iter()
        .filter(|q| {
            q.snapshot.account.adapter == model.adapter()
                && provenant_records::AdapterSpec::get(model.adapter()).is_none_or(|spec| {
                    spec.account_serves_model(&q.snapshot.account.account_key, model.model())
                })
        })
        .map(|q| Some(q.snapshot.account.account_key.clone()))
        .collect();
    accounts.extend(
        context
            .availability
            .iter()
            .filter(|a| {
                a.adapter == model.adapter()
                    && provenant_records::AdapterSpec::get(model.adapter()).is_none_or(|spec| {
                        a.account_key
                            .as_deref()
                            .is_none_or(|key| spec.account_serves_model(key, model.model()))
                    })
            })
            .map(|a| a.account_key.clone()),
    );
    if accounts.is_empty() {
        accounts.insert(None);
    }
    accounts
        .into_iter()
        .map(|account| {
            let (policy, _, _, _) = effective_profile(context, model, account.as_deref());
            let (factor_basis_points, detail, _) =
                pacing_factor_basis_points(policy, model, account.as_deref(), context);
            PacingPreview {
                account,
                policy,
                factor_basis_points,
                detail,
            }
        })
        .collect()
}

struct ReserveWindow {
    pool: String,
    percent: u8,
    profile: bool,
    seven_day: bool,
}

fn pacing_reserve_window(
    snapshot: &provenant_quota::QuotaSnapshot,
    now: i64,
    reserve: PacingReserve,
    profile: Option<&PacingProfile>,
    adapter: &str,
    model_pool: Option<&str>,
) -> Option<ReserveWindow> {
    let reserve_for = |window: &provenant_quota::QuotaWindow| {
        let kind = [
            PacingWindow::Rolling,
            PacingWindow::FiveHour,
            PacingWindow::SevenDay,
            PacingWindow::Daily,
            PacingWindow::Monthly,
        ]
        .into_iter()
        .find(|k| window_matches(window, *k, adapter));
        let selected = profile.and_then(|p| {
            p.reserve
                .iter()
                .find(|(k, _)| window_matches(window, **k, adapter))
                .map(|(_, value)| *value)
        });
        let percent = selected.unwrap_or(match kind {
            Some(PacingWindow::FiveHour | PacingWindow::Rolling) => reserve.five_hour_percent,
            Some(PacingWindow::SevenDay) => reserve.seven_day_percent,
            _ => 0,
        });
        (
            percent,
            selected.is_some(),
            matches!(kind, Some(PacingWindow::SevenDay)),
        )
    };
    snapshot
        .windows
        .iter()
        .filter(|w| selected_window(w, profile, adapter, model_pool))
        .find(|window| {
            let (percent, _, _) = reserve_for(window);
            percent > 0
                && (window.resets_unix_seconds.is_some_and(|reset| reset > now)
                    || (window.resets_unix_seconds.is_none()
                        && provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| {
                            spec.quota_probe == provenant_records::QuotaProbeKind::LocalLedger
                        })
                        && snapshot
                            .details
                            .as_ref()
                            .is_some_and(|d| d.source.as_deref() == Some("local-ledger")))
                    || snapshot
                        .details
                        .as_ref()
                        .is_some_and(|d| d.calibration_active(&window.pool, now)))
                && window.used_basis_points >= 10_000 - u64::from(percent) * 100
        })
        .map(|window| {
            let (percent, profile, seven_day) = reserve_for(window);
            ReserveWindow {
                pool: window.pool.clone(),
                percent,
                profile,
                seven_day,
            }
        })
}

fn availability_reason(
    record: Option<&AvailabilityRecord>,
    ctx: &RoutingContext<'_>,
) -> Option<RouteExclusionReason> {
    let Some(record) = record else {
        return Some(RouteExclusionReason::UnknownAvailability);
    };
    if record.catalogue_revision != ctx.catalogue.revision
        || record.observed_unix_seconds > ctx.now_seconds()
        || record.expires_unix_seconds <= ctx.now_seconds()
    {
        return Some(RouteExclusionReason::StaleAvailability);
    }
    if record.account_key.is_none() {
        return Some(RouteExclusionReason::UnknownAccount);
    }
    None
}
fn account_reason<'q>(
    model: &ModelId,
    availability: &AvailabilityRecord,
    access: Option<&AccessContext<'_>>,
    context: &'q RoutingContext<'_>,
) -> (Option<RouteExclusionReason>, Option<&'q QuotaObservation>) {
    let mut reason = availability_reason(Some(availability), context).filter(|r| {
        !matches!(
            r,
            RouteExclusionReason::StaleAvailability | RouteExclusionReason::UnknownAvailability
        )
    });
    // Missing access means preparation did not establish confinement. Only an
    // explicit disclosure refusal may be reported as a disclosure-policy denial.
    if reason.is_none() && access.is_some_and(|a| a.disclosure_ref.is_empty()) {
        reason = Some(RouteExclusionReason::DisclosureDenied);
    }
    if reason.is_none() && access.is_none_or(|a| a.confinement_ref.is_empty()) {
        reason = Some(RouteExclusionReason::ConfinementUnavailable);
    }
    if reason.is_none()
        && let Some(key) = availability.account_key.as_deref()
    {
        reason = account_block_reason(model, key, context);
    }
    if reason.is_none() {
        reason = readiness_reason(model, availability, context);
    }
    if reason.is_none()
        && latest_reading(model, availability, context, true)
            .is_some_and(|q| q.snapshot.status == QuotaStatus::AuthRequired)
    {
        reason = Some(RouteExclusionReason::AuthRequired);
    }
    // Explicit exact IDs use the executable/account preflight too. The provider
    // owns model availability; named routes and pools require catalogue entries.
    let quota = latest_reading(model, availability, context, true);
    if reason.is_none()
        && let Some(q) = quota
    {
        let partial = provenant_quota::partial_headroom(
            &q.snapshot,
            model.adapter(),
            model.model(),
            &context.catalogue.admission_pools(model),
            context.now_seconds(),
        );
        reason = if partial == provenant_quota::PartialHeadroom::Exhausted
            || reading_exhausted(&q.snapshot, model, context)
        {
            Some(RouteExclusionReason::Exhausted)
        } else {
            None
        };
    }
    let (policy, _, _, _) = effective_profile(context, model, availability.account_key.as_deref());
    if reason.is_none() && policy == PacingPolicy::Off {
        reason = Some(RouteExclusionReason::Disabled);
    }
    (reason, quota)
}
/// How a route's unavailability reads to a person. Account evidence carries the
/// quota repair text, so a refused or mismatched account says what to do next.
pub fn exclusion_detail(reason: &RouteExclusionReason) -> String {
    let diagnostic = reason.diagnostic("REFUSED");
    format!(
        "{}; {}",
        diagnostic.detail.unwrap_or_default(),
        diagnostic.next.unwrap_or_default()
    )
}
/// Persisted mismatch or refusal evidence, assessed by the same quota function
/// that admission rechecks before launch. Invalid evidence excludes only the
/// account it belongs to; it is never read as proof the account is ready.
fn account_block_reason(
    model: &ModelId,
    account_key: &str,
    context: &RoutingContext<'_>,
) -> Option<RouteExclusionReason> {
    let pools = context.catalogue.admission_pools(model);
    let admission = provenant_quota::Admission {
        policies: context.quota_policies,
        now_unix_seconds: context.now_seconds(),
        records: context.accounts,
        pools: &pools,
    };
    match admission.account_block(model.adapter(), account_key, model.model()) {
        Err(_) => Some(RouteExclusionReason::InvalidAccountEvidence),
        Ok(block) => block.map(provenant_quota::AccountBlock::exclusion_reason),
    }
}
/// An `unavailable` readiness verdict for this account on the model's own
/// route or any of its verified pools, or on the whole account, excludes the
/// candidate with the verdict's projected reason. Unknown (absent, stale,
/// unreachable or damaged) never excludes: it is reported, not enforced.
fn readiness_reason(
    model: &ModelId,
    availability: &AvailabilityRecord,
    context: &RoutingContext<'_>,
) -> Option<RouteExclusionReason> {
    let lookup = context.readiness?;
    let account_key = availability.account_key.as_deref()?;
    let now = context.now_seconds();
    std::iter::once(format!("{}/{}", model.adapter(), model.model()))
        .chain(context.catalogue.admission_pools(model))
        .find_map(|route| {
            let read = lookup(
                &provenant_quota::ReadinessScope {
                    host: provenant_quota::LOCAL_HOST.into(),
                    adapter: model.adapter().into(),
                    account_key: account_key.into(),
                    route,
                },
                now,
            );
            if read.observed_scope.is_some()
                && read.observed_unix_seconds.is_some_and(|at| at <= now)
                && read.expires_unix_seconds.is_some_and(|expiry| expiry > now)
            {
                match read.reason {
                    Some(provenant_quota::ReadinessReason::SignedOut) => {
                        return Some(RouteExclusionReason::AuthRequired);
                    }
                    Some(provenant_quota::ReadinessReason::LoginExpired) => {
                        return Some(RouteExclusionReason::LoginExpired);
                    }
                    Some(provenant_quota::ReadinessReason::AccountMismatch) => {
                        return Some(RouteExclusionReason::AccountMismatch);
                    }
                    _ => {}
                }
            }
            if read.state != provenant_quota::ReadinessState::Unavailable {
                return None;
            }
            match read.project() {
                RouteReadiness::Unavailable {
                    reason:
                        RouteExclusionReason::UnknownAvailability
                        | RouteExclusionReason::StaleAvailability
                        | RouteExclusionReason::AuthRequired
                        | RouteExclusionReason::LoginExpired,
                    ..
                } => None,
                RouteReadiness::Unavailable { reason, .. } => Some(reason),
                _ => None,
            }
        })
}

fn uncertainty(
    model: &ModelId,
    availability: &AvailabilityRecord,
    context: &RoutingContext<'_>,
) -> Option<String> {
    if let Some(reason) = availability_reason(Some(availability), context)
        && matches!(
            reason,
            RouteExclusionReason::StaleAvailability | RouteExclusionReason::UnknownAvailability
        )
    {
        return Some(format!("{reason:?}"));
    }
    if latest_reading(model, availability, context, true)
        .is_some_and(|q| q.snapshot.status == QuotaStatus::Unknown)
    {
        return Some("unknown quota".into());
    }
    if latest_reading(model, availability, context, false).is_some()
        && latest_reading(model, availability, context, true).is_none()
    {
        return Some("stale quota".into());
    }
    let lookup = context.readiness?;
    let account_key = availability.account_key.as_ref()?;
    std::iter::once(format!("{}/{}", model.adapter(), model.model()))
        .chain(context.catalogue.admission_pools(model))
        .enumerate()
        .find_map(|(index, route)| {
            let read = lookup(
                &provenant_quota::ReadinessScope {
                    host: provenant_quota::LOCAL_HOST.into(),
                    adapter: model.adapter().into(),
                    account_key: account_key.clone(),
                    route,
                },
                context.now_seconds(),
            );
            if read.observed_scope.is_none()
                && latest_reading(model, availability, context, true).is_some()
            {
                return None;
            }
            ((index == 0 && read.state == provenant_quota::ReadinessState::Unknown)
                || matches!(
                    read.project(),
                    RouteReadiness::Unavailable {
                        reason: RouteExclusionReason::UnknownAvailability
                            | RouteExclusionReason::StaleAvailability
                            | RouteExclusionReason::AuthRequired
                            | RouteExclusionReason::LoginExpired,
                        ..
                    }
                ) && (index == 0 || read.observed_scope.is_some()))
            .then(|| format!("{:?}", read.reason))
        })
}
/// The account's latest reading for this model, only a fresh one when
/// `fresh`. Only a fresh reading may refuse the route; a stale one still ranks
/// the account.
fn latest_reading<'q>(
    model: &ModelId,
    availability: &AvailabilityRecord,
    context: &'q RoutingContext<'_>,
    fresh: bool,
) -> Option<&'q QuotaObservation> {
    let now = context.now_seconds();
    context
        .quota
        .iter()
        .filter(|q| {
            q.model == *model
                && q.snapshot.schema_version == 1
                && q.snapshot.account.adapter == model.adapter()
                && Some(&q.snapshot.account.account_key) == availability.account_key.as_ref()
                && q.snapshot.observed_unix_seconds <= now
                && (!fresh
                    || (q.snapshot.observed_unix_seconds < q.expires_unix_seconds
                        && now < q.expires_unix_seconds))
        })
        .max_by_key(|q| (q.snapshot.observed_unix_seconds, q.snapshot.revision))
}
fn reading_exhausted(
    snapshot: &provenant_quota::QuotaSnapshot,
    model: &ModelId,
    context: &RoutingContext<'_>,
) -> bool {
    quota_reading_spent(
        snapshot,
        model,
        context.catalogue.quota_pool(model),
        context.now_seconds(),
    )
}
/// The one rule for a spent account, whatever its pacing policy: an exhausted
/// snapshot whose full windows serving `model` have not all reset, or a
/// grouped pool serving it at its limit. `model_pool` is the catalogue's quota
/// pool for the model. Freshness is the caller's concern.
pub fn quota_reading_spent(
    snapshot: &provenant_quota::QuotaSnapshot,
    model: &ModelId,
    model_pool: Option<&str>,
    now_seconds: i64,
) -> bool {
    match snapshot.status {
        QuotaStatus::Exhausted => !exhaustion_lapsed(snapshot, now_seconds, model_pool),
        QuotaStatus::Available | QuotaStatus::CreditOnly => {
            model_pool_exhausted(snapshot, model, model_pool, now_seconds)
        }
        _ => false,
    }
}
/// A grouped adapter's snapshot stays available while another group has
/// capacity; the windows serving this model (its pool, ungrouped windows, or
/// every window when it has no pool) may still be spent.
fn model_pool_exhausted(
    snapshot: &provenant_quota::QuotaSnapshot,
    model: &ModelId,
    pool: Option<&str>,
    now: i64,
) -> bool {
    if !provenant_records::AdapterSpec::get(model.adapter()).is_some_and(|s| s.grouped_quota_pools)
    {
        return false;
    }
    snapshot
        .windows
        .iter()
        .filter(|w| window_serves(model.adapter(), &w.pool, pool))
        .any(|w| {
            w.used_basis_points >= 10_000
                && (w.resets_unix_seconds.is_none_or(|reset| reset > now)
                    || snapshot
                        .details
                        .as_ref()
                        .is_some_and(|d| d.calibration_active(&w.pool, now)))
        })
}
fn window_serves(adapter: &str, window: &str, model_pool: Option<&str>) -> bool {
    provenant_records::AdapterSpec::get(adapter)
        .is_none_or(|spec| spec.window_serves_pool(window, model_pool))
}
/// An exhausted snapshot stops binding once every full window has reset.
fn exhaustion_lapsed(
    snapshot: &provenant_quota::QuotaSnapshot,
    now_seconds: i64,
    model_pool: Option<&str>,
) -> bool {
    let mut full = snapshot
        .windows
        .iter()
        .filter(|w| window_serves(&snapshot.account.adapter, &w.pool, model_pool))
        .filter(|w| w.used_basis_points >= 10_000)
        .peekable();
    full.peek().is_some()
        && full.all(|w| {
            !snapshot
                .details
                .as_ref()
                .is_some_and(|d| d.calibration_active(&w.pool, now_seconds))
                && w.resets_unix_seconds
                    .is_some_and(|reset| reset <= now_seconds)
        })
}
/// A registered adapter and, when set, an exact recorded effort control.
fn admissible<'c>(
    model: &ModelId,
    effort: Option<&str>,
    context: &'c ValidatedContext<'_>,
) -> Result<Option<&'c ModelEntry>, RoutingError> {
    context
        .registry
        .iter()
        .find(|r| r.adapter.id() == model.adapter())
        .ok_or(RoutingError::UnknownAdapter)?;
    let entry = context.catalogue.model(model);
    // Backstop for uncatalogued models; catalogued ones record their own efforts.
    if let Some(effort) = effort
        && let Some(spec) = provenant_records::AdapterSpec::get(model.adapter())
        && !spec.launch.effort
    {
        return Err(RoutingError::UnsupportedEffort {
            model: model.as_str().to_owned(),
            effort: effort.to_owned(),
            support: format!("{} takes no effort control", spec.name),
        });
    }
    if let Some(effort) = effort.filter(|e| {
        !crate::request::effort_valid(e)
            || entry.is_some_and(|row| !row.efforts.iter().any(|v| v == e))
    }) {
        let support = match entry {
            Some(row) if row.efforts.is_empty() => "this model takes no effort".to_owned(),
            Some(row) => format!("supported efforts: {}", row.efforts.join(", ")),
            None => {
                let nearest = context.catalogue.nearest_models(model);
                if nearest.is_empty() {
                    "model is not in the catalogue; no effort controls are recorded".to_owned()
                } else {
                    format!(
                        "model is not in the catalogue; nearest catalogued IDs: {}",
                        nearest.join(", ")
                    )
                }
            }
        };
        return Err(RoutingError::UnsupportedEffort {
            model: model.as_str().to_owned(),
            effort: effort.to_owned(),
            support,
        });
    }
    Ok(entry)
}
fn cooling(model: &ModelId, account: Option<&str>, context: &ValidatedContext<'_>) -> bool {
    let now = context.now_seconds();
    context.capacity_cooldowns.iter().any(|record| {
        record.account.adapter == model.adapter()
            && Some(record.account.account_key.as_str()) == account
            && record.model == model.model()
            && record.observed_unix_seconds <= now
            && now < record.expires_unix_seconds
    })
}

fn account_load(
    model: &ModelId,
    availability: &AvailabilityRecord,
    context: &RoutingContext<'_>,
) -> AccountLoad {
    let key = availability.account_key.as_deref();
    let (policy, profile, pacing_source, _) = effective_profile(context, model, key);
    let (pacing_factor, _, _) = pacing_factor_basis_points(policy, model, key, context);
    let quota = latest_reading(model, availability, context, true);
    let limiting = quota
        .into_iter()
        .flat_map(|q| &q.snapshot.windows)
        .filter(|w| {
            selected_window(
                w,
                profile,
                model.adapter(),
                context.catalogue.quota_pool(model),
            )
        })
        .filter(|w| {
            w.resets_unix_seconds
                .is_none_or(|reset| reset > context.now_seconds())
        })
        .map(|w| {
            let headroom = 10_000_u64.saturating_sub(w.used_basis_points.min(10_000));
            let eta = w
                .resets_unix_seconds
                .map(|reset| reset.saturating_sub(context.now_seconds()).max(1) as u64);
            (headroom, eta)
        })
        .min_by_key(|(headroom, eta)| {
            u128::from(*headroom) * 3_600 / u128::from(eta.unwrap_or(3_600))
        });
    let mut warnings = Vec::new();
    if let Err(warning) = check_go_allowance(model, context.config, context.catalogue) {
        let source = context
            .config
            .preference_sources
            .get("domains.routing.opencode_go.min_monthly_allowance_usd")
            .map(String::as_str)
            .unwrap_or("unresolved");
        warnings.push(format!(
            "{warning}; source={source}:domains.routing.opencode_go.min_monthly_allowance_usd"
        ));
    }
    if let Some(q) = quota {
        if !context.config.allow_unknown_partial_headroom
            && provenant_quota::partial_headroom(
                &q.snapshot,
                model.adapter(),
                model.model(),
                &context.catalogue.admission_pools(model),
                context.now_seconds(),
            ) == provenant_quota::PartialHeadroom::Unknown
        {
            warnings.push(format!(
                "partial headroom unknown; ranking penalty; source={:?}",
                context
                    .config
                    .partial_headroom_source
                    .as_ref()
                    .map(|source| format!("{source:?}"))
                    .unwrap_or_else(
                        || "default:routing.allow_unknown_partial_headroom=false".into()
                    )
            ));
        }
        if policy == PacingPolicy::Pace
            && let Some(window) = pacing_reserve_window(
                &q.snapshot,
                context.now_seconds(),
                context
                    .config
                    .pacing_reserve
                    .get(model.adapter())
                    .copied()
                    .unwrap_or_default(),
                profile,
                model.adapter(),
                context.catalogue.quota_pool(model),
            )
        {
            let source = if window.profile {
                if key.is_some_and(|key| {
                    context
                        .config
                        .pacing
                        .accounts
                        .contains_key(&format!("{}/{key}", model.adapter()))
                }) {
                    "config:pacing.accounts"
                } else {
                    "config:pacing.free"
                }
            } else if context.config.pacing_reserve.contains_key(model.adapter()) {
                "config:pacing_reserve"
            } else {
                "default:pacing_reserve"
            };
            warnings.push(format!(
                "pacing reserve {}% window={}; ranking penalty; source={source}",
                window.percent, window.pool
            ));
        }
    }
    AccountLoad {
        account_key: availability.account_key.clone(),
        account_source: availability.fixture_ref.clone(),
        headroom_basis_points: limiting.map(|(headroom, _)| headroom),
        reset_eta_seconds: limiting.and_then(|(_, eta)| eta),
        live_lanes: context
            .selection_evidence
            .filter(|e| e.live_lanes_known)
            .and_then(|e| {
                key.map(|key| {
                    e.live_lanes
                        .get(&(model.adapter().into(), key.into()))
                        .copied()
                        .unwrap_or(0)
                })
            }),
        pacing: policy,
        pacing_factor_basis_points: pacing_factor,
        penalty_basis_points: (0..warnings.len()).fold(10_000_u32, |factor, _| (factor / 4).max(1)),
        source: format!(
            "quota={}; live_lanes={}; pacing={pacing_source}",
            quota
                .map(|q| format!("snapshot/revision/{}", q.snapshot.revision))
                .unwrap_or_else(|| "unknown".into()),
            context
                .selection_evidence
                .map(|e| e.source.as_str())
                .unwrap_or("unknown")
        ),
        warnings,
    }
}

fn candidate(
    model: &ModelId,
    effort: Option<&str>,
    context: &ValidatedContext<'_>,
    avoid_cooling: bool,
    intent: RouteIntent,
) -> Result<RouteAlternative, RoutingError> {
    let entry = admissible(model, effort, context)?;
    #[cfg(test)]
    let spec = context
        .launch_specs
        .and_then(|lookup| lookup(model.adapter()))
        .or_else(|| provenant_records::AdapterSpec::get(model.adapter()).copied());
    #[cfg(not(test))]
    let spec = provenant_records::AdapterSpec::get(model.adapter()).copied();
    if let Some(spec) = spec {
        intent
            .check_launch(&spec, model.model(), effort)
            .map_err(|error| RoutingError::UnsupportedLaunch(error.to_string()))?;
    }
    let access = context.access.iter().find(|a| a.adapter == model.adapter());
    // Several registered accounts may serve one adapter: take a ready one with
    // the most pacing headroom, so exhausted accounts are skipped rather than
    // chosen by registration order. A stale reading that showed the account
    // spent cannot refuse it, but it ranks the account behind every other.
    let mut policy_refused = false;
    let evaluated: Vec<_> = context
        .availability
        .iter()
        .filter(|a| {
            a.adapter == model.adapter()
                && provenant_records::AdapterSpec::get(model.adapter()).is_none_or(|spec| {
                    a.account_key
                        .as_deref()
                        .is_none_or(|key| spec.account_serves_model(key, model.model()))
                })
        })
        .filter(|a| {
            let permitted = context
                .config
                .model_policy
                .effective(
                    model,
                    a.account_key.as_deref().or(context.policy_account_hint),
                    context.now,
                )
                .policy
                .permits(intent);
            policy_refused |= !permitted;
            permitted
        })
        .map(|a| {
            let (reason, _) = account_reason(model, a, access, context);
            let (policy, _, _, _) = effective_profile(context, model, a.account_key.as_deref());
            let (_, _, standby) =
                pacing_factor_basis_points(policy, model, a.account_key.as_deref(), context);
            let load = account_load(model, a, context);
            let spent = latest_reading(model, a, context, false)
                .is_some_and(|q| reading_exhausted(&q.snapshot, model, context));
            (
                a,
                reason,
                (
                    !spent,
                    uncertainty(model, a, context).is_none_or(|why| why == "stale quota"),
                    latest_reading(model, a, context, true).is_some(),
                    !avoid_cooling || !cooling(model, a.account_key.as_deref(), context),
                    !standby,
                    load.score(),
                    std::cmp::Reverse(a.account_key.as_deref()),
                ),
            )
        })
        .collect();
    if evaluated.is_empty() && policy_refused {
        return Err(RoutingError::ReadOnlyPolicy);
    }
    let best = evaluated
        .iter()
        .filter(|(_, reason, _)| reason.is_none())
        .max_by_key(|(_, _, headroom)| *headroom)
        .or_else(|| evaluated.first());
    let availability = best.map(|(a, _, _)| *a);
    let reason = match best {
        Some((_, reason, _)) => reason.clone(),
        None => availability_reason(None, context),
    };
    let account = availability.and_then(|a| a.account_key.clone());
    // No account was ready: keep each one's own reason, so a refusal can say
    // which accounts were passed over and why, not only the first one's.
    let skipped: Vec<_> = evaluated
        .iter()
        .filter_map(|(a, reason, _)| {
            Some(provenant_records::SkippedAccount {
                account: a.account_key.clone()?,
                reason: reason.clone()?,
            })
        })
        .collect();
    let readiness = match (reason, availability) {
        (Some(reason), _) => RouteReadiness::Unavailable {
            extra: if skipped.is_empty() {
                Default::default()
            } else {
                [(
                    provenant_records::SKIPPED_ACCOUNTS.into(),
                    serde_json::json!(skipped),
                )]
                .into_iter()
                .collect()
            },
            reason,
        },
        (None, Some(a)) => RouteReadiness::Ready {
            extra: uncertainty(model, a, context)
                .map(|reason| {
                    [("availability_warning".into(), serde_json::json!(reason))]
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            availability_ref: a.fixture_ref.clone(),
            // A stale observation is not renewed; this bounds the lifetime of
            // the routing decision that permits a launch despite uncertainty.
            expires_unix_seconds: if a.expires_unix_seconds <= context.now_seconds() {
                context.now_seconds().saturating_add(60)
            } else {
                a.expires_unix_seconds
            },
        },
        _ => RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: RouteExclusionReason::UnknownAvailability,
        },
    };
    let mut extra = provenant_records::ExtraFields::default();
    if availability.is_none()
        && let Err(warning) = check_go_allowance(model, context.config, context.catalogue)
    {
        extra.insert("allowance_warning".into(), serde_json::json!({"warning":warning,"source":context.config.preference_sources.get("domains.routing.opencode_go.min_monthly_allowance_usd").map(String::as_str).unwrap_or("unresolved")}));
    }
    if let Some(availability) = availability
        && effective_profile(context, model, availability.account_key.as_deref()).0
            == PacingPolicy::Pace
        && let Some(quota) = latest_reading(model, availability, context, true)
    {
        use provenant_records::diagnostic::{
            PacingReserveSetting, PacingSettingSource, ResourceFact,
        };
        let (_, profile, _, _) =
            effective_profile(context, model, availability.account_key.as_deref());
        let reserve = context
            .config
            .pacing_reserve
            .get(model.adapter())
            .copied()
            .unwrap_or_default();
        if let Some(window) = pacing_reserve_window(
            &quota.snapshot,
            context.now_seconds(),
            reserve,
            profile,
            model.adapter(),
            context.catalogue.quota_pool(model),
        ) {
            let account_profile = availability.account_key.as_deref().is_some_and(|key| {
                context
                    .config
                    .pacing
                    .accounts
                    .contains_key(&format!("{}/{key}", model.adapter()))
            });
            let setting = if !window.profile && window.seven_day {
                PacingReserveSetting::AdapterSevenDay
            } else if !window.profile {
                PacingReserveSetting::AdapterFiveHour
            } else if account_profile {
                PacingReserveSetting::Account
            } else {
                PacingReserveSetting::Free
            };
            let origin = if window.profile {
                PacingSettingSource::Configuration
            } else if context.config.pacing_reserve.contains_key(model.adapter()) {
                PacingSettingSource::Unresolved
            } else {
                PacingSettingSource::ShippedDefault
            };
            extra.insert(
                "pacing_reserve_fact".into(),
                serde_json::json!(ResourceFact::PacingReserve {
                    percent: window.percent,
                    setting,
                    origin
                }),
            );
        }
    }
    if let Some(availability) = availability {
        let load = account_load(model, availability, context);
        extra.insert("account_load".into(), serde_json::json!(load));
        extra.insert("account_load_score".into(), load.score().into());
    }
    if evaluated.iter().all(|(_, reason, _)| reason.is_some()) && !evaluated.is_empty() {
        let details: Vec<_> = evaluated
            .iter()
            .filter_map(|(account, reason, _)| {
                reason.as_ref().map(|reason| {
                    format!(
                        "{}: {}; inspect `provenant quota --full`",
                        account.account_key.as_deref().unwrap_or("unknown account"),
                        exclusion_detail(reason)
                    )
                })
            })
            .collect();
        extra.insert("account_exclusions".into(), serde_json::json!(details));
    }
    Ok(RouteAlternative {
        extra,
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
            extra: Default::default(),
            evidence_ref: access.map_or("", |a| a.disclosure_ref).into(),
        },
    })
}
/// Confidential work requires the catalogue privacy guarantee.
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
    resolve_inner(
        request,
        &ValidatedContext::new(context)?,
        Some((operation_id, task_index)),
        Excluded {
            rejected: &[],
            rules: &[],
            required_family: None,
        },
    )
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
        &[],
    )
}

/// Resolve a seeded pool while excluding members rejected by a later dispatch
/// gate, and those a caller, graph or the configuration excluded by `rules`.
/// The operation and task seed stay fixed across each redraw. A pinned model
/// is never affected by `rules`.
pub fn resolve_for_account_with_seed_excluding(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
    operation_id: &str,
    task_index: u64,
    excluded_members: &[ModelId],
    rules: &[ExclusionRule],
) -> Result<ResolvedRoute, RoutingError> {
    resolve_for_account_with_seed_excluding_family(
        request,
        context,
        account,
        operation_id,
        task_index,
        excluded_members,
        rules,
        None,
    )
}

/// Resolve with a hard provider-family requirement. Candidate filtering and
/// readiness are shared with ordinary selection; absence leaves its draw intact.
#[allow(clippy::too_many_arguments)]
pub fn resolve_for_account_with_seed_excluding_family(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
    operation_id: &str,
    task_index: u64,
    excluded_members: &[ModelId],
    rules: &[ExclusionRule],
    required_family: Option<&str>,
) -> Result<ResolvedRoute, RoutingError> {
    if required_family.is_some_and(|family| !crate::request::identifier(family)) {
        return Err(RoutingError::InvalidValue);
    }
    let account = AccountSelection::parse(account)?;
    let context = ValidatedContext::new(context)?;
    // Exclusions shape named-route pools only: a pinned model keeps its own
    // account and fallback whatever is excluded.
    let named = matches!(request.choice, RouteChoice::NamedRoute(_));
    let rules = if named { rules } else { &[] };
    // Withhold excluded accounts, so an adapter's other accounts still serve.
    let availability: Vec<_> = context
        .availability
        .iter()
        .filter(|record| {
            !rules
                .iter()
                .filter_map(ExclusionRule::account)
                .any(|(adapter, key)| {
                    record.adapter == adapter && record.account_key.as_deref() == Some(key)
                })
        })
        .cloned()
        .collect();
    let kept = ValidatedContext(RoutingContext {
        availability: &availability,
        ..context.0
    });
    let context = &kept;
    let excluded_members = Excluded {
        rejected: if named { excluded_members } else { &[] },
        rules,
        required_family,
    };
    if let AccountSelection::Known(account) = account {
        let mut adapters: BTreeSet<&str> = match &request.choice {
            RouteChoice::Model(model) => [model.adapter()].into_iter().collect(),
            RouteChoice::NamedRoute(name) => {
                let name = name.as_str();
                if let Some(pool) = raw_pool(context, name) {
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
        if let Some(model) = context.choice_context.and_then(|(risk, kind)| {
            context
                .config
                .preferred_model(request, risk, kind, context.now)
        }) {
            adapters.insert(model.adapter());
        }
        let availability: Vec<_> = context
            .availability
            .iter()
            .filter(|record| {
                !adapters.contains(record.adapter.as_str())
                    || record.account_key.as_deref() == Some(account)
            })
            .cloned()
            .collect();
        // Filtering a validated snapshot preserves its bounds and uniqueness.
        let filtered = ValidatedContext(RoutingContext {
            availability: &availability,
            ..context.0
        });
        resolve_inner(
            request,
            &filtered,
            Some((operation_id, task_index)),
            excluded_members,
        )
    } else {
        resolve_inner(
            request,
            context,
            Some((operation_id, task_index)),
            excluded_members,
        )
    }
}

pub fn resolve(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_inner(
        request,
        &ValidatedContext::new(context)?,
        None,
        Excluded {
            rejected: &[],
            rules: &[],
            required_family: None,
        },
    )
}

/// Members withheld from one resolution: models a later dispatch gate
/// rejected, and caller or graph exclusions.
#[derive(Clone, Copy)]
struct Excluded<'a> {
    rejected: &'a [ModelId],
    rules: &'a [ExclusionRule],
    required_family: Option<&'a str>,
}
impl Excluded<'_> {
    /// Provider-family constraints apply to exact models as well as pools.
    fn family_reason(&self, model: &ModelId) -> Option<String> {
        self.required_family.and_then(|required| {
            let actual =
                provenant_records::AdapterSpec::get(model.adapter()).map(|spec| spec.family);
            (actual != Some(required)).then(|| {
                format!(
                    "required family {required} count 1; candidate family {}",
                    actual.unwrap_or("unknown")
                )
            })
        })
    }
    /// The reason a named-route member is excluded, if any. The declared
    /// family precedes rules, persistent exclusions and admission rejections.
    /// An account rule removes a model once no remaining account serves it.
    fn reason(&self, model: &ModelId, context: &ValidatedContext<'_>) -> Option<String> {
        if let Some(reason) = self.family_reason(model) {
            return Some(reason);
        }
        let serving_accounts_remain = context.availability.iter().any(|record| {
            record.adapter == model.adapter()
                && provenant_records::AdapterSpec::get(model.adapter()).is_none_or(|spec| {
                    record
                        .account_key
                        .as_deref()
                        .is_none_or(|key| spec.account_serves_model(key, model.model()))
                })
        });
        if let Some(rule) = self
            .rules
            .iter()
            .find(|rule| rule.removes(model, serving_accounts_remain))
        {
            return Some(rule.to_string());
        }
        if context
            .config
            .model_exclusions
            .iter()
            .any(|exclusion| exclusion.model == *model && exclusion.schedule.active(context.now))
        {
            return Some(format!("excluded by config: model {}", model.as_str()));
        }
        self.rejected
            .contains(model)
            .then(|| "admission rejected".to_owned())
    }
}

struct PoolPick {
    route: RouteAlternative,
    receipt: RouteSelectionReceipt,
}

fn resolve_inner(
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: Excluded<'_>,
) -> Result<ResolvedRoute, RoutingError> {
    let (primary, pool_selection) = match &request.choice {
        RouteChoice::Model(model) => {
            if let Some(detail) = excluded_members.family_reason(model) {
                return Err(RoutingError::NoEligiblePoolMember {
                    exclusions: vec![format!("{}: {detail}", model.as_str())],
                });
            }
            if !privacy_allowed(model, request.confidential, context.catalogue) {
                return Err(RoutingError::ConfidentialModel);
            }
            (
                candidate(
                    model,
                    request.effort.as_deref(),
                    context,
                    false,
                    request.intent,
                )?,
                None,
            )
        }
        RouteChoice::NamedRoute(name) => {
            let name = name.as_str();
            let preference = context.choice_context.and_then(|(risk, kind)| {
                context
                    .config
                    .preferred_model(request, risk, kind, context.now)
            });
            let mut pool = effective_pool(context, name);
            if let Some(model) = preference {
                if pool.is_none() {
                    pool = context
                        .config
                        .routes
                        .get(name)
                        .filter(|route| context.catalogue.model(&route.model).is_some())
                        .map(|route| {
                            vec![RoutePoolMember {
                                model: route.model.clone(),
                                weight: 100,
                                effort: route.effort.clone(),
                                efforts: context
                                    .catalogue
                                    .model(&route.model)
                                    .map(|m| m.efforts.clone())
                                    .unwrap_or_default(),
                                free: false,
                            }]
                        });
                }
                if let Some(members) = &mut pool
                    && !members.iter().any(|member| member.model == *model)
                {
                    members.push(RoutePoolMember {
                        model: model.clone(),
                        weight: 0,
                        effort: None,
                        efforts: context
                            .catalogue
                            .model(model)
                            .map(|entry| entry.efforts.clone())
                            .unwrap_or_default(),
                        free: context
                            .catalogue
                            .model(model)
                            .is_some_and(|entry| entry.free),
                    });
                }
            }
            if let Some(pool) = pool {
                let selected = select_pool(name, &pool, request, context, seed, excluded_members)?;
                (selected.route, Some(selected.receipt))
            } else {
                let named = context
                    .config
                    .routes
                    .get(name)
                    .ok_or(RoutingError::UnknownRoute)?;
                if context.catalogue.model(&named.model).is_none() {
                    return Err(RoutingError::Catalogue);
                }
                if !privacy_allowed(&named.model, request.confidential, context.catalogue) {
                    return Err(RoutingError::ConfidentialModel);
                }
                // A single-model route is a route, not a pin: provider, account
                // and model exclusions apply and the refusal names them.
                let single = Excluded {
                    rejected: &[],
                    rules: excluded_members.rules,
                    required_family: excluded_members.required_family,
                };
                if let Some(detail) = single.reason(&named.model, context) {
                    return Err(RoutingError::NoEligiblePoolMember {
                        exclusions: vec![format!("{}: {detail}", named.model.as_str())],
                    });
                }
                (
                    candidate(
                        &named.model,
                        request.effort.as_deref().or(named.effort.as_deref()),
                        context,
                        false,
                        request.intent,
                    )?,
                    None,
                )
            }
        }
    };
    if let Some(family) = excluded_members.required_family
        && matches!(&primary.readiness, RouteReadiness::Unavailable { reason, .. } if reason.is_account_block())
    {
        return Err(RoutingError::NoEligiblePoolMember {
            exclusions: vec![format!(
                "required family {family} count 1; {}/{}: {:?}",
                primary.adapter, primary.model, primary.readiness
            )],
        });
    }
    let models = match &request.fallback {
        Fallback::None => &[][..],
        Fallback::Models(models) => models.as_slice(),
    };
    let primary_model = ModelId::parse(&format!("{}/{}", primary.adapter, primary.model))?;
    validate_intent(
        &context
            .config
            .model_policy
            .effective(
                &primary_model,
                primary
                    .account_key
                    .as_deref()
                    .or(context.policy_account_hint),
                context.now,
            )
            .policy,
        request.intent,
    )?;
    if models.contains(&primary_model) {
        return Err(RoutingError::Alternatives);
    }
    let named_route = matches!(request.choice, RouteChoice::NamedRoute(_));
    let alternatives = models
        .iter()
        .filter(|model| excluded_members.family_reason(model).is_none())
        .filter(|model| privacy_allowed(model, request.confidential, context.catalogue))
        // A named route's fallback keeps to the same exclusions as its pool; a
        // pinned model's own fallback is never filtered.
        .filter(|model| !named_route || excluded_members.reason(model, context).is_none())
        .map(|model| {
            candidate(
                model,
                primary.effort.as_deref(),
                context,
                false,
                request.intent,
            )
        })
        .filter_map(|candidate| match candidate {
            Err(RoutingError::ReadOnlyPolicy | RoutingError::UnsupportedLaunch(_)) => None,
            candidate => Some(candidate),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let exclusions = std::iter::once(&primary)
        .chain(alternatives.iter())
        .filter_map(|candidate| {
            if let RouteReadiness::Unavailable { reason, .. } = &candidate.readiness {
                Some(RouteExclusion {
                    extra: Default::default(),
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
        extra: primary.extra,
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
    zero_ready: Vec<usize>,
    standby_ready: Vec<usize>,
    stale_spent: BTreeSet<usize>,
    first_zero_ready: Option<usize>,
    first_standby_ready: Option<usize>,
    first_recoverable_unavailable: Option<usize>,
    first_other_unavailable: Option<usize>,
    exclusions: Vec<String>,
}
struct PoolDraw {
    exploration: bool,
    index: usize,
    roll: u64,
    total_weight: u64,
}

fn go_quota_observation<'a>(
    model: &ModelId,
    account: Option<&str>,
    context: &'a RoutingContext<'_>,
) -> Option<&'a QuotaObservation> {
    let now = context.now_seconds();
    context
        .quota
        .iter()
        .filter(|q| {
            q.model == *model
                && q.snapshot.account.adapter == model.adapter()
                && Some(q.snapshot.account.account_key.as_str()) == account
                && q.snapshot.observed_unix_seconds <= now
                && now < q.expires_unix_seconds
        })
        .max_by_key(|q| (q.snapshot.observed_unix_seconds, q.snapshot.revision))
}

fn image_input_supported(model: &ModelId, _context: &ValidatedContext<'_>) -> bool {
    #[cfg(test)]
    let spec = _context
        .launch_specs
        .and_then(|lookup| lookup(model.adapter()))
        .or_else(|| provenant_records::AdapterSpec::get(model.adapter()).copied());
    #[cfg(not(test))]
    let spec = provenant_records::AdapterSpec::get(model.adapter()).copied();
    spec.is_some_and(|spec| spec.launch.image_input)
}

fn classify_pool(
    name: &str,
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    excluded_members: Excluded<'_>,
    avoid_cooling: bool,
) -> Result<ClassifiedPool, RoutingError> {
    let mut classified = ClassifiedPool {
        candidates: Vec::with_capacity(members.len()),
        routes: Vec::with_capacity(members.len()),
        weighted: Vec::new(),
        zero_ready: Vec::new(),
        standby_ready: Vec::new(),
        stale_spent: BTreeSet::new(),
        first_zero_ready: None,
        first_standby_ready: None,
        first_recoverable_unavailable: None,
        first_other_unavailable: None,
        exclusions: Vec::new(),
    };
    for member in members {
        let effort = request.effort.clone().or(member.effort.clone());
        let mut factor_basis_points = NEUTRAL_FACTOR_BASIS_POINTS;
        let spent = context
            .availability
            .iter()
            .filter(|a| {
                a.adapter == member.model.adapter()
                    && provenant_records::AdapterSpec::get(member.model.adapter()).is_none_or(
                        |spec| {
                            a.account_key.as_deref().is_none_or(|key| {
                                spec.account_serves_model(key, member.model.model())
                            })
                        },
                    )
            })
            .map(|a| latest_reading(&member.model, a, context, false))
            .collect::<Vec<_>>();
        let is_spent = |q: &QuotaObservation| {
            reading_exhausted(&q.snapshot, &member.model, context)
                || provenant_quota::partial_headroom(
                    &q.snapshot,
                    member.model.adapter(),
                    member.model.model(),
                    &context.catalogue.admission_pools(&member.model),
                    context.now_seconds(),
                ) == provenant_quota::PartialHeadroom::Exhausted
        };
        let all_spent = !spent.is_empty() && spent.iter().all(|q| q.is_some_and(is_spent));
        let exhausted = all_spent
            && spent.iter().all(|q| {
                q.is_some_and(|q| {
                    q.snapshot.observed_unix_seconds < q.expires_unix_seconds
                        && context.now_seconds() < q.expires_unix_seconds
                })
            });
        if all_spent && !exhausted {
            classified.stale_spent.insert(classified.candidates.len());
        }
        let reset = spent
            .iter()
            .filter_map(|q| *q)
            .flat_map(|q| &q.snapshot.windows)
            .filter(|w| {
                window_serves(
                    member.model.adapter(),
                    &w.pool,
                    context.catalogue.quota_pool(&member.model),
                )
            })
            .filter(|w| w.used_basis_points >= 10_000)
            .filter_map(|w| w.resets_unix_seconds)
            .filter(|reset| *reset > context.now_seconds())
            .min();
        let spent_detail = exhausted.then(|| match reset {
            Some(reset) => format!("exhausted; earliest known reset {reset}"),
            None => "exhausted; reset unknown".to_owned(),
        });
        let exclusion_detail = match (
            excluded_members.reason(&member.model, context),
            spent_detail,
        ) {
            (Some(rule), Some(quota)) => Some(format!("{rule}; {quota}")),
            (rule, quota) => rule.or(quota),
        };
        if let Some(detail) = exclusion_detail {
            classified
                .exclusions
                .push(format!("{}: {detail}", member.model.as_str()));
            classified.candidates.push(RouteSelectionCandidate {
                extra: Default::default(),
                model: member.model.as_str().into(),
                weight: member.weight,
                factor_basis_points,
                pacing_detail: None,
                effort,
                readiness: None,
                exclusion: Some(detail),
            });
            classified.routes.push(None);
            continue;
        }
        let exclusion = if !privacy_allowed(&member.model, request.confidential, context.catalogue)
        {
            Some(("confidential", "confidential model"))
        } else if effort
            .as_deref()
            .is_some_and(|value| !member.efforts.iter().any(|allowed| allowed == value))
        {
            Some(("effort", "unsupported effort"))
        } else {
            None
        };
        let mut pacing_detail = None;
        let mut readiness = None;
        let mut option = None;
        let mut ranking = provenant_records::ExtraFields::default();
        let exclusion = if let Some((kind, detail)) = exclusion {
            classified.exclusions.push(detail.to_owned());
            Some(kind.to_owned())
        } else {
            match candidate(
                &member.model,
                effort.as_deref(),
                context,
                avoid_cooling,
                request.intent,
            ) {
                Ok(resolved) => {
                    if let Some(family) = excluded_members.required_family
                        && matches!(&resolved.readiness, RouteReadiness::Unavailable { reason, .. } if reason.is_account_block())
                    {
                        let detail = format!(
                            "required family {family} count 1; unavailable: {:?}",
                            resolved.readiness
                        );
                        classified
                            .exclusions
                            .push(format!("{}: {detail}", member.model.as_str()));
                        classified.candidates.push(RouteSelectionCandidate {
                            extra: Default::default(),
                            model: member.model.as_str().into(),
                            weight: member.weight,
                            factor_basis_points,
                            pacing_detail: None,
                            effort,
                            readiness: Some(resolved.readiness.clone()),
                            exclusion: Some(detail),
                        });
                        classified.routes.push(None);
                        continue;
                    }
                    let model_policy = context
                        .config
                        .model_policy
                        .effective(
                            &member.model,
                            resolved
                                .account_key
                                .as_deref()
                                .or(context.policy_account_hint),
                            context.now,
                        )
                        .policy;
                    if !model_policy.permits(request.intent) {
                        classified
                            .exclusions
                            .push(format!("{}: read-only model policy", member.model.as_str()));
                        classified.candidates.push(RouteSelectionCandidate {
                            extra: Default::default(),
                            model: member.model.as_str().into(),
                            weight: member.weight,
                            factor_basis_points,
                            pacing_detail: None,
                            effort,
                            readiness: Some(resolved.readiness.clone()),
                            exclusion: Some("read-only model policy".into()),
                        });
                        classified.routes.push(None);
                        continue;
                    }
                    let (policy, _, _, _) =
                        effective_profile(context, &member.model, resolved.account_key.as_deref());
                    let (factor, detail, is_standby) = pacing_factor_basis_points(
                        policy,
                        &member.model,
                        resolved.account_key.as_deref(),
                        context,
                    );
                    let cache_factor = model_policy.cache_factor_basis_points(request.intent);
                    let capability = context
                        .catalogue
                        .model(&member.model)
                        .map(|m| m.capability)
                        .unwrap_or_default();
                    let role = context
                        .selection_evidence
                        .map(|e| e.role(request.intent.workload, request.intent.writes))
                        .unwrap_or(if request.intent.workload == Workload::Review {
                            "review"
                        } else if request.intent.writes {
                            "writer"
                        } else {
                            "task"
                        });
                    let selector =
                        if role == "review" || request.intent.workload == Workload::Review {
                            "review"
                        } else {
                            name
                        };
                    let (preferences, source) = context
                        .config
                        .capability_preferences
                        .get(selector)
                        .map(|weights| (Some(weights), "config"))
                        .unwrap_or((
                            context.catalogue.capability_preferences.get(selector),
                            "catalogue",
                        ));
                    let capability_weight = preferences
                        .and_then(|weights| weights.get(&capability))
                        .copied()
                        .unwrap_or(1)
                        .max(1);
                    let prior = context
                        .config
                        .outcome_priors
                        .enabled
                        .then(|| {
                            context.selection_evidence.and_then(|e| {
                                e.priors.iter().find(|prior| {
                                    prior.model == member.model
                                        && prior.pool == name
                                        && prior.role == role
                                })
                            })
                        })
                        .flatten();
                    let prior_factor = prior.map_or(10_000, RoutePrior::factor_basis_points);
                    let load = context
                        .availability
                        .iter()
                        .find(|a| {
                            a.adapter == member.model.adapter()
                                && a.account_key == resolved.account_key
                        })
                        .map(|a| account_load(&member.model, a, context));
                    let load_factor = load.as_ref().map_or(u64::from(factor), |load| load.score());
                    factor_basis_points =
                        ((u64::from(factor) * u64::from(cache_factor)) / 10_000) as u32;
                    let ranking_factor_basis_points = (u128::from(load_factor)
                        * u128::from(cache_factor)
                        * u128::from(capability_weight)
                        * u128::from(prior_factor)
                        / 100_000_000)
                        .clamp(1, u128::from(u32::MAX))
                        as u32;
                    ranking.insert(
                        "ranking_factor_basis_points".into(),
                        ranking_factor_basis_points.into(),
                    );
                    ranking.insert("capability_preference".into(), serde_json::json!({"capability":capability,"weight":capability_weight,"selector":selector,"source":source}));
                    ranking.insert("outcome_prior".into(), serde_json::json!({"enabled":context.config.outcome_priors.enabled,"role":role,"pool":name,"factor_basis_points":prior_factor,"evidence":prior,"source":if !context.config.outcome_priors.enabled {"config:disabled"} else if prior.is_some() {"route_outcomes"} else {"no_evidence"}}));
                    if let Some(load) = load {
                        ranking.insert("account_load".into(), serde_json::json!(load));
                    }
                    if avoid_cooling
                        && cooling(&member.model, resolved.account_key.as_deref(), context)
                    {
                        classified.candidates.push(RouteSelectionCandidate {
                            extra: Default::default(),
                            model: member.model.as_str().into(),
                            weight: member.weight,
                            factor_basis_points,
                            pacing_detail: Some(detail),
                            effort,
                            readiness: Some(resolved.readiness.clone()),
                            exclusion: Some("capacity cooldown".into()),
                        });
                        classified.routes.push(None);
                        continue;
                    }
                    pacing_detail = Some(if cache_factor != 10_000 {
                        format!(
                            "{detail}; cheap-cache read-heavy preference factor={cache_factor}/10000 (read ratio {:?}, context {:?})",
                            model_policy.cache.read_ratio, model_policy.cache.context_window
                        )
                    } else {
                        detail
                    });
                    readiness = Some(resolved.readiness.clone());
                    if matches!(resolved.readiness, RouteReadiness::Ready { .. }) {
                        if is_standby {
                            classified.standby_ready.push(classified.candidates.len());
                            if classified.first_standby_ready.is_none() {
                                classified.first_standby_ready = Some(classified.candidates.len());
                            }
                        } else if member.weight == 0 {
                            classified.zero_ready.push(classified.candidates.len());
                            if classified.first_zero_ready.is_none() {
                                classified.first_zero_ready = Some(classified.candidates.len());
                            }
                        } else {
                            let weight = u64::from(member.weight)
                                .saturating_mul(u64::from(ranking_factor_basis_points));
                            classified
                                .weighted
                                .push((classified.candidates.len(), weight));
                        }
                    } else if let RouteReadiness::Unavailable { reason, .. } = &resolved.readiness {
                        match reason {
                            RouteExclusionReason::Disabled => classified
                                .exclusions
                                .push(format!("{}: disabled", member.model.as_str())),
                            // Refusals expire and so are recoverable; a mismatch or
                            // invalid evidence needs repair, so it ranks with the
                            // other unavailable members. The CLI's relaxation set
                            // (`is_account_block`) is a different question: whether
                            // the chosen route can be used at all.
                            RouteExclusionReason::StaleAvailability
                            | RouteExclusionReason::AuthRequired
                            | RouteExclusionReason::LoginExpired
                            | RouteExclusionReason::Exhausted
                            | RouteExclusionReason::FreeTierRefused
                            | RouteExclusionReason::ProviderRefused
                            | RouteExclusionReason::PacingReserve => {
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
                Err(RoutingError::ReadOnlyPolicy) => {
                    classified
                        .exclusions
                        .push(format!("{}: read-only model policy", member.model.as_str()));
                    Some("read-only model policy".into())
                }
                Err(RoutingError::UnsupportedLaunch(detail)) => {
                    classified
                        .exclusions
                        .push(format!("{}: {detail}", member.model.as_str()));
                    Some(detail)
                }
                Err(RoutingError::UnsupportedEffort { .. }) => {
                    classified.exclusions.push("unsupported effort".into());
                    Some("effort".into())
                }
                Err(error) => return Err(error),
            }
        };
        classified.candidates.push(RouteSelectionCandidate {
            extra: ranking,
            model: member.model.as_str().into(),
            weight: member.weight,
            factor_basis_points,
            pacing_detail,
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
    settings: &OutcomePriorSettings,
) -> Result<PoolDraw, RoutingError> {
    let (operation, task) = seed.unwrap_or(("route-preview", 0));
    if settings.enabled
        && !classified.weighted.is_empty()
        && stable_roll(operation, task, &format!("{name}/exploration"), 10_000)
            < u64::from(settings.exploration_percent) * 100
    {
        let roll = stable_roll(
            operation,
            task,
            &format!("{name}/exploration/member"),
            classified.weighted.len() as u64,
        );
        return Ok(PoolDraw {
            exploration: true,
            index: classified.weighted[roll as usize].0,
            roll,
            total_weight: classified.weighted.len() as u64,
        });
    }
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
            .or(classified.first_standby_ready)
            .or(classified.first_recoverable_unavailable)
            .or(classified.first_other_unavailable)
    };
    let index = selected_index.ok_or_else(|| RoutingError::NoEligiblePoolMember {
        exclusions: classified.exclusions.clone(),
    })?;
    Ok(PoolDraw {
        exploration: false,
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
        extra: Default::default(),
        pool: name.into(),
        candidates: classified.candidates.clone(),
        roll: draw.roll,
        total_weight: draw.total_weight,
        pick: format!("{}/{}", route.adapter, route.model),
        reason: None,
        redraws: Vec::new(),
    }
}

fn classify_with_image_preference(
    name: &str,
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    excluded: Excluded<'_>,
    avoid_cooling: bool,
) -> Result<ClassifiedPool, RoutingError> {
    let mut classified = classify_pool(name, members, request, context, excluded, avoid_cooling)?;
    let image = |index: usize| image_input_supported(&members[index].model, context);
    let has_ready_image = members
        .iter()
        .zip(&classified.routes)
        .any(|(member, route)| {
            image_input_supported(&member.model, context)
                && route
                    .as_ref()
                    .is_some_and(|route| matches!(route.readiness, RouteReadiness::Ready { .. }))
        });
    if request.intent.image_input && has_ready_image {
        classified.weighted.retain(|(index, _)| image(*index));
        classified.zero_ready.retain(|index| image(*index));
        classified.standby_ready.retain(|index| image(*index));
        classified.first_zero_ready = classified.zero_ready.first().copied();
        classified.first_standby_ready = classified.standby_ready.first().copied();
    }
    // Stale exhaustion is a preference, never a launch refusal. Retain the
    // original draw when every ready member has only stale spent evidence.
    let ready = classified
        .weighted
        .iter()
        .map(|(index, _)| *index)
        .chain(classified.zero_ready.iter().copied())
        .chain(classified.standby_ready.iter().copied());
    if ready
        .into_iter()
        .any(|index| !classified.stale_spent.contains(&index))
    {
        classified
            .weighted
            .retain(|(index, _)| !classified.stale_spent.contains(index));
        classified
            .zero_ready
            .retain(|index| !classified.stale_spent.contains(index));
        classified
            .standby_ready
            .retain(|index| !classified.stale_spent.contains(index));
        classified.first_zero_ready = classified.zero_ready.first().copied();
        classified.first_standby_ready = classified.standby_ready.first().copied();
    }
    Ok(classified)
}

fn select_pool(
    name: &str,
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: Excluded<'_>,
) -> Result<PoolPick, RoutingError> {
    let mut classified =
        classify_with_image_preference(name, members, request, context, excluded_members, true)?;
    let (draw, reason) = match draw_pool(name, &classified, seed, &context.config.outcome_priors) {
        Ok(draw)
            if !matches!(
                classified.routes[draw.index]
                    .as_ref()
                    .map(|route| &route.readiness),
                Some(RouteReadiness::Ready { .. })
            ) && classified.candidates.iter().any(|candidate| {
                candidate.exclusion.as_deref() == Some("capacity cooldown")
                    && matches!(candidate.readiness, Some(RouteReadiness::Ready { .. }))
            }) =>
        {
            let normal = classify_with_image_preference(
                name,
                members,
                request,
                context,
                excluded_members,
                false,
            )?;
            let normal_draw = draw_pool(name, &normal, seed, &context.config.outcome_priors)?;
            if matches!(
                normal.routes[normal_draw.index]
                    .as_ref()
                    .map(|route| &route.readiness),
                Some(RouteReadiness::Ready { .. })
            ) {
                classified = normal;
                (
                    normal_draw,
                    Some("cooling ready candidate selected over unavailable candidate"),
                )
            } else {
                (draw, None)
            }
        }
        Ok(draw) => (draw, None),
        Err(error)
            if classified
                .candidates
                .iter()
                .any(|candidate| candidate.exclusion.as_deref() == Some("capacity cooldown")) =>
        {
            classified = classify_with_image_preference(
                name,
                members,
                request,
                context,
                excluded_members,
                false,
            )?;
            let draw = draw_pool(name, &classified, seed, &context.config.outcome_priors)
                .map_err(|_| error)?;
            (
                draw,
                Some("all eligible candidates cooling, normal selection"),
            )
        }
        Err(error) => return Err(error),
    };
    let preference = context.choice_context.and_then(|(risk, kind)| {
        context
            .config
            .preferred_model(request, risk, kind, context.now)
    });
    let preferred_index = preference.and_then(|model| {
        classified
            .weighted
            .iter()
            .map(|(index, _)| *index)
            .chain(classified.zero_ready.iter().copied())
            .find(|index| members[*index].model == *model)
    });
    let draw = if let Some(index) = preferred_index {
        PoolDraw {
            exploration: false,
            index,
            roll: 0,
            total_weight: 0,
        }
    } else {
        draw
    };
    let route = classified
        .routes
        .get(draw.index)
        .and_then(Option::as_ref)
        .cloned()
        .ok_or(RoutingError::Catalogue)?;
    let mut receipt = selection_receipt(name, &classified, &draw, &route);
    receipt.extra.insert(
        "weight_source".into(),
        if context.config.pools.contains_key(name) {
            "config"
        } else {
            "catalogue"
        }
        .into(),
    );
    receipt.extra.insert("exploration".into(), serde_json::json!({"selected":draw.exploration,"percent":context.config.outcome_priors.exploration_percent,"enabled":context.config.outcome_priors.enabled,"source":context.config.preference_sources.get("domains.routing.outcome_priors.exploration_percent").map(String::as_str).unwrap_or("default")}));
    if let Some(load) = route.extra.get("account_load") {
        receipt.extra.insert("account_load".into(), load.clone());
    }
    let rule = if preferred_index.is_some() {
        "model_choice_override"
    } else if draw.exploration {
        "exploration_draw"
    } else if draw.total_weight > 0 {
        "weighted_draw"
    } else if classified.first_zero_ready == Some(draw.index) {
        "first_zero_weight_ready"
    } else if classified.first_standby_ready == Some(draw.index) {
        "first_standby_ready"
    } else if classified.first_recoverable_unavailable == Some(draw.index) {
        "first_recoverable_unavailable"
    } else {
        "first_unavailable"
    };
    receipt.extra.insert("selection_rule".into(), rule.into());
    if !excluded_members.rules.is_empty() {
        receipt.extra.insert(
            "considered_exclusions".into(),
            excluded_members
                .rules
                .iter()
                .map(|rule| format!("source={} selector={}", rule.source.as_str(), rule.selector))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    receipt.reason = reason.map(str::to_owned);
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

/// Readiness of every pool model, and of `also`, on each registered account of
/// its provider, for [`next_after_depletion`]. Each pair is judged by the same
/// checks as a dispatch, with that one account as the only choice, so exhausted,
/// unauthenticated, stale or unobserved accounts are not ready. A pair the
/// dispatch checks would refuse outright, such as an unsupported launch, has no
/// fact and so is unknown. Availability older than one hour is unknown here.
pub fn depletion_facts(
    context: &RoutingContext<'_>,
    intent: RouteIntent,
    also: &[ModelId],
) -> Result<Vec<ReadinessFact>, RoutingError> {
    let context = ValidatedContext::new(context)?;
    let mut models: Vec<ModelId> = effective_pool_names(&context)
        .iter()
        .filter_map(|name| raw_pool(&context, name))
        .flatten()
        .map(|member| member.model.clone())
        .chain(also.iter().cloned())
        .collect();
    models.sort();
    models.dedup();
    let now = context.now_seconds();
    let mut facts = Vec::new();
    for model in &models {
        for record in context.availability.iter().filter(|record| {
            record.adapter == model.adapter()
                && record.observed_unix_seconds <= now
                && now.saturating_sub(record.observed_unix_seconds) <= 3600
        }) {
            let one = std::slice::from_ref(record);
            let scoped = ValidatedContext(RoutingContext {
                availability: one,
                ..context.0
            });
            let Ok(route) = candidate(model, None, &scoped, false, intent) else {
                continue;
            };
            let state = match route.readiness {
                RouteReadiness::Ready { .. } => Readiness::Ready,
                RouteReadiness::Unavailable { .. } => {
                    match latest_reading(model, record, &scoped, true)
                        .filter(|reading| reading_exhausted(&reading.snapshot, model, &scoped))
                    {
                        Some(reading) => Readiness::Depleted {
                            resets_unix_seconds: spent_windows_reset(
                                &reading.snapshot,
                                catalogue_pool(&scoped, model),
                                now,
                            ),
                        },
                        None => Readiness::Unavailable,
                    }
                }
            };
            facts.push(ReadinessFact {
                model: model.clone(),
                account: record.account_key.clone(),
                state,
            });
        }
    }
    Ok(facts)
}
fn catalogue_pool<'a>(context: &'a RoutingContext<'_>, model: &ModelId) -> Option<&'a str> {
    context.catalogue.quota_pool(model)
}
/// When every full window serving the model has reset; `None` if any has no
/// reset time or none is full.
fn spent_windows_reset(
    snapshot: &provenant_quota::QuotaSnapshot,
    model_pool: Option<&str>,
    now: i64,
) -> Option<i64> {
    let mut latest = None;
    for window in snapshot
        .windows
        .iter()
        .filter(|w| window_serves(&snapshot.account.adapter, &w.pool, model_pool))
        .filter(|w| w.used_basis_points >= 10_000)
    {
        let reset = window.resets_unix_seconds.filter(|reset| *reset > now)?;
        latest = latest.max(Some(reset));
    }
    latest
}
