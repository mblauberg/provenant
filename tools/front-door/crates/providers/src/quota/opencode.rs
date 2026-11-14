use super::*;

pub(crate) fn decode_source(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    super::decode_source(
        "opencode",
        plan,
        capture,
        |payload, windows, details| {
            decode_opencode(payload, windows);
            if !provenant_records::AdapterSpec::get(&plan.account.adapter).is_some_and(|spec| {
                spec.is_free_plan(details.as_ref().and_then(|details| details.plan.as_deref()))
            }) {
                let details = super::details_mut(details);
                details.source = Some("local-ledger".into());
                details.coverage = Some("partial".into());
                details.note = Some(
                    "Partial lower bound: this host only; other devices are not counted".into(),
                );
            }
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
    super::status_from_windows(windows, |_| true, can_use_credits)
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
