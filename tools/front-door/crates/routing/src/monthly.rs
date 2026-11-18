//! Monthly pacing is account eligibility, not merely a ranking factor.
use crate::{
    Catalogue, ModelId, MonthlyPacingSettings, PacingPolicy, PacingWindow, QuotaObservation,
    RoutingConfig,
};

#[derive(Debug, serde::Serialize)]
pub struct MonthlyAllowanceAssessment {
    pub active: bool,
    pub constrained: bool,
    pub settings: SourcedMonthlySettings,
    pub windows: Vec<MonthlyBudget>,
    pub initial_launch_cost_basis_points: u64,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
pub struct MonthlyBudget {
    pub pool: Option<String>,
    pub cycle_start_unix_seconds: i64,
    pub estimated_basis_points: u64,
    pub next_launch_basis_points: u64,
    pub budget_basis_points: u64,
    pub elapsed_basis_points: u64,
    pub cost_basis_points: u64,
    pub estimated_cycle_source: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SourcedMonthlySettings {
    #[serde(flatten)]
    pub values: MonthlyPacingSettings,
    pub source: String,
    pub knob: String,
}

pub fn global_monthly_settings(config: &RoutingConfig) -> SourcedMonthlySettings {
    SourcedMonthlySettings {
        values: config.pacing.monthly.unwrap_or_default(),
        source: if config.pacing.monthly.is_some() {
            "configuration"
        } else {
            "bundled default"
        }
        .into(),
        knob: "domains.routing.pacing.monthly".into(),
    }
}

pub fn monthly_allowance_assessment(
    catalogue: &Catalogue,
    config: &RoutingConfig,
    now: provenant_core::WallTime,
    model: &ModelId,
    account: Option<&str>,
    observation: Option<&QuotaObservation>,
) -> Option<MonthlyAllowanceAssessment> {
    let (policy, profile, policy_source, _) =
        crate::account_pacing(catalogue, config, now, model, account);
    let (settings, source, key) = if let Some(settings) = profile.and_then(|p| p.monthly) {
        let account_profile = account.is_some_and(|key| {
            config
                .pacing
                .accounts
                .contains_key(&format!("{}/{key}", model.adapter()))
        });
        if account_profile {
            (
                settings,
                "account configuration",
                format!(
                    "domains.routing.pacing.accounts.{}/{}.monthly",
                    model.adapter(),
                    account.unwrap_or("unknown")
                ),
            )
        } else {
            (
                settings,
                "free configuration",
                format!("domains.routing.pacing.free.{}.monthly", model.adapter()),
            )
        }
    } else if let Some(settings) = config.pacing.monthly {
        (
            settings,
            "configuration",
            "domains.routing.pacing.monthly".into(),
        )
    } else {
        (
            MonthlyPacingSettings::default(),
            "bundled default",
            "domains.routing.pacing.monthly".into(),
        )
    };
    let monthly_selected = profile
        .and_then(|p| p.windows.as_ref())
        .is_none_or(|kinds| kinds.contains(&PacingWindow::Monthly));
    let serves_pool = |pool: &str| {
        provenant_records::AdapterSpec::get(model.adapter())
            .is_none_or(|spec| spec.window_serves_pool(pool, catalogue.quota_pool(model)))
    };
    // A declared monthly allowance cannot become neutral just because its report is absent.
    let required =
        provenant_records::AdapterSpec::get(model.adapter()).is_some_and(|s| s.monthly_allowance)
            || profile.is_some_and(|p| {
                p.monthly.is_some()
                    || p.windows
                        .as_ref()
                        .is_some_and(|w| w.contains(&PacingWindow::Monthly))
            })
            || observation.is_some_and(|q| {
                q.snapshot.windows.iter().any(|window| {
                    provenant_quota::monthly_window(window) && serves_pool(&window.pool)
                }) || q.allowance.windows.keys().any(|pool| serves_pool(pool))
            });
    if !required {
        return None;
    }
    let prefix = format!(
        "monthly policy={policy:?} policy_source={policy_source} enabled={} burst_percent={} initial_launch_cost_basis_points={} source={source} knob={key}",
        settings.enabled, settings.burst_percent, settings.initial_launch_cost_basis_points
    );
    let sourced = || SourcedMonthlySettings {
        values: settings,
        source: source.into(),
        knob: key.clone(),
    };
    if policy != PacingPolicy::Pace || !settings.enabled || !monthly_selected {
        return Some(MonthlyAllowanceAssessment {
            active: false,
            windows: Vec::new(),
            settings: sourced(),
            constrained: false,
            initial_launch_cost_basis_points: settings.initial_launch_cost_basis_points,
            detail: format!("{prefix}; monthly cap bypassed"),
        });
    }
    let now = now.unix_seconds();
    let unregistered_local = account.is_some_and(|key| {
        provenant_records::AdapterSpec::get(model.adapter())
            .is_some_and(|spec| spec.local_login && spec.local_account.1 == key)
    });
    let scoped = observation.filter(|q| {
        q.model == *model
            && q.snapshot.account.adapter == model.adapter()
            && Some(q.snapshot.account.account_key.as_str()) == account
    });
    let unknown = || {
        if unregistered_local {
            let (reset, duration, cycle_source) = estimated_cycle(scoped, now, &serves_pool)?;
            let start = reset.checked_sub(duration)?;
            let elapsed =
                ((u128::from(now.saturating_sub(start) as u64) * 10_000) / duration as u128) as u64;
            let budget = elapsed
                .saturating_add(u64::from(settings.burst_percent) * 100)
                .min(10_000);
            let ledger = scoped.map(|q| &q.allowance);
            let cost = ledger
                .and_then(|l| {
                    l.windows
                        .iter()
                        .filter(|(pool, _)| serves_pool(pool))
                        .filter_map(|(_, w)| w.cost_basis_points)
                        .max()
                })
                .unwrap_or(settings.initial_launch_cost_basis_points);
            let launches = ledger.map_or(0, |l| l.launches_in_cycle(start, now));
            let estimated = ledger.map_or(0, |l| {
                let pending = l.estimated_in_cycle(start, now, cost);
                l.windows
                    .iter()
                    .filter(|(pool, w)| serves_pool(pool) && w.reset == reset)
                    .map(|(_, w)| l.estimated(w))
                    .max()
                    .unwrap_or(pending)
                    .max(pending)
            });
            let next = estimated.saturating_add(cost);
            let constrained = next > budget;
            return Some(MonthlyAllowanceAssessment {
                active: true,
                windows: vec![MonthlyBudget {
                    pool: None,
                    cycle_start_unix_seconds: start,
                    estimated_basis_points: estimated,
                    next_launch_basis_points: next,
                    budget_basis_points: budget,
                    elapsed_basis_points: elapsed,
                    cost_basis_points: cost,
                    estimated_cycle_source: Some(cycle_source.into()),
                }],
                settings: sourced(),
                constrained,
                initial_launch_cost_basis_points: settings.initial_launch_cost_basis_points,
                detail: format!(
                    "{prefix}; estimated cycle: {cycle_source}; reset={reset} duration={duration} estimated_bp={estimated} next_launch_bp={next} budget_bp={budget} elapsed_bp={elapsed} cost_bp={cost} ledger_launches={launches}{}",
                    if constrained { "; constrained" } else { "" }
                ),
            });
        }
        Some(MonthlyAllowanceAssessment {
            active: true,
            windows: Vec::new(),
            settings: sourced(),
            constrained: true,
            initial_launch_cost_basis_points: settings.initial_launch_cost_basis_points,
            detail: format!("{prefix}; monthly window missing, stale or incomplete; constrained"),
        })
    };
    let Some(q) = observation.filter(|q| {
        q.snapshot.schema_version == 1
            && q.model == *model
            && q.snapshot.account.adapter == model.adapter()
            && Some(q.snapshot.account.account_key.as_str()) == account
            && q.snapshot.observed_unix_seconds <= now
            && q.snapshot.observed_unix_seconds < q.expires_unix_seconds
            && q.expires_unix_seconds > now
            && matches!(
                q.snapshot.status,
                provenant_quota::QuotaStatus::Available
                    | provenant_quota::QuotaStatus::CreditOnly
                    | provenant_quota::QuotaStatus::Exhausted
            )
    }) else {
        return unknown();
    };
    let windows: Vec<_> = q
        .snapshot
        .windows
        .iter()
        .filter(|w| {
            provenant_quota::monthly_window(w)
                && provenant_records::AdapterSpec::get(model.adapter())
                    .is_none_or(|s| s.window_serves_pool(&w.pool, catalogue.quota_pool(model)))
        })
        .collect();
    if windows.is_empty() {
        return unknown();
    }
    let mut constrained = false;
    let mut details = Vec::new();
    let mut budgets = Vec::new();
    for w in windows {
        let (Some(reset), Some(duration)) = (w.resets_unix_seconds, w.window_seconds) else {
            return unknown();
        };
        let Ok(duration) = i64::try_from(duration) else {
            return unknown();
        };
        let Some(start) = reset.checked_sub(duration) else {
            return unknown();
        };
        if duration <= 0 || now < start || now >= reset {
            return unknown();
        }
        let elapsed =
            ((u128::from(now.saturating_sub(start) as u64) * 10_000) / duration as u128) as u64;
        let budget = elapsed
            .saturating_add(u64::from(settings.burst_percent) * 100)
            .min(10_000);
        let ledger = q
            .allowance
            .windows
            .get(&w.pool)
            .filter(|l| l.reset == reset);
        if ledger.is_none() && q.allowance.launches > 0 {
            return unknown();
        }
        let cost = ledger
            .and_then(|l| l.cost_basis_points)
            .unwrap_or(settings.initial_launch_cost_basis_points);
        let estimated = ledger.map_or(w.used_basis_points, |l| {
            q.allowance.estimated(l).max(w.used_basis_points)
        });
        let next = estimated.saturating_add(cost);
        constrained |= next > budget;
        budgets.push(MonthlyBudget {
            pool: Some(w.pool.clone()),
            cycle_start_unix_seconds: start,
            estimated_basis_points: estimated,
            next_launch_basis_points: next,
            budget_basis_points: budget,
            elapsed_basis_points: elapsed,
            cost_basis_points: cost,
            estimated_cycle_source: None,
        });
        details.push(format!("window={} estimated_bp={estimated} next_launch_bp={next} budget_bp={budget} elapsed_bp={elapsed} cost_bp={cost} ledger_launches={}", w.pool, q.allowance.launches));
    }
    Some(MonthlyAllowanceAssessment {
        active: true,
        windows: budgets,
        settings: sourced(),
        constrained,
        initial_launch_cost_basis_points: settings.initial_launch_cost_basis_points,
        detail: format!(
            "{prefix}; {}{}",
            details.join("; "),
            if constrained { "; constrained" } else { "" }
        ),
    })
}

/// Carry a known cycle forward; UTC calendar months supply the last-resort estimate.
fn estimated_cycle(
    q: Option<&QuotaObservation>,
    now: i64,
    serves_pool: &impl Fn(&str) -> bool,
) -> Option<(i64, i64, &'static str)> {
    let roll = |reset: i64, duration: u64| {
        let duration = i64::try_from(duration).ok().filter(|d| *d > 0)?;
        let reset = if now >= reset {
            reset.checked_add(
                now.checked_sub(reset)?
                    .checked_div(duration)?
                    .checked_add(1)?
                    .checked_mul(duration)?,
            )?
        } else {
            reset
        };
        (reset.checked_sub(duration)? <= now).then_some((reset, duration))
    };
    if let Some(q) = q {
        if let Some((reset, duration)) = q
            .allowance
            .windows
            .iter()
            .filter(|(pool, w)| serves_pool(pool) && w.observed <= now)
            .filter_map(|(_, w)| roll(w.reset, w.duration))
            .max_by_key(|(reset, _)| *reset)
        {
            return Some((reset, duration, "ledger reset and duration"));
        }
        if let Some((reset, duration)) = q
            .snapshot
            .windows
            .iter()
            .filter(|w| {
                q.snapshot.observed_unix_seconds <= now
                    && provenant_quota::monthly_window(w)
                    && serves_pool(&w.pool)
            })
            .filter_map(|w| roll(w.resets_unix_seconds?, w.window_seconds?))
            .max_by_key(|(reset, _)| *reset)
        {
            return Some((reset, duration, "previous snapshot reset and duration"));
        }
    }
    let date = jiff::Timestamp::from_second(now)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    let start = jiff::civil::Date::new(date.year(), date.month(), 1)
        .ok()?
        .at(0, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    let duration = i64::from(date.days_in_month()) * 86400;
    Some((start.checked_add(duration)?, duration, "calendar month UTC"))
}
