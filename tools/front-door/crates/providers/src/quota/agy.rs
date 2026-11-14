use super::*;

pub(crate) fn decode_source(
    plan: &QuotaProbePlan,
    capture: &provenant_quota::ProbeCapture,
) -> Result<QuotaSnapshot, QuotaError> {
    super::decode_source("agy", plan, capture, decode_agy, status)
}

fn status(_: &str, _: &Value, windows: &[QuotaWindow], _: Option<&QuotaDetails>) -> QuotaStatus {
    if windows.is_empty() {
        return QuotaStatus::Unknown;
    }
    let mut pools = std::collections::BTreeMap::<String, bool>::new();
    for window in windows {
        let pool = window
            .pool
            .split_once('.')
            .map_or("", |(pool, _)| pool)
            .to_owned();
        *pools.entry(pool).or_insert(true) &= window.used_basis_points < 10_000;
    }
    if pools.values().any(|available| *available) {
        QuotaStatus::Available
    } else {
        QuotaStatus::Exhausted
    }
}

fn decode_agy(
    payload: &Value,
    windows: &mut Vec<QuotaWindow>,
    details: &mut Option<QuotaDetails>,
) -> Result<(), QuotaError> {
    if payload.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return Err(QuotaError::Invalid("agy quota response"));
    }
    let models = extract_agy_models(Some(payload), None);
    if !models.is_empty() {
        let d = details_mut(details);
        d.models = models;
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

pub(crate) fn extract_agy_models(payload: Option<&Value>, stderr: Option<&str>) -> Vec<String> {
    let mut models = Vec::new();
    if let Some(payload) = payload {
        let root = payload.get("payload").unwrap_or(payload);
        let data = root
            .get("command")
            .and_then(|c| c.get("data"))
            .unwrap_or(root);
        if let Some(arr) = data.get("models").and_then(Value::as_array) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    let s = s.trim();
                    if !s.is_empty() && !models.iter().any(|m: &String| m.eq_ignore_ascii_case(s)) {
                        models.push(s.to_owned());
                    }
                }
            }
        }
        if let Some(groups) = data.get("groups").and_then(Value::as_array) {
            for group in groups {
                if let Some(arr) = group.get("models").and_then(Value::as_array) {
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            let s = s.trim();
                            if !s.is_empty()
                                && !models.iter().any(|m: &String| m.eq_ignore_ascii_case(s))
                            {
                                models.push(s.to_owned());
                            }
                        }
                    }
                }
                if let Some(desc) = group.get("description").and_then(Value::as_str) {
                    const PREFIX: &str = "models within this group:";
                    let lower = desc.to_ascii_lowercase();
                    if let Some(idx) = lower.find(PREFIX) {
                        let list = &desc[idx + PREFIX.len()..];
                        for item in list.split(',') {
                            let item = item.trim().trim_end_matches('.');
                            if !item.is_empty()
                                && !models.iter().any(|m: &String| m.eq_ignore_ascii_case(item))
                            {
                                models.push(item.to_owned());
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(stderr) = stderr {
        let mut in_models = false;
        for line in stderr.lines() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("available models:") || lower.starts_with("offered models:") {
                in_models = true;
                continue;
            }
            if in_models {
                if trimmed.is_empty() {
                    in_models = false;
                    continue;
                }
                let clean = trimmed.trim_start_matches(['-', '*', '•']).trim();
                if !clean.is_empty()
                    && !models
                        .iter()
                        .any(|m: &String| m.eq_ignore_ascii_case(clean))
                {
                    models.push(clean.to_owned());
                }
            }
        }
    }
    models
}
