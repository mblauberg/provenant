//! Edge for quota history: append under `quota.lock`, rotate by size, read back.
//!
//! One JSON-lines file per account and adapter at
//! `quota/history/<account_key>.<adapter>.jsonl`, with one rotated predecessor
//! `.jsonl.1`. The cap covers both files, so disk use stays under it. Readings
//! come from snapshots the existing refresh already obtained; nothing here
//! calls a provider or opens a credential.
use crate::quota::{EdgeError, child};
use provenant_os::fs::Directory;
use provenant_quota::{HistoryRecord, QuotaSnapshot, decode_lines, encode_line};
use std::{
    ffi::{OsStr, OsString},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) const DIRECTORY: &str = "history";
const CURRENT: &str = "jsonl";
const ROTATED: &str = "jsonl.1";
/// Each file rotates at half the cap, so reading this much always reaches
/// the tail that the torn-line and dedupe checks inspect.
const READ_LIMIT: u64 = provenant_quota::MAX_HISTORY_MAX_BYTES / 2;

fn file_name(snapshot: &QuotaSnapshot, suffix: &str) -> OsString {
    format!(
        "{}.{}.{suffix}",
        snapshot.account.account_key, snapshot.account.adapter
    )
    .into()
}

fn read_file(directory: &Directory, name: &OsStr, limit: u64) -> Result<Vec<u8>, EdgeError> {
    let reader = match directory.open_read(name) {
        Ok(reader) => reader,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| EdgeError::Read {
            path: name.into(),
            source,
        })?;
    Ok(bytes)
}

// Walk backwards by blocks until the newest valid reading is found.
fn last_reading(
    directory: &Directory,
    name: &OsStr,
) -> Result<(Option<HistoryRecord>, bool), EdgeError> {
    let mut file = match directory.open_read(name) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok((None, false)),
        Err(error) => return Err(error.into()),
    };
    let mut end = file.metadata()?.size.min(READ_LIMIT);
    // Bytes of the current line, collected newest first, so each byte is
    // visited once however many blocks the line spans.
    let mut pending = Vec::new();
    let mut torn = false;
    let mut first = true;
    while end > 0 {
        let start = end.saturating_sub(4096);
        file.seek(SeekFrom::Start(start))
            .map_err(|source| EdgeError::Read {
                path: name.into(),
                source,
            })?;
        let mut block = vec![0; (end - start) as usize];
        file.read_exact(&mut block)
            .map_err(|source| EdgeError::Read {
                path: name.into(),
                source,
            })?;
        if first {
            torn = block.last().is_some_and(|b| *b != b'\n');
            first = false;
        }
        for byte in block.into_iter().rev() {
            if byte != b'\n' {
                pending.push(byte);
                continue;
            }
            pending.reverse();
            if let Some(record) = decode_lines(&pending).pop() {
                return Ok((Some(record), torn));
            }
            pending.clear();
        }
        end = start;
    }
    pending.reverse();
    Ok((decode_lines(&pending).pop(), torn))
}

