//! Quota history: pure reading classification and plan price lookup.
//!
//! The edge appends the records this module builds. A record carries metadata
//! only: pool names, basis points, reset times and the plan label. It never
//! copies provider payloads, credentials or prompt text.
use crate::{AccountScope, QuotaSnapshot, QuotaStatus, QuotaWindow};
use serde::{Deserialize, Serialize};

pub const HISTORY_SCHEMA_VERSION: u64 = 1;
pub const DEFAULT_HISTORY_MAX_BYTES: u64 = 4 * 1024 * 1024;
pub const MIN_HISTORY_MAX_BYTES: u64 = 64 * 1024;
pub const MAX_HISTORY_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest plan price table kept per account.
pub const MAX_PLAN_PRICES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerKind {
    /// A window's used amount fell or its reset time advanced.
    #[serde(rename = "reset")]
    Reset,
    /// The detected plan changed between two readings.
    #[serde(rename = "plan.change")]
    PlanChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryMarker {
    pub kind: MarkerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRecord {
    pub schema_version: u64,
    pub observed_unix_seconds: i64,
    pub account_key: String,
    pub label: String,
    pub provider: String,
    pub status: QuotaStatus,
    /// Plan as detected or registered for the account; null when unknown.
    pub plan: Option<String>,
    /// Tier as reported by the provider response; null when it reports none.
    pub tier: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub credit_balance: Option<String>,
    pub reset_credits: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<HistoryMarker>,
}

impl HistoryRecord {
    /// A reading worth keeping has at least one window or a credit balance.
    pub fn from_snapshot(snapshot: &QuotaSnapshot) -> Option<Self> {
        let details = snapshot.details.as_ref();
        let record = Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            observed_unix_seconds: snapshot.observed_unix_seconds,
            account_key: snapshot.account.account_key.clone(),
            label: snapshot.account.label.clone(),
            provider: snapshot.account.adapter.clone(),
            status: snapshot.status,
            plan: details.and_then(|d| d.plan.clone()),
            tier: details.and_then(|d| d.tier.clone()),
            windows: snapshot.windows.clone(),
            credit_balance: details.and_then(|d| d.credit_balance.clone()),
            reset_credits: details.and_then(|d| d.reset_credits),
            markers: Vec::new(),
        };
        (!record.windows.is_empty() || record.credit_balance.is_some()).then_some(record)
    }

    /// Whether two readings say the same thing. Time and status do not count:
    /// an identical reading seen again is not new evidence.
    pub fn same_reading(&self, other: &Self) -> bool {
        self.plan == other.plan
            && self.tier == other.tier
            && self.windows == other.windows
            && self.credit_balance == other.credit_balance
            && self.reset_credits == other.reset_credits
    }

    /// The record to append for `snapshot`, or none when it repeats `previous`
    /// or carries no reading. Markers compare against `previous`.
    pub fn observe(previous: Option<&Self>, snapshot: &QuotaSnapshot) -> Option<Self> {
        let mut next = Self::from_snapshot(snapshot)?;
        let Some(previous) = previous else {
            return Some(next);
        };
        if next.same_reading(previous)
            || next.observed_unix_seconds < previous.observed_unix_seconds
        {
            return None;
        }
        for window in &next.windows {
            let Some(before) = previous.windows.iter().find(|w| w.pool == window.pool) else {
                continue;
            };
            let advanced = matches!(
                (before.resets_unix_seconds, window.resets_unix_seconds),
                (Some(a), Some(b)) if b > a
            );
            if window.used_basis_points < before.used_basis_points || advanced {
                next.markers.push(HistoryMarker {
                    kind: MarkerKind::Reset,
                    pool: Some(window.pool.clone()),
                    from: None,
                    to: None,
                });
            }
        }
        // Only two known labels differ: a plan appearing or vanishing is detection noise.
        if let (Some(a), Some(b)) = (&previous.plan, &next.plan)
            && a != b
        {
            next.markers.push(HistoryMarker {
                kind: MarkerKind::PlanChange,
                pool: None,
                from: Some(a.clone()),
                to: Some(b.clone()),
            });
        }
        Some(next)
    }

    pub fn account(&self) -> AccountScope {
        AccountScope {
            adapter: self.provider.clone(),
            account_key: self.account_key.clone(),
            label: self.label.clone(),
        }
    }
}

/// Parse JSON lines, skipping blank, torn and foreign lines.
pub fn decode_lines(bytes: &[u8]) -> Vec<HistoryRecord> {
    bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<HistoryRecord>(line).ok())
        .filter(|r| r.schema_version == HISTORY_SCHEMA_VERSION)
        .collect()
}

