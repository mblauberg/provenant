//! Provider quota wire decoding. Effects and credentials stay at the supervisor edge.
use provenant_quota::{
    AccountScope, QuotaDetails, QuotaEffectKind, QuotaError, QuotaProbeInput, QuotaProbePlan,
    QuotaSnapshot, QuotaStatus, QuotaWindow,
};
use serde_json::Value;

#[cfg(test)]
mod tests;

fn effect_kind(adapter: &str) -> Result<QuotaEffectKind, QuotaError> {
    use provenant_records::{AdapterSpec, QuotaProbeKind};
    Ok(
        match AdapterSpec::get(adapter)
            .ok_or(QuotaError::Disabled)?
            .quota_probe
        {
            QuotaProbeKind::HttpGet => QuotaEffectKind::HttpGet,
            QuotaProbeKind::Command => QuotaEffectKind::Command,
            QuotaProbeKind::LocalLedger => QuotaEffectKind::LocalLedger,
        },
    )
}

pub fn plan(
    adapter: &str,
    account: &AccountScope,
    input: &QuotaProbeInput,
) -> Result<QuotaProbePlan, QuotaError> {
    account.validate()?;
    if account.adapter != adapter {
        return Err(QuotaError::Invalid("quota adapter identity"));
    }
    let kind = effect_kind(adapter)?;
    Ok(QuotaProbePlan {
        kind,
        account: account.clone(),
        input: input.clone(),
    })
}

pub fn decode(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    let account = &plan.account;
    account.validate()?;
    if capture.overflowed || capture.exit_code.is_some_and(|code| code != 0) {
        return Err(QuotaError::Invalid("provider quota capture"));
    }
    let adapter = account.adapter.as_str();
    let kind = effect_kind(adapter)?;
    let effect_matches =
        plan.kind == kind && (kind != QuotaEffectKind::Command || capture.exit_code == Some(0));
    if !effect_matches {
        return Err(QuotaError::Invalid("provider quota effect"));
    }

    let envelope: Value = serde_json::from_slice(&capture.stdout)
        .map_err(|_| QuotaError::Invalid("provider quota capture"))?;
    let payload = envelope
        .get("payload")
        .filter(|value| value.is_object())
        .ok_or(QuotaError::Invalid("provider quota payload"))?;
    let mut details = match envelope.get("details") {
        Some(Value::Null) | None => None,
        Some(value) => Some(
            serde_json::from_value::<QuotaDetails>(value.clone())
                .map_err(|_| QuotaError::Invalid("provider quota details"))?,
        ),
    };
    let mut windows = Vec::new();
    match adapter {
        "claude" => decode_claude(payload, &mut windows, &mut details),
        "codex" => decode_codex(payload, &mut windows, &mut details),
        "agy" => decode_agy(payload, &mut windows)?,
        "cursor" => decode_cursor(payload, &mut windows, &mut details),
        "opencode" => {
            decode_opencode(payload, &mut windows);
            if account.account_key != "zen" {
                let details = details_mut(&mut details);
                details.source = Some("local-ledger".into());
                details.coverage = Some("partial".into());
                details.note = Some(
                    "Partial lower bound: this host only; other devices are not counted".into(),
                );
            }
        }
        _ => return Err(QuotaError::Disabled),
    }

    let status = snapshot_status(adapter, payload, &windows, details.as_ref());
    let snapshot = QuotaSnapshot {
        schema_version: 1,
        account: account.clone(),
        revision: 0,
        status,
        observed_unix_seconds: plan.input.now_unix_seconds,
        source_ref: format!("quota.provider.{adapter}"),
        authority_ref: plan.input.authority_ref.clone(),
        windows,
        details,
    };
    snapshot.validate()?;
    Ok(snapshot)
}

