//! Pure aggregation of build outcome rows for the soak report.
use super::{Exception, ExecPlace, JobOutcome, Origin, Priority, Reset};
use provenant_config::BuildPort;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    NotYet,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Wait {
    pub count: u64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub rows: u64,
    pub lane_rows: u64,
    pub span_ms: u64,
    pub waits: BTreeMap<Priority, Wait>,
    pub peak_concurrent: u64,
    pub peak_rss_bytes: Option<u64>,
    pub rss_fraction: Option<f64>,
    pub exceptions: BTreeMap<String, u64>,
    pub exception_fractions: BTreeMap<String, f64>,
    pub exception_fraction: Option<f64>,
    pub churn_observed: u64,
    pub churn_clean: u64,
    pub churn_unknown: u64,
    pub unexpected_packages: BTreeSet<String>,
    pub freshness_failures: u64,
    pub recovery_failures: u64,
    pub criteria: BTreeMap<String, Verdict>,
    pub warnings: Vec<String>,
}

/// Return an empty report before any outcomes have been observed.
pub fn empty() -> Report {
    Report::default()
}

fn percentile(values: &mut [u64], percentile: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[(values.len() * percentile).div_ceil(100) - 1])
}

/// Aggregate rows. `expected` returns None when the expected set cannot be
/// determined; such rows are unknown and cannot establish a clean soak.
/// A full selection must be returned as all rebuilt package names.
pub fn aggregate<F>(
    rows: &[JobOutcome],
    port: &BuildPort,
    limit: u64,
    physical_ram: Option<u64>,
    expected: F,
) -> Report
where
    F: Fn(&str, &str) -> Option<BTreeSet<String>>,
{
    let mut report = Report {
        rows: rows.len() as u64,
        ..Report::default()
    };
    let lane: Vec<_> = rows.iter().filter(|r| r.origin == Origin::Lane).collect();
    report.lane_rows = lane.len() as u64;
    if let (Some(first), Some(last)) = (
        lane.iter().map(|r| r.started_unix_ms).min(),
        lane.iter().map(|r| r.finished_unix_ms).max(),
    ) {
        report.span_ms = last.saturating_sub(first);
    }
    for priority in [
        Priority::Interactive,
        Priority::Test,
        Priority::Train,
        Priority::Prewarm,
    ] {
        let mut samples: Vec<_> = rows
            .iter()
            .filter(|r| r.priority == priority)
            .map(|r| r.waited_ms)
            .collect();
        let count = samples.len() as u64;
        let p50_ms = percentile(&mut samples, 50);
        let p95_ms = percentile(&mut samples, 95);
        report.waits.insert(
            priority,
            Wait {
                count,
                p50_ms,
                p95_ms,
            },
        );
    }
    // Closed intervals: a start at the same instant as an end overlaps it.
    let mut events = Vec::new();
    for row in rows
        .iter()
        .filter(|r| r.exec == ExecPlace::Slot && r.finished_unix_ms >= r.started_unix_ms)
    {
        events.push((row.started_unix_ms, 0u8));
        events.push((row.finished_unix_ms, 1u8));
    }
    events.sort_unstable();
    let mut active = 0u64;
    for (_, end) in events {
        if end == 0 {
            active += 1;
            report.peak_concurrent = report.peak_concurrent.max(active);
        } else {
            active = active.saturating_sub(1);
        }
    }
    report.peak_rss_bytes = rows.iter().filter_map(|r| r.peak_rss_bytes).max();
    report.rss_fraction = report
        .peak_rss_bytes
        .zip(physical_ram.filter(|v| *v > 0))
        .map(|(rss, ram)| rss as f64 / ram as f64);
    for row in rows {
        if let Some(reason) = row.exception {
            let key = serde_json::to_value(reason)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            *report.exceptions.entry(key).or_default() += 1;
        }
        if row.warnings.iter().any(|w| w.contains("freshness_failure")) {
            report.freshness_failures += 1;
        }
        if row.reset == Reset::Quarantine
            || row.exception == Some(Exception::AllQuarantined)
            || (row.infra.is_some() && !row.cancelled)
        {
            report.recovery_failures += 1;
        }
        if row.exec == ExecPlace::Slot
            && let (Some(prev), Some(snap)) = (&row.prev_snapshot, &row.snapshot)
        {
            match expected(prev, snap) {
                Some(expected) => {
                    report.churn_observed += 1;
                    let unexpected: BTreeSet<_> = row
                        .rebuilt_packages
                        .iter()
                        .filter(|p| !expected.contains(*p))
                        .cloned()
                        .collect();
                    if unexpected.is_empty() {
                        report.churn_clean += 1;
                    }
                    report.unexpected_packages.extend(unexpected);
                }
                None => report.churn_unknown += 1,
            }
        }
    }
    if !rows.is_empty() {
        report.exception_fraction =
            Some(report.exceptions.values().sum::<u64>() as f64 / rows.len() as f64);
        report.exception_fractions = report
            .exceptions
            .iter()
            .map(|(reason, count)| (reason.clone(), *count as f64 / rows.len() as f64))
            .collect();
    }
    let ready = report.lane_rows >= port.soak.min_runs
        && report.span_ms >= port.soak.min_days.saturating_mul(86_400_000);
    let verdict = |value: Option<bool>| {
        if !ready || value.is_none() {
            Verdict::NotYet
        } else if value == Some(true) {
            Verdict::Pass
        } else {
            Verdict::Fail
        }
    };
    let mut criterion = |name: &str, value: Option<bool>| {
        report.criteria.insert(name.to_owned(), verdict(value));
    };
    criterion(
        "interactive_wait_p95",
        report.waits[&Priority::Interactive]
            .p95_ms
            .map(|v| v <= port.soak_interactive_wait_p95_duration().value.as_millis() as u64),
    );
    criterion(
        "test_wait_p95",
        report.waits[&Priority::Test]
            .p95_ms
            .map(|v| v <= port.soak_test_wait_p95_duration().value.as_millis() as u64),
    );
    criterion("concurrency", Some(report.peak_concurrent <= limit));
    criterion(
        "rss",
        report.rss_fraction.map(|v| v <= port.soak.rss_fraction),
    );
    criterion(
        "local_exceptions",
        report
            .exception_fraction
            .map(|v| v <= port.soak.local_exception_fraction),
    );
    criterion(
        "churn",
        if report.churn_unknown > 0 || report.churn_observed == 0 {
            None
        } else {
            Some(
                report.churn_clean as f64 / report.churn_observed as f64
                    >= port.soak.churn_clean_fraction,
            )
        },
    );
    criterion("freshness", Some(report.freshness_failures == 0));
    criterion("recovery", Some(report.recovery_failures == 0));
    report
        .warnings
        .push("Jobs that neither ran nor reported are not observable from the ledger.".into());
    report
}

