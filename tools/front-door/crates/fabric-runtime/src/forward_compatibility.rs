//! Stored requests and replies retain unknown fields through nested wire shapes.
use crate::{
    coordination::{facade::*, *},
    handoff::*,
    hosts::*,
    inbox::{InboxInput, InboxReply, InboxResult},
    runs::{AttemptSummary, RunResult, SubmissionReceipt},
};
use provenant_fabric_store::{
    ClaimFence, ClaimRequest, LandingRequest, MessageRef, PushOperation, TaskFence, TaskRef,
    WorkScope,
};
use provenant_records::CanonicalV1;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

const ID: &str = "11111111111111111111111111111111";
const TOKEN: &str = "00000000000000000000000000000000";
const AT: &str = "2026-10-29T00:00:00.000000Z";
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

fn round_trip_at<T: Serialize + DeserializeOwned>(seed: Value, pointers: &[&str]) {
    let baseline: T = serde_json::from_value(seed)
        .unwrap_or_else(|error| panic!("{} seed: {error}", std::any::type_name::<T>()));
    let mut newer = serde_json::to_value(baseline).unwrap();
    for pointer in pointers {
        newer.pointer_mut(pointer).unwrap()["future"] = json!({"nested":[null,u64::MAX,true]});
    }
    assert_round_trip::<T>(newer);
}
fn assert_round_trip<T: Serialize + DeserializeOwned>(newer: Value) {
    let digest = CanonicalV1::record_digest("fabric-peer", 1, &newer).unwrap();
    let decoded: T = serde_json::from_value(newer.clone()).unwrap();
    assert_eq!(
        CanonicalV1::bytes(&decoded).unwrap(),
        CanonicalV1::bytes(&newer).unwrap(),
        "{} lost an unknown field",
        std::any::type_name::<T>()
    );
    assert_eq!(
        CanonicalV1::record_digest("fabric-peer", 1, &decoded).unwrap(),
        digest
    );
}
macro_rules! cases {
    ($($ty:ty => $seed:expr, $pointers:expr;)+) => {$(
        round_trip_at::<$ty>($seed, &$pointers);
    )+};
}

#[test]
fn request_shapes_preserve_unknown_fields() {
    cases! {
        PeerRequest => peer(json!({"action":"health"})), ["", "/project", "/trace", "/action"];
        TaskInput => json!({"op":"create","objective":"work","note":null}), [""];
        EventsInput => json!({"op":"note","note":"work","dedup_key":null}), [""];
        ClaimInput => json!({"op":"claim_acquire","scope":scope(),"lease_seconds":1}), ["", "/scope"];
        LandingInput => json!({"op":"landing_verify","proof":landing_proof()}), ["", "/proof"];
        IdentityInput => json!({"op":"inspect"}), [""];
        GraphInput => json!({"op":"run","document":"graph.json"}), [""];
        RunInput => json!({"op":"status","target":target()}), ["", "/target"];
        OutputInput => json!({"op":"read","target":target(),"part":"result","offset":0,"max_bytes":1024}), ["", "/target"];
        HostsInput => json!({"op":"hosts","limit":10}), [""];
        SessionHandoff => handoff(), ["", "/attachments/0"];
        InboxInput => json!({"op":"list"}), [""];
    }
}

#[test]
fn reference_and_store_request_shapes_preserve_unknown_fields() {
    cases! {
        TaskProof => json!({"task":ID,"generation":1,"token":TOKEN}), [""];
        ClaimProof => claim_proof(), [""];
        MessageRef => json!({"scope":"local","id":ID}), [""];
        TaskRef => json!({"scope":"remote","host":ID,"id":ID}), [""];
        WorkScope => json!({"kind":"paths","paths":["/repo/src"]}), [""];
        WorkScope => json!({"kind":"worktree","path":"/repo"}), [""];
        ClaimRequest => json!({"id":ID,"project":ID,"actor":ID,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"lease":1000000,"scope":scope()}), ["", "/scope"];
        ClaimFence => json!({"id":ID,"actor":ID,"generation":1,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}), [""];
        TaskFence => json!({"task":ID,"actor":ID,"generation":1,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}), [""];
        LandingRequest => json!({"id":ID,"project":ID,"actor":ID,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"reference":"refs/heads/work","expected":oid(),"candidate":oid(),"lease":1000000}), [""];
        PushOperation => json!({"header":{"schema_version":1},"id":"push","digest":digest(),"request":push_request(),"phase":"prepared","observed_remote":null,"updated_at":AT}), ["", "/header", "/request", "/request/header"];
    }
}
fn push_request() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"claim":ID,"actor":ID,"generation":1,"reference":"refs/heads/work","expected":oid(),"candidate":oid(),"remote":"origin"})
}

