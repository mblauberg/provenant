//! Expiring dashboard floors are applied to copies of raw ledger observations.
//! Never persist the adjusted windows: expiry must reveal the original ledger.
use crate::{AccountRecord, QuotaStatus, QuotaWindow};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    pub five_hour: Option<u64>,
    pub seven_day: Option<u64>,
    pub thirty_day: Option<u64>,
    /// RFC 3339 UTC timestamp. A floor stops applying at this instant.
    pub expires: String,
}

impl Calibration {
    fn expiry(&self) -> Option<i64> {
        self.expires
            .parse::<jiff::Timestamp>()
            .ok()
            .map(|t| t.as_second())
    }

    pub(crate) fn valid(&self) -> bool {
        let values = [self.five_hour, self.seven_day, self.thirty_day];
        self.expiry().is_some()
            && values.iter().any(Option::is_some)
            && values.into_iter().flatten().all(|percent| percent <= 100)
    }
}

impl crate::Settings {
    /// Calibrations are keyed by the OpenCode account key, usually `go`.
    /// Call on raw records at every read, including routing, so cached snapshots
    /// cannot retain a floor beyond its expiry or a configuration change.
    pub fn apply_calibration(&self, records: &mut [AccountRecord], now: i64) {
        for record in records {
            if record.account.adapter != "opencode" || record.account.account_key == "zen" {
                continue;
            }
            let Some(snapshot) = &mut record.snapshot else {
                continue;
            };
            let details = snapshot.details.get_or_insert_default();
            details.source = Some("local-ledger".into());
            details.coverage = Some("partial".into());
            details.calibrated_windows.clear();
            details.calibration_expires_unix_seconds = None;
            let Some(calibration) = self.calibration.get(&record.account.account_key) else {
                continue;
            };
            let Some(expiry) = calibration.expiry().filter(|expiry| now < *expiry) else {
                continue;
            };
            if !calibration.valid() {
                continue;
            }
            details.calibration_expires_unix_seconds = Some(expiry);
            for (pool, percent, seconds) in [
                ("five_hour", calibration.five_hour, 18_000),
                ("seven_day", calibration.seven_day, 604_800),
                ("thirty_day", calibration.thirty_day, 2_592_000),
            ] {
                let Some(percent) = percent else { continue };
                details.calibrated_windows.push(pool.into());
                if let Some(window) = snapshot.windows.iter_mut().find(|w| w.pool == pool) {
                    window.used_basis_points = window.used_basis_points.max(percent * 100);
                } else {
                    snapshot.windows.push(QuotaWindow {
                        pool: pool.into(),
                        used_basis_points: percent * 100,
                        window_seconds: Some(seconds),
                        resets_unix_seconds: None,
                    });
                }
            }
            if matches!(
                snapshot.status,
                QuotaStatus::Available | QuotaStatus::Unknown
            ) && snapshot
                .windows
                .iter()
                .any(|w| w.used_basis_points >= 10_000)
            {
                snapshot.status = QuotaStatus::Exhausted;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_config::DomainConfig;

    fn settings(expires: &str) -> crate::Settings {
        let mut settings = crate::Settings::default();
        settings.calibration.insert(
            "go".into(),
            Calibration {
                five_hour: Some(43),
                seven_day: Some(17),
                thirty_day: Some(59),
                expires: expires.into(),
            },
        );
        settings
    }

    fn ledger(account: &str) -> AccountRecord {
        let scope = crate::AccountScope {
            adapter: "opencode".into(),
            account_key: account.into(),
            label: account.into(),
        };
        let mut record = AccountRecord::empty(scope.clone());
        record.snapshot = Some(crate::QuotaSnapshot {
            schema_version: 1,
            account: scope,
            revision: 0,
            status: QuotaStatus::Available,
            observed_unix_seconds: 1,
            source_ref: "quota.provider.opencode".into(),
            authority_ref: None,
            windows: [("five_hour", 401), ("seven_day", 160), ("thirty_day", 7000)]
                .into_iter()
                .map(|(pool, used_basis_points)| QuotaWindow {
                    pool: pool.into(),
                    used_basis_points,
                    window_seconds: None,
                    resets_unix_seconds: None,
                })
                .collect(),
            details: None,
        });
        record
    }

    #[test]
    fn calibration_takes_maximum_and_expires_even_with_cached_ledger() {
        let settings = settings("2026-11-04T00:00:00Z");
        let expiry = settings.calibration["go"].expiry().unwrap();
        let raw = vec![ledger("go"), ledger("zen")];
        let mut active = raw.clone();
        settings.apply_calibration(&mut active, expiry - 1);
        let snapshot = active[0].snapshot.as_ref().unwrap();
        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|w| w.used_basis_points)
                .collect::<Vec<_>>(),
            [4300, 1700, 7000]
        );
        assert_eq!(
            snapshot.details.as_ref().unwrap().calibrated_windows.len(),
            3
        );
        assert_eq!(active[1], raw[1]);
        for now in [expiry, expiry + 1] {
            let mut expired = raw.clone();
            settings.apply_calibration(&mut expired, now);
            let snapshot = expired[0].snapshot.as_ref().unwrap();
            assert_eq!(snapshot.windows, raw[0].snapshot.as_ref().unwrap().windows);
            assert_eq!(
                snapshot
                    .details
                    .as_ref()
                    .unwrap()
                    .calibration_expires_unix_seconds,
                None
            );
            assert_eq!(
                snapshot.details.as_ref().unwrap().coverage.as_deref(),
                Some("partial")
            );
        }
    }

    #[test]
    fn calibration_is_validated_and_cannot_hide_exhaustion() {
        assert!(settings("2026-11-04T00:00:00Z").validate().is_empty());
        assert!(!settings("tomorrow").validate().is_empty());
        let mut settings = settings("2026-11-04T00:00:00Z");
        settings.calibration.get_mut("go").unwrap().five_hour = Some(101);
        assert!(!settings.validate().is_empty());
        settings.calibration.get_mut("go").unwrap().five_hour = Some(100);
        let mut records = vec![ledger("go")];
        settings.apply_calibration(&mut records, 1);
        assert_eq!(
            records[0].snapshot.as_ref().unwrap().status,
            QuotaStatus::Exhausted
        );
    }

    #[test]
    fn calibration_config_accepts_partial_floors_and_requires_expiry() {
        let base = "max_parallel = 3\nwall_budget_seconds = 55\n[calibration.go]\n";
        let parsed: crate::Settings = toml_edit::de::from_str(&format!(
            "{base}thirty_day = 59\nexpires = \"2026-11-04T00:00:00Z\"\n"
        ))
        .unwrap();
        assert!(parsed.validate().is_empty());
        assert_eq!(parsed.calibration["go"].five_hour, None);
        assert!(
            toml_edit::de::from_str::<crate::Settings>(&format!("{base}five_hour = 43\n")).is_err()
        );
    }
}