pub fn encode_line(record: &HistoryRecord) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    Ok(line)
}

/// One price in an account's `plans` table. The price in force at a time is
/// the entry with the latest `effective_from` on or before that date (UTC).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPrice {
    pub name: String,
    /// Price per month in `currency`; 0 for a free plan. `price_usd` is accepted as a name.
    #[serde(alias = "price_usd")]
    pub price: f64,
    /// ISO 4217 code; USD when omitted.
    #[serde(default = "usd")]
    pub currency: String,
    /// What a free or discounted plan would cost at list price, in USD.
    #[serde(default)]
    pub notional_usd: Option<f64>,
    /// First day the price applies, as YYYY-MM-DD.
    pub effective_from: String,
}

fn usd() -> String {
    "USD".into()
}

impl PlanPrice {
    pub fn date(&self) -> Option<jiff::civil::Date> {
        self.effective_from.parse().ok()
    }
    pub fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.len() <= 64
            && !self.name.chars().any(char::is_control)
            && self.price.is_finite()
            && self.price >= 0.0
            && self.price <= 1_000_000.0
            && self
                .notional_usd
                .is_none_or(|n| n.is_finite() && (0.0..=1_000_000.0).contains(&n))
            && self.currency.len() == 3
            && self.currency.bytes().all(|b| b.is_ascii_uppercase())
            && self.date().is_some()
    }
}

/// Every entry valid, bounded, and at most one price per effective date.
pub fn valid_plans(plans: &[PlanPrice]) -> bool {
    plans.len() <= MAX_PLAN_PRICES
        && plans.iter().all(PlanPrice::valid)
        && plans.iter().enumerate().all(|(i, p)| {
            plans[..i]
                .iter()
                .all(|q| q.effective_from != p.effective_from)
        })
}

