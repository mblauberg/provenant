use super::*;
use provenant_config::{DomainConfig, DomainSections};
use provenant_providers::{AvailabilityRecord, REGISTRY};
use provenant_records::{ResolvedRoute, RouteEligibility, RouteExclusionReason, RouteReadiness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn resolve_for_account(
    request: &RouteRequest,
    context: &RoutingContext<'_>,
    account: Option<&str>,
) -> Result<ResolvedRoute, RoutingError> {
    resolve_for_account_with_seed(request, context, account, "route-preview", 0)
}

fn now() -> Result<provenant_core::WallTime, Box<dyn std::error::Error>> {
    Ok(provenant_core::WallTime::from_micros(100_000_000)?)
}
fn context<'a>(
    catalogue: &'a Catalogue,
    config: &'a RoutingConfig,
    now: provenant_core::WallTime,
) -> RoutingContext<'a> {
    RoutingContext {
        policy_account_hint: None,
        choice_context: None,
        catalogue,
        config,
        registry: REGISTRY,
        availability: &[],
        quota: &[],
        capacity_cooldowns: &[],
        accounts: &[],
        quota_policies: &[],
        launch_specs: None,
        readiness: None,
        access: &[],
        now,
        host: "test-host",
    }
}
fn request(model: &str) -> Result<RouteRequest, RoutingError> {
    Ok(RouteRequest {
        intent: Default::default(),
        choice: RouteChoice::Model(ModelId::parse(model)?),
        effort: None,
        confidential: false,
        fallback: Fallback::None,
    })
}

/// Zen serves the free OpenCode routes and Go the paid flash routes.
fn opencode_accounts() -> [AvailabilityRecord; 2] {
    ["zen", "go"].map(|key| AvailabilityRecord {
        adapter: "opencode".into(),
        account_key: Some(key.into()),
        ..availability()
    })
}
fn opencode_access() -> [AccessContext<'static>; 1] {
    [AccessContext {
        adapter: "opencode",
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }]
}
/// The junior pool resolved under a range of seeds, so every member gets drawn.
fn junior_draws(
    query: &RouteRequest,
    ctx: &RoutingContext<'_>,
) -> Result<Vec<ResolvedRoute>, Box<dyn std::error::Error>> {
    let mut routes = Vec::new();
    for task_index in 0..16 {
        routes.push(resolve_with_seed(query, ctx, "junior-free", task_index)?);
    }
    Ok(routes)
}
fn picked_model(route: &ResolvedRoute) -> Result<ModelId, RoutingError> {
    ModelId::parse(&format!("{}/{}", route.adapter, route.model))
}

fn availability() -> AvailabilityRecord {
    AvailabilityRecord {
        schema_version: 1,
        adapter: "codex".into(),
        executable: "/fixture/codex".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "test".into(),
        catalogue_revision: Catalogue::bundled().unwrap().revision,
        account_key: Some("opaque-account".into()),
        observed_models: vec!["test".into()],
        observed_unix_seconds: 80,
        expires_unix_seconds: 120,
    }
}

fn reason(route: &ResolvedRoute) -> Option<&RouteExclusionReason> {
    match &route.readiness {
        RouteReadiness::Unavailable { reason, .. } => Some(reason),
        _ => None,
    }
}
fn quota(
    model: &str,
    account: &str,
    status: provenant_quota::QuotaStatus,
    revision: u64,
    observed: i64,
    expires: i64,
) -> Result<QuotaObservation, RoutingError> {
    Ok(QuotaObservation {
        model: ModelId::parse(model)?,
        expires_unix_seconds: expires,
        snapshot: provenant_quota::QuotaSnapshot {
            schema_version: 1,
            account: provenant_quota::AccountScope {
                adapter: "codex".into(),
                account_key: account.into(),
                label: "test".into(),
            },
            revision,
            status,
            observed_unix_seconds: observed,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: vec![],
        },
    })
}

