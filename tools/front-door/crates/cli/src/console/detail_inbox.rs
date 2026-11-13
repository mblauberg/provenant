//! Read verified inbox links without creating owner state.
use provenant_core::{CancelToken, Roots};
use provenant_fabric_store::{Request, Store};
use provenant_os::clock::SystemClock;
use provenant_records::inbox::HumanRequest;
use provenant_run::AttemptRecord;
use serde_json::{Value, json};

fn linked(request: &HumanRequest, attempt: &AttemptRecord) -> bool {
    request.verified_work().is_some_and(|work| {
        work.graph == attempt.key.graph()
            && work
                .node
                .as_deref()
                .is_none_or(|node| node == attempt.key.node())
            && work.attempt_id.is_none_or(|id| id == attempt.attempt_id)
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
            if linked(item, attempt) {
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
    use provenant_core::{
        ActorId, AttemptId, Clock, Digest, HostId, HumanRequestId, OperationId, ProjectId,
        RootInputs,
    };
    use provenant_records::CanonicalV1;
    use provenant_run::{CanonicalRequest, ExecutionContext, PreparedRun, RunStore};

    #[test]
    fn only_verified_matching_work_is_linked() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        std::fs::create_dir(base.join("home"))?;
        std::fs::create_dir(base.join("checkout"))?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let call = provenant_run::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let project = ProjectId::from_bytes([5; 16]);
        let store = RunStore::open(
            &ExecutionContext {
                state_root: roots.state.clone(),
                project_id: project,
                actor_id: ActorId::from_bytes([2; 16]),
                read_prompts: false,
            },
            &call,
        )?;
        let attempt = AttemptId::from_bytes([3; 16]);
        let request: CanonicalRequest = serde_json::from_value(json!({
            "schema_version":1,
            "task":{"context":{"graph_id":"graph","node_id":"node","attempt":1},
                "prompt_digest":CanonicalV1::file_digest(b"body"),
                "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":"high"},
                "authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null},
            "parent":null,"lane_depth":1
        }))?;
        store.create(
            &PreparedRun {
                attempt_id: attempt,
                operation_id: OperationId::parse("inbox-link")?,
                created_at: clock.wall_now()?,
                request,
                prompt: "body".into(),
            },
            &call,
        )?;
        let record = store.record(attempt, &call)?;
        let mut request: HumanRequest = serde_json::from_value(json!({
            "schema_version":1,"id":HumanRequestId::from_bytes([4;16]),"revision":1,
            "operation_id":"fixture","created_at":"2026-01-01T00:00:00.000000Z","updated_at":"2026-01-01T00:00:00.000000Z",
            "project_id":project,"project_key":null,"owner_host":HostId::from_bytes([6;16]),
            "chair_session_id":"chair","chair_actor":ActorId::from_bytes([7;16]),
            "request":{"kind":"question","title":"Question","body":"Details","work":{"graph":"graph","node":"node","attempt_id":attempt}},
            "work_coverage":"verified","state":"open","answer_id":null,"closure_reason":null
        }))?;
        assert!(linked(&request, &record));
        request.request.work.as_mut().ok_or("work")?.node = Some("other".into());
        assert!(!linked(&request, &record));
        request.request.work.as_mut().ok_or("work")?.node = None;
        request.request.work.as_mut().ok_or("work")?.attempt_id =
            Some(AttemptId::from_bytes([8; 16]));
        assert!(!linked(&request, &record));
        request.request.work.as_mut().ok_or("work")?.attempt_id = Some(attempt);
        request.work_coverage = Some(provenant_records::inbox::WorkCoverage::Partial);
        assert!(!linked(&request, &record));
        Ok(())
    }
}