/// The price in force at `observed_unix_seconds`, if any entry has started.
pub fn price_in_force(plans: &[PlanPrice], observed_unix_seconds: i64) -> Option<&PlanPrice> {
    let day = jiff::Timestamp::from_second(observed_unix_seconds)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC)
        .date();
    plans
        .iter()
        .filter_map(|p| p.date().map(|d| (d, p)))
        .filter(|(d, _)| *d <= day)
        .max_by_key(|(d, _)| *d)
        .map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QuotaDetails;

    fn window(pool: &str, used: u64, reset: Option<i64>) -> QuotaWindow {
        QuotaWindow {
            pool: pool.into(),
            window_seconds: Some(18_000),
            used_basis_points: used,
            resets_unix_seconds: reset,
        }
    }
    fn snapshot(at: i64, plan: Option<&str>, windows: Vec<QuotaWindow>) -> QuotaSnapshot {
        QuotaSnapshot {
            schema_version: 1,
            account: AccountScope {
                adapter: "claude".into(),
                account_key: "k1".into(),
                label: "work".into(),
            },
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: at,
            source_ref: "src".into(),
            authority_ref: None,
            windows,
            details: Some(QuotaDetails {
                plan: plan.map(str::to_owned),
                ..QuotaDetails::default()
            }),
        }
    }
    fn plan(name: &str, price: f64, from: &str) -> PlanPrice {
        PlanPrice {
            name: name.into(),
            price,
            currency: "USD".into(),
            notional_usd: None,
            effective_from: from.into(),
        }
    }

    #[test]
    fn identical_readings_are_skipped_and_changes_are_kept() {
        let first = HistoryRecord::observe(
            None,
            &snapshot(100, None, vec![window("5h", 100, Some(900))]),
        )
        .unwrap();
        let same = snapshot(160, None, vec![window("5h", 100, Some(900))]);
        assert!(HistoryRecord::observe(Some(&first), &same).is_none());
        let moved = snapshot(160, None, vec![window("5h", 150, Some(900))]);
        let next = HistoryRecord::observe(Some(&first), &moved).unwrap();
        assert!(next.markers.is_empty());
    }

    #[test]
    fn empty_readings_are_not_recorded() {
        assert!(HistoryRecord::observe(None, &snapshot(1, None, vec![])).is_none());
    }

    #[test]
    fn drop_in_use_and_advanced_reset_are_marked() {
        let first = HistoryRecord::observe(
            None,
            &snapshot(100, None, vec![window("5h", 900, Some(500))]),
        )
        .unwrap();
        let dropped = HistoryRecord::observe(
            Some(&first),
            &snapshot(200, None, vec![window("5h", 50, Some(500))]),
        )
        .unwrap();
        assert_eq!(dropped.markers.len(), 1);
        assert_eq!(dropped.markers[0].kind, MarkerKind::Reset);
        assert_eq!(dropped.markers[0].pool.as_deref(), Some("5h"));
        let advanced = HistoryRecord::observe(
            Some(&first),
            &snapshot(200, None, vec![window("5h", 950, Some(900))]),
        )
        .unwrap();
        assert_eq!(advanced.markers[0].kind, MarkerKind::Reset);
        let plain = HistoryRecord::observe(
            Some(&first),
            &snapshot(200, None, vec![window("5h", 950, Some(500))]),
        )
        .unwrap();
        assert!(plain.markers.is_empty());
    }

    #[test]
    fn plan_change_is_marked_only_between_known_plans() {
        let first = HistoryRecord::observe(
            None,
            &snapshot(100, Some("max_5x"), vec![window("5h", 10, None)]),
        )
        .unwrap();
        let changed = HistoryRecord::observe(
            Some(&first),
            &snapshot(200, Some("max_20x"), vec![window("5h", 10, None)]),
        )
        .unwrap();
        assert_eq!(changed.markers.len(), 1);
        assert_eq!(changed.markers[0].kind, MarkerKind::PlanChange);
        assert_eq!(changed.markers[0].from.as_deref(), Some("max_5x"));
        assert_eq!(changed.markers[0].to.as_deref(), Some("max_20x"));
        let lost = HistoryRecord::observe(
            Some(&first),
            &snapshot(200, None, vec![window("5h", 10, None)]),
        )
        .unwrap();
        assert!(lost.markers.is_empty());
    }

    #[test]
    fn markers_serialise_with_dotted_plan_change() {
        let marker = HistoryMarker {
            kind: MarkerKind::PlanChange,
            pool: None,
            from: None,
            to: None,
        };
        assert_eq!(
            serde_json::to_string(&marker).unwrap(),
            r#"{"kind":"plan.change"}"#
        );
    }

    #[test]
    fn decode_skips_torn_lines() {
        let record =
            HistoryRecord::from_snapshot(&snapshot(1, None, vec![window("5h", 1, None)])).unwrap();
        let mut bytes = encode_line(&record).unwrap();
        bytes.extend_from_slice(b"{\"schema_version\":1,\"obser");
        bytes.push(b'\n');
        bytes.extend(encode_line(&record).unwrap());
        assert_eq!(decode_lines(&bytes).len(), 2);
    }

    #[test]
    fn price_in_force_uses_the_latest_started_entry() {
        let plans = [
            plan("max_20x", 200.0, "2026-06-01"),
            plan("max_5x", 100.0, "2026-01-01"),
        ];
        let at = |date: &str| {
            let day: jiff::civil::Date = date.parse().unwrap();
            day.at(12, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap()
                .timestamp()
                .as_second()
        };
        assert!(price_in_force(&plans, at("2025-12-31")).is_none());
        assert_eq!(
            price_in_force(&plans, at("2026-01-01")).unwrap().name,
            "max_5x"
        );
        assert_eq!(
            price_in_force(&plans, at("2026-05-31")).unwrap().name,
            "max_5x"
        );
        assert_eq!(
            price_in_force(&plans, at("2026-06-01")).unwrap().name,
            "max_20x"
        );
    }

    #[test]
    fn plan_tables_are_validated() {
        assert!(valid_plans(&[
            plan("a", 0.0, "2026-01-01"),
            plan("b", 20.5, "2026-02-01")
        ]));
        assert!(!valid_plans(&[plan("a", -1.0, "2026-01-01")]));
        assert!(!valid_plans(&[plan("a", f64::NAN, "2026-01-01")]));
        assert!(!valid_plans(&[plan("a", 1.0, "2026-13-01")]));
        assert!(!valid_plans(&[
            plan("a", 1.0, "2026-01-01"),
            plan("b", 2.0, "2026-01-01")
        ]));
        let mut bad = plan("a", 1.0, "2026-01-01");
        bad.currency = "usd".into();
        assert!(!valid_plans(&[bad]));
        assert!(!valid_plans(&[plan("", 1.0, "2026-01-01")]));
        let mut free = plan("student", 0.0, "2026-01-01");
        free.notional_usd = Some(20.0);
        assert!(valid_plans(&[free.clone()]));
        free.notional_usd = Some(-1.0);
        assert!(!valid_plans(&[free]));
        let parsed: PlanPrice = serde_json::from_str(
            r#"{"name":"s","price_usd":0,"notional_usd":20,"effective_from":"2026-01-01"}"#,
        )
        .unwrap();
        assert_eq!(parsed.currency, "USD");
        assert_eq!(parsed.notional_usd, Some(20.0));
    }
}
