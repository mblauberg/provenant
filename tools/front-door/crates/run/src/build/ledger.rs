//! Append-only build job outcomes with a host ledger and lane fallback.

use super::{JobOutcome, paths};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use provenant_os::fs::{Directory, LockMode, ledger::Ledger};
use std::{ffi::OsStr, fs::OpenOptions, io::Write, path::Path};

/// Current wall time in Unix milliseconds, saturating on clock conversion.
pub fn now_unix_ms() -> u64 {
    u64::try_from(
        provenant_os::clock::SystemClock::new()
            .wall_now()
            .map_or(0, |time| time.unix_millis().max(0)),
    )
    .unwrap_or_default()
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

fn warn(message: &str) {
    let _ = writeln!(std::io::stderr().lock(), "provenant: {message}");
}
