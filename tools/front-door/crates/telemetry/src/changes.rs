//! Change comparison policy, independent of GitHub transport and ledger queries.
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub const MIN_SAMPLE: u64 = 10;
pub const DEFAULT_WINDOW: &str = "7d";
pub const BUDGET: usize = 4000;
pub const SYNC_LIMIT: usize = 200;
pub const SEARCH_LIMIT: usize = 1000;
pub const SYNC_PAGES: usize = 25;
pub const SYNC_FLOOR: &str = "2008-01-01T00:00:00Z";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    SuccessRate,
    RetriesPerLane,
    LatencyP50S,
    CacheReadShare,
    CostUsdPerDay,
    LimitEventsPerDay,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetricSpec {
    pub metric: Metric,
    pub name: &'static str,
    pub sample: &'static str,
    pub higher_is_better: bool,
    pub short: Option<&'static str>,
}
/// One table supplies metric names, sample units, direction and retro labels.
pub const METRICS: [MetricSpec; 6] = [
    MetricSpec {
        metric: Metric::SuccessRate,
        name: "success_rate",
        sample: "lanes",
        higher_is_better: true,
        short: Some("ok"),
    },
    MetricSpec {
        metric: Metric::RetriesPerLane,
        name: "retries_per_lane",
        sample: "lanes",
        higher_is_better: false,
        short: None,
    },
    MetricSpec {
        metric: Metric::LatencyP50S,
        name: "latency_p50_s",
        sample: "timed attempts",
        higher_is_better: false,
        short: Some("p50_s"),
    },
    MetricSpec {
        metric: Metric::CacheReadShare,
        name: "cache_read_share",
        sample: "complete rows",
        higher_is_better: true,
        short: None,
    },
    MetricSpec {
        metric: Metric::CostUsdPerDay,
        name: "cost_usd_per_day",
        sample: "priced rows",
        higher_is_better: false,
        short: Some("usd/d"),
    },
    MetricSpec {
        metric: Metric::LimitEventsPerDay,
        name: "limit_events_per_day",
        sample: "usage rows",
        higher_is_better: false,
        short: Some("limits/d"),
    },
];
impl Metric {
    pub const ALL: [Self; 6] = [
        METRICS[0].metric,
        METRICS[1].metric,
        METRICS[2].metric,
        METRICS[3].metric,
        METRICS[4].metric,
        METRICS[5].metric,
    ];
    pub fn spec(self) -> Option<&'static MetricSpec> {
        METRICS.iter().find(|spec| spec.metric == self)
    }
    pub fn higher(self) -> bool {
        self.spec().is_some_and(|spec| spec.higher_is_better)
    }
    pub fn sample(self) -> &'static str {
        self.spec().map_or("unknown", |spec| spec.sample)
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Improved,
    Regressed,
    WithinNoise,
    Inconclusive,
}
const VERDICT_SPECS: [(Verdict, &str, char); 4] = [
    (Verdict::Improved, "improved", '+'),
    (Verdict::Regressed, "regressed", '-'),
    (Verdict::WithinNoise, "within_noise", '='),
    (Verdict::Inconclusive, "inconclusive", '?'),
];

/// A small insertion-ordered map for Python dictionaries whose order reaches
/// JSON or text output. It avoids adding an order-map dependency.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderedMap<K, V>(Vec<(K, V)>);
impl<K, V> Default for OrderedMap<K, V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<K: PartialEq, V> OrderedMap<K, V> {
    pub fn get(&self, key: &K) -> Option<&V> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.0.iter().map(|(key, value)| (key, value))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl<K: PartialEq, V> std::iter::FromIterator<(K, V)> for OrderedMap<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(values: T) -> Self {
        let mut map = Self::default();
        for (key, value) in values {
            if let Some((_, old)) = map.0.iter_mut().find(|(candidate, _)| candidate == &key) {
                *old = value;
            } else {
                map.0.push((key, value));
            }
        }
        map
    }
}
impl<K: Serialize, V: Serialize> Serialize for OrderedMap<K, V> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de, K, V> Deserialize<'de> for OrderedMap<K, V>
where
    K: Deserialize<'de> + PartialEq,
    V: Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct OrderedVisitor<K, V>(std::marker::PhantomData<(K, V)>);
        impl<'de, K, V> serde::de::Visitor<'de> for OrderedVisitor<K, V>
        where
            K: Deserialize<'de> + PartialEq,
            V: Deserialize<'de>,
        {
            type Value = OrderedMap<K, V>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an insertion-ordered map")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values: Vec<(K, V)> = Vec::new();
                while let Some((key, value)) = access.next_entry::<K, V>()? {
                    if let Some((_, old)) =
                        values.iter_mut().find(|(candidate, _)| candidate == &key)
                    {
                        *old = value;
                    } else {
                        values.push((key, value));
                    }
                }
                Ok(OrderedMap(values))
            }
        }
        deserializer.deserialize_map(OrderedVisitor(std::marker::PhantomData))
    }
}

pub type MetricSamples = OrderedMap<Metric, u64>;
pub type OrderedCounts = OrderedMap<String, u64>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Side {
    pub start: String,
    pub end: String,
    pub lanes: u64,
    pub usage_rows: u64,
    pub samples: MetricSamples,
    pub inconclusive: bool,
    pub success_rate: Option<f64>,
    pub retries_per_lane: Option<f64>,
    pub latency_p50_s: Option<f64>,
    pub cache_read_share: Option<f64>,
    pub cost_usd: Option<f64>,
    pub cost_usd_per_day: Option<f64>,
    pub missing_cost_rows: u64,
    pub cost_coverage: Option<f64>,
    pub limit_events: u64,
    pub limit_events_per_day: f64,
    pub error_classes: OrderedCounts,
}
impl Side {
    pub fn value(&self, metric: Metric) -> Option<f64> {
        match metric {
            Metric::SuccessRate => self.success_rate,
            Metric::RetriesPerLane => self.retries_per_lane,
            Metric::LatencyP50S => self.latency_p50_s,
            Metric::CacheReadShare => self.cache_read_share,
            Metric::CostUsdPerDay => self.cost_usd_per_day,
            Metric::LimitEventsPerDay => Some(self.limit_events_per_day),
        }
    }
}

pub fn verdict(metric: Metric, before: &Side, after: &Side) -> Verdict {
    let nb = before.samples.get(&metric).copied().unwrap_or(0);
    let na = after.samples.get(&metric).copied().unwrap_or(0);
    let (Some(b), Some(a)) = (before.value(metric), after.value(metric)) else {
        return Verdict::Inconclusive;
    };
    if nb < MIN_SAMPLE
        || na < MIN_SAMPLE
        || metric == Metric::CostUsdPerDay
            && (before.missing_cost_rows > 0 || after.missing_cost_rows > 0)
    {
        return Verdict::Inconclusive;
    }
    let noise = if metric == Metric::SuccessRate {
        let (nb, na) = (nb as f64, na as f64);
        let pooled = (b * nb + a * na) / (nb + na);
        (a - b).abs() <= 2.0 * (pooled * (1.0 - pooled) * (1.0 / nb + 1.0 / na)).sqrt()
    } else {
        (a - b).abs() <= 0.10 * a.abs().max(b.abs())
    };
    if noise {
        Verdict::WithinNoise
    } else if (a > b) == metric.higher() {
        Verdict::Improved
    } else {
        Verdict::Regressed
    }
}

/// Hours in a comparison window. Overflow is a typed refusal, never a wrap.
pub fn window(value: &str) -> Result<u64> {
    let invalid = || {
        Error::Invalid(format!(
            "window must look like 24h, 7d or 2w, got {}",
            crate::pending::python_repr(value)
        ))
    };
    let Some((suffix, digits)) = value.as_bytes().split_last() else {
        return Err(invalid());
    };
    if digits.is_empty() || !matches!(suffix, b'h' | b'd' | b'w') {
        return Err(invalid());
    }
    let count = value[..value.len() - 1]
        .chars()
        .try_fold(0u64, |count, c| {
            count
                .checked_mul(10)?
                .checked_add(crate::pending::decimal_digit(c)?.into())
        })
        .ok_or_else(invalid)?;
    let factor = match suffix {
        b'h' => 1,
        b'd' => 24,
        b'w' => 168,
        _ => return Err(invalid()),
    };
    count
        .checked_mul(factor)
        .filter(|n| *n > 0)
        .ok_or_else(invalid)
}

