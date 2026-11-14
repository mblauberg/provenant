use super::*;

pub(crate) fn decode_source(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    super::decode_source(
        "claude",
        plan,
        capture,
        |payload, windows, details| {
            decode_claude(payload, windows, details);
            Ok(())
        },
        status,
    )
}

fn status(
    adapter: &str,
    _: &Value,
    windows: &[QuotaWindow],
    details: Option<&QuotaDetails>,
) -> QuotaStatus {
    let can_use_credits = provenant_records::AdapterSpec::get(adapter)
        .is_some_and(|spec| spec.quota_credits)
        && details
            .is_some_and(|details| super::positive_balance(details.credit_balance.as_deref()));
    super::status_from_windows(
        windows,
        |window| !window.pool.starts_with("weekly_scoped:"),
        can_use_credits,
    )
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
