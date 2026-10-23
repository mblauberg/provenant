//! Change comparison policy, independent of GitHub transport and ledger queries.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MIN_SAMPLE: u64 = 10;
pub const DEFAULT_WINDOW: &str = "7d";
pub const BUDGET: usize = 4000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    SuccessRate, RetriesPerLane, LatencyP50S, CacheReadShare, CostUsdPerDay, LimitEventsPerDay,
}
impl Metric {
    pub const ALL: [Self; 6] = [Self::SuccessRate, Self::RetriesPerLane, Self::LatencyP50S, Self::CacheReadShare, Self::CostUsdPerDay, Self::LimitEventsPerDay];
    pub fn higher(self) -> bool { matches!(self, Self::SuccessRate | Self::CacheReadShare) }
    pub fn sample(self) -> &'static str {
        match self { Self::SuccessRate | Self::RetriesPerLane => "lanes", Self::LatencyP50S => "timed attempts", Self::CacheReadShare => "complete rows", Self::CostUsdPerDay => "priced rows", Self::LimitEventsPerDay => "usage rows" }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict { Improved, Regressed, WithinNoise, Inconclusive }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Side {
    pub start: String, pub end: String, pub lanes: u64, pub usage_rows: u64,
    pub samples: BTreeMap<Metric, u64>, pub inconclusive: bool,
    pub success_rate: Option<f64>, pub retries_per_lane: Option<f64>,
    pub latency_p50_s: Option<f64>, pub cache_read_share: Option<f64>,
    pub cost_usd: Option<f64>, pub cost_usd_per_day: Option<f64>,
    pub missing_cost_rows: u64, pub cost_coverage: Option<f64>,
    pub limit_events: u64, pub limit_events_per_day: f64,
    pub error_classes: BTreeMap<String, u64>,
}
impl Side {
    pub fn value(&self, metric: Metric) -> Option<f64> {
        match metric { Metric::SuccessRate => self.success_rate, Metric::RetriesPerLane => self.retries_per_lane, Metric::LatencyP50S => self.latency_p50_s, Metric::CacheReadShare => self.cache_read_share, Metric::CostUsdPerDay => self.cost_usd_per_day, Metric::LimitEventsPerDay => Some(self.limit_events_per_day) }
    }
}

pub fn verdict(metric: Metric, before: &Side, after: &Side) -> Verdict {
    let nb = before.samples.get(&metric).copied().unwrap_or(0);
    let na = after.samples.get(&metric).copied().unwrap_or(0);
    let (Some(b), Some(a)) = (before.value(metric), after.value(metric)) else { return Verdict::Inconclusive; };
    if nb < MIN_SAMPLE || na < MIN_SAMPLE || metric == Metric::CostUsdPerDay && (before.missing_cost_rows > 0 || after.missing_cost_rows > 0) { return Verdict::Inconclusive; }
    let noise = if metric == Metric::SuccessRate {
        let (nb, na) = (nb as f64, na as f64);
        let pooled = (b * nb + a * na) / (nb + na);
        (a-b).abs() <= 2.0 * (pooled * (1.0-pooled) * (1.0/nb + 1.0/na)).sqrt()
    } else { (a-b).abs() <= 0.10 * a.abs().max(b.abs()) };
    if noise { Verdict::WithinNoise } else if (a > b) == metric.higher() { Verdict::Improved } else { Verdict::Regressed }
}

/// Hours in a comparison window. Overflow is a typed refusal, never a wrap.
pub fn window(value: &str) -> Result<u64> {
    let invalid = || Error::Invalid(format!("window must look like 24h, 7d or 2w, got {value:?}"));
    let Some((suffix, digits)) = value.as_bytes().split_last() else { return Err(invalid()); };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) { return Err(invalid()); }
    let count: u64 = value[..value.len()-1].parse().map_err(|_| invalid())?;
    let factor = match suffix { b'h' => 1, b'd' => 24, b'w' => 168, _ => return Err(invalid()) };
    count.checked_mul(factor).filter(|n| *n > 0).ok_or_else(invalid)
}