fn push_operation() -> Value {
    json!({"header":{"schema_version":1},"id":"push","digest":digest(),"request":push_request(),"phase":"prepared","observed_remote":null,"updated_at":AT})
}
fn stored_task() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"id":ID,"objective":"work","state":"open","generation":0,"created_at":AT,"updated_at":AT})
}
fn stored_claim() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"id":ID,"scope":scope(),"generation":1,"updated_at":AT})
}
fn stored_landing() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"id":ID,"reference":"refs/heads/work","expected":oid(),"candidate":oid(),"generation":1,"updated_at":AT})
}
fn stored_message() -> Value {
    json!({"header":{"schema_version":1},"project":ID,"id":ID,"sender":ID,"recipients":[ID],"kind":"note","body":"hello","artifacts":[],"created_at":AT})
}
fn stored_event() -> Value {
    json!({"seq":1,"record":{"header":{"schema_version":1},"project":ID,"kind":"note","detail":"work","at":AT}})
}
fn summary() -> Value {
    json!({"attempt_id":ID,"run_id":ID,"task_id":ID,"operation_id":"submit","state":{"state":"queued"},"requested_route":{"kind":"named","name":"senior"},"queued_at":AT})
}
fn attempt() -> Value {
    json!({"schema_version":1,"attempt_id":ID,"run_id":ID,"project_id":ID,"task_id":ID,"operation_id":"submit","attempt_number":1,"revision":1,"generation":1,
        "requested_route":{"kind":"named","name":"senior"},"authority_digest":digest(),"queued_at":AT,"provider_seconds":null,"state":{"state":"queued"},"usage":{},
        "prompt":{"path":{"encoding":"utf8","path":"prompt"},"digest":digest(),"bytes":0,"complete":true,"redacted":false}})
}
fn submitted() -> Value {
    json!({"operation_id":"submit","targets":[target()],"worktree":{"path":"/repo/work","branch":"work"}})
}
fn output() -> Value {
    json!({"kind":"chunk","target":target(),"part":"result","state":"queued","offset":0,"next_offset":0,"eof":true,"digest":digest(),"bytes":{"encoding":"utf8","data":""}})
}
fn saved_inbox() -> Value {
    json!({"result":"saved","operation_id":"request","request_id":ID,"revision":1,"state":"open"})
}
fn human_request() -> Value {
    json!({"schema_version":1,"id":ID,"revision":1,"operation_id":"request","created_at":AT,"updated_at":AT,"project_id":ID,"owner_host":ID,"chair_session_id":"chair","chair_actor":ID,
        "request":{"kind":"question","title":"Continue?","body":"Choose what to do"},"state":"open"})
}

#[test]
fn attempt_summary_conversion_keeps_record_extensions() {
    let mut stored = attempt();
    stored.as_object_mut().unwrap().remove("provider_seconds");
    stored["provider_ms"] = Value::Null;
    stored["future"] = json!({"nested":true});
    let record: provenant_records::run::AttemptRecord = serde_json::from_value(stored).unwrap();
    let summary = AttemptSummary::from(record);
    assert_eq!(summary.extra["future"], json!({"nested":true}));
}

#[test]
fn run_reply_shapes_preserve_unknown_fields() {
    cases! {
        SubmissionReceipt => submitted(), ["", "/targets/0", "/worktree"];
        AttemptSummary => summary(), ["", "/state", "/requested_route"];
        RunResult => json!({"result":"list","page":{"items":[summary()],"truncated":false}}), ["", "/page", "/page/items/0", "/page/items/0/state", "/page/items/0/requested_route"];
        RunResult => json!({"result":"wait","condition":"all","attempts":[attempt()],"satisfied":false,"timed_out":true}), ["", "/attempts/0", "/attempts/0/state", "/attempts/0/usage", "/attempts/0/prompt"];
        OutputResult => output(), ["", "/target"];
        OutputResult => json!({"kind":"artifact","target":target(),"artifact":{"path":{"encoding":"utf8","path":"out"},"digest":null},"digest":digest(),"bytes":{"encoding":"base64","data":""}}), ["", "/target", "/artifact", "/artifact/path"];
    }
    for result in ["status", "cancel", "resume"] {
        round_trip_at::<RunResult>(
            json!({"result":result,"attempt":attempt()}),
            &["", "/attempt", "/attempt/state", "/attempt/requested_route"],
        );
    }
}

