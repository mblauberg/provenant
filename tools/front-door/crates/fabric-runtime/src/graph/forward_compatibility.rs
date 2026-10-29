//! Newer writers may add fields without invalidating an older reader's digests.
use super::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn stored() -> Value {
    serde_json::from_str(include_str!("../fixtures/legacy-graph-checkpoint.json")).unwrap()
}

fn preserves_digest<T: Serialize + DeserializeOwned>(seed: Value) {
    let record: T = serde_json::from_value(seed).unwrap();
    let mut newer = serde_json::to_value(record).unwrap();
    newer["future"] = json!({"nested": [null, true, {"integer": u64::MAX}]});
    let expected = CanonicalV1::record_digest("forward-compatibility", 1, &newer).unwrap();
    let decoded: T = serde_json::from_value(newer.clone()).unwrap();
    assert_eq!(
        CanonicalV1::bytes(&decoded).unwrap(),
        CanonicalV1::bytes(&newer).unwrap()
    );
    assert_eq!(
        CanonicalV1::record_digest("forward-compatibility", 1, &decoded).unwrap(),
        expected
    );
}

macro_rules! record_cases {
    ($($name:ident: $ty:ty => $seed:expr;)+) => {$(
        #[test]
        fn $name() { preserves_digest::<$ty>($seed); }
    )+};
}

record_cases! {
    document_digest_round_trip: GraphDocument => stored()["execution"]["document"].clone();
    route_digest_round_trip: GraphRoute => json!({"alias":"senior"});
    execution_digest_round_trip: GraphExecution => stored()["execution"].clone();
    node_digest_round_trip: GraphNode => stored()["execution"]["document"]["nodes"][0].clone();
    condition_verdict_digest_round_trip: GateCondition => json!({"type":"verdict","expected":"PASS","threshold":"all"});
    condition_budget_digest_round_trip: GateCondition => json!({"type":"budget"});
    condition_ci_digest_round_trip: GateCondition => json!({"type":"ci","target":"main"});
    ci_observation_digest_round_trip: CiObservation => json!({"state":"passed","summary":"green"});
    ci_target_digest_round_trip: CiTarget => json!({"node":"write"});
    ci_failed_check_digest_round_trip: CiFailedCheck => json!({"name":"build","url":"https://ci/1","excerpt":"failed"});
    ci_stamp_digest_round_trip: CiHostStamp => json!({"boot_identity":"boot","host_monotonic_micros":1});
    ci_evidence_digest_round_trip: CiEvidence => json!({"state":"passed","summary":"green"});
    gate_result_digest_round_trip: GateResult => json!({"passed":true,"reason":null,"on_fail":null});
    input_digest_round_trip: GraphInput => json!({"node":"reader","outputs":["result"]});
    outputs_digest_round_trip: NodeOutputs => json!({"schema_version":2,"result":null,"verdict":null,"findings":[],"branch":null,"commit":null,"warnings":[]});
    result_digest_round_trip: ResultOutput => json!({"excerpt":"done","path":null,"digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","truncated":false});
    finding_digest_round_trip: Finding => json!({"id":"finding","severity":"warning","file":"src/lib.rs","line":null,"summary":"note","status":"open"});
    checkpoint_digest_round_trip: GraphCheckpoint => stored()["checkpoint"].clone();
    progress_digest_round_trip: NodeProgress => stored()["checkpoint"]["nodes"][0].clone();
    attempt_digest_round_trip: GraphAttempt => stored()["checkpoint"]["nodes"][0]["attempts"][0].clone();
    usage_digest_round_trip: AttemptUsage => json!({});
    usage_event_digest_round_trip: UnknownUsageEvent => json!({"node_id":"reader","route_index":0,"metric":"tokens"});
    attempt_key_digest_round_trip: AttemptKey => stored()["checkpoint"]["nodes"][0]["attempts"][0]["key"].clone();
    envelope_digest_round_trip: StoredCheckpoint => stored();
    loop_spec_digest_round_trip: LoopSpec => json!({"body":[],"until":"gate","max_rounds":1});
    node_ask_digest_round_trip: NodeAsk => json!({"ask_id":"r.rounds-1","kind":"loop","node":"r","rounds":1,"session":"chair","title":"t","body":"b","options":[{"id":"stop","label":"Stop"}],"resolved":true,"closed":false,"answer":{"source":"answered","option":"stop","actor":"chair"}});
    decide_spec_digest_round_trip: DecideSpec => json!({"question":"q","options":[{"id":"a","label":"A"},{"id":"b","label":"B"}]});
    loop_round_digest_round_trip: LoopRound => json!({"loop_id":"loop","round":1,"node_id":"reader","until":false,"previous_gate":null});
    plan_spec_digest_round_trip: PlanSpec => json!({"kinds":["task"],"max_nodes":1});
    expansion_digest_round_trip: Expansion => json!({"source":"planner","continuation":false,"nodes":[]});
    remaining_digest_round_trip: RemainingBudgets => json!({"nodes":1,"edges":1,"continuations":1});
}