fn decode_claude(
    payload: &Value,
    windows: &mut Vec<QuotaWindow>,
    details: &mut Option<QuotaDetails>,
) {
    for name in ["five_hour", "seven_day"] {
        let Some(value) = payload.get(name).and_then(Value::as_object) else {
            continue;
        };
        let Some(used) = value.get("utilization").and_then(basis_points) else {
            continue;
        };
        windows.push(QuotaWindow {
            pool: name.to_owned(),
            used_basis_points: used,
            window_seconds: Some(if name == "five_hour" { 18_000 } else { 604_800 }),
            resets_unix_seconds: value.get("resets_at").and_then(unix_seconds),
        });
    }
    if let Some(limits) = payload
        .get("limits")
        .and_then(Value::as_array)
        .filter(|limits| !limits.is_empty())
    {
        for (index, limit) in limits.iter().enumerate() {
            let Some(limit) = limit.as_object() else {
                continue;
            };
            if limit.get("is_active") == Some(&Value::Bool(false)) {
                continue;
            }
            let Some(used) = limit.get("percent").and_then(basis_points) else {
                continue;
            };
            let model = limit
                .get("scope")
                .and_then(|scope| scope.get("model"))
                .and_then(|model| {
                    model
                        .get("id")
                        .and_then(Value::as_str)
                        .or_else(|| model.get("display_name").and_then(Value::as_str))
                })
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            let kind = limit
                .get("kind")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("limit");
            let group = limit.get("group").and_then(Value::as_str).unwrap_or("");
            let pool = if kind == "weekly_scoped" {
                format!(
                    "weekly_scoped:{}",
                    model.unwrap_or_else(|| (index + 1).to_string())
                )
            } else if group == "session" || kind == "session" {
                "five_hour".into()
            } else if group == "weekly" || kind == "weekly_all" {
                "seven_day".into()
            } else {
                model.map_or_else(|| kind.to_owned(), |model| format!("{kind}:{model}"))
            };
            let pool = if windows.iter().any(|window| window.pool == pool) {
                pool
            } else {
                unique_pool(windows, pool)
            };
            let row = QuotaWindow {
                pool: pool.clone(),
                used_basis_points: used,
                window_seconds: limit.get("window_minutes").and_then(minutes_to_seconds).or(
                    match group {
                        "session" => Some(18_000),
                        "weekly" => Some(604_800),
                        _ => None,
                    },
                ),
                resets_unix_seconds: limit.get("resets_at").and_then(unix_seconds),
            };
            if let Some(existing) = windows.iter_mut().find(|window| window.pool == pool) {
                *existing = row;
            } else {
                windows.push(row);
            }
        }
    }

    if let Some(balance) = payload
        .get("spend")
        .and_then(|spend| spend.get("balance"))
        .and_then(decimal_balance)
    {
        details_mut(details).credit_balance = Some(balance);
    }
}

fn decode_codex(
    payload: &Value,
    windows: &mut Vec<QuotaWindow>,
    details: &mut Option<QuotaDetails>,
) {
    let root = payload
        .get("result")
        .filter(|result| result.is_object())
        .unwrap_or(payload);
    let by_id = root
        .get("rateLimitsByLimitId")
        .filter(|value| value.is_object());
    let main = nonempty_object(root.get("rateLimits"))
        .or_else(|| nonempty_object(root.get("rate_limit")))
        .or_else(|| by_id.and_then(|map| map.get("codex")))
        .unwrap_or(&Value::Null);

    decode_codex_group(main, "", windows);
    if let Some(by_id) = by_id.and_then(Value::as_object) {
        for (key, group) in by_id {
            if key == "codex" || !group.is_object() {
                continue;
            }
            decode_codex_group(group, &format!("{}.", slug(key)), windows);
        }
    }
    if let Some(extra) = root.get("additional_rate_limits").and_then(Value::as_array) {
        for (index, group) in extra.iter().enumerate() {
            let Some(group) = group.as_object() else {
                continue;
            };
            let name = group
                .get("metered_feature")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("extra-{index}"));
            decode_codex_group(
                &Value::Object(group.clone().into_iter().collect()),
                &format!("{}.", slug(&name)),
                windows,
            );
        }
    }

    let reset_source = root
        .get("rateLimitResetCredits")
        .or_else(|| root.get("rate_limit_reset_credits"));
    let reset_credits = reset_source
        .and_then(|value| {
            value
                .get("availableCount")
                .or_else(|| value.get("available_count"))
        })
        .and_then(Value::as_u64);
    if let Some(count) = reset_credits {
        details_mut(details).reset_credits = Some(count);
    }
    if let Some(credits) = reset_source
        .and_then(|value| value.get("credits"))
        .and_then(Value::as_array)
    {
        let mut expiries = credits
            .iter()
            .filter(|credit| credit.get("status").and_then(Value::as_str) == Some("available"))
            .filter_map(|credit| credit.get("expiresAt").and_then(unix_seconds))
            .collect::<Vec<_>>();
        expiries.sort_unstable();
        expiries.dedup();
        if !expiries.is_empty() {
            details_mut(details).reset_expiries_unix_seconds = expiries;
        }
    }
    let credits = root.get("credits").or_else(|| main.get("credits"));
    if let Some(balance) = credits
        .and_then(|credits| credits.get("balance"))
        .and_then(decimal_balance)
    {
        details_mut(details).credit_balance = Some(balance);
    } else if credits.is_some_and(|credits| {
        credits
            .get("hasCredits")
            .or_else(|| credits.get("has_credits"))
            == Some(&Value::Bool(true))
            && credits.get("unlimited") == Some(&Value::Bool(true))
    }) {
        details_mut(details).credit_balance = Some("unlimited".into());
    }
    if details.as_ref().is_none_or(|value| value.plan.is_none())
        && let Some(plan) = main
            .get("planType")
            .or_else(|| main.get("plan_type"))
            .or_else(|| root.get("plan_type"))
            .and_then(Value::as_str)
    {
        details_mut(details).plan = Some(plan.to_owned());
    }
    if codex_credit_blocked(payload) {
        details_mut(details).note = Some("provider spend control reached".into());
    }
}

