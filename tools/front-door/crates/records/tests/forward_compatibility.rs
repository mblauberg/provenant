//! Newer record fields must survive canonical byte and digest round trips.
use provenant_records::{inbox, run, workflow, *};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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
    let bytes = serde_json::to_vec(&value)?;
    let decoded: T = decode(bytes.as_slice(), Limits::default())?;
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

fn add_future_fields_except(value: &mut Value, skipped: &[&str]) {
    match value {
        Value::Object(fields) => {
            for (key, child) in fields.iter_mut() {
                if !skipped.contains(&key.as_str()) {
                    add_future_fields_except(child, skipped);
                }
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
                add_future_fields_except(item, skipped);
            }
        }
        _ => {}
    }
}

macro_rules! cases {
    ($($name:ident: $type:ty => $seed:expr;)*) => {
        $(fn $name() -> TestResult { round_trip::<$type>(stringify!($name), $seed) })*

        fn stored_record_registry() -> Vec<(&'static str, fn() -> TestResult)> {
            let mut registry = vec![$((stringify!($name), $name as fn() -> TestResult),)*];
            registry.push(("attempt_record_preserves_digest", attempt_record_round_trip));
            registry.push(("work_key_preserves_identity_and_unknown_fields", work_key_round_trip));
            registry.push(("account_identity_preserves_all_cases", account_identity_round_trip));
            registry.push(("port_command_receipt_preserves_all_lifetimes", port_command_receipt_round_trip));
            registry.push(("forge_observation_preserves_all_variants", forge_observation_round_trip));
            registry.push(("landing_record_preserves_unknown_fields", landing_record_round_trip));
            registry.push(("custody_record_preserves_unknown_fields", custody_record_round_trip));
            registry.push(("ref_ownership_preserves_unknown_fields", ref_ownership_round_trip));
            registry.push(("removal_record_preserves_outcomes", removal_record_round_trip));
            registry.push(("retirement_record_preserves_unknown_fields", retirement_record_round_trip));
            registry.push(("handoff_preserves_unknown_fields", handoff_round_trip));
            registry.push(("pause_preserves_unknown_fields", pause_round_trip));
            registry.push(("stop_preserves_unknown_fields", stop_round_trip));
            registry.push(("finding_decision_preserves_unknown_fields", finding_decision_round_trip));
            registry.push(("limit_preserves_unknown_fields", limit_round_trip));
            registry.push(("control_receipt_preserves_unknown_fields", control_receipt_round_trip));
            registry.push(("attempt_key_preserves_unknown_fields", attempt_key_round_trip));
            registry
        }

        #[test]
        fn stored_record_codecs_preserve_unknown_fields() -> TestResult {
            for (name, check) in stored_record_registry() {
                check().map_err(|error| std::io::Error::other(format!("{name}: {error}")))?;
            }
            Ok(())
        }
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
    let mut ranking =
        serde_json::to_value(provenant_records::route_selection::CandidateRanking::default())
            .unwrap();
    ranking["outcome_prior"] = json!({
        "enabled":true,"role":"reviewer","pool":"senior","factor_basis_points":11000,
        "evidence_count":1,"evidence":{"model":"codex/model","pool":"senior","role":"reviewer",
        "successes":1,"failures":0,"source":"fixture"},"source":"route_outcomes"
    });
    json!({"ranking":ranking,"model":"codex/model","weight":1,"factor_basis_points":10000,
        "effort":null,"readiness":readiness(),"exclusion":null})
}
fn redraw() -> Value {
    json!({"rejected_pick":"codex/other","roll":0,"total_weight":1,"reason":"stale"})
}
fn selection() -> Value {
    json!({"selection_rule":"weighted_draw","considered_exclusions":[],"exploration":{"selected":false,"percent":10,"enabled":true,"source":"fixture"},"weight_source":"catalogue","account_load":null,"pool":"senior","candidates":[candidate()],"roll":0,"total_weight":1,
        "pick":"codex/model","redraws":[redraw()]})
}
fn alternative() -> Value {
    json!({"diagnostics":serde_json::to_value(provenant_records::route_selection::RouteDiagnostics::default()).unwrap(),"adapter":"codex","model":"model","effort":null,"account_key":null,
        "host":"local","eligibility":"eligible","readiness":readiness(),"confidentiality":confidentiality()})
}
fn exclusion() -> Value {
    json!({"adapter":"codex","model":"other","reason":"disabled"})
}
fn resolved_route() -> Value {
    let mut value = alternative();
    value["selection_context"] = json!({"workload":"general","writes":false});
    value["catalogue_revision"] = json!("revision");
    value["alternatives"] = json!([alternative()]);
    value["exclusions"] = json!([exclusion()]);
    value["pool_selection"] = selection();
    value
}
fn route_record() -> Value {
    json!({"selection_context":{"workload":"general","writes":false},"provider":"codex","model":"model","effort":null,
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
        "read_only_writable":[],"write_grants":[write_grant()],"protected_paths":[path()],"delivery":writer_delivery_record(),
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
    json!({"factors":risk_factors(),"derived":"substantial","tier":"routine",
        "downgrade_approval":{"actor_id":ID,"evidence":evidence(),"reason":"bounded scope"}})
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
    json!({"schema_version":inbox::HUMAN_REQUEST_SCHEMA_VERSION,"id":ID,"revision":1,"operation_id":"operation",
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
    json!({"harness":"codex","skills_dir":absolute_path(),"config_file":absolute_path()})
}
fn install_receipt() -> Value {
    json!({"schema_version":INSTALL_RECEIPT_SCHEMA_VERSION,"state_format_version":2,"source_digest":source_digest(),"manifest_digest":digest(),
        "asset_digest":source_digest(),"target":"target","toolchain":"toolchain","bin_dir":absolute_path(),
        "harnesses":["codex"],"harness_targets":[harness_target()],"entries":[owned_entry()]})
}

cases! {
    work_context_preserves_digest: run::WorkContext => json!({"work_item":"issue-2268","role":"reviewer"});
    chair_binding_preserves_digest: provenant_records::custody::Chair => json!({"host":ID,"actor":ID});
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
    lane_wait_preserves_digest: run::LaneWait => json!({"attempt_id":ID,"lane_class":"writer","waiting_for":{"kind":"provider_silence","silent_for_ms":5}});
    admission_ticket_preserves_digest: run::AdmissionTicket => json!({"schema_version":1,"operation_id":"operation","attempt_id":ID,"project_id":ID,"sequence":1,"generation":1,"active":true,"lane_class":"writer","memory_floor_basis_points":1000,"startup_spacing_ms":1,"run_concurrency":1,"runtime_seconds":60,"admission_deadline":NOW});
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
    command_exit_codes_preserve_unknown_fields: CommandExitCodes => json!({"success":[0],"unknown":[75],"other":"unknown"});
    port_command_preserves_unknown_fields: PortCommand => json!({"argv":["provenant","verify"],"exit_codes":{"success":[0],"unknown":[75],"other":"unknown"}});
    trace_preserves_digest: Trace => json!({"host":ID,"run":ID,"attempt":ID});
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
    budget_policy_preserves_digest: BudgetPolicy => json!({"action":"wrap_up","action_source":"default","grace":"0.1","grace_source":"default"});
    lane_limits_preserve_digest: LaneLimits => json!({});
    skipped_account_preserves_digest: SkippedAccount => json!({"account":"fixture","reason":"disabled"});
}

fn attempt_record_round_trip() -> TestResult {
    let mut value = normalise::<run::AttemptRecord>(attempt())?;
    add_future_fields(&mut value);
    assert_preserved::<run::AttemptRecord>("run-attempt", value)
}

fn work_key_round_trip() -> TestResult {
    use std::collections::{BTreeSet, HashSet};

    let key = WorkKey::new(
        provenant_core::ProjectId::from_bytes([7; 16]),
        "graph",
        "node",
        1,
    )?;
    let mut value = serde_json::to_value(&key)?;
    add_future_fields(&mut value);
    let bytes = serde_json::to_vec(&value)?;
    let decoded: WorkKey = decode(bytes.as_slice(), Limits::default())?;
    assert_eq!(decoded, key);

    let mut hashed = HashSet::new();
    hashed.insert(key.clone());
    assert!(hashed.contains(&decoded));
    let mut ordered = BTreeSet::new();
    ordered.insert(key.clone());
    assert!(ordered.contains(&decoded));

    assert_preserved::<WorkKey>("work-key", value)
}

fn account_identity_round_trip() -> TestResult {
    use usage_identity::AccountIdentity;

    for identity in [
        AccountIdentity::known("codex", "account-1", "epoch-1"),
        AccountIdentity::provider_observation("codex", "account-2"),
        AccountIdentity::unknown(),
        AccountIdentity::conflicting_provider_observations(),
        AccountIdentity::no_provider_account(),
    ] {
        let mut value = serde_json::to_value(identity)?;
        add_future_fields(&mut value);
        assert_preserved::<AccountIdentity>("account-identity", value)?;
    }
    Ok(())
}

fn port_command_receipt_round_trip() -> TestResult {
    use port_receipt::{PortCommandReceipt, PortReceiptLifetime};
    use provenant_core::{ActorId, AttemptId, ProjectId};

    for lifetime in [
        PortReceiptLifetime::AttemptTerminalAndRelease {
            attempt_id: AttemptId::from_bytes([1; 16]),
            project_id: ProjectId::from_bytes([2; 16]),
            actor_id: ActorId::from_bytes([3; 16]),
            lease_epoch: 4,
            extra: Default::default(),
        },
        PortReceiptLifetime::AgeDays {
            days: 30,
            extra: Default::default(),
        },
    ] {
        let receipt = PortCommandReceipt::new(
            "fixture".into(),
            PortSource::Project,
            "publish".into(),
            CanonicalV1::file_digest(b"request"),
            CanonicalV1::file_digest(b"command"),
            port_receipt::PortCommandDisposition::Succeeded,
            Some(0),
            Some(1),
            None,
            true,
            false,
            false,
            10,
            lifetime,
        );
        let mut value = serde_json::to_value(receipt)?;
        add_future_fields(&mut value);
        assert_preserved::<PortCommandReceipt>("port-command-receipt", value)?;
    }
    Ok(())
}

fn forge_observation_round_trip() -> TestResult {
    use landing::ForgeObservation;

    for mut value in [
        json!({"state":"pending","pending":"submitted"}),
        json!({"state":"merged","evidence":{"candidate_commit":null,
            "candidate_tree":null,"target_base":null,"member_commits":[],"members":[]}}),
        json!({"state":"rejected","reason":"closed"}),
        json!({"state":"unknown","reason":"unavailable"}),
    ] {
        add_future_fields(&mut value);
        assert_preserved::<ForgeObservation>("forge-observation", value)?;
    }
    Ok(())
}

fn landing_record_round_trip() -> TestResult {
    use landing::{
        Capabilities, CleanupDebt, CleanupDebtItem, Compose, ConsumerCursor, GateDefinition,
        GateKind, LandRequest, LandingId, LandingRecord, MemberOutcome, MemberProgress,
        PublicationGuarantee, ResolvedPlan, RestackMapping, Strategy, Subscriber,
    };

    let request = LandRequest::default();
    let resolved_plan = ResolvedPlan::new(
        Strategy::Batch,
        &Compose::default(),
        Capabilities::default(),
        BTreeMap::new(),
    )?;
    let id = LandingId::of("fixture/repo", "main", &request, &resolved_plan.digest)?;
    let record = LandingRecord {
        schema_version: 2,
        id,
        repository: "fixture/repo".into(),
        target: "main".into(),
        request,
        generation: 1,
        revision: 1,
        resolved_plan,
        gate_definition: GateDefinition {
            kind: GateKind::Checks,
            digest: digest(),
            checks: Vec::new(),
            commands: Vec::new(),
            issuer: None,
            executor: None,
            coverage: Vec::new(),
            timeout_seconds: 0,
            pool: None,
            env_passthrough: Vec::new(),
            extra: Default::default(),
        },
        outcomes: BTreeMap::from([(
            1,
            MemberOutcome::Pending {
                reason: "awaiting observation".into(),
                extra: Default::default(),
            },
        )]),
        progress: BTreeMap::from([(
            1,
            MemberProgress::Pending {
                extra: Default::default(),
            },
        )]),
        operation: None,
        submission: None,
        unavailable: None,
        evidence: None,
        publication_guarantee: PublicationGuarantee::None,
        cleanup_debt: CleanupDebt {
            items: vec![CleanupDebtItem {
                step: "fixture".into(),
                reason: "pending".into(),
                attempts: 0,
                extra: Default::default(),
            }],
            extra: Default::default(),
        },
        restack_mappings: vec![RestackMapping {
            pr: 1,
            previous_head: "a".repeat(40),
            new_head: "b".repeat(40),
            previous_base: "c".repeat(40),
            new_base: "d".repeat(40),
            extra: Default::default(),
        }],
        subscribers: vec![Subscriber {
            run_id: Some("run".into()),
            node_id: Some("node".into()),
            session_id: Some("session".into()),
            extra: Default::default(),
        }],
        consumer_cursors: vec![ConsumerCursor {
            subscriber: Subscriber::default(),
            revision: 1,
            extra: Default::default(),
        }],
        applied_observations: BTreeSet::new(),
        warnings: Vec::new(),
        passes: 0,
        extra: Default::default(),
    };
    let mut value = normalise::<LandingRecord>(serde_json::to_value(record)?)?;
    // LandRequest is an authority-bearing request; `values` is enum-keyed data,
    // not an object record whose unknown fields use ExtraFields.
    add_future_fields_except(&mut value, &["request", "values"]);
    assert_preserved::<LandingRecord>("landing-record", value)
}

fn custody_record_round_trip() -> TestResult {
    use custody::{Chair, CustodyRecord, CustodyState, Holder, Origin, Subject, SubjectKind};
    use provenant_core::{ActorId, HostId, ProjectId, WallTime};

    let record = CustodyRecord {
        extra: Default::default(),
        schema_version: 2,
        subject: Subject::new(SubjectKind::Graph, "graph-1"),
        holder: Holder::Chair {
            chair: Chair {
                extra: Default::default(),
                host: HostId::from_bytes([1; 16]),
                actor: ActorId::from_bytes([2; 16]),
                session: Some("session".into()),
                process: None,
            },
            extra: Default::default(),
        },
        project: ProjectId::from_bytes([3; 16]),
        origin: Origin::Provenant,
        state: CustodyState::Active {
            extra: Default::default(),
        },
        last_activity: WallTime::from_micros(0)?,
        links: Vec::new(),
        provenance: None,
    };
    let mut value = serde_json::to_value(record)?;
    add_future_fields(&mut value);
    assert_preserved::<CustodyRecord>("custody-record", value)
}

fn ref_ownership_round_trip() -> TestResult {
    use custody::{RefEndpoint, RefOrigin, RefOwnership};
    use provenant_core::ProjectId;

    let ownership = RefOwnership {
        repository: ProjectId::from_bytes([1; 16]),
        endpoint: RefEndpoint::Local {
            extra: Default::default(),
        },
        reference: "refs/heads/feature".into(),
        generation: 1,
        origin: RefOrigin::Created,
        extra: Default::default(),
    };
    let mut value = serde_json::to_value(ownership)?;
    add_future_fields(&mut value);
    assert_preserved::<RefOwnership>("ref-ownership", value)
}

fn removal_record_round_trip() -> TestResult {
    use custody::{RemovalOutcome, RemovalRecord, RemovalUncertainty, Subject};
    use provenant_core::{GitOid, ProjectId};

    let project = ProjectId::from_bytes([1; 16]);
    let subject = Subject::worktree("/fixture/feature", "generation-1");
    let tip = GitOid::parse(&"aa".repeat(20))?;
    let outcomes = [
        RemovalOutcome::Kept {
            cause: RemovalUncertainty::Busy,
            extra: Default::default(),
        },
        RemovalOutcome::Removed {
            evidence: custody::RemovalEvidence::IdleRetirement {
                tip,
                extra: Default::default(),
            },
            retirement_id: "retirement-1".into(),
            extra: Default::default(),
        },
    ];
    for outcome in outcomes {
        let record = RemovalRecord {
            project,
            subject: subject.clone(),
            tip,
            outcome,
            extra: Default::default(),
        };
        let mut value = serde_json::to_value(record)?;
        add_future_fields(&mut value);
        assert_preserved::<RemovalRecord>("removal-record", value)?;
    }
    Ok(())
}

fn retirement_record_round_trip() -> TestResult {
    use custody::{
        RefEndpoint, RefOrigin, RefOwnership, RemovalEvidence, RetirementDecision,
        RetirementRecord, Subject,
    };
    use provenant_core::{ActorId, GitOid, ProjectId};

    let project = ProjectId::from_bytes([1; 16]);
    let tip = GitOid::parse(&"aa".repeat(20))?;
    let reference = RefOwnership {
        repository: project,
        endpoint: RefEndpoint::Local {
            extra: Default::default(),
        },
        reference: "refs/heads/feature".into(),
        generation: 1,
        origin: RefOrigin::Created,
        extra: Default::default(),
    };
    let record = RetirementRecord::plan(
        project,
        vec![Subject::worktree("/fixture/feature", "generation-1")],
        reference,
        tip,
        RetirementDecision::RetireWorktree {
            authority: ActorId::from_bytes([2; 16]),
            evidence: RemovalEvidence::IdleRetirement {
                tip,
                extra: Default::default(),
            },
            extra: Default::default(),
        },
    )?;
    let mut value = serde_json::to_value(record)?;
    add_future_fields(&mut value);
    assert_preserved::<RetirementRecord>("retirement-record", value)
}

fn handoff_round_trip() -> TestResult {
    use handoff::Handoff;

    for write_scope in [
        json!({"kind":"full","schema_version":1,"code":"WRITE_SCOPE","base":"1".repeat(40),
            "head":"2".repeat(40),"strength":"warn","source":"project","out_of_scope_paths":[]}),
        json!({"kind":"compact","code":"WRITE_SCOPE","strength":"warn","source":"project",
            "paths_artifact":"policy-write-scope.json","out_of_scope_count":1,
            "paths_omitted_from_handoff":true}),
    ] {
        round_trip::<Handoff>(
            "handoff",
            json!({"schema_version":2,"status":"done","note":"finished",
                "remaining":[],"decisions":[],"verify":[],
                "spec_concerns":[{"criterion":"C1","claim":"Concern","evidence":"Evidence",
                    "proposed_change":"Change","status":"as_written"}],
                "write_scope":write_scope}),
        )?;
    }
    Ok(())
}

fn pause_round_trip() -> TestResult {
    use pause::{Pause, PauseCause};
    round_trip::<Pause>(
        "pause",
        serde_json::to_value(Pause::new(
            PauseCause::BudgetExceeded,
            None,
            "Budget paused.",
        )?)?,
    )
}

fn stop_round_trip() -> TestResult {
    use stop::{Stop, StopCause};
    round_trip::<Stop>(
        "stop",
        serde_json::to_value(Stop::new(StopCause::Lost, None)?)?,
    )
}

fn finding_decision_round_trip() -> TestResult {
    use finding_decision::{FindingDecision, FindingDisposition};
    use provenant_core::GitOid;
    let decision = FindingDecision::new(
        "F1",
        FindingDisposition::Act,
        "Addressed.",
        GitOid::parse(&"aa".repeat(20))?,
        Some(GitOid::parse(&"bb".repeat(20))?),
    )?;
    round_trip::<FindingDecision>("finding-decision", serde_json::to_value(decision)?)
}

fn limit_round_trip() -> TestResult {
    round_trip::<Limit>(
        "limit",
        serde_json::to_value(Limit::new(3, LimitSource::Launch, LimitStrength::Default))?,
    )
}

fn control_receipt_round_trip() -> TestResult {
    use control::{ControlKind, ControlReceipt, ControlState, ControlSupport, LaunchKind};
    use provenant_core::OperationId;

    let receipt = ControlReceipt {
        extra: Default::default(),
        operation_id: OperationId::parse("fixture-operation")?,
        control: ControlKind::Steer,
        target: "attempt-1".into(),
        launch: LaunchKind::Lane,
        support: ControlSupport::Unsupported,
        transport: None,
        state: ControlState::Unsupported,
        reason: "no live control path".into(),
        message_id: None,
    };
    round_trip::<ControlReceipt>("control-receipt", serde_json::to_value(receipt)?)
}

fn attempt_key_round_trip() -> TestResult {
    round_trip::<graph::AttemptKey>(
        "graph-attempt-key",
        json!({"retry_generation":null,"previous_attempt_ref":null,
            "graph_id":"graph","node_id":"node","attempt":1,"route_index":0}),
    )
}

#[test]
fn human_request_uses_its_declared_schema_version() -> TestResult {
    let mut current = human_request();
    let current_bytes = serde_json::to_vec(&current)?;
    let _: inbox::HumanRequest = decode(current_bytes.as_slice(), Limits::default())?;

    current["schema_version"] = json!(1);
    let legacy_bytes = serde_json::to_vec(&current)?;
    assert!(decode::<inbox::HumanRequest, _>(legacy_bytes.as_slice(), Limits::default()).is_err());
    Ok(())
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

#[test]
fn typed_route_selection_preserves_nested_priors_and_rejects_malformed_fields() {
    use provenant_records::{ResolvedRoute, RouteSelectionReceipt, run::RouteRecord};
    let decode_selection = |value: &Value| {
        let bytes = serde_json::to_vec(value).unwrap();
        decode::<RouteSelectionReceipt, _>(bytes.as_slice(), Limits::default())
    };
    let mut wire = selection();
    wire["account_load"] = json!({
        "account_key":"account", "account_source":"fixture", "headroom_basis_points":5000,
        "reset_eta_seconds":60, "live_lanes":2, "pacing":"pace", "pacing_factor_basis_points":10000,
        "penalty_basis_points":10000, "source":"fixture", "warnings":[]
    });
    wire["future_selection_fact"] = json!({"value":true});
    for rule in [
        "weighted_draw",
        "exploration_draw",
        "model_choice_override",
        "first_zero_weight_ready",
        "first_standby_ready",
        "first_recoverable_unavailable",
        "first_unavailable",
    ] {
        for source in ["config", "catalogue"] {
            for exclusion in ["config", "flag", "graph"] {
                wire["selection_rule"] = json!(rule);
                wire["exploration"]["selected"] = json!(rule == "exploration_draw");
                wire["weight_source"] = json!(source);
                wire["considered_exclusions"] =
                    json!([{"source":exclusion,"selector":"model codex/other"}]);
                let decoded = decode_selection(&wire).unwrap();
                let encoded = serde_json::to_value(&decoded).unwrap();
                let again = decode_selection(&encoded).unwrap();
                assert_eq!(decoded, again);
                assert_eq!(
                    again.extra["future_selection_fact"],
                    wire["future_selection_fact"]
                );
                assert!(!again.extra.contains_key("selection_rule"));
            }
        }
    }
    for (path, bad) in [
        ("/selection_rule", json!(42)),
        (
            "/considered_exclusions",
            json!(["source=config selector=x"]),
        ),
        ("/considered_exclusions/0/source", json!("unknown")),
        ("/considered_exclusions/0/selector", json!(false)),
        ("/exploration/selected", json!("yes")),
        ("/exploration/percent", json!(-1)),
        ("/exploration/enabled", json!(1)),
        ("/exploration/source", json!([])),
        ("/weight_source", json!("unknown")),
        ("/account_load/live_lanes", json!("two")),
        ("/account_load/pacing", json!(false)),
        ("/candidates/0/ranking", json!([])),
    ] {
        let mut malformed = wire.clone();
        *malformed.pointer_mut(path).unwrap() = bad;
        assert!(
            matches!(decode_selection(&malformed), Err(DecodeError::Typed { .. })),
            "{path}"
        );
    }
    let mut record = route_record();
    record["host_native"] = json!("reviewer");
    let record_bytes = serde_json::to_vec(&record).unwrap();
    let decoded: RouteRecord = decode(record_bytes.as_slice(), Limits::default()).unwrap();
    assert_eq!(decoded.host_native.as_deref(), Some("reviewer"));
    record["host_native"] = json!(7);
    let malformed_bytes = serde_json::to_vec(&record).unwrap();
    assert!(matches!(
        decode::<RouteRecord, _>(malformed_bytes.as_slice(), Limits::default()),
        Err(DecodeError::Typed { .. })
    ));
    let mut resolved = resolved_route();
    resolved["selection_context"] = json!({"workload":"review","writes":true});
    resolved["diagnostics"]["account_load"] = wire["account_load"].clone();
    let resolved_bytes = serde_json::to_vec(&resolved).unwrap();
    let decoded: ResolvedRoute = decode(resolved_bytes.as_slice(), Limits::default()).unwrap();
    let record = RouteRecord::from_resolved(&decoded, None, "fixture");
    assert_eq!(
        record.selection_context.as_ref(),
        Some(&decoded.selection_context)
    );
    assert_eq!(
        decode::<ResolvedRoute, _>(
            serde_json::to_vec(&decoded).unwrap().as_slice(),
            Limits::default()
        )
        .unwrap(),
        decoded
    );
    resolved["diagnostics"]["account_load"]["headroom_basis_points"] = json!("unknown");
    let resolved_bytes = serde_json::to_vec(&resolved).unwrap();
    assert!(matches!(
        decode::<ResolvedRoute, _>(resolved_bytes.as_slice(), Limits::default()),
        Err(DecodeError::Typed { .. })
    ));
}
