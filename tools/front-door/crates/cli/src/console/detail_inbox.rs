//! Read verified inbox links without creating owner state.
use provenant_core::{CancelToken, Roots};
use provenant_fabric_store::{Request, Store};
use provenant_os::clock::SystemClock;
use provenant_records::inbox::HumanRequest;
use provenant_run::AttemptRecord;
use serde_json::{Value, json};

fn linked(
    request: &HumanRequest,
    run: provenant_core::RunId,
    task: provenant_core::TaskId,
    attempt: provenant_core::AttemptId,
) -> bool {
    request.verified_work().is_some_and(|work| {
        work.run_id == run
            && work.task_id.is_none_or(|id| id == task)
            && work.attempt_id.is_none_or(|id| id == attempt)
    })
}

/// The inbox has a 10,000-request retention limit. Bound both the scan and
/// returned links, and make any incomplete coverage visible to the caller.
pub(super) fn read(roots: &Roots, attempt: &AttemptRecord) -> (Vec<Value>, &'static str) {
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = match Store::open_existing(&roots.state, &call) {
        Ok(store) => store,
        Err(provenant_fabric_store::Error::NotFound) => return (vec![], "complete"),
        Err(_) => return (vec![], "unavailable"),
    };
    let mut after = None;
    let mut scanned = 0;
    let mut links = Vec::new();
    loop {
        let page = match store.human_requests(attempt.project_id, None, true, after, 100, &call) {
            Ok(page) => page,
            Err(_) => return (links, "unavailable"),
        };
        scanned += page.items.len();
        for item in &page.items {
            if linked(item, attempt.run_id, attempt.task_id, attempt.attempt_id) {
                if links.len() == 100 {
                    return (links, "truncated");
                }
                links.push(json!({"id":item.id,"revision":item.revision,"state":item.state,"title":item.request.title,"work":item.verified_work()}));
            }
        }
        if !page.truncated {
            return (links, "complete");
        }
        if scanned >= 10_000 || page.items.is_empty() {
            return (links, "truncated");
        }
        after = page.items.last().map(|item| item.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{ActorId, AttemptId, HostId, HumanRequestId, ProjectId, RunId, TaskId};

    #[test]
    fn only_verified_matching_work_is_linked() {
        let run = RunId::from_bytes([1; 16]);
        let task = TaskId::from_bytes([2; 16]);
        let attempt = AttemptId::from_bytes([3; 16]);
        let mut request: HumanRequest = serde_json::from_value(json!({
            "schema_version":1,"id":HumanRequestId::from_bytes([4;16]),"revision":1,
            "operation_id":"fixture","created_at":"2026-01-01T00:00:00.000000Z","updated_at":"2026-01-01T00:00:00.000000Z",
            "project_id":ProjectId::from_bytes([5;16]),"project_key":null,"owner_host":HostId::from_bytes([6;16]),
            "chair_session_id":"chair","chair_actor":ActorId::from_bytes([7;16]),
            "request":{"kind":"question","title":"Question","body":"Details","work":{"run_id":run,"task_id":task,"attempt_id":attempt}},
            "work_coverage":"verified","state":"open","answer_id":null,"closure_reason":null
        })).unwrap();
        assert!(linked(&request, run, task, attempt));
        assert!(!linked(&request, run, task, AttemptId::from_bytes([8; 16])));
        request.work_coverage = Some(provenant_records::inbox::WorkCoverage::Partial);
        assert!(!linked(&request, run, task, attempt));
    }
}