/// Exercise profile decisions through the public resolver and its receipt.
fn profile_preview(
    config: RoutingConfig,
    model: &str,
    account: &str,
    windows: Vec<provenant_quota::QuotaWindow>,
    references: Vec<QuotaObservation>,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    profile_preview_with_status(
        config,
        model,
        account,
        windows,
        references,
        provenant_quota::QuotaStatus::Available,
    )
}
fn profile_preview_with_status(
    mut config: RoutingConfig,
    model: &str,
    account: &str,
    windows: Vec<provenant_quota::QuotaWindow>,
    mut references: Vec<QuotaObservation>,
    status: provenant_quota::QuotaStatus,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let id = ModelId::parse(model)?;
    config.pools.insert(
        "profile-test".into(),
        vec![RoutePoolMember {
            model: id.clone(),
            weight: 1,
            effort: None,
            efforts: vec![],
            free: false,
        }],
    );
    let mut record = availability();
    record.adapter = id.adapter().into();
    record.account_key = Some(account.into());
    let records = [record];
    let access = [AccessContext {
        adapter: id.adapter(),
        disclosure_ref: "policy",
        confinement_ref: "grant",
    }];
    let mut observation = quota(model, account, status, 1, 80, 120)?;
    observation.snapshot.account.adapter = id.adapter().into();
    observation.snapshot.windows = windows;
    references.push(observation);
    let mut ctx = context(&catalogue, &config, now()?);
    ctx.availability = &records;
    ctx.access = &access;
    ctx.quota = &references;
    let mut query = request(model)?;
    query.choice = RouteChoice::NamedRoute(RouteName::parse("profile-test")?);
    Ok(resolve(&query, &ctx)?)
}

