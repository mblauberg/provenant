//! Append-only build job outcomes with a host ledger and lane fallback.

use super::{JobOutcome, paths};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::fs::{Directory, LockMode, ledger::Ledger};
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const LEDGER_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const ROTATION_RETENTION_MS: u64 = 30 * 86_400_000;

/// What one ledger rotation pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerRotation {
    pub rotated: bool,
    pub expired: u64,
}

/// Append one row under the host ledger lock. If host state cannot be written,
/// append to the attempt scratch file and report the failure if that also fails.
pub fn append(row: &JobOutcome, state: &Path, attempt_scratch: Option<&Path>) {
    let mut bytes = match serde_json::to_vec(row) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn(&format!("cannot encode build job outcome: {error}"));
            return;
        }
    };
    bytes.push(b'\n');

    match append_host(&bytes, state) {
        Ok(()) => {}
        Err(host_error) => {
            if let Some(scratch) = attempt_scratch {
                let fallback = scratch.join("build-jobs.jsonl");
                match append_path(&fallback, &bytes) {
                    Ok(()) => warn(&format!(
                        "build outcome stored in attempt scratch after host ledger failure: {host_error}"
                    )),
                    Err(fallback_error) => warn(&format!(
                        "cannot append build outcome to host ledger ({host_error}) or attempt scratch ({fallback_error})"
                    )),
                }
            } else {
                warn(&format!(
                    "cannot append build outcome to host ledger: {host_error}"
                ));
            }
        }
    }
}

fn append_host(bytes: &[u8], state: &Path) -> std::io::Result<()> {
    let root = paths::build_root(state);
    std::fs::create_dir_all(&root)?;
    let directory = Directory::open_anchor(&root).map_err(std::io::Error::other)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let lock = directory
        .lock(
            OsStr::new("jobs.lock"),
            LockMode::Exclusive,
            WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(60_000_000)),
            &cancel,
            &clock,
        )
        .map_err(std::io::Error::other)?;
    Ledger::new(&directory, OsStr::new("jobs.jsonl"))
        .append(bytes)
        .map_err(std::io::Error::other)?;
    drop(lock);
    Ok(())
}

fn append_path(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let written = file.write(bytes)?;
    if written != bytes.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short build outcome append",
        ));
    }
    file.flush()
}

/// Rotate `jobs.jsonl` above 64 MiB and drop rotations older than 30 days.
/// Both changes happen under the ledger lock so appends never see a partial file.
pub fn rotate(state: &Path, now: SystemTime) -> Result<LedgerRotation, String> {
    let root = paths::build_root(state);
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let directory = Directory::open_anchor(&root).map_err(|error| error.to_string())?;
    let clock = provenant_os::clock::SystemClock::new();
    let lock = directory
        .lock(
            OsStr::new("jobs.lock"),
            LockMode::Exclusive,
            WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(60_000_000)),
            &CancelToken::new(),
            &clock,
        )
        .map_err(|error| error.to_string())?;
    let mut result = LedgerRotation::default();
    let now_ms = unix_millis(now);
    let canonical = root.join("jobs.jsonl");
    if std::fs::metadata(&canonical)
        .map(|meta| meta.len() > LEDGER_LIMIT_BYTES)
        .unwrap_or(false)
    {
        let stamp = date_stamp(now_ms);
        let mut target = root.join(format!("jobs-{stamp}.jsonl"));
        if target.exists() {
            target = root.join(format!("jobs-{stamp}-{now_ms}.jsonl"));
        }
        std::fs::rename(&canonical, &target).map_err(|error| error.to_string())?;
        result.rotated = true;
    }
    for entry in std::fs::read_dir(&root)
        .map_err(|error| error.to_string())?
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("jobs-") && name.ends_with(".jsonl")) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .map(unix_millis)
            .unwrap_or(now_ms);
        if now_ms.saturating_sub(modified) >= ROTATION_RETENTION_MS
            && std::fs::remove_file(entry.path()).is_ok()
        {
            result.expired += 1;
        }
    }
    drop(lock);
    Ok(result)
}

fn unix_millis(time: SystemTime) -> u64 {
    u64::try_from(
        time.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// `yyyymmdd` for a Unix-millisecond timestamp, without a calendar dependency.
fn date_stamp(unix_ms: u64) -> String {
    let days = (unix_ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}{month:02}{day:02}")
}

fn warn(message: &str) {
    use provenant_records::warning::{BUILD_LEDGER_DIAGNOSTIC, Warning, warn};
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    warn(Warning::new(
        BUILD_LEDGER_DIAGNOSTIC,
        format!("provenant: {message}"),
    ));
}

#[cfg(test)]
mod rotation_tests {
    use super::*;

    #[test]
    fn date_stamp_matches_utc_calendar() {
        // 2026-11-20T00:00:00Z.
        assert_eq!(date_stamp(1_795_132_800_000), "20261120");
        assert_eq!(date_stamp(0), "19700101");
    }

    #[test]
    fn small_ledger_is_not_rotated() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir_all(paths::build_root(&state)).unwrap();
        std::fs::write(paths::jobs_path(&state), b"{}\n").unwrap();
        let result = rotate(
            &state,
            UNIX_EPOCH + std::time::Duration::from_millis(1_795_132_800_000),
        )
        .unwrap();
        assert_eq!(result, LedgerRotation::default());
        assert!(paths::jobs_path(&state).exists());
    }
}