/// Append the snapshot as one record unless it repeats the last reading.
/// The caller holds `quota.lock`, so lines never interleave. Returns whether
/// a record was written.
pub(crate) fn record(
    quota: &Directory,
    snapshot: &QuotaSnapshot,
    max_bytes: u64,
) -> Result<bool, EdgeError> {
    if HistoryRecord::from_snapshot(snapshot).is_none() {
        return Ok(false);
    }
    let directory = child(quota, OsStr::new(DIRECTORY))?;
    let current = file_name(snapshot, CURRENT);
    let rotated = file_name(snapshot, ROTATED);
    let half = max_bytes / 2;
    let (mut last, torn) = last_reading(&directory, &current)?;
    if last.is_none() {
        last = last_reading(&directory, &rotated)?.0;
    }
    let Some(next) = HistoryRecord::observe(last.as_ref(), snapshot) else {
        return Ok(false);
    };
    let line = encode_line(&next)?;
    let size = directory
        .open_read(&current)
        .ok()
        .and_then(|file| file.metadata().ok())
        .map_or(0, |meta| meta.size);
    // Rotate before the append, so the newest record always lands in `current`.
    let rotated_now = size > 0 && size.saturating_add(line.len() as u64) > half;
    if rotated_now {
        directory.replace(&current, &directory, &rotated)?;
        directory.sync()?;
    }
    // Rotation left an empty file. Otherwise a crashed writer may have left an
    // unterminated fragment, which the new line would glue onto and spoil.
    let torn = !rotated_now && torn;
    let mut file = directory.open_append(&current)?;
    if torn {
        file.write_all(b"\n").map_err(|source| EdgeError::Read {
            path: current.clone().into(),
            source,
        })?;
    }
    file.write_all(&line).map_err(|source| EdgeError::Read {
        path: current.clone().into(),
        source,
    })?;
    file.sync()?;
    Ok(true)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Query {
    /// Account label or key.
    pub account: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
}

/// Every matching record from the history files, oldest first. Torn lines
/// are skipped. A missing history directory is an empty history.
pub(crate) fn read(state: &Path, query: &Query) -> Result<Vec<HistoryRecord>, EdgeError> {
    let root = match Directory::open_anchor(state) {
        Ok(root) => root,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let quota = match root.open_verified_directory(OsStr::new("quota")) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let directory = match quota.open_verified_directory(OsStr::new(DIRECTORY)) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in directory.entries()? {
        let entry = entry?;
        let Some(name) = entry.to_str() else { continue };
        if !(name.ends_with(&format!(".{CURRENT}")) || name.ends_with(&format!(".{ROTATED}"))) {
            continue;
        }
        for record in decode_lines(&read_file(&directory, &entry, READ_LIMIT)?) {
            if query
                .account
                .as_ref()
                .is_some_and(|a| *a != record.label && *a != record.account_key)
                || query
                    .since
                    .is_some_and(|since| record.observed_unix_seconds < since)
                || query
                    .until
                    .is_some_and(|until| record.observed_unix_seconds > until)
            {
                continue;
            }
            records.push(record);
        }
    }
    records.sort_by(|a, b| {
        (a.observed_unix_seconds, &a.account_key, &a.provider).cmp(&(
            b.observed_unix_seconds,
            &b.account_key,
            &b.provider,
        ))
    });
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::private;
    use provenant_core::{CancelToken, Clock, WaitBudget};
    use provenant_os::{clock::SystemClock, fs::LockMode};
    use provenant_quota::{AccountScope, MarkerKind, QuotaDetails, QuotaStatus, QuotaWindow};
    use std::os::unix::fs::PermissionsExt;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn snapshot(at: i64, plan: Option<&str>, used: u64, reset: i64) -> QuotaSnapshot {
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
            source_ref: "fixture".into(),
            authority_ref: None,
            windows: vec![QuotaWindow {
                pool: "five_hour".into(),
                window_seconds: Some(18_000),
                used_basis_points: used,
                resets_unix_seconds: Some(reset),
            }],
            details: Some(QuotaDetails {
                plan: plan.map(str::to_owned),
                ..QuotaDetails::default()
            }),
        }
    }
    fn state() -> Result<(tempfile::TempDir, Directory), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
        let root = private(temp.path())?;
        let quota = child(&root, OsStr::new("quota"))?;
        Ok((temp, quota))
    }

    #[test]
    fn identical_refresh_appends_nothing_and_changes_append_once() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        assert!(record(&quota, &snapshot(100, None, 500, 900), cap)?);
        assert!(!record(&quota, &snapshot(160, None, 500, 900), cap)?);
        assert!(record(&quota, &snapshot(220, None, 600, 900), cap)?);
        // A new reset time alone is a change and is marked as a reset.
        assert!(record(&quota, &snapshot(280, None, 600, 1900), cap)?);
        assert!(record(&quota, &snapshot(340, None, 20, 1900), cap)?);
        let all = read(temp.path(), &Query::default())?;
        assert_eq!(all.len(), 4);
        assert!(all[1].markers.is_empty());
        assert_eq!(all[2].markers[0].kind, MarkerKind::Reset);
        assert_eq!(all[3].markers[0].kind, MarkerKind::Reset);
        Ok(())
    }

    #[test]
    fn plan_change_is_marked_and_history_survives_snapshot_overwrite() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        record(&quota, &snapshot(100, Some("max_5x"), 500, 900), cap)?;
        // The snapshot file is overwritten on every refresh; history is not.
        let account = child(&quota, OsStr::new("k1"))?;
        account.unlink(OsStr::new("claude.json")).ok();
        record(&quota, &snapshot(200, Some("max_20x"), 500, 900), cap)?;
        let all = read(temp.path(), &Query::default())?;
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].markers.len(), 1);
        assert_eq!(all[1].markers[0].kind, MarkerKind::PlanChange);
        assert_eq!(all[1].plan.as_deref(), Some("max_20x"));
        Ok(())
    }

    #[test]
    fn rotation_bounds_disk_and_keeps_the_newest_record() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::MIN_HISTORY_MAX_BYTES;
        let count = 600_i64;
        for n in 0..count {
            record(
                &quota,
                &snapshot(1_000 + n, None, u64::try_from(n)?, 99_999),
                cap,
            )?;
        }
        let directory = child(&quota, OsStr::new(DIRECTORY))?;
        let mut total = 0;
        for entry in directory.entries()? {
            total += directory.open_read(&entry?)?.metadata()?.size;
        }
        assert!(total <= cap, "{total} > {cap}");
        let all = read(temp.path(), &Query::default())?;
        assert!(all.len() > 100 && all.len() < usize::try_from(count)?);
        let newest = all.last().ok_or("newest")?;
        assert_eq!(newest.observed_unix_seconds, 1_000 + count - 1);
        // Dedupe continues across a rotation: repeating the newest appends nothing.
        assert!(!record(
            &quota,
            &snapshot(5_000, None, u64::try_from(count - 1)?, 99_999),
            cap
        )?);
        Ok(())
    }

    #[test]
    fn locked_concurrent_appenders_never_tear_a_line() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        // The first lock creates the lock file; contenders then queue on it.
        {
            let clock = SystemClock::new();
            let budget = WaitBudget::cli(clock.mono_now(), 30)?;
            let _first = quota.lock(
                OsStr::new("quota.lock"),
                LockMode::Exclusive,
                budget,
                &CancelToken::new(),
                &clock,
            )?;
        }
        let quota = std::sync::Arc::new(quota);
        let clockwork = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let mut workers = Vec::new();
        for _ in 0..4 {
            let quota = quota.clone();
            let ticks = clockwork.clone();
            workers.push(std::thread::spawn(move || -> Result<(), String> {
                for _ in 0..25 {
                    let clock = SystemClock::new();
                    let cancel = CancelToken::new();
                    let budget =
                        WaitBudget::cli(clock.mono_now(), 30).map_err(|e| format!("{e:?}"))?;
                    let _lock = quota
                        .lock(
                            OsStr::new("quota.lock"),
                            LockMode::Exclusive,
                            budget,
                            &cancel,
                            &clock,
                        )
                        .map_err(|e| e.to_string())?;
                    // Read under the lock so observed times rise in append order.
                    let n = ticks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let used = u64::try_from(n).map_err(|e| e.to_string())?;
                    record(&quota, &snapshot(10_000 + n, None, used, 99_999), cap)
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            }));
        }
        for worker in workers {
            worker.join().map_err(|_| "worker panicked")??;
        }
        let bytes = std::fs::read(temp.path().join("quota/history/k1.claude.jsonl"))?;
        let lines = bytes
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count();
        assert_eq!(lines, 100);
        assert_eq!(decode_lines(&bytes).len(), 100);
        Ok(())
    }

    #[test]
    fn records_hold_no_source_text_beyond_the_reading() -> TestResult {
        let (temp, quota) = state()?;
        let mut source = snapshot(100, Some("max_5x"), 500, 900);
        source.source_ref = "sk-ant-secret-source".into();
        source.authority_ref = Some("Bearer secret-authority".into());
        if let Some(details) = source.details.as_mut() {
            details.fingerprint = Some("0123456789abcdef".into());
            details.note = Some("prompt text secret-note".into());
            details.source = Some("secret-detail-source".into());
        }
        record(&quota, &source, provenant_quota::DEFAULT_HISTORY_MAX_BYTES)?;
        let path = temp.path().join("quota/history/k1.claude.jsonl");
        let text = std::fs::read_to_string(&path)?;
        for secret in ["secret", "0123456789abcdef", "fixture", "Bearer", "sk-ant"] {
            assert!(!text.contains(secret), "{secret}");
        }
        assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o077, 0);
        Ok(())
    }

    #[test]
    fn last_reading_crosses_blocks_and_skips_corrupt_tail() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        record(&quota, &snapshot(100, None, 500, 900), cap)?;
        let path = temp.path().join("quota/history/k1.claude.jsonl");
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        file.write_all(&vec![b'x'; 9000])?;
        drop(file);
        assert!(!record(&quota, &snapshot(200, None, 500, 900), cap)?);
        assert!(record(&quota, &snapshot(300, None, 600, 900), cap)?);
        assert_eq!(read(temp.path(), &Query::default())?.len(), 2);
        Ok(())
    }

    #[test]
    fn a_long_unterminated_tail_is_scanned_in_linear_time() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        record(&quota, &snapshot(100, None, 500, 900), cap)?;
        let path = temp.path().join("quota/history/k1.claude.jsonl");
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        // Spans many 4 KiB blocks with no newline. Re-searching the growing
        // buffer per block took tens of seconds here; one pass takes well under
        // a second.
        file.write_all(&vec![b'x'; 4 << 20])?;
        drop(file);
        let clock = SystemClock::new();
        let started = clock.mono_now().as_micros();
        assert!(!record(&quota, &snapshot(200, None, 500, 900), cap)?);
        let elapsed = clock.mono_now().as_micros().saturating_sub(started);
        assert!(elapsed < 5_000_000, "{elapsed} us");
        Ok(())
    }

    #[test]
    fn a_torn_tail_or_corrupt_line_does_not_spoil_the_next_record() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        record(&quota, &snapshot(100, None, 500, 900), cap)?;
        let path = temp.path().join("quota/history/k1.claude.jsonl");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"{\"schema_version\":1,\"obser")?;
        drop(file);
        assert!(record(&quota, &snapshot(200, None, 600, 900), cap)?);
        assert_eq!(read(temp.path(), &Query::default())?.len(), 2);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(b"not json\n")?;
        drop(file);
        assert!(record(&quota, &snapshot(300, None, 700, 900), cap)?);
        let all = read(temp.path(), &Query::default())?;
        assert_eq!(
            all.iter()
                .map(|r| r.observed_unix_seconds)
                .collect::<Vec<_>>(),
            [100, 200, 300]
        );
        Ok(())
    }

    #[test]
    fn readings_filter_by_account_and_time() -> TestResult {
        let (temp, quota) = state()?;
        let cap = provenant_quota::DEFAULT_HISTORY_MAX_BYTES;
        for at in [100, 200, 300] {
            record(&quota, &snapshot(at, None, u64::try_from(at)?, 900), cap)?;
        }
        let pick = |account: Option<&str>, since: Option<i64>, until: Option<i64>| {
            read(
                temp.path(),
                &Query {
                    account: account.map(str::to_owned),
                    since,
                    until,
                },
            )
            .map(|r| r.len())
        };
        assert_eq!(pick(Some("work"), None, None)?, 3);
        assert_eq!(pick(Some("k1"), Some(200), None)?, 2);
        assert_eq!(pick(None, Some(150), Some(250))?, 1);
        assert_eq!(pick(Some("other"), None, None)?, 0);
        Ok(())
    }
}
