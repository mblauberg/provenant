use std::{
    collections::{HashMap, HashSet},
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use jiff::{Span, Timestamp, tz::TimeZone};
use rusqlite::{Connection, OpenFlags, params_from_iter, types::Value as SqlValue};
use serde_json::{Value, json};

use provenant_quota::ProbeFailure;

const PAID_PROVIDER: &str = "opencode-go";
const FIVE_HOURS_SECONDS: i64 = 5 * 60 * 60;
const WEEK_SECONDS: i64 = 7 * 24 * 60 * 60;
const ROLLING_MONTH_SECONDS: i64 = 30 * 24 * 60 * 60;
const ERROR_LOOKBACK_SECONDS: i64 = 32 * 24 * 60 * 60;
const MAX_ERROR_ROWS: i64 = 4096;
const MAX_ERROR_BODY_CHARS: usize = 4096;

#[derive(Clone, Copy)]
struct WindowFrame {
    pool: &'static str,
    start: Timestamp,
    reset: Option<Timestamp>,
    minutes: i64,
    share: f64,
}

pub(super) fn capture(
    identity_dir: &Path,
    options: &Value,
    now: i64,
    deadline: i64,
) -> Result<Value, ProbeFailure> {
    let wall_deadline =
        Instant::now() + Duration::from_secs(deadline.saturating_sub(now).max(0) as u64);
    let now = Timestamp::from_second(now).map_err(|_| ProbeFailure::Malformed)?;
    let now_ms = now.as_millisecond();
    let frames = frames(options, now).ok_or(ProbeFailure::Malformed)?;
    let providers = providers(options);
    let database = identity_dir.join("opencode.db");
    let db = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ProbeFailure::Malformed)?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(|_| ProbeFailure::Malformed)?;

    db.progress_handler(1000, Some(move || Instant::now() >= wall_deadline))
        .map_err(|_| ProbeFailure::Network)?;

    let tables = table_names(&db).map_err(|_| ProbeFailure::Malformed)?;
    let since = frames.map(|frame| frame.start);
    let spends = spend(&db, now_ms, &providers, since, &tables);
    let errors = limit_errors(&db, now_ms, &providers);
    let cap = monthly_cap(options);

    let windows = frames
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| {
            let mut used = spends
                .as_ref()
                .and_then(|totals| totals.get(index).copied())
                .flatten()
                .and_then(|dollars| cap.map(|cap| dollars / (cap * frame.share) * 100.0));
            let mut reset = frame.reset.map(Timestamp::as_second);

            if let Some(failed_ms) = errors.get(frame.pool).copied()
                && failed_ms >= frame.start.as_millisecond()
            {
                used = Some(used.unwrap_or(0.0).max(100.0));
                if reset.is_none() {
                    reset = (failed_ms / 1000).checked_add(frame.minutes * 60);
                }
            }

            let used = used.filter(|value| value.is_finite())?;
            Some(json!({
                "pool": frame.pool,
                "used_percent": used,
                "resets_at": reset,
                "window_seconds": frame.minutes * 60,
            }))
        })
        .collect::<Vec<_>>();

    if Instant::now() >= wall_deadline {
        return Err(ProbeFailure::Network);
    }
    Ok(json!({ "windows": windows }))
}

fn frames(options: &Value, now: Timestamp) -> Option<[WindowFrame; 3]> {
    let limits = options.get("limits");
    let anchor = limits
        .and_then(|value| value.get("cycle_anchor"))
        .and_then(parse_anchor);
    let five_hour = WindowFrame {
        pool: "five_hour",
        start: now
            .checked_sub(Span::new().seconds(FIVE_HOURS_SECONDS))
            .ok()?,
        reset: None,
        minutes: 5 * 60,
        share: 0.2,
    };

    let (week_start, week_end) = match anchor {
        Some(cycle_anchor) => {
            let weekly_anchor = limits
                .and_then(|value| value.get("weekly_anchor"))
                .and_then(parse_anchor)
                .unwrap_or(cycle_anchor);
            let elapsed = now.duration_since(weekly_anchor).as_secs_f64();
            let weeks = (elapsed / WEEK_SECONDS as f64).floor() as i64;
            let start = weekly_anchor
                .checked_add(Span::new().seconds(weeks.checked_mul(WEEK_SECONDS)?))
                .ok()?;
            let end = start.checked_add(Span::new().seconds(WEEK_SECONDS)).ok()?;
            (Some(start), Some(end))
        }
        None => (None, None),
    };

    let (month_start, month_end) = match anchor {
        Some(anchor) => fixed_month_cycle(anchor, now)?,
        None => (
            now.checked_sub(Span::new().seconds(ROLLING_MONTH_SECONDS))
                .ok()?,
            now,
        ),
    };

    let weekly = match (week_start, week_end) {
        (Some(start), Some(end)) => WindowFrame {
            pool: "seven_day",
            start,
            reset: Some(end),
            minutes: 7 * 24 * 60,
            share: 0.5,
        },
        _ => WindowFrame {
            pool: "seven_day",
            start: now.checked_sub(Span::new().seconds(WEEK_SECONDS)).ok()?,
            reset: None,
            minutes: 7 * 24 * 60,
            share: 0.5,
        },
    };
    let monthly = WindowFrame {
        pool: "thirty_day",
        start: month_start,
        reset: anchor.map(|_| month_end),
        minutes: if anchor.is_some() {
            month_end
                .as_second()
                .saturating_sub(month_start.as_second())
                / 60
        } else {
            30 * 24 * 60
        },
        share: 1.0,
    };
    Some([five_hour, weekly, monthly])
}

