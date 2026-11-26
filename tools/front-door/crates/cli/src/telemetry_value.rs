//! `telemetry value` and `telemetry cost`: plan value and cost, each read from
//! the usage ledger. Nothing here prices anything; the ledger rows carry the
//! cost settled with the attempt.
use crate::Target;
use crate::run::{Edge, Reply};
use crate::telemetry_args::{CostBy, TelemetryCost, TelemetryValue};
use provenant_fabric_runtime::hosts::plain_text;
use provenant_quota::{HistoryRecord, MarkerKind};
use provenant_records::{CommandError, CommandResult, Outcome as Status};
use provenant_run::ErrorKind;
use provenant_telemetry::ledger::{self, Row, report};
use provenant_telemetry::value::{self, CreditPoint, Reading as ValueReading};
use serde_json::{Value, json};
use std::collections::BTreeMap;

type Failure = (ErrorKind, String);
type Output = Result<(Value, String), Failure>;

const DEFAULT_DAYS: i64 = 30;
const DAY: i64 = 86_400;

fn reply(command: &str, output: Output) -> Reply {
    match output {
        Ok((data, human)) => Reply {
            result: CommandResult::new(command, Status::Ok, data),
            human,
            summary: None,
            read_only: true,
        },
        Err((kind, message)) => {
            let repair: String = match kind {
                ErrorKind::Io => {
                    "Verify the state path named in the error is readable, then rerun the command."
                        .into()
                }
                ErrorKind::NotFound => {
                    "Launch the target with `provenant run` first, then rerun the command.".into()
                }
                ErrorKind::Invalid => "Wait for the lane's usage row to be written when it settles, then rerun the command.".into(),
                _ => kind.diagnostic().next.unwrap_or_else(|| {
                    format!(
                        "Resolve the {} condition named in the error, then retry the command.",
                        kind.code()
                    )
                }),
            };
            let mut result = CommandResult::new(command, Status::Error, Value::Null);
            result.error = Some(CommandError::new(kind.code(), message, None, repair));
            Reply {
                result,
                human: String::new(),
                summary: None,
                read_only: true,
            }
        }
    }
}

fn io(error: impl std::fmt::Display) -> Failure {
    (ErrorKind::Io, error.to_string())
}

fn read(edge: &Edge<'_>) -> Result<ledger::Ledger, Failure> {
    ledger::read(edge.roots.state.as_path()).map_err(io)
}

fn seconds(row: &Row) -> i64 {
    row.finished.as_timestamp().as_second()
}

/// A ledger line this build cannot read is named, never skipped.
fn unrecognised(ledger: &ledger::Ledger, data: &mut Value, human: &mut String) {
    if ledger.unrecognised.is_empty() {
        return;
    }
    for item in ledger.unrecognised.iter().take(3) {
        human.push_str(&format!(
            "unrecognised ledger line {}\n",
            plain_text(&item.to_string(), 200)
        ));
    }
    data["unrecognised_ledger_lines"] = json!(ledger.unrecognised.len());
}

fn money(totals: &report::Totals) -> String {
    match (&totals.usd, totals.complete) {
        (Some(usd), true) => format!("${usd}"),
        (Some(usd), false) => format!("at least ${usd}"),
        (None, _) => "unknown".into(),
    }
}

fn coverage(totals: &report::Totals) -> String {
    let mut text = format!(
        "{} of {} attempts priced",
        totals.priced_attempts, totals.attempts
    );
    if totals.unattributed_attempts > 0 {
        text.push_str(&format!(
            ", {} without an account",
            totals.unattributed_attempts
        ));
    }
    text
}

fn usd(microdollars: u64) -> String {
    format!(
        "${}.{:06}",
        microdollars / 1_000_000,
        microdollars % 1_000_000
    )
}

fn percent(basis_points: u64) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

fn named(reason: Option<value::Unavailable>) -> String {
    reason.map_or_else(|| "unknown".to_owned(), |kind| kind.name().to_owned())
}

