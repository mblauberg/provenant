use super::{LandingError, model::CostEstimate};
use provenant_cli::domains::BudgetSettings;

/// Estimate the cost of observed pool durations, preserving unknown prices.
///
/// Cost is rounded up to whole micros per pool. Any pool without a configured rate makes the
/// whole estimate unknown: a partial sum would understate spending.
pub fn estimate(
    settings: &BudgetSettings,
    durations_seconds: &[(String, u64)],
) -> Result<CostEstimate, LandingError> {
    if durations_seconds.is_empty() {
        return Ok(CostEstimate::Unknown {
            reason: "no pool durations recorded".into(),
        });
    }
    let mut unpriced: Vec<&str> = durations_seconds
        .iter()
        .filter(|(pool, _)| !settings.rate_micros_per_minute.contains_key(pool))
        .map(|(pool, _)| pool.as_str())
        .collect();
    if !unpriced.is_empty() {
        unpriced.sort_unstable();
        unpriced.dedup();
        return Ok(CostEstimate::Unknown {
            reason: format!("no price configured for pool {}", unpriced.join(", ")),
        });
    }
    let mut total: u128 = 0;
    for (pool, seconds) in durations_seconds {
        let rate = u128::from(settings.rate_micros_per_minute[pool]);
        total += (rate * u128::from(*seconds)).div_ceil(60);
    }
    Ok(CostEstimate::Known {
        micros: u64::try_from(total).unwrap_or(u64::MAX),
    })
}

/// Return an advisory warning or refuse an explicit cap excess.
pub fn check(
    settings: &BudgetSettings,
    estimate: &CostEstimate,
) -> Result<Option<String>, LandingError> {
    match estimate {
        CostEstimate::Known { micros } => {
            if let Some(cap) = settings.cap_micros
                && *micros > cap
            {
                return Err(LandingError::SpendingCap {
                    micros: *micros,
                    cap,
                });
            }
            Ok(settings
                .advisory_micros
                .filter(|advisory| micros > advisory)
                .map(|advisory| {
                    format!("estimated cost {micros} micros exceeds the advisory budget of {advisory} micros")
                }))
        }
        CostEstimate::Unknown { reason } => {
            // An unknown price cannot be shown to exceed a cap, so it warns rather than refuses.
            if settings.advisory_micros.is_some() || settings.cap_micros.is_some() {
                Ok(Some(format!(
                    "cost is unknown, so the configured budget cannot be checked: {reason}"
                )))
            } else {
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn settings(advisory: Option<u64>, cap: Option<u64>) -> BudgetSettings {
        BudgetSettings {
            rate_micros_per_minute: BTreeMap::from([("fast".to_string(), 600_000)]),
            advisory_micros: advisory,
            cap_micros: cap,
        }
    }

    #[test]
    fn prices_known_pools_and_rounds_up() -> Result<(), LandingError> {
        let s = settings(None, None);
        assert_eq!(
            estimate(&s, &[("fast".into(), 90), ("fast".into(), 1)])?,
            CostEstimate::Known { micros: 910_000 }
        );
        let mut fractional = s.clone();
        fractional.rate_micros_per_minute.insert("fast".into(), 1);
        assert_eq!(
            estimate(&fractional, &[("fast".into(), 1)])?,
            CostEstimate::Known { micros: 1 }
        );
        Ok(())
    }

    #[test]
    fn unknown_price_is_reported_not_zero() -> Result<(), LandingError> {
        let s = settings(None, None);
        let got = estimate(&s, &[("fast".into(), 60), ("slow".into(), 60)])?;
        assert!(matches!(got, CostEstimate::Unknown { ref reason } if reason.contains("slow")));
        assert!(matches!(estimate(&s, &[])?, CostEstimate::Unknown { .. }));
        Ok(())
    }

    #[test]
    fn advisory_warns_and_cap_refuses_only_above() -> Result<(), LandingError> {
        let s = settings(Some(100), Some(200));
        assert_eq!(check(&s, &CostEstimate::Known { micros: 100 })?, None);
        assert!(check(&s, &CostEstimate::Known { micros: 101 })?.is_some());
        assert!(check(&s, &CostEstimate::Known { micros: 200 })?.is_some());
        assert!(check(&s, &CostEstimate::Known { micros: 201 }).is_err());
        let no_cap = settings(Some(1), None);
        assert!(check(&no_cap, &CostEstimate::Known { micros: u64::MAX })?.is_some());
        Ok(())
    }

    #[test]
    fn unknown_cost_warns_when_a_budget_is_configured_and_never_refuses() -> Result<(), LandingError>
    {
        let unknown = CostEstimate::Unknown { reason: "r".into() };
        assert!(check(&settings(None, Some(1)), &unknown)?.is_some());
        assert_eq!(check(&settings(None, None), &unknown)?, None);
        Ok(())
    }
}