/// Keep whole lines, including the omission notice, inside the UTF-8 budget.
pub fn fit(lines: &[String], budget: usize) -> String {
    if lines.iter().map(|s| s.len() + 1).sum::<usize>() <= budget {
        return lines.join("\n");
    }
    let reserve = format!("{} lines omitted to fit 4 KB; use --json", lines.len()).len();
    let mut kept = Vec::new();
    let mut used = 0;
    for line in lines {
        let size = line.len() + 1;
        if used + size + reserve <= budget {
            kept.push(line.as_str());
            used += size;
        }
    }
    let notice = format!(
        "{} lines omitted to fit 4 KB; use --json",
        lines.len() - kept.len()
    );
    kept.push(&notice);
    kept.join("\n")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Attention {
    pub sessions_started: u64,
    pub user_prompts: Option<u64>,
    pub tool_decisions: Option<u64>,
    pub active_time_s: Option<BTreeMap<String, crate::Numeric>>,
}
/// Attention proxies only use metadata already retained by the ledger.
pub fn attention(events: &[crate::Event], since: &str) -> Result<Attention> {
    let retained = || {
        events
            .iter()
            .filter(|event| event.ts.as_deref().is_some_and(|ts| ts >= since))
    };
    let mut counts = BTreeMap::<String, u64>::new();
    for event in retained().filter(|event| {
        matches!(
            event.kind.as_str(),
            "session.start"
                | "otel.user_prompt"
                | "otel.tool_decision"
                | "otel.codex.user_prompt"
                | "otel.codex.tool_decision"
        )
    }) {
        let count = counts
            .entry(event.kind.replace("otel.codex.", "otel."))
            .or_default();
        *count = count.checked_add(1).ok_or(Error::NumberOutOfRange)?;
    }
    let mut active = Vec::<((Option<String>, String), crate::Numeric)>::new();
    for event in retained().filter(|event| event.kind == "otel.metric") {
        let session = event.session.clone();
        let attrs = &event.attrs;
        if attrs["name"] != "claude_code.active_time.total" {
            continue;
        }
        let value = match &attrs["value"] {
            Value::Null => continue,
            Value::Bool(value) => crate::Numeric::from(if *value { 1i64 } else { 0i64 }),
            Value::Number(value) => crate::Numeric::from(value.clone()),
            _ => return Err(Error::Invalid("active-time value must be numeric".into())),
        };
        let raw_type = &attrs["type"];
        let kind = if !crate::pending::python_truthy(raw_type) {
            "total".to_owned()
        } else {
            raw_type
                .as_str()
                .ok_or_else(|| Error::Invalid("active-time type must be a string".into()))?
                .to_owned()
        };
        let key = (session, kind);
        let position = active.iter().position(|(candidate, _)| candidate == &key);
        let entry = if let Some(position) = position {
            &mut active[position].1
        } else {
            active.push((key, crate::Numeric::ZERO));
            &mut active.last_mut().ok_or(Error::NumberOutOfRange)?.1
        };
        if crate::pending::python_equal(&attrs["temporality"], &serde_json::json!(2)) {
            if value > *entry {
                *entry = value;
            }
        } else {
            *entry = entry.checked_add(value)?;
        }
    }
    let mut totals = Vec::<(String, crate::Numeric)>::new();
    for ((_, kind), seconds) in active {
        if let Some((_, total)) = totals.iter_mut().find(|(candidate, _)| candidate == &kind) {
            *total = total.checked_add(seconds)?;
        } else {
            totals.push((kind, seconds));
        }
    }
    let totals = totals
        .into_iter()
        .map(|(kind, seconds)| {
            let crate::Numeric::Float(seconds) = seconds else {
                return Ok((kind, seconds));
            };
            let value = seconds.round_ties_even();
            if !value.is_finite()
                || !(-9_223_372_036_854_775_808.0..18_446_744_073_709_551_616.0).contains(&value)
            {
                return Err(Error::NumberOutOfRange);
            }
            Ok((kind, crate::Numeric::Integer(value as i128)))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut totals = totals;
    totals.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(Attention {
        sessions_started: counts.get("session.start").copied().unwrap_or(0),
        user_prompts: counts.get("otel.user_prompt").copied(),
        tool_decisions: counts.get("otel.tool_decision").copied(),
        active_time_s: if totals.is_empty() {
            None
        } else {
            Some(totals.into_iter().collect())
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn side(n: u64) -> Side {
        Side {
            start: "start".into(),
            end: "end".into(),
            lanes: n,
            usage_rows: n,
            samples: Metric::ALL.into_iter().map(|k| (k, n)).collect(),
            inconclusive: n < 10,
            success_rate: Some(0.5),
            retries_per_lane: Some(1.0),
            latency_p50_s: Some(10.0),
            cache_read_share: Some(0.5),
            cost_usd: Some(10.0),
            cost_usd_per_day: Some(10.0),
            missing_cost_rows: 0,
            cost_coverage: Some(1.0),
            limit_events: 0,
            limit_events_per_day: 0.0,
            error_classes: OrderedCounts::default(),
        }
    }
    #[test]
    fn small_samples_and_missing_prices_never_win() {
        let b = side(100);
        let mut a = side(9);
        a.success_rate = Some(1.0);
        assert_eq!(verdict(Metric::SuccessRate, &b, &a), Verdict::Inconclusive);
        a = side(100);
        a.cost_usd_per_day = Some(0.0);
        a.missing_cost_rows = 1;
        assert_eq!(
            verdict(Metric::CostUsdPerDay, &b, &a),
            Verdict::Inconclusive
        );
    }
    #[test]
    fn noise_and_direction() {
        let b = side(1000);
        let mut a = side(1000);
        a.latency_p50_s = Some(9.5);
        assert_eq!(verdict(Metric::LatencyP50S, &b, &a), Verdict::WithinNoise);
        a.latency_p50_s = Some(5.0);
        assert_eq!(verdict(Metric::LatencyP50S, &b, &a), Verdict::Improved);
        a.success_rate = Some(0.9);
        assert_eq!(verdict(Metric::SuccessRate, &b, &a), Verdict::Improved);
    }
    #[test]
    fn utf8_budget_keeps_whole_lines() {
        let lines = vec!["é".repeat(2500), "short".into(), "long".repeat(500)];
        let rendered = fit(&lines, BUDGET);
        assert!(rendered.len() <= BUDGET);
        assert!(rendered.starts_with("short\n"));
        assert!(rendered.contains("1 lines omitted"));
    }
    #[test]
    fn valid_windows_and_refusals() {
        assert_eq!(window("2w").unwrap(), 336);
        for bad in ["0h", "-1d", "7D", "7d\n", ""] {
            assert!(window(bad).is_err());
        }
    }
    #[test]
    fn general_format_matches_python_at_rounded_exponent_boundaries() {
        let values = [999.9, 999.4, 0.00009999, 0.00009994, 0.0001, 1e-5, 1000.0];
        let expected = [
            "1e+03", "999", "0.0001", "9.99e-05", "0.0001", "1e-05", "1e+03",
        ];
        for (value, expected) in values.into_iter().zip(expected) {
            assert_eq!(general(value, 3), expected);
        }
        let oracle = crate::differential::python(
            "import json,sys\nprint(json.dumps([format(value, '.3g') for value in json.load(sys.stdin)]))",
            &json!(values),
        );
        if let Some(oracle) = oracle {
            assert_eq!(
                serde_json::to_value(values.map(|value| general(value, 3))).unwrap(),
                oracle
            );
        }
    }
    #[test]
    fn review_regression_active_time_is_an_integer() {
        let events: Vec<_> = [("a", 6.25), ("b", 6.25)]
            .into_iter()
            .map(|(session, seconds)| crate::Event {
                kind: "otel.metric".into(),
                ts: Some("2026-01-01".into()),
                session: Some(session.into()),
                attrs: serde_json::json!({"name":"claude_code.active_time.total","value":seconds}),
                ..crate::Event::default()
            })
            .collect();
        let report = attention(&events, "2026-01-01").unwrap();
        assert_eq!(
            serde_json::to_value(report).unwrap()["active_time_s"]["total"].to_string(),
            "12"
        );
    }
    #[test]
    fn active_time_preserves_large_integer_values() {
        let event = crate::Event {
            kind: "otel.metric".into(),
            ts: Some("2026-01-01".into()),
            attrs: serde_json::json!({"name":"claude_code.active_time.total","value":9007199254740993u64}),
            ..crate::Event::default()
        };
        let result = attention(&[event], "2026-01-01").unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["active_time_s"]["total"].to_string(),
            "9007199254740993"
        );
    }
    #[test]
    fn active_time_treats_booleans_as_numbers_and_matches_python() {
        let event = |session: &str, value: Value, temporality: i64| crate::Event {
            kind: "otel.metric".into(),
            ts: Some("2026-01-01T00:00:00Z".into()),
            session: Some(session.into()),
            attrs: json!({
                "name":"claude_code.active_time.total",
                "value":value,
                "temporality":temporality
            }),
            ..crate::Event::default()
        };
        let events = vec![
            event("delta", json!(true), 1),
            event("delta", json!(false), 1),
            event("cumulative", json!(true), 2),
            event("cumulative", json!(false), 2),
        ];
        let report = attention(&events, "2026-01-01").unwrap();
        let rust = serde_json::to_value(&report).unwrap()["active_time_s"].clone();
        assert_eq!(rust["total"], json!(2));

        let fixture = json!(events
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap());
        let oracle = crate::differential::python(
            "import json,sys\nevents=json.load(sys.stdin)\nactive={}\nfor event in events:\n attrs=event['attrs']\n if attrs.get('name') != 'claude_code.active_time.total' or attrs.get('value') is None: continue\n key=(event.get('session'),attrs.get('type') or 'total')\n active[key]=max(active.get(key,0),attrs['value']) if attrs.get('temporality') == 2 else active.get(key,0)+attrs['value']\ntotals={}\nfor (_,kind),seconds in active.items(): totals[kind]=totals.get(kind,0)+seconds\nprint(json.dumps({'active_time_s':dict(sorted((kind,round(seconds)) for kind,seconds in totals.items()))}))",
            &fixture,
        );
        if let Some(oracle) = oracle {
            assert_eq!(rust, oracle["active_time_s"]);
        }
        let malformed = event("bad", json!("many"), 1);
        assert!(attention(&[malformed], "2026-01-01").is_err());
    }
    #[test]
    fn active_time_sums_sessions_in_source_insertion_order() {
        let events: Vec<_> = [("z", 0.6), ("x", 0.6), ("y", 10_000_000_000_000_000.0)]
            .into_iter()
            .map(|(session, value)| crate::Event {
                kind: "otel.metric".into(),
                ts: Some("2026-01-01T00:00:00Z".into()),
                session: Some(session.into()),
                attrs: json!({"name":"claude_code.active_time.total","value":value}),
                ..crate::Event::default()
            })
            .collect();
        let report = attention(&events, "2026-01-01").unwrap();
        let rust = serde_json::to_value(&report).unwrap()["active_time_s"].clone();
        assert_eq!(rust["total"], json!(10_000_000_000_000_002u64));

        let fixture = json!(events
            .iter()
            .map(|event| serde_json::to_value(event).unwrap())
            .collect::<Vec<_>>());
        let oracle = crate::differential::python(
            "import json,sys\nevents=json.load(sys.stdin)\nactive={}\nfor event in events:\n attrs=event['attrs']\n if attrs.get('name') != 'claude_code.active_time.total' or attrs.get('value') is None: continue\n key=(event.get('session'),attrs.get('type') or 'total')\n active[key]=max(active.get(key,0),attrs['value']) if attrs.get('temporality') == 2 else active.get(key,0)+attrs['value']\ntotals={}\nfor (_,kind),seconds in active.items(): totals[kind]=totals.get(kind,0)+seconds\nprint(json.dumps({'active_time_s':dict(sorted((kind,round(seconds)) for kind,seconds in totals.items()))}))",
            &fixture,
        );
        if let Some(oracle) = oracle {
            assert_eq!(rust, oracle["active_time_s"]);
        }
    }
}

/// A GitHub or Git utility failure, rendered by the compatibility edge.
///
/// The transport is injected because telemetry owns query policy while the
/// front door owns bounded utility execution. A caller should implement this
/// with the supervisor's safe bounded-command API.
#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    #[error("{0}")]
    Invalid(String),
    #[error("gh pr list could not run ({kind}); install and authenticate gh")]
    GhCouldNotRun { kind: String },
    #[error("gh pr list failed with exit {code}; run `gh auth status`")]
    GhFailed { code: i32 },
    #[error("gh pr list returned malformed JSON")]
    GhMalformedJson,
    #[error("gh pr list returned non-UTF-8 output")]
    GhInvalidUtf8,
    #[error("change command timed out")]
    CommandTimeout,
    #[error("change command failed: {0}")]
    Command(String),
    #[error("change ledger returned malformed state")]
    MalformedState,
    #[error("change report returned malformed data: {0}")]
    MalformedReport(String),
    #[error("telemetry value is outside the supported range")]
    NumberOutOfRange,
    #[error(transparent)]
    Telemetry(#[from] Error),
}
impl From<rusqlite::Error> for ChangeError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Telemetry(Error::Sqlite(error))
    }
}
impl From<serde_json::Error> for ChangeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Telemetry(Error::Json(error))
    }
}

pub type ChangeResult<T> = std::result::Result<T, ChangeError>;

/// Explicit time and identity values keep comparisons deterministic and use
/// the shared Python-compatible timestamp and identity owners at the edge.
pub trait ChangeClock {
    fn now(&self) -> jiff::Timestamp;
    fn parse_time(&self, value: &str) -> ChangeResult<jiff::Timestamp>;
    fn iso(&self, value: jiff::Timestamp) -> String;
    fn identity(&self, values: &[&str]) -> ChangeResult<String>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangeRecord {
    pub change_id: String,
    pub kind: String,
    pub pr: Option<i64>,
    pub merge_commit: Option<String>,
    pub merged_at: String,
    pub title: Option<String>,
    pub labels: Vec<Value>,
    pub added_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LaneTotal {
    pub lanes: u64,
    pub retries: u64,
    pub latency_samples: u64,
    pub success_rate: Option<f64>,
    pub latency_p50_s: Option<f64>,
    pub error_classes: OrderedCounts,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UsageTotal {
    pub rows: u64,
    pub known_cost_usd: Option<f64>,
    pub missing_cost_rows: u64,
    pub cache_share_rows: u64,
    pub cache_share_input: u64,
    pub cache_share_read: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimitTotal {
    pub events: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttemptTotal {
    pub error_class: Option<String>,
    pub attempts: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct LaneStatusTotal {
    pub status: String,
    pub lanes: u64,
}

/// The subset of `telemetry_report.stats` consumed by compare and retro.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StatsReport {
    pub lane_totals: Vec<LaneTotal>,
    pub usage_totals: Vec<UsageTotal>,
    pub limits_per_day: Vec<LimitTotal>,
    pub attempts: Vec<AttemptTotal>,
    pub lanes: Vec<LaneStatusTotal>,
}

/// Statistics and outcomes from `telemetry_report.stats`.
pub trait ReportOwner {
    fn stats(&mut self, since: &str, until: Option<&str>) -> ChangeResult<StatsReport>;
}

/// Ledger queries and mutations used by the change marker owner.
///
/// `save_sync_state` must commit the state and all preceding upserts as one
/// durable checkpoint, matching the Python ledger's save boundary.
pub trait ChangeStore: ReportOwner {
    fn sync_state(&mut self) -> ChangeResult<Option<Value>>;
    /// Start a durable checkpoint before page upserts. Memory and remote
    /// stores may keep the default no-op implementation.
    fn begin_sync_checkpoint(&mut self) -> ChangeResult<()> {
        Ok(())
    }
    /// Roll back a checkpoint when a page fails before its state is saved.
    fn abort_sync_checkpoint(&mut self) {}
    fn save_sync_state(&mut self, state: &Value) -> ChangeResult<()>;
    /// Upsert by `change_id`, returning whether a row was inserted.
    fn upsert_change(&mut self, change: &ChangeRecord) -> ChangeResult<bool>;
    /// Insert only if absent and commit, as `changes --add` does.
    fn insert_manual_change(&mut self, change: &ChangeRecord) -> ChangeResult<()>;
    fn changes(&mut self, since: Option<&str>) -> ChangeResult<Vec<ChangeRecord>>;
    fn change_by_pr(&mut self, pr: i64) -> ChangeResult<Option<ChangeRecord>>;
    fn change_by_commit_prefix(&mut self, prefix: &str) -> ChangeResult<Option<ChangeRecord>>;
    fn events_since(&mut self, since: &str) -> ChangeResult<Vec<crate::Event>>;
}

/// SQLite implementation for the change marker tables in the telemetry
/// ledger. Stats are supplied by the report owner, matching the source
/// module's `telemetry.report_owner().stats(...)` seam.
pub struct SqliteChangeStore<'a, R> {
    connection: &'a Connection,
    reports: R,
    checkpoint: bool,
}

impl<'a, R> SqliteChangeStore<'a, R> {
    pub fn new(connection: &'a Connection, reports: R) -> Self {
        Self {
            connection,
            reports,
            checkpoint: false,
        }
    }

    pub fn reports(&self) -> &R {
        &self.reports
    }

    fn rollback_checkpoint(&mut self) {
        if self.checkpoint {
            let _ = self.connection.execute_batch("ROLLBACK");
            self.checkpoint = false;
        }
    }
}

impl<R: ReportOwner> ReportOwner for SqliteChangeStore<'_, R> {
    fn stats(&mut self, since: &str, until: Option<&str>) -> ChangeResult<StatsReport> {
        self.reports.stats(since, until)
    }
}

impl<R: ReportOwner> ChangeStore for SqliteChangeStore<'_, R> {
    fn sync_state(&mut self) -> ChangeResult<Option<Value>> {
        let value = self
            .connection
            .query_row(
                "SELECT value FROM meta WHERE key='changes_sync'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        value
            .map(|value| crate::decode_json(value.as_bytes()).map_err(ChangeError::from))
            .transpose()
    }

    fn begin_sync_checkpoint(&mut self) -> ChangeResult<()> {
        if self.checkpoint {
            return Err(ChangeError::Command(
                "change sync checkpoint is already open".into(),
            ));
        }
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        self.checkpoint = true;
        Ok(())
    }

    fn abort_sync_checkpoint(&mut self) {
        self.rollback_checkpoint();
    }

    fn save_sync_state(&mut self, state: &Value) -> ChangeResult<()> {
        let serialized = serde_json::to_string(state)?;
        let write = self.connection.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES ('changes_sync',?1)",
            [serialized],
        );
        if let Err(error) = write {
            self.rollback_checkpoint();
            return Err(ChangeError::from(error));
        }
        if self.checkpoint {
            if let Err(error) = self.connection.execute_batch("COMMIT") {
                self.rollback_checkpoint();
                return Err(ChangeError::from(error));
            }
            self.checkpoint = false;
        }
        Ok(())
    }

    fn upsert_change(&mut self, change: &ChangeRecord) -> ChangeResult<bool> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM changes WHERE change_id=?1",
                [&change.change_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let labels = serde_json::to_string(&change.labels)?;
        self.connection.execute(
            "INSERT INTO changes(change_id,kind,pr,merge_commit,merged_at,title,labels,added_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
             ON CONFLICT(change_id) DO UPDATE SET merge_commit=excluded.merge_commit, \
             merged_at=excluded.merged_at, title=excluded.title, labels=excluded.labels",
            params![
                change.change_id,
                change.kind,
                change.pr,
                change.merge_commit,
                change.merged_at,
                change.title,
                labels,
                change.added_at
            ],
        )?;
        Ok(!exists)
    }

    fn insert_manual_change(&mut self, change: &ChangeRecord) -> ChangeResult<()> {
        let labels = serde_json::to_string(&change.labels)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO changes(change_id,kind,pr,merge_commit,merged_at,title,labels,added_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                change.change_id,
                change.kind,
                change.pr,
                change.merge_commit,
                change.merged_at,
                change.title,
                labels,
                change.added_at
            ],
        )?;
        Ok(())
    }

    fn changes(&mut self, since: Option<&str>) -> ChangeResult<Vec<ChangeRecord>> {
        let (sql, parameter) = if since.is_some() {
            (
                "SELECT change_id,kind,pr,merge_commit,merged_at,title,labels,added_at \
                 FROM changes WHERE merged_at >= ?1 ORDER BY merged_at DESC, change_id",
                since,
            )
        } else {
            (
                "SELECT change_id,kind,pr,merge_commit,merged_at,title,labels,added_at \
                 FROM changes ORDER BY merged_at DESC, change_id",
                None,
            )
        };
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = if let Some(since) = parameter {
            statement.query([since])?
        } else {
            statement.query([])?
        };
        let mut changes = Vec::new();
        while let Some(row) = rows.next()? {
            changes.push(change_from_row(row)?);
        }
        Ok(changes)
    }

    fn change_by_pr(&mut self, pr: i64) -> ChangeResult<Option<ChangeRecord>> {
        self.read_one(
            "SELECT change_id,kind,pr,merge_commit,merged_at,title,labels,added_at \
             FROM changes WHERE pr=?1",
            [pr],
        )
    }

    fn change_by_commit_prefix(&mut self, prefix: &str) -> ChangeResult<Option<ChangeRecord>> {
        self.read_one(
            "SELECT change_id,kind,pr,merge_commit,merged_at,title,labels,added_at \
             FROM changes WHERE merge_commit LIKE ?1 ORDER BY merged_at LIMIT 1",
            [format!("{prefix}%")],
        )
    }

    fn events_since(&mut self, since: &str) -> ChangeResult<Vec<crate::Event>> {
        let mut statement = self
            .connection
            .prepare("SELECT kind,ts,session,attrs FROM events WHERE ts >= ?1")?;
        let mut rows = statement.query([since])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let attrs: String = row.get(3)?;
            events.push(crate::Event {
                kind: row.get(0)?,
                ts: row.get(1)?,
                session: row.get(2)?,
                attrs: crate::decode_json(attrs.as_bytes())?,
                ..crate::Event::default()
            });
        }
        Ok(events)
    }
}

impl<R> Drop for SqliteChangeStore<'_, R> {
    fn drop(&mut self) {
        self.rollback_checkpoint();
    }
}

