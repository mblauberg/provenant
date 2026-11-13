//! `telemetry value` and `telemetry cost`: plan value and cost, each read from
//! the usage ledger. Nothing here prices anything; the ledger rows carry the
//! cost settled with the attempt.
use crate::Target;
use crate::run::{Edge, Reply};
use crate::telemetry_args::{CostBy, TelemetryCost, TelemetryValue};
use provenant_fabric_runtime::hosts::plain_text;
use provenant_records::{CommandError, CommandResult, Outcome as Status};
use provenant_run::ErrorKind;
use provenant_telemetry::ledger::{self, Row, report};
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
            let mut result = CommandResult::new(command, Status::Error, Value::Null);
            result.error = Some(CommandError {
                diagnostic: None,
                code: kind.code().into(),
                message,
                cause: None,
                repair: None,
            });
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

// ---- cost ----

/// Whether `row` is the target's attempt, task or run, or a graph or node of
/// that name. Historical ledger rows retain their prior labels as opaque text.
fn selects(target: &Target, row: &Row) -> bool {
    let prefix = |held: Option<&str>, part: &str| held.is_some_and(|held| held.starts_with(part));
    let (run, task) = (row.labels.run.as_deref(), row.labels.task.as_deref());
    if target.run.is_some() || target.task.is_some() || target.attempt.is_some() {
        return target
            .run
            .is_none_or(|id| run == Some(id.to_string().as_str()))
            && target
                .task
                .is_none_or(|id| task == Some(id.to_string().as_str()))
            && target
                .attempt
                .is_none_or(|id| row.attempt == id.to_string());
    }
    let Some(selection) = target.selection.as_deref() else {
        return true;
    };
    let parts: Vec<&str> = selection.split('/').collect();
    let attempt = Some(row.attempt.as_str());
    match parts[..] {
        [one] => {
            prefix(attempt, one)
                || prefix(run, one)
                || prefix(task, one)
                || row.graph.as_deref() == Some(one)
                || row.node.as_deref() == Some(one)
        }
        [first, second] => {
            (row.graph.as_deref() == Some(first) && row.node.as_deref() == Some(second))
                || (prefix(run, first) && (prefix(task, second) || prefix(attempt, second)))
        }
        [first, second, third] => {
            prefix(run, first) && prefix(task, second) && prefix(attempt, third)
        }
        _ => false,
    }
}

pub fn cost(command: TelemetryCost, edge: &Edge<'_>) -> Reply {
    reply("telemetry cost", cost_inner(&command, edge))
}

fn cost_inner(command: &TelemetryCost, edge: &Edge<'_>) -> Output {
    let ledger = read(edge)?;
    let since = command.since.map(|time| time.as_second());
    let rows: Vec<&Row> = ledger
        .rows
        .iter()
        .filter(|row| since.is_none_or(|since| seconds(row) >= since))
        .filter(|row| command.target.as_ref().is_none_or(|t| selects(t, row)))
        .collect();
    if rows.is_empty()
        && let Some(target) = &command.target
    {
        let shown = target.selection.as_deref().unwrap_or("the given IDs");
        return Err((
            ErrorKind::Invalid,
            format!(
                "no usage ledger row matches {}; a lane's row is written when it settles",
                plain_text(shown, 80)
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
    let config =
        provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS).map_err(io)?;
    let history = crate::quota_history::read(
        edge.roots.state.as_path(),
        &crate::quota_history::Query {
            account: command.account.clone(),
            since: None,
            until: None,
        },
    )
    .map_err(|error| (ErrorKind::Io, crate::run::history_unavailable_cause(&error)))?;
    // The latest reading of each account, whatever its window.
    let mut latest = BTreeMap::new();
    for record in history {
        latest.insert((record.provider.clone(), record.label.clone()), record);
    }
    let mut rows = Vec::new();
    let mut human = format!(
        "plan value: API-equivalent usage since {}\n",
        jiff::Timestamp::from_second(since).map_err(io)?
    );
    for ((provider, label), reading) in &latest {
        let Some(entry) = config.domains.accounts.entries.values().find(|entry| {
            entry.adapter == *provider && entry.label == *label && entry.key == reading.account_key
        }) else {
            continue;
        };
        let used = report::totals(in_window.iter().copied().filter(|row| {
            row.provider.as_deref() == Some(provider) && row.account.as_deref() == Some(label)
        }))
        .map_err(io)?;
        let price = provenant_quota::price_in_force(&entry.plans, reading.observed_unix_seconds);
        let windows: Vec<_> = reading
            .windows
            .iter()
            .filter(|w| command.window.as_deref().is_none_or(|name| w.pool == name))
            .map(|w| {
                json!({
                    "pool": w.pool,
                    "used_basis_points": w.used_basis_points,
                    "window_seconds": w.window_seconds,
                })
            })
            .collect();
        human.push_str(&format!(
            "{}: plan {}, API-equivalent {} ({}), {} quota windows\n",
            plain_text(label, 128),
            price.map_or("unpriced", |p| p.name.as_str()),
            money(&used),
            coverage(&used),
            windows.len(),
        ));
        rows.push(json!({
            "account": label,
            "provider": provider,
            "plan": reading.plan.as_deref().or(price.map(|p| p.name.as_str())),
            "monthly_price": price.map(|p| json!({"amount": p.price, "currency": p.currency})),
            "observed_unix_seconds": reading.observed_unix_seconds,
            "quota_windows": windows,
            "usage": used,
            "coverage": {
                "attempts": used.attempts,
                "priced_attempts": used.priced_attempts,
                "unpriced_attempts": used.unpriced_attempts,
            },
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
            .filter(|row| row.account.is_none()),
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
