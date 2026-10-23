//! Route outcome reducers and text tables. Full ledger report selection is a later slice.
use crate::{
    Numeric, Result, Usage,
    costs::{self, Prices},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const OUTCOME_KEYS: [&str; 5] = ["adapter", "model", "effort", "task_class", "route"];
pub const ROW_LIMIT: usize = 12;
const TOKEN_FIELDS: [&str; 5] = [
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
    "reasoning_tokens",
];
pub fn percentile(values: &[f64], share: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    values
        .get(((share * values.len() as f64).ceil() as usize).saturating_sub(1))
        .copied()
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Outcome {
    pub adapter: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub task_class: Option<String>,
    pub route: Option<String>,
    pub lanes: u64,
    pub succeeded: u64,
    pub success_rate: Option<f64>,
    pub attempts: u64,
    pub retries: Numeric,
    pub known_cost_usd: f64,
    pub missing_cost_rows: u64,
    pub cost_per_success_usd: Option<f64>,
    pub error_classes: BTreeMap<String, u64>,
    pub latency_samples: usize,
    pub latency_p50_s: Option<f64>,
    pub latency_p90_s: Option<f64>,
    pub output_tokens_per_s: Option<f64>,
}
fn successful(row: &Usage) -> bool {
    matches!(row.status.as_deref(), Some("ok" | "succeeded"))
}
fn key(row: &Usage, total: bool) -> Vec<Option<String>> {
    if total {
        return vec![];
    }
    [
        &row.adapter,
        &row.model,
        &row.effort,
        &row.task_class,
        &row.route,
    ]
    .into_iter()
    .map(|v| v.clone().filter(|v| !v.is_empty()))
    .collect()
}
/// Latest attempts decide lane success; every attempt contributes errors, cost and timing.
/// `total` computes percentiles from all samples rather than averaging route quantiles.
pub fn outcomes(rows: &[Usage], prices: &Prices, total: bool) -> Result<Vec<Outcome>> {
    type LaneKey = (Option<String>, Option<String>, Option<String>);
    #[derive(Default)]
    struct Group {
        key: Vec<Option<String>>,
        out: Outcome,
        latency: Vec<f64>,
        speed: Vec<f64>,
        latest: BTreeMap<LaneKey, usize>,
    }
    let mut groups = Vec::<Group>::new();
    for (index, row) in rows.iter().enumerate() {
        let k = key(row, total);
        let position = groups.iter().position(|g| g.key == k).unwrap_or_else(|| {
            groups.push(Group {
                key: k,
                ..Group::default()
            });
            groups.len() - 1
        });
        let group = &mut groups[position];
        group.out.attempts += 1;
        if let Some(cost) = costs::estimated_cost(row, &prices.entry(row)?) {
            group.out.known_cost_usd += cost;
        } else {
            group.out.missing_cost_rows += 1;
        }
        if !successful(row) {
            if let Some(error) = row.error_class.as_ref().filter(|e| !e.is_empty()) {
                *group.out.error_classes.entry(error.clone()).or_default() += 1;
            }
        }
        if let Some(ms) = row.duration_ms {
            group.latency.push(ms.as_f64() / 1000.0);
        }
        if let Some(ms) = row.provider_ms.filter(|ms| !ms.is_zero()) {
            if row.output_tokens.is_some() || row.reasoning_tokens.is_some() {
                group.speed.push(
                    row.output_tokens
                        .unwrap_or(Numeric::ZERO)
                        .checked_add(row.reasoning_tokens.unwrap_or(Numeric::ZERO))?
                        .as_f64()
                        / (ms.as_f64() / 1000.0),
                );
            }
        }
        let lane = (
            row.repository.clone(),
            row.run_id.clone(),
            row.task_id.clone(),
        );
        if group.latest.get(&lane).is_none_or(|old| {
            row.attempt.unwrap_or(Numeric::ZERO) > rows[*old].attempt.unwrap_or(Numeric::ZERO)
        }) {
            group.latest.insert(lane, index);
        }
    }
    groups.sort_by_key(|g| {
        g.key
            .iter()
            .map(|k| k.clone().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    groups
        .into_iter()
        .map(|mut g| {
            for index in g.latest.values() {
                let row = &rows[*index];
                g.out.lanes += 1;
                g.out.succeeded += u64::from(successful(row));
                g.out.retries = g.out.retries.checked_add(
                    row.attempt
                        .filter(|n| !n.is_zero())
                        .unwrap_or(Numeric::ONE)
                        .subtract(Numeric::ONE)?
                        .non_negative(),
                )?;
            }
            if !total {
                g.out.adapter = g.key[0].clone();
                g.out.model = g.key[1].clone();
                g.out.effort = g.key[2].clone();
                g.out.task_class = g.key[3].clone();
                g.out.route = g.key[4].clone();
            }
            g.out.success_rate = if g.out.lanes > 0 {
                Some(costs::round(g.out.succeeded as f64 / g.out.lanes as f64, 3))
            } else {
                None
            };
            g.out.cost_per_success_usd = if g.out.succeeded > 0 && g.out.missing_cost_rows == 0 {
                costs::micro(Some(g.out.known_cost_usd / g.out.succeeded as f64))
            } else {
                None
            };
            g.out.known_cost_usd = costs::round(g.out.known_cost_usd, 6);
            g.out.latency_samples = g.latency.len();
            g.out.latency_p50_s = percentile(&g.latency, 0.5);
            g.out.latency_p90_s = percentile(&g.latency, 0.9);
            g.speed.sort_by(f64::total_cmp);
            let n = g.speed.len();
            g.out.output_tokens_per_s = if n == 0 {
                None
            } else if n % 2 == 1 {
                Some(costs::round(g.speed[n / 2], 2))
            } else {
                Some(costs::round((g.speed[n / 2 - 1] + g.speed[n / 2]) / 2.0, 2))
            };
            Ok(g.out)
        })
        .collect::<Result<Vec<_>>>()
}
/// Peer v1 intentionally predates cost enrichment; do not expose unknown numeric fields.
pub fn peer_outcome(row: &Outcome) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut value = serde_json::to_value(row)?
        .as_object()
        .cloned()
        .unwrap_or_default();
    for field in [
        "known_cost_usd",
        "missing_cost_rows",
        "cost_per_success_usd",
    ] {
        value.remove(field);
    }
    Ok(value.into_iter().collect())
}

fn significant(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let exponent = value.abs().log10().floor() as i32;
    let rounded = costs::round(value, ((2 - exponent).max(0)) as usize);
    let exponent = if rounded == 0.0 {
        exponent
    } else {
        rounded.abs().log10().floor() as i32
    };
    if !(-4..3).contains(&exponent) {
        let scientific = format!("{value:.2e}");
        if let Some((mantissa, exp)) = scientific.split_once('e') {
            let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
            if let Ok(exp) = exp.parse::<i32>() {
                return format!(
                    "{mantissa}e{}{abs:02}",
                    if exp < 0 { '-' } else { '+' },
                    abs = exp.abs()
                );
            }
        }
        scientific
    } else {
        let decimals = (2 - exponent).max(0) as usize;
        let text = format!("{value:.decimals$}");
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').into()
        } else {
            text
        }
    }
}
pub fn compact(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "-".into();
    };
    if value.fract() != 0.0 {
        return format!("{value:.2}");
    }
    for (size, suffix) in [(1e9, "B"), (1e6, "M"), (1e3, "k")] {
        if value.abs() >= size {
            return format!("{}{suffix}", significant(value / size));
        }
    }
    format!("{value:.0}")
}
pub fn table(headers: &[String], rows: &[Vec<String>]) -> Result<Vec<String>> {
    if rows.iter().any(|r| r.len() != headers.len()) {
        return Err(crate::Error::Invalid("invalid table row width".into()));
    }
    let widths: Vec<_> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain([h.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    Ok(std::iter::once(headers)
        .chain(rows.iter().map(Vec::as_slice))
        .map(|row| {
            row.iter()
                .zip(&widths)
                .map(|(cell, width)| {
                    format!(
                        "{cell}{}",
                        " ".repeat(width.saturating_sub(cell.chars().count()))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
                .trim_end_matches(crate::pending::python_whitespace)
                .to_owned()
        })
        .collect())
}
pub fn bounded_table(
    headers: &[String],
    rows: &[Vec<Option<String>>],
    limit: Option<usize>,
    always: &[&str],
) -> Result<Vec<String>> {
    if rows.iter().any(|r| r.len() != headers.len()) {
        return Err(crate::Error::Invalid("invalid table row width".into()));
    }
    let keep: Vec<_> = headers
        .iter()
        .enumerate()
        .filter(|(i, h)| always.contains(&h.as_str()) || rows.iter().any(|r| r[*i].is_some()))
        .map(|(i, _)| i)
        .collect();
    let selected = if keep.is_empty() {
        headers.to_vec()
    } else {
        keep.iter().map(|i| headers[*i].clone()).collect()
    };
    let cells: Vec<_> = rows
        .iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|r| {
            keep.iter()
                .map(|i| r[*i].clone().unwrap_or_else(|| "-".into()))
                .collect()
        })
        .collect();
    table(&selected, &cells)
}

fn field<'a>(value: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    value.get(name).unwrap_or(&serde_json::Value::Null)
}
fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "None".into(),
        serde_json::Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}
fn text_or(value: &serde_json::Value, fallback: &str) -> String {
    if value.is_null() || value.as_str().is_some_and(str::is_empty) {
        fallback.into()
    } else {
        value_text(value)
    }
}
fn compact_json(value: &serde_json::Value) -> Option<String> {
    let number = value.as_number()?;
    if let Some(n) = number.as_i64() {
        return Some(compact(Some(n as f64)));
    }
    if let Some(n) = number.as_u64() {
        if n < 1000 {
            return Some(n.to_string());
        }
        return Some(compact(Some(n as f64)));
    }
    number.as_f64().map(|n| compact(Some(n)))
}
fn python_general(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let scientific = format!("{value:.5e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent = exponent.parse::<i32>().unwrap_or(0);
    if !(-4..6).contains(&exponent) {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        format!(
            "{mantissa}e{}{abs:02}",
            if exponent < 0 { '-' } else { '+' },
            abs = exponent.abs()
        )
    } else {
        let rounded = scientific.parse::<f64>().unwrap_or(value);
        let digits = (5 - exponent).max(0) as usize;
        let fixed = format!("{rounded:.digits$}");
        if fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.').into()
        } else {
            fixed
        }
    }
}
fn array(value: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    value.as_array().into_iter().flatten()
}
fn text_row(values: impl IntoIterator<Item = String>) -> String {
    format!("  {}", values.into_iter().collect::<Vec<_>>().join("  "))
}

/// Render the existing stats report without owning its ledger or printing directly.
pub fn render_stats(report: &serde_json::Value, detail: bool) -> Result<String> {
    let limit = if detail { usize::MAX } else { ROW_LIMIT };
    let mut lines = vec![format!(
        "Usage on {}{}: costs are API-equivalent USD estimates; - marks missing usage or prices.",
        value_text(field(report, "host")),
        if field(report, "since").is_null() || field(report, "since") == "" {
            String::new()
        } else {
            format!(" since {}", value_text(field(report, "since")))
        }
    )];
    let usage: Vec<_> = array(field(report, "usage")).collect();
    let usage_rows: Vec<Vec<Option<String>>> = usage
        .iter()
        .map(|row| {
            let mut cells = vec![
                Some(text_or(field(row, "provider"), "-")),
                Some(text_or(field(row, "adapter"), "None")),
                Some(text_or(field(row, "model"), "None")),
                Some(value_text(field(row, "rows"))),
            ];
            cells.extend(
                TOKEN_FIELDS
                    .iter()
                    .map(|name| compact_json(field(row, name))),
            );
            cells.push(
                field(row, "cache_read_share")
                    .as_f64()
                    .map(|v| format!("{:.0}%", v * 100.0)),
            );
            cells.push(
                field(row, "estimated_cost_usd")
                    .as_f64()
                    .map(|v| format!("{v:.2}")),
            );
            cells.push(Some(text_or(field(row, "billing"), "-")));
            cells
        })
        .collect();
    lines.extend(bounded_table(
        &[
            "provider",
            "adapter",
            "model",
            "rows",
            "input",
            "cache_read",
            "cache_write",
            "output",
            "reasoning",
            "cache%",
            "cost_usd",
            "billing",
        ]
        .map(str::to_owned),
        &usage_rows,
        Some(limit),
        &["cost_usd"],
    )?);
    if !detail && usage.len() > ROW_LIMIT {
        lines.push(format!(
            "{} more; use --detail/--json",
            usage.len() - ROW_LIMIT
        ));
    }

    let outcomes: Vec<Vec<Option<String>>> = array(field(report, "outcomes"))
        .map(|row| {
            let route = format!(
                "{}/{}{}",
                text_or(field(row, "adapter"), "None"),
                text_or(field(row, "model"), "None"),
                field(row, "effort")
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("@{s}"))
                    .unwrap_or_default()
            );
            let errors = field(row, "error_classes")
                .as_object()
                .map(|m| {
                    m.iter()
                        .map(|(k, v)| format!("{k}:{}", value_text(v)))
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "-".into());
            let cost = if field(row, "known_cost_usd").is_number()
                && field(row, "missing_cost_rows").as_u64().unwrap_or(0) == 0
            {
                field(row, "known_cost_usd")
                    .as_f64()
                    .map(|v| format!("{v:.2}"))
            } else {
                None
            };
            vec![
                Some(route),
                Some(text_or(field(row, "task_class"), "-")),
                Some(text_or(field(row, "route"), "-")),
                Some(value_text(field(row, "lanes"))),
                field(row, "success_rate")
                    .as_f64()
                    .map(|v| format!("{:.0}%", v * 100.0)),
                Some(value_text(field(row, "retries"))),
                Some(errors),
                cost,
                field(row, "cost_per_success_usd")
                    .as_f64()
                    .map(|v| format!("{v:.2}")),
                compact_json(field(row, "latency_p50_s")),
                compact_json(field(row, "latency_p90_s")),
                compact_json(field(row, "output_tokens_per_s")),
            ]
        })
        .collect();
    if !outcomes.is_empty() {
        lines.extend([
            String::new(),
            "Outcomes by route (latest attempt per lane):".into(),
        ]);
        lines.extend(bounded_table(
            &[
                "route",
                "task_class",
                "alias",
                "lanes",
                "ok",
                "retries",
                "errors",
                "cost_usd",
                "$/ok",
                "p50_s",
                "p90_s",
                "out_tok/s",
            ]
            .map(str::to_owned),
            &outcomes,
            Some(limit),
            &["cost_usd", "$/ok"],
        )?);
        if !detail && outcomes.len() > ROW_LIMIT {
            lines.push(format!(
                "{} more; use --detail/--json",
                outcomes.len() - ROW_LIMIT
            ));
        }
    }
    let last_limits: Vec<Vec<String>> = array(field(report, "last_limit_events"))
        .map(|row| {
            vec![
                text_or(field(row, "provider"), "-"),
                text_or(field(row, "account"), "unattributed"),
                value_text(field(row, "timestamp")),
                format!("{}s ago", value_text(field(row, "age_seconds"))),
            ]
        })
        .collect();
    let mut limit_rows: Vec<_> = array(field(report, "limits_per_day")).collect();
    let limit_rows = limit_rows
        .drain(limit_rows.len().saturating_sub(14)..)
        .map(|r| {
            vec![
                text_or(field(r, "day"), "-"),
                text_or(field(r, "provider"), "-"),
                value_text(field(r, "status")),
                value_text(field(r, "events")),
            ]
        })
        .collect();
    let mut sections: Vec<(&str, Vec<Vec<String>>)> = vec![
        (
            "Lanes",
            array(field(report, "lanes"))
                .map(|r| {
                    vec![
                        value_text(field(r, "status")),
                        text_or(field(r, "error_class"), "-"),
                        value_text(field(r, "lanes")),
                    ]
                })
                .collect(),
        ),
        (
            "Compactions per lane",
            array(field(report, "compactions_per_lane"))
                .map(|r| {
                    vec![
                        value_text(field(r, "lane")),
                        value_text(field(r, "compactions")),
                        text_or(field(r, "host"), "-"),
                    ]
                })
                .collect(),
        ),
        ("Limit events", limit_rows),
        (
            "Last limit events",
            last_limits.iter().take(limit).cloned().collect(),
        ),
    ];
    if detail {
        sections.push((
            "Latest quota observations",
            array(field(report, "quota_observations"))
                .map(|r| {
                    vec![
                        text_or(field(r, "provider"), "-"),
                        value_text(field(r, "window")),
                        field(r, "used_percent")
                            .as_f64()
                            .map(|v| format!("{}%", python_general(v)))
                            .unwrap_or_else(|| "None%".into()),
                        text_or(field(r, "reset_at"), "-"),
                        text_or(field(r, "timestamp"), "-"),
                    ]
                })
                .collect(),
        ));
    }
    for (title, rows) in sections {
        if !rows.is_empty() {
            lines.push(String::new());
            lines.push(format!("{title}:"));
            lines.extend(rows.into_iter().map(text_row));
        }
    }
    if !detail && last_limits.len() > ROW_LIMIT {
        lines.push(format!(
            "{} more limit events; use --detail/--json",
            last_limits.len() - ROW_LIMIT
        ));
    }
    let quota = array(field(report, "quota_observations")).count();
    if !detail && quota > 0 {
        lines.push(format!("{quota} quota observations; use --detail/--json"));
    }
    let unattributed: Vec<_> = array(field(report, "unattributed")).collect();
    let excluded = field(report, "excluded_rows").as_object();
    if !unattributed.is_empty() || excluded.is_some_and(|m| !m.is_empty()) {
        let summary_rows = unattributed
            .iter()
            .map(|r| {
                format!(
                    "{}/{} rows={}",
                    value_text(field(r, "source")),
                    text_or(field(r, "adapter"), "-"),
                    value_text(field(r, "rows"))
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let mut summary = if summary_rows.is_empty() {
            "Not attributed to a model: ".to_owned()
        } else {
            format!("Not attributed to a model: {summary_rows}")
        };
        if let Some(excluded) = excluded {
            for (name, count) in excluded {
                summary.push_str(&format!("; excluded {name}={}", value_text(count)));
            }
        }
        lines.extend([String::new(), summary]);
    }
    for host in array(field(report, "hosts")) {
        lines.push(format!(
            "host {}: {}{}",
            value_text(field(host, "host")),
            value_text(field(host, "status")),
            field(host, "reason")
                .as_str()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        ));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_lane_outcome_with_attempt_costs_and_quantiles() {
        let base = Usage {
            adapter: Some("codex".into()),
            model: Some("observed".into()),
            repository: Some("repo".into()),
            run_id: Some("run".into()),
            task_id: Some("one".into()),
            attempt: Some(1.into()),
            status: Some("failed".into()),
            error_class: Some("provider_timeout".into()),
            duration_ms: Some(30000.into()),
            provider_ms: Some(30000.into()),
            ..Usage::default()
        };
        let success = Usage {
            attempt: Some(2.into()),
            status: Some("ok".into()),
            error_class: None,
            duration_ms: Some(10000.into()),
            provider_ms: Some(10000.into()),
            output_tokens: Some(100.into()),
            ..base.clone()
        };
        let other = Usage {
            task_id: Some("two".into()),
            attempt: Some(1.into()),
            duration_ms: Some(20000.into()),
            provider_ms: Some(20000.into()),
            ..success.clone()
        };
        let rows = outcomes(&[base, success, other], &Prices::default(), false).unwrap();
        let row = &rows[0];
        assert_eq!(
            (row.lanes, row.succeeded, row.attempts, row.retries),
            (2, 2, 3, Numeric::ONE)
        );
        assert_eq!(row.latency_p50_s, Some(20.0));
        assert_eq!(row.latency_p90_s, Some(30.0));
        assert_eq!(row.output_tokens_per_s, Some(7.5));
        assert_eq!(row.cost_per_success_usd, None);
        assert!(!peer_outcome(row).unwrap().contains_key("known_cost_usd"));
    }
    #[test]
    fn identical_lane_ids_in_distinct_repositories_remain_distinct() {
        let a = Usage {
            repository: Some("a".into()),
            status: Some("ok".into()),
            ..Usage::default()
        };
        let b = Usage {
            repository: Some("b".into()),
            ..a.clone()
        };
        assert_eq!(
            outcomes(&[a, b], &Prices::default(), true).unwrap()[0].lanes,
            2
        );
    }
    #[test]
    fn compact_and_unicode_tables() {
        assert_eq!(compact(None), "-");
        assert_eq!(compact(Some(1000.0)), "1k");
        assert_eq!(compact(Some(1250.0)), "1.25k");
        assert_eq!(compact(Some(1.25)), "1.25");
        let headers = vec!["id".into(), "cost".into(), "absent".into()];
        let rows = vec![
            vec![Some("é".into()), None, None],
            vec![Some("long".into()), None, None],
        ];
        assert_eq!(
            bounded_table(&headers, &rows, Some(1), &["cost"]).unwrap(),
            vec!["id cost", "é  -"]
        );
    }
    #[test]
    fn percentile_uses_nearest_rank_not_interpolation() {
        assert_eq!(percentile(&[1.0, 4.0, 2.0, 3.0], 0.5), Some(2.0));
        assert_eq!(percentile(&[], 0.9), None);
    }
}
