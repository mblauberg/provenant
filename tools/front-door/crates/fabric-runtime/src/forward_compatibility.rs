//! Peer digests include unknown fields at every level of known requests.
use crate::{
    coordination::{facade::*, *},
    handoff::*,
    hosts::*,
    inbox::InboxInput,
};
use provenant_fabric_store::{
    ClaimFence, ClaimRequest, LandingRequest, MessageRef, PushOperation, PushRequest, TaskFence,
    TaskRef, WorkScope,
};
use provenant_records::CanonicalV1;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const ID: &str = "11111111111111111111111111111111";
const TOKEN: &str = "00000000000000000000000000000000";
fn oid() -> String {
    "ab".repeat(20)
}
fn digest() -> String {
    format!("sha256:{}", "ab".repeat(32))
}
fn target() -> Value {
    json!({"attempt":ID})
}
fn task() -> Value {
    json!({"prompt":"work","route":{"kind":"named","name":"senior","effort":null},"timeout_seconds":60,
        "deliver":{"remote":"origin","branch":"work","open_pr":true},"new_worktree":{"base":"main","branch":"work"}})
}
fn submit() -> Value {
    json!({"op":"submit","operation_id":"submit","tasks":[task()]})
}
fn peer(action: Value) -> Value {
    json!({"version":1,"protocol":PROTOCOL,"target_host":ID,"source_host":ID,"agent":"chair","project":{"key":"project","common_dir":"repo/.git"},"action":action,"trace":{}})
}
fn scope() -> Value {
    json!({"kind":"issue","key":"2144"})
}
fn claim_proof() -> Value {
    json!({"id":ID,"generation":1,"token":TOKEN})
}
fn landing_proof() -> Value {
    let mut v = claim_proof();
    v["reference"] = json!("refs/heads/work");
    v
}
fn mail() -> Value {
    json!({"op":"send","recipients":[ID],"kind":"note","body":"hello","artifacts":[{"path":{"encoding":"utf8","path":"out"},"digest":null}],
        "reply_to":{"scope":"remote","host":ID,"id":ID},"task":{"scope":"local","id":ID}})
}
fn handoff() -> Value {
    json!({"operation_id":"handoff","account":"main","model":null,"prompt":"# Session hand-off\nContinue","branches":[],"attachments":[{"name":"note.md","base64":"aGVsbG8="}],"launch":false})
}