fn decode_codex_group(group: &Value, prefix: &str, windows: &mut Vec<QuotaWindow>) {
    let group = group
        .get("rate_limit")
        .filter(|value| value.is_object())
        .unwrap_or(group);
    for name in ["primary", "secondary"] {
        let window = group
            .get(name)
            .filter(|value| !value.is_null() && *value != &Value::Object(Default::default()))
            .or_else(|| group.get(format!("{name}_window")));
        let Some(window) = window else { continue };
        let used = window
            .get("usedPercent")
            .or_else(|| window.get("used_percent"))
            .and_then(basis_points);
        let Some(used) = used else { continue };
        let resets = window
            .get("resetsAt")
            .or_else(|| window.get("reset_at"))
            .and_then(unix_seconds);
        let pool = unique_pool(windows, format!("{prefix}{name}"));
        windows.push(QuotaWindow {
            pool,
            used_basis_points: used,
            window_seconds: window_duration_seconds(window),
            resets_unix_seconds: resets,
        });
    }
}

fn decode_agy(payload: &Value, windows: &mut Vec<QuotaWindow>) -> Result<(), QuotaError> {
    if payload.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return Err(QuotaError::Invalid("agy quota response"));
    }
    let data = payload
        .get("command")
        .and_then(|command| command.get("data"))
        .filter(|data| data.is_object())
        .unwrap_or(&Value::Null);
    append_agy_buckets(data.get("buckets"), None, windows);
    if let Some(groups) = data.get("groups").and_then(Value::as_array) {
        for group in groups {
            let Some(name) = group.get("name").and_then(Value::as_str) else {
                continue;
            };
            let pool = slug(name).split('-').next().unwrap_or("").to_owned();
            append_agy_buckets(group.get("buckets"), Some(&pool), windows);
        }
    }
    Ok(())
}

fn append_agy_buckets(buckets: Option<&Value>, pool: Option<&str>, windows: &mut Vec<QuotaWindow>) {
    let Some(buckets) = buckets.and_then(Value::as_array) else {
        return;
    };
    for bucket in buckets {
        if bucket.get("disabled") == Some(&Value::Bool(true)) {
            continue;
        }
        let Some(remaining) = bucket.get("remaining_fraction").and_then(number) else {
            continue;
        };
        if !(0.0..=1.0).contains(&remaining) {
            continue;
        }
        let Some(used) = basis_points_from_percent((1.0 - remaining) * 100.0) else {
            continue;
        };
        let window_name = bucket
            .get("window")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let id = bucket
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let name = match (pool.filter(|p| !p.is_empty()), window_name) {
            (Some(pool), Some(window)) => format!("{pool}.{window}"),
            _ => id.map(str::to_owned).unwrap_or_else(|| "quota".to_owned()),
        };
        let name = unique_pool(windows, name);
        windows.push(QuotaWindow {
            pool: name,
            used_basis_points: used,
            window_seconds: bucket
                .get("window")
                .and_then(Value::as_str)
                .and_then(named_window_seconds),
            resets_unix_seconds: bucket.get("reset_time").and_then(unix_seconds),
        });
    }
}

