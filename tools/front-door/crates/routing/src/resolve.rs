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
    /// Preview-only account policy selection. It never supplies availability.
    pub policy_account_hint: Option<&'a str>,
    pub catalogue: &'a Catalogue,
    pub config: &'a RoutingConfig,
    pub registry: &'a [Registration],
    pub availability: &'a [AvailabilityRecord],
    pub quota: &'a [QuotaObservation],
    pub capacity_cooldowns: &'a [provenant_quota::CapacityCooldown],
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
    pub expires_unix_seconds: Option<i64>,
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
            if check_go_allowance(&entry.model, context.config, context.catalogue).is_err() {
                continue;
            }
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
        active_overrides: context
            .config
            .pacing_overrides
            .iter()
            .filter(|o| o.expires_unix_seconds > context.now_seconds())
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
                    expires_unix_seconds: expiry,
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
/// Providers named by the primary route or pool and its explicit fallbacks.
/// Discovery must not run commands for unrelated installed providers.
pub fn probe_adapters(
    request: &RouteRequest,
    catalogue: &Catalogue,
    config: &RoutingConfig,
) -> Result<Vec<&'static provenant_records::AdapterSpec>, RoutingError> {
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
    Ok(provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| adapters.contains(spec.name))
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
    Option<i64>,
) {
    // Direct library callers may supply legacy bare keys. These match any
    // adapter; callers with an account registry should qualify them first.
    let account_profile = account.and_then(|a| {
        context
            .config
            .pacing
            .accounts
            .get(&format!("{}/{a}", model.adapter()))
            .or_else(|| context.config.pacing.accounts.get(a))
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
        o.expires_unix_seconds > context.now_seconds()
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
        return (o.policy, profile, "override", Some(o.expires_unix_seconds));
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
            adapter == "opencode" && (name.ends_with("five_hour") || name.ends_with("rolling"))
        }
    }
}
fn selected_window(
    window: &provenant_quota::QuotaWindow,
    profile: Option<&PacingProfile>,
    adapter: &str,
) -> bool {
    profile
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
            // Bare keys are supported for direct library callers without a
            // registry and can match any adapter. Qualify when one is available.
            deadline.reset_of.split_once('/').map_or_else(
                || o.snapshot.account.account_key == deadline.reset_of,
                |(adapter, key)| {
                    o.snapshot.account.adapter == adapter && o.snapshot.account.account_key == key
                },
            ) && o.snapshot.schema_version == 1
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
    context: &RoutingContext<'_>,
) -> Option<i64> {
    let observation = reference_observation(deadline, context)
        .filter(|o| reference_status_usable(o.snapshot.status))?;
    observation
        .snapshot
        .windows
        .iter()
        .filter(|w| window_matches(w, deadline.window, &observation.snapshot.account.adapter))
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
    context: &RoutingContext<'_>,
) -> Option<(u32, String, bool)> {
    snapshot
        .windows
        .iter()
        .filter(|w| selected_window(w, profile, adapter))
        .filter_map(|window| {
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
                None if adapter == "opencode"
                    && snapshot
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
                    reference_deadline(reference, Some(window), context).map(|d| (d, None))
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
    let detail = if model.adapter() == "opencode" && model.model().starts_with("opencode-go/") {
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
            } else {
                format!(
                    "{detail}; Go quota source: local-ledger, coverage: partial; Go calibration missing or stale: {} (calibration key must match account key {:?}); calibration would tighten pacing",
                    missing.join(", "),
                    account.unwrap_or("unknown")
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
            )
        })
        .map(|window| format!(" reserve exclusion window={window}"))
        .unwrap_or_default();
    let detail = format!("{detail}{reserve_detail}");
    let neutral_detail = match profile.and_then(|p| p.deadline.as_ref()) {
        Some(PacingDeadline::ResetOf(reference))
            if reference_deadline(reference, None, context).is_none() =>
        {
            format!(
                "{detail} {}; falling back to window reset; unobserved windows neutral",
                fallback_reason(reference, context)
            )
        }
        _ => format!("{detail} unobserved windows neutral"),
    };
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

fn pacing_reserve_window(
    snapshot: &provenant_quota::QuotaSnapshot,
    now: i64,
    reserve: PacingReserve,
    profile: Option<&PacingProfile>,
    adapter: &str,
) -> Option<String> {
    snapshot
        .windows
        .iter()
        .filter(|w| selected_window(w, profile, adapter))
        .find(|window| {
            let kind = [
                PacingWindow::Rolling,
                PacingWindow::FiveHour,
                PacingWindow::SevenDay,
                PacingWindow::Daily,
                PacingWindow::Monthly,
            ]
            .into_iter()
            .find(|k| window_matches(window, *k, adapter));
            let percent = profile
                .and_then(|p| {
                    p.reserve
                        .iter()
                        .find(|(k, _)| window_matches(window, **k, adapter))
                        .map(|(_, value)| *value)
                })
                .unwrap_or(match kind {
                    Some(PacingWindow::FiveHour | PacingWindow::Rolling) => {
                        reserve.five_hour_percent
                    }
                    Some(PacingWindow::SevenDay) => reserve.seven_day_percent,
                    _ => 0,
                });
            percent > 0
                && (window.resets_unix_seconds.is_some_and(|reset| reset > now)
                    || (window.resets_unix_seconds.is_none()
                        && adapter == "opencode"
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
        .map(|w| w.pool.clone())
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
    let mut reason = availability_reason(Some(availability), context);
    if reason.is_none() && access.is_none_or(|a| a.disclosure_ref.is_empty()) {
        reason = Some(RouteExclusionReason::DisclosureDenied);
    }
    if reason.is_none() && access.is_none_or(|a| a.confinement_ref.is_empty()) {
        reason = Some(RouteExclusionReason::ConfinementUnavailable);
    }
    // Explicit exact IDs use the executable/account preflight too. The provider
    // owns model availability; named routes and pools require catalogue entries.
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
    let (policy, profile, _, _) =
        effective_profile(context, model, availability.account_key.as_deref());
    if reason.is_none() && policy == PacingPolicy::Off {
        reason = Some(RouteExclusionReason::Disabled);
    }
    if reason.is_none()
        && policy == PacingPolicy::Pace
        && let Some(q) = quota
        && matches!(
            q.snapshot.status,
            QuotaStatus::Available | QuotaStatus::CreditOnly | QuotaStatus::Exhausted
        )
        && pacing_reserve_window(
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
        )
        .is_some()
    {
        reason = Some(RouteExclusionReason::PacingReserve);
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
    if let Some(effort) = effort.filter(|e| {
        !crate::request::effort_valid(e)
            || entry.is_none_or(|row| !row.efforts.iter().any(|v| v == e))
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

fn candidate(
    model: &ModelId,
    effort: Option<&str>,
    context: &ValidatedContext<'_>,
    avoid_cooling: bool,
    intent: RouteIntent,
) -> Result<RouteAlternative, RoutingError> {
    let entry = admissible(model, effort, context)?;
    let access = context.access.iter().find(|a| a.adapter == model.adapter());
    // Several registered accounts may serve one adapter: take a ready one with
    // the most pacing headroom, so exhausted accounts are skipped rather than
    // chosen by registration order.
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
                    context.now_seconds(),
                )
                .policy
                .permits(intent);
            policy_refused |= !permitted;
            permitted
        })
        .map(|a| {
            let (reason, _) = account_reason(model, a, access, context);
            let (policy, _, _, _) = effective_profile(context, model, a.account_key.as_deref());
            let (headroom, _, standby) =
                pacing_factor_basis_points(policy, model, a.account_key.as_deref(), context);
            (
                a,
                reason,
                (
                    !avoid_cooling || !cooling(model, a.account_key.as_deref(), context),
                    !standby,
                    headroom,
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
    resolve_inner(
        request,
        &ValidatedContext::new(context)?,
        Some((operation_id, task_index)),
        &[],
    )
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
    let account = AccountSelection::parse(account)?;
    let context = &ValidatedContext::new(context)?;
    if let AccountSelection::Known(account) = account {
        let adapters: BTreeSet<&str> = match &request.choice {
            RouteChoice::Model(model) => [model.adapter()].into_iter().collect(),
            RouteChoice::NamedRoute(name) => {
                let name = name.as_str();
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
    resolve_inner(request, &ValidatedContext::new(context)?, None, &[])
}

struct PoolPick {
    route: RouteAlternative,
    receipt: RouteSelectionReceipt,
}

fn resolve_inner(
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: &[ModelId],
) -> Result<ResolvedRoute, RoutingError> {
    let (primary, pool_selection) = match &request.choice {
        RouteChoice::Model(model) => {
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
            if let Some(pool) = effective_pool(context, name) {
                let selected = select_pool(name, pool, request, context, seed, excluded_members)?;
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
                context.now_seconds(),
            )
            .policy,
        request.intent,
    )?;
    if models.contains(&primary_model) {
        return Err(RoutingError::Alternatives);
    }
    let alternatives = models
        .iter()
        .filter(|model| privacy_allowed(model, request.confidential, context.catalogue))
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
            Err(RoutingError::ReadOnlyPolicy) => None,
            candidate => Some(candidate),
        })
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
    first_standby_ready: Option<usize>,
    first_recoverable_unavailable: Option<usize>,
    first_other_unavailable: Option<usize>,
    exclusions: Vec<String>,
}
struct PoolDraw {
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

fn classify_pool(
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    excluded_members: &[ModelId],
    avoid_cooling: bool,
) -> Result<ClassifiedPool, RoutingError> {
    let mut classified = ClassifiedPool {
        candidates: Vec::with_capacity(members.len()),
        routes: Vec::with_capacity(members.len()),
        weighted: Vec::new(),
        first_zero_ready: None,
        first_standby_ready: None,
        first_recoverable_unavailable: None,
        first_other_unavailable: None,
        exclusions: Vec::new(),
    };
    for member in members {
        let effort = request.effort.clone().or(member.effort.clone());
        let mut factor_basis_points = NEUTRAL_FACTOR_BASIS_POINTS;
        if excluded_members.contains(&member.model) {
            classified
                .exclusions
                .push(format!("{}: admission rejected", member.model.as_str()));
            classified.candidates.push(RouteSelectionCandidate {
                model: member.model.as_str().into(),
                weight: member.weight,
                factor_basis_points,
                pacing_detail: None,
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
        let mut pacing_detail = None;
        let mut readiness = None;
        let mut option = None;
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
                    let model_policy = context
                        .config
                        .model_policy
                        .effective(
                            &member.model,
                            resolved
                                .account_key
                                .as_deref()
                                .or(context.policy_account_hint),
                            context.now_seconds(),
                        )
                        .policy;
                    if !model_policy.permits(request.intent) {
                        classified
                            .exclusions
                            .push(format!("{}: read-only model policy", member.model.as_str()));
                        classified.candidates.push(RouteSelectionCandidate {
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
                    factor_basis_points =
                        ((u64::from(factor) * u64::from(cache_factor)) / 10_000) as u32;
                    if avoid_cooling
                        && cooling(&member.model, resolved.account_key.as_deref(), context)
                    {
                        classified.candidates.push(RouteSelectionCandidate {
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
                            if classified.first_standby_ready.is_none() {
                                classified.first_standby_ready = Some(classified.candidates.len());
                            }
                        } else if member.weight == 0 {
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
                            | RouteExclusionReason::Exhausted
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
                Err(RoutingError::UnsupportedEffort { .. }) => {
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
            .or(classified.first_standby_ready)
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
        reason: None,
        redraws: Vec::new(),
    }
}

fn select_pool(
    name: &str,
    members: &[RoutePoolMember],
    request: &RouteRequest,
    context: &ValidatedContext<'_>,
    seed: Option<(&str, u64)>,
    excluded_members: &[ModelId],
) -> Result<PoolPick, RoutingError> {
    let mut classified = classify_pool(members, request, context, excluded_members, true)?;
    let (draw, reason) = match draw_pool(name, &classified, seed) {
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
            let normal = classify_pool(members, request, context, excluded_members, false)?;
            let normal_draw = draw_pool(name, &normal, seed)?;
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
            classified = classify_pool(members, request, context, excluded_members, false)?;
            let draw = draw_pool(name, &classified, seed).map_err(|_| error)?;
            (
                draw,
                Some("all eligible candidates cooling, normal selection"),
            )
        }
        Err(error) => return Err(error),
    };
    let route = classified
        .routes
        .get(draw.index)
        .and_then(Option::as_ref)
        .cloned()
        .ok_or(RoutingError::Catalogue)?;
    let mut receipt = selection_receipt(name, &classified, &draw, &route);
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
