//! Monthly pacing is account eligibility, not merely a ranking factor.
use crate::{
    Catalogue, ModelId, MonthlyPacingSettings, PacingPolicy, PacingWindow, QuotaObservation,
    RoutingConfig,
};

#[derive(Debug)]
pub struct MonthlyAllowanceAssessment {
    pub constrained: bool,
    pub detail: String,
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
    if policy != PacingPolicy::Pace || !settings.enabled || !monthly_selected {
        return Some(MonthlyAllowanceAssessment {
            constrained: false,
            detail: format!("{prefix}; monthly cap bypassed"),
        });
    }
    let now = now.unix_seconds();
    let unregistered_local = account.is_some_and(|key| {
        provenant_records::AdapterSpec::get(model.adapter())
            .is_some_and(|spec| spec.local_login && spec.local_account.1 == key)
    });
    let unknown = || {
        let constrained = !unregistered_local;
        let outcome = if constrained {
            "constrained"
        } else {
            "advisory for unregistered local login"
        };
        Some(MonthlyAllowanceAssessment {
            constrained,
            detail: format!("{prefix}; monthly window missing, stale or incomplete; {outcome}"),
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
            l.estimated(
                q.allowance.launches,
                settings.initial_launch_cost_basis_points,
            )
            .max(w.used_basis_points)
        });
        let next = estimated.saturating_add(cost);
        constrained |= next > budget;
        details.push(format!("window={} estimated_bp={estimated} next_launch_bp={next} budget_bp={budget} elapsed_bp={elapsed} cost_bp={cost} ledger_launches={}", w.pool, q.allowance.launches));
    }
    Some(MonthlyAllowanceAssessment {
        constrained,
        detail: format!(
            "{prefix}; {}{}",
            details.join("; "),
            if constrained { "; constrained" } else { "" }
        ),
    })
}
