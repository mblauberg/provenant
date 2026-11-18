//! Account-local monthly allowance estimates. Raw provider snapshots stay raw.
use crate::{QuotaError, QuotaSnapshot, QuotaWindow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LAUNCH_JOURNAL: usize = 4096;

/// Recognise billing cycles independently of adapter names.
pub fn monthly_window(window: &QuotaWindow) -> bool {
    let pool = window.pool.to_ascii_lowercase();
    let name = pool.split(':').next().unwrap_or(&pool);
    ["monthly", "thirty_day", "billing_cycle"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
        || window
            .window_seconds
            .is_some_and(|seconds| (28 * 86400..=31 * 86400).contains(&seconds))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AllowanceLedger {
    pub launches: u64,
    /// Cost used to reserve launches until provider samples can calibrate it.
    pub reservation_cost_basis_points: Option<u64>,
    pub windows: BTreeMap<String, AllowanceWindow>,
    // Bounded replay protection for launch preparation of an existing attempt.
    pub recent_launches: Vec<AllowanceLaunch>,
    pub evicted_through: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowanceLaunch {
    pub id: String,
    pub unix_seconds: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowanceWindow {
    pub reset: i64,
    pub duration: u64,
    pub observed: i64,
    pub reported_basis_points: u64,
    pub anchor_launches: u64,
    #[serde(default)]
    pub confirmation_credit_basis_points: u64,
    pub cost_basis_points: Option<u64>,
}

impl AllowanceWindow {
    pub fn estimated(&self, launches: u64, default_cost: u64) -> u64 {
        self.reported_basis_points.saturating_add(
            launches
                .saturating_sub(self.anchor_launches)
                .saturating_mul(self.cost_basis_points.unwrap_or(default_cost)),
        )
    }
}

impl AllowanceLedger {
    pub fn validate(&self) -> Result<(), QuotaError> {
        if self
            .reservation_cost_basis_points
            .is_some_and(|cost| cost == 0 || cost > 10_000)
            || self.windows.len() > crate::MAX_WINDOWS
            || self.recent_launches.len() > MAX_LAUNCH_JOURNAL
            || self.recent_launches.len() as u64 > self.launches
            || self
                .recent_launches
                .iter()
                .map(|launch| &launch.id)
                .collect::<BTreeSet<_>>()
                .len()
                != self.recent_launches.len()
            || self
                .evicted_through
                .is_some_and(|at| jiff::Timestamp::from_second(at).is_err())
            || self.recent_launches.iter().any(|launch| {
                !crate::component(&launch.id)
                    || jiff::Timestamp::from_second(launch.unix_seconds).is_err()
            })
            || self.windows.iter().any(|(pool, w)| {
                !crate::state::reference(pool)
                    || w.duration == 0
                    || w.duration > i64::MAX as u64
                    || w.anchor_launches > self.launches
                    || w.cost_basis_points
                        .is_some_and(|cost| cost == 0 || cost > 10_000)
                    || w.reported_basis_points > 10_000
                    || w.observed >= w.reset
                    || jiff::Timestamp::from_second(w.reset).is_err()
                    || jiff::Timestamp::from_second(w.observed).is_err()
            })
        {
            return Err(QuotaError::Invalid("monthly allowance ledger"));
        }
        Ok(())
    }

    pub fn record_launch(&mut self, id: &str, unix_seconds: i64) -> Result<(), QuotaError> {
        if !crate::component(id) || jiff::Timestamp::from_second(unix_seconds).is_err() {
            return Err(QuotaError::Invalid("allowance launch id"));
        }
        if self
            .recent_launches
            .iter()
            .any(|previous| previous.id == id)
        {
            return Ok(());
        }
        self.launches = self
            .launches
            .checked_add(1)
            .ok_or(QuotaError::NumberOutOfRange)?;
        if self.recent_launches.len() == MAX_LAUNCH_JOURNAL {
            let removed = self.recent_launches.remove(0);
            self.evicted_through = Some(
                self.evicted_through
                    .map_or(removed.unix_seconds, |at| at.max(removed.unix_seconds)),
            );
        }
        self.recent_launches.push(AllowanceLaunch {
            id: id.to_owned(),
            unix_seconds,
        });
        Ok(())
    }

    /// A probe may finish after newer launches. Those launches must remain debt.
    fn launches_before(&self, observed: i64) -> Option<u64> {
        if self.evicted_through.is_some_and(|at| at >= observed) {
            return None;
        }
        let pending = self
            .recent_launches
            .iter()
            .filter(|launch| launch.unix_seconds >= observed)
            .count() as u64;
        self.launches.checked_sub(pending)
    }

    pub fn observe(&mut self, snapshot: &QuotaSnapshot) {
        if !matches!(
            snapshot.status,
            crate::QuotaStatus::Available
                | crate::QuotaStatus::CreditOnly
                | crate::QuotaStatus::Exhausted
        ) {
            return;
        }
        let Some(observed_launches) = self.launches_before(snapshot.observed_unix_seconds) else {
            // Insufficient journal coverage cannot clear or recalibrate known debt.
            return;
        };
        // Retain only the latest bounded set; a missing report does not reset debt.
        for window in snapshot.windows.iter().filter(|w| monthly_window(w)) {
            let (Some(reset), Some(duration)) = (window.resets_unix_seconds, window.window_seconds)
            else {
                continue;
            };
            if duration == 0
                || duration > i64::MAX as u64
                || snapshot.observed_unix_seconds >= reset
            {
                continue;
            }
            let previous = self.windows.get(&window.pool);
            if previous.is_some_and(|w| w.observed >= snapshot.observed_unix_seconds) {
                continue;
            }
            let same_cycle = previous.filter(|w| w.reset == reset);
            if let Some(previous) = same_cycle {
                // Flat or decreasing delayed readings never forgive pending launches.
                if window.used_basis_points <= previous.reported_basis_points {
                    continue;
                }
            }
            // A provider increase may cover only some of the outstanding launches.
            // Never infer a lower per-launch cost from an ambiguous partial report.
            let (anchor_launches, cost, credit) = if let Some(previous) = same_cycle {
                let pending = observed_launches.saturating_sub(previous.anchor_launches);
                let delta = window
                    .used_basis_points
                    .saturating_sub(previous.reported_basis_points);
                let floor = previous
                    .cost_basis_points
                    .unwrap_or(0)
                    .max(self.reservation_cost_basis_points.unwrap_or(100));
                let credit = previous
                    .confirmation_credit_basis_points
                    .saturating_add(delta);
                let confirmed = (credit / floor).min(pending);
                let remaining_credit = if confirmed < pending {
                    credit % floor
                } else {
                    0
                };
                let cost = Some(if pending == 1 && confirmed == 1 {
                    floor.max(delta).min(10_000)
                } else {
                    floor
                });
                (
                    previous.anchor_launches.saturating_add(confirmed),
                    cost,
                    remaining_credit,
                )
            } else {
                (
                    observed_launches,
                    previous.and_then(|w| w.cost_basis_points),
                    0,
                )
            };
            self.windows.insert(
                window.pool.clone(),
                AllowanceWindow {
                    reset,
                    duration,
                    observed: snapshot.observed_unix_seconds,
                    reported_basis_points: window.used_basis_points.min(10_000),
                    anchor_launches,
                    confirmation_credit_basis_points: credit,
                    cost_basis_points: cost,
                },
            );
        }
        if self.windows.len() > crate::MAX_WINDOWS {
            let mut rows: Vec<_> = self
                .windows
                .iter()
                .map(|(key, w)| (w.reset, key.clone()))
                .collect();
            rows.sort();
            for (_, key) in rows
                .into_iter()
                .take(self.windows.len() - crate::MAX_WINDOWS)
            {
                self.windows.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(at: i64, used: u64, reset: i64) -> QuotaSnapshot {
        QuotaSnapshot::test_snapshot(
            crate::AccountScope {
                extra: Default::default(),
                adapter: "cursor".into(),
                account_key: "a".into(),
                label: "A".into(),
            },
            crate::QuotaStatus::Available,
            at,
            vec![QuotaWindow {
                extra: Default::default(),
                pool: "billing_cycle".into(),
                window_seconds: Some(30 * 86400),
                resets_unix_seconds: Some(reset),
                used_basis_points: used,
            }],
        )
    }
    // Spec: docs/specs/routing.md#monthly-allowance-ledger
    #[test]
    fn monthly_ledger_retains_delayed_debt_calibrates_and_rolls_over() {
        let mut ledger = AllowanceLedger::default();
        ledger.observe(&snapshot(10, 1000, 30 * 86400));
        for id in ["launch-a", "launch-b"] {
            ledger.record_launch(id, 15).unwrap();
        }
        ledger.record_launch("launch-a", 16).unwrap();
        assert_eq!(ledger.launches, 2); // Replaying preparation must not spend twice.
        assert_eq!(
            ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
            1200
        );
        for used in [1000, 999] {
            ledger.observe(&snapshot(20, used, 30 * 86400));
            assert_eq!(
                ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
                1200
            );
        }
        ledger.observe(&snapshot(30, 1400, 30 * 86400));
        assert_eq!(ledger.windows["billing_cycle"].cost_basis_points, Some(100));
        assert_eq!(
            ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
            1400
        );
        ledger.record_launch("launch-c", 35).unwrap();
        ledger.observe(&snapshot(40, 1800, 30 * 86400));
        assert_eq!(ledger.windows["billing_cycle"].cost_basis_points, Some(400));
        ledger.record_launch("launch-d", 45).unwrap();
        ledger.observe(&snapshot(30 * 86400 + 10, 0, 60 * 86400));
        assert_eq!(
            ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
            0
        );
        assert_eq!(ledger.windows["billing_cycle"].cost_basis_points, Some(400));
        // Completion can race a launch after the provider's observation instant.
        let mut slow_probe = AllowanceLedger::default();
        slow_probe.observe(&snapshot(10, 0, 30 * 86400));
        slow_probe.record_launch("before-probe", 15).unwrap();
        slow_probe.record_launch("during-probe", 25).unwrap();
        slow_probe.observe(&snapshot(20, 200, 30 * 86400));
        assert_eq!(
            slow_probe.windows["billing_cycle"].cost_basis_points,
            Some(200)
        );
        assert_eq!(
            slow_probe.windows["billing_cycle"].estimated(slow_probe.launches, 100),
            400
        );
        ledger.validate().unwrap();
        let persisted = serde_json::to_vec(&ledger).unwrap();
        assert_eq!(
            serde_json::from_slice::<AllowanceLedger>(&persisted).unwrap(),
            ledger
        );
    }
    // Spec: docs/specs/routing.md#monthly-allowance-ledger
    #[test]
    fn partial_reports_keep_unconfirmed_launch_debt() {
        let mut ledger = AllowanceLedger::default();
        ledger.observe(&snapshot(10, 0, 30 * 86400));
        for n in 0..10 {
            ledger.record_launch(&format!("launch-{n}"), 15).unwrap();
        }
        ledger.observe(&snapshot(20, 100, 30 * 86400));
        assert_eq!(
            ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
            1000
        );
        assert!(
            ledger.windows["billing_cycle"]
                .cost_basis_points
                .unwrap_or(100)
                >= 100
        );
        for used in [100, 99] {
            ledger.observe(&snapshot(21, used, 30 * 86400));
            assert_eq!(
                ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
                1000
            );
        }
        ledger.observe(&snapshot(22, 1000, 30 * 86400));
        assert_eq!(
            ledger.windows["billing_cycle"].estimated(ledger.launches, 100),
            1000
        );
        assert_eq!(ledger.windows["billing_cycle"].anchor_launches, 10);
        let mut increments = AllowanceLedger::default();
        increments.observe(&snapshot(10, 0, 30 * 86400));
        increments.record_launch("increment", 15).unwrap();
        increments.observe(&snapshot(20, 50, 30 * 86400));
        assert_eq!(increments.windows["billing_cycle"].anchor_launches, 0);
        increments.observe(&snapshot(21, 100, 30 * 86400));
        assert_eq!(
            increments.windows["billing_cycle"].estimated(increments.launches, 100),
            100
        );
        let mut configured = AllowanceLedger {
            reservation_cost_basis_points: Some(250),
            ..Default::default()
        };
        configured.observe(&snapshot(10, 0, 30 * 86400));
        for n in 0..4 {
            configured
                .record_launch(&format!("configured-{n}"), 15)
                .unwrap();
        }
        configured.observe(&snapshot(20, 250, 30 * 86400));
        assert_eq!(
            configured.windows["billing_cycle"].estimated(configured.launches, 250),
            1000
        );
    }
}
