//! Stratified comparisons. Finding volume alone is never a ranking signal.
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::outcome::{Attributed, Outcomes};

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Interval {
    pub low: f64,
    pub high: f64,
}

/// Two-sided 95% Wilson score interval. No data is not a zero rate.
pub fn wilson(successes: u64, trials: u64) -> Option<Interval> {
    if trials == 0 || successes > trials {
        return None;
    }
    let n = trials as f64;
    let p = successes as f64 / n;
    let z2 = 1.959963984540054_f64.powi(2);
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = (z2 * (p * (1.0 - p) / n + z2 / (4.0 * n * n))).sqrt() / (1.0 + z2 / n);
    Some(Interval {
        low: (centre - half).max(0.0),
        high: (centre + half).min(1.0),
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Rate {
    pub numerator: u64,
    pub denominator: u64,
    pub value: Option<f64>,
    pub interval: Option<Interval>,
}
fn rate(numerator: u64, denominator: u64) -> Rate {
    Rate {
        numerator,
        denominator,
        value: (denominator > 0).then(|| numerator as f64 / denominator as f64),
        interval: wilson(numerator, denominator),
    }
}

#[derive(Debug, Serialize)]
pub struct Arm {
    pub dimension: &'static str,
    pub choice: String,
    pub risk_tier: String,
    pub task_kind: String,
    pub attribution: &'static str,
    /// Independent graphs, not attempts or repeated verifier copies.
    pub samples: usize,
    pub confirmed: u64,
    pub false_positives: u64,
    pub cost_microdollars: u64,
    pub missing_cost: usize,
    pub missing_dispositions: usize,
    pub confirmed_per_dollar: Option<f64>,
    /// Wilson confirmed share scaled by dispositions per dollar. Conditional
    /// on observed costs; this does not estimate cost variability.
    pub confirmed_per_dollar_interval: Option<Interval>,
    pub false_positive_rate: Rate,
    pub escaped_defect_rate: Rate,
    pub ci: &'static str,
    pub status: &'static str,
    /// Explains why an arm cannot participate in a stratified comparison.
    pub insufficient_reason: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Suggestion {
    pub dimension: &'static str,
    pub risk_tier: String,
    pub task_kind: String,
    pub preferred: String,
    pub compared_with: String,
    /// Indices into the arms array carry every count and interval.
    pub evidence: [usize; 2],
    pub counts: [usize; 2],
    pub interval: [Interval; 2],
    pub expires: String,
    pub accept: Option<String>,
    pub advisory_reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Suggestions {
    pub min_samples: usize,
    pub expiry_days: u32,
    pub arms: Vec<Arm>,
    pub suggestions: Vec<Suggestion>,
}

/// A conservative Pareto comparison: separated confirmed-per-dollar intervals,
/// and no worse observed false-positive or escaped-defect rates. Only mature,
/// fully measured arms with the same risk/task strata can generate advice.
pub fn rank(
    outcomes: &Outcomes,
    min_samples: usize,
    expiry_days: u32,
    now: jiff::Timestamp,
) -> Result<Suggestions, String> {
    if min_samples == 0 || expiry_days == 0 {
        return Err("sample minimum and expiry must be positive".into());
    }
    let expires = now
        .checked_add(jiff::SignedDuration::from_secs(
            i64::from(expiry_days) * 86400,
        ))
        .map_err(|e| e.to_string())?;
    let mut arms = Vec::new();
    for dimension in ["pattern", "lens", "model"] {
        let mut groups =
            BTreeMap::<(String, String, String, &'static str), Vec<&Attributed>>::new();
        for row in &outcomes.rows {
            // Young merges remain in the outcome report, but cannot mix a
            // shorter observation period into the ranking cohort.
            if row.merge.is_some() && !row.mature {
                continue;
            }
            let e = &row.evidence;
            let choice = match dimension {
                "pattern" => &e.pattern,
                "lens" => &e.lens,
                _ => &e.model,
            };
            groups
                .entry((
                    choice.as_deref().unwrap_or("unknown").into(),
                    e.risk_tier.as_deref().unwrap_or("unknown").into(),
                    e.task_kind.as_deref().unwrap_or("unknown").into(),
                    row.stratum,
                ))
                .or_default()
                .push(row);
        }
        for ((choice, risk_tier, task_kind, attribution), rows) in groups {
            let samples: BTreeSet<_> = rows
                .iter()
                .map(|r| r.evidence.graph.as_deref().unwrap_or(&r.evidence.sample))
                .collect();
            let mut confirmed = 0u64;
            let mut false_positives = 0u64;
            let mut cost = 0u64;
            let mut missing_cost = 0;
            let mut missing_dispositions = 0;
            let mut merges = BTreeSet::new();
            let mut escaped = BTreeSet::new();
            let mut identities = BTreeSet::new();
            for row in &rows {
                // A caller cannot inflate a sample by repeating its row.
                if !identities.insert(&row.evidence.sample) {
                    continue;
                }
                let e = &row.evidence;
                let add = |a: u64, b: u64| {
                    a.checked_add(b)
                        .ok_or_else(|| "ranking count overflow".to_owned())
                };
                if let Some(value) = e.cost_microdollars {
                    cost = add(cost, value)?;
                } else {
                    missing_cost += 1;
                }
                match (e.confirmed, e.false_positives) {
                    (Some(c), Some(f)) => {
                        confirmed = add(confirmed, c)?;
                        false_positives = add(false_positives, f)?;
                    }
                    _ => missing_dispositions += 1,
                }
                if row.mature
                    && let Some(merge) = &row.merge
                {
                    merges.insert(merge);
                    if row.signals.escaped() {
                        escaped.insert(merge);
                    }
                }
            }
            let dispositions = confirmed
                .checked_add(false_positives)
                .ok_or("ranking count overflow")?;
            let per_dollar = (missing_cost == 0 && cost > 0 && missing_dispositions == 0)
                .then(|| confirmed as f64 * 1_000_000.0 / cost as f64);
            let interval = per_dollar
                .and_then(|_| wilson(confirmed, dispositions))
                .map(|i| {
                    let scale = dispositions as f64 * 1_000_000.0 / cost as f64;
                    Interval {
                        low: i.low * scale,
                        high: i.high * scale,
                    }
                });
            let sufficient = samples.len() >= min_samples
                && merges.len() >= min_samples
                && missing_cost == 0
                && missing_dispositions == 0
                && dispositions > 0
                && interval.is_some()
                && outcomes.ci == "measured"
                && rows.iter().all(|row| row.signals.ci_failures.is_some())
                && attribution == "attributed"
                && choice != "unknown"
                && risk_tier != "unknown"
                && task_kind != "unknown";
            let insufficient_reason = if risk_tier == "unknown" || task_kind == "unknown" {
                Some("risk tier and task kind must both be known")
            } else if !sufficient {
                Some("insufficient or incomplete evidence")
            } else {
                None
            };
            let mut escaped_defect_rate = rate(escaped.len() as u64, merges.len() as u64);
            if outcomes.ci != "measured" || rows.iter().any(|row| row.signals.ci_failures.is_none())
            {
                escaped_defect_rate.value = None;
                escaped_defect_rate.interval = None;
            }
            arms.push(Arm {
                dimension,
                choice,
                risk_tier,
                task_kind,
                attribution,
                samples: samples.len(),
                confirmed,
                false_positives,
                cost_microdollars: cost,
                missing_cost,
                missing_dispositions,
                confirmed_per_dollar: per_dollar,
                confirmed_per_dollar_interval: interval,
                false_positive_rate: rate(false_positives, dispositions),
                escaped_defect_rate,
                ci: outcomes.ci,
                status: if sufficient {
                    "sufficient"
                } else {
                    "insufficient"
                },
                insufficient_reason,
            });
        }
    }
    let mut suggestions = Vec::new();
    for (i, a) in arms
        .iter()
        .enumerate()
        .filter(|(_, a)| a.status == "sufficient")
    {
        for (j, b) in arms
            .iter()
            .enumerate()
            .filter(|(_, b)| b.status == "sufficient")
        {
            if i == j
                || a.dimension != b.dimension
                || a.risk_tier != b.risk_tier
                || a.task_kind != b.task_kind
            {
                continue;
            }
            let (Some(ai), Some(bi)) = (
                a.confirmed_per_dollar_interval,
                b.confirmed_per_dollar_interval,
            ) else {
                continue;
            };
            if ai.low > bi.high
                && a.false_positive_rate.value <= b.false_positive_rate.value
                && a.escaped_defect_rate.value <= b.escaped_defect_rate.value
            {
                suggestions.push(Suggestion { dimension: a.dimension, risk_tier: a.risk_tier.clone(), task_kind: a.task_kind.clone(), preferred: a.choice.clone(), compared_with: b.choice.clone(), evidence: [i, j], counts: [a.samples, b.samples], interval: [ai, bi], expires: expires.to_string(), accept: None, advisory_reason: "No scheduled choice write path exists; graph node asks are not implemented." });
            }
        }
    }
    Ok(Suggestions {
        min_samples,
        expiry_days,
        arms,
        suggestions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::{Evidence, Signals};

    #[test]
    fn unknown_strata_cannot_generate_comparisons() {
        let now: jiff::Timestamp = "2026-02-01T00:00:00Z".parse().unwrap();
        for (risk, task) in [
            (None, Some("repair")),
            (Some("high"), None),
            (Some("unknown"), Some("repair")),
        ] {
            let mut rows = Vec::new();
            for (choice, confirmed, cost) in [("good", 10, 1_000_000), ("poor", 1, 10_000_000)] {
                for i in 0..20 {
                    let sample = format!("{choice}-{i}");
                    rows.push(Attributed {
                        evidence: Evidence {
                            sample: sample.clone(),
                            graph: Some(sample.clone()),
                            pattern: Some(choice.into()),
                            risk_tier: risk.map(str::to_owned),
                            task_kind: task.map(str::to_owned),
                            cost_microdollars: Some(cost),
                            confirmed: Some(confirmed),
                            false_positives: Some(0),
                            ..Evidence::default()
                        },
                        stratum: "attributed",
                        merge: Some(sample),
                        mature: true,
                        signals: Signals {
                            ci_failures: Some(0),
                            ..Signals::default()
                        },
                    });
                }
            }
            let outcomes = Outcomes {
                ci_coverage_from: None,
                skipped_ci_runs: 0,
                window_days: 14,
                ci: "measured",
                unattributed_rows: 0,
                rows,
            };
            let result = rank(&outcomes, 20, 14, now).unwrap();
            assert!(result.suggestions.is_empty());
            for arm in &result.arms {
                assert_eq!(arm.status, "insufficient");
                assert_eq!(
                    arm.insufficient_reason,
                    Some("risk tier and task kind must both be known")
                );
            }
        }
    }
}
