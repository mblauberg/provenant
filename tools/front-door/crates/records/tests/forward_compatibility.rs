//! Newer record fields must survive canonical byte and digest round trips.
use provenant_records::{inbox, run, workflow, *};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const ID: &str = "11111111111111111111111111111111";
const NOW: &str = "2026-10-29T00:00:00.000000Z";
const GIT_OID: &str = "0123456789abcdef0123456789abcdef01234567";

fn digest() -> String {
    format!("sha256:{}", "ab".repeat(32))
}
fn path() -> Value {
    json!({"encoding":"utf8","path":"result.md"})
}
fn absolute_path() -> Value {
    json!({"relative_to_root":path()})
}
fn normalise<T: DeserializeOwned + Serialize>(seed: Value) -> Result<Value, serde_json::Error> {
    serde_json::to_value(serde_json::from_value::<T>(seed)?)
}

/// Change every original object, including tagged variants and transparent containers.
fn add_future_fields(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for child in fields.values_mut() {
                add_future_fields(child);
            }
            fields.insert(
                "future_object".into(),
                json!({"writer":"next","values":[1,null,{"ok":true}]}),
            );
            fields.insert("future_integer".into(), json!(u64::MAX));
            fields.insert("future_null".into(), Value::Null);
        }
        Value::Array(items) => {
            for item in items {
                add_future_fields(item);
            }
        }
        _ => {}
    }
}

fn assert_preserved<T: DeserializeOwned + Serialize>(kind: &str, value: Value) -> TestResult {
    let before = CanonicalV1::bytes(&value)?;
    let digest_before = CanonicalV1::record_digest(kind, 1, &value)?;
    let decoded: T = serde_json::from_value(value.clone())?;
    let after = CanonicalV1::bytes(&decoded)?;
    assert_eq!(before.as_bytes(), after.as_bytes(), "{kind}");
    assert_eq!(
        digest_before,
        CanonicalV1::record_digest(kind, 1, &decoded)?,
        "{kind}"
    );
    assert_eq!(serde_json::to_value(decoded)?, value, "{kind}");
    Ok(())
}

fn round_trip<T: DeserializeOwned + Serialize>(kind: &str, seed: Value) -> TestResult {
    let mut value = normalise::<T>(seed)?;
    add_future_fields(&mut value);
    assert_preserved::<T>(kind, value)
}