#[test]
fn normalised_plan_retains_document_and_node_extras() {
    let mut value = stored()["execution"]["document"].clone();
    value["future_document"] = json!({"token":u64::MAX});
    value["nodes"][0]["future_node"] = json!([null, true]);
    let document: GraphDocument = serde_json::from_value(value).unwrap();
    let (plan, _) = validate_plan_with_limits(&document, &Default::default()).unwrap();
    let encoded = serde_json::to_value(&plan).unwrap();
    assert_eq!(encoded["future_document"], json!({"token":u64::MAX}));
    assert_eq!(encoded["nodes"][0]["future_node"], json!([null, true]));
    let plain: GraphDocument =
        serde_json::from_value(stored()["execution"]["document"].clone()).unwrap();
    let (plain_plan, _) = validate_plan_with_limits(&plain, &Default::default()).unwrap();
    assert_ne!(
        CanonicalV1::record_digest("fabric-graph-plan", 1, &plan).unwrap(),
        CanonicalV1::record_digest("fabric-graph-plan", 1, &plain_plan).unwrap()
    );
}

#[test]
fn journal_load_and_save_preserve_newer_checkpoint_digests() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let directory = Directory::open_anchor(temp.path()).unwrap();
    let mut value = stored();
    value["future_envelope"] = json!({"integer":u64::MAX});
    for pointer in [
        "/checkpoint",
        "/checkpoint/nodes/0",
        "/checkpoint/nodes/0/attempts/0",
        "/checkpoint/nodes/0/attempts/0/key",
        "/checkpoint/nodes/0/attempts/0/route",
        "/execution",
        "/execution/document",
        "/execution/document/nodes/0",
        "/execution/document/nodes/0/route",
    ] {
        value.pointer_mut(pointer).unwrap()["future"] = json!({"nested":[null, u64::MAX]});
    }
    // Hash the newer writer's untyped bytes, before the older reader sees them.
    let digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&value["checkpoint"], &value["execution"]),
    )
    .unwrap();
    value["digest"] = json!(digest.to_string());
    let bytes = CanonicalV1::bytes(&value).unwrap();
    std::fs::write(temp.path().join("graph-g-test.json"), bytes.as_bytes()).unwrap();
    let loaded = load_checkpoint(&directory, "g-test").unwrap().unwrap();
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(1).unwrap(),
        provenant_core::MonoTime::from_micros(1),
    );
    let cancel = CancelToken::new();
    let budget = WaitBudget::cli(clock.mono_now(), 5).unwrap();
    let lock = directory
        .lock(
            OsStr::new("graph-g-test.lock"),
            LockMode::Exclusive,
            budget,
            &cancel,
            &clock,
        )
        .unwrap();
    let mut journal = FileJournal {
        extra: loaded.extra,
        graph_id: "g-test".into(),
        directory,
        execution: Some(loaded.execution),
        _lock: lock,
    };
    journal.save(&loaded.checkpoint).unwrap();
    let reloaded = load_checkpoint(&journal.directory, "g-test")
        .unwrap()
        .unwrap();
    assert_eq!(reloaded.digest, digest.to_string());
    assert_eq!(CanonicalV1::bytes(&reloaded).unwrap(), bytes);
}

#[test]
fn newer_checkpoint_envelope_and_payload_versions_are_not_corruption() {
    let temp = crate::test_support::tempdir().unwrap();
    let directory = Directory::open_anchor(temp.path()).unwrap();
    for pointer in [
        "/schema_version",
        "/checkpoint/schema_version",
        "/execution/document/schema_version",
    ] {
        let mut value = stored();
        *value.pointer_mut(pointer).unwrap() = json!(2);
        std::fs::write(
            temp.path().join("graph-g-test.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        assert!(
            matches!(
                load_checkpoint(&directory, "g-test"),
                Err(JournalError::Decode(DecodeError::UnsupportedVersion {
                    found: 2,
                    ..
                }))
            ),
            "{pointer}"
        );
    }
}
