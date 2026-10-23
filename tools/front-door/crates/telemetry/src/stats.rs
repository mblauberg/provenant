//! Ledger selection and peer-safe stats projections.
//!
//! The source report accepts a telemetry module whose host, quota, cost and
//! delivery owners vary independently. This module keeps those boundaries as
//! traits and receives pricing/catalogue data as values.
use crate::{
    costs::{self, Catalogue, Exclusion, Prices, SqlCellQuery, UsageCell, UsageCellStore},
    delivery::{self, CompactionRow, Compactions, DeliveryReportRow, DeliverySummary},
    report, Error, HostIdentity, Numeric, Result, Usage,
};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Host identity is derived from configured aliases and the IDs recorded by
/// the ledger, rather than process-global caches.
pub trait HostProvider {
    fn identity(&self, ledger_ids: &BTreeSet<String>) -> Result<HostIdentity>;
    fn now(&self) -> Result<jiff::Timestamp>;
}

/// Delivery policy receives rows selected by the ledger store. The default
/// policy delegates to the delivery owner; a future owner can supply a new
/// implementation without changing report selection.
pub trait DeliveryReporter {
    fn report_deliveries(
        &self,
        rows: &[DeliveryReportRow],
        since: Option<&str>,
    ) -> Result<Vec<DeliverySummary>>;
    fn report_compactions(
        &self,
        rows: &[CompactionRow],
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<Compactions>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LedgerDeliveryReporter;

impl DeliveryReporter for LedgerDeliveryReporter {
    fn report_deliveries(
        &self,
        rows: &[DeliveryReportRow],
        since: Option<&str>,
    ) -> Result<Vec<DeliverySummary>> {
        delivery::report_deliveries_from_rows(rows, since)
    }

    fn report_compactions(
        &self,
        rows: &[CompactionRow],
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<Compactions>> {
        delivery::report(rows, since, until)
    }
}

/// Quota metadata is sanitised at the peer boundary by the quota owner. The
/// report module only chooses documented fields and applies this projection.
pub trait QuotaProjector {
    fn snapshots(&self) -> Result<Vec<Value>>;
    fn window_amounts(&self, row: &Value) -> Result<Map<String, Value>>;
    fn normalise_usage_credits(&self, value: &Value) -> Result<Option<Value>>;
    fn normalise_resets(&self, value: &Value) -> Result<Option<Value>>;
    fn normalise_allowance(&self, value: &Value) -> Result<Option<Value>>;
}

/// Complete set of external report owners used by stats and peer projections.
pub trait StatsServices: HostProvider + DeliveryReporter + QuotaProjector {}
impl<T: HostProvider + DeliveryReporter + QuotaProjector> StatsServices for T {}

#[derive(Clone, Debug)]
struct Receipt {
    first_row: String,
    usage: Usage,
}

/// Build a local report over a ledger connection. `prices` and `catalogue`
/// are loaded by their owner before this pure report operation begins.
#[allow(clippy::too_many_arguments)]
pub fn stats<S: UsageCellStore, E: StatsServices>(
    store: &mut S,
    services: &E,
    prices: &Prices,
    catalogue: &Catalogue,
    since: Option<&str>,
    summary: bool,
    until: Option<&str>,
) -> Result<Value> {
    let boundary = since
        .filter(|value| !value.is_empty())
        .map(parse_time)
        .transpose()?;
    let end = until
        .filter(|value| !value.is_empty())
        .map(parse_time)
        .transpose()?;
    let mut where_clause = String::new();
    let mut parameters = BTreeMap::new();
    let (order, cell_order, order_by) = if let Some(boundary) = boundary {
        let margin = boundary
            .checked_sub(jiff::Span::new().hours(24))
            .map_err(|_| Error::Timestamp(boundary.to_string()))?;
        let start = margin
            .to_zoned(jiff::tz::TimeZone::UTC)
            .strftime("%Y-%m-%d")
            .to_string();
        where_clause = " AND timestamp >= :start".into();
        parameters.insert("start".into(), Value::String(start));
        (
            "printf('%s' || char(1) || '%020d', COALESCE(timestamp,''), rowid)",
            "printf('%s' || char(1) || '%020d', COALESCE(timestamp,''), ledger_rowid)",
            "timestamp, rowid",
        )
    } else {
        (
            "printf('%020d', rowid)",
            "printf('%020d', ledger_rowid)",
            "rowid",
        )
    };

    let ledger_ids = ledger_host_ids(store)?;
    let identity = services.identity(&ledger_ids)?;
    let aliases = identity.aliases.clone();
    let current = identity.current.clone();
    let receipt_sql = format!(
        "SELECT {order} AS first_row, * FROM usage WHERE source='fabric'{where_clause} ORDER BY {order_by}"
    );
    let queried = query_rows(store, &receipt_sql, parameters.clone())?;
    let mut receipts = Vec::<Receipt>::new();
    for values in queried {
        let first_row = text_value(&values, "first_row")
            .ok_or_else(|| Error::Invalid("usage row missing first_row".into()))?;
        let mut usage = usage_from_sql(&values)?;
        let stored_host = usage.host.clone();
        usage.host = identity.canonical(&stored_host).to_owned();
        let key = (
            usage.source.clone(),
            usage.event_id.clone(),
            usage.host.clone(),
        );
        if let Some(position) = receipts.iter().position(|row| {
            (
                row.usage.source.clone(),
                row.usage.event_id.clone(),
                row.usage.host.clone(),
            ) == key
        }) {
            // Replacing a dict value does not change its original insertion order.
            if stored_host == current {
                receipts[position] = Receipt { first_row, usage };
            }
        } else {
            receipts.push(Receipt { first_row, usage });
        }
    }

    let mut selected = Vec::<Receipt>::new();
    let mut excluded = BTreeMap::<String, Numeric>::new();
    let mut unattributed_rows = Vec::<Usage>::new();
    for mut receipt in receipts {
        if !included(receipt.usage.timestamp.as_deref(), boundary, end)? {
            continue;
        }
        let reason = attribute(&mut receipt.usage, catalogue);
        match reason {
            Some(Exclusion::TestFixture) => {
                increment(&mut excluded, "test_fixture", Numeric::ONE)?;
                continue;
            }
            Some(Exclusion::Unattributed) => unattributed_rows.push(receipt.usage.clone()),
            None => {}
        }
        selected.push(receipt);
    }
    let attributed: Vec<Usage> = selected
        .iter()
        .filter(|row| costs::classify(&row.usage, row.usage.model_raw.as_deref()).is_none())
        .map(|row| row.usage.clone())
        .collect();

    let (cells, native_rows) = costs::usage_cells(
        store,
        &where_clause,
        &parameters,
        summary,
        boundary,
        end,
        !attributed.is_empty(),
        cell_order,
        &current,
        &aliases,
    )?;
    let mut classified_cells = Vec::<(UsageCell, Option<Exclusion>)>::new();
    for mut cell in cells {
        if !included(cell.usage.timestamp.as_deref(), boundary, end)? {
            continue;
        }
        let reason = attribute(&mut cell.usage, catalogue);
        if reason == Some(Exclusion::TestFixture) {
            increment(&mut excluded, "test_fixture", cell.rows)?;
        }
        if reason.is_none() {
            cell.dimensions
                .insert("model".into(), optional_string(cell.usage.model.clone()));
            cell.dimensions.insert(
                "day".into(),
                optional_string(day(cell.usage.timestamp.as_deref())?),
            );
        }
        classified_cells.push((cell, reason));
    }

    let mut native = BTreeSet::<(Option<String>, Option<String>)>::new();
    for row in native_rows {
        if !included(row.timestamp.as_deref(), boundary, end)? {
            continue;
        }
        let mut usage = Usage {
            adapter: row.adapter.clone(),
            provider: row.provider.clone(),
            model: row.model.clone(),
            session: row.session.clone(),
            timestamp: row.timestamp.clone(),
            ..Usage::default()
        };
        if attribute(&mut usage, catalogue).is_none() {
            native.insert((usage.adapter, usage.session));
        }
    }
    let mut usage_with_day = Vec::<(String, Usage, Option<String>)>::new();
    for receipt in selected.iter().filter(|receipt| {
        costs::classify(&receipt.usage, receipt.usage.model_raw.as_deref()).is_none()
            && (receipt.usage.source != "fabric"
                || !native
                    .contains(&(receipt.usage.adapter.clone(), receipt.usage.session.clone())))
    }) {
        usage_with_day.push((
            receipt.first_row.clone(),
            receipt.usage.clone(),
            day(receipt.usage.timestamp.as_deref())?,
        ));
    }

    let usage = finalised_groups(
        &usage_with_day,
        &classified_cells,
        &["provider", "adapter", "model"],
        prices,
    )?;
    let usage_per_day = finalised_groups(
        &usage_with_day,
        &classified_cells,
        &["day", "provider", "adapter", "account"],
        prices,
    )?;
    let usage_totals = rounded_raw_groups(&usage_with_day, &classified_cells, &[], prices)?;
    let parent_rows: Vec<Usage> = attributed
        .iter()
        .filter(|row| {
            row.source == "fabric" && row.parent_run_id.as_deref().is_some_and(|s| !s.is_empty())
        })
        .cloned()
        .collect();
    let usage_by_parent = costs::aggregate(
        &parent_rows,
        &["parent_run_id", "parent_task_id", "parent_attempt"],
        prices,
    )?;
    let fabric_attributed: Vec<Usage> = attributed
        .iter()
        .filter(|row| row.source == "fabric")
        .cloned()
        .collect();
    let outcomes = report::outcomes(&fabric_attributed, prices, false)?;
    let lane_totals = report::outcomes(&fabric_attributed, prices, true)?;

    let mut missing = BTreeMap::<(String, Option<String>), MissingGroup>::new();
    for row in &unattributed_rows {
        missing_group(&mut missing, &row.source, row.adapter.clone())
            .add_usage(row, Numeric::ONE)?;
    }
    for (cell, reason) in &classified_cells {
        if *reason == Some(Exclusion::Unattributed) {
            missing_group(&mut missing, &cell.usage.source, cell.usage.adapter.clone())
                .add_usage(&cell.usage, cell.rows)?;
        }
    }
    let unattributed = missing_values(missing)?;

    let (lanes, attempts) = attempt_stats(&selected);
    let (limits_per_day, last_limit_events) = limit_stats(
        &selected,
        &observation_rows(store, false)?,
        &identity,
        boundary,
        end,
        services.now()?,
    )?;
    let quota_observations =
        quota_observations(&observation_rows(store, true)?, &identity, boundary, end)?;
    let deliveries = services.report_deliveries(&delivery_rows(store)?, since)?;
    let compactions = services.report_compactions(&compaction_rows(store)?, since, until)?;
    let pending_source_rebuilds = pending_rebuilds(store)?;
    let receipt_usage_rows_excluded = attributed.len().saturating_sub(usage_with_day.len());

    let mut result = json!({
        "schema_version": 2,
        "since": since,
        "host": current,
        "usage": usage,
        "usage_per_day": usage_per_day,
        "usage_by_parent": usage_by_parent,
        "outcomes": outcomes,
        "lane_totals": lane_totals,
        "usage_totals": usage_totals,
        "lanes": lanes,
        "attempts": attempts,
        "limits_per_day": limits_per_day,
        "last_limit_events": last_limit_events,
        "quota_observations": quota_observations,
        "deliveries": deliveries,
        "excluded_rows": excluded,
        "unattributed": unattributed,
        "receipt_usage_rows_excluded": receipt_usage_rows_excluded,
        "pending_source_rebuilds": pending_source_rebuilds,
        "compactions_per_lane": compactions,
    });
    if summary {
        result["session_usage"] = Value::Array(
            raw_groups(
                &usage_with_day,
                &classified_cells,
                &[
                    "host", "source", "session", "day", "provider", "adapter", "account", "model",
                ],
                prices,
            )?
            .into_iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()?,
        );
    }
    Ok(result)
}

/// Build the versioned peer envelope and remove local-only cost enrichment.
#[allow(clippy::too_many_arguments)]
pub fn peer_summary<S: UsageCellStore, E: StatsServices>(
    store: &mut S,
    services: &E,
    prices: &Prices,
    catalogue: &Catalogue,
    since: Option<&str>,
    host: Option<&str>,
) -> Result<Value> {
    let mut report = stats(store, services, prices, catalogue, since, true, None)?;
    if host.is_some_and(|value| !value.is_empty()) {
        report["host"] = Value::String(host.unwrap_or_default().to_owned());
        if let Some(groups) = report["session_usage"].as_array_mut() {
            for row in groups {
                row["host"] = Value::String(host.unwrap_or_default().to_owned());
            }
        }
    }
    if let Some(outcomes) = report["outcomes"].as_array_mut() {
        for row in outcomes {
            if let Some(object) = row.as_object_mut() {
                for field in [
                    "known_cost_usd",
                    "missing_cost_rows",
                    "cost_per_success_usd",
                ] {
                    object.remove(field);
                }
            }
        }
    }
    let identity = services.identity(&ledger_host_ids(store)?)?;
    let public_host = if host.is_some_and(|value| !value.is_empty()) {
        host.unwrap_or_default().to_owned()
    } else {
        identity.current.clone()
    };
    let mut snapshots = Vec::new();
    for value in services.snapshots()? {
        if !value.is_object() {
            return Err(Error::Invalid("quota snapshot must be an object".into()));
        }
        let snapshot_host = value.get("host").and_then(Value::as_str);
        if snapshot_host.is_some_and(|value| identity.canonical(value) == identity.current) {
            let mut value = value;
            if let Some(object) = value.as_object_mut() {
                object.insert("host".into(), Value::String(public_host.clone()));
            }
            snapshots.push(public_snapshot(services, &value)?);
        }
    }
    report["schema"] = Value::String(crate::hosts::SUMMARY_SCHEMA.into());
    report["quota_snapshots"] = Value::Array(snapshots);
    if let Some(observations) = report["quota_observations"].as_array_mut() {
        for row in observations {
            *row = selected_fields(
                row,
                &[
                    "host",
                    "provider",
                    "account",
                    "window",
                    "used_percent",
                    "window_minutes",
                    "reset_at",
                    "timestamp",
                ],
            );
        }
    }
    Ok(report)
}

/// Keep only documented quota metadata before a snapshot crosses the peer boundary.
pub fn public_snapshot(quota: &impl QuotaProjector, value: &Value) -> Result<Value> {
    if !value.is_object() {
        return Err(Error::Invalid("quota snapshot must be an object".into()));
    }
    let mut result = selected_fields(
        value,
        &[
            "schema",
            "provider",
            "account",
            "host",
            "plan",
            "active",
            "priority",
            "observed_at",
            "attempted_at",
            "status",
            "error",
            "source",
            "unreported",
        ],
    );
    let windows = match value.get("windows") {
        None => &[][..],
        Some(Value::Array(windows)) => windows.as_slice(),
        Some(_) => return Err(Error::Invalid("quota windows must be an array".into())),
    };
    let mut public_windows = Vec::new();
    for row in windows.iter().filter(|row| row.is_object()) {
        let mut window = selected_fields(
            row,
            &[
                "name",
                "used_percent",
                "window_minutes",
                "reset_at",
                "raw_kind",
                "window_start",
            ],
        );
        if let Some(object) = window.as_object_mut() {
            for (key, amount) in quota.window_amounts(row)? {
                object.insert(key, amount);
            }
        }
        if row.get("advisory") == Some(&Value::Bool(true)) {
            window["advisory"] = Value::Bool(true);
        }
        public_windows.push(window);
    }
    result["windows"] = Value::Array(public_windows);
    for (key, extra) in [
        (
            "usage_credits",
            quota.normalise_usage_credits(value.get("usage_credits").unwrap_or(&Value::Null))?,
        ),
        (
            "resets",
            quota.normalise_resets(value.get("resets").unwrap_or(&Value::Null))?,
        ),
        (
            "allowance",
            quota.normalise_allowance(value.get("allowance").unwrap_or(&Value::Null))?,
        ),
    ] {
        if extra.as_ref().is_some_and(provenant_core::py::python_truth) {
            result[key] = extra.unwrap_or(Value::Null);
        }
    }
    if let Some(credits) = value.get("credits").and_then(Value::as_object) {
        if let Some(reset) = credits
            .get("reset_credits")
            .and_then(Value::as_i64)
            .filter(|n| *n >= 0)
        {
            let mut public_credits = Map::new();
            public_credits.insert("reset_credits".into(), json!(reset));
            if matches!(
                credits.get("source").and_then(Value::as_str),
                Some("manual" | "provider")
            ) {
                public_credits.insert(
                    "source".into(),
                    credits.get("source").cloned().unwrap_or(Value::Null),
                );
            }
            result["credits"] = Value::Object(public_credits);
        } else if let Some(reset) = credits.get("reset_credits").and_then(Value::as_u64) {
            let mut public_credits = Map::new();
            public_credits.insert("reset_credits".into(), json!(reset));
            if matches!(
                credits.get("source").and_then(Value::as_str),
                Some("manual" | "provider")
            ) {
                public_credits.insert(
                    "source".into(),
                    credits.get("source").cloned().unwrap_or(Value::Null),
                );
            }
            result["credits"] = Value::Object(public_credits);
        }
    }
    Ok(result)
}

fn selected_fields(value: &Value, names: &[&str]) -> Value {
    let mut output = Map::new();
    if let Some(object) = value.as_object() {
        for name in names {
            if let Some(value) = object.get(*name) {
                output.insert((*name).into(), value.clone());
            }
        }
    }
    Value::Object(output)
}

fn parse_time(value: &str) -> Result<jiff::Timestamp> {
    let parsed = provenant_core::py::python_fromisoformat(value)
        .map_err(|_| Error::Timestamp(value.into()))?;
    let parsed = if parsed.offset_microseconds.is_none() {
        provenant_core::py::PythonDateTime {
            offset_microseconds: Some(0),
            ..parsed
        }
    } else {
        parsed
    };
    parsed
        .timestamp()
        .and_then(|value| value.to_jiff())
        .map_err(|_| Error::Timestamp(value.into()))
}

fn included(
    value: Option<&str>,
    low: Option<jiff::Timestamp>,
    high: Option<jiff::Timestamp>,
) -> Result<bool> {
    if low.is_none() && high.is_none() {
        return Ok(true);
    }
    let Some(value) = value else { return Ok(false) };
    let stamp = parse_time(value)?;
    Ok(low.is_none_or(|low| stamp >= low) && high.is_none_or(|high| stamp < high))
}

fn day(value: Option<&str>) -> Result<Option<String>> {
    value
        .filter(|value| !value.is_empty())
        .map(parse_time)
        .transpose()
        .map(|timestamp| {
            timestamp.map(|value| {
                value
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .strftime("%Y-%m-%d")
                    .to_string()
            })
        })
}

fn attribute(row: &mut Usage, catalogue: &Catalogue) -> Option<Exclusion> {
    let raw = row.model.clone();
    row.model_raw = raw.clone();
    row.model = costs::normalise_model(
        row.adapter.as_deref(),
        row.provider.as_deref(),
        row.model.as_deref(),
        catalogue,
    );
    costs::classify(row, raw.as_deref())
}

fn optional_string(value: Option<String>) -> Value {
    value.map(Value::String).unwrap_or(Value::Null)
}

fn usage_from_sql(row: &BTreeMap<String, Value>) -> Result<Usage> {
    let text = |name: &str| row.get(name).and_then(Value::as_str).map(str::to_owned);
    let number = |name: &str| row.get(name).and_then(Numeric::from_value);
    let float = |name: &str| row.get(name).and_then(Value::as_f64);
    Ok(Usage {
        source: required_text(row, "source")?,
        event_id: required_text(row, "event_id")?,
        host: required_text(row, "host")?,
        session: text("session"),
        lane: text("lane"),
        run_id: text("run_id"),
        task_id: text("task_id"),
        attempt: number("attempt"),
        graph_node: text("graph_node"),
        adapter: text("adapter"),
        provider: text("provider"),
        model: text("model"),
        effort: text("effort"),
        account: text("account"),
        repository: text("repository"),
        revision: text("revision"),
        timestamp: text("timestamp"),
        input_tokens: number("input_tokens"),
        cache_read_tokens: number("cache_read_tokens"),
        cache_write_tokens: number("cache_write_tokens"),
        output_tokens: number("output_tokens"),
        reasoning_tokens: number("reasoning_tokens"),
        usage_source: text("usage_source"),
        reported_cost: float("reported_cost"),
        status: text("status"),
        error_class: text("error_class"),
        task_class: text("task_class"),
        route: text("route"),
        duration_ms: number("duration_ms"),
        provider_ms: number("provider_ms"),
        classified_by: text("classified_by"),
        classifier_confidence: float("classifier_confidence"),
        shadow_answers: text("shadow_answers"),
        parent_run_id: text("parent_run_id"),
        parent_task_id: text("parent_task_id"),
        parent_attempt: number("parent_attempt"),
        requested_model: text("requested_model"),
        resolved_model: text("resolved_model"),
        observed_model: text("observed_model"),
        delivery_run_id: text("delivery_run_id"),
        model_raw: None,
    })
}

fn required_text(row: &BTreeMap<String, Value>, key: &str) -> Result<String> {
    row.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("ledger row missing {key}")))
}

fn text_value(row: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn query_rows<S: UsageCellStore>(
    store: &mut S,
    sql: &str,
    parameters: BTreeMap<String, Value>,
) -> Result<Vec<BTreeMap<String, Value>>> {
    store.query_rows(&SqlCellQuery {
        sql: sql.into(),
        parameters,
    })
}

fn ledger_host_ids<S: UsageCellStore>(store: &mut S) -> Result<BTreeSet<String>> {
    let rows = match query_rows(
        store,
        "SELECT key, value FROM meta WHERE key IN ('host_id','host_ids')",
        BTreeMap::new(),
    ) {
        Ok(rows) => rows,
        Err(Error::Sqlite(_)) => return Ok(BTreeSet::new()),
        Err(error) => return Err(error),
    };
    let mut result = BTreeSet::new();
    for row in rows {
        let key = text_value(&row, "key").unwrap_or_default();
        let value = text_value(&row, "value").unwrap_or_default();
        let values = if key == "host_ids" {
            serde_json::from_str::<Value>(&value).ok()
        } else {
            Some(json!([value]))
        };
        if let Some(values) = values.and_then(|value| value.as_array().cloned()) {
            result.extend(
                values.into_iter().filter_map(|value| {
                    value.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
                }),
            );
        }
    }
    Ok(result)
}

fn observation_rows<S: UsageCellStore>(
    store: &mut S,
    used_only: bool,
) -> Result<Vec<BTreeMap<String, Value>>> {
    let sql = if used_only {
        "SELECT * FROM observations WHERE used_percent IS NOT NULL ORDER BY timestamp"
    } else {
        "SELECT * FROM observations ORDER BY source"
    };
    query_rows(store, sql, BTreeMap::new())
}

fn delivery_rows<S: UsageCellStore>(store: &mut S) -> Result<Vec<DeliveryReportRow>> {
    query_rows(
        store,
        "SELECT run_id, started_at, risk_tier, chair_family, writer_model, review_verdicts FROM deliveries ORDER BY started_at, run_id",
        BTreeMap::new(),
    )?
    .into_iter()
    .map(|row| Ok(DeliveryReportRow {
        run_id: text_value(&row, "run_id"),
        started_at: text_value(&row, "started_at"),
        risk_tier: text_value(&row, "risk_tier"),
        chair_family: text_value(&row, "chair_family"),
        writer_model: text_value(&row, "writer_model"),
        review_verdicts: text_value(&row, "review_verdicts"),
    }))
    .collect()
}

fn compaction_rows<S: UsageCellStore>(store: &mut S) -> Result<Vec<CompactionRow>> {
    query_rows(
        store,
        "SELECT DISTINCT usage.lane, events.event_id, events.ts FROM events JOIN usage ON usage.session=events.session WHERE events.kind='session.compaction' AND usage.lane IS NOT NULL",
        BTreeMap::new(),
    )?
    .into_iter()
    .map(|row| {
        Ok(CompactionRow {
            lane: required_text(&row, "lane")?,
            event_id: required_text(&row, "event_id")?,
            ts: text_value(&row, "ts"),
        })
    })
    .collect()
}

fn pending_rebuilds<S: UsageCellStore>(store: &mut S) -> Result<Vec<String>> {
    query_rows(
        store,
        "SELECT source FROM rebuilds ORDER BY source",
        BTreeMap::new(),
    )?
    .into_iter()
    .map(|row| {
        text_value(&row, "source")
            .ok_or_else(|| Error::Invalid("rebuild row missing source".into()))
    })
    .collect()
}

fn finalised_groups(
    rows: &[(String, Usage, Option<String>)],
    cells: &[(UsageCell, Option<Exclusion>)],
    keys: &[&str],
    prices: &Prices,
) -> Result<Vec<Value>> {
    costs::finalise(raw_groups(rows, cells, keys, prices)?, keys)
        .into_iter()
        .map(|row| serde_json::to_value(row).map_err(Error::from))
        .collect()
}

fn raw_groups(
    rows: &[(String, Usage, Option<String>)],
    cells: &[(UsageCell, Option<Exclusion>)],
    keys: &[&str],
    prices: &Prices,
) -> Result<Vec<costs::Group>> {
    let mut groups = Vec::<(String, costs::Group)>::new();
    for (first_row, usage, day) in rows {
        let mut value = serde_json::to_value(usage)?;
        value["day"] = optional_string(day.clone());
        groups.push((
            first_row.clone(),
            costs::unit_ordered(
                usage,
                usage.model_raw.as_deref(),
                costs::OrderedFields::from_keys(keys, |key| {
                    value.get(key).cloned().unwrap_or(Value::Null)
                }),
                keys,
                prices,
            )?,
        ));
    }
    for (cell, reason) in cells {
        if reason.is_some() {
            continue;
        }
        groups.push((
            cell.first_row.clone(),
            costs::cell_unit(cell, keys, prices)?,
        ));
    }
    groups.sort_by(|left, right| left.0.cmp(&right.0));
    costs::combine(groups.into_iter().map(|(_, group)| group), keys)
}

fn rounded_raw_groups(
    rows: &[(String, Usage, Option<String>)],
    cells: &[(UsageCell, Option<Exclusion>)],
    keys: &[&str],
    prices: &Prices,
) -> Result<Vec<Value>> {
    raw_groups(rows, cells, keys, prices)?
        .into_iter()
        .map(|group| {
            let mut value = serde_json::to_value(group)?;
            if let Some(cost) = value.get("known_cost_usd").and_then(Value::as_f64) {
                value["known_cost_usd"] = json!(costs::round(cost, 6));
            }
            if let Some(cost) = value.get("reported_cost_usd").and_then(Value::as_f64) {
                value["reported_cost_usd"] = json!(costs::round(cost, 6));
            }
            Ok(value)
        })
        .collect()
}

fn increment(counter: &mut BTreeMap<String, Numeric>, key: &str, amount: Numeric) -> Result<()> {
    let value = counter.entry(key.into()).or_insert(Numeric::ZERO);
    *value = value.checked_add(amount)?;
    Ok(())
}

#[derive(Default)]
struct MissingGroup {
    rows: Numeric,
    input_tokens: Numeric,
    statuses: BTreeMap<String, Numeric>,
}

impl MissingGroup {
    fn add_usage(&mut self, row: &Usage, count: Numeric) -> Result<()> {
        self.rows = self.rows.checked_add(count)?;
        self.input_tokens = self
            .input_tokens
            .checked_add(row.input_tokens.unwrap_or(Numeric::ZERO))?;
        if let Some(status) = row.status.as_deref().filter(|status| !status.is_empty()) {
            increment(&mut self.statuses, status, count)?;
        }
        Ok(())
    }
}

fn missing_group<'a>(
    groups: &'a mut BTreeMap<(String, Option<String>), MissingGroup>,
    source: &str,
    adapter: Option<String>,
) -> &'a mut MissingGroup {
    groups.entry((source.to_owned(), adapter)).or_default()
}