#[cfg(test)]
pub(super) fn test_row() -> JobOutcome {
    use super::{JobKind, Stack, StrategyName};
    JobOutcome {
        schema: 1,
        job_id: "job".into(),
        project_key: "project".into(),
        owner: "owner".into(),
        origin: Origin::Lane,
        kind: JobKind::Intercepted {
            stack: Stack::Rust,
            subcommand: "check".into(),
        },
        priority: Priority::Interactive,
        strategy: StrategyName::Slots,
        exec: ExecPlace::Slot,
        exception: None,
        slot: Some(0),
        snapshot: Some("b".into()),
        prev_snapshot: Some("a".into()),
        base: None,
        enqueued_unix_ms: 0,
        started_unix_ms: 0,
        finished_unix_ms: 1,
        waited_ms: 0,
        wall_ms: 1,
        exit: None,
        timed_out: false,
        cancelled: false,
        reset: Reset::None,
        infra: None,
        units_fresh: None,
        units_rebuilt: None,
        rebuilt_packages: vec![],
        peak_rss_bytes: None,
        slot_changes: None,
        warnings: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_rank_overlap_and_insufficient_soak() {
        let mut rows = Vec::new();
        for i in 1..=20 {
            let mut r = test_row();
            r.waited_ms = i;
            r.started_unix_ms = i;
            r.finished_unix_ms = i + 1;
            rows.push(r);
        }
        let report = aggregate(
            &rows,
            &provenant_config::PortTable::default().build,
            2,
            Some(100),
            |_, _| Some(BTreeSet::new()),
        );
        assert_eq!(report.waits[&Priority::Interactive].p50_ms, Some(10));
        assert_eq!(report.waits[&Priority::Interactive].p95_ms, Some(19));
        assert_eq!(report.peak_concurrent, 2);
        assert!(report.criteria.values().all(|v| *v == Verdict::NotYet));
    }
    #[test]
    fn expected_and_always_rebuild_packages_are_not_churn() {
        let mut row = test_row();
        row.rebuilt_packages = vec!["changed".into(), "root".into(), "surprise".into()];
        let report = aggregate(
            &[row],
            &provenant_config::PortTable::default().build,
            1,
            None,
            |_, _| Some(BTreeSet::from(["changed".into(), "root".into()])),
        );
        assert_eq!(
            report.unexpected_packages,
            BTreeSet::from(["surprise".into()])
        );
        assert_eq!(report.churn_clean, 0);
    }
    #[test]
    fn all_quarantined_is_a_recovery_failure() {
        let mut row = test_row();
        row.exception = Some(Exception::AllQuarantined);
        let report = aggregate(
            &[row],
            &provenant_config::PortTable::default().build,
            1,
            None,
            |_, _| Some(BTreeSet::new()),
        );
        assert_eq!(report.recovery_failures, 1);
    }
}
