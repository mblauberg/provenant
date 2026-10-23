//! Adaptive refresh scheduling. No clock reads; activity discovery only reads metadata.
use crate::{
    time::{add_seconds, from_timestamp, iso, parse_time, seconds_between, stamp},
    *,
};
use jiff::{tz::TimeZone, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub enabled: bool,
    pub min_interval_seconds: f64,
    pub max_interval_seconds: f64,
    pub active_window_seconds: f64,
    pub jitter_fraction: f64,
    pub coalesce_seconds: f64,
    pub near_exhaustion_percent: f64,
    pub near_exhaustion_factor: f64,
    pub runs_out_horizon_factor: f64,
    pub reset_grace_seconds: f64,
    pub exhausted_recheck_seconds: f64,
    pub battery_factor: f64,
    pub low_power_factor: f64,
    pub low_power_skip_costs: Vec<String>,
    pub manual_min_age_seconds: f64,
    pub event_debounce_seconds: f64,
    pub max_parallel: u64,
    pub base_seconds: f64,
    pub max_seconds: f64,
    pub auth_seconds: f64,
    pub missing_seconds: f64,
    pub cost: String,
    pub solo: bool,
    pub ttl_active_seconds: f64,
    pub ttl_idle_seconds: f64,
    pub ttl_panel_seconds: f64,
}
const KEYS: &[&str] = &[
    "enabled",
    "min_interval_seconds",
    "max_interval_seconds",
    "active_window_seconds",
    "jitter_fraction",
    "coalesce_seconds",
    "near_exhaustion_percent",
    "near_exhaustion_factor",
    "runs_out_horizon_factor",
    "reset_grace_seconds",
    "exhausted_recheck_seconds",
    "battery_factor",
    "low_power_factor",
    "low_power_skip_costs",
    "manual_min_age_seconds",
    "event_debounce_seconds",
    "max_parallel",
    "base_seconds",
    "max_seconds",
    "auth_seconds",
    "missing_seconds",
    "cost",
    "solo",
    "ttl_active_seconds",
    "ttl_idle_seconds",
    "ttl_panel_seconds",
];
fn numeric(v: &Value) -> Option<f64> {
    v.as_f64().filter(|v| v.is_finite())
}
fn valid(key: &str, v: &Value) -> bool {
    match key {
        "enabled" | "solo" => v.is_boolean(),
        "cost" => v
            .as_str()
            .is_some_and(|s| ["cheap", "moderate", "expensive"].contains(&s)),
        "low_power_skip_costs" => v
            .as_array()
            .is_some_and(|items| items.iter().all(|i| valid("cost", i))),
        "max_parallel" => v.as_u64().is_some_and(|v| v >= 1) && !v.is_f64(),
        _ => numeric(v).is_some_and(|n| match key {
            "jitter_fraction" => (0.0..1.0).contains(&n),
            "near_exhaustion_percent" => (0.0..=100.0).contains(&n),
            "near_exhaustion_factor" => n > 0.0 && n <= 1.0,
            "battery_factor" | "low_power_factor" => n >= 1.0,
            "min_interval_seconds"
            | "max_interval_seconds"
            | "runs_out_horizon_factor"
            | "base_seconds"
            | "max_seconds"
            | "ttl_active_seconds"
            | "ttl_idle_seconds"
            | "ttl_panel_seconds" => n > 0.0,
            _ => n >= 0.0,
        }),
    }
}
impl Config {
    pub fn for_provider(provider: &str) -> Self {
        let (cost, solo, active, idle, panel) = match provider {
            "claude" => ("moderate", false, 300.0, 1800.0, 120.0),
            "codex" => ("moderate", false, 900.0, 3600.0, 300.0),
            "cursor" => ("cheap", false, 1800.0, 3600.0, 600.0),
            "opencode" => ("cheap", false, 300.0, 3600.0, 60.0),
            "agy" => ("expensive", true, 1200.0, 7200.0, 900.0),
            _ => ("moderate", false, 900.0, 3600.0, 300.0),
        };
        Self {
            enabled: true,
            min_interval_seconds: 60.0,
            max_interval_seconds: 21600.0,
            active_window_seconds: 900.0,
            jitter_fraction: 0.1,
            coalesce_seconds: 120.0,
            near_exhaustion_percent: 85.0,
            near_exhaustion_factor: 0.5,
            runs_out_horizon_factor: 2.0,
            reset_grace_seconds: 60.0,
            exhausted_recheck_seconds: 21600.0,
            battery_factor: 2.0,
            low_power_factor: 4.0,
            low_power_skip_costs: vec!["expensive".into()],
            manual_min_age_seconds: 30.0,
            event_debounce_seconds: 30.0,
            max_parallel: 3,
            base_seconds: 120.0,
            max_seconds: 3600.0,
            auth_seconds: 3600.0,
            missing_seconds: 86400.0,
            cost: cost.into(),
            solo,
            ttl_active_seconds: active,
            ttl_idle_seconds: idle,
            ttl_panel_seconds: panel,
        }
    }
}
/// Invalid layers fall through in source order and emit a note for each refusal.
pub fn resolve(
    provider: &str,
    account: &Value,
    settings: &Value,
) -> Result<(Config, Vec<String>), QuotaError> {
    let mut values =
        serde_json::to_value(Config::for_provider(provider)).map_err(QuotaError::Record)?;
    let mut notes = vec![];
    for &key in KEYS {
        let backoff = [
            "base_seconds",
            "max_seconds",
            "auth_seconds",
            "missing_seconds",
        ]
        .contains(&key);
        let group = if backoff { "backoff" } else { "schedule" };
        for (name, layer) in [
            ("account quota".to_owned(), account),
            (
                format!("quota.providers.{provider}"),
                &settings["providers"][provider],
            ),
            (format!("quota.{group}"), &settings[group]),
        ] {
            let Some(value) = layer.as_object().and_then(|o| o.get(key)) else {
                continue;
            };
            if valid(key, value) {
                values[key] = value.clone();
                break;
            }
            notes.push(format!("{name}.{key} is invalid; using the default"));
        }
    }
    let mut config: Config = serde_json::from_value(values).map_err(QuotaError::Record)?;
    if config.max_interval_seconds < config.min_interval_seconds {
        notes.push("max_interval_seconds is below min_interval_seconds; using the defaults".into());
        config.min_interval_seconds = 60.0;
        config.max_interval_seconds = 21600.0;
    }
    Ok((config, notes))
}
pub fn jitter(seed: &str, fraction: f64) -> f64 {
    let hash = Sha256::digest(seed.as_bytes());
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&hash[..8]);
    (u64::from_be_bytes(bytes) as f64 / 18446744073709551616.0 * 2.0 - 1.0) * fraction
}
pub fn backoff_seconds(
    kind: ErrorKind,
    failures: i64,
    retry_after: Option<f64>,
    config: &Config,
    seed: &str,
) -> f64 {
    let wait = match kind {
        ErrorKind::Missing => config.missing_seconds,
        ErrorKind::Auth => config.auth_seconds,
        _ => config
            .max_seconds
            .min(config.base_seconds * 2.0_f64.powi((failures.clamp(1, 32) - 1) as i32)),
    } * (1.0 + jitter(&format!("{seed}|{failures}"), config.jitter_fraction));
    if kind == ErrorKind::RateLimited {
        retry_after
            .filter(|v| v.is_finite())
            .map_or(wait, |r| wait.max(r))
    } else {
        wait
    }
}
fn sets_state(row: &Window) -> bool {
    row.advisory != Some(true)
        && !(row.limit_amount.is_some_and(|v| v != 0.0)
            && row.window_minutes.is_none_or(|v| v == 0.0))
}
fn near(rows: &[&Window], now: Timestamp, ttl: f64, config: &Config) -> bool {
    rows.iter().any(|row| {
        let used = row.used_percent;
        if used >= 100.0 {
            return false;
        }
        if used >= config.near_exhaustion_percent {
            return true;
        }
        let Some((reset, minutes)) =
            stamp(row.reset_at.as_deref()).zip(row.window_minutes.filter(|v| *v != 0.0))
        else {
            return false;
        };
        let remaining = seconds_between(reset, now);
        let elapsed = minutes * 60.0 - remaining;
        if used <= 0.0 || remaining <= 0.0 || elapsed <= 0.0 {
            return false;
        }
        let runs_out = (100.0 - used) / (used / elapsed);
        runs_out < remaining && runs_out <= config.runs_out_horizon_factor * ttl
    })
}
pub struct PlanInput<'a> {
    pub account_id: &'a str,
    pub snapshot: Option<&'a Snapshot>,
    pub now: Timestamp,
    pub config: &'a Config,
    pub trigger: &'a str,
    pub power: &'a str,
    pub activity_at: Option<f64>,
    pub ignore_backoff: bool,
    pub notes: &'a [String],
}
pub fn plan(input: PlanInput<'_>) -> Result<(Schedule, bool), QuotaError> {
    let PlanInput {
        account_id,
        snapshot,
        now,
        config,
        trigger,
        power,
        activity_at,
        ignore_backoff,
        notes,
    } = input;
    let blank = Snapshot::default();
    let snapshot = snapshot.unwrap_or(&blank);
    let state = snapshot.fetch.clone().unwrap_or_default();
    let attempted = snapshot.attempted_at.as_ref().and_then(parse_time);
    let failures = if state.failures.is_finite() {
        state.failures as i64
    } else {
        0
    };
    let kind = state
        .error_kind
        .as_deref()
        .filter(|s| ["rate_limited", "network", "auth", "missing", "other"].contains(s))
        .map(ErrorKind::from_name);
    let backoff = if failures != 0 {
        state.backoff_until.as_ref().and_then(parse_time)
    } else {
        None
    };
    let duration = state
        .last_duration_ms
        .filter(|v| v.is_finite())
        .unwrap_or(0.0)
        / 1000.0;
    let seen = activity_at
        .filter(|v| v.is_finite())
        .map(from_timestamp)
        .transpose()?;
    let own = attempted
        .zip(seen)
        .is_some_and(|(a, s)| a <= s && seconds_between(s, a) <= duration + 5.0);
    let active =
        seen.is_some_and(|s| !own && seconds_between(now, s) <= config.active_window_seconds);
    let since_fetch = seen.zip(attempted).is_some_and(|(s, a)| !own && s > a);
    let base = if active {
        config.ttl_active_seconds
    } else {
        config.ttl_idle_seconds
    };
    let mut reason = if active { "active" } else { "idle" };
    let rows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|r| r.used_percent.is_finite() && sets_state(r))
        .collect();
    let mut factor = 1.0;
    if near(&rows, now, base, config) {
        factor = config.near_exhaustion_factor;
        reason = "near_exhaustion";
    }
    if trigger != "manual" {
        factor *= match power {
            "battery" => config.battery_factor,
            "low" => config.low_power_factor,
            _ => 1.0,
        };
    }
    let mut ttl = config
        .max_interval_seconds
        .min(config.min_interval_seconds.max(base * factor));
    if power == "low" && trigger != "manual" && config.low_power_skip_costs.contains(&config.cost) {
        ttl = config.max_interval_seconds;
        reason = "low_power";
    }
    let mut next_due = if let Some(attempted) = attempted {
        let mut due = add_seconds(
            attempted,
            ttl * (1.0
                + jitter(
                    &format!("{account_id}|{}", iso(attempted)),
                    config.jitter_fraction,
                )),
        )?;
        let exhausted: Vec<_> = rows
            .iter()
            .filter(|r| r.used_percent >= 100.0)
            .map(|r| stamp(r.reset_at.as_deref()))
            .collect();
        if !exhausted.is_empty() && exhausted.iter().all(|r| r.is_some_and(|r| r > now)) {
            if let Some(last) = exhausted.into_iter().flatten().max() {
                due = add_seconds(last, config.reset_grace_seconds)?
                    .min(add_seconds(attempted, config.exhausted_recheck_seconds)?);
                reason = "exhausted_until_reset";
            }
        }
        if let Some(passed) = rows
            .iter()
            .filter_map(|r| stamp(r.reset_at.as_deref()))
            .filter(|r| attempted < *r && *r <= now)
            .min()
        {
            due = passed;
            reason = "reset_passed";
        }
        due
    } else {
        reason = "first_fetch";
        now
    };
    let mut blocked = false;
    let mut plain = next_due;
    if let Some(backoff) = backoff {
        if !(kind == Some(ErrorKind::Auth) && since_fetch) {
            next_due = backoff;
            reason = "backoff";
            blocked = backoff > now
                && !ignore_backoff
                && !(trigger == "manual" && kind != Some(ErrorKind::RateLimited));
        } else {
            next_due = now;
            plain = now;
        }
    }
    let age = attempted
        .map(|a| seconds_between(now, a))
        .unwrap_or(f64::INFINITY);
    let coalesce = config.coalesce_seconds.min(0.25 * ttl);
    let timer_due = seconds_between(next_due.min(plain), now) <= coalesce;
    let due = !blocked
        && match trigger {
            "timer" => timer_due,
            "panel" => timer_due || age >= config.ttl_panel_seconds,
            "manual" => timer_due || age >= config.manual_min_age_seconds,
            "event" => {
                timer_due
                    || age
                        >= config
                            .min_interval_seconds
                            .max(config.event_debounce_seconds)
            }
            "wake" => timer_due || age >= config.min_interval_seconds,
            _ => false,
        };
    Ok((
        Schedule {
            next_due_at: iso(next_due),
            reason: reason.into(),
            ttl_seconds: ttl.round_ties_even(),
            trigger: state.trigger,
            failures,
            backoff_until: backoff.map(iso),
            error_kind: if failures != 0 { kind } else { None },
            last_duration_ms: state.last_duration_ms,
            source_kind: state
                .source_kind
                .filter(|s| !s.is_empty())
                .or_else(|| attempted.map(|_| "fetch".into())),
            notes: notes.to_vec(),
        },
        due,
    ))
}
fn newest_files(directory: &Path, suffix: &str, limit: usize) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return vec![];
    };
    let mut folders = vec![];
    for entry in entries {
        let Ok(entry) = entry else {
            return vec![];
        };
        // DirEntry::is_dir in Python follows live links but skips dangling ones.
        if entry.path().is_dir() {
            let Ok(metadata) = fs::metadata(entry.path()) else {
                return vec![];
            };
            let Ok(modified) = metadata.modified() else {
                return vec![];
            };
            folders.push((modified, entry.path()));
        }
    }
    folders.sort_by(|a, b| b.cmp(a));
    folders
        .into_iter()
        .take(limit)
        .flat_map(|(_, folder)| {
            fs::read_dir(folder)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
                .map(|e| e.path())
        })
        .collect()
}
/// `now` and the local zone are explicit, including across midnight and DST changes.
pub fn activity_paths(
    provider: &str,
    identity_dir: &Path,
    now: Timestamp,
    home: &Path,
    timezone: &TimeZone,
) -> Vec<PathBuf> {
    let root = if identity_dir == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(rest) = identity_dir.strip_prefix("~/") {
        home.join(rest)
    } else {
        identity_dir.to_path_buf()
    };
    match provider {
        "claude" => {
            let mut paths = vec![root.join("history.jsonl")];
            paths.extend(newest_files(&root.join("projects"), ".jsonl", 4));
            paths
        }
        "codex" => {
            let today = now.to_zoned(timezone.clone()).date();
            let mut paths = vec![];
            for day in [
                Some(today),
                today.checked_sub(jiff::Span::new().days(1)).ok(),
            ]
            .into_iter()
            .flatten()
            {
                let folder = root.join("sessions").join(format!(
                    "{:04}/{:02}/{:02}",
                    day.year(),
                    day.month(),
                    day.day()
                ));
                if let Ok(entries) = fs::read_dir(folder) {
                    paths.extend(entries.filter_map(Result::ok).map(|e| e.path()));
                }
            }
            paths
        }
        "opencode" => vec![root.join("opencode.db-wal")],
        "agy" => vec![home.join(".gemini/antigravity-cli/log")],
        _ => vec![],
    }
}
pub fn activity(
    provider: &str,
    identity_dir: &Path,
    now: Timestamp,
    home: &Path,
    timezone: &TimeZone,
) -> Option<f64> {
    activity_paths(provider, identity_dir, now, home, timezone)
        .iter()
        .filter_map(|p| {
            fs::metadata(p)
                .ok()?
                .modified()
                .ok()
                .map(|m| match m.duration_since(UNIX_EPOCH) {
                    Ok(d) => d.as_secs_f64(),
                    Err(e) => -e.duration().as_secs_f64(),
                })
        })
        .filter(|v| *v != 0.0)
        .max_by(f64::total_cmp)
}