fn decode_cursor(
    payload: &Value,
    windows: &mut Vec<QuotaWindow>,
    details: &mut Option<QuotaDetails>,
) {
    let individual = payload
        .get("individualUsage")
        .filter(|value| value.is_object())
        .unwrap_or(&Value::Null);
    let team = payload
        .get("teamUsage")
        .filter(|value| value.is_object())
        .unwrap_or(&Value::Null);
    let usage = [
        individual.get("plan"),
        individual.get("overall"),
        team.get("pooled"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        let used = value.get("used").and_then(number)?;
        let limit = value.get("limit").and_then(number)?;
        (limit > 0.0).then_some((used / limit) * 100.0)
    });
    if let Some(used) = usage.and_then(basis_points_from_percent) {
        windows.push(QuotaWindow {
            pool: "billing_cycle".into(),
            used_basis_points: used,
            window_seconds: payload
                .get("billingCycleStart")
                .and_then(unix_seconds)
                .zip(payload.get("billingCycleEnd").and_then(unix_seconds))
                .and_then(|(start, end)| u64::try_from(end.saturating_sub(start)).ok())
                .filter(|duration| *duration > 0),
            resets_unix_seconds: payload.get("billingCycleEnd").and_then(unix_seconds),
        });
    }
    if details.as_ref().is_none_or(|value| value.plan.is_none())
        && let Some(plan) = payload
            .get("membershipType")
            .or_else(|| payload.get("limitType"))
            .and_then(Value::as_str)
    {
        details_mut(details).plan = Some(plan.to_owned());
    }
}

fn decode_opencode(payload: &Value, windows: &mut Vec<QuotaWindow>) {
    let Some(rows) = payload.get("windows").and_then(Value::as_array) else {
        return;
    };
    for row in rows {
        let Some(pool) = row
            .get("pool")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some(used) = row
            .get("used_percent")
            .or_else(|| row.get("usedPercent"))
            .and_then(basis_points)
        else {
            continue;
        };
        let pool = unique_pool(windows, pool.to_owned());
        windows.push(QuotaWindow {
            pool,
            used_basis_points: used,
            window_seconds: row
                .get("window_seconds")
                .and_then(Value::as_u64)
                .or_else(|| row.get("window_minutes").and_then(minutes_to_seconds))
                .or_else(|| {
                    named_window_seconds(row.get("pool").and_then(Value::as_str).unwrap_or(""))
                }),
            resets_unix_seconds: row
                .get("resets_at")
                .or_else(|| row.get("reset_at"))
                .and_then(unix_seconds),
        });
    }
}

fn snapshot_status(
    adapter: &str,
    payload: &Value,
    windows: &[QuotaWindow],
    details: Option<&QuotaDetails>,
) -> QuotaStatus {
    if windows.is_empty() {
        return QuotaStatus::Unknown;
    }
    if adapter == "agy" {
        let mut pools = std::collections::BTreeMap::<String, bool>::new();
        for window in windows {
            let pool = window
                .pool
                .split_once('.')
                .map_or("", |(pool, _)| pool)
                .to_owned();
            *pools.entry(pool).or_insert(true) &= window.used_basis_points < 10_000;
        }
        return if pools.values().any(|available| *available) {
            QuotaStatus::Available
        } else {
            QuotaStatus::Exhausted
        };
    }
    let binding = windows
        .iter()
        .filter(|window| {
            !(adapter == "claude" && window.pool.starts_with("weekly_scoped:"))
                && !(adapter == "codex" && window.pool.contains('.'))
        })
        .collect::<Vec<_>>();
    if binding.is_empty() {
        return QuotaStatus::Unknown;
    }
    if binding
        .iter()
        .any(|window| window.used_basis_points >= 10_000)
    {
        let provider_credits = provenant_records::AdapterSpec::get(adapter)
            .is_some_and(|spec| spec.quota_credits)
            && details.is_some_and(|details| positive_balance(details.credit_balance.as_deref()));
        let credit_blocked = adapter == "codex" && codex_credit_blocked(payload);
        if provider_credits && !credit_blocked {
            QuotaStatus::CreditOnly
        } else {
            QuotaStatus::Exhausted
        }
    } else {
        QuotaStatus::Available
    }
}

fn positive_balance(balance: Option<&str>) -> bool {
    balance.is_some_and(|balance| {
        balance == "unlimited" || balance.parse::<f64>().is_ok_and(|value| value > 0.0)
    })
}

fn codex_credit_blocked(payload: &Value) -> bool {
    let root = payload
        .get("result")
        .filter(|result| result.is_object())
        .unwrap_or(payload);
    let main = nonempty_object(root.get("rateLimits"))
        .or_else(|| nonempty_object(root.get("rate_limit")))
        .unwrap_or(&Value::Null);
    root.get("spend_control")
        .and_then(|value| value.get("reached"))
        == Some(&Value::Bool(true))
        || root
            .get("credits")
            .or_else(|| main.get("credits"))
            .and_then(|credits| credits.get("overage_limit_reached"))
            == Some(&Value::Bool(true))
}

