use super::*;

pub(crate) fn decode_source(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    super::decode_source(
        "cursor",
        plan,
        capture,
        |payload, windows, details| {
            decode_cursor(payload, windows, details);
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
