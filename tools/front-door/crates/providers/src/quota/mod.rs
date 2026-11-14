//! Provider quota wire decoding. Effects and credentials stay at the supervisor edge.
use provenant_quota::{
    AccountScope, QuotaDetails, QuotaEffectKind, QuotaError, QuotaProbeInput, QuotaProbePlan,
    QuotaSnapshot, QuotaStatus, QuotaWindow,
};
use serde_json::Value;

pub(crate) mod agy;
pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod cursor;
pub(crate) mod opencode;

#[cfg(test)]
mod tests;

fn effect_kind(adapter: &str) -> Result<QuotaEffectKind, QuotaError> {
    use provenant_records::{AdapterSpec, QuotaProbeKind};
    Ok(
        match AdapterSpec::get(adapter)
            .ok_or(QuotaError::Disabled)?
            .quota_probe
        {
            QuotaProbeKind::None => return Err(QuotaError::Disabled),
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

fn decode_source(
    adapter: &str,
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
    decode_payload: impl FnOnce(
        &Value,
        &mut Vec<QuotaWindow>,
        &mut Option<QuotaDetails>,
    ) -> Result<(), QuotaError>,
    status: impl FnOnce(&str, &Value, &[QuotaWindow], Option<&QuotaDetails>) -> QuotaStatus,
) -> Result<QuotaSnapshot, QuotaError> {
    let account = &plan.account;
    account.validate()?;
    if capture.overflowed || capture.exit_code.is_some_and(|code| code != 0) {
        return Err(QuotaError::Invalid("provider quota capture"));
    }
    if account.adapter != adapter {
        return Err(QuotaError::Invalid("quota adapter identity"));
    }
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
    decode_payload(payload, &mut windows, &mut details)?;

    let status = status(adapter, payload, &windows, details.as_ref());
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

pub(super) fn status_from_windows(
    windows: &[QuotaWindow],
    is_binding: impl Fn(&QuotaWindow) -> bool,
    can_use_credits: bool,
) -> QuotaStatus {
    let binding = windows
        .iter()
        .filter(|window| is_binding(window))
        .collect::<Vec<_>>();
    if binding.is_empty() {
        QuotaStatus::Unknown
    } else if binding
        .iter()
        .any(|window| window.used_basis_points >= 10_000)
    {
        if can_use_credits {
            QuotaStatus::CreditOnly
        } else {
            QuotaStatus::Exhausted
        }
    } else {
        QuotaStatus::Available
    }
}

pub(super) fn positive_balance(balance: Option<&str>) -> bool {
    balance.is_some_and(|balance| {
        balance == "unlimited" || balance.parse::<f64>().is_ok_and(|value| value > 0.0)
    })
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
    let lower = value.to_ascii_lowercase();
    match lower.as_str() {
        "5h" | "five_hour" | "session" => Some(18_000),
        "hourly" | "one_hour" => Some(3_600),
        "daily" | "one_day" => Some(86_400),
        "weekly" | "seven_day" => Some(604_800),
        "thirty_day" | "monthly" => Some(2_592_000),
        _ => crate::duration::suffix_seconds(&lower),
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