fn missing_values(groups: BTreeMap<(String, Option<String>), MissingGroup>) -> Result<Vec<Value>> {
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|((source, adapter), _)| {
        python_tuple_key(&[Some(source.clone()), adapter.clone()])
    });
    groups
        .into_iter()
        .map(|((source, adapter), group)| {
            Ok(json!({
                "source": source,
                "adapter": adapter,
                "rows": group.rows,
                "input_tokens": group.input_tokens,
                "statuses": group.statuses,
            }))
        })
        .collect()
}

fn python_repr(value: Option<&str>) -> String {
    value
        .map(provenant_core::py::python_repr)
        .unwrap_or_else(|| "None".into())
}

fn python_tuple_key(values: &[Option<String>]) -> String {
    format!(
        "({})",
        values
            .iter()
            .map(|value| python_repr(value.as_deref()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn status_rows(counts: BTreeMap<(Option<String>, Option<String>), u64>, field: &str) -> Vec<Value> {
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by_key(|((status, error), _)| python_tuple_key(&[status.clone(), error.clone()]));
    counts
        .into_iter()
        .map(|((status, error), count)| {
            let mut row = Map::new();
            row.insert("status".into(), optional_string(status));
            row.insert("error_class".into(), optional_string(error));
            row.insert(field.into(), json!(count));
            Value::Object(row)
        })
        .collect()
}

fn attempt_stats(selected: &[Receipt]) -> (Vec<Value>, Vec<Value>) {
    let mut attempts = BTreeMap::<(Option<String>, Option<String>), u64>::new();
    let mut latest = Vec::<((Option<String>, Option<String>, Option<String>), usize)>::new();
    for (index, receipt) in selected.iter().enumerate() {
        let row = &receipt.usage;
        *attempts
            .entry((row.status.clone(), row.error_class.clone()))
            .or_default() += 1;
        let key = (
            row.repository.clone(),
            row.run_id.clone(),
            row.task_id.clone(),
        );
        if let Some((_, position)) = latest.iter_mut().find(|(candidate, _)| *candidate == key) {
            let previous = &selected[*position].usage;
            if row.attempt.unwrap_or(Numeric::ZERO) > previous.attempt.unwrap_or(Numeric::ZERO) {
                *position = index;
            }
        } else {
            latest.push((key, index));
        }
    }
    let mut lanes = BTreeMap::<(Option<String>, Option<String>), u64>::new();
    for (_, index) in latest {
        let row = &selected[index].usage;
        *lanes
            .entry((row.status.clone(), row.error_class.clone()))
            .or_default() += 1;
    }
    (
        status_rows(lanes, "lanes"),
        status_rows(attempts, "attempts"),
    )
}

type LimitKey = (Option<String>, Option<String>, Option<String>, String);
type HostProviderAccount = (Option<String>, Option<String>, Option<String>);

fn limit_stats(
    selected: &[Receipt],
    observations: &[BTreeMap<String, Value>],
    identity: &HostIdentity,
    boundary: Option<jiff::Timestamp>,
    end: Option<jiff::Timestamp>,
    now: jiff::Timestamp,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut limits = BTreeMap::<LimitKey, u64>::new();
    let mut last = BTreeMap::<HostProviderAccount, String>::new();
    let mut receipts = BTreeSet::<(Option<String>, Option<String>, Option<String>, String)>::new();
    for receipt in selected {
        let row = &receipt.usage;
        if !matches!(
            row.status.as_deref(),
            Some("rate_limited" | "usage_limited")
        ) {
            continue;
        }
        remember_limit(
            &mut last,
            Some(row.host.clone()),
            row.provider.clone(),
            row.account.clone(),
            row.timestamp.as_deref(),
        )?;
        receipts.insert((
            row.adapter.clone(),
            row.model_raw.clone(),
            row.run_id.clone(),
            row.status.clone().unwrap_or_default(),
        ));
        let day = day(row.timestamp.as_deref())?;
        *limits
            .entry((
                day,
                row.provider.clone(),
                row.account.clone(),
                row.status.clone().unwrap_or_default(),
            ))
            .or_default() += 1;
    }
    let mut seen = BTreeSet::new();
    for observation in observations {
        let timestamp = text_value(observation, "timestamp");
        if !included(timestamp.as_deref(), boundary, end)? {
            continue;
        }
        let status = text_value(observation, "status");
        let Some(status @ ("rate_limited" | "usage_limited")) = status.as_deref() else {
            continue;
        };
        let canonical_host =
            text_value(observation, "host").map(|host| identity.canonical(&host).to_owned());
        remember_limit(
            &mut last,
            canonical_host,
            text_value(observation, "provider"),
            text_value(observation, "account"),
            timestamp.as_deref(),
        )?;
        let match_receipt = (
            text_value(observation, "adapter"),
            text_value(observation, "model"),
            text_value(observation, "run_id"),
            status.to_owned(),
        );
        if receipts.contains(&match_receipt) {
            continue;
        }
        let date = day(timestamp.as_deref())?;
        let run_or_time = text_value(observation, "run_id")
            .filter(|value| !value.is_empty())
            .or(timestamp.clone());
        let dedup = (
            text_value(observation, "adapter"),
            text_value(observation, "model"),
            run_or_time,
            status.to_owned(),
            date.clone(),
        );
        if !seen.insert(dedup) {
            continue;
        }
        *limits
            .entry((
                date,
                text_value(observation, "provider"),
                text_value(observation, "account"),
                status.to_owned(),
            ))
            .or_default() += 1;
    }
    let mut limit_rows: Vec<_> = limits.into_iter().collect();
    limit_rows.sort_by_key(|((day, provider, account, status), _)| {
        python_tuple_key(&[
            day.clone(),
            provider.clone(),
            account.clone(),
            Some(status.clone()),
        ])
    });
    let limit_values = limit_rows.into_iter().map(|((day, provider, account, status), events)| json!({
        "day": day, "provider": provider, "account": account, "status": status, "events": events,
    })).collect();
    let mut last_rows: Vec<_> = last.into_iter().collect();
    last_rows.sort_by_key(|((host, provider, account), _)| {
        python_tuple_key(&[host.clone(), provider.clone(), account.clone()])
    });
    let mut last_values = Vec::new();
    for ((host, provider, account), stamp) in last_rows {
        let age = rounded_nonnegative_seconds(now, parse_time(&stamp)?);
        last_values.push(json!({"host":host,"provider":provider,"account":account,"timestamp":stamp,"age_seconds":age}));
    }
    Ok((limit_values, last_values))
}

fn remember_limit(
    last: &mut BTreeMap<HostProviderAccount, String>,
    host: Option<String>,
    provider: Option<String>,
    account: Option<String>,
    timestamp: Option<&str>,
) -> Result<()> {
    let Some(stamp) = timestamp.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let current = parse_time(stamp)?;
    let key = (host, provider, account);
    let replace = last
        .get(&key)
        .map(|old| parse_time(old))
        .transpose()?
        .is_none_or(|old| current > old);
    if replace {
        last.insert(key, stamp.into());
    }
    Ok(())
}

fn rounded_nonnegative_seconds(now: jiff::Timestamp, then: jiff::Timestamp) -> i64 {
    let delta = i128::from(now.as_microsecond()) - i128::from(then.as_microsecond());
    if delta <= 0 {
        return 0;
    }
    let seconds = delta / 1_000_000;
    let fraction = delta % 1_000_000;
    let rounded = if fraction > 500_000 || (fraction == 500_000 && seconds % 2 == 1) {
        seconds + 1
    } else {
        seconds
    };
    i64::try_from(rounded).unwrap_or(i64::MAX)
}

fn quota_observations(
    rows: &[BTreeMap<String, Value>],
    identity: &HostIdentity,
    boundary: Option<jiff::Timestamp>,
    end: Option<jiff::Timestamp>,
) -> Result<Vec<Value>> {
    let mut windows = Vec::<((Option<String>, Option<String>, Option<String>), Value)>::new();
    for row in rows {
        if !included(text_value(row, "timestamp").as_deref(), boundary, end)? {
            continue;
        }
        let mut value = Value::Object(
            row.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        if let Some(host) = text_value(row, "host") {
            value["host"] = Value::String(identity.canonical(&host).to_owned());
        }
        let key = (
            text_value(row, "provider"),
            text_value(row, "account"),
            text_value(row, "window"),
        );
        if let Some((_, previous)) = windows.iter_mut().find(|(candidate, _)| *candidate == key) {
            *previous = value;
        } else {
            windows.push((key, value));
        }
    }
    let mut rows = windows;
    rows.sort_by_key(|((provider, account, window), _)| {
        [provider, account, window]
            .into_iter()
            .map(|v| {
                v.as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    });
    Ok(rows.into_iter().map(|(_, value)| value).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::differential::{equal, python, LOAD};
    use rusqlite::{params_from_iter, types::Value as SqlValue, Connection};
    use serde_json::json;

    #[derive(Clone)]
    struct FixtureServices;
    impl HostProvider for FixtureServices {
        fn identity(&self, ledger_ids: &BTreeSet<String>) -> Result<HostIdentity> {
            let aliases = ledger_ids
                .iter()
                .cloned()
                .chain(["old-host".into()])
                .collect();
            Ok(HostIdentity::new(
                "fixture-host".into(),
                aliases,
                &BTreeSet::new(),
            ))
        }
        fn now(&self) -> Result<jiff::Timestamp> {
            parse_time("2026-10-23T12:00:00Z")
        }
    }
    impl DeliveryReporter for FixtureServices {
        fn report_deliveries(
            &self,
            rows: &[DeliveryReportRow],
            since: Option<&str>,
        ) -> Result<Vec<DeliverySummary>> {
            LedgerDeliveryReporter.report_deliveries(rows, since)
        }
        fn report_compactions(
            &self,
            rows: &[CompactionRow],
            since: Option<&str>,
            until: Option<&str>,
        ) -> Result<Vec<Compactions>> {
            LedgerDeliveryReporter.report_compactions(rows, since, until)
        }
    }
    impl QuotaProjector for FixtureServices {
        fn snapshots(&self) -> Result<Vec<Value>> {
            Ok(Vec::new())
        }
        fn window_amounts(&self, _row: &Value) -> Result<Map<String, Value>> {
            Ok(Map::new())
        }
        fn normalise_usage_credits(&self, _value: &Value) -> Result<Option<Value>> {
            Ok(None)
        }
        fn normalise_resets(&self, _value: &Value) -> Result<Option<Value>> {
            Ok(None)
        }
        fn normalise_allowance(&self, _value: &Value) -> Result<Option<Value>> {
            Ok(None)
        }
    }

    fn fixture_db(input: &Value) -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE usage (
                source TEXT, event_id TEXT, host TEXT, session TEXT, lane TEXT, run_id TEXT, task_id TEXT,
                attempt INTEGER, graph_node TEXT, adapter TEXT, provider TEXT, model TEXT, effort TEXT,
                account TEXT, repository TEXT, revision TEXT, timestamp TEXT, input_tokens INTEGER,
                cache_read_tokens INTEGER, cache_write_tokens INTEGER, output_tokens INTEGER,
                reasoning_tokens INTEGER, usage_source TEXT, reported_cost REAL, status TEXT, error_class TEXT,
                task_class TEXT, route TEXT, duration_ms INTEGER, provider_ms INTEGER, classified_by TEXT,
                classifier_confidence REAL, shadow_answers TEXT, parent_run_id TEXT, parent_task_id TEXT,
                parent_attempt INTEGER, requested_model TEXT, resolved_model TEXT, observed_model TEXT,
                delivery_run_id TEXT);
             CREATE TABLE observations (
                event_id TEXT, source TEXT, host TEXT, provider TEXT, adapter TEXT, model TEXT, account TEXT,
                session TEXT, origin_path TEXT, lane TEXT, run_id TEXT, timestamp TEXT, window TEXT,
                used_percent REAL, window_minutes REAL, reset_at TEXT, status TEXT);
             CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE rebuilds (source TEXT PRIMARY KEY, paths TEXT NOT NULL);
             CREATE TABLE deliveries (
                run_dir TEXT, host TEXT, run_id TEXT, started_at TEXT, risk_tier TEXT, chair_family TEXT,
                writer_model TEXT, review_verdicts TEXT);
             CREATE TABLE events (event_id TEXT, kind TEXT, host TEXT, ts TEXT, session TEXT, lane TEXT);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO meta(key,value) VALUES ('host_ids',?1)",
            [json!(["old-host"]).to_string()],
        )
        .unwrap();
        for row in input["rows"].as_array().unwrap() {
            insert_object(&db, "usage", row);
        }
        for row in input["observations"].as_array().unwrap() {
            insert_object(&db, "observations", row);
        }
        for row in input["deliveries"].as_array().unwrap() {
            insert_object(&db, "deliveries", row);
        }
        for row in input["events"].as_array().unwrap() {
            insert_object(&db, "events", row);
        }
        db
    }

    fn insert_object(db: &Connection, table: &str, value: &Value) {
        let Some(object) = value.as_object() else {
            return;
        };
        if object.is_empty() {
            return;
        }
        let columns = object.keys().cloned().collect::<Vec<_>>();
        let placeholders = std::iter::repeat_n("?", columns.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({placeholders})",
            columns.join(",")
        );
        let values = columns
            .iter()
            .map(|key| match &object[key] {
                Value::Null => SqlValue::Null,
                Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
                Value::Number(value) => value
                    .as_i64()
                    .map(SqlValue::Integer)
                    .or_else(|| value.as_f64().map(SqlValue::Real))
                    .unwrap_or(SqlValue::Null),
                Value::String(value) => SqlValue::Text(value.clone()),
                Value::Array(_) | Value::Object(_) => SqlValue::Text(object[key].to_string()),
            })
            .collect::<Vec<_>>();
        db.execute(&sql, params_from_iter(values)).unwrap();
    }

    fn stats_input() -> Value {
        let receipt = |event_id: &str,
                       host: &str,
                       attempt: i64,
                       status: &str,
                       timestamp: &str,
                       session: &str| {
            json!({"source":"fabric","event_id":event_id,"host":host,"session":session,"lane":"lane-a",
                "run_id":"run-a","task_id":"task-a","attempt":attempt,"adapter":"codex","provider":"openai",
                "model":"GPT 5.5","effort":"high","account":"main","repository":"repo","timestamp":timestamp,
                "input_tokens":100,"cache_read_tokens":20,"cache_write_tokens":0,"output_tokens":30,
                "reasoning_tokens":5,"reported_cost":0.0000004,"status":status,"error_class":null,
                "task_class":"review","route":"alias","duration_ms":1000,"provider_ms":500,
                "parent_run_id":"parent","parent_task_id":"parent-task","parent_attempt":1})
        };
        json!({
            "since":"2026-10-20T00:00:00-04:00", "until":"2026-10-22T00:00:00Z", "summary":true,
            "prices":{"models":{"codex/gpt-5.5":{"billing":"api","usd_per_million":{"input":2,"output":5}}}},
            "catalogue":{"codex":{"models":[{"id":"gpt-5.5","names":["GPT 5.5"]}]}},
            "rows":[
                receipt("e1","old-host",1,"failed","2026-10-21T10:00:00Z","s1"),
                receipt("e1","fixture-host",1,"failed","2026-10-21T11:00:00Z","s1"),
                receipt("e2","fixture-host",2,"succeeded","2026-10-21T11:30:00Z","s-native"),
                receipt("e3","fixture-host",1,"rate_limited","2026-10-21T12:00:00Z","s3"),
                receipt("old","fixture-host",1,"failed","2026-10-19T00:00:00Z","old-session"),
                {"source":"fabric","event_id":"unattributed","host":"fixture-host","session":"s4","run_id":"run-b",
                 "task_id":"task-b","attempt":1,"adapter":null,"provider":"openai","model":"<synthetic>",
                 "repository":"repo","timestamp":"2026-10-21T12:30:00Z","input_tokens":7,"status":"failed","error_class":"x"},
                {"source":"fabric","event_id":"fixture","host":"fixture-host","session":"s5","run_id":"run-b",
                 "task_id":"task-c","attempt":1,"adapter":"codex","provider":"openai","model":"fixture",
                 "repository":"repo","timestamp":"2026-10-21T12:40:00Z","input_tokens":1,"status":"failed"},
                {"source":"codex","event_id":"native","host":"fixture-host","session":"s-native","adapter":"codex",
                 "provider":"openai","model":"gpt-5.5","account":"main","timestamp":"2026-10-21T11:31:00Z",
                 "input_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0,"output_tokens":2,"reasoning_tokens":0},
                {"source":"claude","event_id":"empty-adapter","host":"fixture-host","session":"empty-adapter",
                 "adapter":"","provider":"anthropic","model":"claude-opus","timestamp":"2026-10-21T16:00:00Z",
                 "input_tokens":3,"cache_read_tokens":0,"cache_write_tokens":0,"output_tokens":1,"reasoning_tokens":0},
                {"source":"claude","event_id":"empty-model","host":"fixture-host","session":"empty-model",
                 "adapter":"claude","provider":"anthropic","model":"","timestamp":"2026-10-21T16:01:00Z",
                 "input_tokens":4,"cache_read_tokens":0,"cache_write_tokens":0,"output_tokens":1,"reasoning_tokens":0}
            ],
            "observations":[
                {"event_id":"obs-match","source":"codex","host":"old-host","provider":"openai","adapter":"codex",
                 "model":"gpt-5.5","account":"main","session":"s3","run_id":"run-a","timestamp":"2026-10-21T12:00:00Z",
                 "window":"limits","used_percent":null,"status":"rate_limited"},
                {"event_id":"obs-other","source":"codex","host":"fixture-host","provider":"openai","adapter":"codex",
                 "model":"gpt-5.5","account":"main","session":"s3","timestamp":"2026-10-21T13:00:00Z",
                 "window":"limits","used_percent":null,"status":"rate_limited"},
                {"event_id":"quota","source":"codex","host":"old-host","provider":"openai","adapter":"codex",
                 "model":"gpt-5.5","account":"main","session":"s3","timestamp":"2026-10-21T14:00:00Z",
                 "window":"5h","used_percent":55.5,"window_minutes":300,"reset_at":"2026-10-21T18:00:00Z","status":"ok"}
            ],
            "deliveries":[{"run_dir":"run-a","host":"fixture-host","run_id":"run-a","started_at":"2026-10-21T09:00:00Z",
                "risk_tier":"medium","chair_family":"codex","writer_model":"gpt-5.5","review_verdicts":"[]"}],
            "events":[{"event_id":"compact","kind":"session.compaction","host":"fixture-host","ts":"2026-10-21T15:00:00Z","session":"s1","lane":"lane-a"}]
        })
    }

    fn python_stats_source(peer: bool) -> String {
        let operation = if peer {
            "report.peer_summary(t,db,input['since'],host='peer')"
        } else {
            "report.stats(t,db,input['since'],input['prices'],summary=input['summary'],until=input['until'])"
        };
        format!(
            "{LOAD}\nimport os,tempfile\nreport=load('telemetry_report')\nstate=tempfile.mkdtemp()\nos.environ['AGENT_FABRIC_STATE_ROOT']=state\nt.host_id=lambda:'fixture-host'\nt.host_aliases=lambda:{{'fixture-host','old-host'}}\nt.same_host=lambda value,known=None: ('fixture-host' if value in ('fixture-host','old-host') else value)\nt.now=lambda:'2026-10-23T12:00:00Z'\ncosts=t.costs_owner()\ncosts.catalogue=lambda telemetry:input['catalogue']\nprice_table=input['prices']\ninput['prices']=price_table['models']\ncosts.load_prices=lambda telemetry:(input['prices'],price_table.get('free_patterns',{{}}))\ndb=t.connect()\nfor row in input['rows']:\n cols=list(row)\n db.execute('INSERT INTO usage('+','.join(cols)+') VALUES ('+','.join('?' for _ in cols)+')', [row[k] for k in cols])\nfor row in input['observations']:\n cols=list(row)\n db.execute('INSERT INTO observations('+','.join(cols)+') VALUES ('+','.join('?' for _ in cols)+')', [row[k] for k in cols])\nfor row in input['deliveries']:\n cols=list(row)\n db.execute('INSERT INTO deliveries('+','.join(cols)+') VALUES ('+','.join('?' for _ in cols)+')', [row[k] for k in cols])\nfor row in input['events']:\n cols=list(row)\n db.execute('INSERT INTO events('+','.join(cols)+') VALUES ('+','.join('?' for _ in cols)+')', [row[k] for k in cols])\ndb.commit()\nprint(json.dumps({operation}))"
        )
    }

    fn first_difference(path: &str, actual: &Value, expected: &Value) -> Option<String> {
        match (actual, expected) {
            (Value::Object(actual), Value::Object(expected)) => {
                if actual.keys().collect::<Vec<_>>() != expected.keys().collect::<Vec<_>>() {
                    return Some(format!("{path}: object keys differ"));
                }
                actual.iter().find_map(|(key, value)| {
                    first_difference(&format!("{path}.{key}"), value, &expected[key])
                })
            }
            (Value::Array(actual), Value::Array(expected)) => {
                if actual.len() != expected.len() {
                    return Some(format!(
                        "{path}: lengths {} != {}",
                        actual.len(),
                        expected.len()
                    ));
                }
                actual
                    .iter()
                    .zip(expected)
                    .enumerate()
                    .find_map(|(index, (a, e))| first_difference(&format!("{path}[{index}]"), a, e))
            }
            (Value::Number(actual), Value::Number(expected)) => (actual.to_string()
                != expected.to_string())
            .then(|| format!("{path}: {actual} != {expected}")),
            _ => (actual != expected).then(|| format!("{path}: {actual} != {expected}")),
        }
    }

    #[test]
    fn ledger_stats_and_peer_projection_match_python() {
        let input = stats_input();
        let prices: Prices = serde_json::from_value(input["prices"].clone()).unwrap();
        let catalogue: Catalogue = serde_json::from_value(input["catalogue"].clone()).unwrap();
        let services = FixtureServices;
        for (peer, since, summary, until) in [
            (
                false,
                Some("2026-10-20T00:00:00-04:00"),
                false,
                Some("2026-10-22T00:00:00Z"),
            ),
            (
                false,
                Some("2026-10-20T00:00:00-04:00"),
                true,
                Some("2026-10-22T00:00:00Z"),
            ),
            (false, None, false, None),
            (false, None, true, None),
            (false, None, false, Some("2026-10-22T00:00:00Z")),
            (false, Some("2026-10-20T00:00:00-04:00"), false, None),
            (true, Some("2026-10-20T00:00:00-04:00"), true, None),
        ] {
            let mut fixture = input.clone();
            fixture["since"] = since
                .map(|value| Value::String(value.into()))
                .unwrap_or(Value::Null);
            fixture["summary"] = Value::Bool(summary);
            fixture["until"] = until
                .map(|value| Value::String(value.into()))
                .unwrap_or(Value::Null);
            let source = python_stats_source(peer);
            let Some(expected) = python(&source, &fixture) else {
                return;
            };
            let mut db = fixture_db(&fixture);
            let actual = if peer {
                peer_summary(
                    &mut db,
                    &services,
                    &prices,
                    &catalogue,
                    fixture["since"].as_str(),
                    Some("peer"),
                )
                .unwrap()
            } else {
                stats(
                    &mut db,
                    &services,
                    &prices,
                    &catalogue,
                    fixture["since"].as_str(),
                    fixture["summary"].as_bool().unwrap_or(false),
                    fixture["until"].as_str(),
                )
                .unwrap()
            };
            if let Some(diff) = first_difference("$", &actual, &expected) {
                panic!(
                    "{diff}\nactual session_usage: {}\nexpected session_usage: {}",
                    actual["session_usage"], expected["session_usage"]
                );
            }
            equal(&actual, &expected);
        }
    }

    #[test]
    fn public_snapshot_only_keeps_allowlisted_metadata() {
        let value = json!({"schema":"provenant.quota.v1","provider":"codex","account":"main","host":"h1",
            "plan":null,"status":"ok","secret":"hide","windows":[
                {"name":"five_hours","used_percent":0,"window_minutes":300,"reset_at":null,"secret":"hide"},
                "not-a-window"],"credits":{"reset_credits":true,"source":"manual"}});
        let actual = public_snapshot(&FixtureServices, &value).unwrap();
        assert_eq!(
            actual,
            json!({"schema":"provenant.quota.v1","provider":"codex","account":"main","host":"h1",
            "plan":null,"status":"ok","windows":[{"name":"five_hours","used_percent":0,"window_minutes":300,"reset_at":null}]})
        );
    }

    #[test]
    fn public_snapshot_rejects_malformed_container_shapes() {
        assert!(matches!(
            public_snapshot(&FixtureServices, &Value::Null),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            public_snapshot(&FixtureServices, &json!({"windows": null})),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn public_snapshot_matches_python_projection() {
        let value = json!({"schema":"provenant.quota.v1","provider":"codex","account":"default","host":"studio",
            "observed_at":"2026-10-23T11:00:00Z","status":"ok","windows":[{"name":"budget","used_percent":5,
            "window_minutes":60,"reset_at":null,"raw_kind":"weekly","window_start":"2026-10-22T00:00:00Z","advisory":true}],
            "credits":{"reset_credits":3,"source":"manual"}});
        let source = format!("{LOAD}\nreport=load('telemetry_report')\nprint(json.dumps(report.public_snapshot(t,input['value'])))");
        let Some(expected) = python(&source, &json!({"value": value})) else {
            return;
        };
        equal(
            &public_snapshot(&FixtureServices, &value).unwrap(),
            &expected,
        );
    }
}