fn fixed_month_cycle(anchor: Timestamp, now: Timestamp) -> Option<(Timestamp, Timestamp)> {
    let anchor_zoned = anchor.to_zoned(TimeZone::UTC);
    let now_zoned = now.to_zoned(TimeZone::UTC);
    let mut months = (i64::from(now_zoned.year()) - i64::from(anchor_zoned.year())) * 12
        + i64::from(now_zoned.month())
        - i64::from(anchor_zoned.month());
    if shift_months(anchor, months)? > now {
        months = months.checked_sub(1)?;
    }
    Some((
        shift_months(anchor, months)?,
        shift_months(anchor, months.checked_add(1)?)?,
    ))
}

fn shift_months(anchor: Timestamp, months: i64) -> Option<Timestamp> {
    anchor
        .to_zoned(TimeZone::UTC)
        .checked_add(Span::new().months(months))
        .ok()
        .map(|value| value.timestamp())
}

fn parse_anchor(value: &Value) -> Option<Timestamp> {
    if let Some(number) = value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
    {
        return parse_epoch(number);
    }

    let value = value.as_str()?.trim();
    if let Ok(timestamp) = Timestamp::from_str(value) {
        return Some(timestamp);
    }
    if let Ok(datetime) = jiff::civil::DateTime::from_str(value) {
        return datetime
            .to_zoned(TimeZone::UTC)
            .ok()
            .map(|zoned| zoned.timestamp());
    }
    jiff::civil::Date::from_str(value)
        .ok()?
        .at(0, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| zoned.timestamp())
}

fn parse_epoch(number: f64) -> Option<Timestamp> {
    if !number.is_finite() || number <= 0.0 {
        return None;
    }
    let seconds = if number > 10_000_000_000.0 {
        number / 1000.0
    } else {
        number
    };
    if seconds > 253_402_300_799.0 {
        return None;
    }
    Timestamp::from_nanosecond((seconds * 1_000_000_000.0).round() as i128).ok()
}

fn providers(options: &Value) -> Vec<String> {
    let configured = options
        .get("providers")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if configured.is_empty() {
        vec![PAID_PROVIDER.to_owned()]
    } else {
        configured
    }
}

fn monthly_cap(options: &Value) -> Option<f64> {
    let configured = options
        .get("limits")
        .and_then(|value| value.get("monthly_usd"));
    let cap = match configured {
        Some(Value::Number(value)) => value.as_f64(),
        Some(Value::String(value)) => value.parse::<f64>().ok(),
        _ => None,
    }
    .or_else(|| {
        let plan = options
            .get("plan")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("go");
        (plan.trim().eq_ignore_ascii_case("go")).then_some(32.0)
    })?;
    (cap.is_finite() && cap > 0.0).then_some(cap)
}

fn table_names(db: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = db.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    rows.collect()
}

