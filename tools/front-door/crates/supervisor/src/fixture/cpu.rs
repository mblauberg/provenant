//! Manual native CPU capture shared by custody and composition fixtures.
// Real time and a native sampling process are the measurement boundary.
#![allow(clippy::disallowed_methods)]
use super::RecordedBootObserver;
use crate::census::{ProcessIdentity, ProcessObservation, ProcessObserver};
use std::{
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Sample only this guardian, then measure native CPU over at least a minute.
/// Run manually on macOS, outside nextest's short timeout.
pub fn capture_guardian_cpu(attempt: &Path) -> Result<serde_json::Value> {
    let observed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(attempt.join("process.json"))?)?;
    let identity: ProcessIdentity = serde_json::from_value(observed["guardian"].clone())?;
    let observer = RecordedBootObserver;
    let cpu = || -> Result<u64> {
        match observer.observe(identity.pid) {
            ProcessObservation::Present(fact) if fact.identity == identity => fact
                .cpu_micros
                .ok_or("guardian CPU accounting unavailable".into()),
            other => Err(format!("guardian identity unavailable: {other:?}").into()),
        }
    };
    let sample_path = std::env::temp_dir().join(format!("supervisor-{}.sample.txt", identity.pid));
    let sample = std::process::Command::new("/usr/bin/sample")
        .args([
            identity.pid.to_string(),
            "10".into(),
            "-file".into(),
            sample_path.display().to_string(),
        ])
        .output()?;
    if !sample.status.success() {
        return Err(format!("sample failed: {}", String::from_utf8_lossy(&sample.stderr)).into());
    }
    let before_census = observer.census(65_536)?;
    let processes_before = before_census.facts().count() + before_census.unknown_pids().len();
    let cpu_before = cpu()?;
    let started = Instant::now();
    let duration = Duration::from_secs(60);
    while started.elapsed() < duration {
        std::thread::park_timeout(duration.saturating_sub(started.elapsed()));
    }
    let cpu_after = cpu()?;
    let elapsed = started.elapsed();
    let after_census = observer.census(65_536)?;
    let processes_after = after_census.facts().count() + after_census.unknown_pids().len();
    let cpu_used = cpu_after
        .checked_sub(cpu_before)
        .ok_or("CPU counter regressed")?;
    let percent = cpu_used as f64 / elapsed.as_secs_f64() / 10_000.0;
    Ok(serde_json::json!({
        "build": env!("CARGO_PKG_VERSION"),
        "debug_assertions": cfg!(debug_assertions),
        "pid": identity.pid,
        "method": "10-second sample, then native PROC_PIDTASKINFO CPU delta over at least 60 seconds",
        "processes_before": processes_before,
        "processes_after": processes_after,
        "elapsed_seconds": elapsed.as_secs_f64(),
        "cpu_before_micros": cpu_before,
        "cpu_after_micros": cpu_after,
        "percent_of_one_core": percent,
        "under_three_percent": percent < 3.0,
        "sample": sample_path,
    }))
}
