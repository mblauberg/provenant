//! Chrome Trace Event export of terminal attempts, for ui.perfetto.dev.
use std::collections::BTreeMap;

use provenant_core::{AttemptId, HostId, WallTime};
use provenant_records::AttemptFact;
use serde_json::{Value, json};

/// The hosts an export knows by name. A fact whose trace names no host belongs
/// to `local`; a host without a name is labelled by its ID.
#[derive(Debug, Default)]
pub struct Hosts {
    pub local: Option<HostId>,
    pub names: BTreeMap<HostId, String>,
}

/// One process per host and one thread per attempt; `ts` and `dur` are
/// microseconds. The local host comes first. A flow arrow joins an attempt to
/// the parent it descends from when the export holds both, on this host or
/// across hosts. Attempt facts carry no run ID, so the run filter is applied by
/// the store.
pub fn trace(facts: &[AttemptFact], hosts: &Hosts, since: Option<WallTime>) -> Value {
    let mut facts: Vec<&AttemptFact> = facts
        .iter()
        .filter(|f| since.is_none_or(|s| f.finished_at >= s))
        .collect();
    facts.sort_by_key(|f| (f.started_at.unwrap_or(f.queued_at), f.attempt_number));
    let host_of = |fact: &AttemptFact| fact.trace.host.or(hosts.local);
    let mut keys: Vec<_> = facts.iter().map(|f| host_of(f)).collect();
    keys.sort_by_key(|key| (*key != hosts.local, key.is_none(), *key));
    keys.dedup();
    let mut events = Vec::new();
    let mut pids = BTreeMap::new();
    for (pid, key) in (1u64..).zip(&keys) {
        let label = match key {
            Some(host) => match hosts.names.get(host) {
                Some(name) => format!("host {name} ({host})"),
                None => format!("host {host}"),
            },
            None => "unknown host".to_owned(),
        };
        events.push(meta("process_name", pid, 0, label));
        pids.insert(*key, pid);
    }
    let mut placed: BTreeMap<AttemptId, (u64, u64, i64, i64)> = BTreeMap::new();
    for (tid, fact) in (1u64..).zip(&facts) {
        let pid = pids[&host_of(fact)];
        events.push(meta(
            "thread_name",
            pid,
            tid,
            format!(
                "task {} attempt {} ({})",
                fact.task_id, fact.attempt_number, fact.attempt_id
            ),
        ));
        let start = fact.started_at.unwrap_or(fact.queued_at).as_micros();
        let end = fact.finished_at.as_micros().max(start);
        placed.insert(fact.attempt_id, (pid, tid, start, end));
        let route = fact.resolved_route.as_ref();
        let outcome = serde_json::to_value(fact.terminal_state).unwrap_or(Value::Null);
        let u = &fact.usage;
        events.push(json!({
            "name": outcome, "cat": "attempt", "ph": "X", "pid": pid, "tid": tid,
            "ts": start, "dur": end - start,
            "args": {
                "task": fact.task_id,
                "provider": route.and_then(|r| r.provider.as_ref()),
                "model": route.and_then(|r| r.model.as_ref()),
                "account": route.and_then(|r| r.account_label.as_ref()),
                "outcome": outcome,
                "tokens": {
                    "uncached_input": u.uncached_input, "cache_read": u.cache_read,
                    "cache_write": u.cache_write, "output": u.output_excluding_reasoning,
                    "reasoning": u.reasoning,
                },
                "trace": fact.trace,
            },
        }));
        if fact.started_at.is_some() {
            events.push(json!({
                "name": "queued", "cat": "state", "ph": "i", "s": "t", "pid": pid, "tid": tid,
                "ts": fact.queued_at.as_micros().min(start),
            }));
        }
    }
    let mut flow = 0u64;
    for fact in &facts {
        let Some(&(from_pid, from_tid, from_start, from_end)) =
            fact.trace.parent.and_then(|parent| placed.get(&parent))
        else {
            continue;
        };
        let (pid, tid, start, _) = placed[&fact.attempt_id];
        flow += 1;
        let out = start.clamp(from_start, from_end);
        for (ph, pid, tid, ts) in [
            ("s", from_pid, from_tid, out),
            ("f", pid, tid, start.max(out)),
        ] {
            events.push(json!({
                "name": "parent", "cat": "parent", "ph": ph, "bp": "e", "id": flow,
                "pid": pid, "tid": tid, "ts": ts,
            }));
        }
    }
    json!({ "traceEvents": events, "displayTimeUnit": "ms" })
}

fn meta(name: &str, pid: u64, tid: u64, label: String) -> Value {
    json!({ "name": name, "ph": "M", "pid": pid, "tid": tid, "args": { "name": label } })
}