#[test]
fn coordination_reply_shapes_preserve_unknown_fields() {
    cases! {
        MailResult => json!({"result":"sent","id":ID}), [""];
        MailResult => json!({"result":"marked_read","cursor":1}), [""];
        MailResult => json!({"result":"read","page":{"items":[{"seq":1,"message":stored_message()}],"truncated":false},"cursor":1}), ["", "/page", "/page/items/0", "/page/items/0/message", "/page/items/0/message/header"];
        TaskResult => json!({"result":"listed","page":{"items":[stored_task()],"truncated":false}}), ["", "/page", "/page/items/0"];
        TaskResult => json!({"result":"claimed","proof":{"task":ID,"generation":1,"token":TOKEN}}), ["", "/proof"];
        EventsResult => json!({"result":"read","page":{"items":[stored_event()],"truncated":false},"cursor":1,"timed_out":false}), ["", "/page", "/page/items/0", "/page/items/0/record", "/page/items/0/record/header"];
        EventsResult => json!({"result":"noted","seq":1}), [""];
        ClaimResult => json!({"result":"acquired","proof":claim_proof(),"claim":stored_claim()}), ["", "/proof", "/claim", "/claim/scope"];
        ClaimResult => json!({"result":"released","id":ID}), [""];
        ClaimResult => json!({"result":"listed","page":{"items":[stored_claim()],"truncated":false}}), ["", "/page", "/page/items/0"];
        LandingResult => json!({"result":"acquired","proof":landing_proof(),"landing":stored_landing()}), ["", "/proof", "/landing", "/landing/header"];
        LandingResult => json!({"result":"released","id":ID,"reference":"refs/heads/work"}), [""];
        LandingResult => json!({"result":"waited","landing":stored_landing(),"timed_out":false}), ["", "/landing"];
    }
    for result in ["created", "updated", "finished", "released"] {
        round_trip_at::<TaskResult>(
            json!({"result":result,"task":stored_task()}),
            &["", "/task"],
        );
    }
    for result in ["renewed", "verified"] {
        round_trip_at::<ClaimResult>(
            json!({"result":result,"claim":stored_claim()}),
            &["", "/claim"],
        );
        round_trip_at::<LandingResult>(
            json!({"result":result,"landing":stored_landing()}),
            &["", "/landing"],
        );
    }
    for (domain, result) in [
        ("mail", json!({"result":"sent","id":ID})),
        ("task", json!({"result":"created","task":stored_task()})),
        ("events", json!({"result":"noted","seq":1})),
        ("claim", json!({"result":"released","id":ID})),
        (
            "landing",
            json!({"result":"released","id":ID,"reference":"refs/heads/work"}),
        ),
    ] {
        round_trip_at::<CoordinationResult>(
            json!({"domain":domain,"result":result}),
            &["", "/result"],
        );
    }
}

#[test]
fn inbox_and_peer_reply_envelopes_preserve_unknown_fields() {
    cases! {
        InboxResult => saved_inbox(), [""];
        InboxResult => json!({"result":"listed","items":[human_request()],"truncated":false,"coverage":"local"}), ["", "/items/0", "/items/0/request"];
        InboxResult => json!({"result":"item","item":{"request":human_request(),"history":[human_request()]}}), ["", "/item", "/item/request", "/item/history/0"];
        InboxReply => json!({"status":"ok","result":saved_inbox()}), ["", "/result"];
        InboxReply => json!({"status":"error","code":"conflict","message":"retry"}), [""];
    }
    // Each result variant has its own flattened map. Exercise object, array,
    // nested tagged and arbitrary JSON payloads through the outer response.
    for (result, data, nested) in [
        (
            "health",
            json!({"host":ID,"available":true,"execution":false}),
            Some("/data"),
        ),
        ("submitted", submitted(), Some("/data")),
        (
            "run",
            json!({"result":"status","attempt":attempt()}),
            Some("/data/attempt"),
        ),
        ("run_statuses", json!([attempt()]), Some("/data/0")),
        (
            "lane_waits",
            json!([{"attempt_id":ID,"lane_class":"read_only","waiting_for":"capacity"}]),
            Some("/data/0"),
        ),
        ("output", output(), Some("/data")),
        (
            "run_events",
            json!({"items":[],"cursor":0,"truncated":false}),
            Some("/data"),
        ),
        ("push", push_operation(), Some("/data/request")),
        (
            "inbox",
            json!({"status":"ok","result":saved_inbox()}),
            Some("/data/result"),
        ),
        (
            "coordination",
            json!({"domain":"mail","result":{"result":"sent","id":ID}}),
            Some("/data/result"),
        ),
        ("console", json!({"freeform":true}), Some("/data")),
        (
            "session_handoff",
            json!({"outcome":"refused","code":"account","message":"choose account","accounts":[]}),
            Some("/data"),
        ),
        (
            "session_handoff",
            json!({"outcome":"delivered","prompt_path":"/repo/prompt","project":"/repo","worktrees":[{"branch":"work","created":true}],"attachments":[],"launched":false}),
            Some("/data/worktrees/0"),
        ),
        (
            "error",
            json!({"error":{"code":"not_found"}}),
            Some("/data"),
        ),
    ] {
        let seed = json!({"result":result,"data":data});
        let mut pointers = vec![""];
        if seed["data"].is_object() {
            pointers.push("/data");
        }
        if let Some(nested) = nested {
            pointers.push(nested);
        }
        round_trip_at::<PeerResult>(seed.clone(), &pointers);
        let mut pointers: Vec<_> = pointers
            .iter()
            .map(|path| format!("/result{path}"))
            .collect();
        pointers.push(String::new());
        round_trip_at::<PeerResponse>(
            json!({"version":1,"protocol":PROTOCOL,"host":ID,"project":"project","result":seed}),
            &pointers.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }
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
