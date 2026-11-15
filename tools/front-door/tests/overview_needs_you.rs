//! The local overview is a compact, app-independent read of attention.
mod support;
use serde_json::{Value, json};
use support::{Result, TestRig, code};

fn call(rig: &TestRig, args: &[&str]) -> Result<Value> {
    support::call(rig, args, 0)
}

#[test]
fn summary_comparison_uses_local_host_and_reports_missing_identity() -> Result {
    let rig = TestRig::new()?;
    let checkpoint = rig.root.join("checkpoint.md");
    std::fs::write(&checkpoint, "# Checkpoint\n")?;
    call(
        &rig,
        &[
            "workflow",
            "session",
            "save",
            "chair-a",
            "--file",
            checkpoint.to_str().ok_or("checkpoint")?,
        ],
    )?;
    let draft = rig.root.join("draft.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"decision", "title":"Question", "body":"Details", "blocking":true,
            "options":[], "recommended_option_id":null, "proceeding_with":null
        }))?,
    )?;
    let ask = call(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("draft")?,
            "--session",
            "chair-a",
        ],
    )?;
    let id = ask["data"]["request_id"].as_str().ok_or("request ID")?;
    let patch = rig.root.join("patch.json");
    std::fs::write(
        &patch,
        serde_json::to_vec(&json!({"needs_you":["old-request"]}))?,
    )?;
    call(
        &rig,
        &[
            "chair",
            "summary",
            "update",
            "chair-a",
            "--file",
            patch.to_str().ok_or("patch")?,
            "--expect-revision",
            "0",
        ],
    )?;
    let show = call(&rig, &["chair", "summary", "show", "chair-a"])?;
    assert_eq!(show["data"]["comparison"]["coverage"], "complete");
    assert_eq!(show["data"]["comparison"]["stale"], json!(["old-request"]));
    assert_eq!(show["data"]["comparison"]["missing"], json!([id]));
    std::fs::write(&patch, serde_json::to_vec(&json!({"now":"Working"}))?)?;
    call(
        &rig,
        &[
            "chair",
            "summary",
            "update",
            "chair-a",
            "--file",
            patch.to_str().ok_or("patch")?,
            "--expect-revision",
            "1",
        ],
    )?;
    let diff = call(
        &rig,
        &[
            "chair", "summary", "diff", "chair-a", "--from", "1", "--to", "2",
        ],
    )?;
    assert_eq!(
        diff["data"]["diff"]["now"],
        json!({"from":"", "to":"Working"})
    );
    assert_eq!(diff["data"]["diff"]["unavailable"], json!([]));
    let current = call(
        &rig,
        &["chair", "summary", "diff", "chair-a", "--from", "1"],
    )?;
    assert_eq!(current["data"]["diff"], diff["data"]["diff"]);
    for args in [
        &["chair", "summary", "diff", "chair-a", "--from", "0"][..],
        &[
            "chair", "summary", "diff", "chair-a", "--from", "1", "--to", "0",
        ][..],
    ] {
        assert_eq!(code(&rig.command(args)?), 2);
    }
    std::fs::remove_file(rig.state.join("host-id"))?;
    let unavailable = call(&rig, &["chair", "summary", "show", "chair-a"])?;
    assert_eq!(
        unavailable["data"]["comparison"]["coverage"],
        "host_unavailable"
    );
    assert_eq!(unavailable["data"]["comparison"]["stale"], json!([]));
    assert_eq!(
        unavailable["data"]["comparison"]["unknown"],
        json!(["old-request"])
    );
    assert!(!rig.state.join("host-id").exists());
    Ok(())
}

