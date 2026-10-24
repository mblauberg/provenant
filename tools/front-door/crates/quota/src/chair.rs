//! Cached-quota chair placement. This policy never probes an account.
use crate::{QuotaSnapshot, QuotaStatus};

#[derive(Debug, PartialEq, Eq)]
pub enum SwitchDecision<'a> {
    Stay,
    Switch(&'a str),
    Refuse(&'static str),
}

/// Targets are already registered, in the user's allow-list order.
pub fn decide_switch<'a>(
    current: Option<&QuotaSnapshot>,
    targets: &[(&'a str, Option<&QuotaSnapshot>)],
    threshold_percent: u64,
    now: i64,
    automatic: bool,
) -> SwitchDecision<'a> {
    if !(1..=100).contains(&threshold_percent) {
        return SwitchDecision::Refuse("switch threshold must be between 1 and 100 percent");
    }
    let threshold = threshold_percent * 100;
    if automatic {
        let Some(current) = current.filter(|s| fresh(s, now)) else {
            return SwitchDecision::Refuse("current account quota is missing or stale");
        };
        if !current.windows.iter().any(|w| {
            relevant(&w.pool)
                && w.used_basis_points >= threshold
                && w.resets_unix_seconds.is_some_and(|reset| reset > now)
        }) {
            return SwitchDecision::Stay;
        }
    }
    for (label, snapshot) in targets {
        if snapshot.is_some_and(|s| {
            fresh(s, now)
                && s.status == QuotaStatus::Available
                && ["five_hour", "seven_day"].iter().all(|pool| {
                    s.windows.iter().any(|w| {
                        w.pool == *pool
                            && w.used_basis_points < threshold
                            && w.resets_unix_seconds.is_some_and(|reset| reset > now)
                    })
                })
        }) {
            return SwitchDecision::Switch(label);
        }
    }
    SwitchDecision::Refuse(
        "no allowed registered Claude account has fresh quota below the threshold on both windows",
    )
}

fn relevant(pool: &str) -> bool {
    matches!(pool, "five_hour" | "seven_day")
}
fn fresh(snapshot: &QuotaSnapshot, now: i64) -> bool {
    snapshot.validate().is_ok()
        && snapshot.account.adapter == "claude"
        && matches!(
            snapshot.status,
            QuotaStatus::Available | QuotaStatus::Exhausted
        )
        && (0..=900).contains(&now.saturating_sub(snapshot.observed_unix_seconds))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{AccountScope, QuotaWindow};
    fn snapshot(five: u64, week: u64) -> QuotaSnapshot {
        QuotaSnapshot {
            schema_version: 1,
            account: AccountScope {
                adapter: "claude".into(),
                account_key: "account".into(),
                label: "account".into(),
            },
            revision: 1,
            status: QuotaStatus::Available,
            observed_unix_seconds: 1000,
            source_ref: "fixture".into(),
            authority_ref: None,
            details: None,
            windows: [("five_hour", 18000, five), ("seven_day", 604800, week)]
                .into_iter()
                .map(|(pool, seconds, used)| QuotaWindow {
                    pool: pool.into(),
                    window_seconds: Some(seconds),
                    used_basis_points: used,
                    resets_unix_seconds: Some(2000),
                })
                .collect(),
        }
    }
    #[test]
    fn chair_switch_threshold_is_inclusive_on_either_window_and_preserves_order() {
        let low = snapshot(9199, 9199);
        let hot = snapshot(9200, 200);
        let weekly = snapshot(200, 9200);
        let targets = [("first", Some(&low)), ("second", Some(&low))];
        assert_eq!(
            decide_switch(Some(&low), &targets, 92, 1001, true),
            SwitchDecision::Stay
        );
        for current in [&hot, &weekly] {
            assert_eq!(
                decide_switch(Some(current), &targets, 92, 1001, true),
                SwitchDecision::Switch("first")
            );
        }
        assert_eq!(
            decide_switch(Some(&low), &targets, 92, 1001, false),
            SwitchDecision::Switch("first")
        );
    }
    #[test]
    fn chair_switch_skips_full_accounts_and_honours_custom_threshold_and_cache_expiry() {
        let current = snapshot(9100, 100);
        let full = snapshot(9200, 100);
        let low = snapshot(100, 100);
        let targets = [
            ("full", Some(&full)),
            ("second", Some(&low)),
            ("third", Some(&low)),
        ];
        assert_eq!(
            decide_switch(Some(&current), &targets, 91, 1001, true),
            SwitchDecision::Switch("second")
        );
        let mut future = low.clone();
        future.observed_unix_seconds = 1002;
        let mut expired = low.clone();
        expired.windows[0].resets_unix_seconds = Some(1001);
        assert!(matches!(
            decide_switch(
                Some(&full),
                &[("future", Some(&future)), ("expired", Some(&expired))],
                92,
                1001,
                true
            ),
            SwitchDecision::Refuse(_)
        ));
    }
    #[test]
    fn chair_switch_refuses_unknown_stale_partial_and_full_targets() {
        let hot = snapshot(9200, 100);
        let mut stale = snapshot(100, 100);
        stale.observed_unix_seconds = 0;
        let mut partial = snapshot(100, 100);
        partial.windows.pop();
        let targets = [
            ("unknown", None),
            ("stale", Some(&stale)),
            ("partial", Some(&partial)),
            ("full", Some(&hot)),
        ];
        assert!(matches!(
            decide_switch(Some(&hot), &targets, 92, 1001, true),
            SwitchDecision::Refuse(_)
        ));
        assert!(matches!(
            decide_switch(Some(&hot), &[], 92, 1001, true),
            SwitchDecision::Refuse(_)
        ));
        assert!(matches!(
            decide_switch(None, &targets, 92, 1001, true),
            SwitchDecision::Refuse(_)
        ));
    }
}
