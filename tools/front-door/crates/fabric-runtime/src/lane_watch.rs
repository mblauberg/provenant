//! Lane wake/debounce policy with explicit millisecond clocks. Native recursive
//! watch registration and the bounded wait edge await supervisor support.
use std::path::Path;

pub const SETTLE_MS: u64 = 300;

pub fn relevant(name: Option<&Path>) -> bool {
    let Some(name) = name else {
        return true;
    };
    let name = name
        .file_name()
        .unwrap_or(name.as_os_str())
        .to_string_lossy();
    let bytes = name.as_bytes();
    name.ends_with(".json")
        || name == "result.md"
        || (bytes.len() >= 14
            && bytes[..8].iter().all(u8::is_ascii_digit)
            && bytes[8] == b'-'
            && bytes[9..13].iter().all(u8::is_ascii_digit)
            && bytes[13] == b'-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdleAction {
    WaitUntil(u64),
    SettleUntil(u64),
}

#[derive(Default)]
pub struct LaneIdle {
    changed: bool,
}
impl LaneIdle {
    pub fn change(&mut self, name: Option<&Path>) -> bool {
        if !relevant(name) {
            return false;
        }
        self.changed = true;
        true
    }
    /// The edge refreshes watch roots on every call, even for a zero budget.
    pub fn begin(&self, now_ms: u64, budget_ms: u64) -> IdleAction {
        let deadline = now_ms.saturating_add(budget_ms);
        if self.changed {
            IdleAction::SettleUntil(deadline.min(now_ms.saturating_add(SETTLE_MS)))
        } else {
            IdleAction::WaitUntil(deadline)
        }
    }
    /// Called when change or the fallback timer wakes a wait. Settling never
    /// extends the caller's deadline. A change during settling is coalesced.
    pub fn woke(&self, now_ms: u64, deadline_ms: u64) -> IdleAction {
        IdleAction::SettleUntil(if self.changed {
            deadline_ms
                .max(now_ms)
                .min(now_ms.saturating_add(SETTLE_MS))
        } else {
            now_ms
        })
    }
    pub fn settled(&mut self) {
        self.changed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_receipts_results_and_new_runs_wake_but_logs_do_not() {
        for name in [
            "attempt.json",
            "nested/dispatch-status.json",
            "result.md",
            "20260101-0000-batch-fixture",
        ] {
            assert!(relevant(Some(Path::new(name))), "{name}");
        }
        for name in [
            "stdout.jsonl",
            "stderr.log",
            "other.md",
            "xresult.md",
            "2026010x-0000-batch",
            "20260101-000x-batch",
            "attempt.json.tmp",
        ] {
            assert!(!relevant(Some(Path::new(name))), "{name}");
        }
        assert!(relevant(None));
    }
    #[test]
    fn relevant_change_shortens_wait_and_settles_within_budget() {
        let mut lane = LaneIdle::default();
        assert_eq!(lane.begin(100, 20_000), IdleAction::WaitUntil(20_100));
        lane.change(Some(Path::new("receipt.json")));
        assert_eq!(lane.woke(150, 20_100), IdleAction::SettleUntil(450));
        assert_eq!(lane.woke(150, 160), IdleAction::SettleUntil(160));
        lane.settled();
        assert_eq!(lane.begin(450, 0), IdleAction::WaitUntil(450));
    }
    #[test]
    fn streaming_change_cannot_wake_and_prior_change_uses_remaining_budget() {
        let mut lane = LaneIdle::default();
        assert!(!lane.change(Some(Path::new("stdout.jsonl"))));
        assert_eq!(lane.begin(0, 800), IdleAction::WaitUntil(800));
        assert_eq!(lane.woke(800, 800), IdleAction::SettleUntil(800));
        lane.change(None);
        assert_eq!(lane.begin(100, 60), IdleAction::SettleUntil(160));
        assert_eq!(lane.begin(100, 0), IdleAction::SettleUntil(100));
    }
    #[test]
    fn relevance_matches_ts_regexp_when_node_is_present() {
        let names = [
            "x.json",
            "nested/stdout.jsonl",
            "result.md",
            "20260101-0000-x",
            "x.json\n",
            "20260101-0000-",
            "🦀.json",
        ];
        let args = serde_json::to_string(&names).unwrap();
        let Some(expected) = crate::test_support::node(
            r#"import {basename} from 'node:path'; console.log(JSON.stringify(JSON.parse(process.argv[1]).map(n=>/\.json$|^result\.md$|^\d{8}-\d{4}-/u.test(basename(n)))))"#,
            &[args],
        ) else {
            return;
        };
        let expected: Vec<bool> = serde_json::from_value(expected).unwrap();
        assert_eq!(
            names.map(|n| relevant(Some(Path::new(n)))).as_slice(),
            expected
        );
    }
}
