//! Wait scheduling for provider-silence deadlines.

use super::{GraphWake, Reading};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Remaining time until the earliest live local attempt's configured stall
/// deadline. The attempt row carries the same resolved limit Run used to
/// project `stalled`, plus the activity timestamp from which Run measures it.
pub(super) fn local_stall_wait(readings: &[Reading], now: SystemTime) -> Option<Duration> {
    let now_micros = now.duration_since(UNIX_EPOCH).ok()?.as_micros();
    readings
        .iter()
        .flat_map(|reading| reading.data["rows"].as_array().into_iter().flatten())
        .filter(|row| {
            row["kind"] == "attempt"
                && row["state"] == "running"
                && row["presence"] == "live"
                && row["stalled"] != true
                && row["last_event_source"] != "queued"
        })
        .filter_map(|row| {
            let threshold = row["limits"]["stall_after_seconds"]["value"].as_u64()?;
            if threshold == 0 {
                return None;
            }
            let last_event = row["last_event_at"].as_str()?;
            let last_event = provenant_core::WallTime::parse_rfc3339(last_event).ok()?;
            let deadline = last_event
                .as_micros()
                .checked_add(i64::try_from(threshold.checked_mul(1_000_000)?).ok()?)?;
            let remaining = (i128::from(deadline) - i128::try_from(now_micros).ok()?).max(0);
            Some(Duration::from_micros(u64::try_from(remaining).ok()?))
        })
        .min()
}

pub(super) fn timeout_wake(now: Instant, stall_deadline: Option<Instant>) -> GraphWake {
    if stall_deadline.is_some_and(|deadline| now >= deadline) {
        GraphWake::StallDeadline
    } else {
        GraphWake::NativeTimeout
    }
}

#[cfg(test)]
mod tests {
    #[test]
    // Spec: docs/specs/cli.md#waits-are-readers
    fn local_stall_deadline_rereads_a_pending_wait_without_an_event() {
        use super::{local_stall_wait, timeout_wake};
        use crate::status_wait::{GraphWake, Settle, classify_rows, graph_reads_for_wake};
        use serde_json::json;
        use std::time::{Duration, Instant, UNIX_EPOCH};

        let last_event = provenant_core::WallTime::parse_rfc3339("2026-11-26T04:58:00.000000Z")
            .expect("fixture last event");
        let before_threshold =
            UNIX_EPOCH + Duration::from_micros((last_event.as_micros() as u64) + 29_000_000);
        let mut rows = vec![
            json!({"kind":"graph","state":"running"}),
            json!({
                "kind":"attempt","state":"running","presence":"live","stalled":false,
                "last_event_source":"output","last_event_at":last_event,
                "limits":{"stall_after_seconds":{"value":30}}
            }),
        ];
        let pending = classify_rows("quiet", &rows);
        assert_eq!(pending.settle, Settle::Pending);

        let until_stall = local_stall_wait(&[pending], before_threshold)
            .expect("a live attempt has an upcoming stall deadline");
        assert_eq!(until_stall, Duration::from_secs(1));

        let mono_start = Instant::now();
        let stall_deadline = mono_start + until_stall;
        let wait_timeout = mono_start + Duration::from_secs(10);
        assert!(stall_deadline < wait_timeout);
        assert_eq!(wait_timeout.min(stall_deadline) - mono_start, until_stall);
        assert_eq!(
            timeout_wake(
                mono_start + Duration::from_millis(999),
                Some(stall_deadline)
            ),
            GraphWake::NativeTimeout,
            "the wait remains pending before its configured threshold"
        );
        let wake = timeout_wake(stall_deadline, Some(stall_deadline));
        assert_eq!(wake, GraphWake::StallDeadline);
        assert_eq!(graph_reads_for_wake(&[0], &Default::default(), wake), [0]);

        rows[1]["stalled"] = json!(true);
        let stalled = classify_rows("quiet", &rows);
        assert_eq!(stalled.settle, Settle::Stalled);
        assert!(
            stalled.done(),
            "the deadline reread ends wait before timeout"
        );

        rows[1]["limits"]["stall_after_seconds"]["value"] = json!(0);
        assert_eq!(
            local_stall_wait(&[classify_rows("quiet", &rows)], before_threshold),
            None
        );
    }
}