fn price_per_window_usd(
    plan: Option<&provenant_quota::PlanPrice>,
    window_seconds: Option<u64>,
) -> (Option<u64>, Option<value::Unavailable>) {
    let Some(plan) = plan else {
        return (None, None);
    };
    let monthly_usd = if plan.currency == "USD" {
        if plan.price > 0.0 {
            Some(plan.price)
        } else {
            plan.notional_usd
        }
    } else if plan.notional_usd.is_some() {
        plan.notional_usd
    } else {
        return (None, Some(value::Unavailable::UnsupportedCurrency));
    };
    let Some(length) = window_seconds else {
        return (None, None);
    };
    let Some(monthly_usd) = monthly_usd.filter(|amount| amount.is_finite() && *amount > 0.0) else {
        return (None, None);
    };
    (
        Some((monthly_usd * 1_000_000.0 * length as f64 / 2_592_000.0).round() as u64),
        None,
    )
}

fn window_cost_rows<'a>(
    rows: &'a [Row],
    provider: &str,
    identity_key: &str,
    bounds: Option<(i64, i64)>,
) -> Vec<&'a Row> {
    let Some((start, end)) = bounds else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| attributed(row, provider, identity_key))
        .filter(|row| (start..=end).contains(&seconds(row)))
        .collect()
}

/// Credit balances arrive as decimal strings (`9.50`) or `unlimited`.
fn credit_balance_microdollars(text: &str) -> Option<u64> {
    let trimmed = text.trim();
    let amount = trimmed
        .strip_prefix('$')
        .unwrap_or(trimmed)
        .parse::<f64>()
        .ok()?;
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    Some((amount * 1_000_000.0).round() as u64)
}

// ---- cost ----

fn attributed(row: &Row, provider: &str, identity_key: &str) -> bool {
    row.provider.as_deref() == Some(provider) && row.identity.account_key() == identity_key
}

/// Whether `row` lies under the target: a graph, a node, or the one attempt
/// whose id `attempt` names.
fn selects(target: &Target, attempt: Option<&str>, row: &Row) -> bool {
    row.graph == target.name
        && target.node.as_deref().is_none_or(|node| row.node == node)
        && attempt.is_none_or(|attempt| row.attempt == attempt)
}

pub fn cost(command: TelemetryCost, edge: &Edge<'_>) -> Reply {
    reply("telemetry cost", cost_inner(&command, edge))
}

fn cost_inner(command: &TelemetryCost, edge: &Edge<'_>) -> Output {
    // Only `#N` needs the checkpoint: a graph or node matches the ledger's
    // own identities, so its cost stays readable after the graph is collected.
    let attempt = match &command.target {
        Some(target) if target.attempt.is_some() => {
            let resolved = crate::run::resolve_target(edge, target, crate::target::Want::Work)
                .map_err(|fail| (fail.kind, fail.message))?;
            let reference = resolved
                .attempt_ref
                .ok_or_else(|| (ErrorKind::NotFound, format!("{target} has not launched")))?;
            reference.rsplit('/').next().map(str::to_owned)
        }
        _ => None,
    };
    let ledger = read(edge)?;
    let since = command.since.map(|time| time.as_second());
    let rows: Vec<&Row> = ledger
        .rows
        .iter()
        .filter(|row| since.is_none_or(|since| seconds(row) >= since))
        .filter(|row| {
            command
                .target
                .as_ref()
                .is_none_or(|target| selects(target, attempt.as_deref(), row))
        })
        .collect();
    if rows.is_empty()
        && let Some(target) = &command.target
    {
        let shown = target.to_string();
        return Err((
            ErrorKind::Invalid,
            format!(
                "no usage ledger row matches {}; a lane's row is written when it settles",
                plain_text(&shown, 80)
            ),
        ));
    }
    let totals = report::totals(rows.iter().copied()).map_err(io)?;
    let mut human = format!("{}: {}\n", coverage(&totals), money(&totals));
    let mut data = json!({"totals": totals});
    if totals.unpriced_attempts > 0 {
        human.push_str("unpriced attempts are not counted as zero; the cost is a lower bound\n");
    }
    if let Some(by) = command.by {
        let by = match by {
            CostBy::Route => report::By::Route,
            CostBy::Account => report::By::Account,
            CostBy::Model => report::By::Model,
            CostBy::Graph => report::By::Graph,
            CostBy::Node => report::By::Node,
        };
        let groups = report::group(rows.iter().copied(), by).map_err(io)?;
        for group in &groups {
            human.push_str(&format!(
                "{}: {} ({})\n",
                plain_text(&group.key, 128),
                money(&group.totals),
                coverage(&group.totals)
            ));
        }
        data["groups"] = json!(groups);
    }
    unrecognised(&ledger, &mut data, &mut human);
    Ok((data, human))
}