fn details_mut(details: &mut Option<QuotaDetails>) -> &mut QuotaDetails {
    details.get_or_insert_with(QuotaDetails::default)
}

fn nonempty_object(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| value.as_object().is_some_and(|map| !map.is_empty()))
}

fn unique_pool(windows: &[QuotaWindow], pool: String) -> String {
    if windows.iter().all(|window| window.pool != pool) {
        return pool;
    }
    let mut suffix = 2_u64;
    loop {
        let candidate = format!("{pool}:{suffix}");
        if windows.iter().all(|window| window.pool != candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for byte in text.bytes().map(|byte| byte.to_ascii_lowercase()) {
        if byte.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('-');
            }
            out.push(char::from(byte));
            separator = false;
        } else {
            separator = true;
        }
    }
    out
}

fn basis_points(value: &Value) -> Option<u64> {
    number(value).and_then(basis_points_from_percent)
}

fn window_duration_seconds(window: &Value) -> Option<u64> {
    ["windowDurationMins", "window_minutes"]
        .iter()
        .find_map(|key| window.get(key).and_then(minutes_to_seconds))
        .or_else(|| window.get("limit_window_seconds").and_then(Value::as_u64))
}

fn minutes_to_seconds(value: &Value) -> Option<u64> {
    let minutes = number(value)?;
    if !minutes.is_finite() || minutes <= 0.0 || minutes > (u64::MAX as f64 / 60.0) {
        return None;
    }
    Some((minutes * 60.0).round() as u64)
}

fn named_window_seconds(value: &str) -> Option<u64> {
    match value.to_ascii_lowercase().as_str() {
        "5h" | "five_hour" | "session" => Some(18_000),
        "hourly" | "one_hour" => Some(3_600),
        "daily" | "one_day" => Some(86_400),
        "weekly" | "seven_day" => Some(604_800),
        "thirty_day" | "monthly" => Some(2_592_000),
        _ => {
            let lower = value.to_ascii_lowercase();
            let (amount, unit) = lower.split_at(lower.find(|ch: char| !ch.is_ascii_digit())?);
            let amount = amount.parse::<u64>().ok()?;
            let multiplier = match unit {
                "m" => 60,
                "h" => 3_600,
                "d" => 86_400,
                "w" => 604_800,
                _ => return None,
            };
            amount.checked_mul(multiplier)
        }
    }
}

fn basis_points_from_percent(value: f64) -> Option<u64> {
    if !value.is_finite() || !(0.0..=10_000.0).contains(&value) {
        return None;
    }
    Some((value * 100.0).round() as u64)
}

fn number(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse::<f64>().ok()?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn decimal_balance(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Object(value) => {
            let minor = value.get("amount_minor")?.as_i64()?;
            let exponent = value.get("exponent")?.as_u64()?;
            (exponent <= 9).then(|| format_minor(minor, exponent as usize))
        }
        _ => None,
    }
}

fn format_minor(minor: i64, exponent: usize) -> String {
    if exponent == 0 {
        return minor.to_string();
    }
    let negative = minor < 0;
    let digits = minor.unsigned_abs().to_string();
    let padded = if digits.len() <= exponent {
        format!("{}{}", "0".repeat(exponent + 1 - digits.len()), digits)
    } else {
        digits
    };
    let point = padded.len() - exponent;
    format!(
        "{}{}.{}",
        if negative { "-" } else { "" },
        &padded[..point],
        &padded[point..]
    )
}

fn unix_seconds(value: &Value) -> Option<i64> {
    match value {
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                return epoch_seconds(integer as f64);
            }
            value.as_f64().and_then(epoch_seconds)
        }
        Value::String(value) => value
            .parse::<f64>()
            .ok()
            .and_then(epoch_seconds)
            .or_else(|| parse_iso_timestamp(value)),
        _ => None,
    }
}

fn epoch_seconds(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }
    let seconds = if value.abs() >= 100_000_000_000.0 {
        value / 1000.0
    } else {
        value
    };
    let rounded = seconds.round();
    (rounded >= i64::MIN as f64 && rounded <= i64::MAX as f64).then_some(rounded as i64)
}

fn parse_iso_timestamp(value: &str) -> Option<i64> {
    value
        .trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|timestamp| timestamp.as_second())
}