fn spend(
    db: &Connection,
    now_ms: i64,
    providers: &[String],
    starts: [Timestamp; 3],
    tables: &HashSet<String>,
) -> Option<[Option<f64>; 3]> {
    let since_ms = starts.map(Timestamp::as_millisecond);
    let oldest = *since_ms.iter().min()?;
    let mut totals = [0.0; 3];
    let mut readable = false;

    for (table, provider_expression, assistant_only) in [
        ("message", "json_extract(data,'$.providerID')", false),
        (
            "session_message",
            "json_extract(data,'$.model.providerID')",
            true,
        ),
    ] {
        let mut sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN time_created>=? THEN json_extract(data,'$.cost') ELSE 0 END),0), \
             COALESCE(SUM(CASE WHEN time_created>=? THEN json_extract(data,'$.cost') ELSE 0 END),0), \
             COALESCE(SUM(CASE WHEN time_created>=? THEN json_extract(data,'$.cost') ELSE 0 END),0) \
             FROM {table} WHERE time_created BETWEEN ? AND ?"
        );
        if assistant_only {
            sql.push_str(" AND type='assistant'");
        } else {
            if tables.contains("session_message") {
                sql.push_str(
                    " AND NOT EXISTS (SELECT 1 FROM session_message AS v2 WHERE v2.id=message.id)",
                );
            }
        }
        sql.push_str(&format!(" AND {provider_expression} IN ("));
        sql.push_str(
            &std::iter::repeat_n("?", providers.len())
                .collect::<Vec<_>>()
                .join(","),
        );
        sql.push(')');

        let mut values = vec![
            SqlValue::Integer(since_ms[0]),
            SqlValue::Integer(since_ms[1]),
            SqlValue::Integer(since_ms[2]),
            SqlValue::Integer(oldest),
            SqlValue::Integer(now_ms),
        ];
        values.extend(providers.iter().cloned().map(SqlValue::Text));
        let row = db.query_row(&sql, params_from_iter(values.iter()), |row| {
            Ok([
                row.get::<_, f64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
            ])
        });
        let Ok(row) = row else {
            continue;
        };
        readable = true;
        for (total, value) in totals.iter_mut().zip(row) {
            *total += value;
        }
    }

    readable.then_some(totals.map(|value| value.is_finite().then_some(value)))
}

fn limit_errors(db: &Connection, now_ms: i64, providers: &[String]) -> HashMap<&'static str, i64> {
    if providers.is_empty() {
        return HashMap::new();
    }
    let oldest_ms = now_ms.saturating_sub(ERROR_LOOKBACK_SECONDS.saturating_mul(1000));
    let error_sql = format!(
        "SELECT time_created, substr(json_extract(data,'$.error.response.body'),1,{MAX_ERROR_BODY_CHARS}) \
         FROM session_message WHERE type='assistant' \
         AND json_extract(data,'$.error.type')='provider.quota' \
         AND time_created BETWEEN ? AND ? ORDER BY time_created DESC LIMIT {MAX_ERROR_ROWS}"
    );
    let mut errors = match db.prepare(&error_sql) {
        Ok(statement) => statement,
        Err(_) => return HashMap::new(),
    };
    let rows = match errors.query_map([oldest_ms, now_ms], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
    }) {
        Ok(rows) => rows,
        Err(_) => return HashMap::new(),
    };
    let mut candidates = Vec::new();
    for row in rows {
        match row {
            Ok(row) => candidates.push(row),
            Err(_) => return HashMap::new(),
        }
    }

    let marks = std::iter::repeat_n("?", providers.len())
        .collect::<Vec<_>>()
        .join(",");
    let accepted_sql = format!(
        "SELECT MAX(time_created) FROM session_message WHERE type='assistant' \
         AND json_extract(data,'$.error') IS NULL AND json_extract(data,'$.cost')>0 \
         AND time_created BETWEEN ? AND ? \
         AND json_extract(data,'$.model.providerID') IN ({marks})"
    );
    let mut values = vec![SqlValue::Integer(oldest_ms), SqlValue::Integer(now_ms)];
    values.extend(providers.iter().cloned().map(SqlValue::Text));
    let accepted = match db.query_row(&accepted_sql, params_from_iter(values.iter()), |row| {
        row.get::<_, Option<i64>>(0)
    }) {
        Ok(value) => value.unwrap_or(0),
        Err(_) => return HashMap::new(),
    };

    let mut latest = HashMap::new();
    for (created, body) in candidates {
        if created <= accepted {
            continue;
        }
        let Some(body) = body else {
            continue;
        };
        let Ok(body) = serde_json::from_str::<Value>(&body) else {
            continue;
        };
        if body.pointer("/error/type").and_then(Value::as_str) != Some("GoUsageLimitError") {
            continue;
        }
        let name = body
            .get("metadata")
            .and_then(|value| value.get("limitName"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let pool = match name.as_str() {
            "weekly" => "seven_day",
            "monthly" => "thirty_day",
            _ => "five_hour",
        };
        latest.entry(pool).or_insert(created);
    }
    latest
}