// Fresh exhaustion now excludes a named-pool member before account selection.
// A one-member profile therefore refuses the pool rather than returning an
// unavailable route. Check the exact member and reset, not an arbitrary error.
fn agy_profile_ready(
    result: Result<ResolvedRoute, Box<dyn std::error::Error>>,
    model: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    match result {
        Ok(route) => {
            assert!(matches!(route.readiness, RouteReadiness::Ready { .. }));
            Ok(true)
        }
        Err(error) => match error.downcast_ref::<RoutingError>() {
            Some(RoutingError::NoEligiblePoolMember { exclusions }) => {
                assert_eq!(
                    exclusions,
                    &[format!("{model}: exhausted; earliest known reset 604900")]
                );
                Ok(false)
            }
            _ => Err(error),
        },
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingDomains {
    routing: RoutingConfig,
}
/// Load `text` through the config loader with only the routing section.
fn load_routing(
    text: &str,
) -> Result<
    Result<provenant_config::ResolvedConfig<RoutingDomains>, provenant_config::ConfigError>,
    Box<dyn std::error::Error>,
> {
    static ENTRIES: [provenant_config::DomainSection; 1] = [SECTION];
    struct Resolver;
    impl provenant_core::PathResolver for Resolver {
        fn resolve_existing_ancestor(
            &self,
            path: &std::path::Path,
        ) -> std::io::Result<std::path::PathBuf> {
            Ok(path.into())
        }
    }
    let mut inputs =
        provenant_core::RootInputs::new("/fixture/home".into(), "/fixture/assets".into());
    inputs.state_dir = Some("/fixture/state".into());
    let roots = inputs.resolve(&Resolver)?;
    Ok(provenant_config::load_with_domains(
        &roots,
        Some(text.as_bytes()),
        &DomainSections::<RoutingDomains>::new(&ENTRIES),
    ))
}

fn account_scope(key: &str) -> provenant_quota::AccountScope {
    provenant_quota::AccountScope {
        adapter: "codex".into(),
        account_key: key.into(),
        label: "test".into(),
    }
}
fn two_accounts() -> [AvailabilityRecord; 2] {
    let mut other = availability();
    other.account_key = Some("other-account".into());
    [availability(), other]
}
const ACCESS: [AccessContext<'static>; 1] = [AccessContext {
    adapter: "codex",
    disclosure_ref: "policy",
    confinement_ref: "grant",
}];

/// A two-provider pool with a Claude and a Codex member, each ready on an account.
fn exclusion_fixture() -> Result<(Catalogue, Vec<AvailabilityRecord>), Box<dyn std::error::Error>> {
    let mut catalogue = Catalogue::bundled()?;
    let pick = |catalogue: &Catalogue, adapter: &str| {
        catalogue
            .pools
            .values()
            .flatten()
            .find(|m| m.model.adapter() == adapter)
            .cloned()
            .ok_or("missing member")
    };
    let members = vec![pick(&catalogue, "claude")?, pick(&catalogue, "codex")?];
    catalogue.pools.insert("trial".into(), members);
    let records = ["claude", "codex"]
        .map(|adapter| {
            let mut record = availability();
            record.adapter = adapter.into();
            record.account_key = Some(format!("{adapter}-key"));
            record
        })
        .to_vec();
    Ok((catalogue, records))
}
fn trial_picks(
    catalogue: &Catalogue,
    records: &[AvailabilityRecord],
    rules: &[ExclusionRule],
) -> Result<Vec<String>, RoutingError> {
    let config = RoutingConfig::default();
    let mut ctx = context(catalogue, &config, now().unwrap());
    ctx.availability = records;
    let mut query = request("codex/test").unwrap();
    query.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    let route =
        resolve_for_account_with_seed_excluding(&query, &ctx, None, "exclusion", 0, &[], rules)?;
    let receipt = route.pool_selection.ok_or(RoutingError::Catalogue)?;
    let mut adapters: Vec<String> = receipt
        .candidates
        .iter()
        .filter(|candidate| candidate.exclusion.is_none())
        .filter_map(|candidate| candidate.model.split('/').next().map(str::to_owned))
        .collect();
    adapters.dedup();
    Ok(adapters)
}
fn rule(selector: ExclusionSelector) -> ExclusionRule {
    ExclusionRule {
        selector,
        source: ExclusionSource::Flag,
    }
}

fn observation(
    adapter: &str,
    account: &str,
    route: &str,
    state: provenant_quota::ReadinessState,
    reason: Option<provenant_quota::ReadinessReason>,
    window: (i64, i64),
) -> provenant_quota::RouteReadiness {
    provenant_quota::RouteReadiness {
        schema_version: 1,
        scope: provenant_quota::ReadinessScope {
            host: provenant_quota::LOCAL_HOST.into(),
            adapter: adapter.into(),
            account_key: account.into(),
            route: route.into(),
        },
        state,
        reason,
        observed_unix_seconds: window.0,
        expires_unix_seconds: window.1,
        reset_unix_seconds: None,
        evidence_ref: Some("fixture".into()),
    }
}

/// A caller-built lookup over fixed observations, as the CLI builds one over
/// the readiness files on disk.
fn readiness_over(
    records: Vec<provenant_quota::RouteReadiness>,
) -> impl Fn(&provenant_quota::ReadinessScope, i64) -> provenant_quota::ReadinessRead {
    move |scope, now| {
        provenant_quota::ReadinessFile {
            schema_version: 1,
            login: None,
            records: records
                .iter()
                .filter(|r| {
                    r.scope.adapter == scope.adapter && r.scope.account_key == scope.account_key
                })
                .cloned()
                .collect(),
        }
        .get(scope, now)
    }
}

fn readiness_access() -> [AccessContext<'static>; 2] {
    ["codex", "agy"].map(|adapter| AccessContext {
        adapter,
        disclosure_ref: "fixture",
        confinement_ref: "fixture",
    })
}

fn agy_account(key: &str) -> AvailabilityRecord {
    AvailabilityRecord {
        adapter: "agy".into(),
        account_key: Some(key.into()),
        ..availability()
    }
}

/// A snapshot can only be built by resolving against current, validated inputs;
/// deserialized ResolvedRoute projections cannot assert fresh availability.
struct RoutingSnapshot<'a> {
    catalogue_revision: String,
    now_unix_seconds: i64,
    routes: Vec<ResolvedRoute>,
    families: &'a [FamilyFact],
}
impl<'a> RoutingSnapshot<'a> {
    pub fn new(
        requests: &[crate::RouteRequest],
        context: &crate::RoutingContext<'_>,
        families: &'a [FamilyFact],
    ) -> Result<Self, RoutingError> {
        if requests.len() > 256 || families.len() > 256 {
            return Err(RoutingError::ReviewDemand);
        }
        let routes = requests
            .iter()
            .map(|r| crate::resolve(r, context))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            catalogue_revision: context.catalogue.revision.clone(),
            now_unix_seconds: context.now.as_timestamp().as_second(),
            routes,
            families,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewDemand {
    pub family_slots: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize)]
struct ReviewAvailability {
    catalogue_revision: String,
    expires_unix_seconds: Option<i64>,
    families: BTreeMap<String, u64>,
    blockers: Vec<ReviewBlocker>,
    satisfied: bool,
}
impl ReviewAvailability {
    pub fn satisfied(&self) -> bool {
        self.satisfied
    }
}
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReviewBlocker {
    Route {
        adapter: String,
        model: String,
        reason: RouteExclusionReason,
    },
    UnknownFamily {
        model: String,
    },
    FamilySlots {
        family: String,
        required: u64,
        available: u64,
    },
}
fn review_availability(
    snapshot: &RoutingSnapshot<'_>,
    demand: &ReviewDemand,
) -> Result<ReviewAvailability, RoutingError> {
    if demand.family_slots.is_empty()
        || demand.family_slots.len() > 16
        || demand
            .family_slots
            .iter()
            .any(|(f, n)| !crate::request::identifier(f) || !(1..=64).contains(n))
        || snapshot.routes.len() > 256
        || snapshot.families.len() > 256
    {
        return Err(RoutingError::ReviewDemand);
    }
    let mut families = BTreeMap::<String, u64>::new();
    let mut blockers = Vec::new();
    let mut expiry = None::<i64>;
    let mut seen = std::collections::BTreeSet::new();
    for route in &snapshot.routes {
        if !seen.insert((&route.adapter, &route.model)) {
            continue;
        }
        let ready_until = match &route.readiness {
            RouteReadiness::Ready {
                expires_unix_seconds,
                ..
            } if route.catalogue_revision == snapshot.catalogue_revision
                && *expires_unix_seconds > snapshot.now_unix_seconds =>
            {
                *expires_unix_seconds
            }
            _ => {
                let reason = match &route.readiness {
                    RouteReadiness::Unavailable { reason, .. } => reason.clone(),
                    _ => RouteExclusionReason::StaleAvailability,
                };
                blockers.push(ReviewBlocker::Route {
                    adapter: route.adapter.clone(),
                    model: route.model.clone(),
                    reason,
                });
                continue;
            }
        };
        let fact = snapshot.families.iter().map(|f| f.record()).find(|f| {
            f.model.adapter() == route.adapter
                && f.model.model() == route.model
                && f.catalogue_revision == snapshot.catalogue_revision
                && f.observed_unix_seconds <= snapshot.now_unix_seconds
                && f.expires_unix_seconds > snapshot.now_unix_seconds
                && f.family.is_some()
        });
        if let Some(fact) = fact {
            if let Some(family) = &fact.family {
                *families.entry(family.clone()).or_default() += 1;
                let until = ready_until.min(fact.expires_unix_seconds);
                expiry = Some(expiry.map_or(until, |current| current.min(until)));
            }
        } else {
            blockers.push(ReviewBlocker::UnknownFamily {
                model: format!("{}/{}", route.adapter, route.model),
            });
        }
    }
    let mut satisfied = true;
    for (family, required) in &demand.family_slots {
        let available = families.get(family).copied().unwrap_or(0);
        if available < *required {
            satisfied = false;
            blockers.push(ReviewBlocker::FamilySlots {
                family: family.clone(),
                required: *required,
                available,
            });
        }
    }
    Ok(ReviewAvailability {
        catalogue_revision: snapshot.catalogue_revision.clone(),
        expires_unix_seconds: expiry,
        families,
        blockers,
        satisfied,
    })
}

fn go_alias_quota_case(
    all_spent: bool,
    fresh: bool,
    seed: u64,
) -> Result<ResolvedRoute, Box<dyn std::error::Error>> {
    let catalogue = Catalogue::bundled()?;
    let config: RoutingConfig = serde_json::from_value(serde_json::json!({"pools":{"trial":[
        {"model":"opencode/opencode-go/deepseek-v4.1-flash","weight":100},
        {"model":"codex/gpt-6-luna","weight":1}
    ]}}))?;
    let records = [
        availability(),
        AvailabilityRecord {
            adapter: "opencode".into(),
            account_key: Some("go".into()),
            ..availability()
        },
    ];
    let access = [
        AccessContext {
            adapter: "codex",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        },
        AccessContext {
            adapter: "opencode",
            disclosure_ref: "fixture",
            confinement_ref: "fixture",
        },
    ];
    let mut q = quota(
        "opencode/opencode-go/deepseek-v4.1-flash",
        "go",
        provenant_quota::QuotaStatus::Available,
        1,
        50,
        if fresh { 120 } else { 60 },
    )?;
    q.snapshot.account.adapter = "opencode".into();
    q.snapshot.windows.push(provenant_quota::QuotaWindow {
        pool: "five_hour".into(),
        window_seconds: Some(18000),
        used_basis_points: 10000,
        resets_unix_seconds: (!all_spent).then_some(400),
    });
    let mut readings = vec![q];
    if all_spent {
        readings.push(quota(
            "codex/gpt-6-luna",
            "opaque-account",
            provenant_quota::QuotaStatus::Exhausted,
            1,
            50,
            60,
        )?);
    }
    let ctx = RoutingContext {
        availability: &records,
        access: &access,
        quota: &readings,
        ..context(&catalogue, &config, now()?)
    };
    let mut alias = request("codex/gpt-6-luna")?;
    alias.choice = RouteChoice::NamedRoute(RouteName::parse("trial")?);
    Ok(resolve_with_seed(&alias, &ctx, "go-exhaustion", seed)?)
}

mod alternatives;
mod availability;
mod bare_model;
mod exclusions;
mod ladder;
mod policies;
mod preferences;
mod quotas;
mod readiness;
mod selection;
