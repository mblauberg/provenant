//! Chrome Trace Event export of terminal attempts, for ui.perfetto.dev.
use std::collections::BTreeMap;

use provenant_core::WallTime;
use provenant_records::AttemptFact;
use serde_json::{Value, json};

/// One process per task and one thread per attempt; `ts` and `dur` are
/// microseconds. Attempt facts carry no run ID, so the run filter is applied by
/// the store and tasks group the tracks.
pub fn trace(facts: &[AttemptFact], since: Option<WallTime>) -> Value {
    let mut facts: Vec<&AttemptFact> = facts
        .iter()
        .filter(|f| since.is_none_or(|s| f.finished_at >= s))
        .collect();
    facts.sort_by_key(|f| (f.started_at.unwrap_or(f.queued_at), f.attempt_number));
    let mut pids = BTreeMap::new();
    let mut events = Vec::new();
    for (tid, fact) in (1u64..).zip(&facts) {
        let next = pids.len() as u64 + 1;
        let pid = *pids.entry(fact.task_id.to_string()).or_insert_with(|| {
            events.push(meta(
                "process_name",
                next,
                0,
                format!("task {}", fact.task_id),
            ));
            next
        });
        events.push(meta(
            "thread_name",
            pid,
            tid,
            format!("attempt {} ({})", fact.attempt_number, fact.attempt_id),
        ));
        let start = fact.started_at.unwrap_or(fact.queued_at).as_micros();
        let end = fact.finished_at.as_micros().max(start);
        let route = fact.resolved_route.as_ref();
        let outcome = serde_json::to_value(fact.terminal_state).unwrap_or(Value::Null);
        let u = &fact.usage;
        events.push(json!({
            "name": outcome, "cat": "attempt", "ph": "X", "pid": pid, "tid": tid,
            "ts": start, "dur": end - start,
            "args": {
                "provider": route.and_then(|r| r.provider.as_ref()),
                "model": route.and_then(|r| r.model.as_ref()),
                "account": route.and_then(|r| r.account_label.as_ref()),
                "outcome": outcome,
                "tokens": {
                    "uncached_input": u.uncached_input, "cache_read": u.cache_read,
                    "cache_write": u.cache_write, "output": u.output_excluding_reasoning,
                    "reasoning": u.reasoning,
                },
            },
        }));
        if fact.started_at.is_some() {
            events.push(json!({
                "name": "queued", "cat": "state", "ph": "i", "s": "t", "pid": pid, "tid": tid,
                "ts": fact.queued_at.as_micros().min(start),
            }));
        }
    }
    json!({ "traceEvents": events, "displayTimeUnit": "ms" })
}

fn meta(name: &str, pid: u64, tid: u64, label: String) -> Value {
    json!({ "name": name, "ph": "M", "pid": pid, "tid": tid, "args": { "name": label } })
}