/// Keep whole lines, including the omission notice, inside the UTF-8 budget.
pub fn fit(lines: &[String], budget: usize) -> String {
    if lines.iter().map(|s| s.len()+1).sum::<usize>() <= budget { return lines.join("\n"); }
    let reserve = format!("{} lines omitted to fit 4 KB; use --json", lines.len()).len();
    let mut kept = Vec::new(); let mut used = 0;
    for line in lines {
        let size = line.len()+1;
        if used+size+reserve <= budget { kept.push(line.as_str()); used += size; }
    }
    let notice = format!("{} lines omitted to fit 4 KB; use --json", lines.len()-kept.len());
    kept.push(&notice); kept.join("\n")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attention {
    pub sessions_started: u64, pub user_prompts: Option<u64>, pub tool_decisions: Option<u64>,
    pub active_time_s: Option<BTreeMap<String, f64>>,
}
/// Attention proxies only use metadata already retained by the ledger.
pub fn attention(db: &rusqlite::Connection, since: &str) -> Result<Attention> {
    let mut counts = BTreeMap::<String,u64>::new();
    let mut query = db.prepare("SELECT kind, COUNT(*) FROM events WHERE ts >= ? AND kind IN ('session.start','otel.user_prompt','otel.tool_decision','otel.codex.user_prompt','otel.codex.tool_decision') GROUP BY kind")?;
    for row in query.query_map([since], |r| Ok((r.get::<_,String>(0)?, r.get::<_,u64>(1)?)))? {
        let (kind, count) = row?; *counts.entry(kind.replace("otel.codex.", "otel.")).or_default() += count;
    }
    let mut active = BTreeMap::<(Option<String>,String),f64>::new();
    let mut query = db.prepare("SELECT session,attrs FROM events WHERE kind='otel.metric' AND ts >= ?")?;
    for row in query.query_map([since], |r| Ok((r.get::<_,Option<String>>(0)?, r.get::<_,String>(1)?)))? {
        let (session, attrs) = row?; let attrs: serde_json::Value = serde_json::from_str(&attrs)?;
        if attrs["name"] != "claude_code.active_time.total" { continue; }
        let Some(value) = attrs["value"].as_f64() else { continue; };
        let key = (session, attrs["type"].as_str().filter(|s| !s.is_empty()).unwrap_or("total").into());
        let entry = active.entry(key).or_default();
        if attrs["temporality"] == 2 { *entry = entry.max(value); } else { *entry += value; }
    }
    let mut totals = BTreeMap::<String,f64>::new();
    for ((_,kind), seconds) in active { *totals.entry(kind).or_default() += seconds; }
    for value in totals.values_mut() { *value = value.round_ties_even(); }
    Ok(Attention { sessions_started: counts.get("session.start").copied().unwrap_or(0), user_prompts: counts.get("otel.user_prompt").copied(), tool_decisions: counts.get("otel.tool_decision").copied(), active_time_s: if totals.is_empty() { None } else { Some(totals) } })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn side(n: u64) -> Side {
        Side { start: "start".into(), end: "end".into(), lanes:n, usage_rows:n, samples:Metric::ALL.into_iter().map(|k|(k,n)).collect(), inconclusive:n<10, success_rate:Some(0.5), retries_per_lane:Some(1.0), latency_p50_s:Some(10.0), cache_read_share:Some(0.5), cost_usd:Some(10.0), cost_usd_per_day:Some(10.0), missing_cost_rows:0, cost_coverage:Some(1.0), limit_events:0, limit_events_per_day:0.0, error_classes:BTreeMap::new() }
    }
    #[test] fn small_samples_and_missing_prices_never_win() {
        let b = side(100); let mut a = side(9); a.success_rate=Some(1.0);
        assert_eq!(verdict(Metric::SuccessRate,&b,&a), Verdict::Inconclusive);
        a=side(100); a.cost_usd_per_day=Some(0.0); a.missing_cost_rows=1;
        assert_eq!(verdict(Metric::CostUsdPerDay,&b,&a),Verdict::Inconclusive);
    }
    #[test] fn noise_and_direction() {
        let b=side(1000); let mut a=side(1000); a.latency_p50_s=Some(9.5);
        assert_eq!(verdict(Metric::LatencyP50S,&b,&a),Verdict::WithinNoise);
        a.latency_p50_s=Some(5.0);
        assert_eq!(verdict(Metric::LatencyP50S,&b,&a),Verdict::Improved);
        a.success_rate=Some(0.9);
        assert_eq!(verdict(Metric::SuccessRate,&b,&a),Verdict::Improved);
    }
    #[test] fn utf8_budget_keeps_whole_lines() {
        let lines=vec!["é".repeat(2500),"short".into(),"long".repeat(500)];
        let rendered=fit(&lines,BUDGET);
        assert!(rendered.len()<=BUDGET); assert!(rendered.starts_with("short\n"));
        assert!(rendered.contains("1 lines omitted"));
    }
    #[test] fn valid_windows_and_refusals() {
        assert_eq!(window("2w").unwrap(),336);
        for bad in ["0h","-1d","7D","7d\n",""] { assert!(window(bad).is_err()); }
    }
}