macro_rules! cases {
    ($($name:ident: $type:ty => $seed:expr;)*) => {
        $(#[test] fn $name() -> TestResult { round_trip::<$type>(stringify!($name), $seed) })*
    };
}

fn profile() -> Value {
    json!({"name":"rigorous","knobs":{},"overrides":[]})
}
fn requested_route() -> Value {
    json!({"kind":"named","name":"senior","effort":null})
}
fn writer_delivery_request() -> Value {
    json!({"remote":"origin","branch":"work","open_pr":true})
}
fn task() -> Value {
    json!({"prompt_digest":digest(),"route":requested_route(),
        "fallback":[route_record()],"authority_digest":digest(),
        "provider_timeout_seconds":60,"delivery_id":null,
        "writer":{"project":"/repo","worktree":"/repo/work"},"deliver":writer_delivery_request()})
}
fn request() -> Value {
    json!({"schema_version":1,"task":task(),"parent":null,
        "chair":{"host":ID,"actor":ID}})
}
fn readiness() -> Value {
    json!({"status":"ready","availability_ref":"availability","expires_unix_seconds":10})
}
fn confidentiality() -> Value {
    json!({"evidence_ref":"policy"})
}
fn candidate() -> Value {
    json!({"model":"codex/model","weight":1,"factor_basis_points":10000,
        "effort":null,"readiness":readiness(),"exclusion":null})
}
fn redraw() -> Value {
    json!({"rejected_pick":"codex/other","roll":0,"total_weight":1,"reason":"stale"})
}
fn selection() -> Value {
    json!({"pool":"senior","candidates":[candidate()],"roll":0,"total_weight":1,
        "pick":"codex/model","redraws":[redraw()]})
}
fn alternative() -> Value {
    json!({"adapter":"codex","model":"model","effort":null,"account_key":null,
        "host":"local","eligibility":"eligible","readiness":readiness(),"confidentiality":confidentiality()})
}
fn exclusion() -> Value {
    json!({"adapter":"codex","model":"other","reason":"disabled"})
}
fn resolved_route() -> Value {
    let mut value = alternative();
    value["catalogue_revision"] = json!("revision");
    value["alternatives"] = json!([alternative()]);
    value["exclusions"] = json!([exclusion()]);
    value["pool_selection"] = selection();
    value
}
fn route_record() -> Value {
    json!({"provider":"codex","model":"model","effort":null,
        "account_label":null,"pool_selection":selection()})
}
fn terminal_state() -> Value {
    json!({"state":"terminal","outcome":"succeeded","exit_code":0,"finished_at":NOW,"reason":null})
}
fn artifact() -> Value {
    json!({"path":path(),"digest":digest(),"bytes":1,"complete":true,"redacted":false})
}
fn write_grant() -> Value {
    json!({"source":"write_root","path":absolute_path(),"scope":"subtree"})
}
fn review_acceptance() -> Value {
    json!({"actor":ID,"protected_paths":[path()]})
}
fn writer_delivery_record() -> Value {
    json!({"schema_version":1,"remote":"origin","branch":"work","head":null,
        "review_acceptance":review_acceptance(),"state":{"state":"delivered","pull_request":null}})
}
fn writer_integration() -> Value {
    json!({"remote":"upstream","push_url":"https://example.test/repo.git",
        "target":"refs/heads/work","previous":null,"integrated":GIT_OID})
}
fn integrated_delivery_record() -> Value {
    json!({"schema_version":1,"remote":"upstream","branch":"work","head":GIT_OID,
        "state":{"state":"delivered","pull_request":null},"integration":writer_integration()})
}
fn skipped_route() -> Value {
    json!({"route":"codex/other","kind":"unavailable","detail":"unavailable"})
}
fn attempt() -> Value {
    json!({"schema_version":1,"attempt_id":ID,"project_id":ID,
        "key":{"project":ID,"graph":"g","node":"n","attempt":1},"attempt_number":1,"revision":1,"generation":1,
        "requested_route":requested_route(),"resolved_route":route_record(),
        "applied_route":route_record(),"observed_route":route_record(),"authority_digest":digest(),
        "write_grants":[write_grant()],"protected_paths":[path()],"delivery":writer_delivery_record(),
        "queued_at":NOW,"state":terminal_state(),"usage":{},"prompt":artifact(),"result":artifact(),
        "exit_evidence":artifact(),"skipped_routes":[skipped_route()],"trace":{"host":ID,"attempt":ID}})
}
fn evidence() -> Value {
    json!({"path":path(),"digest":digest()})
}
fn approval() -> Value {
    json!({"actor_id":ID,"evidence":evidence()})
}
fn risk_factors() -> Value {
    json!({"blast_radius":"local","reversibility":"easy","data_sensitivity":"public",
        "migration":"none","oracle_quality":"strong","external_effects":"none","critical_surface":"none"})
}
fn risk() -> Value {
    json!({"factors":risk_factors(),"derived":"routine","tier":"routine","downgrade_approval":null})
}
fn subject() -> Value {
    json!({"kind":"document","path":path(),"digest":digest()})
}
fn observed_provenance() -> Value {
    json!({"source":"observed","route":"codex/model@high","tier":"principal","adapter":"codex",
        "endpoint_provider":null,"model_family":null,"evidence":evidence()})
}
fn accepted_decision() -> Value {
    json!({"status":"accepted","actor_id":ID,"at":10,"reason":"done","user_approval":approval()})
}
fn receipt() -> Value {
    json!({"schema_version":2,"delivery_id":ID,"project_id":ID,"revision":1,"subject":subject(),
        "authors":[{"kind":"agent","actor_id":ID,"route":observed_provenance()}],"risk":risk(),
        "review_attempts":[],"decision":accepted_decision(),"created_at":10,"updated_at":10})
}
fn request_work() -> Value {
    json!({"graph":"g","node":null,"repository":absolute_path(),"head":null})
}
fn request_option() -> Value {
    json!({"id":"continue","label":"Continue","description":"Proceed"})
}
fn replacement() -> Value {
    json!({"id":ID,"expect_revision":1})
}
fn request_draft() -> Value {
    json!({"supersedes":replacement(),"deadline":NOW,"work":request_work(),"kind":"question",
        "title":"Decision","body":"Proceed?","blocking":true,"options":[request_option()],
        "recommended_option_id":"continue","proceeding_with":null})
}
fn human_request() -> Value {
    json!({"schema_version":1,"id":ID,"revision":1,"operation_id":"operation",
        "created_at":NOW,"updated_at":NOW,"project_id":ID,"owner_host":ID,
        "chair_session_id":"session","chair_actor":ID,"request":request_draft(),
        "state":"open","answer_id":null,"closure_reason":null})
}
fn human_answer() -> Value {
    json!({"schema_version":1,"id":ID,"request_id":ID,"request_revision":1,"action":"reply",
        "text":"Yes","actor":ID,"source_host":ID,"channel":"cli","answered_at":NOW,"operation_id":"operation"})
}
fn answer_delivery() -> Value {
    json!({"schema_version":1,"answer_id":ID,"target_host":ID,"target_session":"session",
        "target_actor":ID,"transport":"fabric_mail","message_id":ID,"state":"queued","attempts":0})
}
fn release_file() -> Value {
    json!({"path":path(),"digest":digest(),"bytes":1,"mode":384})
}
fn source_digest() -> String {
    format!("sha256:{}", "cd".repeat(32))
}
fn release_manifest() -> Value {
    json!({"schema_version":1,"source_digest":source_digest(),"asset_digest":source_digest(),
        "target":"target","toolchain":"toolchain","files":[release_file()]})
}
fn owned_entry() -> Value {
    json!({"path":absolute_path(),"harness":"codex","selector":"owned",
        "expected":{"kind":"file","digest":digest(),"mode":384}})
}
fn harness_target() -> Value {
    json!({"harness":"codex","skills_dir":absolute_path(),"mcp_file":absolute_path()})
}
fn install_receipt() -> Value {
    json!({"schema_version":1,"state_format_version":1,"source_digest":source_digest(),"manifest_digest":digest(),
        "asset_digest":source_digest(),"target":"target","toolchain":"toolchain","bin_dir":absolute_path(),
        "harnesses":["codex"],"harness_targets":[harness_target()],"entries":[owned_entry()]})
}

cases! {
    work_context_preserves_digest: run::WorkContext => json!({"work_item":"issue-2268","role":"reviewer"});
    chair_binding_preserves_digest: run::ChairBinding => json!({"host":ID,"actor":ID});
    canonical_request_preserves_digest: run::CanonicalRequest => request();
    harness_profile_preserves_digest: run::HarnessProfile => profile();
    task_request_preserves_digest: run::TaskRequest => task();
    lane_access_preserves_digest: run::LaneAccess => json!({});
    writer_request_preserves_digest: run::WriterRequest => json!({"project":"/repo","worktree":"/repo/work"});
    writer_delivery_request_preserves_digest: run::WriterDeliveryRequest => writer_delivery_request();
    writer_review_acceptance_preserves_digest: run::WriterReviewAcceptance => review_acceptance();
    writer_delivery_record_preserves_digest: run::WriterDeliveryReceipt => writer_delivery_record();
    integrated_delivery_record_preserves_digest: run::WriterDeliveryReceipt => integrated_delivery_record();
    writer_integration_preserves_digest: run::WriterIntegration => writer_integration();
    delivered_state_preserves_digest: run::WriterDeliveryState => json!({"state":"delivered","pull_request":"https://example.test/pr/1"});
    no_new_commits_state_preserves_digest: run::WriterDeliveryState => json!({"state":"no_new_commits"});
    not_attempted_state_preserves_digest: run::WriterDeliveryState => json!({"state":"not_attempted"});
    failed_delivery_state_preserves_digest: run::WriterDeliveryState => json!({"state":"failed","stage":"push"});
    requested_model_preserves_digest: run::RequestedRoute => json!({"kind":"model","provider":"codex","model":"model","effort":null});
    requested_named_preserves_digest: run::RequestedRoute => requested_route();
    requested_review_preserves_digest: run::RequestedRoute => json!({"kind":"review","name":"senior","effort":null,"excluded_families":[],"distinct_families":false});
    route_record_preserves_digest: run::RouteRecord => route_record();
    usage_record_preserves_digest: run::UsageRecord => json!({});
    operation_record_preserves_digest: run::OperationRecord => json!({"schema_version":1,"project_id":ID,"actor_id":ID,"operation_id":"operation","kind":"dispatch","request_digest":digest(),"state":"accepted"});
    refused_operation_preserves_digest: run::OperationRecord => json!({"schema_version":1,"project_id":ID,"actor_id":ID,"operation_id":"operation","kind":"dispatch","request_digest":digest(),"state":"refused","refusal":{"code":"REFUSED","detail":"not admitted"},"worktree":"lane-branch","refused_at":NOW,"refusal_chair":{"host":ID,"actor":ID}});
    queued_state_preserves_digest: run::AttemptState => json!({"state":"queued"});
    starting_state_preserves_digest: run::AttemptState => json!({"state":"starting"});
    running_state_preserves_digest: run::AttemptState => json!({"state":"running"});
    settling_state_preserves_digest: run::AttemptState => json!({"state":"settling","publication_pending":true});
    terminal_state_preserves_digest: run::AttemptState => terminal_state();
    skipped_route_preserves_digest: run::SkippedRoute => skipped_route();
    artifact_record_preserves_digest: run::ArtifactRecord => artifact();
    write_grant_record_preserves_digest: run::WriteGrantRecord => write_grant();
    lane_wait_preserves_digest: run::LaneWait => json!({"attempt_id":ID,"lane_class":"writer","waiting_for":"capacity"});
    admission_ticket_preserves_digest: run::AdmissionTicket => json!({"schema_version":1,"operation_id":"operation","attempt_id":ID,"project_id":ID,"sequence":1,"generation":1,"active":true,"memory_floor_basis_points":1000,"disk_floor_bytes":1,"startup_spacing_ms":1,"run_concurrency":1,"admission_deadline":NOW});
    run_event_preserves_digest: run::RunEvent => json!({"schema_version":1,"sequence":1,"project_id":ID,"attempt_id":ID,"revision":1,"change":terminal_state(),"trace":{}});

    resolved_route_preserves_digest: ResolvedRoute => resolved_route();
    route_selection_receipt_preserves_digest: RouteSelectionReceipt => selection();
    route_selection_redraw_preserves_digest: RouteSelectionRedraw => redraw();
    route_selection_candidate_preserves_digest: RouteSelectionCandidate => candidate();
    route_alternative_preserves_digest: RouteAlternative => alternative();
    route_alternatives_preserve_digest: RouteAlternatives => json!([alternative()]);
    ready_route_preserves_digest: RouteReadiness => readiness();
    unavailable_route_preserves_digest: RouteReadiness => json!({"status":"unavailable","reason":"disabled"});
    route_confidentiality_preserves_digest: RouteConfidentiality => confidentiality();
    route_exclusion_preserves_digest: RouteExclusion => exclusion();

    path_wire_preserves_digest: PathWire => path();
    absolute_path_preserves_digest: AbsolutePath => absolute_path();
    artifact_ref_preserves_digest: ArtifactRef => json!({"path":path(),"digest":digest()});
    trace_preserves_digest: Trace => json!({"host":ID,"run":ID,"attempt":ID});
    record_header_preserves_digest: RecordHeader => json!({"schema_version":1});

    answer_target_preserves_digest: inbox::AnswerTarget => json!({"host":ID,"project":ID,"session":"session","actor":ID});
    answer_offer_preserves_digest: inbox::AnswerOffer => json!({"answer":human_answer(),"delivery":answer_delivery()});
    request_option_preserves_digest: inbox::RequestOption => request_option();
    request_work_preserves_digest: inbox::RequestWork => request_work();
    request_replacement_preserves_digest: inbox::RequestReplacement => replacement();
    request_draft_preserves_digest: inbox::RequestDraft => request_draft();
    human_request_preserves_digest: inbox::HumanRequest => human_request();
    human_answer_preserves_digest: inbox::HumanAnswer => human_answer();
    answer_delivery_preserves_digest: inbox::AnswerDelivery => answer_delivery();
    inbox_item_preserves_digest: inbox::InboxItem => json!({"request":human_request(),"answer":human_answer(),"delivery":answer_delivery(),"history":[human_request()]});

    risk_factors_preserve_digest: workflow::RiskFactors => risk_factors();
    evidence_ref_preserves_digest: workflow::EvidenceRef => evidence();
    approval_ref_preserves_digest: workflow::ApprovalRef => approval();
    risk_record_preserves_digest: workflow::RiskRecord => risk();
    pr_subject_preserves_digest: workflow::Subject => json!({"kind":"pr","repository":ID,"number":1,"head":"abababababababababababababababababababab","base":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"});
    document_subject_preserves_digest: workflow::Subject => subject();
    observed_provenance_preserves_digest: workflow::RouteProvenance => observed_provenance();
    attested_provenance_preserves_digest: workflow::RouteProvenance => json!({"source":"chair_attested","route":"codex/model@high","tier":"principal","declared_family":null,"evidence":evidence()});
    unknown_provenance_preserves_digest: workflow::RouteProvenance => json!({"source":"unknown"});
    user_author_preserves_digest: workflow::Author => json!({"kind":"user","actor_id":ID});
    agent_author_preserves_digest: workflow::Author => json!({"kind":"agent","actor_id":ID,"route":observed_provenance()});
    pending_decision_preserves_digest: workflow::Decision => json!({"status":"pending"});
    accepted_decision_preserves_digest: workflow::Decision => accepted_decision();
    abandoned_decision_preserves_digest: workflow::Decision => json!({"status":"abandoned","actor_id":ID,"at":10,"reason":"closed","user_approval":approval()});
    workflow_receipt_preserves_digest: workflow::DeliveryReceipt => receipt();

    release_file_preserves_digest: ReleaseFile => release_file();
    release_manifest_preserves_digest: ReleaseManifest => release_manifest();
    owned_file_preserves_digest: OwnedValue => json!({"kind":"file","digest":digest(),"mode":384});
    owned_link_preserves_digest: OwnedValue => json!({"kind":"link","target":absolute_path()});
    owned_selector_preserves_digest: OwnedValue => json!({"kind":"selector","value":{"owned":true}});
    owned_entry_preserves_digest: OwnedEntry => owned_entry();
    resolved_harness_target_preserves_digest: ResolvedHarnessTarget => harness_target();
    install_receipt_preserves_digest: InstallReceipt => install_receipt();
    version_info_preserves_digest: VersionInfo => json!({"source_digest":source_digest(),"asset_digest":source_digest(),"target":"target","toolchain":"toolchain","record_schema_version":1,"config_schema_version":1});
}

#[test]
fn attempt_record_preserves_digest() -> TestResult {
    let mut value = normalise::<run::AttemptRecord>(attempt())?;
    // WorkKey has exactly four identity fields; unlike record payloads, it
    // deliberately discards unknown fields (covered by format2_tests).
    let key = value["key"].clone();
    add_future_fields(&mut value);
    value["key"] = key;
    assert_preserved::<run::AttemptRecord>("run-attempt", value)
}

#[test]
fn request_digest_matches_the_newer_writers_canonical_record() -> TestResult {
    let mut value = normalise::<run::CanonicalRequest>(request())?;
    add_future_fields(&mut value);
    let expected = CanonicalV1::record_digest("run-request", 1, &value)?;
    let request: run::CanonicalRequest = serde_json::from_value(value)?;
    assert_eq!(request.digest()?, expected);
    Ok(())
}

#[test]
fn unknown_profile_and_access_fields_survive_parent_skip_rules() -> TestResult {
    let mut task = normalise::<run::TaskRequest>(task())?;
    assert!(task.get("profile").is_none());
    assert!(task.get("access").is_none());
    // Known fields are defaults. Only future fields make the objects non-empty.
    let mut empty_profile = json!({"name":"","knobs":{},"overrides":[]});
    add_future_fields(&mut empty_profile);
    empty_profile["knobs"] = json!({});
    let mut empty_access = json!({});
    add_future_fields(&mut empty_access);
    task["profile"] = empty_profile.clone();
    task["access"] = empty_access;
    assert_preserved::<run::TaskRequest>("run-task-with-future-policy", task.clone())?;
    let mut request = normalise::<run::CanonicalRequest>(request())?;
    request["task"] = task;
    assert_preserved::<run::CanonicalRequest>("run-request", request)?;
    let mut attempt = normalise::<run::AttemptRecord>(attempt())?;
    assert!(attempt.get("profile").is_none());
    attempt["profile"] = empty_profile;
    assert_preserved::<run::AttemptRecord>("run-attempt-with-future-profile", attempt)
}

#[test]
fn route_exclusion_reason_codes_round_trip() -> TestResult {
    // Existing wire names stay readable; the account-evidence codes are additive.
    // Every variant of RouteExclusionReason, so a new wire name must be listed here.
    for code in [
        "setup_failed",
        "account_pinned",
        "admission_refused",
        "disabled",
        "missing_executable",
        "provider_updating",
        "unknown_availability",
        "stale_availability",
        "unknown_account",
        "disclosure_denied",
        "confinement_unavailable",
        "unsupported_effort",
        "exhausted",
        "pacing_reserve",
        "unknown_partial_headroom",
        "auth_required",
        "login_expired",
        "unverified_model",
        "account_mismatch",
        "free_tier_refused",
        "provider_refused",
        "invalid_account_evidence",
    ] {
        let reason: RouteExclusionReason = serde_json::from_value(json!(code))?;
        assert_eq!(serde_json::to_value(&reason)?, json!(code));
        let seed = json!({"status":"unavailable","reason":code});
        assert_preserved::<RouteReadiness>("route_readiness", {
            let mut v = seed.clone();
            add_future_fields(&mut v);
            v
        })?;
    }
    // Unknown readiness projects onto existing wire names: a new status would
    // fail to decode in an older reader.
    for (readiness, reason) in [
        (RouteReadiness::unknown(), "unknown_availability"),
        (RouteReadiness::stale(), "stale_availability"),
    ] {
        assert!(readiness.is_unknown());
        assert_eq!(
            serde_json::to_value(&readiness)?,
            json!({"status":"unavailable","reason":reason})
        );
    }
    assert!(serde_json::from_value::<RouteReadiness>(json!({"status":"unknown"})).is_err());
    assert!(!serde_json::from_value::<RouteReadiness>(readiness())?.is_unknown());
    Ok(())
}

#[test]
fn older_install_receipt_defaults_state_format_version() -> TestResult {
    let old = json!({"schema_version":1,"source_digest":source_digest(),"manifest_digest":digest(),
        "asset_digest":source_digest(),"target":"target","toolchain":"toolchain","bin_dir":absolute_path(),
        "harnesses":["codex"],"harness_targets":[harness_target()],"entries":[owned_entry()]});
    let receipt: InstallReceipt = serde_json::from_value(old)?;
    assert_eq!(receipt.state_format_version, BASELINE_STATE_FORMAT_VERSION);
    Ok(())
}

#[test]
fn receipt_without_changed_paths_and_with_repo_id_loads() -> TestResult {
    let mut value = receipt();
    value["subject"] =
        json!({"kind":"pr","repository":ID,"number":1,"head":GIT_OID,"base":GIT_OID});
    let decoded: workflow::DeliveryReceipt = serde_json::from_value(value.clone())?;
    assert!(decoded.risk().changed_paths().is_empty());
    assert!(
        matches!(decoded.subject(), workflow::Subject::Pr { repository, .. }
        if repository.to_string() == ID)
    );
    assert_eq!(serde_json::to_value(decoded)?, value);
    Ok(())
}

#[test]
fn recorded_changed_paths_survive_round_trip() -> TestResult {
    let mut value = risk();
    value["changed_paths"] = json!(["src/auth.rs"]);
    let decoded: workflow::RiskRecord = serde_json::from_value(value.clone())?;
    assert_eq!(decoded.changed_paths(), &["src/auth.rs"]);
    assert_eq!(serde_json::to_value(decoded)?, value);
    Ok(())
}
