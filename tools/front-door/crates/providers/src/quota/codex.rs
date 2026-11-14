use super::*;

pub(crate) fn decode_source(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    super::decode_source(
        "codex",
        plan,
        capture,
        |payload, windows, details| {
            decode_codex(payload, windows, details);
            Ok(())
        },
        status,
    )
}

fn status(
    adapter: &str,
    payload: &Value,
    windows: &[QuotaWindow],
    details: Option<&QuotaDetails>,
) -> QuotaStatus {
    let blocked = codex_credit_blocked(payload) || codex_credits_unspendable(payload);
    let can_use_credits = !blocked
        && provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| spec.quota_credits)
        && details
            .is_some_and(|details| super::positive_balance(details.credit_balance.as_deref()));
    super::status_from_windows(
        windows,
        |window| !window.pool.contains('.'),
        can_use_credits,
    )
}

fn codex_credit_blocked(payload: &Value) -> bool {
    let root = payload
        .get("result")
        .filter(|result| result.is_object())
        .unwrap_or(payload);
    let main = nonempty_object(root.get("rateLimits"))
        .or_else(|| nonempty_object(root.get("rate_limit")))
        .unwrap_or(&Value::Null);
    let credits = root.get("credits").or_else(|| main.get("credits"));
    root.get("spend_control")
        .and_then(|value| value.get("reached"))
        == Some(&Value::Bool(true))
        || credits.and_then(|credits| credits.get("overage_limit_reached"))
            == Some(&Value::Bool(true))
}

/// Credits the provider says it will not spend are not spendable. Only the
/// status choice asks: an account without purchased credits is not blocked.
fn codex_credits_unspendable(payload: &Value) -> bool {
    let root = payload
        .get("result")
        .filter(|result| result.is_object())
        .unwrap_or(payload);
    let main = nonempty_object(root.get("rateLimits"))
        .or_else(|| nonempty_object(root.get("rate_limit")))
        .unwrap_or(&Value::Null);
    root.get("credits")
        .or_else(|| main.get("credits"))
        .and_then(|credits| {
            credits
                .get("hasCredits")
                .or_else(|| credits.get("has_credits"))
        })
        == Some(&Value::Bool(false))
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
