//! `telemetry mileage`: quotas as money. Readings come from the quota history
//! the refresh already wrote; costs come from ingested native usage priced by
//! the reviewed table. Nothing here calls a provider or reads a credential.
use crate::Mileage;
use crate::run::{Edge, Reply};
use provenant_records::{CommandError, CommandResult, Outcome as Status};
use provenant_run::ErrorKind;
use provenant_telemetry::ingest;
use provenant_telemetry::mileage::{self, Effective, Observation, Reading, Series};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const SCHEMA_VERSION: u64 = 1;
const DAY: i64 = 86_400;

pub fn run(command: Mileage, edge: &Edge<'_>) -> Reply {
    match inner(&command, edge) {
        Ok((data, human)) => Reply {
            result: CommandResult::new("telemetry mileage", Status::Ok, data),
            human,
            summary: None,
            read_only: true,
        },
        Err((kind, message)) => {
            let mut result = CommandResult::new("telemetry mileage", Status::Error, Value::Null);
            result.error = Some(CommandError {
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

type Failure = (ErrorKind, String);

fn prices(assets: &Path) -> Option<provenant_telemetry::PriceTable> {
    let text = std::fs::read_to_string(assets.join("config/telemetry-prices.toml")).ok()?;
    let configured =
        toml_edit::de::from_str::<provenant_telemetry::ConfiguredPrices>(&text).ok()?;
    provenant_telemetry::validate_prices(&configured).ok()
}

fn microdollars(numerator: u128) -> Option<u64> {
    u64::try_from(numerator.checked_add(500_000)? / 1_000_000).ok()
}

#[derive(Default)]
struct Tally {
    usage: [u64; 5],
    numerator: [u128; 5],
    /// Tokens whose price is known, by bucket.
    priced_usage: [u64; 5],
    models: BTreeMap<String, [u64; 5]>,
    present: bool,
    complete: bool,
}

impl Tally {
    fn new() -> Self {
        Self {
            complete: true,
            ..Self::default()
        }
    }

    fn add(&mut self, row: &ingest::Row, prices: Option<&provenant_telemetry::PriceTable>) {
        self.present = true;
        let rates = prices.and_then(|prices| mileage::api_rates(prices, &row.provider, &row.model));
        let buckets = row.buckets();
        let entry = self.models.entry(row.model.clone()).or_default();
        for (index, count) in buckets.into_iter().enumerate() {
            self.usage[index] = self.usage[index].saturating_add(count);
            entry[index] = entry[index].saturating_add(count);
            match rates.as_ref().and_then(|rates| rates[index]) {
                Some(rate) if count > 0 => {
                    self.priced_usage[index] = self.priced_usage[index].saturating_add(count);
                    self.numerator[index] =
                        self.numerator[index].saturating_add(u128::from(count) * u128::from(rate));
                }
                Some(_) => {}
                None if count > 0 => self.complete = false,
                None => {}
            }
        }
    }

    /// API-equivalent cost, or none when the interval has no rows or an
    /// unpriced bucket.
    fn cost(&self) -> Option<u64> {
        if !self.present || !self.complete {
            return None;
        }
        microdollars(self.numerator.iter().sum())
    }

    fn priced(&self) -> bool {
        self.present && self.complete
    }
}

/// An account's ingested rows with their Unix seconds, oldest first.
type Timed<'a> = [(i64, &'a ingest::Row)];

/// Usage stamped in `(after, until]`. Rows are sorted, so the bounds are found
/// by binary search and each row is visited once per interval.
fn interval(
    rows: &Timed<'_>,
    after: i64,
    until: i64,
    prices: Option<&provenant_telemetry::PriceTable>,
) -> Tally {
    let start = rows.partition_point(|(at, _)| *at <= after);
    let end = rows.partition_point(|(at, _)| *at <= until).max(start);
    let mut tally = Tally::new();
    for (_, row) in &rows[start..end] {
        tally.add(row, prices);
    }
    tally
}

fn price_of(
    entries: &BTreeMap<String, provenant_providers::accounts::Registration>,
    provider: &str,
    key: &str,
    label: &str,
    at: i64,
) -> Option<u64> {
    let entry = entries
        .values()
        .find(|entry| entry.adapter == provider && entry.key == key)
        .or_else(|| {
            entries
                .values()
                .find(|entry| entry.adapter == provider && entry.label == label)
        })?;
    let plan = provenant_quota::price_in_force(&entry.plans, at)?;
    let microdollars = (plan.price * 1_000_000.0).round();
    (microdollars.is_finite() && (0.0..=u64::MAX as f64).contains(&microdollars))
        .then_some(microdollars as u64)
}

fn api_rates_of(tally: &Tally) -> [Option<u64>; 5] {
    std::array::from_fn(|index| {
        let tokens = tally.priced_usage[index];
        (tokens > 0)
            .then(|| tally.numerator[index] / u128::from(tokens))
            .and_then(|rate| u64::try_from(rate).ok())
    })
}

fn human_money(value: &Option<mileage::Money>) -> String {
    value
        .as_ref()
        .map(|money| format!("${}", money.usd))
        .unwrap_or_else(|| "unknown".into())
}

fn inner(command: &Mileage, edge: &Edge<'_>) -> Result<(Value, String), Failure> {
    let config = provenant_config::read_with_domains(edge.roots, &crate::domains::SECTIONS)
        .map_err(|error| (ErrorKind::Io, error.to_string()))?;
    let accounts = &config.domains.accounts.entries;
    let history = crate::quota_history::read(
        edge.roots.state.as_path(),
        &crate::quota_history::Query {
            account: command.account.clone(),
            since: command.since.map(|time| time.as_second()),
            until: None,
        },
    )
    .map_err(|error| (ErrorKind::Io, error.to_string()))?;
    let rows = ingest::read_rows(
        edge.roots,
        &ingest::RowFilter {
            account: None,
            model: command.model.clone(),
            since: command.since.map(|time| time.as_second()),
            until: None,
        },
    )
    .map_err(|error| (ErrorKind::Io, error.to_string()))?;
    let table = prices(edge.assets);
    let options = mileage::Options {
        min_runs: command.min_runs,
        fall_percent: command.fall_percent,
        recent_days: command.recent_days,
        ..mileage::Options::default()
    };
    // One series per account and window; readings stay in observed order.
    let mut grouped: BTreeMap<(String, String, String), Vec<&provenant_quota::HistoryRecord>> =
        BTreeMap::new();
    for record in &history {
        grouped
            .entry((
                record.account_key.clone(),
                record.provider.clone(),
                record.label.clone(),
            ))
            .or_default()
            .push(record);
    }
    // Each account's rows are timed once, not once per reading and window.
    let mut timed_by_account: BTreeMap<&str, Vec<(i64, &ingest::Row)>> = BTreeMap::new();
    for row in &rows {
        if let Some(at) = row.unix_seconds() {
            timed_by_account
                .entry(row.account.as_str())
                .or_default()
                .push((at, row));
        }
    }
    let mut reports = Vec::new();
    let mut human = String::new();
    for ((key, provider, label), records) in &grouped {
        let account_rows: &Timed<'_> = timed_by_account
            .get(label.as_str())
            .map_or(&[][..], Vec::as_slice);
        let latest = records
            .iter()
            .map(|record| record.observed_unix_seconds)
            .max()
            .unwrap_or(0);
        let mut windows = BTreeMap::new();
        for record in records {
            for window in &record.windows {
                windows
                    .entry(window.pool.clone())
                    .or_insert_with(Vec::new)
                    .push((record, window));
            }
        }
        let mut window_reports = Vec::new();
        let mut fits = Vec::new();
        for (pool, mut readings) in windows {
            if command
                .window
                .as_ref()
                .is_some_and(|selected| *selected != pool)
            {
                continue;
            }
            readings.sort_by_key(|(record, _)| record.observed_unix_seconds);
            let mut series = Series {
                account_key: key.clone(),
                label: label.clone(),
                provider: provider.clone(),
                window: pool.clone(),
                window_seconds: readings
                    .first()
                    .and_then(|(_, window)| window.window_seconds),
                readings: Vec::new(),
            };
            let mut observations: BTreeMap<String, Vec<Observation>> = BTreeMap::new();
            for (record, window) in &readings {
                let at = record.observed_unix_seconds;
                let previous = series
                    .readings
                    .last()
                    .map_or(at, |reading| reading.observed_unix_seconds);
                let tally = interval(account_rows, previous, at, table.as_ref());
                let reading = Reading {
                    observed_unix_seconds: at,
                    used_basis_points: window.used_basis_points,
                    resets_unix_seconds: window.resets_unix_seconds,
                    plan: record.plan.clone(),
                    tier: record.tier.clone(),
                    cost_microdollars: tally.cost(),
                    usage: tally.usage,
                    priced: tally.priced(),
                };
                if let Some(prev) = series.readings.last()
                    && mileage::continues(prev, &reading)
                {
                    let delta = reading.used_basis_points - prev.used_basis_points;
                    for (model, tokens) in &tally.models {
                        observations
                            .entry(model.clone())
                            .or_default()
                            .push((*tokens, delta));
                    }
                }
                series.readings.push(reading);
            }
            let report = mileage::analyse(&series, options);
            let effective = effective(
                key,
                provider,
                label,
                latest,
                account_rows,
                table.as_ref(),
                &report.mileage.capacity,
                report.window_seconds,
                series
                    .readings
                    .last()
                    .map(|reading| reading.used_basis_points),
                report.mileage.n,
                command.recent_days,
                accounts,
            );
            for (model, observations) in observations {
                if let Some(fit) = mileage::fit(&observations, command.min_runs) {
                    fits.push(json!({
                        "account": label, "window": pool, "model": model,
                        "n": fit.n, "r_squared": fit.r_squared,
                        "weights_basis_points_per_million_tokens": fit.weights_basis_points_per_million_tokens,
                    }));
                }
            }
            human.push_str(&human_series(&report));
            human.push_str(&human_effective(&effective));
            window_reports.push(json!({
                "window": report.window,
                "mileage_usd_per_percent": report.mileage.mileage.as_ref().map(|m| m.usd.clone()),
                "capacity_usd_per_full_window": report.mileage.capacity.as_ref().map(|m| m.usd.clone()),
                "mileage_microdollars": report.mileage.mileage.as_ref().map(|m| m.microdollars),
                "capacity_microdollars": report.mileage.capacity.as_ref().map(|m| m.microdollars),
                "n": report.mileage.n,
                "priced_segments": report.mileage.priced,
                "priced_share_basis_points": report.mileage.priced_share_basis_points,
                "runs": report.runs,
                "step_change": report.step_change,
                "upgrade": report.upgrade,
                "effective": effective,
            }));
        }
        reports.push(json!({
            "account_key": key, "label": label, "provider": provider,
            "price_usd": price_of(accounts, provider, key, label, latest)
                .map(mileage::Money::new),
            "windows": window_reports,
            "fits": fits,
        }));
    }
    let data = json!({
        "schema_version": SCHEMA_VERSION,
        "generated_from": {
            "readings": history.len(),
            "ingested_messages": rows.len(),
            "prices": table.is_some(),
        },
        "accounts": reports,
    });
    let bytes = serde_json::to_vec(&data).map_err(|error| (ErrorKind::Io, error.to_string()))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err((ErrorKind::Invalid, "the mileage report is too large".into()));
    }
    if reports.is_empty() {
        human.push_str("No quota readings or ingested usage in this window.\n");
    }
    Ok((data, human))
}

fn human_series(report: &mileage::SeriesReport) -> String {
    let mut human = format!(
        "{} [{}] {}: {} per point, {} per window (n={}, priced {}%)\n",
        report.label.escape_debug(),
        report.provider.escape_debug(),
        report.window.escape_debug(),
        human_money(&report.mileage.mileage),
        human_money(&report.mileage.capacity),
        report.mileage.n,
        report.mileage.priced_share_basis_points.unwrap_or(0) / 100,
    );
    for run in &report.runs {
        human.push_str(&format!(
            "  run {} to {}: {} segments, {} per window{}\n",
            run.start_unix_seconds,
            run.end_unix_seconds,
            run.segments,
            human_money(&run.capacity),
            run.reset
                .map(|reset| format!(", reset {reset}"))
                .unwrap_or_default(),
        ));
    }
    let step = &report.step_change;
    human.push_str(&format!(
        "  step change: {} ({}%, recent n={}, prior n={})\n",
        step.status,
        step.percent
            .map(|percent| format!("{percent:.1}"))
            .unwrap_or_else(|| "?".into()),
        step.recent_n,
        step.prior_n,
    ));
    if let Some(upgrade) = &report.upgrade {
        human.push_str(&format!(
            "  upgrade: {} to {} is {} capacity change\n",
            upgrade.plan_from.as_deref().unwrap_or("-"),
            upgrade.plan_to.as_deref().unwrap_or("-"),
            upgrade
                .percent
                .map(|percent| format!("{percent:.1}%"))
                .unwrap_or_else(|| "an unknown".into()),
        ));
    }
    human
}

fn human_effective(effective: &Effective) -> String {
    let mut human = format!(
        "  effective: latest used {}.{:02}%, n={}, coverage {}.{:02}%\n",
        effective.latest_used_basis_points.unwrap_or(0) / 100,
        effective.latest_used_basis_points.unwrap_or(0) % 100,
        effective.n,
        effective.coverage_basis_points.unwrap_or(0) / 100,
        effective.coverage_basis_points.unwrap_or(0) % 100,
    );
    for bucket in &effective.buckets {
        if bucket.tokens == 0 && bucket.potential.is_none() {
            continue;
        }
        human.push_str(&format!(
            "    {}: {} tokens, potential {}, realised {}\n",
            bucket.bucket,
            bucket.tokens,
            human_money(&bucket.potential),
            human_money(&bucket.realised),
        ));
    }
    human.push_str(&format!("    note: {}\n", effective.note));
    human
}

#[allow(clippy::too_many_arguments)]
fn effective(
    key: &str,
    provider: &str,
    label: &str,
    latest: i64,
    rows: &Timed<'_>,
    prices: Option<&provenant_telemetry::PriceTable>,
    capacity: &Option<mileage::Money>,
    window_seconds: Option<u64>,
    latest_used: Option<u64>,
    n: u64,
    recent_days: u32,
    accounts: &BTreeMap<String, provenant_providers::accounts::Registration>,
) -> Effective {
    let period_seconds = i128::from(recent_days) * i128::from(DAY);
    // The plan price is monthly; report the effective rate for the days the
    // tokens cover, and expand one window's capacity to that whole period.
    let period_capacity = capacity.as_ref().and_then(|one_window| {
        window_seconds
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| {
                round_ratio(
                    u128::from(one_window.microdollars) * u128::try_from(period_seconds).ok()?,
                    u128::from(seconds),
                )
            })
            .map(mileage::Money::new)
    });
    let period_price = price_of(accounts, provider, key, label, latest).and_then(|monthly| {
        round_ratio(
            u128::from(monthly) * u128::try_from(period_seconds).ok()?,
            u128::try_from(30 * i128::from(DAY)).ok()?,
        )
    });
    let tally = interval(rows, latest - i64::from(recent_days) * DAY, latest, prices);
    let input = mileage::EffectiveInput {
        price_microdollars: period_price,
        usage: tally.usage,
        priced_usage: tally.priced_usage,
        api_rates: api_rates_of(&tally),
        n,
        period_days: recent_days,
    };
    mileage::effective(&input, period_capacity, latest_used)
}

fn round_ratio(numerator: u128, denominator: u128) -> Option<u64> {
    if denominator == 0 {
        return None;
    }
    u64::try_from(
        numerator
            .checked_add(denominator / 2)?
            .checked_div(denominator)?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: i64) -> ingest::Row {
        ingest::Row {
            schema_version: ingest::ROW_SCHEMA_VERSION,
            account: "work".into(),
            provider: "claude".into(),
            model: "m".into(),
            effort: None,
            uncached_input: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: 0,
            output: 1,
            timestamp: jiff::Timestamp::from_second(at)
                .expect("a valid timestamp")
                .to_string(),
            session: "s1".into(),
            message: format!("m{at}"),
            source: ingest::Source::Lane,
        }
    }

    #[test]
    fn an_interval_takes_each_row_once_in_its_half_open_bounds() {
        let rows = [0, 100, 200, 300].map(row);
        let timed: Vec<(i64, &ingest::Row)> = rows
            .iter()
            .map(|row| (row.unix_seconds().expect("timed"), row))
            .collect();
        // (after, until]: the row at `after` is out and the row at `until` is in.
        let output = |after, until| interval(&timed, after, until, None).usage[4];
        assert_eq!(output(0, 200), 2);
        assert_eq!(output(-1, 0), 1);
        assert_eq!(output(200, 200), 0);
        assert_eq!(output(-100, 1_000), 4);
        assert!(!interval(&timed, 300, 400, None).present);
        assert!(!interval(&[], 0, 100, None).present);
    }
}