#[test]
fn overview_needs_you() -> Result {
    let empty = TestRig::new()?.without_state()?;
    let overview = call(&empty, &["console", "overview"])?;
    assert_eq!(overview["data"]["needs_you"], json!([]));
    assert_eq!(overview["data"]["counts"]["needs_you"], Value::Null);
    assert_eq!(overview["data"]["counts"]["running"], Value::Null);
    assert_eq!(overview["data"]["counts"]["attention"], Value::Null);
    assert_eq!(overview["data"]["coverage"]["inbox"], "unavailable");
    assert_eq!(overview["data"]["coverage"]["runs"], "unavailable");
    assert!(
        !empty.state.exists(),
        "overview created an absent state root"
    );

    let rig = TestRig::new()?;
    let draft = rig.root.join("request.json");
    std::fs::write(
        &draft,
        serde_json::to_vec(&json!({
            "kind":"decision", "title":"Choose a repair", "body":"PRIVATE BODY MARKER",
            "blocking":true, "options":[], "recommended_option_id":null,
            "proceeding_with":null
        }))?,
    )?;
    let created = call(
        &rig,
        &[
            "ask",
            "--file",
            draft.to_str().ok_or("draft path")?,
            "--session",
            "chair-a",
        ],
    )?;
    let id = created["data"]["request_id"].as_str().ok_or("request ID")?;
    let overview = call(&rig, &["console", "overview"])?;
    let data = &overview["data"];
    let rows = data["needs_you"].as_array().ok_or("needs-you rows")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["title"], "Choose a repair");
    assert_eq!(rows[0]["project_label"], "project");
    assert!(rows[0]["project_id"].is_string());
    assert!(rows[0]["task_label"].is_null());
    assert_eq!(rows[0]["chair_session_id"], "chair-a");
    assert_eq!(data["counts"]["needs_you"], 1);
    assert!(!overview.to_string().contains("PRIVATE BODY MARKER"));

    // Submit a queued lane through its owner, without starting a provider.
    use provenant_core::{ActorId, CancelToken, OperationId, ProjectId};
    use provenant_run::{
        DispatchRequest, DispatchTask, ExecutionContext, HarnessProfile, LaneAccess, Request,
        RequestedRoute, RunStore, Service,
    };
    let project = ProjectId::parse(rows[0]["project_id"].as_str().ok_or("project ID")?)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = CancelToken::new();
    let request = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let context = ExecutionContext {
        state_root: inputs.resolve(&provenant_os::fs::SystemPathResolver)?.state,
        project_id: project,
        actor_id: ActorId::from_bytes([2; 16]),
        read_prompts: false,
    };
    let store = RunStore::open(&context, &request)?;
    Service::new(&store).dispatch(
        DispatchRequest {
            chair: None,
            operation_id: OperationId::parse("overview-lane")?,
            task: DispatchTask {
                context: Some(provenant_records::run::WorkContext {
                    graph_id: Some("overview-graph".into()),
                    node_id: Some("review".into()),
                    attempt: Some(1),
                    ..Default::default()
                }),
                build_heavy: false,
                title: Some("Review changes".into()),
                prompt: "PRIVATE PROMPT MARKER".into(),
                base: None,
                pinned_base: None,
                source_head: None,
                head: None,
                route: RequestedRoute::Named {
                    required_family: None,
                    extra: Default::default(),
                    name: "senior".into(),
                    effort: None,
                },
                account: None,
                profile: HarnessProfile::default(),
                timeout_seconds: 60,
                delivery_id: None,
                deliver: None,
                worktree: None,
                access: LaneAccess::default(),
            },
            lane_depth: 1,
            parent_attempt: None,
        },
        &rig.project,
        &request,
    )?;
    drop(store);
    let with_lane = call(&rig, &["console", "overview"])?;
    let lane = &with_lane["data"]["lanes"][0];
    assert_eq!(lane["task_label"], "Review changes");
    assert_eq!(lane["project_label"], "project");
    assert_eq!(lane["state"], "queued");
    assert!(lane["attempt_id"].is_string());
    assert_eq!(lane["key"]["graph"], "overview-graph");
    assert_eq!(lane["key"]["node"], "review");
    assert_eq!(lane["key"]["attempt"], 1);
    assert_eq!(with_lane["data"]["counts"]["running"], 1);
    assert!(!with_lane.to_string().contains("PRIVATE PROMPT MARKER"));

    let human = rig.command(&["console", "overview", "--full"])?;
    assert_eq!(code(&human), 0);
    let human = String::from_utf8(human.stdout.bytes)?;
    assert!(human.contains("Choose a repair"));
    assert!(human.contains("chair_session_id=chair-a"));
    assert!(!human.contains("PRIVATE BODY MARKER"));
    call(
        &rig,
        &[
            "inbox",
            "close",
            id,
            "--state",
            "withdrawn",
            "--reason",
            "No longer needed",
            "--expect-revision",
            "1",
        ],
    )?;
    let after = call(&rig, &["console", "overview"])?;
    assert_eq!(after["data"]["needs_you"], json!([]));
    assert_eq!(after["data"]["counts"]["needs_you"], 0);
    Ok(())
}