impl<R: ReportOwner> SqliteChangeStore<'_, R> {
    fn read_one<P: rusqlite::Params>(
        &self,
        sql: &str,
        parameters: P,
    ) -> ChangeResult<Option<ChangeRecord>> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(parameters)?;
        rows.next()?.map(change_from_row).transpose()
    }
}

fn change_from_row(row: &rusqlite::Row<'_>) -> ChangeResult<ChangeRecord> {
    let labels: String = row.get(6)?;
    let labels: Value = crate::decode_json(labels.as_bytes())?;
    let labels = labels
        .as_array()
        .cloned()
        .ok_or(ChangeError::MalformedState)?;
    Ok(ChangeRecord {
        change_id: row.get(0)?,
        kind: row.get(1)?,
        pr: row.get(2)?,
        merge_commit: row.get(3)?,
        merged_at: row.get(4)?,
        title: row.get(5)?,
        labels,
        added_at: row.get(7)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentPolicy {
    /// Inherit the process environment after excluding every `GIT_*` name.
    InheritWithoutGitVariables,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtilityRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub environment: EnvironmentPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UtilityOutput {
    /// Python subprocess return codes use negative signal numbers.
    pub return_code: i32,
    pub stdout: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UtilityFailure {
    Spawn { python_exception: String },
    Timeout,
    Other(String),
}

/// Bounded command execution seam. Its implementation must not expose stderr
/// or environment values and must finish or stop the child before returning.
pub trait UtilityRunner {
    fn run(
        &mut self,
        request: &UtilityRequest,
    ) -> std::result::Result<UtilityOutput, UtilityFailure>;
}

pub trait PullRequestSource {
    fn merged_between(&mut self, low: &str, high: &str, limit: usize) -> ChangeResult<Vec<Value>>;
}

pub trait RevisionSource {
    fn commit_time(&mut self, revision: &str) -> ChangeResult<Option<String>>;
}

/// The gh command shape is kept here; only bounded process execution is
/// supplied by the caller.
pub struct GhPullRequestSource<R> {
    runner: R,
    product_root: PathBuf,
}
impl<R> GhPullRequestSource<R> {
    pub fn new(runner: R, product_root: PathBuf) -> Self {
        Self {
            runner,
            product_root,
        }
    }
    pub fn into_runner(self) -> R {
        self.runner
    }
}
impl<R: UtilityRunner> PullRequestSource for GhPullRequestSource<R> {
    fn merged_between(&mut self, low: &str, high: &str, limit: usize) -> ChangeResult<Vec<Value>> {
        let query = format!(
            "merged:>={} merged:<={}",
            low.replace('Z', "+00:00"),
            high.replace('Z', "+00:00")
        );
        let request = UtilityRequest {
            program: "gh".into(),
            args: vec![
                "pr".into(),
                "list".into(),
                "--state".into(),
                "merged".into(),
                "--limit".into(),
                limit.to_string(),
                "--json".into(),
                "number,mergeCommit,mergedAt,title,labels".into(),
                "--search".into(),
                query,
            ],
            cwd: Some(self.product_root.clone()),
            timeout: Duration::from_secs(60),
            environment: EnvironmentPolicy::InheritWithoutGitVariables,
        };
        let output = self.runner.run(&request).map_err(|failure| match failure {
            UtilityFailure::Spawn { python_exception } => ChangeError::GhCouldNotRun {
                kind: python_exception,
            },
            UtilityFailure::Timeout => ChangeError::GhCouldNotRun {
                kind: "TimeoutExpired".into(),
            },
            UtilityFailure::Other(_) => ChangeError::GhCouldNotRun {
                kind: "SubprocessError".into(),
            },
        })?;
        if output.return_code != 0 {
            return Err(ChangeError::GhFailed {
                code: output.return_code,
            });
        }
        let stdout = std::str::from_utf8(&output.stdout).map_err(|_| ChangeError::GhInvalidUtf8)?;
        let parsed =
            crate::decode_json::<Value>(stdout.as_bytes()).map_err(|error| match error {
                Error::NumberOutOfRange => ChangeError::NumberOutOfRange,
                _ => ChangeError::GhMalformedJson,
            })?;
        Ok(parsed.as_array().cloned().unwrap_or_default())
    }
}

/// Git revision resolution mirrors `git log -1 --format=%cI REV --`; utility
/// failures are intentionally treated as an unresolved revision.
pub struct GitRevisionSource<R> {
    runner: R,
    product_root: PathBuf,
}
impl<R> GitRevisionSource<R> {
    pub fn new(runner: R, product_root: PathBuf) -> Self {
        Self {
            runner,
            product_root,
        }
    }
    pub fn into_runner(self) -> R {
        self.runner
    }
}
impl<R: UtilityRunner> RevisionSource for GitRevisionSource<R> {
    fn commit_time(&mut self, revision: &str) -> ChangeResult<Option<String>> {
        let request = UtilityRequest {
            program: "git".into(),
            args: vec![
                "-C".into(),
                self.product_root.to_string_lossy().into_owned(),
                "log".into(),
                "-1".into(),
                "--format=%cI".into(),
                revision.into(),
                "--".into(),
            ],
            cwd: None,
            timeout: Duration::from_secs(10),
            environment: EnvironmentPolicy::InheritWithoutGitVariables,
        };
        let Ok(output) = self.runner.run(&request) else {
            return Ok(None);
        };
        if output.return_code != 0 {
            return Ok(None);
        }
        let text = String::from_utf8(output.stdout)
            .map_err(|_| ChangeError::Command("invalid git output".into()))?;
        let text = crate::pending::python_strip(&text);
        Ok((!text.is_empty()).then(|| text.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncLimits {
    pub page_limit: usize,
    pub search_limit: usize,
    pub pages_per_run: usize,
}
impl Default for SyncLimits {
    fn default() -> Self {
        Self {
            page_limit: SYNC_LIMIT,
            search_limit: SEARCH_LIMIT,
            pages_per_run: SYNC_PAGES,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MetricResult {
    pub metric: Metric,
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub n_before: u64,
    pub n_after: u64,
    pub sample: String,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CompareReport {
    pub schema: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub change: Option<ChangeRecord>,
    pub pivot: String,
    pub window: String,
    pub min_sample: u64,
    pub before: Side,
    pub after: Side,
    pub metrics: Vec<MetricResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetroChange {
    #[serde(flatten)]
    pub change: ChangeRecord,
    pub compare: CompareReport,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RetroReport {
    pub schema: String,
    pub since: String,
    pub until: String,
    pub window: String,
    pub overall: Side,
    pub lanes_input_required: u64,
    pub changes: Vec<RetroChange>,
    pub top_error_classes: OrderedCounts,
    pub attention: Attention,
    pub events_by_kind: OrderedCounts,
}

fn timestamp_from_micros(value: i64) -> ChangeResult<jiff::Timestamp> {
    jiff::Timestamp::from_microsecond(value).map_err(|_| ChangeError::NumberOutOfRange)
}
fn floor_second(value: jiff::Timestamp) -> ChangeResult<jiff::Timestamp> {
    let micros = value.as_microsecond();
    timestamp_from_micros(micros.div_euclid(1_000_000) * 1_000_000)
}
fn plus_micros(value: jiff::Timestamp, micros: i64) -> ChangeResult<jiff::Timestamp> {
    timestamp_from_micros(
        value
            .as_microsecond()
            .checked_add(micros)
            .ok_or(ChangeError::NumberOutOfRange)?,
    )
}
fn delta_micros(start: jiff::Timestamp, end: jiff::Timestamp) -> ChangeResult<i64> {
    end.as_microsecond()
        .checked_sub(start.as_microsecond())
        .ok_or(ChangeError::NumberOutOfRange)
}
fn value_string(value: &Value) -> Option<&str> {
    value.as_str()
}
fn state_object_mut(value: &mut Value) -> ChangeResult<&mut serde_json::Map<String, Value>> {
    value.as_object_mut().ok_or(ChangeError::MalformedState)
}
fn state_array_mut<'a>(value: &'a mut Value, key: &str) -> ChangeResult<&'a mut Vec<Value>> {
    state_object_mut(value)?
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .ok_or(ChangeError::MalformedState)
}
fn state_array<'a>(value: &'a Value, key: &str) -> ChangeResult<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or(ChangeError::MalformedState)
}

fn date_error(value: &str, what: &str) -> ChangeError {
    ChangeError::Invalid(format!(
        "{what}, got {}",
        crate::pending::python_repr(value)
    ))
}

fn python_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn python_less(left: &Value, right: &Value) -> ChangeResult<bool> {
    let incompatible = || {
        ChangeError::Invalid(format!(
            "'<' not supported between instances of '{}' and '{}'",
            python_type(left),
            python_type(right)
        ))
    };
    match (left, right) {
        (Value::String(left), Value::String(right)) => Ok(left < right),
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            let number = |value: &Value| -> Option<crate::Numeric> {
                match value {
                    Value::Bool(value) => Some(crate::Numeric::from(i32::from(*value))),
                    Value::Number(value) => Some(crate::Numeric::from(value.clone())),
                    _ => None,
                }
            };
            number(left)
                .zip(number(right))
                .and_then(|(left, right)| left.partial_cmp(&right))
                .map(|order| order.is_lt())
                .ok_or_else(incompatible)
        }
        (Value::Array(left), Value::Array(right)) => {
            for (left, right) in left.iter().zip(right) {
                if crate::pending::python_equal(left, right) {
                    continue;
                }
                return python_less(left, right);
            }
            Ok(left.len() < right.len())
        }
        _ => Err(incompatible()),
    }
}

fn python_sort_labels(labels: &mut [Value]) -> ChangeResult<()> {
    // Insertion sort returns Python's comparison failure instead of using a
    // Rust total order for mixed JSON types.
    for index in 1..labels.len() {
        let mut current = index;
        while current > 0 && python_less(&labels[current], &labels[current - 1])? {
            labels.swap(current, current - 1);
            current -= 1;
        }
    }
    Ok(())
}

/// Bind a scalar into a SQLite TEXT-affinity column and retain the text SQLite
/// stores. Python sqlite3 rejects containers and over-range integers here.
fn sqlite_text_binding(value: Option<&Value>, parameter: usize) -> ChangeResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        Value::Bool(value) => Ok(Some(if *value { "1" } else { "0" }.into())),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return Ok(Some(value.to_string()));
            }
            if let Some(value) = number.as_u64() {
                return i64::try_from(value)
                    .map(|value| Some(value.to_string()))
                    .map_err(|_| ChangeError::NumberOutOfRange);
            }
            let value = number.as_f64().ok_or(ChangeError::NumberOutOfRange)?;
            let connection = Connection::open_in_memory()?;
            let text = connection.query_row(
                "SELECT CAST(?1 AS TEXT)",
                [rusqlite::types::Value::Real(value)],
                |row| row.get(0),
            )?;
            Ok(Some(text))
        }
        Value::Array(_) | Value::Object(_) => Err(ChangeError::Invalid(format!(
            "Error binding parameter {parameter}: type '{}' is not supported",
            python_type(value)
        ))),
    }
}

/// Upsert well-formed GitHub rows and report the count of newly inserted PRs.
pub fn upsert<S: ChangeStore, C: ChangeClock>(
    clock: &C,
    store: &mut S,
    rows: &[Value],
) -> ChangeResult<usize> {
    let mut added = 0usize;
    for row in rows {
        let Some(object) = row.as_object() else {
            continue;
        };
        let Some(raw_number) = object.get("number") else {
            continue;
        };
        let (id_number, pr) = match raw_number {
            Value::Bool(value) => (
                if *value { "True" } else { "False" }.to_owned(),
                i64::from(*value),
            ),
            Value::Number(number) => {
                let Some(number) = number
                    .as_i64()
                    .or_else(|| number.as_u64().and_then(|n| i64::try_from(n).ok()))
                else {
                    continue;
                };
                (number.to_string(), number)
            }
            _ => continue,
        };
        let Some(merged_at_value) = object
            .get("mergedAt")
            .filter(|value| crate::pending::python_truthy(value))
        else {
            continue;
        };
        let mut labels = Vec::new();
        if let Some(raw_labels) = object
            .get("labels")
            .filter(|value| crate::pending::python_truthy(value))
        {
            match raw_labels {
                Value::Array(entries) => {
                    for entry in entries {
                        if let Some(label) = entry.as_object() {
                            labels.push(label.get("name").cloned().unwrap_or(Value::Null));
                        }
                    }
                }
                // Python iterates dict keys and string characters here; neither
                // is a label object, so they are ignored by the comprehension.
                Value::Object(_) | Value::String(_) => {}
                value => {
                    return Err(ChangeError::Invalid(format!(
                        "'{}' object is not iterable",
                        python_type(value)
                    )));
                }
            }
        }
        python_sort_labels(&mut labels)?;
        let merge_commit_value = match object
            .get("mergeCommit")
            .filter(|value| crate::pending::python_truthy(value))
        {
            Some(Value::Object(merge)) => merge.get("oid"),
            Some(value) => {
                return Err(ChangeError::Invalid(format!(
                    "'{}' object has no attribute 'get'",
                    python_type(value)
                )));
            }
            None => None,
        };
        let merge_commit = sqlite_text_binding(merge_commit_value, 4)?;
        let Some(merged_at) = merged_at_value.as_str() else {
            return Err(ChangeError::Invalid(
                "mergedAt must be an ISO timestamp".into(),
            ));
        };
        let parsed = clock.parse_time(merged_at)?;
        let stamp = clock.iso(parsed);
        let title = sqlite_text_binding(object.get("title"), 6)?;
        let change = ChangeRecord {
            change_id: format!("pr:{id_number}"),
            kind: "pr".into(),
            pr: Some(pr),
            merge_commit,
            merged_at: stamp,
            title,
            labels,
            added_at: clock.iso(clock.now()),
        };
        added += usize::from(store.upsert_change(&change)?);
    }
    Ok(added)
}

/// Synchronise merged PRs in resumable, durable ranges.
pub fn sync<S, C, P>(
    clock: &C,
    source: &mut P,
    store: &mut S,
    limits: SyncLimits,
) -> ChangeResult<(usize, usize, usize)>
where
    S: ChangeStore,
    C: ChangeClock,
    P: PullRequestSource,
{
    let mut state = store.sync_state()?.unwrap_or_else(|| json!({}));
    if !state
        .get("pending")
        .is_some_and(crate::pending::python_truthy)
    {
        let through_value = state.get("through");
        let through = if through_value.is_some_and(crate::pending::python_truthy) {
            Some(
                through_value
                    .and_then(Value::as_str)
                    .ok_or(ChangeError::MalformedState)?
                    .to_owned(),
            )
        } else {
            None
        };
        let low = if let Some(through) = through.as_deref() {
            let previous = clock.parse_time(through)?;
            clock.iso(plus_micros(previous, -86_400_000_000)?)
        } else {
            SYNC_FLOOR.into()
        };
        let high = clock.iso(floor_second(clock.now())?);
        let map = state_object_mut(&mut state)?;
        map.insert("through".into(), through.map_or(Value::Null, Value::String));
        map.insert("sweep".into(), Value::String(high.clone()));
        map.insert("pending".into(), json!([[low, high]]));
    }
    if state.get("incomplete").is_none() {
        state_object_mut(&mut state)?.insert("incomplete".into(), json!([]));
    }
    let mut added = 0usize;
    let mut queries = 0usize;
    while !state_array(&state, "pending")?.is_empty() && queries < limits.pages_per_run {
        let item = state_array(&state, "pending")?
            .last()
            .ok_or(ChangeError::MalformedState)?;
        let range = item.as_array().ok_or(ChangeError::MalformedState)?;
        if range.len() < 2 {
            return Err(ChangeError::MalformedState);
        }
        let low = value_string(&range[0])
            .ok_or(ChangeError::MalformedState)?
            .to_owned();
        let high = value_string(&range[1])
            .ok_or(ChangeError::MalformedState)?
            .to_owned();
        let limit = if let Some(value) = range.get(2) {
            value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(ChangeError::MalformedState)?
        } else {
            limits.page_limit
        };
        let rows = source.merged_between(&low, &high, limit)?;
        queries += 1;
        store.begin_sync_checkpoint()?;
        let checkpoint: ChangeResult<()> = (|| {
            state_array_mut(&mut state, "pending")?.pop();
            added += upsert(clock, store, &rows)?;
            let first = clock.parse_time(&low)?;
            let last = clock.parse_time(&high)?;
            if rows.len() >= limit {
                let elapsed = delta_micros(first, last)?;
                if elapsed >= 1_000_000 {
                    let middle = split(clock, &rows, first, last)?;
                    let older_high = plus_micros(middle, -1_000_000)?;
                    state_array_mut(&mut state, "pending")?
                        .push(json!([low, clock.iso(older_high)]));
                    state_array_mut(&mut state, "pending")?.push(json!([clock.iso(middle), high]));
                } else if limit < limits.search_limit {
                    state_array_mut(&mut state, "pending")?.push(json!([
                        low,
                        high,
                        limits.search_limit
                    ]));
                } else {
                    let candidate = json!([low, high]);
                    if !state_array(&state, "incomplete")?.contains(&candidate) {
                        state_array_mut(&mut state, "incomplete")?.push(candidate);
                    }
                }
            }
            store.save_sync_state(&state)
        })();
        if let Err(error) = checkpoint {
            store.abort_sync_checkpoint();
            return Err(error);
        }
    }
    let pending = state_array(&state, "pending")?.len();
    if pending == 0 {
        let sweep = state
            .get("sweep")
            .cloned()
            .ok_or(ChangeError::MalformedState)?;
        let incomplete = state
            .get("incomplete")
            .cloned()
            .unwrap_or_else(|| json!([]));
        state = json!({"through": sweep, "pending": [], "incomplete": incomplete});
        store.save_sync_state(&state)?;
    }
    Ok((added, pending, state_array(&state, "incomplete")?.len()))
}

/// Start of the newer half of a full search page, clamped to `(first, last]`.
pub fn split<C: ChangeClock>(
    clock: &C,
    rows: &[Value],
    first: jiff::Timestamp,
    last: jiff::Timestamp,
) -> ChangeResult<jiff::Timestamp> {
    let mut stamps = Vec::new();
    for row in rows {
        if let Some(stamp) = row
            .as_object()
            .and_then(|row| row.get("mergedAt"))
            .and_then(Value::as_str)
        {
            stamps.push(floor_second(clock.parse_time(stamp)?)?);
        }
    }
    stamps.sort();
    let middle = if stamps.is_empty() {
        let half = delta_micros(first, last)? / 2;
        plus_micros(first, half)?
    } else {
        stamps[stamps.len() / 2]
    };
    let min = plus_micros(first, 1_000_000)?;
    Ok(middle.max(min).min(last))
}

/// Insert a named manual marker. Duplicate labels at the same timestamp are idempotent.
pub fn add<S: ChangeStore, C: ChangeClock>(
    clock: &C,
    store: &mut S,
    label: &str,
    at: &str,
) -> ChangeResult<String> {
    let clean = crate::pending::python_strip(label);
    if clean.is_empty() {
        return Err(ChangeError::Invalid("--add needs a non-empty label".into()));
    }
    let parsed = clock
        .parse_time(at)
        .map_err(|_| date_error(at, "--at must be an ISO date or timestamp"))?;
    let stamp = clock.iso(parsed);
    // Python hashes the original label, then strips only the stored title.
    let identity = clock.identity(&[label, &stamp])?;
    let change_id = format!("manual:{}", identity.chars().take(16).collect::<String>());
    store.insert_manual_change(&ChangeRecord {
        change_id: change_id.clone(),
        kind: "manual".into(),
        pr: None,
        merge_commit: None,
        merged_at: stamp,
        title: Some(clean.to_owned()),
        labels: Vec::new(),
        added_at: clock.iso(clock.now()),
    })?;
    Ok(change_id)
}

pub fn changes<S: ChangeStore>(
    store: &mut S,
    since: Option<&str>,
) -> ChangeResult<Vec<ChangeRecord>> {
    store.changes(since.filter(|value| !value.is_empty()))
}

pub fn reference(change: &ChangeRecord) -> String {
    change
        .pr
        .map(|pr| format!("#{pr}"))
        .or_else(|| change.title.clone())
        .unwrap_or_default()
}

/// Resolve a PR number, merge revision or ISO timestamp to a marker and pivot.
pub fn pivot<S, C, G>(
    clock: &C,
    store: &mut S,
    git: &mut G,
    reference: &str,
) -> ChangeResult<(Option<ChangeRecord>, jiff::Timestamp)>
where
    S: ChangeStore,
    C: ChangeClock,
    G: RevisionSource,
{
    let value = crate::pending::python_strip(reference);
    if let Some(number) = parse_pr_reference(value) {
        let row = store.change_by_pr(number)?.ok_or_else(|| {
            ChangeError::Invalid(format!(
                "PR {value} has no change marker; run `provenant telemetry changes --sync`"
            ))
        })?;
        return Ok((Some(row.clone()), clock.parse_time(&row.merged_at)?));
    }
    if is_commit_prefix(value) {
        if let Some(row) = store.change_by_commit_prefix(&value.to_ascii_lowercase())? {
            return Ok((Some(row.clone()), clock.parse_time(&row.merged_at)?));
        }
    }
    if let Ok(stamp) = clock.parse_time(value) {
        return Ok((None, stamp));
    }
    if valid_git_revision(value) {
        if let Some(stamp) = git.commit_time(value)? {
            return Ok((None, clock.parse_time(&stamp)?));
        }
    }
    Err(ChangeError::Invalid(format!(
        "cannot resolve {} as a PR number, revision or ISO date",
        crate::pending::python_repr(reference)
    )))
}

fn parse_pr_reference(value: &str) -> Option<i64> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if digits.is_empty() || digits.chars().count() > 6 {
        return None;
    }
    digits.chars().try_fold(0i64, |number, digit| {
        number
            .checked_mul(10)?
            .checked_add(i64::from(crate::pending::decimal_digit(digit)?))
    })
}
fn is_commit_prefix(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn valid_git_revision(value: &str) -> bool {
    static PATTERN: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| regex::Regex::new(r"\A\w[\w./^~-]*\z").ok())
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(value))
}

/// Metrics from the ledger's own stats owner for `[start, end)`.
pub fn side<S: ChangeStore, C: ChangeClock>(
    clock: &C,
    store: &mut S,
    start: jiff::Timestamp,
    end: jiff::Timestamp,
) -> ChangeResult<Side> {
    let start_text = clock.iso(start);
    let end_text = clock.iso(end);
    let report = store.stats(&start_text, Some(&end_text))?;
    let lane = report.lane_totals.first().cloned().unwrap_or_default();
    let usage = report.usage_totals.first().cloned().unwrap_or_default();
    let duration = delta_micros(start, end)?;
    let whole_days = duration.div_euclid(86_400_000_000);
    let remaining = duration.rem_euclid(86_400_000_000);
    let total_seconds = whole_days as f64 * 86_400.0
        + (remaining / 1_000_000) as f64
        + (remaining % 1_000_000) as f64 / 1_000_000.0;
    let days = (total_seconds / 86_400.0).max(1.0 / 24.0);
    let limits = report
        .limits_per_day
        .iter()
        .try_fold(0u64, |sum, row| sum.checked_add(row.events))
        .ok_or(ChangeError::NumberOutOfRange)?;
    let cost = usage.known_cost_usd;
    let priced =
        usage
            .rows
            .checked_sub(usage.missing_cost_rows)
            .ok_or(ChangeError::MalformedReport(
                "missing cost rows exceed usage rows".into(),
            ))?;
    let samples = [
        (Metric::SuccessRate, lane.lanes),
        (Metric::RetriesPerLane, lane.lanes),
        (Metric::LatencyP50S, lane.latency_samples),
        (Metric::CacheReadShare, usage.cache_share_rows),
        (Metric::CostUsdPerDay, priced),
        (Metric::LimitEventsPerDay, usage.rows),
    ]
    .into_iter()
    .collect();
    Ok(Side {
        start: start_text,
        end: end_text,
        lanes: lane.lanes,
        usage_rows: usage.rows,
        samples,
        inconclusive: lane.lanes < MIN_SAMPLE,
        success_rate: lane.success_rate,
        retries_per_lane: if lane.lanes > 0 {
            Some(crate::costs::round(
                lane.retries as f64 / lane.lanes as f64,
                3,
            ))
        } else {
            None
        },
        latency_p50_s: lane.latency_p50_s,
        cache_read_share: (usage.cache_share_input > 0).then(|| {
            crate::costs::round(
                usage.cache_share_read as f64 / usage.cache_share_input as f64,
                3,
            )
        }),
        cost_usd: cost.map(|value| crate::costs::round(value, 4)),
        cost_usd_per_day: if usage.rows > 0 {
            Some(crate::costs::round(
                cost.ok_or_else(|| {
                    ChangeError::MalformedReport(
                        "usage rows exist without a known-cost total".into(),
                    )
                })? / days,
                4,
            ))
        } else {
            None
        },
        missing_cost_rows: usage.missing_cost_rows,
        cost_coverage: (usage.rows > 0)
            .then(|| crate::costs::round(priced as f64 / usage.rows as f64, 3)),
        limit_events: limits,
        limit_events_per_day: crate::costs::round(limits as f64 / days, 3),
        error_classes: lane
            .error_classes
            .iter()
            .map(|(key, value)| (key.clone(), *value))
            .collect(),
    })
}

pub fn compare<S, C, G>(
    clock: &C,
    store: &mut S,
    git: &mut G,
    reference: &str,
    span: &str,
) -> ChangeResult<CompareReport>
where
    S: ChangeStore,
    C: ChangeClock,
    G: RevisionSource,
{
    let (change, at) = pivot(clock, store, git, reference)?;
    let mut report = compare_at(clock, store, change.clone(), at, span, None)?;
    if change.is_none() {
        report.reference = reference.to_owned();
    }
    Ok(report)
}

pub fn compare_at<S: ChangeStore, C: ChangeClock>(
    clock: &C,
    store: &mut S,
    change: Option<ChangeRecord>,
    at: jiff::Timestamp,
    span: &str,
    now: Option<jiff::Timestamp>,
) -> ChangeResult<CompareReport> {
    let hours = window(span)?;
    let width = i64::try_from(hours)
        .ok()
        .and_then(|hours| hours.checked_mul(3_600_000_000))
        .ok_or(ChangeError::NumberOutOfRange)?;
    let current = match now {
        Some(value) => value,
        None => floor_second(clock.now())?,
    };
    let before = side(clock, store, plus_micros(at, -width)?, at)?;
    let after_end = plus_micros(at, width)?.min(current).max(at);
    let after = side(clock, store, at, after_end)?;
    let metrics = Metric::ALL
        .into_iter()
        .map(|metric| MetricResult {
            metric,
            before: before.value(metric),
            after: after.value(metric),
            n_before: before.samples.get(&metric).copied().unwrap_or(0),
            n_after: after.samples.get(&metric).copied().unwrap_or(0),
            sample: metric.sample().into(),
            verdict: verdict(metric, &before, &after),
        })
        .collect();
    Ok(CompareReport {
        schema: "provenant.telemetry.compare.v1".into(),
        reference: change
            .as_ref()
            .map(reference)
            .unwrap_or_else(|| clock.iso(at)),
        change,
        pivot: clock.iso(at),
        window: span.into(),
        min_sample: MIN_SAMPLE,
        before,
        after,
        metrics,
    })
}

fn percentage(value: f64) -> String {
    format!("{:.0}%", value * 100.0)
}
fn general(value: f64, precision: usize) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    // Round first, then choose fixed or scientific notation from the rounded
    // exponent. This matches Python's `g` format at boundaries such as
    // 999.9 (`1e+03`) and 9.999e-5 (`0.0001`).
    let significant = precision.max(1);
    let scientific = format!("{value:.digits$e}", digits = significant - 1);
    let (rounded_mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent = exponent.parse::<i32>().unwrap_or(0);
    if exponent < -4 || exponent >= significant as i32 {
        let mantissa = rounded_mantissa.trim_end_matches('0').trim_end_matches('.');
        return format!(
            "{mantissa}e{}{abs:02}",
            if exponent < 0 { '-' } else { '+' },
            abs = exponent.abs()
        );
    }
    let rounded = scientific.parse::<f64>().unwrap_or(value);
    let decimals = (significant as i32 - 1 - exponent).max(0) as usize;
    let text = format!("{rounded:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').into()
    } else {
        text
    }
}
pub fn show(value: Option<f64>, metric: Option<Metric>) -> String {
    let Some(value) = value else {
        return "-".into();
    };
    if matches!(metric, Some(Metric::SuccessRate | Metric::CacheReadShare)) {
        percentage(value)
    } else {
        general(value, 3)
    }
}
fn bounded_table(
    headers: &[&str],
    rows: &[Vec<Option<String>>],
    limit: Option<usize>,
    always: &[&str],
) -> ChangeResult<Vec<String>> {
    let headers = headers.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>();
    crate::report::bounded_table(&headers, rows, limit, always).map_err(ChangeError::from)
}

pub fn render_compare(report: &CompareReport) -> ChangeResult<String> {
    let title = report
        .change
        .as_ref()
        .filter(|change| change.pr.is_some())
        .and_then(|change| change.title.as_deref())
        .map(|title| format!(" {}", title.chars().take(60).collect::<String>()))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "Compare {}{} at {}; window {} each side.",
        report.reference, title, report.pivot, report.window
    )];
    for (name, side) in [("before", &report.before), ("after", &report.after)] {
        lines.push(format!(
            "{name:<6} {}..{} lanes={} usage_rows={}{}",
            side.start,
            side.end,
            side.lanes,
            crate::report::compact(Some(side.usage_rows as f64)),
            if side.inconclusive {
                format!(" inconclusive (n<{MIN_SAMPLE})")
            } else {
                String::new()
            }
        ));
    }
    let rows = report
        .metrics
        .iter()
        .map(|row| {
            vec![
                Some(metric_name(row.metric).into()),
                Some(show(row.before, Some(row.metric))),
                Some(show(row.after, Some(row.metric))),
                Some(format!("{}/{} {}", row.n_before, row.n_after, row.sample)),
                Some(verdict_name(row.verdict).into()),
            ]
        })
        .collect::<Vec<_>>();
    lines.extend(bounded_table(
        &["metric", "before", "after", "n", "verdict"],
        &rows,
        None,
        &["before", "after"],
    )?);
    let mut errors: Vec<_> = report.after.error_classes.iter().collect();
    // Python Counter.most_common is stable for equal counts; retain the source
    // report's insertion order for ties.
    errors.sort_by(|(_, count_a), (_, count_b)| count_b.cmp(count_a));
    if !errors.is_empty() {
        lines.push(format!(
            "after errors: {}",
            errors
                .into_iter()
                .take(5)
                .map(|(name, count)| format!("{name}:{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.push(format!(
        "Verdicts need n>={MIN_SAMPLE} on both sides; smaller samples are inconclusive, never a win. Full report: --json"
    ));
    Ok(fit(&lines, BUDGET))
}

fn metric_name(metric: Metric) -> &'static str {
    metric.spec().map_or("unknown", |spec| spec.name)
}
fn verdict_name(verdict: Verdict) -> &'static str {
    VERDICT_SPECS
        .iter()
        .find(|(candidate, _, _)| *candidate == verdict)
        .map_or("unknown", |(_, name, _)| name)
}

/// Attention counters operate over metadata already selected by the ledger.
pub fn attention_since<S: ChangeStore, C: ChangeClock>(
    _clock: &C,
    store: &mut S,
    since: &str,
) -> ChangeResult<Attention> {
    let events = store.events_since(since)?;
    attention(&events, since).map_err(ChangeError::from)
}

pub fn retro<S: ChangeStore, C: ChangeClock>(
    clock: &C,
    store: &mut S,
    since: Option<&str>,
    span: &str,
    now: Option<jiff::Timestamp>,
) -> ChangeResult<RetroReport> {
    let current = match now {
        Some(value) => value,
        None => floor_second(clock.now())?,
    };
    let start = match since.filter(|value| !value.is_empty()) {
        Some(value) => clock.parse_time(value)?,
        None => plus_micros(current, -604_800_000_000)?,
    };
    let overall = side(clock, store, start, current)?;
    let start_text = clock.iso(start);
    let report = store.stats(&start_text, None)?;
    let mut errors = Vec::<(String, u64)>::new();
    for row in &report.attempts {
        if let Some(class) = row.error_class.as_deref().filter(|class| !class.is_empty()) {
            let entry = errors.iter_mut().find(|(name, _)| name == class);
            if let Some((_, count)) = entry {
                *count = count
                    .checked_add(row.attempts)
                    .ok_or(ChangeError::NumberOutOfRange)?;
            } else {
                errors.push((class.to_owned(), row.attempts));
            }
        }
    }
    errors.sort_by(|(_, count_a), (_, count_b)| count_b.cmp(count_a));
    // Preserve Python Counter.most_common's first-seen tie ordering.
    let top_order = errors.iter().take(5).cloned().collect::<Vec<_>>();
    let top = top_order.iter().cloned().collect::<OrderedCounts>();
    let marked = changes(store, Some(&start_text))?;
    let mut marked_with_comparison = Vec::with_capacity(marked.len());
    for change in marked {
        let at = clock.parse_time(&change.merged_at)?;
        let compared = compare_at(clock, store, Some(change.clone()), at, span, Some(current))?;
        marked_with_comparison.push(RetroChange {
            change,
            compare: compared,
        });
    }
    let lanes_input_required = report
        .lanes
        .iter()
        .filter(|row| row.status == "input_required")
        .try_fold(0u64, |sum, row| sum.checked_add(row.lanes))
        .ok_or(ChangeError::NumberOutOfRange)?;
    let events = store.events_since(&start_text)?;
    let mut event_counts = BTreeMap::<String, u64>::new();
    for event in events {
        let count = event_counts.entry(event.kind).or_default();
        *count = count.checked_add(1).ok_or(ChangeError::NumberOutOfRange)?;
    }
    let mut event_order = event_counts.iter().collect::<Vec<_>>();
    event_order.sort_by(|(name_a, count_a), (name_b, count_b)| {
        count_b.cmp(count_a).then_with(|| name_a.cmp(name_b))
    });
    let events_by_kind = event_order
        .into_iter()
        .map(|(name, count)| (name.clone(), *count))
        .collect::<OrderedCounts>();
    Ok(RetroReport {
        schema: "provenant.telemetry.retro.v1".into(),
        since: start_text.clone(),
        until: clock.iso(current),
        window: span.into(),
        overall,
        lanes_input_required,
        changes: marked_with_comparison,
        top_error_classes: top,
        attention: attention_since(clock, store, &start_text)?,
        events_by_kind,
    })
}

fn marker(verdict: Verdict) -> char {
    VERDICT_SPECS
        .iter()
        .find(|(candidate, _, _)| *candidate == verdict)
        .map_or('?', |(_, _, mark)| *mark)
}

pub fn render_retro(report: &RetroReport, limit: usize) -> ChangeResult<String> {
    let overall = &report.overall;
    let mut lines = vec![format!(
        "Retro {}..{}: lanes={} ok={} usage_rows={} usd={} limit_events={}",
        report.since,
        report.until,
        overall.lanes,
        show(overall.success_rate, Some(Metric::SuccessRate)),
        crate::report::compact(Some(overall.usage_rows as f64)),
        show(overall.cost_usd, None),
        overall.limit_events
    )];
    if !report.changes.is_empty() {
        lines.push(format!(
            "Changes (newest first; {} each side; + improved, - regressed, = within noise, ? inconclusive n<{}):",
            report.window, MIN_SAMPLE
        ));
        for change in report.changes.iter().take(limit) {
            let mut summary = Vec::new();
            for spec in METRICS.iter().filter(|spec| spec.short.is_some()) {
                let metric = spec.metric;
                let short = spec.short.unwrap_or_default();
                let row = change
                    .compare
                    .metrics
                    .iter()
                    .find(|row| row.metric == metric)
                    .ok_or_else(|| ChangeError::MalformedReport("metric missing".into()))?;
                summary.push(format!(
                    "{short} {}->{}{}",
                    show(row.before, Some(metric)),
                    show(row.after, Some(metric)),
                    marker(row.verdict)
                ));
            }
            let reference = change
                .change
                .pr
                .map(|pr| format!("#{pr}"))
                .unwrap_or_else(|| "marker".into());
            let title = change
                .change
                .title
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>();
            lines.push(format!(
                "  {reference} {} {title}: {} lanes={}/{}",
                change.change.merged_at.chars().take(10).collect::<String>(),
                summary.join(" "),
                change.compare.before.lanes,
                change.compare.after.lanes
            ));
        }
        if report.changes.len() > limit {
            lines.push(format!(
                "  {} more; use --json",
                report.changes.len() - limit
            ));
        }
    } else {
        lines.push(
            "No change markers in this window; run `provenant telemetry changes --sync`.".into(),
        );
    }
    if !report.top_error_classes.is_empty() {
        lines.push(format!(
            "Top error classes (attempts): {}",
            report
                .top_error_classes
                .iter()
                .map(|(name, count)| format!("{name}:{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let seen = &report.attention;
    let mut parts = vec![
        format!("sessions started {}", seen.sessions_started),
        format!("lanes awaiting input {}", report.lanes_input_required),
    ];
    if let Some(value) = seen.user_prompts {
        parts.push(format!("user prompts {value}"));
    }
    if let Some(value) = seen.tool_decisions {
        parts.push(format!("tool decisions {value}"));
    }
    if let Some(active) = &seen.active_time_s {
        if !active.is_empty() {
            let mut entries = Vec::new();
            for (kind, seconds) in active {
                entries.push(format!("{kind} {:.1}h", seconds.as_f64() / 3600.0));
            }
            parts.push(format!("active {}", entries.join(" ")));
        }
    }
    lines.push(format!("Attention: {}", parts.join("; ")));
    if !report.events_by_kind.is_empty() {
        let kinds = report.events_by_kind.iter().collect::<Vec<_>>();
        let shown = kinds
            .iter()
            .take(limit)
            .map(|(kind, count)| format!("{kind}:{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!(
            "Events: {shown}{}",
            if kinds.len() > limit {
                format!(" (+{} kinds)", kinds.len() - limit)
            } else {
                String::new()
            }
        ));
    }
    lines.push("Detail: provenant stats --compare <PR#|rev|date> [--window 7d] [--json]".into());
    Ok(fit(&lines, BUDGET))
}

pub fn render_changes(rows: &[ChangeRecord], limit: usize) -> ChangeResult<String> {
    let cells = rows
        .iter()
        .map(|row| -> ChangeResult<Vec<Option<String>>> {
            let labels = row
                .labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    label.as_str().ok_or_else(|| {
                        ChangeError::Invalid(format!(
                            "sequence item {index}: expected str instance, {} found",
                            python_type(label)
                        ))
                    })
                })
                .collect::<ChangeResult<Vec<_>>>()?
                .join(",")
                .chars()
                .take(30)
                .collect::<String>();
            Ok(vec![
                Some(row.merged_at.clone()),
                Some(if row.pr.is_some() {
                    reference(row)
                } else {
                    "manual".into()
                }),
                row.merge_commit
                    .as_deref()
                    .map(|commit| commit.chars().take(10).collect::<String>())
                    .filter(|commit| !commit.is_empty()),
                (!labels.is_empty()).then_some(labels),
                Some(
                    row.title
                        .as_deref()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect(),
                ),
            ])
        })
        .collect::<ChangeResult<Vec<_>>>()?;
    let mut lines = if rows.is_empty() {
        vec!["No change markers; run `provenant telemetry changes --sync`.".into()]
    } else {
        bounded_table(
            &["merged_at", "change", "commit", "labels", "title"],
            &cells,
            Some(limit),
            &[],
        )?
    };
    if rows.len() > limit {
        lines.push(format!("{} more; use --json", rows.len() - limit));
    }
    Ok(fit(&lines, BUDGET))
}

#[cfg(test)]
mod composition_tests {
    use super::*;

    const PYTHON_CHANGE_BOOTSTRAP: &str = r#"
import importlib.util, json, pathlib, sqlite3, sys, types
root = pathlib.Path(sys.argv[1])
def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module
telemetry = load('port_telemetry_source', root / 'scripts/telemetry.py')
owner = load('port_changes_source', root / 'scripts/telemetry_changes.py')
input = json.load(sys.stdin)
report_owner = telemetry.report_owner()
report_owner.stats = lambda t, db, since=None, prices=None, summary=False, until=None: input.get('stats', {})
t = types.SimpleNamespace(
    now=lambda: input.get('now', '2026-10-23T00:00:00Z'),
    since_time=telemetry.since_time,
    iso=telemetry.iso,
    identity=telemetry.identity,
    report_owner=lambda: report_owner,
    PRODUCT_ROOT=root,
)
db = sqlite3.connect(':memory:')
db.row_factory = sqlite3.Row
db.executescript('''
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE changes (change_id TEXT PRIMARY KEY, kind TEXT NOT NULL, pr INTEGER,
 merge_commit TEXT, merged_at TEXT NOT NULL, title TEXT, labels TEXT NOT NULL DEFAULT '[]', added_at TEXT NOT NULL);
CREATE TABLE events (event_id TEXT PRIMARY KEY, kind TEXT NOT NULL, ts TEXT, session TEXT, attrs TEXT NOT NULL DEFAULT '{}');
''')
for row in input.get('markers', []):
    db.execute('INSERT INTO changes VALUES (?,?,?,?,?,?,?,?)',
        (row['change_id'], row['kind'], row.get('pr'), row.get('merge_commit'), row['merged_at'],
         row.get('title'), json.dumps(row.get('labels', [])), row['added_at']))
for index, row in enumerate(input.get('events', [])):
    db.execute('INSERT INTO events VALUES (?,?,?,?,?)',
        (f'e{index}', row['kind'], row.get('ts'), row.get('session'), json.dumps(row.get('attrs', {}))))
"#;

    fn python_oracle(code: &str, input: &Value) -> Option<Value> {
        crate::differential::python(&format!("{PYTHON_CHANGE_BOOTSTRAP}\n{code}"), input)
    }

    struct FixedClock(jiff::Timestamp);
    impl ChangeClock for FixedClock {
        fn now(&self) -> jiff::Timestamp {
            self.0
        }
        fn parse_time(&self, value: &str) -> ChangeResult<jiff::Timestamp> {
            crate::parse_time(value).map_err(ChangeError::from)
        }
        fn iso(&self, value: jiff::Timestamp) -> String {
            crate::pending::iso(value)
        }
        fn identity(&self, values: &[&str]) -> ChangeResult<String> {
            let values = values
                .iter()
                .map(|value| Value::String((*value).to_owned()))
                .collect::<Vec<_>>();
            Ok(crate::pending::identity(&values))
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        state: Option<Value>,
        markers: Vec<ChangeRecord>,
        report: StatsReport,
        events: Vec<crate::Event>,
    }
    impl ChangeStore for MemoryStore {
        fn sync_state(&mut self) -> ChangeResult<Option<Value>> {
            Ok(self.state.clone())
        }
        fn save_sync_state(&mut self, state: &Value) -> ChangeResult<()> {
            self.state = Some(state.clone());
            Ok(())
        }
        fn upsert_change(&mut self, change: &ChangeRecord) -> ChangeResult<bool> {
            if let Some(old) = self
                .markers
                .iter_mut()
                .find(|old| old.change_id == change.change_id)
            {
                *old = change.clone();
                Ok(false)
            } else {
                self.markers.push(change.clone());
                Ok(true)
            }
        }
        fn insert_manual_change(&mut self, change: &ChangeRecord) -> ChangeResult<()> {
            if !self
                .markers
                .iter()
                .any(|old| old.change_id == change.change_id)
            {
                self.markers.push(change.clone());
            }
            Ok(())
        }
        fn changes(&mut self, since: Option<&str>) -> ChangeResult<Vec<ChangeRecord>> {
            let mut rows = self
                .markers
                .iter()
                .filter(|row| since.is_none_or(|since| row.merged_at.as_str() >= since))
                .cloned()
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| {
                right
                    .merged_at
                    .cmp(&left.merged_at)
                    .then_with(|| left.change_id.cmp(&right.change_id))
            });
            Ok(rows)
        }
        fn change_by_pr(&mut self, pr: i64) -> ChangeResult<Option<ChangeRecord>> {
            Ok(self.markers.iter().find(|row| row.pr == Some(pr)).cloned())
        }
        fn change_by_commit_prefix(&mut self, prefix: &str) -> ChangeResult<Option<ChangeRecord>> {
            Ok(self
                .markers
                .iter()
                .filter(|row| {
                    row.merge_commit
                        .as_deref()
                        .is_some_and(|commit| commit.starts_with(prefix))
                })
                .min_by(|left, right| {
                    left.merged_at
                        .cmp(&right.merged_at)
                        .then_with(|| left.change_id.cmp(&right.change_id))
                })
                .cloned())
        }
        fn events_since(&mut self, since: &str) -> ChangeResult<Vec<crate::Event>> {
            Ok(self
                .events
                .iter()
                .filter(|event| event.ts.as_deref().is_some_and(|ts| ts >= since))
                .cloned()
                .collect())
        }
    }
    impl ReportOwner for MemoryStore {
        fn stats(&mut self, _since: &str, _until: Option<&str>) -> ChangeResult<StatsReport> {
            Ok(self.report.clone())
        }
    }

    #[derive(Default)]
    struct FilteringSource {
        rows: Vec<Value>,
        calls: Vec<(String, String, usize)>,
    }
    impl PullRequestSource for FilteringSource {
        fn merged_between(
            &mut self,
            low: &str,
            high: &str,
            limit: usize,
        ) -> ChangeResult<Vec<Value>> {
            self.calls.push((low.into(), high.into(), limit));
            let low = crate::parse_time(low)?;
            let high = crate::parse_time(high)?;
            let mut rows = self
                .rows
                .iter()
                .filter(|row| {
                    row.get("mergedAt")
                        .and_then(Value::as_str)
                        .and_then(|stamp| crate::parse_time(stamp).ok())
                        .is_some_and(|stamp| stamp >= low && stamp <= high)
                })
                .cloned()
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| {
                left.get("mergedAt")
                    .and_then(Value::as_str)
                    .cmp(&right.get("mergedAt").and_then(Value::as_str))
            });
            rows.truncate(limit);
            Ok(rows)
        }
    }

    #[derive(Default)]
    struct OneRevision(Option<String>);
    impl RevisionSource for OneRevision {
        fn commit_time(&mut self, _revision: &str) -> ChangeResult<Option<String>> {
            Ok(self.0.clone())
        }
    }

    struct FixedRunner {
        output: std::result::Result<UtilityOutput, UtilityFailure>,
        requests: Vec<UtilityRequest>,
    }
    impl UtilityRunner for FixedRunner {
        fn run(
            &mut self,
            request: &UtilityRequest,
        ) -> std::result::Result<UtilityOutput, UtilityFailure> {
            self.requests.push(request.clone());
            self.output.clone()
        }
    }

    fn clock() -> FixedClock {
        FixedClock(crate::parse_time("2026-10-23T00:00:00Z").unwrap())
    }
    fn pull(number: i64, merged_at: String, title: &str) -> Value {
        json!({
            "number": number,
            "mergeCommit": {"oid": format!("{number:07}abcdef")},
            "mergedAt": merged_at,
            "title": title,
            "labels": [{"name": "area:telemetry"}, {"name": "area:rust"}]
        })
    }
    fn hourly_rows(count: usize) -> Vec<Value> {
        let first = crate::parse_time("2026-09-01T00:00:00Z").unwrap();
        (0..count)
            .map(|index| {
                let stamp = plus_micros(first, index as i64 * 3_600_000_000).unwrap();
                pull(index as i64 + 1, crate::pending::iso(stamp), "merge")
            })
            .collect()
    }
    fn report(lanes: u64) -> StatsReport {
        StatsReport {
            lane_totals: vec![LaneTotal {
                lanes,
                retries: lanes / 2,
                latency_samples: lanes,
                success_rate: Some(0.75),
                latency_p50_s: Some(8.0),
                error_classes: [("provider_timeout".into(), lanes / 4)]
                    .into_iter()
                    .collect(),
            }],
            usage_totals: vec![UsageTotal {
                rows: lanes,
                known_cost_usd: Some(12.34567),
                missing_cost_rows: 0,
                cache_share_rows: lanes,
                cache_share_input: 100,
                cache_share_read: 50,
            }],
            limits_per_day: vec![LimitTotal { events: 3 }],
            attempts: vec![AttemptTotal {
                error_class: Some("provider_timeout".into()),
                attempts: 4,
            }],
            lanes: vec![LaneStatusTotal {
                status: "input_required".into(),
                lanes: 2,
            }],
        }
    }

    #[test]
    fn sync_splits_full_pages_and_is_idempotent() {
        let clock = clock();
        let mut source = FilteringSource {
            rows: hourly_rows(201),
            ..FilteringSource::default()
        };
        let mut store = MemoryStore::default();
        let (added, pending, incomplete) =
            sync(&clock, &mut source, &mut store, SyncLimits::default()).unwrap();
        assert_eq!((added, pending, incomplete), (201, 0, 0));
        assert_eq!(store.markers.len(), 201);
        assert!(source.calls.len() <= 5);
        assert!(source.calls.iter().all(|call| call.2 == SYNC_LIMIT));
        let (added, pending, _) =
            sync(&clock, &mut source, &mut store, SyncLimits::default()).unwrap();
        assert_eq!((added, pending), (0, 0));
    }

    #[test]
    fn sync_splits_a_full_two_second_range_before_searching_deeper() {
        let clock = clock();
        let rows = (0..8)
            .map(|index| {
                let second = index % 2;
                pull(800 + index, format!("2026-09-01T00:00:0{second}Z"), "burst")
            })
            .collect();
        let mut source = FilteringSource {
            rows,
            ..FilteringSource::default()
        };
        let mut store = MemoryStore {
            state: Some(json!({
                "through": null,
                "sweep": "2026-09-02T00:00:00Z",
                "incomplete": [],
                "pending": [["2026-09-01T00:00:00Z", "2026-09-01T00:00:01Z"]]
            })),
            ..MemoryStore::default()
        };
        let (added, pending, incomplete) = sync(
            &clock,
            &mut source,
            &mut store,
            SyncLimits {
                page_limit: 5,
                search_limit: 6,
                pages_per_run: 25,
            },
        )
        .unwrap();
        assert_eq!((added, pending, incomplete), (8, 0, 0));
        assert!(source.calls.len() >= 2);
        assert_eq!(store.markers.len(), 8);
    }

    #[test]
    fn sync_keeps_search_capped_single_second_ranges_incomplete() {
        let clock = clock();
        let rows = (0..7)
            .map(|index| pull(900 + index, "2026-09-01T00:00:00Z".into(), "burst"))
            .collect();
        let mut source = FilteringSource {
            rows,
            ..FilteringSource::default()
        };
        let mut store = MemoryStore {
            state: Some(json!({
                "through": null,
                "sweep": "2026-09-02T00:00:00Z",
                "incomplete": [],
                "pending": [["2026-09-01T00:00:00Z", "2026-09-01T00:00:00Z"]]
            })),
            ..MemoryStore::default()
        };
        let (added, pending, incomplete) = sync(
            &clock,
            &mut source,
            &mut store,
            SyncLimits {
                page_limit: 5,
                search_limit: 6,
                pages_per_run: 25,
            },
        )
        .unwrap();
        assert_eq!((added, pending, incomplete), (6, 0, 1));
        assert_eq!(store.markers.len(), 6);
        assert_eq!(
            store.state.unwrap()["incomplete"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn capped_sync_resumes_durable_ranges() {
        let clock = clock();
        let mut source = FilteringSource {
            rows: hourly_rows(12),
            ..FilteringSource::default()
        };
        let mut store = MemoryStore::default();
        let limits = SyncLimits {
            page_limit: 5,
            search_limit: 1000,
            pages_per_run: 2,
        };
        let (first_added, mut pending, _) = sync(&clock, &mut source, &mut store, limits).unwrap();
        assert!(pending > 0);
        assert!(first_added < 12);
        let mut runs = 1;
        while pending > 0 {
            let (_, next_pending, _) = sync(&clock, &mut source, &mut store, limits).unwrap();
            pending = next_pending;
            runs += 1;
            assert!(runs < 20);
        }
        assert_eq!(store.markers.len(), 12);
        assert_eq!(store.state.as_ref().unwrap()["pending"], json!([]));
    }

    #[test]
    fn capped_sync_policy_matches_python_on_the_same_rows() {
        let rows = hourly_rows(12);
        let input = json!({
            "now":"2026-10-23T00:00:00Z",
            "rows":rows,
            "page_limit":5,
            "pages":2
        });
        let oracle = python_oracle(
            "calls=[]\ndef merged_between(telemetry,low,high,limit=None):\n calls.append([low,high,limit])\n rows=[row for row in input['rows'] if telemetry.since_time(low)<=telemetry.since_time(row['mergedAt'])<=telemetry.since_time(high)]\n return sorted(rows,key=lambda row:row['mergedAt'])[:limit or owner.SYNC_LIMIT]\nowner.SYNC_LIMIT=input['page_limit']\nowner.SYNC_PAGES=input['pages']\nowner.merged_between=merged_between\nadded,pending,incomplete=owner.sync(t,db)\nstate=json.loads(db.execute(\"SELECT value FROM meta WHERE key='changes_sync'\").fetchone()[0])\nprint(json.dumps({'result':[added,pending,incomplete],'state':state,'calls':calls,'rows':owner.changes(db)},sort_keys=True))",
            &input,
        );
        let clock = clock();
        let mut source = FilteringSource {
            rows: input["rows"].as_array().unwrap().clone(),
            ..FilteringSource::default()
        };
        let mut store = MemoryStore::default();
        let (added, pending, incomplete) = sync(
            &clock,
            &mut source,
            &mut store,
            SyncLimits {
                page_limit: input["page_limit"].as_u64().unwrap() as usize,
                search_limit: SEARCH_LIMIT,
                pages_per_run: input["pages"].as_u64().unwrap() as usize,
            },
        )
        .unwrap();
        let markers = changes(&mut store, None).unwrap();
        let rust = json!({
            "result":[added,pending,incomplete],
            "state":store.state,
            "calls":source.calls,
            "rows":markers
        });
        if let Some(oracle) = oracle {
            crate::differential::equal(&rust, &oracle);
        }
    }

    #[test]
    fn manual_marker_and_pivot_keep_python_strip_and_reference_rules() {
        let clock = clock();
        let mut store = MemoryStore::default();
        let id = add(
            &clock,
            &mut store,
            "\u{001c} changed model policy \u{001f}",
            "2026-10-03",
        )
        .unwrap();
        assert!(id.starts_with("manual:"));
        let row = changes(&mut store, None).unwrap().remove(0);
        assert_eq!(row.title.as_deref(), Some("changed model policy"));
        assert_eq!(reference(&row), "changed model policy");
        let mut git = OneRevision(Some("2026-10-04T00:00:00Z".into()));
        let (change, at) = pivot(&clock, &mut store, &mut git, "  topic  ").unwrap();
        assert!(change.is_none());
        assert_eq!(clock.iso(at), "2026-10-04T00:00:00Z");
        assert!(add(&clock, &mut store, "\u{001c}\u{001f}", "2026-10-03").is_err());
    }

    #[test]
    fn upsert_add_list_and_pivot_match_the_python_source() {
        let clock = clock();
        let input = json!({
            "now":"2026-10-23T00:00:00Z",
            "label":"\u{001c} changed 模型 policy \u{001f}",
            "at":"2026-10-05",
            "rows":[
                {"number":4,"mergeCommit":{"oid":"abcdef012345"},"mergedAt":"2026-10-06T00:00:00Z","title":"four","labels":[{"name":"z"},{"name":"a"}]},
                {"number":true,"mergedAt":"2026-10-07T00:00:00Z","title":"boolean PR","labels":[]},
                {"number":5.0,"mergedAt":"2026-10-08T00:00:00Z","title":"float PR"},
                {"number":"6","mergedAt":"2026-10-09T00:00:00Z","title":"string PR"},
                {"number":7,"mergedAt":"","title":"empty timestamp"},
                {"number":8,"mergeCommit":null,"mergedAt":"2026-10-10T00:00:00Z","title":"no commit","labels":["ignored",{"name":"x"}]},
                {"number":9,"mergeCommit":{"oid":123},"mergedAt":"2026-10-11T00:00:00Z","title":1.0,"labels":[{}]},
                {"number":10,"mergeCommit":{"oid":false},"mergedAt":"2026-10-12T00:00:00Z","title":false,"labels":[{"name":true},{"name":false}]}
            ]
        });
        let oracle = python_oracle(
            "added=owner.upsert(t,db,input['rows'])\nmanual=owner.add(t,db,input['label'],input['at'])\nmanual_again=owner.add(t,db,input['label'],input['at'])\nrows=owner.changes(db)\npr,pr_at=owner.pivot(t,db,'4')\ncommit,commit_at=owner.pivot(t,db,'abcdef0')\ndate,date_at=owner.pivot(t,db,'2026-10-05')\nprint(json.dumps({'added':added,'manual':manual,'manual_again':manual_again,'rows':rows,'pivots':[{'change':pr,'at':telemetry.iso(pr_at)},{'change':commit,'at':telemetry.iso(commit_at)},{'change':date,'at':telemetry.iso(date_at)}]},sort_keys=True))",
            &input,
        );

        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE changes (change_id TEXT PRIMARY KEY, kind TEXT NOT NULL, pr INTEGER, \
                 merge_commit TEXT, merged_at TEXT NOT NULL, title TEXT, labels TEXT NOT NULL DEFAULT '[]', added_at TEXT NOT NULL);",
            )
            .unwrap();
        let mut store = SqliteChangeStore::new(&connection, MemoryStore::default());
        let rows = input["rows"].as_array().unwrap().clone();
        let added = upsert(&clock, &mut store, &rows).unwrap();
        let label = input["label"].as_str().unwrap();
        let at = input["at"].as_str().unwrap();
        let manual = add(&clock, &mut store, label, at).unwrap();
        let manual_again = add(&clock, &mut store, label, at).unwrap();
        let rows = changes(&mut store, None).unwrap();
        let mut git = OneRevision(None);
        let (pr, pr_at) = pivot(&clock, &mut store, &mut git, "4").unwrap();
        let (commit, commit_at) = pivot(&clock, &mut store, &mut git, "abcdef0").unwrap();
        let (date, date_at) = pivot(&clock, &mut store, &mut git, "2026-10-05").unwrap();
        let rust = json!({
            "added":added,
            "manual":manual,
            "manual_again":manual_again,
            "rows":rows,
            "pivots":[
                {"change":pr,"at":clock.iso(pr_at)},
                {"change":commit,"at":clock.iso(commit_at)},
                {"change":date,"at":clock.iso(date_at)}
            ]
        });
        if let Some(oracle) = oracle {
            crate::differential::equal(&rust, &oracle);
        }
    }

    #[test]
    fn upsert_malformed_values_match_python_sort_and_attribute_errors() {
        let clock = clock();
        let input = json!({"now":"2026-10-23T00:00:00Z"});
        let oracle = python_oracle(
            "def error(row):\n try: owner.upsert(t,db,[row])\n except Exception as exc: return str(exc)\n raise AssertionError('expected error')\nbase={'number':21,'mergedAt':'2026-10-06T00:00:00Z'}\nerrors={'mixed_labels':error({**base,'labels':[{'name':None},{'name':'x'}]}),'bad_merge_commit':error({**base,'mergeCommit':'bad'})}\nprint(json.dumps(errors))",
            &input,
        );
        let mut store = MemoryStore::default();
        let mixed_labels = json!({"number":21,"mergedAt":"2026-10-06T00:00:00Z","labels":[{"name":null},{"name":"x"}]});
        let bad_merge_commit =
            json!({"number":21,"mergedAt":"2026-10-06T00:00:00Z","mergeCommit":"bad"});
        let rust = json!({
            "mixed_labels":upsert(&clock, &mut store, &[mixed_labels]).unwrap_err().to_string(),
            "bad_merge_commit":upsert(&clock, &mut store, &[bad_merge_commit]).unwrap_err().to_string()
        });
        if let Some(oracle) = oracle {
            crate::differential::equal(&rust, &oracle);
        }
    }

    #[test]
    fn gh_transport_builds_the_exact_bounded_query_and_hides_failures() {
        let output = UtilityOutput {
            return_code: 0,
            stdout: br#"[{"number":4}]"#.to_vec(),
        };
        let runner = FixedRunner {
            output: Ok(output),
            requests: Vec::new(),
        };
        let mut source = GhPullRequestSource::new(runner, PathBuf::from("/repo"));
        let rows = source
            .merged_between("2026-10-01T00:00:00Z", "2026-10-02T00:00:00Z", SYNC_LIMIT)
            .unwrap();
        assert_eq!(rows.len(), 1);
        let runner = source.into_runner();
        let request = &runner.requests[0];
        assert_eq!(request.program, "gh");
        assert_eq!(request.cwd.as_deref(), Some(std::path::Path::new("/repo")));
        assert_eq!(request.timeout, Duration::from_secs(60));
        assert_eq!(request.args[5], "200");
        assert_eq!(
            request.args[9],
            "merged:>=2026-10-01T00:00:00+00:00 merged:<=2026-10-02T00:00:00+00:00"
        );

        let runner = FixedRunner {
            output: Err(UtilityFailure::Spawn {
                python_exception: "FileNotFoundError".into(),
            }),
            requests: Vec::new(),
        };
        let mut source = GhPullRequestSource::new(runner, PathBuf::from("/repo"));
        assert_eq!(
            source
                .merged_between("lo", "hi", 1)
                .unwrap_err()
                .to_string(),
            "gh pr list could not run (FileNotFoundError); install and authenticate gh"
        );

        let runner = FixedRunner {
            output: Ok(UtilityOutput {
                return_code: 4,
                stdout: b"private output".to_vec(),
            }),
            requests: Vec::new(),
        };
        let mut source = GhPullRequestSource::new(runner, PathBuf::from("/repo"));
        assert_eq!(
            source
                .merged_between("lo", "hi", 1)
                .unwrap_err()
                .to_string(),
            "gh pr list failed with exit 4; run `gh auth status`"
        );

        let runner = FixedRunner {
            output: Ok(UtilityOutput {
                return_code: 0,
                stdout: b"not json".to_vec(),
            }),
            requests: Vec::new(),
        };
        let mut source = GhPullRequestSource::new(runner, PathBuf::from("/repo"));
        assert_eq!(
            source
                .merged_between("lo", "hi", 1)
                .unwrap_err()
                .to_string(),
            "gh pr list returned malformed JSON"
        );
    }

    #[test]
    fn add_and_pivot_refusal_messages_match_the_python_source() {
        let clock = clock();
        let input = json!({"now":"2026-10-23T00:00:00Z"});
        let oracle = python_oracle(
            "def error(call):\n try: call()\n except Exception as exc: return str(exc)\n raise AssertionError('expected error')\nerrors={'empty_label':error(lambda:owner.add(t,db,'  ','2026-10-03')),'bad_date':error(lambda:owner.add(t,db,'valid','not-a-date')),'missing_pr':error(lambda:owner.pivot(t,db,'999')),'bad_reference':error(lambda:owner.pivot(t,db,'not a ref!'))}\nprint(json.dumps(errors))",
            &input,
        );
        let mut store = MemoryStore::default();
        let mut git = OneRevision(None);
        let rust = json!({
            "empty_label":add(&clock, &mut store, "  ", "2026-10-03").unwrap_err().to_string(),
            "bad_date":add(&clock, &mut store, "valid", "not-a-date").unwrap_err().to_string(),
            "missing_pr":pivot(&clock, &mut store, &mut git, "999").unwrap_err().to_string(),
            "bad_reference":pivot(&clock, &mut store, &mut git, "not a ref!").unwrap_err().to_string()
        });
        if let Some(oracle) = oracle {
            crate::differential::equal(&rust, &oracle);
        }
    }

    #[test]
    fn comparison_and_retro_render_keep_sample_counts_and_untruncated_json() {
        let clock = clock();
        let mut store = MemoryStore {
            report: report(12),
            ..MemoryStore::default()
        };
        let pivot = clock.parse_time("2026-10-10T00:00:00Z").unwrap();
        let compare_input = json!({
            "now":clock.iso(clock.now()),
            "pivot":clock.iso(pivot),
            "span":"7d",
            "stats":serde_json::to_value(&store.report).unwrap()
        });
        let compare_oracle = python_oracle(
            "pivot=telemetry.since_time(input['pivot'])\nreport=owner.compare_at(t,db,None,pivot,input['span'],now=telemetry.since_time(input['now']))\nprint(json.dumps({'report':report,'text':owner.render_compare(t,report)},sort_keys=True))",
            &compare_input,
        );
        let compared = compare_at(&clock, &mut store, None, pivot, "7d", None).unwrap();
        assert_eq!(compared.before.lanes, 12);
        assert_eq!(compared.after.lanes, 12);
        assert_eq!(compared.metrics.len(), 6);
        let text = render_compare(&compared).unwrap();
        assert!(text.contains("12/12 lanes"));
        if let Some(oracle) = compare_oracle {
            crate::differential::equal(&json!({"report":compared,"text":text}), &oracle);
        }

        for index in 0..15 {
            let stamp = format!("2026-10-{:02}T00:00:00Z", 1 + index);
            let mut row = pull(100 + index as i64, stamp.clone(), "long title");
            row["mergedAt"] = Value::String(stamp);
            let _ = upsert(&clock, &mut store, &[row]).unwrap();
        }
        store.events = vec![
            crate::Event {
                kind: "session.start".into(),
                ts: Some("2026-10-12T01:00:00Z".into()),
                session: Some("session-a".into()),
                attrs: json!({}),
                ..crate::Event::default()
            },
            crate::Event {
                kind: "otel.user_prompt".into(),
                ts: Some("2026-10-12T01:01:00Z".into()),
                session: Some("session-a".into()),
                attrs: json!({}),
                ..crate::Event::default()
            },
            crate::Event {
                kind: "otel.metric".into(),
                ts: Some("2026-10-12T01:02:00Z".into()),
                session: Some("session-a".into()),
                attrs: json!({
                    "name":"claude_code.active_time.total",
                    "value":2.5,
                    "type":"user",
                    "temporality":1
                }),
                ..crate::Event::default()
            },
        ];
        let retro_input = json!({
            "now":clock.iso(clock.now()),
            "since":"2026-10-01",
            "span":"7d",
            "stats":serde_json::to_value(&store.report).unwrap(),
            "markers":store.markers,
            "events":store.events
        });
        let retro_oracle = python_oracle(
            "report=owner.retro(t,db,input['since'],input['span'],now=telemetry.since_time(input['now']))\nrows=owner.changes(db)\nprint(json.dumps({'report':report,'text':owner.render_retro(t,report,12),'changes_text':owner.render_changes(t,rows,12)},sort_keys=True))",
            &retro_input,
        );
        let retro = retro(
            &clock,
            &mut store,
            Some("2026-10-01"),
            "7d",
            Some(clock.now()),
        )
        .unwrap();
        assert_eq!(retro.changes.len(), 15);
        let rendered = render_retro(&retro, 12).unwrap();
        assert!(rendered.contains("3 more; use --json"));
        assert!(
            serde_json::to_value(&retro).unwrap()["changes"]
                .as_array()
                .unwrap()
                .len()
                == 15
        );
        let changes_text = render_changes(&changes(&mut store, None).unwrap(), 12).unwrap();
        if let Some(oracle) = retro_oracle {
            crate::differential::equal(
                &json!({
                    "report":retro,
                    "text":rendered,
                    "changes_text":changes_text
                }),
                &oracle,
            );
        }
    }

    #[test]
    fn sqlite_store_commits_sync_pages_and_preserves_marker_ordering() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE changes (
                    change_id TEXT PRIMARY KEY, kind TEXT NOT NULL, pr INTEGER,
                    merge_commit TEXT, merged_at TEXT NOT NULL, title TEXT,
                    labels TEXT NOT NULL DEFAULT '[]', added_at TEXT NOT NULL
                 );
                 CREATE TABLE events (
                    event_id TEXT PRIMARY KEY, kind TEXT NOT NULL, ts TEXT,
                    session TEXT, attrs TEXT NOT NULL DEFAULT '{}'
                 );",
            )
            .unwrap();
        let mut store = SqliteChangeStore::new(&connection, MemoryStore::default());
        let marker = ChangeRecord {
            change_id: "pr:4".into(),
            kind: "pr".into(),
            pr: Some(4),
            merge_commit: Some("abcdef012345".into()),
            merged_at: "2026-10-20T00:00:00Z".into(),
            title: Some("first".into()),
            labels: vec!["area:rust".into(), "area:telemetry".into()],
            added_at: "2026-10-23T00:00:00Z".into(),
        };
        store.begin_sync_checkpoint().unwrap();
        assert!(store.upsert_change(&marker).unwrap());
        store
            .save_sync_state(&json!({"through":"2026-10-20T00:00:00Z","pending":[]}))
            .unwrap();
        assert_eq!(store.sync_state().unwrap().unwrap()["pending"], json!([]));
        assert_eq!(store.change_by_pr(4).unwrap(), Some(marker.clone()));
        assert_eq!(
            store.change_by_commit_prefix("abcdef0").unwrap(),
            Some(marker.clone())
        );
        assert_eq!(store.changes(None).unwrap(), vec![marker.clone()]);

        let mut renamed = marker.clone();
        renamed.title = Some("renamed".into());
        assert!(!store.upsert_change(&renamed).unwrap());
        assert_eq!(
            store.changes(None).unwrap()[0].title.as_deref(),
            Some("renamed")
        );

        connection
            .execute(
                "INSERT INTO events(event_id,kind,ts,session,attrs) VALUES ('e1','session.start',?1,'s','{}')",
                ["2026-10-21T00:00:00Z"],
            )
            .unwrap();
        let events = store.events_since("2026-10-01T00:00:00Z").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "session.start");

        store.begin_sync_checkpoint().unwrap();
        let transient = ChangeRecord {
            change_id: "pr:5".into(),
            pr: Some(5),
            ..marker
        };
        assert!(store.upsert_change(&transient).unwrap());
        store.abort_sync_checkpoint();
        assert!(store.change_by_pr(5).unwrap().is_none());
    }
}