// ---- value ----

pub fn value(command: TelemetryValue, edge: &Edge<'_>) -> Reply {
    reply("telemetry value", value_inner(&command, edge))
}

fn value_inner(command: &TelemetryValue, edge: &Edge<'_>) -> Output {
    use provenant_core::Clock;
    let now = provenant_os::clock::SystemClock::new()
        .wall_now()
        .map_err(io)?
        .as_timestamp()
        .as_second();
    let since = command
        .since
        .map_or(now - DEFAULT_DAYS * DAY, |time| time.as_second());
    let ledger = read(edge)?;
    let in_window: Vec<&Row> = ledger
        .rows
        .iter()
        .filter(|row| seconds(row) >= since)
        .collect();
    let (accepted_json, accepted_line) = accepted_section(edge, since, &in_window)?;
    let config = crate::config_reads::read_for(edge.roots, "telemetry").map_err(io)?;
    // Quota history is an optional source: a failed read names its cause and
    // degrades the value fields, rather than refusing the whole report.
    let (history, history_unavailable) = match crate::quota_history::read(
        edge.roots.state.as_path(),
        &crate::quota_history::Query {
            account: command.account.clone(),
            since: None,
            until: None,
        },
    ) {
        Ok(history) => (history, None),
        Err(error) => (
            Vec::new(),
            Some(crate::run::history_unavailable_cause(&error)),
        ),
    };
    let mut series: BTreeMap<(String, String), Vec<HistoryRecord>> = BTreeMap::new();
    for record in history {
        series
            .entry((record.provider.clone(), record.label.clone()))
            .or_default()
            .push(record);
    }
    let mut rows = Vec::new();
    let mut human = format!(
        "plan value: API-equivalent usage since {}\n",
        jiff::Timestamp::from_second(since).map_err(io)?
    );
    if let Some(cause) = &history_unavailable {
        human.push_str(&format!(
            "quota history: unavailable ({})\n",
            plain_text(cause, 256)
        ));
    }
    for ((provider, label), records) in &series {
        let Some(reading) = records.last() else {
            continue;
        };
        let Some((entry_key, entry)) = config.domains.accounts.entries.iter().find(|(_, entry)| {
            entry.adapter == *provider && entry.label == *label && entry.key == reading.account_key
        }) else {
            continue;
        };
        let identity_key = format!("{provider}/{}", reading.account_key);
        let used = report::totals(
            in_window
                .iter()
                .copied()
                .filter(|row| attributed(row, provider, &identity_key)),
        )
        .map_err(io)?;
        let price = provenant_quota::price_in_force(&entry.plans, reading.observed_unix_seconds);
        let price_source = crate::config_reads::setting_source(
            &config.origins,
            &format!("domains.accounts.entries.{entry_key}.plans"),
        );
        let window_source = command.window.as_deref().map_or_else(
            || "compiled defaults (all windows)".to_owned(),
            |name| format!("--window {name}"),
        );
        let mut window_lines = Vec::new();
        let windows: Vec<Value> = reading
            .windows
            .iter()
            .filter(|w| command.window.as_deref().is_none_or(|name| w.pool == name))
            .map(|w| -> Result<Value, Failure> {
                let window_readings: Vec<ValueReading> = records
                    .iter()
                    .map(|record| {
                        let used = record
                            .windows
                            .iter()
                            .find(|candidate| candidate.pool == w.pool)
                            .map(|candidate| candidate.used_basis_points);
                        ValueReading {
                            observed_unix_seconds: record.observed_unix_seconds,
                            used_basis_points: used,
                            reset: record.markers.iter().any(|marker| {
                                marker.kind == MarkerKind::Reset
                                    && marker.pool.as_deref() == Some(w.pool.as_str())
                            }),
                        }
                    })
                    .collect();
                let unused = value::unused_at_reset(&window_readings);
                let at_reset: Vec<u64> = window_readings
                    .windows(2)
                    .filter(|pair| pair[1].reset)
                    .filter_map(|pair| pair[0].used_basis_points)
                    .collect();
                let trend = value::trend(&at_reset);
                // A report cutoff cannot truncate the denominator's quota cycle.
                let bounds = w.window_seconds.zip(w.resets_unix_seconds).map(|(len, reset)| {
                    (
                        reset.saturating_sub(i64::try_from(len).unwrap_or(i64::MAX)),
                        reset,
                    )
                });
                let cost_basis = bounds.map_or_else(
                    || "unavailable_no_window_bounds".to_owned(),
                    |(start, end)| format!("api_equivalent_{start}_to_{end}"),
                );
                let cost_rows = window_cost_rows(&ledger.rows, provider, &identity_key, bounds);
                let totals = report::totals(cost_rows.iter().copied()).map_err(io)?;
                let (price_per_window, price_unavailable) =
                    price_per_window_usd(price, w.window_seconds);
                let mut window_value = value::window_value(
                    totals.microdollars,
                    w.used_basis_points,
                    price_per_window,
                    totals.complete,
                    bounds.is_some(),
                );
                if window_value.cost_per_full_window_microdollars.is_some()
                    && price_unavailable.is_some()
                {
                    window_value.unavailable = price_unavailable;
                }
                let cost_text = match window_value.cost_per_full_window_microdollars {
                    Some(micros) => usd(micros),
                    None => format!("unavailable ({})", named(window_value.unavailable)),
                };
                let multiple_text = match window_value.value_multiple_basis_points {
                    Some(bp) => format!("{}.{:02}x", bp / 10_000, (bp % 10_000) / 100),
                    None => format!("unavailable ({})", named(window_value.unavailable)),
                };
                let unused_text = unused.as_ref().map_or_else(
                    || "unavailable (no_readings)".to_owned(),
                    |share| match share.unused_basis_points {
                        Some(bp) => format!(
                            "{} ({}, {} samples, observed {}{})",
                            percent(bp),
                            share.basis,
                            share.samples,
                            share.observed_unix_seconds.map_or_else(
                                || "unknown".to_owned(),
                                |at| jiff::Timestamp::from_second(at)
                                    .map_or_else(|_| "unknown".to_owned(), |time| time.to_string())
                            ),
                            if share.below_half_used {
                                ", below half used"
                            } else {
                                ""
                            }
                        ),
                        None => format!("unavailable ({})", named(share.unavailable)),
                    },
                );
                let trend_text = trend.direction.map_or_else(
                    || format!("unavailable ({})", named(trend.unavailable)),
                    |direction| direction.to_owned(),
                );
                window_lines.push(format!(
                    "  {}: cost per full window {} ({}), value multiple {}, unused at reset {}, usage-at-reset trend {} ({} windows)\n",
                    plain_text(&w.pool, 128),
                    cost_text,
                    coverage(&totals),
                    multiple_text,
                    unused_text,
                    trend_text,
                    trend.samples,
                ));
                if let Some(bound) = window_value.lower_bound_microdollars {
                    window_lines.push(format!(
                        "    unpriced bucket: cost is a lower bound of {} per full window\n",
                        usd(bound)
                    ));
                }
                Ok(json!({
                    "pool": w.pool,
                    "used_basis_points": w.used_basis_points,
                    "window_seconds": w.window_seconds,
                    "resets_unix_seconds": w.resets_unix_seconds,
                    "samples": window_readings.len(),
                    "cost_basis": cost_basis,
                    "usage": totals,
                    "coverage": {
                        "attempts": totals.attempts,
                        "priced_attempts": totals.priced_attempts,
                        "unpriced_attempts": totals.unpriced_attempts,
                        "unattributed_attempts": totals.unattributed_attempts,
                    },
                    "cost_per_full_window": window_value,
                    "unused_share_at_reset": unused,
                    "trend": {"basis": "used_basis_points_at_reset", "value": trend},
                }))
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let credit_points: Vec<CreditPoint> = records
            .iter()
            .map(|record| CreditPoint {
                observed_unix_seconds: record.observed_unix_seconds,
                balance_microdollars: record
                    .credit_balance
                    .as_deref()
                    .and_then(credit_balance_microdollars),
            })
            .collect();
        let runway = value::credit_runway(&credit_points);
        let credit_text = match (
            runway.draw_down_microdollars_per_day,
            runway.days_to_exhaustion,
        ) {
            (Some(rate), Some(days)) => format!(
                "credit: draw-down {}/day, exhaustion in {days} days ({} readings)\n",
                usd(rate),
                runway.samples
            ),
            _ => format!(
                "credit: draw-down unavailable ({}) ({} readings)\n",
                named(runway.unavailable),
                runway.samples
            ),
        };
        human.push_str(&format!(
            "{}: plan {}, API-equivalent {} ({}), {} quota windows\n",
            plain_text(label, 128),
            price.map_or("unpriced", |p| p.name.as_str()),
            money(&used),
            coverage(&used),
            windows.len(),
        ));
        for line in &window_lines {
            human.push_str(line);
        }
        human.push_str(&credit_text);
        rows.push(json!({
            "account": label,
            "provider": provider,
            "plan": reading.plan.as_deref().or(price.map(|p| p.name.as_str())),
            "monthly_price": price.map(|p| json!({"amount": p.price, "currency": p.currency})),
            "price_source": price_source,
            "window_source": window_source,
            "observed_unix_seconds": reading.observed_unix_seconds,
            "quota_windows": windows,
            "usage": used,
            "coverage": {
                "attempts": used.attempts,
                "priced_attempts": used.priced_attempts,
                "unpriced_attempts": used.unpriced_attempts,
            },
            "credit_runway": runway,
        }));
    }
    if rows.is_empty() {
        human.push_str("no account has a quota reading and a configured plan\n");
    }
    human.push_str(&accepted_line);
    // Usage that cannot be tied to an account with a reading is stated, not dropped.
    let unattributed = report::totals(
        in_window
            .iter()
            .copied()
            .filter(|row| row.identity.is_unknown()),
    )
    .map_err(io)?;
    if unattributed.attempts > 0 {
        human.push_str(&format!(
            "unattributed: {} ({})\n",
            money(&unattributed),
            coverage(&unattributed)
        ));
    }
    let mut data = json!({
        "since_unix_seconds": since,
        "basis": "api_equivalent_since_vs_monthly_plan_price",
        "accounts": rows,
        "unattributed": unattributed,
        "accepted_outcomes": accepted_json,
        "quota_history": { "unavailable": history_unavailable },
    });
    unrecognised(&ledger, &mut data, &mut human);
    Ok((data, human))
}

/// Cost per accepted outcome for this project, priced from the ledger rows of
/// the attempts its accepted deliveries link to. `value` is account-level, so
/// outside a project, or when the project's store cannot be read, the section
/// says why instead of failing the whole report.
fn accepted_section(
    edge: &Edge<'_>,
    since: i64,
    in_window: &[&Row],
) -> Result<(Value, String), Failure> {
    let accepted = match crate::run::accepted_attempts(edge, since) {
        Ok(accepted) => accepted,
        Err(fail) => {
            let line = format!(
                "accepted outcomes: unavailable ({})\n",
                plain_text(&fail.message, 256)
            );
            return Ok((json!({"unavailable": fail.message}), line));
        }
    };
    let linked: Vec<&Row> = in_window
        .iter()
        .copied()
        .filter(|row| row.project == accepted.project && accepted.attempts.contains(&row.attempt))
        .collect();
    let recorded: std::collections::BTreeSet<&str> =
        linked.iter().map(|row| row.attempt.as_str()).collect();
    let unrecorded = accepted
        .attempts
        .iter()
        .filter(|attempt| !recorded.contains(attempt.as_str()))
        .count();
    let totals = report::totals(linked.iter().copied()).map_err(io)?;
    let line = format!(
        "accepted outcomes: {} (this project; {})\n",
        accepted.denominator,
        money(&totals)
    );
    let section = accepted_outcomes(
        accepted.definition,
        accepted.denominator,
        &totals,
        unrecorded,
    );
    Ok((section, line))
}

/// Cost per accepted outcome. The numerator is the complete API-equivalent
/// cost of the linked attempts, so it is absent while any of them is unpriced
/// or has no ledger row.
fn accepted_outcomes(
    definition: &str,
    denominator: usize,
    totals: &report::Totals,
    unrecorded: usize,
) -> Value {
    let numerator = totals
        .microdollars
        .filter(|_| totals.complete && unrecorded == 0);
    json!({
        "scope": "this project's finished attempts since --since; the account filter does not apply",
        "definition": definition,
        "denominator": denominator,
        "numerator_scope": "API-equivalent cost of attempts linked to accepted deliveries, including retries",
        "usage": totals,
        "unrecorded_attempts": unrecorded,
        "microdollars_per_accepted_outcome": if denominator == 0 {
            None
        } else {
            numerator.map(|microdollars| json!({"numerator": microdollars, "denominator": denominator, "unit": "microdollars"}))
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ProjectId, WallTime};
    use provenant_records::{AccountIdentity, UsageSource};
    use provenant_telemetry::ledger::{Cost, FORMAT, Tokens};

    fn row(at: i64, attempt: &str) -> Row {
        Row {
            format: FORMAT.into(),
            source: UsageSource::Ingest,
            project: ProjectId::from_bytes([1; 16]),
            graph: "graph".into(),
            node: "node".into(),
            attempt: attempt.into(),
            provider: Some("claude".into()),
            model: Some("model".into()),
            effort: None,
            identity: AccountIdentity::known("claude", "account", "epoch"),
            outcome: "succeeded".into(),
            started: None,
            finished: WallTime::from_micros(at * 1_000_000).expect("valid timestamp"),
            tokens: Tokens::default(),
            cost: Some(Cost {
                picodollars: 0,
                complete: true,
                subscription: false,
                prices: "fixture".into(),
                extra: Default::default(),
            }),
            context_band: None,
            extra: Default::default(),
        }
    }

    fn plan(price: f64, currency: &str, notional_usd: Option<f64>) -> provenant_quota::PlanPrice {
        provenant_quota::PlanPrice {
            name: "fixture".into(),
            price,
            currency: currency.into(),
            notional_usd,
            effective_from: "2026-01-01".into(),
        }
    }

    #[test]
    fn plan_value_uses_only_a_usd_price_or_explicit_usd_notional() {
        assert_eq!(
            price_per_window_usd(Some(&plan(30.0, "USD", None)), Some(2_592_000)),
            (Some(30_000_000), None)
        );
        assert_eq!(
            price_per_window_usd(Some(&plan(40.0, "AUD", None)), Some(2_592_000)),
            (None, Some(value::Unavailable::UnsupportedCurrency))
        );
        assert_eq!(
            price_per_window_usd(Some(&plan(40.0, "AUD", Some(25.0))), Some(2_592_000)),
            (Some(25_000_000), None)
        );
    }

    #[test]
    fn full_window_cost_includes_cycle_rows_earlier_than_the_report_cutoff() {
        let rows = [
            row(90, "outside"),
            row(110, "before_cutoff"),
            row(150, "after_cutoff"),
        ];
        let selected = window_cost_rows(&rows, "claude", "account", Some((100, 200)));
        assert_eq!(
            selected
                .iter()
                .map(|row| row.attempt.as_str())
                .collect::<Vec<_>>(),
            ["before_cutoff", "after_cutoff"]
        );
        assert!(window_cost_rows(&rows, "claude", "account", None).is_empty());
    }

    #[test]
    fn a_non_numeric_credit_balance_degrades_to_an_absent_reading() {
        assert_eq!(credit_balance_microdollars("9.50"), Some(9_500_000));
        assert_eq!(credit_balance_microdollars("$9.50"), Some(9_500_000));
        assert_eq!(credit_balance_microdollars("unlimited"), None);
        assert_eq!(credit_balance_microdollars("-1"), None);
        assert_eq!(credit_balance_microdollars("not a number"), None);
    }

    #[test]
    fn accepted_cost_needs_every_linked_attempt_priced_and_recorded() {
        let complete = report::Totals {
            attempts: 2,
            priced_attempts: 2,
            microdollars: Some(12),
            complete: true,
            ..report::Totals::default()
        };
        let ratio = accepted_outcomes("definition", 2, &complete, 0);
        assert_eq!(ratio["denominator"], 2);
        assert_eq!(ratio["microdollars_per_accepted_outcome"]["numerator"], 12);
        assert_eq!(ratio["microdollars_per_accepted_outcome"]["denominator"], 2);
        assert!(
            accepted_outcomes("definition", 0, &complete, 0)["microdollars_per_accepted_outcome"]
                .is_null()
        );
        assert!(
            accepted_outcomes("definition", 2, &complete, 1)["microdollars_per_accepted_outcome"]
                .is_null()
        );
        let partial = report::Totals {
            attempts: 2,
            priced_attempts: 1,
            unpriced_attempts: 1,
            microdollars: Some(5),
            complete: false,
            ..report::Totals::default()
        };
        let ratio = accepted_outcomes("definition", 2, &partial, 0);
        assert!(ratio["microdollars_per_accepted_outcome"].is_null());
        assert_eq!(ratio["usage"]["microdollars"], 5);
    }
}