fn round_trip<T: Serialize + DeserializeOwned>(seed: Value) {
    let baseline: T = serde_json::from_value(seed).unwrap();
    let mut newer = serde_json::to_value(baseline).unwrap();
    newer["future"] = json!({"nested":[null,u64::MAX,true]});
    assert_round_trip::<T>(newer);
}
fn assert_round_trip<T: Serialize + DeserializeOwned>(newer: Value) {
    let digest = CanonicalV1::record_digest("fabric-peer", 1, &newer).unwrap();
    let decoded: T = serde_json::from_value(newer.clone()).unwrap();
    assert_eq!(
        CanonicalV1::bytes(&decoded).unwrap(),
        CanonicalV1::bytes(&newer).unwrap()
    );
    assert_eq!(
        CanonicalV1::record_digest("fabric-peer", 1, &decoded).unwrap(),
        digest
    );
}
macro_rules! cases {
    ($($name:ident: $ty:ty => $seed:expr;)+) => {$(
        #[test] fn $name() { round_trip::<$ty>($seed); }
    )+};
}
cases! {
    peer_request_digest_round_trip: PeerRequest => peer(json!({"action":"health"}));
    project_locator_digest_round_trip: ProjectLocator => json!({"key":"project","common_dir":"repo/.git"});
    peer_health_action_digest_round_trip: PeerAction => json!({"action":"health"});
    peer_submit_action_digest_round_trip: PeerAction => json!({"action":"submit","input":submit()});
    coordination_input_digest_round_trip: CoordinationInput => json!({"domain":"mail","input":mail()});
    mail_input_digest_round_trip: MailInput => mail();
    task_input_digest_round_trip: TaskInput => json!({"op":"create","objective":"work","note":null});
    events_input_digest_round_trip: EventsInput => json!({"op":"note","note":"work","dedup_key":null});
    claim_input_digest_round_trip: ClaimInput => json!({"op":"claim_acquire","scope":scope(),"lease_seconds":1});
    landing_input_digest_round_trip: LandingInput => json!({"op":"landing_verify","proof":landing_proof()});
    identity_input_digest_round_trip: IdentityInput => json!({"op":"inspect"});
    graph_input_digest_round_trip: GraphInput => json!({"op":"run","document":"graph.json"});
    submit_input_digest_round_trip: SubmitInput => submit();
    run_input_digest_round_trip: RunInput => json!({"op":"status","target":target()});
    output_input_digest_round_trip: OutputInput => json!({"op":"read","target":target(),"part":"result","offset":0,"max_bytes":1024});
    hosts_input_digest_round_trip: HostsInput => json!({"op":"hosts","limit":10});
    submitted_task_digest_round_trip: SubmittedTask => task();
    attempt_target_digest_round_trip: AttemptTarget => target();
    new_worktree_digest_round_trip: NewWorktree => json!({"base":"main","branch":"work"});
    task_proof_digest_round_trip: TaskProof => json!({"task":ID,"generation":1,"token":TOKEN});
    claim_proof_digest_round_trip: ClaimProof => claim_proof();
    landing_proof_digest_round_trip: LandingProof => landing_proof();
    handoff_digest_round_trip: SessionHandoff => handoff();
    attachment_digest_round_trip: Attachment => json!({"name":"note.md","base64":"aGVsbG8="});
    inbox_input_digest_round_trip: InboxInput => json!({"op":"list"});
    local_message_ref_digest_round_trip: MessageRef => json!({"scope":"local","id":ID});
    remote_message_ref_digest_round_trip: MessageRef => json!({"scope":"remote","host":ID,"id":ID});
    local_task_ref_digest_round_trip: TaskRef => json!({"scope":"local","id":ID});
    remote_task_ref_digest_round_trip: TaskRef => json!({"scope":"remote","host":ID,"id":ID});
    issue_scope_digest_round_trip: WorkScope => scope();
    paths_scope_digest_round_trip: WorkScope => json!({"kind":"paths","paths":["/repo/src"]});
    worktree_scope_digest_round_trip: WorkScope => json!({"kind":"worktree","path":"/repo"});
    claim_request_digest_round_trip: ClaimRequest => json!({"id":ID,"project":ID,"actor":ID,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"lease":1000000,"scope":scope()});
    claim_fence_digest_round_trip: ClaimFence => json!({"id":ID,"actor":ID,"generation":1,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]});
    task_fence_digest_round_trip: TaskFence => json!({"task":ID,"actor":ID,"generation":1,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]});
    landing_request_digest_round_trip: LandingRequest => json!({"id":ID,"project":ID,"actor":ID,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"reference":"refs/heads/work","expected":oid(),"candidate":oid(),"lease":1000000});
    push_request_digest_round_trip: PushRequest => push_request();
    push_operation_digest_round_trip: PushOperation => json!({"header":{"schema_version":1},"id":"push","digest":digest(),"request":push_request(),"phase":"prepared","observed_remote":null,"updated_at":"2026-10-29T00:00:00.000000Z"});
}
fn push_request() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"claim":ID,"actor":ID,"generation":1,"reference":"refs/heads/work","expected":oid(),"candidate":oid(),"remote":"origin"})
}
#[test]
fn nested_peer_submit_fields_preserve_the_actual_request_digest() {
    let original: PeerRequest =
        serde_json::from_value(peer(json!({"action":"submit","input":submit()}))).unwrap();
    let mut newer = serde_json::to_value(original).unwrap();
    for pointer in [
        "",
        "/project",
        "/trace",
        "/action",
        "/action/input",
        "/action/input/tasks/0",
        "/action/input/tasks/0/route",
        "/action/input/tasks/0/deliver",
        "/action/input/tasks/0/new_worktree",
    ] {
        newer.pointer_mut(pointer).unwrap()["future"] = json!({"token":u64::MAX,"detail":null});
    }
    let expected = CanonicalV1::record_digest("fabric-peer", 1, &newer).unwrap();
    let decoded: PeerRequest = serde_json::from_value(newer.clone()).unwrap();
    assert_eq!(decoded.digest().unwrap(), expected);
    assert_round_trip::<PeerRequest>(newer);
}
#[test]
fn nested_peer_mail_fields_preserve_the_actual_request_digest() {
    let original: PeerRequest = serde_json::from_value(peer(json!({"action":"coordination","input":{"domain":"mail","input":mail()},"operation_id":"send"}))).unwrap();
    let mut newer = serde_json::to_value(original).unwrap();
    for pointer in [
        "/action",
        "/action/input",
        "/action/input/input",
        "/action/input/input/reply_to",
        "/action/input/input/task",
        "/action/input/input/artifacts/0",
        "/action/input/input/artifacts/0/path",
    ] {
        newer.pointer_mut(pointer).unwrap()["future"] = json!({"token":u64::MAX});
    }
    assert_round_trip::<PeerRequest>(newer);
}
